# Performance / memory audit — the "open a 1h meeting → OOM kills Murmur + Chrome" crash

**Date:** 2026-07-07
**Trigger:** Recorded a ~1h meeting, opened it in Detail → system-wide OOM (macOS killed Murmur *and* other apps, incl. Chrome). A system-wide kill = a **multi-GB** allocation, not a big DOM.
**Method:** inline code recon + `murmur.log` evidence, then a 6-finder adversarial Workflow (fe-render, fe-memory, record-pipeline, on-open-cascade completed; crash-chain + be-memory were cut off by a session limit — their ground is covered by my own recon + on-open-cascade). Every claim below was confirmed against the current working tree (symbols, not line numbers — the files drift).

---

## 1. Root cause of the crash

The crash is **cumulative on-device model residency with no memory-pressure guard anywhere**, tipped over the edge by the work Murmur fires when a meeting opens. It is *not* a single 6.3 GB model — the earlier "Bielik loads on open" framing was **corrected** (see the ⚠️ box).

**The residency baseline after a 1h recording (all never-evicted):** `murmur.log` shows the user is on `brain_backend = Local` with **whisper large-v3** and **`brain_live` (Realtime Reactions) ON**. So during/after the recording, resident at once:
- whisper `WhisperContext` (`transcribe/whisper.rs` `Transcriber`, held for reuse) — **large-v3 ≈ 3.1 GB**;
- **Qwen3-1.7B** light brain (1.28 GB) — loaded by `brain_reactions::reactions_scan` **every ~21 s** during recording (`transcribe/live.rs`, `REACTIONS_SCAN_EVERY=7 × TICK=3 s`), then never freed;
- **Qwen3-4B** heavy brain (2.50 GB) — loaded once, never freed (`reason/mistral.rs` `model_cache` **"REFUSES rather than evicts"**, `MODEL_CACHE_CAP=2`, drop-leaks #723/#865);
- e5 embedder (470 MB), + the record-pipeline **Stop-time peak (~1.4–2 GB transient**, see P1).

That is already **~8–9 GB** of Murmur RSS by the time the recording finishes — with **zero** device-RAM awareness: `MODEL_CACHE_CAP` gates a *count*, not memory, and the `residency_fits`/`combined_residency_gb` estimators (`reason.rs`) are only called by read-only capability probes, **never on the load path** (mem-1).

**What opening the meeting adds (the tipping point):**
- `detail.component.ts › loadMeeting()` calls `void this.loadTimeline()` on **every** unlocked open (and in `unlock()`), no guard — even though the timeline only renders on the Audio tab (default tab is `note`). → `get_timeline` → for an uncached meeting, `summarize/timeline.rs › generate()` joins **all** segments into **one un-chunked prompt** (~15–25k tokens for 1h).
- The default Note tab also mounts `<app-related-meetings>` (`note-panel.component.html`), whose effect fires `relatedMeetings()` → `embed_passage()` → the **e5 embedder** runs on open (confirmed by repeated `local embed model ready e5-small` in the log), concurrent with the timeline work.
- **KV scales with the actual prompt length.** mistralrs plans for `AutoDeviceMapParams::DEFAULT_MAX_SEQ_LEN = 4096` (PagedAttention OFF), but a 1h transcript blows past it → **several GB of prefill KV** at runtime on top of the resident weights. (A truncating path would also derive the timeline from only the first ~4096 tokens ≈ first minutes — latent correctness bug.)

Stacked on the ~8–9 GB baseline, this tips a 16 GB Mac past physical RAM → macOS memory-pressure kill of Murmur **and** other apps (Chrome).

> ⚠️ **CORRECTION — the timeline/note path uses the CLOUD provider by DEFAULT, not the local brain.** `summarize/roles.rs resolve(Role::Notes)` returns `legacy_default_target` = `cfg.provider_id` (default `claude_code`); the unit test `resolve_identity_matrix_with_keys_absent` asserts *"Notes ignores brain_backend entirely"*. **Only `Ask`/`Live` map `brain_backend → Local`.** So `get_timeline`/note-generation run a **local** model **only if the user explicitly set `role_notes_connection = local`** (the per-role AI-settings picker). Whether the on-open timeline was itself a local call therefore depends on the user's config; regardless, the ~8–9 GB baseline + the on-open e5 + KV spike is the mechanism, and the selected heavy model is **Qwen3-4B (2.5 GB)**, not Bielik (Bielik is downloaded but the logs show Qwen loading).

> ✅ **BF16 question — RESOLVED (false alarm), verified in mistralrs-core 0.8.1:** GGUF weights **stay quantized** (`models/quantized_llama.rs` — every attn/FFN matrix is `GgufMatMul` over a `QTensor`; only `tok_embeddings` is dequantized). `DType selected is BF16` is the compute/activation dtype (`utils/normal.rs`), not a weight dequant. So a Q4 model stays ~its on-disk size in RAM, **not** ~3× larger.

**Immediate mitigation (no code):** (1) switch the recording model from **whisper large-v3 → small** (saves ~2.6 GB resident); (2) turn **Realtime Reactions (`brain_live`) OFF** (drops the resident light model + the every-21 s inference); (3) if notes are set to a local model, switch that role to **Cloud**. Any one buys headroom; together they almost certainly stop the OOM today.

---

## 2. P0 — stop the crash (surgical)

**P0.1 — Do NOT auto-generate the timeline on open.** *(highest leverage; FE-only)*
Move `void this.loadTimeline()` out of `loadMeeting()` **and** `unlock()`. Trigger it lazily only when the Audio tab first activates and it's still empty:
```ts
private readonly _timelineOnAudioTab = effect(() => {
  if (this.activeTab() === 'audio' && this.detail() && !this.locked()
      && !this.timeline() && !this.timelineLoading()) {
    void this.loadTimeline();
  }
}, { allowSignalWrites: true });
```
- File: `src/app/features/detail/detail/detail.component.ts`.
- **RED test** (Playwright @ `:1420`, mocked `invoke`): open a meeting → assert `getTimeline` is **not** invoked while `activeTab==='note'`; click Audio → assert it's invoked exactly once. Fails on current code, passes after.

**P0.2 — Cap the transcript fed to ANY local model — timeline AND note generation.** *(backend; the KV lever)*
The KV cost is driven by **prompt length** (mistralrs plans for `max_seq_len=4096`; the bug is feeding a ~15–25k-token 1h transcript past it). Two call sites join **all** segments with **no cap** and hand them to a local model:
- `summarize/timeline.rs › generate()` — the on-open timeline path.
- ✅ **DONE (2026-07-08)** — **`summarize/template.rs › render_prompt()`** — the **note-generation** on-device path. `render_prompt` serves *only* local mistralrs + Ollama (cloud uses `render_user_content` directly), so it now caps the transcript to `LOCAL_NOTE_MAX_CHARS = 40_000` (matching `chat.rs`/`recipes.rs`) — the OOM twin of the timeline cap, on the primary recording flow (mem-2). Cloud note quality is untouched. Tests: `on_device_note_prompt_caps_long_transcript` + `_short_transcript_unchanged`. Full suite 1162 green.
Fix: window/chunk or hard-cap the transcript for local/weak providers (provider-aware, like `related_context.rs::budget_for`); a shared helper so both cap consistently. Optionally enable `.with_paged_attn(...)` in `reason/mistral.rs › model()` and/or lower `max_num_seqs` from 32.
- **RED test**: `template.rs` unit test mirroring `recipes.rs::truncates_long_transcript` — `req.transcript = "word ".repeat(20_000)`; assert `render_user_content(&req)` is char-bounded + contains `[transcript truncated]`. Fails today, passes after. Same for `timeline::generate()`.

**P0.3 — Refuse instead of OOM: a real device-RAM guard on EVERY on-device model load.** *(the config-robust systemic fix — mem-1)*
Today **nothing** checks memory before a load: `reason/mistral.rs › model()` gates only on `cache.len() >= MODEL_CACHE_CAP` (a *count*), and the `residency_fits` / `combined_residency_gb` estimators (`reason.rs`) are only called by read-only capability probes (`brain_live_ram_ok`, `brain_model_dtos`) — **never on the load path**. Fix: gate `model()` *before* `GgufModelBuilder::build()` on projected combined residency, returning `AppError::Unavailable` so the caller degrades to the deterministic floor instead of OOM-ing the machine. Two musts: (1) probe **FREE/available** RAM (`sysctl vm.page_free_count × page size` — no new crate), not just `total_ram_gb()` (`hw.memsize`), so co-resident pressure is caught; (2) make `combined_residency_gb`'s KV term **scale with prompt length** instead of the flat `KV_PER_MODEL_GB=1` (`reason.rs`). Apply the same pre-load guard to whisper (`whisper.rs`), e5 (`candle_bert.rs`), NER (`ner_deberta.rs`). **RED test**: parameterize `model()`'s RAM ceiling (like `residency_fits`'s `Option<u64>`) and assert it refuses when a small injected ceiling is exceeded — no forward pass needed.

**P0.4 — Stale-result / in-flight guard on `loadTimeline()`.**
Mirror `resummarize()`: capture `id`, and after the await only `this.timeline.set(...)` if `this.detail()?.meeting.id === id`; short-circuit if a generation for the same id is already in flight.
*Provider-agnostic* (the timeline provider is whatever `Role::Notes` maps to — a local heavy model only when `brain_backend=Local`). Effect of an overlap differs: on **Cloud** two concurrent generations just waste a subprocess call / double tokens+egress (no OOM); on **Local** they overlap **KV-cache / prefill working set** — **not** the weights, which are a single shared `Arc<Model>` in the never-evict `model_cache` (loaded once, not duplicated). So this is a belt-and-braces cost/robustness fix, **not** an ogniwo of the OOM — after P0.1 (no timeline on open) its practical window is only "opened the Audio tab, then rapidly switched meetings".

---

## 3. P1 — high-impact perf / memory

- **Defer the `related-meetings` embed off the on-open path** *(HIGH, memory)* — `related-meetings.component.ts` fires `relatedMeetings()` (→ e5 forward pass) the instant `meetingId` is set, on the default tab. Wrap in `@defer (on viewport)` / fire on idle, and never concurrently with a timeline generation. Consider caching the meeting centroid so "Related" is a cheap KNN, not an on-open embedding pass. *(It already has a stale guard — the problem is the eager embed, not a leak.)*
- ✅ **DONE (2026-07-09) — Record Stop-time peak** *(CRASH-class, memory)* — `pipeline.rs › run_inner`: the source-rate mic buffer `samples` (~691 MB/h @48 kHz) is now wrapped in `Option` and **dropped immediately after resample** in the common (no-hi-res-masters, default) path, so it no longer co-resides with `mic_16k` + `sys_16k` + `archive_16k` — roughly halving the pre-whisper Stop-time peak. When `keep_hires_masters` is on, `samples` is retained (`Some`) and the faithful `.mic.wav` master write after finalize is byte-identical (no error-path change). 1163 lib tests green. *(Remaining micro-opts — `mic_16k_archive` early drop, the redundant `resample_to_16k` inside `write_wav_16k_mono` — left as follow-ups; the `drop(samples)` is the dominant win.)*
- ✅ **DONE (2026-07-09) — Whisper whole-hour unchunked decode** *(CRASH-class, memory)* — `pipeline.rs › transcribe_stream` now tiles every region through a new pure `decode_windows(start, end, window_len)` with `MAX_WINDOW_S = 120`, so a VAD-less/whole-buffer or long continuous region never decodes >120 s at once (bounds whisper's mel + `set_n_max_text_ctx(16384)` working set to O(window)); per-window timestamp offsetting preserved. 3 tiling tests (`decode_windows_*`). *(Real memory delta needs a signed-Mac run; logic is unit-verified.)*
- 🔴 **DEFERRED (needs a signed-Mac session) — Whole-recording PCM in RAM** *(HIGH, memory)* — `audio/recorder.rs`: the authoritative mic buffer is a single growing `Vec<f32>` for the whole recording (~700 MB/h; `MAX_RECORDING_SECONDS=4h` ⇒ ~2.8 GB cap). The fix (bounded rolling RAM window + stream authoritative PCM to the existing `SpillWriter` + reconstruct/transcribe from disk in chunks at Stop) rewrites the live-capture path — **audio integrity (no dropouts, crash-safety) cannot be verified headless or in a mock; it needs real capture on a signed Mac.** Not blind-shipped to real users. Plan: (1) cap `Shared.samples` to a rolling window sized for `snapshot_tail` live-ASR; (2) make `SpillWriter` the authoritative full-PCM sink; (3) at Stop, `run_inner` sources the full buffer from the spill file instead of the drained RAM `Vec`. Verify with a real 1h recording + waveform/transcript diff against the current path.
- **Live + batch whisper contexts coexist** *(MEDIUM, memory)* — `transcribe/live.rs` self-terminates only on its next 3 s tick, so at Stop both the live and batch `Transcriber` (model weights + Metal buffers, up to ~2.9 GB each for large-v3) can be resident for ~3 s. Signal+join the live thread (or share one `Transcriber`, it's `Send+Sync`) before loading the batch model.
- ✅ **DONE (2026-07-09) — Transcript CD storm + DOM count.** Two parts: (1) the per-fragment **method** binding is gone — `audio-panel.component` derives `activeSegKeys = computed<Set<number>>` (one O(n) scan per `currentTime` tick), template binds `[class.is-active]="activeSegKeys().has(s.idx)"` (O(1)/fragment; a Set preserves highlighting every active fragment when me/others overlap) — kills the ~8k-eval/s storm. (2) **Windowing:** `renderedTurns` caps the rendered turns at `RENDER_CAP = 80` (always extended to include the karaoke-active turn, so auto-scroll never targets an un-rendered row) behind a "Show all N turns" expander — a 1h meeting no longer materializes thousands of `<button>` nodes at once. e2e `transcript-cap.spec.ts` (100 turns → 80 render → Show all → 100) + `timeline-defer` green; ng lint+build green. *(Follow-up: true viewport windowing bounded even during deep playback, and optional `currentTime` throttle to 2–4 Hz.)*

---

## 4. P2 — cleanups

- `meeting-timeline.component`: `isActive()/isActiveChapter()/range()/fmt()` bound as methods, re-run per span each tick → precompute `activeBlockOrder`/`activeTopicOrder` computeds + precompute `range`/label as fields on the geometry object.
- `audio-panel › _karaokeScroll` and `meeting-conversation.component` scroll effect: re-register `afterNextRender` on every tick / every `notes()` change (forced reflow via full `turnRows()` scan). Register once / gate on actual content growth.
- `meeting-conversation.store.ts` (root singleton): `_notes`/threads retained until a different meeting id is set (bounded to one meeting, not the OOM). Clear on leaving the record surface / soft-cap threads.
- `double-mic-copy-when-aec-or-postaec` (`pipeline.rs`): extra full-length 16k mic copies — lower priority (opt-in, `post_aec_enabled`/`keep_hires_masters` default OFF); mostly neutralized by the early `drop(samples)` in P1.
- Note-tab IPC fan-out: `getConfig`, `getMeetingTags`, `folders.load`, `getActionItems`, `listBuiltin/SavedRecipes` all fire eagerly on open. Cache `getConfig` app-level; put non-critical sections behind `@defer (on viewport)`.

**Verified NOT a bug (don't chase):** FE event streams all hold+release `UnlistenFn`; no `.subscribe()`-to-field; no full-WAV read into JS (audio is `convertFileSrc`/`asset:` streaming only); `live_transcript` RAM tail is bounded (`MAX_LIVE_TRANSCRIPT_CHARS=16_000`); FE live caption is `set` not append; `SpillWriter` streams only the delta; Library list rows are lightweight (no N+1); Graph is a bounded card list; no `track $index` on keyed data anywhere.

---

## 5. Suggested execution order

1. **P0.3** (device-RAM refuse-not-OOM) — the **config-robust** first fix: it stops the machine dying regardless of which role/model/backend the user picked. Ship it first. *(`rust-tauri-dev` → `adversarial-verifier`.)*
1b. **P0.1** (FE, no timeline on open) — removes the on-open KV spike + e5 co-load for the common path. RED Playwright test. *(No lock/crypto surface; `get_timeline` already gates on `meeting_is_unlocked`.)*
2. **P0.4** (stale guard) — trivial, same file, same PR as P0.1.
3. **P0.2 + P0.3** (backend context cap + RAM-refuse) — one Rust unit (`rust-tauri-dev`), `cargo test --lib` RED→GREEN. Touches `reason/mistral.rs` + `summarize/timeline.rs` — **not** a lock path, so no lock-security-reviewer needed; still route through `adversarial-verifier`.
4. **P1 record-pipeline trio** (drop(samples), whisper chunk cap, live/batch context) — one Rust unit; note `pipeline.rs`/`live.rs`/`state.rs` are already dirty (two-stage-notes WIP) — coordinate to avoid co-mingling.
5. **P1 FE** (defer related-meetings; transcript virtualization + activeFragKey) — `angular-zoneless-dev`; `ng build` + Playwright.
6. **P2** — batch cleanup pass.

**Needs a signed build / real Mac to verify (honesty bar):** the actual RSS of the resident model set + prefill KV for a real 1h prompt (source confirms Q4 weights stay ~on-disk size; KV is estimated, not measured); the real 1h capture memory curve; and that the machine no longer OOMs on open after P0. Headless `cargo test` / `ng build` prove the code paths, **not** the live memory behavior (a forward pass never runs in tests).

---

## 6. Re-analysis addendum (2026-07-08) — corrections + newly-found items

**Two corrections to the first-pass framing** (both now folded into §1):
- **Notes routes to CLOUD by default** — `roles.rs resolve(Role::Notes) = legacy_default_target` (test: *"Notes ignores brain_backend entirely"*). The on-open timeline / note generation runs a **local** model only if the user explicitly set `role_notes_connection = local`. Only `Ask`/`Live` follow `brain_backend`.
- **BF16 dequant was a false alarm** — GGUF weights stay quantized; a Q4 model stays ~on-disk size in RAM. The active heavy model in the logs is **Qwen3-4B (2.5 GB)**, not Bielik.

**New findings (from the be-memory + broaden re-analysis; verify agents for the broaden set were cut off by a session limit — treat those as finder-reported, re-verify before acting):**
- **mem-1 (HIGH → P0.3):** no device-RAM guard on any model load — see P0.3.
- **mem-2 (HIGH → P0.2):** the note-generation path (`template.rs render_user_content`) lacks the `MAX_TRANSCRIPT_CHARS` cap that `chat.rs`/`recipes.rs` have — see P0.2.
- **F4 / mem-3 (MEDIUM → P1):** `embed::active_embedder()` builds a **fresh `CandleBertEmbedder` per call** (per-instance cache), so e5 (470 MB) reloads on every related-meetings / MCP / Ask op. Give it a process-global load-once cache like the brain's `model_cache`. (Trade-off: keeping it resident costs ~470 MB steady — gate residency behind the P0.3 RAM guard, or keep the transient-reload-then-free which is gentler under memory pressure.)
- ✅ **F5 DONE (2026-07-09):** `db.rs graph_edges_visible` now appends `LIMIT 600` after `ORDER BY weight DESC` — the unbounded quadratic self-join returns only the strongest edges (the brain-map shows ≤60 nodes). The LIMIT sits *after* the `visibility_clause` WHERE, so it only trims magnitude and can never widen visibility (pending lock-security-review confirm).
- **F7 (LOW → DEFERRED):** `db.rs list_entities_visible` no-LIMIT. **NOT** blanket-LIMIT'd on purpose — it's shared with `brain_reactions`/`proactive`, which match *freshly-mentioned* (low-count) entities; an `ORDER BY cnt DESC LIMIT` would drop exactly those → a reactions regression. Correct fix = window-filter the recording-time scan paths (or an additive index to bound the GROUP BY cost). Deferred, not forced.
- **F7 (LOW → P2, perf):** `db.rs list_entities_visible` is an unbounded full-vault `GROUP BY` with **no LIMIT**, re-run on every reactions scan (~21 s during recording) and every graph open. Push the window/limit into SQL for the scan paths.
- **F6 (by-design, watch):** Realtime Reactions runs the light GGUF every ~21 s during recording (`brain_live` ON) — correctly gated + off the tick thread, but it sets the high memory floor documented in §1. Consider not keeping the light model resident once reactions stop.
- **F8 (verified clean):** app startup loads **no** model and builds no index — the heavy work is all on-demand at first inference. Not a boot-time problem.
