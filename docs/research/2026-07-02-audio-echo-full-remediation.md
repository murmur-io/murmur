<!-- Generated 2026-07-02 via /research (murmur-researcher fan-out: competitors / VPIO deep-dive / dedup+alignment / codebase inventory). Pricing/versions = point-in-time. -->
# Research: Full remediation of the speaker→mic echo (double-hearing) in Murmur's audio path

## TL;DR / Verdict

When a user records a call **on speakers**, the remote voice enters twice — digitally via the
system-audio capture and acoustically via the mic — so the archive mix plays it doubled and the
transcript carries the same sentence as both `others` and `me`. **Platform AEC (VPIO) is NOT the
fix**: screenpipe measured **ERLE ≈ 0 dB** for input-only VPIO (no rendered far-end reference —
exactly our `aeccap` configuration), and VPIO additionally **ducks other apps' audio system-wide**
(reports of the system-audio capture dropping to ~−51 dB, i.e. it can silence the very call we're
recording) and conflicts with the call app's own VPIO. The proven industry answer for apps that
don't own call playback is: **(1) transcript-level cross-stream echo dedup** (screenpipe shipped
it with adversarially tuned thresholds; OpenWhispr ships a richer correlation-gated variant),
**(2) time-aligned mixing**, and **(3) offline software AEC using the digitally-captured system
track as the far-end reference** (Hyprnote's DTLN-aec; WebRTC AEC3 — now available as the pure-Rust
`sonora` crate). We hold BOTH full tracks at pipeline time, so post-hoc AEC is the textbook setup —
and unlike VPIO it is deterministic, CI-testable, and has zero effect on the live call.

**Recommended remediation (4 slices):**
1. **Transcript echo dedup** at merge time (pure Rust, no deps, headless-testable) — fixes the
   doubled transcript feeding notes/Ask/graph.
2. **Archive-mix time alignment** (first-frame anchors from the helpers + measured cross-correlation
   offset) + peak-normalized summing — fixes timeline desync and most of the *perceptual* doubling.
3. **Post-hoc AEC3** (`sonora`, needs user approval as a new crate) on the recorded mic with the
   system track as reference — removes the echo *energy* from ASR feed AND playback mix.
4. **Contain/deprecate the VPIO helper** + surface a real "you're on speakers" UI hint.

## What we already have (verified against the tree)

- Dual-stream capture: cpal mic (`audio/recorder.rs:110`) + system audio (Core Audio process tap
  ≥14.4 / SCK sidecar, `audio/system.rs`, `audio/tap.rs`); each transcribed separately; wall-clock
  merge (`audio/merge.rs:63`) with the echo dent explicitly documented (`merge.rs:25-27`).
- Archive = `mix(raw mic, system)` **from sample 0, no offset** (`pipeline.rs:281-287`;
  `mixer.rs:9-17` hard-clamps the sum). Anchor asymmetry: mic = first cpal frame
  (`recorder.rs:428-431`), system = **process-spawn instant** (`system.rs:109`) — helper setup
  latency (est. 100–500 ms) unaccounted.
- VPIO helper `aeccap` (opt-in `aec_enabled`, default OFF, `config.rs:287`): input-only
  `setVoiceProcessingEnabled(true)`, **no ducking configuration, AGC left on**
  (`aeccap.swift:46-64`); output feeds ASR only, never the archive (`pipeline.rs:279`); ±2 s
  duration guard after the 51 s→8 s malformed-feed incident (`pipeline.rs:42-46`).
- No cross-stream dedup anywhere; live captions read the raw mic only (`transcribe/live.rs:159`).
- Headphones banner exists but shows only when NOT recording (`record.component.ts:1036-1039`).
- `onnxruntime` already bundled (sherpa-onnx diarizer, `transcribe/diarize.rs:1-6`); `realfft 3.5.0`
  already compiled transitively via `rubato` (`Cargo.toml:28` + lock).

## Findings (per angle)

### A. Competitors (high confidence — code/issues read directly)

