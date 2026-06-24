# MeetNotes — Phase 0 Integration Status

> Date: 2026-06-24. Author: integrator pass over the Phase 0 scaffold.
> Scope of this document: what is implemented, what was **actually verified to compile/
> pass** vs. what is **unverified**, exact local build/run steps, and prioritized next
> steps. Honest by construction — claims below are backed by commands that were run on
> this machine.

---

## TL;DR

- **Compiles: YES.** `cargo check`, `cargo build`, and `cargo test` (31 tests) all pass for
  the full Rust crate including `whisper-rs`/whisper.cpp (cmake), `cpal`, `tauri`,
  `rusqlite`, `reqwest`. The Angular frontend builds clean (`ng build`).
- **What runs (verified by build/test, NOT by launching the GUI):** the whole pipeline is
  wired behind the Tauri commands; unit tests cover storage, settings round-trip, WAV
  resample, Obsidian export, and title derivation.
- **What is stubbed:** nothing in the Phase 0 pipeline is `todo!()` anymore. The only
  *deferred* (by-design, not stubbed) items are Phase 2+ features: system-audio capture,
  diarization, live transcription, Library/Detail UI, template editor, model-picker UI,
  signing/notarization.
- **Not yet verified:** the running GUI E2E walking-skeleton (step 10 of the plan) was
  **not executed** — `tauri dev` was not launched in this environment (no interactive
  desktop session / mic). See "Unverified" below.

---

## 1. What was implemented in this integration pass

