<!-- Deep-review round of the Murmur Brain implementation, 2026-07-04. A 6-dimension Workflow (per-finding adversarially verified) raised 23 findings; 22 confirmed, 1 refuted. This records the disposition. -->
# Murmur Brain — deep-review disposition

After the initial build + lock-security PASS + adversarial-verifier PASS, a 6-dimension review Workflow
(wiring / invariants / spec-conformance / realtime / frontend / robustness), each finding independently
verified, raised **22 confirmed** issues (1 refuted). Disposition below.

## Fixed (correctness / privacy / wiring — real defects)

1. **`select_brain_model` didn't wire the class handle** (found + fixed pre-Workflow). Selecting a model
   set only `brain_model_id`, but `light()`/`heavy()` read `brain_light_model_id`/`brain_heavy_model_id`
   → "Enable Brain Live" left `light()` on the un-downloaded default → silent stub, reactions never
   fired. Now `select_brain_model` sets the class-specific id from the model's `ModelClass`.
2. **`derive_posture` ignored the Live axis** — Notes+Ask local but @brain (Live) cloud rendered
   "Fully Local" while @brain egressed. Now the FullyLocal branch requires `live_od` too; the mismatch
   renders `Custom`. Test added.
3. **Worker-thread panic wedged `reactions_busy=true`** — a panic in `reactions_scan` skipped the reset
   → all further scans this recording died. Now an RAII `BusyReset` guard resets on every exit.
4. **Dismissed/emitted whisper cards resurfaced** — the scan re-emitted the same contradiction every
   ~21 s (and re-inflated the shadow count). Now a per-recording `AppState.reactions_emitted` dedup set
   (keyed entity|predicate|old-value), cleared at `start_recording`, surfaces each contradiction once.
5. **Download resume couldn't recover a complete `.part`** — a fully-downloaded partial → 416 → permanent
   error. Now a 416 on a resumed request means "complete" → skip to verify + promote.
6. **`light()`/`heavy()` built a fresh tokio Runtime per call** (a thread leak at the ~21 s reaction
   cadence). Now one process-wide `brain_rt()` shared by all instances; a `MistralReasoner` holds only
   paths.
7. **FE (CRITICAL): posture presets clobbered by the next Settings save** — `set_brain_posture` wrote the
   role keys but the reactive form kept stale "" values → the next `save()` reverted the posture
   (zero-egress regression). Fixed FE-side by re-patching the form from fresh config after a posture
   change. (+ posture label refresh after save/role-edit; `brainLiveModelReady` aligned to the
   selected/default light; Brain Live card warns on low RAM via the new `brain_live_ram_ok` command.)

## Cleaned up (dead code that over-claimed)

- **`AppState::is_recording()` removed** — its doc claimed it "drives the recording-active gate", but it
  had zero callers and the gate was never wired. Removed rather than ship a lying dead helper. The
  recording-active contention deferral is documented as deferred below.
- **RAM guard made reachable** — `combined_residency_gb`/`residency_fits` were tested pure fns nobody
  called; now surfaced via the `brain_live_ram_ok` command (the Brain Live card gates on it).

## Deferred honestly (Mac-perf / scope — NOT wired this PR)

Every item here is a signed-build/real-Mac concern that the user tests on the finished feature, or an
explicitly out-of-scope surface. None is a correctness/leak defect.

- **EWMA tick-latency backpressure sensor (spec §4.3)** — only `skip-if-busy` + a fixed ~21 s cadence
  ship. The adaptive backoff keyed on live-caption latency is a refinement; the interim throttle is
  reasonable and the contention it guards is Mac-measured (Spike A).
- **Recording-active gate (spec §3.3)** — heavy/batch/reindex do not defer during a recording. The
  helper was removed (above); wiring a real deferral needs a re-trigger queue and is Mac-perf. Deferred.
- **Cold-launch / record-start warm-up (spec §3.3)** — the light model still loads lazily on first use
  (mid-meeting). A pre-warm at record-start is Mac-perf hardening; deferred.
- **Onboarding posture step (change-map #6)** — the 3-posture chooser ships in Settings only; onboarding
  still shows the legacy provider posture. Deferred (a user reaches postures via Settings).
- **`sha256` registry pins are all `None`** → integrity verification is dormant (warns + skips). Pin real
  HF hashes + gate "default-offered" on `sha256.is_some()` before shipping as a default.
- **`resolve_window_entities` current-meeting exclusion is a no-op** — safe today (a recording's facts
  aren't persisted mid-call), documented at the call site; a defensive exclusion is a later nicety.
- **Rail lock-purge count-drop heuristic** — a simultaneous relock+unlock (equal net visible-folder
  count) doesn't purge; defense-in-depth over the (airtight) backend emission gate. FE nicety.

## Refuted (not a defect)

- "Shadow-mode calibration only refreshes on a proactive recall hint, so it can never appear" — false:
  the shadow count is refreshed on recording stop via the `_shadowOnStop` effect in `record.component.ts`,
  so it reliably surfaces even with zero recall hints.
