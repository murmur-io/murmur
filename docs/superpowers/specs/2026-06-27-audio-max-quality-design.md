# Murmur Audio Max-Quality Overhaul — Hardened Phased Design

> Status: **APPROVED for implementation** (2026-06-27). Source of truth for the multi-phase build.
> Produced by a 13-agent fleet re-analysis (6 dimensions × research→adversarial-refute + synthesis).
> Trust code, not docs: every load-bearing claim was grounded in `file:line` + external sources,
> then attacked by an independent skeptic. Corrections from that pass are baked in below.

## Locked decisions (user, 2026-06-27)

- **Scope:** all 8 research recommendations, incl. N-way diarization (#8). Prod-ready.
- **System source:** global system tap **minus our own app**.
- **macOS strategy:** Core Audio process tap on **14.4+** (deliberate conservative shipping floor,
  not API availability — symbols ship 14.0/14.2), runtime-gated, **SCK as the only fallback** (13–14.3).
- **Master archive format:** **float32 mono** per-stream (faithful master; accept ~691 MB/h and
  ~3× file-size RAM transient at seal). Opt-in, default OFF → zero disk regression for non-opted users.
- **Echo (#5):** **banner "use headphones" first**, then **full AEC** via `VoiceProcessingIO`
  (speakers-only, attribution/ASR-only, raw mic to archive) as a later phase.
- **Dependencies approved:** Swift `audiocap` helper (tap, crash-isolated) · `sherpa-onnx` crate
  (diarization, Apache-2.0, static onnxruntime) · Silero VAD model (~885 kB, downloaded) ·
  `objc2-core-audio` (+`-types`, BT-transport read / optional tap) · AVFoundation AEC binding.
  Each lands behind a **de-risk spike** (otool minos / universal lipo / notarized spctl) before integration.

## What the adversarial pass corrected (vs. the first solo design)

1. **Prod regression is worse than "missing externalBin".** `build.rs:40` exposes the sidecar via
   `option_env!("SYSAUDIO_BIN")` = a compile-time `OUT_DIR` absolute path that doesn't exist on a
   user's machine; `tauri.conf.json` has no `externalBin`/`resources`; `system.rs:16-19` `.exists()`
   → `is_available()` false → **distributed `.app` records mic-only** (`commands.rs:170-172` gate).
   Plus: `build.rs` compiles **host-arch only** (no `-target`/`lipo`) → a universal `.app` needs a
   lipo'd helper; the sign script uses `codesign --force --deep` (line 54) → mis-signs nested helpers,
   reproduces Tauri **#11992** notarization failure → must move to **inside-out signing**.
2. **Shared-clock mic-in-aggregate (#4/#7 "biggest lever") — REFUTED.** No cited reference
   (AudioCap et al.) adds the mic to the aggregate device. **Keep the proven `merge.rs` wall-clock
   merge** for the tap path too; shared-clock is a real-Mac *experiment*, not a committed design.
3. **`FullParams::enable_vad` is a no-op AND a panic-footgun** on `whisper_full_with_state`
   (whisper.cpp #3402). Use **only** the standalone `WhisperVadContext` already in pinned whisper-rs
   0.16 — no version bump.
4. **`masked_detail` (`commands.rs:1477`) builds the locked DTO with `..meeting` spread** → any new
   audio-path field leaks **un-nulled** to a locked meeting; the existing guard test only asserts
   `audio_path` is None and stays GREEN while leaking. Must null all three + extend the test.
5. **The user-facing speaker timeline is LLM-inferred** from label-less transcript
   (`summarize/timeline.rs`); `rename_speaker` edits that JSON. Raw diarized labels would create
   **two divergent speaker attributions** (violates "SQLite canonical"). **Timeline reconciliation
   is a prerequisite of the diarization phase**, not a follow-on. `sherpa-rs` is archived
   (2026-06-06) → use the **official `sherpa-onnx`** crate.
6. **`objc2-core-audio 0.3.2` DOES export the tap symbols** — the separate Swift helper is a
   deliberate **crash-isolation** choice (in-process Core Audio FFI is the abort class
   `rust-tauri.md §7` warns about), not a necessity.

## Honesty boundary (signed-build-on-a-real-Mac only)

`cargo test --lib`, `ng build`, and `swiftc -typecheck` (all `ci.sh` runs for the sidecar — it never
links/runs it) **cannot prove**: TCC prompts, Touch ID KEK release, shared-clock drift, the
all-zero-buffer watchdog, notarization survival of the private TCC SPI `dlopen`, universal-sidecar
packaging, real capture from Zoom/Meet/Teams, AEC device matrix, WER/DER. These are flagged per phase.

---

## Phases (each ships independently; risky pieces behind runtime feature-gates that degrade to today)

### Phase 0 — Sidecar bundling hotfix (latent prod regression) + signing correctness
Make the EXISTING SCK system-audio capture actually work in a distributed, notarized `.app`. Ship as a
standalone hotfix; unblocks every "others"-stream recommendation.
- `build.rs`: compile the Swift sidecar **universal** (`swiftc -target arm64-apple-macos11 …` AND
  `-target x86_64-apple-macos11 …` → `lipo -create`) into a stable in-crate path (`src-tauri/binaries/`).
- `tauri.conf.json`: add `bundle.resources` mapping the lipo'd helper into `Contents/Resources`.
  **Not `externalBin`** (bakes target-triple into the filename, unresolvable under
  `universal-apple-darwin`, Tauri #3355; notarization #11992).
- `audio/system.rs`: thread `AppHandle` into `SystemAudioRecorder::start`; resolve at RUNTIME via
  `app.path().resolve("meetnotes-sysaudio", BaseDirectory::Resource)`; keep `option_env!` as **dev-only**
  fallback. No new crate (raw `Command::new` stays).
- `scripts/macos-sign-notarize.sh` + `release-murmur` skill stage 8: replace `codesign --force --deep`
  (line 54) with **inside-out** signing — helper first (`--options runtime --timestamp` + entitlements),
  app last (no `--deep`), `codesign --verify --strict --verbose=2`, re-sign DMG.
- `sysaudio.swift`: `SCContentFilter excludingApplications:[]` → `[Murmur's SCRunningApplication]`
  (global-minus-self on the SCK path, decision #2).
- `Info.plist`: add `NSAudioCaptureUsageDescription` now (additive; the later tap path needs the copy).
- **DoD:** `cargo test --lib` + `ng build` green; `find Murmur.app -name meetnotes-sysaudio` returns a
  binary inside `Contents/Resources`; `lipo -info` shows arm64+x86_64. **Real-Mac:** notarized `.app`
  produces a non-empty system WAV during a real call; `spctl -a -vvv` says Notarized Developer ID.

### Phase 1 — Input-device picker + first-sample timestamp anchor (cheap cpal wins, zero new deps)
- `settings/config.rs`: add `input_device: Option<String>` + `K_INPUT_DEVICE` (mirror `vault_path`, additive).
- `commands.rs` + `lib.rs`: `list_input_devices()` command (registered in `generate_handler!`)
  enumerating `host.input_devices()` → names + default flag. Persist **by name** (cpal exposes no stable ID).
- `audio/recorder.rs`: `build_and_play` takes `Option<&str>`; Some→match name, None/not-found→default
  with a non-PII "saved device gone" log (device names are PII-adjacent → keep out of logs).
- Update **both** `Recorder::start` callers (`audio/listener.rs:70` voice-trigger + `commands.rs:146`).
- **#7 anchor:** in `recorder.rs` capture both `info.timestamp().capture` (StreamInstant) AND
  `Instant::now()` in the SAME first callback, compute the offset, report the true capture-start anchor.
- FE: `models.ts` + `ipc.service.ts` (`listInputDevices()`) + `settings.component.ts` (`<select>`,
  same pattern as `modelSize`).
- **DoD:** `cargo test --lib` + `ng lint` + `ng build` green; unit test for default-fallback.
  **Real-Mac:** stable device names across built-in/USB/AirPods; first-callback `capture` populated+monotonic.

### Phase 2 — ASR quality: Silero VAD pre-segmentation + context-reset across gaps + ASR-only loudness
Biggest ASR quality+speed win (#1) + cheap loudness win (#6), using only standalone `WhisperVadContext`
(pinned whisper-rs 0.16). No lock-model impact.
- `transcribe/vad.rs` (new): `WhisperVadContext::new(silero_path, …set_use_gpu(true))` once, reused for
  both streams; `segments_from_samples` → speech spans (cs→16k samples). Use the **0.16 setters**
  (`set_threshold`, `set_min_speech_duration`, `set_min_silence_duration`, `set_max_speech_duration`,
  `set_speech_pad`, `set_samples_overlap`). **Never** `FullParams::enable_vad`.
- `transcribe/model.rs`: new `vad_model_url()` → `ggml-org/whisper-vad` (`ggml-silero-v5.1.2.bin`, ~885 kB);
  reuse `download_model` atomic `.part`→rename. On missing/failed download → transcribe whole buffer (never block).
- `pipeline.rs`: per-stream (mic_16k, sys_16k, BEFORE `merge_streams`) run VAD; **group** adjacent speech
  spans separated by `< gap_threshold` into ONE contiguous decode (preserves `set_no_context(false)` within
  a run), **start a fresh decode after a long gap** (resets context → kills repetition-loop hallucination
  through silence). Re-offset every returned segment by its region start so `merge_streams` coordinate is
  unchanged (muted/zero spans yield no VAD segment → skip-muted falls out free). **This unifies #1
  (slice-per-region) with the reset-across-gaps idea — grouping is the reconciliation.**
- `audio/wav.rs`: pure-Rust `normalize_for_asr(&mut [f32])` (peak/modest-RMS scalar gain, clamp, no
  attack/release). Apply to mic_16k/sys_16k AFTER VAD and AFTER `archive_16k` is written. **Never touch the
  master/archive.** Unit-test archive bytes unchanged.
- `settings/config.rs`: VAD enable flag (default ON for Accurate batch only). Leave `live.rs` (Fast/greedy) untouched.
- **Lock note:** no new disk content (VAD in-RAM in existing `spawn_blocking`), no `audio_path`, no
  convertFileSrc path, archive byte-identical; only new artifact is the 885 kB Silero `.bin` in `models_dir()`.
- **DoD:** `cargo test --lib` (VAD offsetting, muted-span timeline preservation, archive-byte-unchanged,
  gap-grouping) + `ng lint` + `ng build` green. **Real-Mac:** WER on PL+EN audio VAD on/off; batch wall-time.

### Phase 3 — Core Audio process tap (14.4+) behind a runtime gate, SCK 13–14.3 fallback **[NEEDS-MAC]**
- **New helper** `audiocap.swift` (crash-isolated, separate process): translate `getpid()`+helper PIDs via
  `kAudioHardwarePropertyTranslatePIDToProcessObject` → `CATapDescription(stereoGlobalTapButExcludeProcesses:[self])`
  → `AudioHardwareCreateProcessTap` → `AudioHardwareCreateAggregateDevice` (output sub-device + tap via
  `kAudioAggregateDeviceTapListKey` + drift compensation) → `AudioDeviceCreateIOProcIDWithBlock` →
  `AudioDeviceStart`; teardown reverse. **CONFIRM on a real Mac** whether the exclude arg takes PIDs-as-NSNumbers
  or translated AudioObjectIDs (wrong type = silent no-exclude or creation failure).
- **TCC:** `dlopen` TCC.framework + `TCCAccessPreflight/Request("kTCCServiceAudioCapture")` behind an
  `ENABLE_TCC_SPI` flag, graceful fallback. Surface OSStatus generically as `AppError::Audio` (don't hardcode
  -10875/-10876).
- **All-zero watchdog** inside `audiocap`: per-callback RMS; if ~0 for N s while IOProc alive → full
  teardown+rebuild of BOTH tap AND aggregate (Apple forum 825780). Generous window + bounded rebuild count.
- `audio/tap.rs` (new) + `system.rs`: runtime selector — `>=14.4` (deliberate floor) AND audiocap available AND
  TCC not denied → tap; 13–14.3 AND SCK → Phase-0 SCK helper; else None (mic-only). Use
  `ProcessInfo.operatingSystemVersion` — **no throwing selector**.
- `build.rs` + `tauri.conf.json` + signing: `audiocap` also universal-lipo'd, bundled in `Contents/Resources`,
  signed inside-out (Phase 0 machinery, now TWO helpers).
- **Lock-model:** tap WAV writes into existing `AUDIO_SUBDIR`, flows through the SAME `crypto::encrypt_file`
  verify-before-destroy seal + masked-DTO `audio_path:None` gate; clean up the helper temp WAV. No new
  convertFileSrc/asset path.
- **Shared-clock EXPERIMENT (do not commit):** optionally add the mic as an aggregate input sub-device, but
  keep `merge.rs` wall-clock merge as the guaranteed path until a 60-min real-Mac recording proves drift-free
  alignment AND cpal's mic reconciles into the aggregate format.
- **DoD:** `cargo test --lib` (selector/OS-branch/AppError mapping) green. **Real-Mac 14.4+:** TCC prompt +
  purple dot; global-minus-self captures Zoom/Meet/Teams excluding our own output; exclude-arg type compiles
  against the shipping SDK; watchdog rebuild restores PCM without false-firing on silence; SCK fallback on
  13–14.3; notarization accepts the private TCC SPI dlopen + two embedded signed helpers.

### Phase 4 — Per-stream float32-mono masters (#3): additive schema, master-SET seal lifecycle **[LOCK-SEC]**
- `storage/db.rs migrate()`: add two nullable columns via `add_column_if_missing` — `mic_master_path TEXT`,
  `sys_master_path TEXT`. Keep `audio_path` as the single PLAYBACK artifact (FE's only convertFileSrc consumer).
  Additive/idempotent. Legacy rows NULL → lock code treats NULL-master as "nothing to seal".
- **Ordinal discipline:** `row_to_meeting` (db.rs:1811+) reads POSITIONALLY with `folder_id` trailing across
  **7 SELECTs** — insert the new columns at a consistent position across ALL 7 AND bump indices; **add a test
  exercising EACH of the 7** (`get_meetings_by_tag` has none today; a mismatch is a runtime panic).
- `commands.rs`: refactor the per-meeting audio block in the FOUR lock helpers (`seal_folder_extras`,
  `unseal_folder_extras`, `reblank_folder_extras`, `unseal_folder_extras_permanent`) into ONE helper called
  per non-NULL artifact. Each INDEPENDENTLY: `encrypt_file` (verify-inside) → remove plaintext → re-point its
  column at `<path>.enc`. **Verify-before-destroy per file** so a crash mid-loop leaves already-sealed `.enc` +
  not-yet-sealed plaintext, never lost audio. `.ends_with(ENC_SUFFIX)` idempotency guard PER column.
- `delete_meeting` (commands.rs:404-410): remove every master + every `.enc` sibling (today removes only `audio_path`).
- **LOCK LEAK FIX (showstopper):** `masked_detail` (commands.rs:1477) `..meeting` spread → explicitly override
  `mic_master_path:None, sys_master_path:None, audio_path:None` AND extend the guard test (commands.rs:2350) to
  assert ALL THREE None.
- Masters **export-only** via NEW gated commands `export_mic_master`/`export_sys_master` calling
  `meeting_is_unlocked` (mirror `export_audio:478`). FE never feeds them to convertFileSrc.
- Cost guardrails: `keep_hires_masters` bool (default FALSE) — off = exactly today's 16k mix, zero regression.
  On = mic-native-mono + system-**MONO**-48k float32. 16k ASR feed stays in-RAM only.
- **Memory honesty:** `encrypt_file` holds read+ciphertext+verify buffers (~3× file size, ~2.1 GB for a 700 MB
  master) → cap/warn on very long recordings. Streaming AEAD is v2 (needs a crate + lock-security review).
- **DoD:** `cargo test --lib` (per-master round-trip byte-identical; masked_detail nulls all three;
  partial-seal reseal convergence; each of the 7 SELECTs correct) + `ng build` green. **LOCK-SECURITY-REVIEWER
  sign-off REQUIRED.** **Real-Mac:** seal→relock→auto-relock→unlock round-trip with real Touch ID KEK release;
  real peak memory; actual float32 WAV size/header.

### Phase 5 — N-way diarization on the "others" stream (#8) + timeline reconciliation **[NEEDS-APPROVAL ✓ / LOCK-SEC]**
- **New crate (approved):** official `sherpa-onnx` v1.13.x (Apache-2.0, static onnxruntime, full
  `OfflineSpeakerDiarization` API). NOT archived `sherpa-rs`. No ORT symbol conflict with whisper-rs.
- **De-risk spike FIRST:** throwaway bin, run the offline-speaker-diarization example on a 2-speaker WAV;
  `otool -l` for `LC_BUILD_VERSION minos` vs 11.0; if >11.0 rebuild from source with
  `CMAKE_OSX_DEPLOYMENT_TARGET=11.0` (`build.rs` honors `SHERPA_ONNX_LIB_DIR`) AND lipo universal; confirm a
  signed+notarized universal bundle passes `spctl`. **Universal lipo of static-ORT slices + notarization is
  the dominant ship risk.**
- `pipeline.rs`: after sys_16k, BEFORE `merge_streams`, when `capture_system_audio` AND a new `diarize_others`
  setting are on, run `OfflineSpeakerDiarization::process(&sys_16k)` (FastClustering num_clusters=-1,
  threshold ~0.5). Relabel each "others" Segment by max-overlap diarized span → `others-0/1/2` ("others" =
  single-cluster fallback). "me" untouched. Cost is zero only when OFF.
- **TIMELINE RECONCILIATION (prerequisite):** either feed `Segment.speaker` labels INTO timeline generation OR
  build the timeline deterministically from `Segment.speaker`, and point `rename_speaker` at the single
  reconciled source. Real, scoped work in this phase.
- Models: ~45 MB on-demand (sherpa-specific `csukuangfj/*` ONNX: pyannote-segmentation-3.0 + 3D-Speaker/CAM++
  embedding, optional Silero VAD) via generalized `models_dir()` download.
- `config.rs` + FE: `diarize_others` bool (default false), DISABLED when `capture_system_audio` off. Frame
  labels as editable best-effort (~12–15% DER clean, worse on VoIP); `rename_speaker` is the correction.
- **Lock-model:** diarization relabels `seg.speaker` on an in-RAM buffer BEFORE `insert_segments`; text seals
  via `text_blob`, gated by `meeting_is_unlocked`; no new command/audio-serve path.
- Cross-meeting identity (others-0 in A ≠ B) is OUT OF SCOPE (a `SpeakerEmbeddingManager` enrollment follow-on).
- **DoD:** `cargo test --lib` (relabel-by-overlap, single-cluster fallback, timeline-from-segments) + crate
  compiles/links. LOCK-SECURITY note + confirm ONE canonical speaker source. **Real-Mac:** spike before
  integration; real wall-time + DER on recorded Zoom/Meet "others" at 16k.

### Phase 6 (deferred within #5 echo) — Full AEC via VoiceProcessingIO **[NEEDS-MAC, lowest confidence]**
Banner "use headphones" ships in an earlier phase as the cheap fix. Full AEC: AVFoundation
`setVoiceProcessingEnabled(true)`, speakers-only (route detection), AEC'd mic → attribution/ASR only, raw mic →
archive. Degrade to raw on -10875/-10876. New FFI surface — guard so failure → raw mic, never abort.
**Real-Mac:** builds without -10875/-10876 on built-in speakers; whether a cpal raw stream and an AVAudioEngine
VPIO stream can coexist on the same input (decides the raw-archive/AEC-attribution split feasibility).

---

## Real-Mac acceptance checklist (the honest bar — needs a signed Developer-ID build)

- [ ] P0: fresh notarized universal `.app`, Screen-Recording TCC granted → non-empty system WAV during a real call.
- [ ] P0: `spctl -a -vvv` = Notarized Developer ID; inside-out-signed lipo'd helper embedded.
- [ ] P1: stable device names (built-in/USB/AirPods); non-default selection captures; first-callback capture ts monotonic.
- [ ] P1.5: `kAudioDevicePropertyTransportType` returns BT correctly; audible HFP downgrade reproduced with AirPods mic.
- [ ] P2: WER VAD on/off (PL+EN, multi-speaker); batch wall-time; Silero loads on Metal alongside large-v3; archive byte-identical.
- [ ] P3: TCC prompt + purple dot on 14.4+; global-minus-self excludes our output on Zoom/Meet/Teams; exclude-arg type correct.
- [ ] P3: all-zero watchdog rebuild restores PCM across 44.1↔48 / AirPods sleep-wake without false-firing.
- [ ] P3: SCK fallback on 13–14.3; notarization accepts private TCC SPI dlopen + two embedded signed helpers.
- [ ] P3 (experiment): mic-in-aggregate drift-free over 60+ min AND cpal mic reconciles — gate before retiring wall-clock merge.
- [ ] P4: seal→relock→auto-relock→unlock round-trip of masters with real Touch ID KEK release; real encrypt_file peak memory; real float32 WAV size.
- [ ] P5: de-risk spike (otool minos, universal lipo, notarized spctl) BEFORE integration; real wall-time + DER on recorded "others".
- [ ] P6 (AEC): builds without -10875/-10876 on speakers; cpal raw + VPIO coexistence on the same input.

## Open questions still to resolve at the relevant phase

- P3 macOS floor: confirm 14.4 (AudioCap's marketed min) vs 14.2 (true symbol availability) — affects tap-vs-SCK split.
- P5: if the sherpa-onnx spike forces minos >11.0 and a from-source 11.0 build is impractical → raise
  `minimumSystemVersion` (drops older Macs) or defer diarization? Escalate, do not decide unilaterally.
- P3 shared-clock: worth the real-Mac investment, or commit to wall-clock `merge.rs` permanently for both paths?