| Product | Capture (macOS) | AEC | Transcript dedup | Storage |
|---|---|---|---|---|
| Murmur today | cpal + tap/SCK, dual-stream ASR | VPIO opt-in OFF, likely 0 dB | **none** | one mixed WAV (doubled on speakers) |
| Granola | mic + system ("Me"/"Them") | none documented; headset guidance | unknown (closed) | **no audio stored** |
| Meetily | cpal + Core Audio tap | **never shipped** (VPIO PR #404 closed unmerged); open echo bug #220 | none | mixed single stream |
| Hyprnote | dual-channel | **DTLN-aec ONNX**, system capture = reference, GCC-PHAT aligned | CrossChannelEchoSuppressor (landing, #5880) | per-session |
| screenpipe | cpal fork + system | VPIO measured **0 dB** (#3938); sw AEC "NO-OP" (#4379) | **shipped** (#4440): Δ≤1.5 s, both ≥4 words, symmetric Jaccard ≥0.85, keep system copy | per-device rows |
| Krisp / Zoom / Teams | own the playback path | yes, default-on (they have the reference) | n/a | — |
| Riverside/Descript | per-participant local | per-participant; headphones still advised | Descript "mic bleed" → headphones | separate tracks |

Key mechanism (screenpipe #3938, measured): VPIO's AEC subtracts the far-end **rendered through the
unit's own output element**; input-only VPIO has no downlink reference → nothing to cancel. Zoom/
Krisp AEC works because they own playback. Notetakers that shipped something real shipped
**transcript dedup** and/or **software AEC with the system capture as reference**.

### B. VPIO deep-dive (why the current `aeccap` path is a dead end)

- **System-wide ducking**: enabling voice processing ducks *other apps' audio* (Apple:
  `voiceProcessingOtherAudioDuckingConfiguration`; control only on macOS 14+). Field report from a
  twin app (whisper.cpp + SCK): system-audio recording dropped to ~−51 dB with VPIO on; workaround
  `duckingLevel: .min`. Our helper sets nothing → default ducking. It can quiet the user's live
  call AND silently gut our `others` track — the duration guard cannot catch that.
- **VPIO-vs-VPIO conflicts**: Zoom/Meet/Safari use voice processing themselves; unanswered Apple
  forum reports of one client cutting the other's input stream (thread 751100). We could break the
  user's live call — the exact scenario we record.
- **Format mutations** are a known unfixed OS pattern (1→3 ch, 2 ch/48 k→7 ch/96 k, thread 710151);
  our 9-channel incident fits it 1:1.
- **Not archive material**: uplink AGC on by default, NS artifacts, near-end suppression during
  double-talk (thread 733733). The existing "AEC never into the archive" rule is correct for VPIO
  — permanently.
- **Reference blindness**: macOS system AEC historically did not track non-default output devices
  (Chrome M68 blog) — calls played over non-default USB/HDMI outputs may cancel nothing, silently.

### C. Post-hoc software AEC (the real audio fix)

- We hold the perfect far-end reference: the digitally-captured system track, plus wall-clock
  anchors for an initial delay hint (`pipeline.rs:193-194`). This is the textbook AEC setup; AEC3's
  render-delay controller continuously re-estimates alignment (tolerates clock drift; Speex-class
  cancellers do NOT — disqualified).
- Crates: **`sonora`** (pure-Rust WebRTC APM port M145: AEC3+NS+AGC2, BSD-3, v0.1.0 2026-02,
  2400+ ported reference tests, ~1.1× C++ speed; no C++ toolchain → notarization-friendly) —
  primary candidate. `webrtc-audio-processing` (C++ submodule + meson — build burden),
  `aec3-rs` (offline file-to-file example but murky license), DTLN-aec via already-bundled
  onnxruntime (Hyprnote-proven; needs model bundling + approval) — fallback.
- Fully headless-testable: synthetic echo (delayed, attenuated copy mixed into mic) → assert ≥15 dB
  echo-energy reduction with near-end speech preserved; RED on passthrough.

### D. Dedup + alignment algorithms (prod-proven values)

- **screenpipe (merged #4440, conservative, ungated)**: collapse only when opposite stream, Δstart
  ≤1.5 s, both ≥4 words, symmetric word-set Jaccard ≥0.85; keep the system copy; never drop both.
- **OpenWhispr (production, richer)**: normalize (lowercase, keep `\p{L}\p{N}`); fast paths equal/
  containment; then `max(multiset token coverage, token-level LCS) / shorter ≥ 0.6` (min 3 tokens);
  relaxed 0.55 stopword-filtered tier **only under acoustic leak evidence**; candidates include
  concatenations of up to 3 adjacent segments (segmentation mismatch across two independent Whisper
  passes); asymmetric window (echo trails the clean copy); always remove the mic copy. Their
  acoustic gate: normalized cross-correlation of raw mic vs rolling system history, lag 0–500 ms,
  strong ≥0.75 / partial ≥0.6, double-talk via RMS ratio. War story: the detector must run on the
  **raw** mic, not an AEC-cleaned buffer.
- **Alignment**: GCC-PHAT or envelope-NCC on 2–3 windows (30–60 s each) costs <1 s per meeting;
  the peak lag τ feeds the mix pad AND centers the dedup window; the peak strength ρ is the
  headphones/speakers discriminator that disarms dedup entirely when there is no leak. Consistency
  across windows guards against drift; fallback = wall-clock anchors.
- **Mix**: sum → peak-normalize only if |peak|>1 (no clipping, level preserved); hard clamp =
  distortion exactly when both parties speak. Once aligned, residual echo overlaps its original
  (mild comb coloration) instead of a distinct double voice.
- Whisper-specific: the echo copy is decoded garbled/shortened/hallucinated (low SNR ⇒ +~20 %
  hallucinations) — argues for /shorter normalization, thresholds ~0.6, and accepting that pure
  hallucination residue is only removable by real AEC.

### E. Codebase change inventory (full detail in the agent brief; key facts)

- Dedup slots in as a pure fn after `merge_streams` at `pipeline.rs:440`, **before**
  `insert_segments` (`pipeline.rs:449`) → DB/note/timeline/resummarize/MCP all see one truth.
  Diarization labels are `others-N` — match `!= SPEAKER_ME`, not `== "others"`.
- Mix alignment: new `mix_aligned` fn (do NOT change `mix` — `examples/e2e_core.rs:42` +
  `scripts/e2e-mix.sh` call it); pad by measured offset, fallback wall-clock anchors.
- First-frame anchors: helpers print one `first-frame` stderr line at the first buffer
  (`sysaudio.swift:100-107`, `audiocap.swift:206-213`); Rust takes `Instant::now()` on receipt in a
  stderr-reader thread (also drains the pipe — closes a latent 64 KB pipe-deadlock risk;
  `system.rs:91` pipes stderr but never reads). Helpers are bundled with the app → protocol change
  is atomic, fallback = today's spawn instant.
- **Lock model: zero impact** as long as the AEC output stays a `$TMPDIR` scratch (ScratchWav
  delete-on-drop, `pipeline.rs:220-221`) and the archive remains the same `<id>.wav` path (AAD
  `StreamRole::Playback` untouched). Do NOT persist any new at-rest audio file.
- AEC-default-ON would NOT reach existing users anyway (saved `"false"` wins over a default flip,
  `config.rs:428`) — moot given the VPIO verdict.
- FE surface: `settings-audio-section.component.ts:94-107` (AEC toggle copy),
  `record.component.ts:75-80,1036-1039` (headphones banner), `settings.store.ts:86/519/752`,
  `onboarding.component.ts:1334`.
- No output-device introspection exists anywhere; the crash-safe route is Core Audio C API
  (`kAudioHardwarePropertyDefaultOutputDevice` → `kAudioDevicePropertyTransportType == BuiltIn`)
  per the rules-§7 FFI discipline; `coreaudio-sys` already in the lock via cpal (direct-dep
  declaration still needs approval).
- Live captions remain echo-affected (batch-only dedup; system WAV unreadable mid-write because
  AVAudioFile finalizes the header on close) — honest known gap.

## Fit with Murmur's constraints

- **Local-first**: everything on-device, zero egress. Post-hoc AEC beats VPIO on the "don't touch
  anything outside ourselves" principle too (no ducking of other apps).
- **SQLite-canonical**: dedup before persist → one truth for UI/MCP/Obsidian.
- **Lock model**: no new read/export/at-rest paths (condition above). Not a lock-touching change;
  adversarial-verifier owns the verdict, lock review only if the scratch/at-rest condition changes.
- **Content-loss discipline**: dedup deletes user speech by design — the #1 hunted failure class.
  Load-bearing mitigations: acoustic gate (no leak evidence ⇒ no relaxed-tier deletion), strict
  ungated tier uses screenpipe's adversarially tuned floor (≥4 words + symmetric ≥0.85), always
  drop `me`/never `others`, never drop both, log counts only (no PII).
- **CI honesty bar**: dedup/alignment/mixer/AEC-offline are pure fns — `cargo test --lib`,
  RED-before-GREEN, headless. "Double hearing is gone on speakers" end-to-end + speakers detection
  + first-frame latency = **real Mac, signed build, recorded evidence**.
- **No new deps without approval**: `sonora` (Phase 3) and optional `realfft`/`coreaudio-sys`
  direct-dep declarations require explicit user sign-off; Phases 1–2 need none.

## Options and tradeoffs

| Option | Effort | Risk | Fixes |
|---|---|---|---|
| 1. Transcript dedup (gated two-tier) | S | low (guards above) | transcript doubling → notes/Ask/graph quality |
| 2. Mix alignment + first-frame anchors + peak-normalize | S–M | low (pure fns; helpers +1 line) | timeline desync; most perceptual doubling |
| 3. Post-hoc AEC3 (`sonora`) behind flag → bake-off → default ON | M | medium (crate v0.1.0; quality needs real-Mac bake-off) | echo **energy** in ASR feed + playback |
| 4. VPIO containment: ducking `.min` + AGC off if kept; deprecate copy; ERLE spike → likely removal after 3 | S | none (opt-in path) | stops harming users who enable it today |
| 5. Speakers-output detection + in-recording banner + post-meeting echo toast | S | low (FFI = C fns only) | prevention UX |
| VPIO default-ON / VPIO into archive | S | **disqualifying** (0 dB, ducking, conflicts, AGC) | — **rejected** |
| Separate-track storage (Zoom/Riverside model) | M | doubles lock-model surface; doesn't fix playback doubling | rejected for now |

## Recommendation and first step

Ship options 1+2 now (one PR-able slice, fully headless-verifiable), run the ERLE spike to
formally kill/contain VPIO (4), then 3 behind a flag with a real-Mac bake-off before default-ON,
and 5 alongside. First verifiable slice: pure `suppress_cross_stream_echo(segments, τ, ρ)` +
`estimate_stream_offset(mic, sys)` with RED tests (echo pair collapses; "yes, ship it" after
"let's ship it" survives; short acks survive; mic-only untouched; `others-2` counts as others).

## Open questions / unverifiable headless

- Real ERLE of our `aeccap` on a real Mac (expected ~0 dB per screenpipe's mechanism).
- Whether the Core Audio tap captures pre- or post-duck audio while another app's VPIO is active.
- Actual helper spawn→first-frame latency (sizes the anchor win).
- `sonora` API shape/quality on our tracks; AEC3 behavior under hour-long clock drift on cpal-vs-tap.
- Dedup thresholds on real Polish recordings (start with screenpipe/OpenWhispr values; calibrate).
- Whether macOS audio device names are localized (breaks name heuristics → prefer transport-type).

## Sources

Competitors / mechanisms: screenpipe #3938 (VPIO ERLE ≈ 0 dB, measured), PR #4440 (shipped dedup
thresholds), #4256, #4379; Hyprnote `crates/aec` (DTLN-aec, GCC-PHAT, #5838, #5216, #5880); Meetily
PR #404 (closed), issue #220, `audio_v2/mixer.rs`; docs.granola.ai (Me/Them, no audio stored);
help.krisp.ai (virtual devices); Zoom devforum 69733; Riverside/Descript help; docs.recall.ai FAQ;
OpenWhispr `transcriptText.js` / `meetingEchoLeakDetector.js` / `ipcHandlers.js` / CHANGELOG.
VPIO: Apple docs `voiceProcessingOtherAudioDuckingConfiguration` + `duckingLevel` +
`isVoiceProcessingAGCEnabled`; Apple forums 751100, 710151, 110816, 733733; dev.to Scripta
write-up (−51 dB ducking); Chrome blog "More native echo cancellation"; HN 40918152.
AEC3/DSP: Switchboard "How WebRTC AEC3 works"; Speex manual (delay limits); github.com/
dignifiedquire/sonora; tonarino/webrtc-audio-processing; RubyBit/aec3-rs; fdaf-aec;
Anguera GCC-PHAT; arXiv 2502.12414, 2402.08021, 2501.11378, 2303.00747 (WhisperX).
Code: `audio/merge.rs:25-27,63-113`; `audio/mixer.rs:9-17`; `pipeline.rs:30-46,193-194,220-287,
302-325,407-449`; `audio/recorder.rs:203-209,428-431`; `audio/system.rs:69-73,87-116`;
`audio/aec.rs:19-25,92-152`; `aeccap.swift:46-64`; `audiocap.swift:94-97,206-213`;
`sysaudio.swift:100-107`; `settings/config.rs:86-98,287,428`; `commands.rs:349-367,449-477,
5460-5758`; `transcribe/live.rs:150-163`; FE: `settings-audio-section.component.ts:94-107`,
`record.component.ts:75-80,1036-1039`, `onboarding.component.ts:1334`; `scripts/e2e-mix.sh`,
`examples/e2e_core.rs:42`; `Cargo.toml:28` (+ lock: realfft via rubato, coreaudio-sys via cpal).