The leaf modules (audio, transcribe, summarize/*, export, storage/db, secrets-signature)
were already authored in the scaffold. This pass implemented the **orchestration glue**
that was left as `todo!()`, fixed **interface drift** against the real crate/framework
APIs, and verified the build.

### Newly implemented (were `todo!()` stubs)
- **`src-tauri/src/state.rs`** — `AppState::init()`: opens SQLite at
  `~/Library/Application Support/MeetNotes/meetnotes.sqlite`, runs migrations, loads
  `AppConfig`.
- **`src-tauri/src/settings/config.rs`** — `AppConfig::load`/`save` against the `settings`
  k/v table (empty strings ↔ `None` for optional fields). + 3 unit tests.
- **`src-tauri/src/secrets/keychain.rs`** — `set_secret`/`get_secret`/`delete_secret` via
  `keyring` v3 (`NoEntry` → `None`/idempotent delete).
- **`src-tauri/src/pipeline.rs`** — `run_after_stop` (full WAV→transcribe→persist→
  summarize→export→status-events flow), a shared `summarize_and_export` helper, and
  `resummarize_existing` (re-runs summary+export from stored segments). Status events are
  emitted on `meetnotes://status` at every stage; on error the meeting is marked `Error`
  and an `error` event is emitted before propagating. + 4 unit tests for title derivation.
- **`src-tauri/src/commands.rs`** — all 10 Tauri commands implemented: `start_recording`,
  `stop_recording`, `recording_level`, `get_last_note`, `get_config`, `save_config`,
  `set_anthropic_key`, `has_anthropic_key`, `provider_statuses`, `resummarize`. Includes
  DTO↔config mapping and a `compute_duration_s` helper.

### Interface drift reconciled (shared/leaf files edited to make the crate compile)
1. **`audio/recorder.rs` — `cpal::Stream` is `!Send`** (CoreAudio boxes a `!Send`
   callback). The original `Recorder` held the `Stream` directly, which made `AppState`
   non-`Send`/`Sync` and broke `tauri::State<AppState>` (this caused **all 52** of the
   first error wave). **Fix:** the `Recorder` now runs cpal on a **dedicated OS thread**;
   the stream is created, played, and dropped entirely on that thread and never crosses a
   thread boundary. The owner holds only `Send` handles (`Arc<Shared>` for samples/peak +
   a stop `mpsc` channel). The public `start`/`stop`/`level` API (PHASE0-PLAN §5.4) is
   unchanged, and `Recorder` is now `Send + Sync`.
2. **`transcribe/whisper.rs` — `whisper-rs` 0.16 API change.** The scaffold used the old
   `state.full_n_segments()? / full_get_segment_text(i)? / full_get_segment_t0/t1(i)?`
   shape. In 0.16 `full_n_segments()` returns `c_int` (not `Result`) and per-segment data
   is read via `state.get_segment(i) -> Option<WhisperSegment>` then
   `.to_str_lossy()` / `.start_timestamp()` / `.end_timestamp()`. Rewrote the segment loop
   accordingly (timestamps are still centiseconds).
3. **`storage/db.rs::row_to_meeting`** — the inner closure used `?` on `row.get()`
   (`rusqlite::Error`) while declaring an `AppError` return, and there is no
   `From<rusqlite::Error> for AppError`. Rewrote it to read columns as `rusqlite::Result`
   first, then fold the status-string parse. Also **added `Db::get_segments`** (needed by
   `resummarize_existing`; the scaffold only had `insert_segments`).
4. **`src/app/app.config.ts` — Angular 18.2 zoneless API.** The plan pinned Angular 18.2
   but used `provideZonelessChangeDetection` (only stable in v19/v20). On 18.2 the symbol
   is `provideExperimentalZonelessChangeDetection`. Switched to it.
5. **`package.json` — `zone.js` peer conflict.** Plan pinned `zone.js@^0.15.0`; Angular
   18.2 requires `zone.js@~0.14.10`. Repinned to `~0.14.10` (app is zoneless anyway; this
   only satisfies CLI peer deps). `npm install` then succeeds with no `--legacy-peer-deps`.

---

## 2. Verification — what actually passed (with the commands)

All run on macOS 26.5 (Darwin 25.5.0), arm64, after installing the toolchain (see §4).

| Check | Command | Result |
|---|---|---|
| Rust typecheck | `cargo check` (in `src-tauri/`) | **PASS**, 0 errors, 0 warnings |
| Rust build (incl. whisper.cpp + Tauri context) | `cargo build` | **PASS** → `target/debug/meetnotes` (~51 MB) |
| Rust tests | `cargo test` | **PASS** — 31 passed, 0 failed |
| Frontend build | `npx ng build` | **PASS** → `dist/meetnotes/browser` |
| Tauri CLI present | `npx tauri --version` | `tauri-cli 2.11.3` |

`cargo build` compiling successfully is meaningful here because `lib.rs` runs
`tauri::generate_context!()` and `tauri::generate_handler![...]` at compile time — so the
**command registry, capabilities, and frontend-dist wiring all type-check**, and the
`gen/schemas/*` were generated.

Test coverage that passed (31): WAV resample (sine downsample, passthrough, empty,
zero-rate), Obsidian export (filename derivation, subfolder, idempotent re-export,
collision suffixing, no temp files, vault-title listing, vault detection), storage
(migrate idempotent, meeting lifecycle, latest-meeting ordering, segment
replace/cascade, note upsert/export-path, settings k/v), settings config
(defaults/round-trip/empty-option), claude_code front-matter detection, and the new
pipeline title-derivation cases.

---

## 3. Unverified / NOT claimed

- **GUI E2E walking skeleton (PHASE0-PLAN step 10) was NOT run.** `tauri dev` /
  `tauri build` were not launched — this environment has no interactive desktop session,
  no microphone, no Whisper model on disk, and no `claude` CLI guaranteed in PATH. So the
  following are **wired and compile but were not observed working at runtime**: live mic
  capture, an actual Whisper transcription, a real provider summary, a `.md` landing in a
  vault, and the status-event stream rendering in the UI.
- **Microphone permission on macOS.** `src-tauri/Info.plist` contains
  `NSMicrophoneUsageDescription`, but it is **not referenced from `tauri.conf.json`**.
  Tauri 2 auto-merges `src-tauri/Info.plist` into the bundle for `tauri build`; for
  `tauri dev` the first `cpal` capture should still trigger the OS prompt, but this was
  not confirmed. If capture fails silently in `dev`, this is the first thing to check.
- **`whisper-rs` Metal at runtime.** It compiles with the `metal` feature; GPU inference
  was not exercised (no model file present).
- **`clippy`** is not installed in this toolchain (`rustup component add clippy` to add);
  lint beyond `rustc`'s own (clean) was not run.
- **Provider availability/summarize** against real `claude`/Ollama/Anthropic endpoints was
  not exercised — only the code paths compile and unit-level invariants hold.

---

## 4. Exact local build & run steps

### One-time toolchain (already done in this environment)
```bash
# Rust (rustup → cargo + rustc). Installed: rustc/cargo 1.96.0.
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"          # add cargo to PATH for the current shell

# Native build deps for whisper.cpp. Installed: cmake 4.3.4, pkg-config 2.5.1.
brew install cmake pkg-config

# Frontend + Tauri CLI deps.
cd /Users/jakubgawronski/Projects/meetnotes
npm install                        # @tauri-apps/cli 2.11.3 included
```

### Build / test (verified)
```bash
cd /Users/jakubgawronski/Projects/meetnotes

# Rust
( cd src-tauri && cargo check && cargo build && cargo test )

# Frontend
npx ng build                       # → dist/meetnotes/browser
```

### Run the app (NOT yet verified — needs a desktop session)
```bash
cd /Users/jakubgawronski/Projects/meetnotes
npx tauri dev                      # launches Angular dev server (:1420) + the Tauri window
```
Before a successful end-to-end run you MUST, in the app's **Settings**:
1. Set the **Vault folder** (any folder; a subfolder like `Meetings` is optional).
2. Set the **Whisper model path** to a downloaded ggml/gguf model, e.g.
   `ggml-base.en.bin`. Download once:
   ```bash
   mkdir -p "$HOME/Library/Application Support/MeetNotes/models"
   curl -L -o "$HOME/Library/Application Support/MeetNotes/models/ggml-base.en.bin" \
     https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin
   ```
   (If placed at that exact path, `transcribe::resolve_model_path` finds it even with the
   setting blank.)
3. Leave **Provider = Claude Code** (default). It requires the `claude` CLI in PATH; the
   Settings "Check providers" button shows each provider's availability. For `anthropic`,
   paste an API key (stored in Keychain). For `ollama`, have `ollama serve` running.

Then: **Record → speak → Stop**, watch the status line cycle
`recording → transcribing → summarizing → exporting → done`, and confirm a `.md` appears
in the vault and in the "Last note" preview.

---

## 5. Prioritized next steps

### Top 5 (immediate)
1. **Run the GUI E2E walking skeleton** (`tauri dev`, real mic + model + `claude`) and
   close out PHASE0-PLAN step 10 for the default ClaudeCode provider — this is the only
   remaining Phase-0 Definition-of-Done item. Capture a note end-to-end and confirm the
   `meetings`/`notes`/`segments` rows.
2. **Confirm the macOS mic-permission prompt** fires in `tauri dev`. If not, wire the
   Info.plist explicitly (Tauri 2 `bundle.macOS` / `tauri.conf.json`) or move the key so
   `dev` builds pick it up — otherwise `cpal` capture fails silently.
3. **Run the other two providers** end-to-end (Ollama with a local model; Anthropic with a
   BYO key) to validate the `availability()` verdicts and `summarize()` paths against real
   services, plus the `resummarize` command (already wired, UI button is a Phase-3 item).
4. **Install + run clippy** and a frontend lint, fix findings; consider a small CI script
   (`cargo check/test` + `ng build`) so interface drift like the `whisper-rs`/zoneless
   issues is caught automatically.
5. **Harden the recorder** thread: surface late cpal `err_cb` failures to the UI (today
   they only reset the meter + log), and add a max-duration / disk-guard. Add an
   integration test that exercises `pipeline::run_after_stop` with a mock provider + a
   tiny model or a stub transcriber.

### Phase 1 — providers (LARGELY DONE in this scaffold)
All three providers (ClaudeCode default, Anthropic, Ollama) + the trait + factory +
Settings switching + `availability()` fan-out are implemented and compile. Remaining:
live-validate each against real endpoints (covered by step 3 above).

### Phase 2 — system audio (highest risk, per DESIGN §10/§11)
ScreenCaptureKit capture (macOS 13+, no virtual device), mic+system **mixing** (ffmpeg
returns here — DESIGN §5.1), and a permissions onboarding flow (Screen Recording + Mic).
`audio/recorder.rs` already documents where a parallel `SystemAudioRecorder` slots in.
Likely needs a small Swift/ObjC shim or a `screencapturekit` crate.

### Phase 3 — product UX
Library + Detail screens (read from SQLite — the schema and `get_segments`/`latest_*`
queries already exist), template editor, Whisper model-size picker UI, and Detail-screen
buttons for re-summarize / re-export (`resummarize` command already exists). Vault
auto-detection from `obsidian.json` is implemented in `export::detect_vaults` but not yet
surfaced by a command/UI.

### Phase 4 — distribution
Apple Developer ID signing + notarization + Tauri auto-updater. Then later: hosted tier,
OpenAI/Groq/Gemini providers, diarization, live transcription, Windows (WASAPI loopback).
