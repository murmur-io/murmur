# Murmur Program Spec — Five Workstreams, Consolidated & Corrected

*Synthesized 2026-07-26 from five ground-truth maps, five designs, and five adversarial critiques. Every load-bearing contested claim below was re-verified against the current tree before inclusion; verification results are inline. This document replaces further design work.*

---

## 1. Executive summary

When all five land, Murmur stops making the user wait and stops making them guess. You press Stop and can immediately start the next meeting — the note writes itself in the background, visibly, resumably, and it yields the moment you record again. You pick transcription quality on a four-rung "power" slider that says what each level *does for you* rather than naming an ML checkpoint, with the level Murmur picked for your actual Mac (RAM, Apple Silicon vs Intel) badged and explained. Every screen speaks plain English — no egress, no GGUF, no posture, no ciphertext — while every privacy fact gets *more* explicit, not less, enforced by a CI vocabulary gate. Settings drops from ~70 everyday controls to ~30 behind real progressive disclosure. And Murmur updates itself: one calm daily check, a quiet download, Apple-notarisation verified on your own Mac before anything is touched, installed when you quit — never restarting on its own, never interrupting a recording, always able to put the old version back.

---

## 2. Cross-workstream coupling

### 2.1 The dependency graph

```
                     ┌─────────────────────────────────────────┐
                     │ P0  FOUNDATIONS                          │
                     │ mur-progress, mur-disclosure, mur-select │
                     │ CVA+value, copy modules, ErrorCopySvc,   │
                     │ sizeLabel(), PipelineBusy guard,         │
                     │ isPolish decouple, dead-code delete      │
                     └───────┬──────────────┬──────────────┬────┘
                             │              │              │
              ┌──────────────▼───┐   ┌──────▼──────┐   ┌───▼──────────────┐
              │ P1 CATALOG (Rust)│   │ P6 QUEUE    │   │ P8 UPDATER core  │
              │ machine profile  │   │ core        │   │ (needs Pipeline- │
              │ + whisper registry│  │ (BE+FE      │   │  Busy from P0)   │
              │ + recommendation │   │  atomic)    │   └───┬──────────────┘
              └──────┬───────────┘   └──────┬──────┘       │
                     │                      │              │
              ┌──────▼───────────┐   ┌──────▼──────┐   ┌───▼──────────────┐
              │ P2 POWER SLIDER  │   │ P7 QUEUE    │   │ P9 UPDATER auto  │
              │ (consumes P1 DTO)│   │ surface     │   │ (scheduler,      │
              └──────┬───────────┘   └─────────────┘   │  on-quit, chip)  │
                     │                                  └──────────────────┘
              ┌──────▼───────────────────────────┐
              │ P3 VOCAB machinery + errors      │
              │ P4 SETTINGS IA (disclosure)      │
              │ P5 VOCAB long tail + gate=FAIL   │
              └──────────────────────────────────┘
```

### 2.2 Hard ordering constraints (each with its reason)

| Constraint | Why |
|---|---|
| **P1 before P2** | The slider renders `WhisperModelDto[]` + `recommendedId`. Without the Rust catalog the FE would need a fourth hardcoded size table — the exact duplication P2 exists to delete. |
| **P1 and P2 own ONE registry** | The "recommendation" and "power slider" designs independently proposed a `WhisperModel` registry + DTO. **Merged**: one `src-tauri/src/transcribe/catalog.rs`. Landing them separately would create two competing registries. |
| **P2 before P3–P5** | The vocabulary pass must rename the *new* surface once, not the old nine-option `<select>` and then the new slider. Reversing this doubles the copy work and re-breaks `e2e/onboarding/model-presence-race.spec.ts` twice. |
| **P0 before P6 and P8** | `PipelineBusy` (RAII counter on `retry_transcription` / `resummarize` / startup salvage) is the *shared* interlock primitive. Verified: `perf::recording_has_priority()` (perf.rs:298) is `active.is_some()`, and `begin_recording_session()` has exactly ONE non-test caller — `commands/mod.rs:1014` `start_recording`. `retry_transcription_prep` (mod.rs:5334) *checks* that flag, meaning while it runs the flag is **false**; `resummarize` (mod.rs:5221) never checks it at all. Both the queue worker and the updater need this counter to be correct. |
| **P6 before P9** | P6 narrows `recording_has_priority()` from "Start→note exported" to "Start→end-of-Draining". An updater that shipped Automatic-install first would silently gain permission to install *during* the note pipeline. `PipelineBusy` closes that, but the queue worker MUST take the guard — a named, tested requirement in P6. |
| **P8 before P9** | Manual "Download & Install" is independently shippable and exercises the whole verify/stage/swap/rollback chain under user supervision before any of it runs unattended. |
| **P6 is atomic (BE+FE in one PR)** | A partial landing surfaces the raw `provider unavailable: another recording model session is already active` to users. The RED test `record-while-processing.spec.ts` is the acceptance gate that both halves are present. |

### 2.3 Shared-file collision map (the merge-conflict surface)

Every file below is touched by 2+ workstreams. Sequential PRs make each safe; **fanning these out in parallel is the merge-skew shape that has bitten this repo before.**

| File | Touched by (in landing order) | Nature of the collision |
|---|---|---|
| `src/app/features/onboarding/onboarding/onboarding.component.{ts,html,scss}` | **P2** (model step rewrite, delete `SIZE_HINTS`, delete `modelSize=signal("small")` at :113 and `?? "small"` at :269) → **P4/P5** (posture tiles, Kong removal, Ollama copy, "Enable the brain" → "Turn on memory and search") | The single hottest file in the program. P2 restructures the model step; P5 rewrites the provider + brain steps. The `modelPresenceRequestId` triple stale-result guard must survive both, verbatim. |
| `src/app/features/settings/sections/settings-transcription-section/*` | **P2** (card 1 → `<app-model-power>`) → **P4** (cards 2+3 behind `<mur-disclosure>`, renames) → **P7** (the "When recordings are processed" policy card) | Three structural rewrites of one section. |
| `src/app/features/settings/settings.store.ts` | **P2** (delete `updateDownloadHint` + 9-row map, add `modelSizeValue`/`setModelSize` bridges) → **P3** (`CONNECTION_LABELS` → `glossary.ts`) → **P7** (policy form controls) | |
| `src-tauri/src/transcribe/model.rs` | **P1** (registry, `recommend()`, `total_ram_bytes` pub(crate), `quality_tier_label`) → **P2** (cancel-generation threading, `delete_whisper_model_files`) | Merged registry; the six existing `default_model_size` tests must pass **unedited** through P1. |
| `src-tauri/src/settings/ai_map.rs` | **P1** (transcription `model` cell → tier label; the assertion that breaks is **:181** `assert_eq!(tr.model, default_model_size_now())`, *not* the :165/:211/:234 brand names) → **P3** (`"On-device NER"`→`"Name masking"`, `"Retrieval only (no model)"`→`"Search only — no AI writing"`) | |
| `src-tauri/src/commands/models.rs` | **P0** (`PipelineBusy` is separate, but the 5 download commands take a busy guard here) → **P1** (`machine_profile`, `list_whisper_models`, `whisper_recommendation`) → **P2** (`delete_whisper_model`, `cancel_model_download`, `fileIndex`/`fileCount`) | |
| `src-tauri/src/events.rs` | **P1** (none — machine-change becomes a *pull*, see §3.3) → **P6** (`EVENT_PROCESSING_QUEUE`, `StatusPayload.progress/total_s`) → **P8** (`EVENT_UPDATE`) | |
| `src-tauri/src/lib.rs` | Every PR (`generate_handler!` + `setup` wiring) | Serial cadence makes this trivial; parallel would guarantee conflicts. |
| `src/app/app-shell/app-shell.component.{ts,html,scss}` | **P5** (nav `Graph`→`Connections`, relock toast copy) → **P7** (Processing chip + panel mount) → **P9** (Update chip) | Three chips land in `.sidebar-footer` above `.unlocked-pill`; ordering avoids three-way conflict. |
| `src/app/features/record/record/record.component.{ts,html}` | **P5** (provider labels, consent banner via `ErrorCopyService`) → **P6** (delete `isProcessing`, drop `!isBusy()` from `canRecord`, hand-off strip) | P5's `needsCloudConsent` switch from prose regex to `code === "cloud-consent"` must land before P6 touches the same file. |
| `src/app/core/{models.ts,ipc.service.ts}` | All PRs | Append-only; safe in series. |
| `e2e/onboarding/model-presence-race.spec.ts` | **P2** (rewrite against the slider) → **P4** (the control moves behind a disclosure) | Must stay RED-before-GREEN in both rewrites — verify by reverting `modelPresenceRequestId` and watching it fail. |
| `src/app/features/settings/sections/settings-about-section/*` | **P5** (keywords, licence lines preserved) → **P8** (Software update block rewrite) | |
| `scripts/ci.sh` | **P3** (vocabulary gate, WARN) → **P5** (gate → FAIL) | |
| `scripts/macos-sign-notarize.sh` + `.claude/skills/release-murmur/SKILL.md` | **P8** (app-level staple + `.sha256` sidecar) | Isolated; can land any time before P9. |

---

## 3. The corrected designs

### 3.0 Program-wide corrections (apply to all five)

Three claims appeared in multiple designs and are **wrong**. They are corrected once here.

1. **`.field-help` is NOT a global primitive.** Verified: absent from `src/app/design-system/primitives.css` and `src/styles.css`; it is redeclared per-section (`settings-notes-section.component.scss:33`, `settings-audio-section.component.scss:27`, `settings-transcription-section.component.scss:92`, `model-effort-picker.component.scss:224`, `ai-connection-card.component.scss:82`). **P0 promotes it to `primitives.css` once and deletes the five copies.**
2. **There are SEVEN hand-rolled progress bars, not four.** Verified `grep -rln "progress-track" src/app`: onboarding, brain-posture-block, local-models-list, model-effort-picker, on-device-intelligence-block, settings-privacy-section, settings-transcription-section. **P0 ships `mur-progress`; P0 migrates all seven** (it is mechanical and prevents the count growing to nine).
3. **The global reduced-motion block already exists** (`src/styles.css:112-118`, `*, *::before, *::after { transition-duration: 0.01ms !important; animation-duration: 0.01ms !important }`). Per-component `prefers-reduced-motion` blocks are belt-and-braces only — say so, don't present them as the mechanism. One exception is legitimate: a keyframe whose `from` state hides content (`opacity: 0`) needs a component-level `animation: none` so the content is not left invisible.

---

### 3.1 W1 — Background processing queue

**The one architectural change stands, verified.** `commands/mod.rs:1891` `transition_to_postprocess()` → `:1940` `run_file_backed(...)` → `:1962` `model_session.finish()`; `perf.rs:521` `begin_recording_session` refuses while `active.is_some()`; `finish()` is the only clearer. Ending Stop at enqueue frees the slot.

#### Accepted corrections

**A1 — LOCKED holds are a REACHABLE, normal state. The "unreachable" argument is deleted.**
Verified in `commands/lock.rs`: `if folder.locked { … finish_folder_lock_after_seal(...); return Ok(()); }` returns **before** the `folder_has_nonterminal_recording_generation` refusal further down. That early return is the *common* case (folder `locked=1`, merely session-unlocked). `relock_folder` (lock.rs:658) and `relock_all_inner` (lock.rs:732) never check it at all. Consequence: the whole checkpoint-safety argument that rested on "a pending job's folder can never be locked" is void.

**A2 — `postprocess_asr_windows` is first-class sealed content, cleared on EVERY relock path.**
Verified: `relock_folder` and `relock_all_inner` both funnel through `commands/mod.rs:8139 reblank_folder_extras`, which knows only `raw_segments`/`raw_timeline`/the session WAV. `seal_folder_extras` (mod.rs:7395) is a *different* function on a *different* path.
→ `clear_asr_windows_for_folder` + per-stream cursor reset goes into **both** `reblank_folder_extras` **and** `seal_folder_extras`. It is a pure cache drop (the verified archive WAV remains the source), never content loss. RED test: seed a checkpoint row, call `relock_all_inner`, assert zero rows.

