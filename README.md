# MeetNotes

Local meeting capture → on-device Whisper transcription → pluggable AI summarizer →
Obsidian Markdown export. Tauri 2 (Rust core) + Angular 18 (zoneless, standalone) frontend.

This repository is the **Phase 0 walking skeleton**. Module bodies are stubs
(`todo!()`); the structure, dependency set, IPC contract, and command registry are final.
See [`docs/PHASE0-PLAN.md`](docs/PHASE0-PLAN.md) for the binding interface contract and
[`docs/DESIGN.md`](docs/DESIGN.md) for the overall design.

## Layout

```
meetnotes/
├─ src/          Angular 18 frontend (standalone, zoneless)
├─ src-tauri/    Rust core (Tauri 2): audio, transcribe, summarize, export, storage
└─ docs/         DESIGN.md, PHASE0-PLAN.md
```

## Prerequisites

| Tool | Why | Install |
|---|---|---|
| Node 22+ / npm | Frontend build + Tauri CLI | (present) |
| Rust toolchain (cargo, rustc) | Rust core | `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh -s -- -y` then `source "$HOME/.cargo/env"` |
| `cmake` | builds whisper.cpp via `whisper-rs-sys` | `brew install cmake pkg-config` |
| clang / Xcode CLT | C/C++ compiler + Metal toolchain | `xcode-select --install` |
| Whisper GGUF model | local transcription | download manually; set its path in Settings |

```bash
rustup target add aarch64-apple-darwin
```

## Install

```bash
npm install                 # frontend deps + @tauri-apps/cli
```

## Develop

```bash
npm run dev                 # tauri dev: launches Angular dev server (:1420) + native window
# or, frontend only:
npm run start               # ng serve on http://localhost:1420
```

## Build

```bash
npm run build               # ng build → dist/meetnotes/browser
npm run bundle              # tauri build → native .app bundle
```

## Compile-check the Rust core

```bash
cd src-tauri && cargo check
```

> Requires `cargo` + `cmake` (see Prerequisites). On a machine without the Rust
> toolchain installed, the Rust core cannot be compiled; install rustup first.

## Phase 0 end-to-end flow (once modules are implemented)

Launch the app → Settings: pick vault folder + Whisper model path + provider
(`claude_code` default) → Record → speak → Stop → watch status stages
(transcribing → summarizing → exporting) → a `.md` note appears in the vault and in the
last-note preview pane.

## Providers

- **claude_code** (default): spawns the `claude` CLI (`-p`); requires `claude` in PATH.
- **anthropic**: Anthropic Messages API; needs an API key stored via Settings (macOS Keychain).
- **ollama**: local Ollama at `http://localhost:11434`.
