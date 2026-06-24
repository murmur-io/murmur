# MeetNotes — Status

> Updated 2026-06-24. Authoritative current state. Honest by construction: every
> "verified" claim below is backed by `bash scripts/ci.sh` passing on this machine.

## TL;DR
- **Implemented + verified (CI-green):** Phase 0 (skeleton), Phase 1 (3 AI providers,
  SQLite, Keychain, Obsidian export), Phase 3 (Library + Detail UI), plus the headless
  core pipeline E2E and the CI gate.
- **One command proves it:** `bash scripts/ci.sh` → clippy (`-D warnings`) + cargo test
  (31) + cargo build + `ng lint` + `ng build` + headless E2E, all green.
- **Remaining for prod-ready (all require YOU / a real Mac — cannot be verified in a
  headless build):** Phase 2 system-audio **runtime** verification (code is implemented +
  compiles + typechecks; capturing live audio needs a desktop + the Screen Recording
  permission), Apple code-signing + notarization, and a real GUI + microphone run.

## Implemented & verified
| Area | What | Verified by |
|---|---|---|
| Skeleton | Tauri (Rust core) + Angular (zoneless, signals) frontend | `cargo build`, `ng build` |
| Capture | Mic via cpal → 16 kHz mono WAV; **system audio via ScreenCaptureKit Swift sidecar (opt-in)** | unit tests + swiftc typecheck (capture runtime: user-gated) |
| Transcription | whisper.cpp (`whisper-rs`, Metal) | **headless E2E** (real audio→text) |
| AI providers | `SummarizerProvider` trait + ClaudeCode (default) / Anthropic / Ollama | unit tests + **E2E** (ClaudeCode) |
| Storage | SQLite (meetings/segments/notes/settings) | unit tests |
| Secrets | Anthropic key in macOS Keychain | unit-level |
| Export | Atomic `.md` write into the vault + vault auto-detect | unit tests + **E2E** |
| Library/Detail | list meetings, view note + transcript, re-summarize | `ng build` + `ng lint` |
| Quality | clippy `-D warnings`, `ng lint`, 31 tests | `scripts/ci.sh` |

The headless E2E (`scripts/e2e-core.sh`) drives `say → ffmpeg → Whisper(base.en) →
ClaudeCodeProvider → Obsidian .md` and asserts a real note with front-matter — the core
pipeline is proven end-to-end, minus the parts that need a desktop (below).

## Remaining for prod-ready (user / runtime gated)
1. **Phase 2 — system-audio capture** (the other side of a call). **Implemented:** a
   Swift ScreenCaptureKit sidecar (`src-tauri/sysaudio/sysaudio.swift`, compiled by
   `build.rs`, typechecked in CI) + `audio::system::SystemAudioRecorder` + a unit-tested
   mic/system `mixer`, opt-in via the Settings "Capture system audio" toggle (default
   off). The compile, typecheck, mixer tests, and the graceful no-permission exit are
   verified. **Unverified (needs a real Mac):** capturing *live* system audio — that
   requires a desktop session + the Screen Recording (TCC) permission + real audio.
   Enable the toggle and confirm a mixed recording. Design notes: `docs/PHASE2-SYSTEM-AUDIO.md`.
2. **Apple code-sign + notarize.** Template in `scripts/macos-sign-notarize.sh`; needs a
   paid Apple Developer ID + a notarytool credential.
3. **Real GUI + mic run** (closes the last Phase-0 DoD item). See §Run below.

## Toolchain (installed in this environment)
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"
brew install cmake pkg-config           # whisper.cpp build deps
cd /Users/jakubgawronski/Projects/meetnotes && npm install
```

## Verify (what's green here)
```bash
bash scripts/ci.sh          # the full gate
bash scripts/e2e-core.sh    # just the headless core pipeline
```

## Run the app (needs a desktop session — NOT verified headless)
```bash
npx tauri dev               # Angular dev server (:1420) + Tauri window
```
Then in **Settings**: set the Vault folder; set the Whisper model path (download once):
```bash
mkdir -p "$HOME/Library/Application Support/MeetNotes/models"
curl -L -o "$HOME/Library/Application Support/MeetNotes/models/ggml-base.en.bin" \
  https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin
```
Leave Provider = Claude Code (needs the `claude` CLI in PATH), or set an Anthropic key /
run Ollama. Then **Record → speak → Stop** and confirm a note appears in the vault.

## Phase roadmap
- **0/1/3 + hygiene** — done (this session).
- **2** — system audio: **implemented + compile-verified**; live-capture runtime is user-gated.
- **4** — signing/notarization (template ready), Tauri auto-updater (needs a signing
  keypair + release endpoint), then later: hosted provider tier, OpenAI/Groq/Gemini,
  diarization, live transcription, Windows (WASAPI loopback).