**A3 — TRANSCRIBE→SUMMARIZE is a single NON-yielding unit.**
`pipeline.rs:1080 db.replace_segments(...)` writes plaintext segments; the seal happens later (`seal_auto_filed_note`). `reblank_folder_extras` skips a segment set with no `text_blob` (`mod.rs:8156 if s.text_blob.is_some() && !s.text.is_empty()`). A job parked at the stage boundary under `manual` policy would leave unsealed plaintext indefinitely. **Yield boundaries are: (a) an ASR chunk edge, (b) *before* the ASR stage begins. Never between `replace_segments` and the terminal seal.**

**A4 — Pause on a RUNNING job gets a real mechanism.**
`should_yield` becomes composite: `perf::background_should_yield() || postprocess::hold_requested(meeting_id)`, backed by a process-global `Mutex<HashSet<String>>` set by `hold_processing_job` **before** its CAS, plus `background_yield_notify().notify_waiters()`.

**A5 — Yield latency is sub-window, and the 30 s Start budget is the binding constraint.**
Verified `pipeline.rs:1134` `const WINDOW_FRAMES: usize = 120 * audio::TARGET_RATE_HZ` — 120 **seconds of audio** per window, tens of seconds of large-v3 decode. Plus an uninterruptible `Transcriber::load` of a 2.9 GB model at `pipeline.rs:1036-1042`. `start_recording` hard-fails after 30 s with `"local AI did not quiesce in time to start recording safely"`.
→ `should_yield` is checked **inside `transcribe_stream`'s per-VAD-chunk loop**, not only at window edges. Additionally the worker refuses to *claim* a new job when the whisper model is not already resident. Wall-clock Stop→Start must be **measured on a real Mac**; a green unit test is not proof.

**A6 — The ASR checkpoint is written by a sink passed INTO the closure, and resume is per-stream.**
Verified: `transcribe_raw_windows` is called twice — mic at `pipeline.rs:1043`, system at `:1056` — both inside ONE `run_heavy_maybe_recording` closure opened at `:1036`. `run_file_backed_inner` never sees intermediate windows.
→ Signature gains `sink: &dyn WindowSink` (an `Arc<Db>` + meeting id + stream tag behind a trait) that persists each window **as it completes**, so the checkpoint is a *crash* checkpoint as well as a pause checkpoint. `AsrOutcome::Yielded` then carries only `next_frame`.
→ **`postprocess_jobs.asr_cursor_frames` is DELETED.** Resume is derived: `SELECT stream, MAX(end_frame) FROM postprocess_asr_windows WHERE meeting_id=? GROUP BY stream`. A single scalar cannot express "mic complete, system at frame N". A denormalised `transcribed_s` stays for the progress DTO only.

**A7 — Progress denominator is total decodable frames across both streams**, not `audio_duration_s` (which would stall at 50 % then restart). Copy: `Transcribing… 12 of 31 min` where both numbers are stream-summed.

**A8 — The admission gate is corrected and completed.**
- `topic_backfill_ram_permits_now()` is called **alone** — verified `transcribe/model.rs:391` already ends in `crate::perf::heavy_op_permitted(floor_ok)`. The design's double-wrap is deleted, as is the false claim that this is the first batch-pipeline consumer (it is used at `audit.rs:414`, `lib.rs:741`, `commands/documents.rs:81`, `commands/models.rs:507`).
- **Add the standby voice listener** to the gate: it owns Whisper RAM (`lib.rs` restarts it only after recovery relinquishes priority; `start_recording` stops it at `mod.rs:1016`). Either idle while it holds a model, or stop/restart it around a claimed job the way `start_recording` does.
- Add `perf::startup_recovery_has_priority()` (verified `perf.rs:212`).

**A9 — Worker spawn ordering is a HARD constraint, not a soft one.**
Verified `lib.rs:559` calls `perf::begin_startup_recovery()` **synchronously** on the setup thread; on `Err` → `show_fatal_init_dialog(...); return Ok(())` — the app is dead. `perf.rs:194-197` refuses when `!state.generations.is_empty()`. A worker that claims a generation before line 559 **hard-kills launch**, violating the binding "startup must never hard-crash" rule. The spawn goes strictly after, with a comment at the site.

**A10 — Retry copy tells the truth about degradation.**
Verified `pipeline.rs:1194-1197` doc: `run_salvage_from_disk` re-runs from the combined archive → single-stream, every segment attributed to `me`, no diarization. Row copy: **`Try again — speaker labels will be lost`**. The WAV path comes from the archive assertion (`audio_dir()?.join(assertion.basename())`), gated by `meeting_is_unlocked` first.

**A11 — `list_processing_jobs` states its gate literally.**
There is no reusable title-masking helper — `get_meeting_detail` uses `masked_detail(anchor)` (`commands/meetings.rs:502`), a whole-DTO builder. Per row: `if !meeting_is_unlocked(state, &id)? { title = "🔒 Locked"; durationS = None; transcribedS = None; folderName = None; }`. Never an on-disk path. RED test named in the verification bar.

**A12 — `between_meetings` gets a persisted anchor.** `last_capture_end_ms` initialised from `MAX(meetings.ended_at)` at boot, not `0` — otherwise the policy silently degrades to `immediate` after every relaunch. Explainer copy states the rule plainly: *"Waits until you've been done recording for N minutes."*

**A13 — Offline is a hold, not a failure.** New `hold_reason='OFFLINE'`. Network-classified errors do **not** increment `attempts`; copy: `Waiting — you're offline`. Without this a cloud summarize on a plane burns all three retries in ~45 s and becomes permanently `FAILED`.

**A14 — Settings and the panel do NOT share a form.** Verified `settings.store.ts:104` is bare `@Injectable()`, provided at `settings.component.ts:91` and `brain-enable-card.component.ts:38`. The shell-mounted panel calls `set_processing_policy` directly (which already validates + clamps server-side) and reads a small root signal-holder; `SettingsStore` refreshes from the same command.

**A15 — `mur-segmented` is not a CVA.** Verified `segmented.component.ts:33 readonly value = model("")`. Bind `[(value)]` + an explicit write, in both the Settings card and the panel. (P0 does **not** add CVA to `mur-segmented`; the two-way binding is sufficient and lower-risk.)

**A16 — Minor fixes:** drop `CHECK (updated_at_ms >= enqueued_at_ms)` (an NTP correction must never wedge a state machine — write `updated_at_ms = MAX(?, enqueued_at_ms)` instead); audit `src-tauri/src/mcp.rs` and the search/graph readers for the new `QUEUED` variant; reconcile the 20 s `finish_task` timeout continuation (which already writes `Error` + `release_for_recovery`) against the enqueue path so both cannot target the same generation.

#### Rejected / partially rejected

- **Rejected: adding a third owner kind to `perf.rs`.** The existing unscoped-generation semantics already encode the priority we want, and a new owner kind fans out through `acquire_work_lease`, `run_heavy_maybe_recording`, `make_provider_resolved` and all three `acquire_external_egress_lease` sites in `summarize/redact.rs`. The two-function `perf.rs` addition (`background_should_yield`, `background_yield_notify`) stands.
- **Partially rejected: "cancel".** The critique is right that refusing a cancel state is a scope reduction the user should be told about, and it is now in §5. But the design's reasoning holds: `Pause` covers "not now", `Discard` covers "gone", and a third "cancelled-but-half-processed" state would be a lie about what happened to the content. **Kept as designed, surfaced as a user decision.**
- **Rejected: making `Processing` the queue's problem to define for the updater.** It is the updater's `PipelineBusy` guard (P0); the queue worker takes one. Two owners of one truth is worse than one.

#### The plaintext-WAV window (new, disclosed)

A `HELD`/`LOCKED` or `manual`-paused job leaves an unencrypted archive WAV in the scoped audio dir indefinitely. The `convertFileSrc` gate still holds (the masked DTO nulls `audio_path`, `commands/meetings.rs:501-502`), so this is **not** a leak — but "briefly" becomes "days". Stated explicitly in the PR body and the lock-security review scope.

---

### 3.2 W2 — Transcription power slider

#### Accepted corrections

**B1 — `mur-power-slider` owns its own `<input type="range">`.**
Verified `slider.component.html`: one `(input)` listener, no `(change)`, no `[attr.aria-valuetext]`, no `[attr.aria-describedby]`, no keydown, no pointer events. Composing it would require six passthroughs and would break the "Appearance is byte-identical" promise. The new control reuses `mur-slider`'s track/thumb SCSS via shared rules in `primitives.css`, not by wrapping the component.

**B2 — `mur-power-slider` IS a `ControlValueAccessor`.** The design's refusal violated `angular-zoneless.md` §6b and its premise was false: `select.component.ts::onChangeEvent` already demonstrates a CVA that calls `onChange` **only** on `change`. Same here — preview on `input`, commit (and `onChange`) on `change`. Settings then binds `formControlName="modelSize"` directly, and `setModelSize`/`languageValue`/`modelSizeValue` all disappear from `SettingsStore`.

**B3 — `MurSelectComponent` gains `value = model<string>("")` + `disabled = input(false)` alongside the CVA.** Verified: it exposes only `ariaLabel` as an input; `value`/`disabled` are internal signals written solely by `writeValue`/`setDisabledState`, and there is no `output()`. Onboarding has no reactive form, so signal I/O is required there. `select.component.{ts,html}` is now in P0's file list.

**B4 — The card is FULLY CONTROLLED in both hosts.** `[size]` in, `(sizeChange)` out. The catalog's `selectedId` seeds the host on first load and drives `selectedOffCatalog`; it never fights the host. Onboarding's handler sets the signal **then** calls `refreshModelPresence()` in that order (`persistConfig` reads `this.modelSize()` — reversing it persists the old size).

**B5 — `delete_whisper_model` gains a fourth refusal: the live-caption pin.**
Verified: `live_model_pin` serde-defaults to `"small"` and `live_pin_size` returns it unconditionally; on the shipped turbo default the batch model is `large-v3-turbo-q8_0` while live captions run `small`. The design would let a user delete `small` (its "Balanced" rung) with no way back — `modelPresent` reports the *batch* model, so the Download button never appears, and `companion_size` is only reachable from `download_model`.
→ Refuse any size whose resolved filename equals `model_filename(live_pin_size(cfg), language)`, with copy naming it as the live-caption model. **Plus a repair affordance**: when `live_captions::dto_probe` reports `NoModel`, the card shows a `Get the live-caption model` button independent of `modelPresent`.

**B6 — Every RAM number comes from `docs/research/2026-07-09-transcription-performance.md`, rounded UP, with the source line in the registry doc-comment.** The design's figures (Sharp "about 1 GB", Maximum "about 3.1 GB", Balanced "about 0.6 GB") all *understate* the cited source (turbo-q8 ~1.2–1.5 GB, large-v3 fp16 ~3.9 GB, small ~0.9 GB). The invented `"Maximum wants about 16 GB of RAM"` sentence is **deleted** — no `min_ram_gb` for `large-v3` exists anywhere in the tree.

**B7 — The `.en` state is explicit.** `model_filename` appends `.en` only for exact `tiny|base|small|medium`. A user with `ggml-small.bin` who selects English sees Balanced flip to not-downloaded. New §6 row: *"You have the multilingual build; English-only is a separate ~470 MB file — or leave Language on Auto-detect and use what you have."* Either add `approx_download_bytes_en` to the registry, or delete the claim that the size changes per language. (Recommend: delete the claim; the `.en` build's size delta is not measured.)

**B8 — Cancel is a generation counter, not a global bool.** `cancel_model_download` bumps `model_download_generation: AtomicU64`; each download captures it at entry and its `should_continue` closure compares. Self-clearing, immune to the post-completion race, and immune to the bug where a cancelled whisper download kills the next Parakeet download (`download_parakeet_models` is a separate command the design never cleared the flag in). Lives as a module-private static in `transcribe::model`, **not** on `AppState` — verified five exhaustive `AppState { … }` literals exist (`commands/documents.rs:877`, three test builders, `AppState::init_at`) with no `..Default::default()`.

**B9 — Cancel returns `Ok`, not `Err`.** Mapping a user-initiated cancel to `AppError::Unavailable("download cancelled")` is the wrong failure domain and forces the card to string-match. `download_model` returns `Ok(DownloadOutcome::Cancelled)`.

**B10 — The "latent double-subscription" bug is DELETED, along with its verification step.** Verified: `TabRouteReuseStrategy::tabKey` covers only `meeting/:id`, `notes/:id`, `org-item/:id`; `/settings` and `/onboarding` are destroyed on navigate; `SettingsStore` is component-provided and releases its listener in `destroyRef.onDestroy`; both handlers self-gate. The two listeners can never be simultaneously alive. **A verification step that cannot fail is worse than none.** The root `TranscriptionModelsService` remains justified on its real merits: two hosts, and `SettingsStore` dies on leaving Settings.

**B11 — The pre-download file plan uses `companion_pending_in`, made `pub(crate)`.** `companion_size` takes the *post*-download path (`&path`, the `ensure_model` return). `src-tauri/src/commands/live_captions.rs` joins the file list. The "never a second FE-side rule" framing is softened — a pre-fetch plan is unavoidably a derivation — and a test asserts `companion_pending_in` agrees with `companion_size` for the default install (template: the existing `companion_pending_matches_the_download_decision`).

**B12 — Onboarding gets a cold-cache fallback.** Onboarding *is* first launch; the root service has nothing cached, and `list_whisper_models` is fallible (`models_dir()?` → `AppError::Transcribe` on `create_dir_all` failure). The step is the app's only hard gate with no skip. → Label gated: `@if (models().length) { Download {{recName()}} ({{size}}) } @else { Download model }`, and the "Show all sizes" disclosure is **always openable** regardless of catalog state.

**B13 — `whisper_model_path` is modelled.** Verified `resolve_model_path` returns the configured path verbatim when it `is_file()`, **before** any size resolution. `WhisperCatalogDto` gains `custom_path_override: bool`; when true the ladder dims and the card says *"A custom model file is configured in Settings → General; it overrides this choice."* — no badge, no tier word.

**B14 — `#[serde(default)]` on `ModelDownloadPayload` is dropped** (verified `events.rs:178` derives `Serialize` only; the cited `EmbedDownloadPayload` mirror carries no such attribute). Fields added plainly.

**B15 — Delete is gated by `lifecycle_guard`, not only `recording_has_priority`.** Verified `retry_transcription_prep` refuses when `recording_has_priority()` — meaning while a retry runs, that flag is false and a delete would sail through. Use the same guard `retry_transcription_prep` uses.

**B16 — The power rank and the accuracy meter agree.** `large-v3` ranks after `large-v3-turbo-q8_0` **on cost, not on accuracy** — stated in the `power` field's doc comment. The cited research lists `large` at FLEURS Polish 7.2, *worse* than large-v2's 5.4, and flags direct turbo-vs-large-v3 as unpublished. Sharp and Maximum share an accuracy meter; the ordering tiebreak is honest.

**B17 — `live_safe` is USED or removed.** Kept, and consumed by: the delete guard (B5) and a `Also powers live captions` note on Balanced. The catalog↔`is_live_heavy_model_file` equivalence test stays.

**B18 — Minor:** `[disabled]` flipping mid-drag must force a DOM resync (write the element's `value` property directly — the signal-CVA revert-coalescing trap); "Auto-detect plus the existing **nine** languages" (not ten).

#### The ONE ladder (reconciling W2 and W5)

The vocabulary workstream proposed a *different* three-tier ladder (`Fast`=small-q8_0 / `Balanced`=turbo-q8 / `Best`=large-v3). That ladder is **wrong**: verified `default_model_size` returns `"small"` on every <12 GiB Mac and on every existing install without turbo on disk (`transcribe/model.rs`, three branches, presence-first). `small` is not on that ladder, so **"Custom" would be the default display state for a whole class of Macs**.

**Binding: one four-rung ladder, and both backend defaults are on it.**

| Rung | id | size | headline |
|---|---|---|---|
| **Light** | `base` | ~150 MB | Gets the gist. |
| **Balanced** | `small` | ~470 MB | Readable transcripts, small footprint. *(a backend default)* |
| **Sharp** | `large-v3-turbo-q8_0` | ~875 MB | Near-best accuracy, no speed penalty. *(the other backend default)* |
| **Maximum** | `large-v3` | ~3 GB | The heaviest model, for hard audio. |

`tiny`, `small-q8_0`, `medium`, `medium-q8_0`, `large-v3-turbo` live in the "Show every size" table with the honest id in muted mono. `large-v3-q5_0` ships `provisional: true` and is hidden until its size is measured (it has no size anywhere in the repo, and inventing one would be exactly the slop this program forbids).

---

### 3.3 W3 — Hardware-aware recommendation *(merged into P1 with W2's registry)*

#### Accepted corrections

**C1 — `recommend()` and `default_model_size()` are DIFFERENT functions.**
Verified `default_model_size` is path-dependent: (1) turbo file on disk → turbo at any RAM; (2) no whisper `ggml-*.bin` **and** RAM ≥ 12 GiB → turbo; (3) else `small`. The shipping tests pin exactly this (`default_model_size(&["ggml-large-v3.bin"], Some(64*GIB)) == "small"`).
→ `catalog::recommend(total_ram_bytes, apple_silicon) -> Recommendation` is **pure over hardware**; no `models_dir_files` argument. `default_model_size` keeps its three branches and consults `recommend()` only for branch (2). `WhisperCatalogDto` then carries **both**: `recommendedId` (the honest hardware answer, which the badge renders) and `autoDefaultId` (the never-surprise first-run answer). Without this split, `tier` and `batchSize` disagree for **every existing install**, and the flagship "your pick is BELOW the recommendation" state is unreachable.

**C2 — Apple Silicon vs Intel is IN the tier mapping.** The brief named it first; the design probed `hw.optional.arm64` and then never used it. Murmur ships `--target universal-apple-darwin`, and `whisper-rs` runs `features = ["metal"]` — Metal on an Intel iGPU is not the machine the 12 GiB floor was tuned against. **Intel caps at `small` regardless of RAM**, with its own tier and its own reason string. Fail-SMALL on an unreadable arch probe. Also read `sysctl.proc_translated` alongside `hw.optional.arm64` (the Rosetta rationale in the design was probably backwards; both are cheap).

**C3 — The machine-change nudge is a PULL, not a push.**
Tauri does not buffer events and the webview has not called `listen()` during `setup`. The design also overwrote `machine_fingerprint` at emit time, making the nudge unrecoverable. → At `setup`, on mismatch write a `machine_change_pending` settings row **and only then** the new fingerprint. Add `machine_change_nudge()` / `dismiss_machine_change_nudge()` commands; `MachineService.refresh()` reads it. `EVENT_MACHINE_CHANGED` and `MachineChangedPayload` are **deleted**. Precedent in-tree: `brain_model_retirement_nudge` returns `Option<RetiredModelNudge>`.

**C4 — Brain advice raises its floor.** The design told a 16 GB Mac it can run the full on-device brain on a 12.2 GB *estimate*, while citing the **measured** 14 GB main-process recording peak to justify whisper conservatism. Same evidence, two standards. → `Full` requires ≥ 24 GB until a real measurement exists; below that, `Reactions`. Route the whisper term through the same measured constant.

**C5 — The reason string is authored in Rust with a REASON ENUM.** `recommended_reason: { AlreadyDownloaded, FreshInstallAmpleRam, IntelCap, ExistingInstall }`, returned adjacent to `default_model_size` so it cannot drift. Three copy variants; the RAM-causal sentence ("Your Mac has 32 GB, so Murmur picked Sharp") is only used for `FreshInstallAmpleRam`. Rendering one causal sentence for a presence-first decision is the same class of dishonesty the vocabulary workstream exists to remove.

**C6 — `mur-recommend-badge` is DELETED. Use `<mur-pill kind="accent" [dot]="false">✓ Recommended for this Mac</mur-pill>`.** Verified `primitives.css:229` `.pill.is-accent { background: var(--accent-soft); border-color: transparent; color: var(--accent-hover) }` and `pill.component.ts:21-24`'s `dot` input already deliver the entire spec, minus the ✓ glyph. The proposed `1px solid var(--accent-ring)` border would make it the only accent pill in the app with a visible ring. If the pop entrance is wanted, add one `.pill.is-pop` rule to `primitives.css` — that is *extending* the catalog, which the rule endorses.

**C7 — The free-disk NSURL read is wrapped in `objc2::exception::catch`, not `std::panic::catch_unwind`.** `resourceValuesForKeys:error:` can raise `NSInvalidArgumentException`, which `catch_unwind` cannot catch — the exact FFI-abort class `rust-tauri.md` §7 exists to prevent. The `exception` feature is already enabled (`Cargo.toml:170`). `catch_unwind` stays correct for the NSProcessInfo *getters* (the shipped `thermal::read_thermal_level` precedent).

**C8 — Free disk counts the companion.** `pendingDownloadBytes: u64` computed in Rust = batch row + companion when `companion_pending_in` says one is planned. The FE compares against that one number and never sums sizes itself.

**C9 — `model_size_source` is backfilled and reversible.** When absent AND `onboarded == true` AND `model_size` non-empty → persist `"user"` (an existing install's choice is deliberate). Carried as `Option<String>` in the DTO (absent = preserve, `Some("auto")` = reset, `Some("user")` = mark), so the field is not write-once. `Switch to Sharp` sets `"auto"`; the Quality control sets `"user"`.

**C10 — `machdep.cpu.brand_string` is normalised.** Accept only `Apple …` ≤ 24 chars; otherwise drop the chip clause entirely (Intel returns `Intel(R) Core(TM) i7-9750H CPU @ 2.60GHz`, which would read absurdly). Unit-test both shapes against literal fixtures. The marketing-name ban (`MacBook`, `iMac`, `Mac mini`, `Mac Studio`) is mechanically checkable — verified `rg -i 'macbook|imac|mac mini|mac studio' src/app src-tauri/src` returns **zero hits today**, so the gate is green at baseline and worth wiring into the vocabulary scanner.

**C11 — Ship only what a consumer reads.** `total_ram_bytes`, `apple_silicon`, `chip_name`, `free_disk_bytes` cross IPC. `logical_cores`, `perf_cores`, `os_version`, `low_power_mode`, `thermal` are probed if free but **not** exposed until something branches on them. (This also removes the `ThermalLevel: !Serialize` problem — verified `thermal.rs:42` derives no `Serialize`.)

**C12 — One command, not two.** `whisper_recommendation()` embeds the machine profile; `machine_profile()` is dropped (or references, never duplicates). Two sources that re-read volatile fields at different instants can disagree about free disk.

**C13 — `machine.refresh()` fires on download completion** (`p.done` in both progress handlers). Otherwise `downloaded` flags go stale immediately after the one action the screen exists to perform.

**C14 — `MachineService.dismissed` is `_dismissed` + `.asReadonly()` + `dismiss()`.** Rule §2.

**C15 — `ai_map.rs`'s breaking assertion is `:181`** (`assert_eq!(tr.model, default_model_size_now())`), not the brand-name assertions at :165/:211/:234 which the non-goals list explicitly keeps. Also define the `model_by_id(...) == None` fallback (blank `model_size`, or a custom path): render the raw id, never an empty cell.

**C16 — `mur-banner` has no dismiss.** Verified `banner.component.html` is icon + `<ng-content/>`, and `banner.component.ts:18` hardcodes `role: "alert"`. Project the ✕ as content, and for the passive/info case use `<div class="banner is-info" role="status">` instead — an assertive live region mutated by a background stream during a recording is exactly wrong. One live region per card.

#### Rejected

- **Rejected: adding `"NSValue"` to `objc2-foundation`** as the default path. It *is* a zero-new-crate feature flag (`NSValue = []` in the crate manifest, Cargo.lock unchanged), and it is offered as a user decision in §5 — but the **recommended default is the `respondsToSelector`-guarded `msg_send![num, longLongValue]` under `objc2::exception::catch`**, which needs no manifest edit at all and is fully rules-compliant. Fifteen lines.
- **Rejected: lowering `TURBO_DEFAULT_MIN_RAM_BYTES` from 12 GiB in this program.** Sharp is ~1.2–1.5 GB resident on top of the app, whisper's working set and the brain sidecar — exactly the pressure class the 2026-07-21 incident measured. It is a behaviour change for low-RAM users and must be gated on one RSS-over-time measurement on a signed build, not a design document. Surfaced in §5 as non-blocking.

---

### 3.4 W4 — Plain-language vocabulary + Settings IA

#### Accepted corrections

**D1 — The Ollama posture tile keeps naming Ollama.**
Verified `onboarding.component.ts:59 LOCAL_PROVIDER_ID = "ollama"` and `:411-412 if (p === "local") { this.provider.set(LOCAL_PROVIDER_ID) }`. The proposed *"Uses Murmur's built-in AI. Nothing leaves your Mac."* is **false**. Corrected: **`Only on this Mac`** / *"Uses Ollama, free software you install on your Mac. Nothing leaves this Mac."*
**And the `ollama serve` / `ollama pull` steps do NOT move behind a disclosure** — a required setup step is not progressive-disclosure material. They stay visible, reworded plainly.

**D2 — The `matchLabel` "real bug" is DELETED, along with its RED test.**
Verified end-to-end: `library.component.ts:914` and `quick-search.component.ts:71` both call `ipc.searchMeetings` → `invoke("search_meetings")` → `commands/meetings.rs:103` → `db.search_visible(...)` → `search_snippet` (db.rs:7295-7326), which returns **only** `"title"`, `"transcript"`, `"note"`. The values `semantic`/`entity`/`topic`/`temporal` are produced by `search_semantic_visible`, `search_hybrid_visible`, `search_visible_in_range` — whose callers are `tools.rs`, `vault_context.rs`, `related_context.rs`, `embed.rs`, none of which returns a `SearchHit` list to a `matchedIn` chip. **A RED test on an unreachable input is a fake RED.**
→ `matchedInLabel()` ships for the three reachable values (`in the title` / `in the transcript` / `in the note`); the other four arms are either dropped or kept with a comment marking them forward-looking. The claim that this is a bug fix is removed from the design, the risks and the verification plan.

**D3 — The ledger renames are corrected.** `Tokens by model` → **`By model`**, not "By AI service" — verified `storage/egress_store.rs:154` groups by model label and `egress-ledger.component.html:54` renders `{{ m.model }}`; two Anthropic models would render as two "AI services". "model" is the honest word and joins the non-goals list. `Tokens sent` keeps a unit that matches the data (`{{ fmtTokens(l.totalTokens) }}` at :35) — either **`Tokens sent`** with a gloss *"roughly ¾ of a word each"*, or change what is counted. Do not rename a unit without changing the number. `PII scrubbed` → **`Emails, phones and cards removed`** (the design's own rule says redaction is *always enumerated*; "Details removed" is a bare noun).

**D4 — The error-copy layer is scoped honestly and fails DENY-BY-DEFAULT.**
Verified: **185 `String(e)` sites across 47 files** (design said "~40"). Rust side: ~2100 `AppError::*` constructions, ~161 with a never-show term in the *body* (`"brain sidecar stdin missing"`, `"account-session mutex poisoned"`, `"E2EE decrypt/authentication failed"`, `"HKDF expand failed"`). The design's fallback heuristic ("contains `::`, a path separator, or an all-lowercase-snake token") matches **none** of those.
→ `[code]` is applied to a named allowlist of the ~20 errors that actually reach a banner or toast, **and any un-coded error renders the generic sentence**. Pass-through is removed. Scope is stated in the PR body: 185 sites, 47 files, routed mechanically.

**D5 — The vocabulary gate scans Rust too.** The single worst offender in the product was a Rust string. A second pass over `src-tauri/src/**/*.rs` restricted to `AppError::*("…")` literals, `format!` args into AppError, and the display fields in `settings/ai_map.rs` + `summarize/roles.rs`. Rust allowlist seeded for log-only strings.

**D6 — The gate ships in WARN mode (P3), flips to FAIL (P5).** The design said both "must fail with dozens of hits on trunk" and "allowlist seeded to pass on current trunk" — an allowlist big enough to make trunk green *is* the rubber stamp its own risk section warns about. → Two files: `.vocabulary-baseline.json` (transitional hit list, asserted monotonically shrinking) and `.vocabulary-allowlist.json` (permanent, `reason` required per entry).

**D7 — Eight `friendly*Error` mappers, not six.** The two missed — `share-panel.component.ts:515` and `note-share-panel.component.ts:406`, both `friendlyUnlockError` on the **lock gate** — join the fold-in list and P3's lock-security scope.

**D8 — A pointer sweep is part of every rename.** Verified dangling cross-references: `ai-privacy-strip.component.html:35` "(in Privacy & integrations)"; `settings-privacy-section.component.html:31` `<h3>Privacy &amp; integrations</h3>` (the section's own heading, not just the sidebar label); `audit.component.html:37` "Settings → AI & Models"; `onboarding.component.html:397` and `:437` the same. Gate rule: a rendered section label appears only in `SETTINGS_SECTIONS` and that section's own `<h3>`.

**D9 — `[code]` coverage or a recorded coupling.** `brain.component.ts:699-738 friendlyImportError` prose-matches six Rust strings. Either extend `[code]`, or record every prose-coupled Rust string in `error.rs`'s doc comment so a future de-jargon PR cannot break them silently. Currently nothing records the coupling.

**D10 — The Quality ladder is W2's four rungs (§3.2), not this workstream's three.** Resolved above.

**D11 — Posture e2e specs are named.** `e2e/settings-ai/brain-posture-block.spec.ts:53,63,66,74,77,79,84` and `e2e/settings-ai/posture-auto-download.spec.ts:14,76,79,86` pin `/Hybrid/`, `/Fully local/`, `/Fully local needs/`, `/Download .* enable Fully local/` — a confirm-first-then-download safety flow. Verified 8 specs assert `"AI & Models"` (of 64 total; the "54 broken" figure was invented). Rewrite selectors to role + testid rather than re-pinning new prose.

**D12 — The `settings.store.ts` `modelSize: "large-v3"` initializer change is REMOVED from this workstream.** Verified `load()` does `cfg.modelSize ?? "small"`, so the initializer never decides anything, and `""` matches no `<option value>`. It is P2's business (the card renders `selectedId`), not a copy PR's.

**D13 — Settings search auto-opens the owning disclosure.** A keyword hit that resolves to an Advanced-only control must reveal it. Precedent exists: `SettingsStore.expandAdvanced()` (settings.store.ts:1186) and the `_autoExpand` effect (ai-advanced-block.component.ts:44-57). The keyword→disclosure map lives in `glossary.ts`; the e2e asserts the **control** is visible, not just the section.

**D14 — `<mur-disclosure>` needs `:host { display: contents; }`** (verified `ai-advanced-block.component.scss:1-3` — required because `settings-ai-section` flex-stacks its children).

**D15 — Label helpers move into a `computed()` view-model**, not template method calls. Verified `library.component.html:206-207` calls `matchBadgeClass(...)` / `matchLabel(...)` inside a `@for` — banned by the rules table. The design *claimed* this was fixed; the file list didn't deliver it.

**D16 — "Connections" is used once.** The glossary bans `connection` as an AI-service alternate, renames nav `Graph`→`Connections`, and renames the lens group `Links`→`Connections`. Resolved: nav → **`Connections`**, lens group stays **`Links`**, and `connection` leaves the never-show list with a note (the graph sense is legitimate).

**D17 — Protected-clause anchors corrected.** P4's broad claim (*"a remote Ollama server counts as cloud"*) lives at `settings-privacy-section.component.html:44-49`, not `ai-connection-card.component.html:326-334` (which says the narrower "a local gateway can still forward to the cloud"). P15's shipped text is *"your **database** encryption keys are never passed through"*. Both fixed so `privacy-honesty.spec.ts` asserts the fact that is actually there. All 15 protected clauses were spot-checked and are real.

**D18 — `note-editor.component.ts:1528/:1606` case-sensitive `"Locked"` guard is fixed in P3.** Verified every producer is lowercase (`error.rs:19 #[error("locked: {0}")]` over `commands/notes.rs:448` etc.), so the guard never fires and a locked-note save falls into the retry branch. It is an error-copy bug, in scope, with a RED test — and it is a warning: **once `humanize()` strips prefixes, every downstream raw-string guard changes behaviour. Enumerate them all before the refactor.**

#### Rejected

- **Rejected: renaming `BRAIN_MODELS[].name`.** Keeping the checkpoint names as *muted secondary* text is both cheaper and more honest (the user should be able to see which model is on their disk) and it keeps `e2e/settings-ai/brain-engine-card.spec.ts:85-86` green. The `isPolish` coupling is decoupled in **P0** regardless (`!m.languages.includes("multi")`, verified equivalent across all six rows).
- **Rejected: per-component `prefers-reduced-motion` as the mechanism.** See §3.0.3.

---

### 3.5 W5 — Auto-updater

**Track A confirmed** (verified download of the published notarised DMG + Gatekeeper verification on-Mac + verify-before-destroy swap with rollback). Neither critique disputed the recommendation. Track B remains fully specified as a user option (§5).

#### Accepted corrections

**E1 — The interlock is completed. The "safety guarantee is complete for current behaviour" sentence is DELETED.**
Verified: `recording_has_priority()` is false during `retry_transcription`'s salvage run (it just checked the flag was false at `mod.rs:5334`), during `resummarize` (`mod.rs:5221` — no such check at all), and during startup disk salvage (which uses the separate `startup_recovery_has_priority()`, `perf.rs:212`).
→ `install_blockers` gains **`startup_recovery_has_priority()`** and **`PipelineBusy`**, an RAII counter taken at the top of `retry_transcription`, `resummarize`, `audio::spill`'s salvage runner, and (later) the queue worker — in addition to the five `commands/models.rs` downloads. `PipelineBusy` lands in **P0**.

**E2 — One-shot installer arming.** Verified `lib.rs:1157 "quit" => app.exit(0)` and `lib.rs:942 RunEvent::ExitRequested` is a single global hook that `AppHandle::exit` also triggers. "Install & Restart" would spawn a second installer racing the first over the same `mv`. → An `AtomicBool`/`Once` consumed by whichever path spawns first, **plus** an explicit `phase != Installing` guard. Unit test: call the install path twice, assert exactly one spawn.

**E3 — Disk preflight sizes the INSTALL, not the download.** The DMG is UDZO-compressed (`macos-sign-notarize.sh:150`); the swap holds the DMG + mount + `staged/` + `rollback/` + `.Murmur-new` + `.Murmur-old` — four uncompressed copies of a bundle carrying five sidecar helpers and the always-compiled ML tree in a 2-arch universal binary. → Preflight `dmg_bytes + 4 × app_uncompressed_bytes`; the sign script emits `app_uncompressed_bytes` into a sidecar. Re-check free space inside `install.sh` immediately before the swap; exit 14 explicitly covers ENOSPC.

**E4 — `install.sh` repeats the FULL verification pin.** The design verified 8 checks at download time and 2 at install time (`codesign --verify --strict` + profile presence) — both trivially satisfiable by anything that can write to `~/Library/Application Support/MeetNotes/updates/staged/`, which is also where the design writes and executes `murmur-install.sh`. → `install.sh` re-runs `codesign -dv --verbose=4` grepped for `TeamIdentifier=BVF778E5QD` and `Authority=Developer ID Application:`, plus `plutil -extract CFBundleIdentifier` == `com.meetnotes.app`, plus the version — each with its own exit code. Better still: Rust re-verifies immediately before spawning and passes a `cdhash` the script pins.

**E5 — Blockers reach the FE via `update_state()`, never a parsed error string.** Verified `error.rs:48-53` serializes `AppError` to a bare `to_string()`. `install_update` returns a plain `AppError::Unavailable`; the FE already holds `blockers: Vec<InstallBlockerDto>` from the authoritative DTO and the `murmur://update` payload. The "serialised blocker list" phrasing is deleted (`rust-tauri.md` §1: *"Do not hand-build error strings the FE has to parse"*).

**E6 — `state::app_dir()` does not exist.** Verified: only `state::app_dir_name() -> &'static str` (state.rs:34). Every real caller does `dirs::data_dir().map(|b| b.join(crate::state::app_dir_name()))`. Spelled out explicitly, or a named helper is added.

**E7 — Update egress is LEDGERED.** Verified `share/mod.rs:226 pub fn ledger_row(db, host, kind, byte_count)` — a content-free host/bytes ledger already exists (the design dismissed only the *LLM* ledger). → `ledger_row(db, "github.com", "update-check", 0)` and `("update-download", bytes)`. One line each; turns the disclosure from a promise into an auditable record, which is the stronger answer to *"new cloud egress must be loud + justified"*.

**E8 — `spctl` output is read from BOTH streams.** Verified `macos-sign-notarize.sh:159` uses `2>&1` precisely because the verbose assessment goes to stderr. Assert on combined output; treat **exit status 0 as the primary signal** and the string as secondary confirmation, so a macOS wording change degrades to a diagnosable refusal. Unit-test against a captured fixture.

**E9 — `ditto` stays, but the claim is verified before implementation, not asserted.** Verified counter-evidence: `macos-sign-notarize.sh:147` stages the signed bundle into every shipped DMG with `cp -R`, and those DMGs notarize and pass `spctl`. The `tar` half of the claim is right; the `cp -R` half was unverified. → The empirical check (`ditto` a signed `Murmur.app`, then `codesign --verify --strict` + `codesign -d --entitlements :-` on the copy) is a **P8 preflight gate**, run before a line is written.

**E10 — Version pinning is real, and the rollback is not pruned on "it launched".**
The brief asked for the pinning story; the design shipped auto-clearing "Skip". → Add `update_pinned_version` suppressing everything above it, surfaced as **`Stay on 1.0.3`**. And the rollback prune trigger changes from "the new version booted" to an explicit **`Keep the previous version`** default that survives until the user dismisses it or a second successful update lands — because the risks section's own highest-stakes unknown is *whether Touch ID still opens the KEK*, which "it launched" does not test.

**E11 — Every installer exit code has copy, split into "attempted" vs "not attempted".** Exit 10 (timed out), 11 (the *backup* failed verification), 16 (not writable) never move anything, so *"Murmur put 1.0.3 back. Nothing was lost."* is false for them. Exit 11 in particular is realistic and would silently disable updates forever — it needs its own copy and a stated policy.

**E12 — `install_blockers(app: &AppHandle)`, one signature.** Uses `app.try_state::<AppState>()` (the shape `relock_and_zeroize_on_lifecycle` already uses at `lib.rs:1000`, which handles init-failure). Threaded through the scheduler and the download loop.

**E13 — `reveal_update_installer` is new, not "the existing technique".** Verified: the only `Command::new("open")` is `update.rs:202` inside the URL-pinned `open_release_page`. The new command takes **no parameters** and derives its path only from internal staged/mount state; it refuses when `staged` is `None` rather than falling through to a bare `open`.

**E14 — Downgrade detection uses `is_newer`** (already written and tested at `update.rs:112`, tests at `:245-267`), not `!=` — otherwise a manual reinstall of 1.0.3 over 1.0.4 shows *"Updated to 1.0.3"* with a `What's new` button pointing at 1.0.4.

**E15 — The blocker banner is `role="status"`, not `role="alert"`.** Verified `banner.component.ts` hardcodes `role: "alert"`. An assertive region mutated by a background event stream **during a recording** is the one moment the app must be silent. Use the global `.banner.is-info` classes with `role="status"`; keep `alert` for the danger case.

**E16 — `xattr` becomes `removexattr(2)` via `libc`** (already in `Cargo.lock`), or is dropped entirely. It is likely a no-op anyway — `reqwest` never sets `com.apple.quarantine` (LaunchServices does), so the file was probably never quarantined. Either way, the **ordering comment stays**: de-quarantine is step 9 of 10, a consequence of verification and never a precondition.

**E17 — The verification copy is softened to what is actually checked**, or the check is added. Recommend adding `spctl -a -vvv -t exec` on the staged app as step 8b (passes offline once the app-level staple lands, online otherwise) so *"Signed and notarised by Apple — verified on this Mac"* is literally true.

**E18 — Quit-mid-recording is resolved empirically.** `lib.rs:947` runs `relock_and_zeroize_on_lifecycle` → `stop_all_capture`, which finalises helpers but does not run the pipeline; the in-flight WAV is left for next-launch salvage. Whether `recording_has_priority()` is still true at the `arm_on_quit_if_ready` call site must be **determined, not assumed**. Safest: block the on-quit install whenever `spill::claim_inflight` would find anything.

**E19 — A persistent-offline state exists.** Track `update_last_check_failed`; render `Couldn't reach GitHub — last tried 2 hours ago` in the muted line only. No toast, no banner — the anti-nag contract survives, and "Not checked yet" for a week stops being indistinguishable from a broken updater.

**E20 — `mur-progress` comes from P0**, not from this workstream (see §3.0.2).

#### Rejected

- **Rejected: shipping the updater before the queue.** Independently valuable and tempting, but P6 narrows `recording_has_priority()` and the compounding benefit does not outweigh the consequence-of-failure ordering. (If the user wants it earlier, `PipelineBusy` from P0 makes it *safe* to reorder — noted, not recommended.)

---

## 4. PR sequencing plan

**Cadence (binding):** SEQUENTIAL. One writer, one worktree, at a time. Every Cargo/CI command through `scripts/agent-resource-run`; the dev server through `scripts/agent-dev-run`. **Never fan out parallel Rust builders** — the always-compiled ML tree freezes this machine. Each PR is CI-gated and merged to `murmur` via `gh pr create` → `gh pr merge` (never a direct push), authored solely by `QueaT <kgm004a@gmail.com>` with no AI trailers. Run `scripts/agent-harness` for every writer task; the implementer never owns the verdict.

**Pre-empt clippy:** the harness's Rust check is `cargo test --lib`; `clippy -D warnings` (`type_complexity`, `too_many_arguments`) only bites at CI. Watch the widened `transcribe_raw_windows`, `whisper_catalog_dto`, and `download_model_streaming` signatures.

---

### P0 — Foundations, primitives, and free fixes
**Scope:** `src/app/design-system/{progress,disclosure}/**` (new) · `design-system/select/select.component.{ts,html}` (add `value` model + `disabled` input) · `design-system/primitives.css` (promote `.field-help`, add `.pill.is-pop`) · the seven progress-bar call sites · `src/app/core/copy/{glossary,labels,error-copy.service}.ts` (new, empty-ish scaffolds) · `src/app/core/format.ts` (`sizeLabel`) · `model-effort-picker.component.ts` (`isPolish` → `!languages.includes("multi")`) · `settings/sections/ai/local-models-list/**` (delete) · `library.component.ts` + `meetings-table-view.component.ts` (`statusPillClass`: `RECORDING` → `is-live`, explicit label map) · `src-tauri/src/{commands/mod.rs,commands/models.rs,audio/spill.rs,perf.rs}` (`PipelineBusy` RAII counter, dormant).
**Delivers alone:** a correct in-progress pill (today `RECORDING` renders red/danger, conflating in-progress with failed), one progress primitive replacing seven copies, a decoupled brain family toggle, ~700 lines of dead UI removed, and the shared interlock counter.
**Risk:** LOW. **Lock model:** NO.
**Verification bar:** `cargo test --lib` + `ng lint` + `ng build` + `ci.sh`. Unit test pinning `!languages.includes("multi")` == `name.includes("bielik")` across all six `BRAIN_MODELS` rows. Playwright screenshot diff of Settings → Appearance (the `mur-slider` consumer) before/after. Confirm `app-local-models-list` has zero mount sites (verified today: only its own selector + `brain-engine-card.spec.ts:9` saying it is superseded).

---

### P1 — Machine profile + whisper catalog + recommendation (Rust)
**Scope:** `src-tauri/src/machine.rs` (new) · `src-tauri/src/transcribe/catalog.rs` (new — the **merged** registry) · `transcribe/model.rs` (delegate branch 2 only; `total_ram_bytes` → `pub(crate)`; `quality_tier_label`) · `transcribe/mod.rs` · `commands/model_perf.rs` (`whisper_recommendation`, `list_whisper_models`, `machine_change_nudge`, `dismiss_machine_change_nudge`) · `reason.rs` (`brain_advice_for`, floor 24 GB) · `settings/{config.rs,ai_map.rs}` · `lib.rs` (registry + fingerprint compare at setup) · `src/app/core/{models.ts,ipc.service.ts}` · `src/app/services/machine.service.ts` (new) · a `<mur-pill kind="accent">` badge next to the **existing** Quality select.
**Delivers alone:** the badge + the honest reason line on the current UI, the raw `large-v3-turbo-q8_0` gone from the AI map, one fewer `sysctl` subprocess per `list_brain_models`, and the marketing-name gate green.
**Risk:** MEDIUM (new FFI). **Lock model:** NO — config reads, `read_dir`, hardware probes, no content, no egress. The verifier must confirm by grepping the diff for `meeting_`, `visibility_clause`, `seal` and finding none.
**Verification bar:** the **six existing `default_model_size` tests must pass UNEDITED** — that is the proof of zero behaviour change. RED-first: `recommendation_ignores_whats_on_disk` (`recommend(Some(64*GIB), Some(true)).batch_id == Some(TURBO)`) **and separately** `default_model_size(&["ggml-large-v3.bin"], Some(64*GIB)) == "small"`. Two separate assertions for Intel (`Some(32*GIB), Some(false)` → `small`; `Some(32*GIB), Some(true)` → turbo) — **never a conjunction**, which pins neither leg. `machine::tests::probes_never_panic` in the `thermal.rs` mould. `registry_live_safe_matches_classifier`. `fingerprint_is_none_without_ram`. Chip-string normalisation against Apple and Intel fixtures.

---

### P2 — Transcription power slider
**Scope:** `design-system/power-slider/**` (new, own `<input type="range">`, full CVA, `aria-valuetext`, PageUp/PageDown) · `design-system/meter/**` (new) · `settings/sections/settings-transcription-section/model-power/**` + `model-catalog-table/**` (new) · `settings-transcription-section.component.{html,ts}` · `onboarding.component.{ts,html,scss}` · `settings.store.ts` · `services/transcription-models.service.ts` (new root) · `commands/models.rs` (`delete_whisper_model`, `cancel_model_download` generation counter, `fileIndex`/`fileCount`) · `commands/live_captions.rs` (`companion_pending_in` → `pub(crate)`) · `transcribe/model.rs` (cancel threading, delete guards) · `events.rs` · `e2e/onboarding/model-presence-race.spec.ts` (rewrite) · `e2e/settings-ai/model-power.spec.ts` (new).
**Delivers alone:** the whole "power slider + honest default" experience, three hardcoded size tables deleted, the 9-vs-6 catalog divergence fixed, a cancellable download, and disk reclaim.
**Risk:** HIGH (destructive `delete_whisper_model`; the app's only unskippable first-run gate). **Lock model:** NO.
**Verification bar:** RED-first — `delete_refuses_the_effective_model`, `delete_refuses_the_live_caption_pin`, `delete_rejects_a_non_registry_id`, `delete_never_touches_vad_or_parakeet` (over a temp models dir), `download_cancel_removes_the_part_file`. The `model-presence-race` rewrite must go **RED when `modelPresenceRequestId` is temporarily removed** — a rewrite that passes against unguarded code did not capture the bug. E2E: a three-rung drag emits exactly ONE `save_config`; an off-catalog `selectedId` renders the custom banner; `aria-valuetext` reads the human stop name; reduced-motion emulation still commits and swaps copy.

---

### P3 — Vocabulary machinery + error copy + privacy vocabulary
**Scope:** `src/app/core/copy/**` (fill in) · **185 `String(e)` sites across 47 files** → `ErrorCopyService.humanize()` · all **eight** `friendly*Error` mappers folded in · `note-editor.component.ts:1528/:1606` case-sensitivity fix · `src-tauri/src/summarize/mod.rs` (the `[cloud-consent]` string) · `summarize/roles.rs` + `settings.store.ts::CONNECTION_LABELS` (**same PR**, they are a documented mirror) · `settings/ai_map.rs` display strings · `error.rs` doc comment (the `[code]` convention + the enumerated prose couplings) · `record.component.ts` (`needsCloudConsent` → code match) · `settings-privacy-section` + `ai-privacy-strip` + `egress-ledger` rewrites · `scripts/check-vocabulary.mjs` + both JSON files + `scripts/ci.sh` (**WARN mode**) · `e2e/settings/privacy-honesty.spec.ts` (new, 15 protected clauses).
**Delivers alone:** no user ever sees `summarizer error:` again; the app's most-hit blocking error becomes readable; the privacy surfaces become plainer AND more explicit.
**Risk:** HIGH (touches lock/egress copy and the consent flow). **Lock model:** **YES — `lock-security-reviewer` required.**
**Verification bar:** RED-first Playwright — with `stop_recording` rejecting the NEW `[cloud-consent] …` string, the pre-change FE (prose regex) must FAIL to render the Allow banner and leak the raw string; post-change it renders. `privacy-honesty.spec.ts` asserts each of the 15 clauses by **fact** (e.g. Privacy must still contain `/emails/i` AND `/card/i` AND `/phone/i` AND an explicit negative about names). `humanize()` unit tests: all 14 prefixes stripped; an un-coded internal-looking message → generic; `friendlyImportError`'s **ordered** ladder preserved (`no text found` must still beat `unsupported document type`). Gate runs and prints hits without failing.

---

### P4 — Settings information architecture
**Scope:** `settings.component.ts` (labels + group names + keyword synonyms, jargon KEPT as hidden synonyms) · `settings-ai-section.component.html` (reorder, privacy strip 8th → 3rd) · `ai-advanced-block` → `<mur-disclosure>` · new disclosures in Transcription / Privacy (MCP) / General / Audio / Connected apps · `glossary.ts` keyword→disclosure map + auto-open · the pointer sweep (D8) · `e2e/settings-ai/{brain-posture-block,posture-auto-download}.spec.ts` + the 8 `"AI & Models"` specs.
**Delivers alone:** everyday Settings ~70 → ~30 controls, with zero privacy disclosure moved behind a disclosure.
**Risk:** MEDIUM. **Lock model:** NO.
**Verification bar:** `ng build` (16 kB budget). E2E: every disclosure toggles `aria-expanded` and `aria-controls` resolves; the AI block's store-owned `_autoExpand` still fires on a role override; **search for `gguf` surfaces the AI section AND the control is visible**, same for `model file`/`egress`/`what leaves`/`mcp`.

---

### P5 — Vocabulary long tail + gate armed
**Scope:** onboarding (posture tiles with the corrected Ollama copy, Kong removal if approved, "Turn on memory and search") · graph/brain (`entity`→`people and projects`, lens labels, `Chunking…`/`Embedding N/M`) · share panels ×3 (`E2EE ciphertext bundle` → `encrypted together with the note`, P5/P8 clauses verbatim) · audit · storage · org · audio · notes · `app-shell` (nav `Graph`→`Connections`, relock toasts) · `scripts/ci.sh` (**gate → FAIL**) · baseline file emptied.
**Risk:** MEDIUM. **Lock model:** **YES** (lock-gate copy in `detail.component`, `move-to-menu`, folder rows, share panels) — `lock-security-reviewer` required.
**Verification bar:** vocabulary gate FAILS on any regression; `privacy-honesty.spec.ts` still green; full e2e suite green.

---

### P6 — Processing queue core *(atomic BE+FE)*
**Scope:** `storage/postprocess_store.rs` (new) · `postprocess/{mod,worker,policy}.rs` (new) · `commands/postprocess.rs` (new) · `perf.rs` (+2 fns) · `commands/mod.rs` (`stop_recording_owner` enqueue, `seal_folder_extras` cache drop, `StopResult.queued`) · `commands/lock.rs` (`unlock_folder` hold release; `reblank_folder_extras` cache drop) · `pipeline.rs` (`PipelineRun::Yielded`, `WindowSink`, sub-window `should_yield`) · `storage/{db,models,mod}.rs` (`MeetingStatus::Queued`) · `events.rs` · `lib.rs` · `settings/config.rs` (policy) · `audio/spill.rs` (`DeferSealed` → `HELD/LOCKED`) · **FE:** `recorder.store.ts` (meeting-id filter, `isBusy` shrink, `finalizing` stage), `record.component.*` (gate removal + hand-off strip + queue line), `bar.component.*`, `meetings-list-store.service.ts` (passive patch), `detail.component`, `models.ts`, `ipc.service.ts`, `services/processing-queue.service.ts` (new root), `app.component.ts` (`queue.init()`).
**Delivers alone:** record #2 immediately after Stop; notes finish in the background; the meetings list finally learns a note completed; a completion toast.
**Risk:** **HIGHEST in the program.** **Lock model:** **YES — `lock-security-reviewer` REQUIRED** (a new content-bearing table + a new deferred-execution window across lock transitions).
**Verification bar:**
- RED #1 `e2e/record/record-while-processing.spec.ts` — Start visible AND enabled with a running job. Fails on trunk.
- RED #2 cross-meeting stage clobber — emit `{stage:'summarizing', meetingId:'other'}` during a live recording; the strip, Stop button and timer must survive. Fails on trunk.
- RED #3 `Queued` survives launch — three `Queued` meetings survive `reconcile_stuck_recordings_except(&[])`; write the mirror with `Recording` first and watch two of three flip to `Error`.
- RED #4 yield-is-not-a-failure — `Err(Unavailable)` with the yield predicate **true** → requeue, `attempts` unchanged; predicate **false** → retryable, `attempts+1`. **Two separate tests.**
- RED #5 relock clears checkpoints — seed a row, `relock_all_inner`, assert zero.
- Durable-handoff proof: `release_for_recovery` → `claim_stale_recording_generation_for_meeting` returns the identical snapshot.
- ASR checkpoint round-trip: interrupt at a chunk edge, resume, assert the per-stream window set is byte-identical to an uninterrupted run and `merge_streams` yields identical segments.
- Migration idempotency: `migrate()` twice over a DB with queue rows.
- **Live dev-app run:** record 2 min → Stop → Start within ~2 s with no `another recording model session is already active` and no `local AI did not quiesce in time`; job #1 reads `Paused for your recording` within seconds; both notes land; force-quit mid-transcription → relaunch → resume with `· resumed`.

---

### P7 — Processing queue surface
**Scope:** `features/processing/{processing-panel,processing-row}/**` (new, teleported to `<body>`, opaque per T3, never moved back on destroy) · `app-shell.component.*` (chip + panel mount + ⌘⇧P + partial-aware Lock all) · `settings-transcription-section` policy card · `settings.store.ts` · `folder-lock-flow.service.ts` (the "Finish processing first" pre-check) · `analytics.component.ts` (`QUEUED`) · `e2e/processing/queue-surface.spec.ts` (new).
**Delivers alone:** visibility and control — Process now, Pause, Retry, the three policies, the lock-hold unlock path, and the raw `AppError::Locked` sentence never reaching the user.
**Risk:** MEDIUM. **Lock model:** **YES** (masked job rows, lock-hold release) — `lock-security-reviewer` required.
**Verification bar:** E2E — a masked (locked) row shows `🔒 Locked` with no progress numbers and no audio affordance; `Process now` invokes the command; the policy segmented control round-trips; the lock pre-check confirm appears instead of the raw error.

---

### P8 — Updater core (manual install)
**Scope:** `src-tauri/src/update/**` (module split + `github.rs`, `download.rs`, `verify.rs`, `install.rs`, `install.sh`) · `events.rs` (`EVENT_UPDATE`) · `settings/config.rs` (5 keys) · `share/mod.rs` ledger calls · `lib.rs` (commands, status file, boot compare) · `settings-about-section/**` (rewrite) · `models.ts`, `ipc.service.ts`, `update.service.ts`, `app.component.ts` · `scripts/macos-sign-notarize.sh` (app-level notarise+staple, warn-and-continue; `.sha256` + uncompressed-size sidecars) · `.claude/skills/release-murmur/SKILL.md` · `e2e/settings-about/software-update.spec.ts` (new — this surface has **zero** e2e coverage today).
**Preflight gate (before any code):** empirically confirm `ditto` of a signed `Murmur.app` survives `codesign --verify --strict --deep` and retains `keychain-access-groups`. **The whole design rests on this.**
**Delivers alone:** one-click Download → verify → Install & Restart from Settings, with rollback. No scheduler, no automatic mode, no on-quit.
**Risk:** **HIGHEST consequence-of-failure** (a broken swap = the user cannot open their encrypted notes). **Lock model:** **YES** (the Keychain ACL is bound to the code signature) — `lock-security-reviewer` required.
**Verification bar:** RED-first `asset_matcher_rejects_near_misses` (`Murmur-1.0.4.dmg.sha256`, `murmur-1.0.4.dmg`, `Murmur-1.0.40.dmg`); `download_url_host_pinning` incl. `murmur-phish` prefix-boundary (run against a naive `starts_with` first); `install_blockers_matrix` one-per-blocker; `team_id_pin_matches_entitlements` via `include_str!`; `installer_exit_code_mapping` all eight codes. E2E: Install disabled-with-reason under `blockers:['recording']`, enabling live on a `murmur://update` event. **Then a real signed-build old→new run** (see §6).

---

### P9 — Updater automatic
**Scope:** `update/scheduler.rs` · mode segmented control + the one-time disclosure toast · `arm_on_quit_if_ready` in `RunEvent::ExitRequested` (after relock + sidecar kill) · `update_pinned_version` ("Stay on 1.0.3") · the sidebar chip · `settings-privacy-section` disclosure line.
**Delivers alone:** the calm experience — checks daily, downloads quietly, installs on quit.
**Risk:** HIGH. **Lock model:** YES (the on-quit hook sits inside the relock/zeroize path).
**Verification bar:** exactly one toast per version across two refreshes; a second emit adds nothing; an *upgrade*-direction machine change fires zero toasts; quit-with-armed-installer does not delay shutdown; **one-shot arming test** (call the install path twice, assert one spawn).

---

## 5. Decisions the USER must make

> Everything not listed here the implementer decides. Tier names, the mur-progress migration scope, the disclosure control shape, the four-rung ladder, PR-internal copy: all implementer calls.

### D1 — Auto-updater mechanism and key custody ⚠️ BLOCKING

| Option | Trade-off |
|---|---|
| **A. Verified download + verify-before-destroy install** *(recommended)* | **Zero new crates. Zero secrets. Zero key custody. No fourth release pause point.** Verified by Gatekeeper on the user's own Mac (a *stronger* bar than the standard plugin, which runs no `codesign` check on what it installs). Automatic rollback. Cost: ~600 lines of bespoke Rust + one shell script, and a preflight `ditto` verification. |
| **B. `tauri-plugin-updater`** | Standard, less bespoke code. Cost: **4 new crates** (`tauri-plugin-updater`, `minisign-verify`, `osakit` + its ObjC tree, `tempfile`); a **permanent minisign private key you must generate interactively and back up before the first release that ships its pubkey** — losing it strands the entire installed base on their current version forever; three permanent manual release steps whose omission silently strands users (regenerate the tarball *after* signing or you ship the 0.7.1 AMFI kill; `COPYFILE_DISABLE=1` or `codesign` breaks on extraction; a hand-maintained `latest.json` with **both** `darwin-aarch64` and `darwin-x86_64` entries — `darwin-universal` is not a key the plugin probes). It also runs no `codesign` check on what it installs and can fire a surprise admin-password prompt. |

**Recommendation: A.** Murmur's bundle carries `Contents/embedded.provisionprofile` and a restricted `keychain-access-groups` entitlement, and the SQLCipher DEK + biometric master KEK sit behind a Keychain ACL bound to that signature. A broken self-replacement here does not mean "the app won't start" — it means **the user cannot open their encrypted notes**. Track A applies this repo's own verify-before-destroy discipline to the install.

**If you pick B, the manual step is yours and only yours:** `npx tauri signer generate -w ~/.tauri/murmur-updater.key` run interactively in your terminal (password on stdin), the private key backed up to 1Password + one offline copy **before** the first release, and `TAURI_SIGNING_PRIVATE_KEY` + `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` supplied from your shell at release time. An agent must never touch it.

---

### D2 — The one dependency-shaped ask: `objc2-foundation` `"NSValue"` feature

Needed only to read the `NSNumber` returned by the free-disk `NSURL` probe. It is **not a new crate** — a feature flag on an already-declared dependency whose manifest declares `NSValue = []` (empty body), so `Cargo.lock` is unchanged and `cargo deny` sees nothing new. But it is a manifest edit, and the repo rule is strict.

| Option | Trade-off |
|---|---|
| **A. Approve the feature** | Typed, generated FFI. One-line manifest edit. |
| **B. Guarded `msg_send![num, longLongValue]` under `objc2::exception::catch`** *(recommended)* | **Zero manifest touch.** The `exception` feature is already enabled. Fifteen lines, fully compliant with `rust-tauri.md` §7's escape hatch. |
| **C. Ship v1 without free disk** | The tier decision, the badge, the reason line and the brain advice all depend on `hw.memsize` alone. Free disk only powers an optional advisory. |

**Recommendation: B.** No approval needed, no precedent set, same result.

---

### D3 — Queue default processing policy ⚠️ affects every Stop

| Option | Trade-off |
|---|---|
| **A. `immediate`** *(recommended)* | A note starts as soon as capture ends and pauses itself the instant you start another meeting. Smallest behavioural change from today (notes still appear on their own); the user never has to learn a new concept to get their note. |
| **B. `between_meetings` (3 min quiet)** | Never competes with a back-to-back meeting. Better for dense calendars. But it is a *timer*, i.e. a heuristic, not the deterministic trigger — and the user must understand the concept to get a note. |
| **C. `manual`** | Nothing runs until pressed. Maximum control, but silently breaks the mental model "I stop, a note appears". |

**Recommendation: A**, with B one click away in the panel.

---

### D4 — Stop no longer returns the note ⚠️ SHIPPED BEHAVIOUR CHANGE

Today `stop_recording` resolves only after transcribe + summarize + export, and `StopResult` carries the note markdown. After P6 it resolves in ~1–2 s carrying `{ queued: true }`, `markdown: ""`, `exported_path: None`. The Record screen shows `✓ Recording saved — writing your note in the background.` and the Brain-reveal / Re-Truth cards render when the job completes instead of when Stop resolves.

This is the point of the feature, but it is a real contract change. **Confirm you want it.** (If not, the whole queue workstream collapses to "show a better spinner".)

---

### D5 — Queue "cancel" semantics ⚠️ scope reduction

The design deliberately ships **no** "cancel this job but keep a half-processed meeting". `Pause` covers "not now"; `Discard` deletes the meeting entirely (audio, transcript, note) behind the existing delete confirm. The reasoning: a third "cancelled-but-half-processed" state would be a lie about what happened to the content.

| Option | Trade-off |
|---|---|
| **A. Pause + Discard only** *(recommended)* | Honest. No state where content is partially gone. |
| **B. Add a real cancel** | Needs its own verify-before-destroy review for a partially-published archive, and produces a meeting the user cannot reason about. |

---

### D6 — Update default mode ⚠️ SHIPPED BEHAVIOUR CHANGE for all users

`automatic` means Murmur downloads ~214 MB in the background without asking, on a privacy-first app.

| Option | Trade-off |
|---|---|
| **A. `automatic`** *(recommended)* | Calmest — the update is simply already there next time you open Murmur. Mitigated by: inbound-only public bytes carrying no identifier beyond the version in the User-Agent; a **one-time, once-ever** disclosure toast on the first automatic download; a permanent plain-language sentence in About **and** Settings → Privacy; a `share::ledger_row` audit entry per check and download; never during a recording; requires the full install-sized free-disk headroom. |
| **B. `notify`** | Nothing arrives without a click. Maximum restraint; more friction on every update. |
| **C. `automatic` for fresh installs, `notify` for existing users** | Respects the status quo for people who never opted in. |

**Recommendation: A**, but this is genuinely your call — it is the one place this program makes the app do something on the network without being asked.

---

### D7 — Keep `Brain` as the product name?

It is a nav destination (`/brain`), a page title, an org feature ("Shared Brain"), and an in-note action ("Ask Brain") — ~40 strings. Today it ambiguously means four things: the knowledge base, the local LLM, the sidecar process, and the live reaction loop.

| Option | Trade-off |
|---|---|
| **A. Keep for the KNOWLEDGE BASE only; ban for the model and the process** *(recommended)* | Removes the ambiguity, keeps a distinctive product name. "Enable the brain" → "Turn on memory and search"; "On-device brain lifecycle" → "Memory and timeouts"; `/brain`, Shared Brain, Ask Brain all stay. |
| **B. Keep everywhere** | No work, ambiguity persists. |
| **C. Retire entirely** | ~40 strings, loses a distinctive name. |

---

### D8 — Remove "Kong AI Gateway" from the first-run wizard?

It is an enterprise product an individual first-run user cannot evaluate, and the wizard currently asks them to paste an "OpenAI-compatible base URL" before they have recorded anything. Removing it from onboarding deletes 21 `gateway` + 7 `Kong` + 4 `OpenAI-compatible` occurrences from the everyday surface in one move. **No capability is lost — it stays fully functional in Settings → AI → Advanced.**

**Recommendation: remove from onboarding.** But I cannot tell from the code whether anyone uses it today, so it is yours.

---

### D9 — Leave `TURBO_DEFAULT_MIN_RAM_BYTES` at 12 GiB? *(non-blocking)*

Sub-12 GB Macs currently default to `small` (Balanced). Lowering the gate would default them to Sharp (~1.2–1.5 GB resident).

**Recommendation: leave it.** The UX change alone already delivers the ask — on a qualifying Mac the default *is* the 875 MB turbo; what was missing was saying so. Lowering it is a real behaviour change for low-RAM users and should be gated on one RSS-over-time measurement on a signed build, not a design document.

---

## 6. What cannot be verified headless

Stated plainly. A green `cargo test --lib` proves plumbing, never any of the following.

| Claim | Why headless fails | What it actually needs |
|---|---|---|
| **The updater's self-replacement works** | The swap, signature survival through `ditto`, Gatekeeper's verdict on the installed app | A signed build + a real old→new run: install v1.0.3, publish v1.0.4, let it download/verify/install, confirm no AMFI kill and `spctl -a -vvv -t exec` accepts |
| **Touch ID still opens the KEK after a self-update** | The Keychain ACL is bound to the code signature | The same signed run, then unlock a locked folder. **The single highest-stakes unknown in the program.** |
| **The `ditto`-preserves-the-signature assumption** | The one empirical claim the whole updater rests on, and the repo's own release script uses `cp -R` successfully | `ditto` a signed `Murmur.app`, then `codesign --verify --strict --deep` + `codesign -d --entitlements :-` on the copy. **P8 preflight gate.** |
| **Is `/Applications` writable without elevation?** | Determines whether the Finder hand-off is the rare path or the common one | One real Mac. If common, the copy weighting in About shifts. |
| **Install-on-quit does not delay or block shutdown** | `RunEvent::ExitRequested` ordering with the relock/zeroize path | Signed build, ⌘Q with an armed installer |
| **Whether `recording_has_priority()` is still true at `arm_on_quit_if_ready`** | Depends on when `stop_all_capture` drops the session owner | Instrumented dev run, quit mid-recording |
| **`sysctl` keys resolve: `machdep.cpu.brand_string`, `hw.optional.arm64`, `sysctl.proc_translated`, `hw.nperflevels`, `hw.perflevel0.logicalcpu`** | **Every `sysctl` read returned "Operation not permitted" under the agent sandbox during this synthesis.** Only `hw.memsize` is proven in production (two shipping subprocess probes) | One run on a real Mac, under **both** the arm64 and x86_64 slices of the universal build |
| **Free disk via `NSURL` returns a plausible number for the models volume** | FFI + volume semantics | Real Mac |
| **`spctl` writes its assessment to stderr and the exact string form** | The gate is a string match on unversioned CLI prose | Real Mac + a captured fixture, at the 13.4 minimum target if possible |
| **A background whisper pass and a live capture coexist under real RAM/thermal load** | The measured recording peak is 14.38 GB main + 3.59 GB sidecar | One RSS-over-time measurement across Stop#1 → Start#2 → Stop#2 on a signed build. Reuse PR #425's `measure-recording-ram.sh`. |
| **Stop→Start wall clock is ~2 s, not ~30 s** | A 120-second ASR window plus an uninterruptible 2.9 GB `Transcriber::load`, against a hard 30 s Start budget | Timed dev-app run with an in-flight ASR |
| **`approx_resident_bytes` per whisper model** | Currently estimates from a research doc | Peak RSS during an Accurate batch with `small`, `large-v3-turbo-q8_0`, `large-v3` resident. **Until then every string says "about".** |
| **Real model download sizes** | No ground truth in-repo; the two `model.rs` comment blocks contradict each other on VAD/diarization | A manual `#[ignore]`d `HEAD`-per-row pass (never in CI — it is network egress). Also supplies the missing `large-v3-q5_0` size so its `provisional` flag can clear. |
| **Screen-share auto-relock moves a running queue job to HELD rather than failing it** | ScreenCaptureKit | Signed build + a real Zoom/Meet share |
| **Component styles render in the packaged WKWebView** | A green `ng build` proves nothing about the prod CSP (trap T4) | The notarized `.dmg`, not `ng serve`. Never remove `dangerousDisableAssetCspModification: ["style-src"]`. |

---

## 7. Risk register

Sorted by expected damage (probability × severity).

| # | Risk | Damage | Mitigation |
|---|---|---|---|
| **1** | **Self-update breaks the code signature → Keychain ACL severed → the user cannot open their encrypted notes.** | Catastrophic, unrecoverable | Five independent defences: `ditto` (never `tar`); the staged bundle passes the **full** pin (codesign strict + TeamIdentifier + Authority + `embedded.provisionprofile` + `CFBundleIdentifier == com.meetnotes.app` + version) **before** anything moves; `install.sh` **repeats the full pin**, not a weaker subset (E4); the installer verifies its **backup** before touching the target; post-swap re-verification restores the old bundle on any failure. Plus the empirical `ditto` preflight (§6) as a P8 gate, and a signed old→new run before P9. |
| **2** | **Sealed-content leak via `postprocess_asr_windows`** — a new table holding raw transcript text that the relock path does not clear. | Catastrophic (the privacy promise) | Cleared in **both** `reblank_folder_extras` (the funnel for `relock_folder` + `relock_all_inner`, verified lock.rs:658/:732) **and** `seal_folder_extras`; deleted on every terminal outcome; a pure cache drop (the verified archive WAV remains the source). The "unreachable" argument is deleted — LOCKED holds are normal. `lock-security-reviewer` REQUIRED on P6. |
| **3** | **Unsealed plaintext segments parked indefinitely** at the TRANSCRIBE→SUMMARIZE yield boundary under `manual` policy, which `reblank_folder_extras` skips (`text_blob.is_some()` guard). | Catastrophic | That boundary is **forbidden**. Yield points are ASR chunk edges and pre-ASR only. RED test asserting no job can be persisted in a state between `replace_segments` and the terminal seal. |
| **4** | **An update kills a running note pipeline** — `retry_transcription`, `resummarize` and startup salvage all run with `recording_has_priority() == false`. | Severe (content loss) | `PipelineBusy` RAII counter (P0) on all three plus the queue worker, `startup_recovery_has_priority()` added to `install_blockers`, blockers consulted at five points, and the "current coverage is complete" claim deleted from the design so nobody stops looking. |
| **5** | **Double-installer race** — "Install & Restart" exits the process, which re-enters `ExitRequested`, which arms a second installer over the same `mv`. | Severe | One-shot arming (`AtomicBool`/`Once`) + an explicit `phase != Installing` guard. Unit test: two calls, one spawn. |
| **6** | **Start #2 hangs 30 s then fails** instead of succeeding — a background ASR that yields only at 120-second window edges cannot beat the quiescence budget. | Severe (worse than today's honest refusal) | Sub-window `should_yield` inside the per-VAD-chunk loop; the worker refuses to claim when the model is not resident; `background_yield_notify` wakes parked awaits. **Measured, not asserted** — a timed dev-app run is the acceptance bar. |
| **7** | **De-jargoning silently makes Murmur less honest** — "names are NOT redacted" compresses beautifully into "your data is protected". | Severe (destroys the differentiator) | 15 protected clauses with file anchors, asserted by fact (not exact sentence) in `e2e/settings/privacy-honesty.spec.ts`. Implementer rule: *a rewrite of a protected clause that is shorter AND drops a noun phrase is wrong.* `lock-security-reviewer` on P3 and P5. |
| **8** | **The consent flow dies silently** — `record.component.ts:287` regex-matches the raw Rust prose to decide whether to show the Allow banner. | High (every cloud user's first note) | The `[code]` contract lands in the SAME PR as the string change; `needsCloudConsent` matches the **code**. RED test: the pre-change FE must FAIL to render the banner against the new string. **And: enumerate every other raw-string guard before `humanize()` strips prefixes** (`note-editor.component.ts:1528/:1606` is one, already broken today by case-sensitivity). |
| **9** | **Deleting the live-caption model with no way back** — `delete_whisper_model` guards only the batch selection, and the Download button is gated on `modelPresent` (the batch model). | High | A fourth refusal on the live pin + a repair affordance driven by `live_captions::dto_probe`. RED test `delete_refuses_the_live_caption_pin`. |
| **10** | **A queued job killed as collateral by `start_recording`'s sidecar/Claude-Code kills is marked FAILED**, so meeting #1 shows an error caused by meeting #2. | High | Classifier rule #1: **any** error observed while `background_should_yield()` is true is a requeue, not a failure, and does not burn a retry. A state check, never a string match, so it cannot drift. |
| **11** | **The recommendation contradicts itself for every existing install** — `tier` says one thing, `batchSize` says another, because `recommend()` was made the `default_model_size` delegate. | High (the feature reads as broken) | Split the two functions; ship both `recommendedId` and `autoDefaultId`; the six existing tests pass unedited. |
| **12** | **The updater silently stops working** because a future release names its DMG differently, invisible until users stop updating. | High | The asset matcher is pinned by a `cargo test` asserting it accepts exactly what `macos-sign-notarize.sh` constructs; no match degrades **visibly** to "Version X is out, but Murmur couldn't find a download"; the release runbook's post-release verification adds the same check. |
| **13** | **`MeetingStatus::Queued` rows destroyed at launch** by `reconcile_stuck_recordings_except`, or a downgrade to an older binary failing `from_str`. | High | `QUEUED` is invisible to `stuck_recording_ids` (which selects `RECORDING` only). RED test with three queued meetings. Downgrade is per-row and loud, never destructive; documented in release notes. Also audit `mcp.rs` and the search/graph readers. |
| **14** | **A worker spawned before `lib.rs:559` hard-kills launch** (`begin_startup_recovery()` refuses on a non-empty generation set → `show_fatal_init_dialog` → dead app). | High | Hard ordering constraint with a comment at the spawn site; the "no ordering guarantee needed" claim is deleted. |
| **15** | **Disk exhaustion mid-swap** — the 3× DMG preflight under-provisions the install by several-fold. | High | Preflight `dmg + 4 × app_uncompressed`; re-check inside `install.sh` immediately before the swap; exit 14 covers ENOSPC; the sign script emits the uncompressed size. |
| **16** | **Onboarding's unskippable model step strands a first-run user** — deleting `SIZE_HINTS` means an empty Download label until three IPCs resolve, permanently if `list_whisper_models` errors. | Medium-High | Gated label fallback; the "Show all sizes" disclosure is always openable; a "Start with Light instead" escape; e2e case for a rejecting catalog. |
| **17** | **The vocabulary gate becomes a rubber stamp** or is disabled by false positives. | Medium | WARN in P3 → FAIL in P5; separate shrinking baseline vs permanent reason-required allowlist; the scanner strips comments/class bindings/non-visible attributes (the filter that corrects the raw `egress` count from ~20 to 4) and covers **Rust** too. |
| **18** | **Merge skew across the three workstreams that all rewrite `onboarding.component.html` and `settings-transcription-section`.** | Medium | Strictly sequential PRs, the §2.3 collision map, and re-running gates on merged trunk after each merge (the documented two-green-PRs-break-trunk lesson). |
| **19** | **A background whisper pass makes the measured 14 GB recording RSS worse.** | Medium | It cannot overlap — `acquire_model_generation` still refuses `(Some(active), None)`. The worker becomes a real consumer of `topic_backfill_ram_permits_now()` (called **once**, not double-wrapped) + thermal. **Honest bar: needs one RSS measurement on a signed build.** |
| **20** | **Fabricated numbers reach users** — RAM figures, wall-clock claims, benchmark deltas. | Medium (trust) | Every RAM figure from the cited research doc, rounded **up**, source in the registry doc-comment; every string hedged "about"; the invented "16 GB" sentence deleted; benchmark numbers only in the advanced table with the "meeting audio is harder than FLEURS" caveat; Sharp and Maximum share an accuracy meter because the delta is genuinely unpublished. |

---

## 8. Verified addendum — 2026-07-26 (post-synthesis, on the real Mac)

Section 6 listed the `sysctl` probes as unverifiable because **every read returned "Operation not
permitted" during the synthesis**. That was a pure artefact of the agent sandbox, not a macOS
restriction. Re-run with the sandbox disabled on the development Mac (`hw.model` = `Mac16,5`):

| Key | Value | Consequence for P1 |
|---|---|---|
| `hw.memsize` | `68719476736` (64 GiB) | Already proven in production; unchanged. |
| `hw.optional.arm64` | `1` | The Apple-Silicon-vs-Intel branch (C2) has a working probe. |
| `sysctl.proc_translated` | `0` | Native arm64, not Rosetta. Both keys are cheap and readable. |
| `machdep.cpu.brand_string` | `Apple M4 Max` | 12 chars, `Apple ` prefix — passes the C10 normalisation rule (accept only `Apple …` ≤ 24 chars) against a **real** string, not a fixture. |
| `hw.nperflevels` | `2` | Available if a later consumer needs it. |
| `hw.perflevel0.logicalcpu` / `hw.perflevel1.logicalcpu` | `12` / `4` | ditto |
| `hw.logicalcpu` | `16` | ditto |

**Still unverified and still in §6:** the same probes under the **x86_64 slice** of the universal
build (needs an Intel Mac — and `machdep.cpu.brand_string` there returns the long
`Intel(R) Core(TM) …` form that C10's normalisation must *reject*), and the free-disk `NSURL` read.

**Trap recorded for future sessions:** hardware probes (`sysctl`, and anything reading outside the
project tree) fail with "Operation not permitted" under the default agent sandbox. That is **not**
evidence the key is missing — re-probe with the sandbox disabled before concluding anything about
hardware availability.

---

## 9. Decisions taken — 2026-07-26

Recorded so no later PR re-litigates them.

| # | Decision | Choice | Note |
|---|---|---|---|
| D1 | Updater mechanism | **Track A** — bespoke verified download → full signature pin → `ditto` swap with verified backup + automatic rollback | Zero new crates, zero key custody. |
| D2 | `objc2-foundation` `NSValue` feature | **Option B** — guarded `msg_send` under `objc2::exception::catch` | No manifest edit, no approval needed. |
| D3 | Queue default policy | **`immediate`** | Other policies one click away in the panel. |
| D4 | Stop returns `{queued:true}` instead of the note | **Confirmed** — this is the requested feature | |
| D5 | Queue cancel semantics | **Pause + Discard only** | No "cancelled-but-half-processed" state. |
| D6 | Update default mode | **`automatic`** | With the one-time disclosure, the permanent privacy sentence, and a `share::ledger_row` entry per check/download. |
| D7 | The word "Brain" | **Knowledge base only** | `/brain`, Shared Brain, Ask Brain stay; "Enable the brain" → "Turn on memory and search". |
| D8 | Kong AI Gateway in onboarding | **KEPT — the user overrode the recommendation** | Stays in the wizard; gets plain-language copy instead of "OpenAI-compatible base URL". It must NOT be removed. |
| D9 | `TURBO_DEFAULT_MIN_RAM_BYTES` | **Left at 12 GiB** | Lowering it is a behaviour change gated on an RSS measurement. |

### Deviation from the §4 sequencing

**`PipelineBusy` moves from P0 to P8.** A counter with no reader fails `cargo clippy --lib -D warnings`
on `dead_code` in CI's lib-only build — the exact class the PR-program playbook warns about. Its only
reader is `install_blockers`, which lands in P8. P8 therefore also owns applying the guard to
`retry_transcription`, `resummarize`, the `audio::spill` salvage runner, the five `commands/models.rs`
downloads, and the P6 queue worker. **P0 is consequently frontend-only and runs no cargo at all.**

### Corrections to §3/§4 counts found while scoping P0

- **`.field-help` is declared in ELEVEN component SCSS files, not five** (§3.0.1 undercounted):
  settings-notes, settings-audio, settings-transcription, ai/model-effort-picker, ai/ai-connection-card,
  ai/ai-setup-block, ai/ai-role-rows, ai/brain-posture-block, ai/local-models-list, settings-connectors,
  settings-storage. Confirmed absent from both `primitives.css` and `styles.css`.
- **`statusPillClass` is duplicated in FOUR files, not two**: `features/library/library`,
  `features/library/meetings-table-view`, `features/detail/detail`, `features/analytics/analytics`.
  Every copy maps `RECORDING` **and** `ERROR` to `is-danger`.
- **No new CSS is needed for the status-pill fix**: `.pill.is-live` already exists at
  `primitives.css:233` backed by `--live-soft` / `--live-hover`.
- The seven `progress-track` call sites in §3.0.2 are **confirmed exactly seven**.

### P1's hardware probe — the actually-zero-manifest path

`libc` is **not** a direct dependency of `src-tauri` (verified: no `libc` line in `Cargo.toml`; it is
only transitively in `Cargo.lock`). So `use libc::sysctlbyname` would still require a manifest edit,
which the §5 D2 discussion glossed over.

The genuinely zero-touch path is already shipped in-tree: `transcribe/model.rs::total_ram_bytes`
(model.rs:157) reads `hw.memsize` by **spawning `sysctl -n`**. P1 extends that same proven pattern to
`hw.optional.arm64`, `sysctl.proc_translated` and `machdep.cpu.brand_string`, and computes the whole
`MachineProfile` **once** behind a `OnceLock`. Net effect: no manifest edit, no new crate, and *fewer*
subprocess spawns than today (the current code re-spawns per call — the §3.3 note about "one fewer
`sysctl` subprocess per `list_brain_models`" is the same observation).

Free disk keeps decision D2: `objc2` already enables the `exception` feature (`Cargo.toml:170`), and
`objc2-foundation` already has `NSURL`/`NSDictionary`/`NSArray` but **not** `NSValue` — so the
`NSNumber` that `resourceValuesForKeys:error:` returns is read with a `respondsToSelector`-guarded
`msg_send![num, longLongValue]` wrapped in `objc2::exception::catch`. Zero manifest change confirmed.

### P8 PREFLIGHT GATE — PASSED 2026-07-26 (the `ditto` assumption is now measured, not assumed)

§4/P8 made this a hard gate before writing a line of updater code, and risk #1 called it the
program's highest-stakes unknown. **It passes.** Run against the real notarised
`/Applications/Murmur.app` 1.0.3 (`Identifier=com.meetnotes.app`,
`Authority=Developer ID Application: Jakub Gawroski (BVF778E5QD)`, `TeamIdentifier=BVF778E5QD`),
`ditto`'d to a temp dir and re-verified **inside-out** (per-helper, never `--deep` — the repo hook
correctly refuses `--deep`, and per-helper is the stronger check anyway):

| Check on the `ditto` copy | Result |
|---|---|
| `codesign --verify --strict --verbose=2` (main bundle) | `valid on disk` + **`satisfies its Designated Requirement`**, exit 0 |
| `codesign --verify --strict` on each of the 4 nested helpers (`meetnotes-aeccap`, `-audiocap`, `-calendar`, `-sysaudio`) | all exit 0 |
| `codesign -d --entitlements :-` | **`keychain-access-groups` = `BVF778E5QD.com.meetnotes.app` PRESERVED**, alongside `application-identifier`, `allow-jit`, `allow-unsigned-executable-memory`, `disable-library-validation`, `device.audio-input`, `personal-information.calendars` |
| `Contents/embedded.provisionprofile` | survived, byte-size identical (12275) |
| `spctl -a -vvv -t exec` | **`accepted` / `source=Notarized Developer ID`** |

**Consequence:** Track A is viable. The Keychain ACL — and therefore Touch ID's ability to release the
master KEK after a self-update — survives a `ditto` self-replacement. Risk #1's five defences stay in
the design, but its premise is no longer speculative.

**Correction to E3 (disk preflight).** `ditto` of the 178 MB bundle completed in **0.17 s** because
APFS `clonefile` makes a same-volume copy near-free in both time and space. So `dmg_bytes + 4 ×
app_uncompressed_bytes` over-provisions badly for the *rollback* copy (same volume → a clone). The
real cost is the DMG plus the extraction **off the mounted image** (a different volume → a genuine
copy). Size the preflight as `dmg_bytes + 2 × app_uncompressed_bytes` and keep the re-check inside
`install.sh` immediately before the swap; do **not** silently rely on cloning, since the target volume
is not guaranteed to be APFS.

**Also measured:** the installed bundle is **178 MB**, not the ~214 MB the §5 D6 discussion assumed for
the download. Use the real DMG size from the release artefact, never a remembered figure.