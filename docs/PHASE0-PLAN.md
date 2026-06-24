# MeetNotes — Phase 0 Implementation Blueprint (Walking Skeleton)

> Status: **Plan — implementation-ready.** Date: 2026-06-24.
> Authoritative source: [`DESIGN.md`](./DESIGN.md) §10 "Faza 0". This document pins the
> exact file tree, dependency versions, module API surface, Tauri command signatures,
> and build order so that independent agents can implement modules in parallel **without
> interface drift**. No code is written here — only the contract.

## 0. Phase 0 scope (locked)

End-to-end proof: **mic-only capture → local Whisper transcription → pluggable
`SummarizerProvider` → Obsidian `.md` export**, with a minimal Tauri UI and minimal
SQLite persistence.

In scope:
- Mic-only capture via `cpal` → 16 kHz mono WAV (written with `hound`; **no ffmpeg
  dependency in Phase 0** — see §1 note).
- Local Whisper transcription via `whisper-rs` (Metal), batch mode (after Stop).
- `SummarizerProvider` trait + **all three v1 providers**: `ClaudeCodeProvider`
  (**default**), `AnthropicProvider`, `OllamaProvider`.
- Obsidian export: atomic `.tmp`→`rename` write into a user-chosen vault folder.
- Minimal Tauri UI: Record/Stop button, live status, last-note Markdown preview,
  Settings (vault path + provider selection + Anthropic key entry).
- Minimal SQLite persistence: `meetings`, `segments`, `notes`, `settings` (the full
  DESIGN §5.4 schema — implemented now, only partly surfaced in UI).
- API key in macOS Keychain via `keyring`.

Explicitly **deferred** (NOT Phase 0): ScreenCaptureKit / system audio, speaker
diarization, live transcription, Library list UI, template editor, Whisper model picker
UI (model path is a setting, downloaded manually for now), `obsidian://` URI, signing/
notarization, vault auto-detection from `obsidian.json` (manual folder pick only in P0).

---

## 1. Toolchain probe (recorded 2026-06-24 on this machine)

| Tool | Status | Version found | Implication for later agents |
|---|---|---|---|
| `node` | ✅ present | v24.16.0 | Frontend build OK. |
| `npm` | ✅ present | 11.13.0 | Frontend deps install OK. |
| `cargo` | ❌ **MISSING** | — | **Cannot compile-check Rust** until installed. |
| `rustc` | ❌ **MISSING** | — | Same. |
| Tauri CLI | ❌ MISSING | — | Install via `cargo install tauri-cli` or `npm i -D @tauri-apps/cli`. |
| `cmake` | ❌ **MISSING** | — | **Required by `whisper-rs-sys`** (builds whisper.cpp). Blocker for transcribe module compile. |
| `ffmpeg` | ✅ present | 8.1.1 | Not needed in P0 (we use `hound`); available for P2 mixing. |
| `clang` | ✅ present | Apple clang 21.0.0 | Satisfies C/C++ compiler for whisper.cpp build. |
| `pkg-config` | ❌ MISSING | — | May be needed by some `-sys` crates; install via Homebrew if a build fails. |
| Xcode CLT | ✅ present | `/Library/Developer/CommandLineTools` | Provides Metal toolchain + SDK headers. |
| OS | macOS 26.5 (Darwin 25.5.0), arm64 (Apple Silicon) | — | Metal acceleration available; target `aarch64-apple-darwin`. |

### Prerequisite-install commands (run BEFORE any Rust compile-check)
```bash
# Rust toolchain (rustup) — installs cargo + rustc
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"
rustup target add aarch64-apple-darwin

# Build deps for whisper.cpp (cmake) + pkg-config, via Homebrew
brew install cmake pkg-config

# Tauri CLI (project-local)
npm install -D @tauri-apps/cli@^2
```
**Gate for compile-checking agents:** until `cargo`, `rustc`, and `cmake` are present,
Rust modules can be authored against the signatures in §5 but **cannot be `cargo
check`-ed**. Frontend (§4) can be built/typechecked immediately (node+npm present).

> **Why no `ffmpeg` crate dependency in Phase 0:** DESIGN §5.1 specifies ffmpeg mixing
> for *mic + system* audio. Phase 0 is **mic-only mono**, so we capture directly at the
> device sample rate via `cpal`, resample to 16 kHz mono in-process, and write WAV with
> `hound`. This removes a heavy native/bundling dependency from the skeleton. ffmpeg
> returns in Phase 2 when two tracks must be mixed. `ffmpeg` binary is present on this
> machine regardless.

---

## 2. Repository file tree (exact, under `/Users/jakubgawronski/Projects/meetnotes`)

```
meetnotes/
├─ docs/
│  ├─ DESIGN.md
│  └─ PHASE0-PLAN.md                      # this file
├─ .gitignore
├─ package.json                           # frontend + tauri CLI scripts
├─ package-lock.json
├─ tsconfig.json
├─ tsconfig.app.json
├─ tsconfig.spec.json
├─ angular.json                           # Angular workspace (single app "meetnotes")
├─ index.html                             # Tauri/Angular entry (devServer + dist root)
├─ src/                                   # ── FRONTEND (Angular 18, standalone) ──
│  ├─ main.ts
│  ├─ styles.css
│  ├─ app/
│  │  ├─ app.config.ts                    # provideZonelessChangeDetection, router, http
│  │  ├─ app.routes.ts                    # /record (default), /settings
│  │  ├─ app.component.ts                 # shell: nav + <router-outlet>
│  │  ├─ core/
│  │  │  ├─ ipc.service.ts                # thin wrapper over @tauri-apps/api invoke/listen
│  │  │  ├─ models.ts                     # TS mirrors of Rust DTOs (§6)
│  │  │  └─ recorder.store.ts             # signal-based state: status, lastNote, error
│  │  └─ features/
│  │     ├─ record/
│  │     │  └─ record.component.ts        # Record/Stop, status line, last-note preview
│  │     └─ settings/
│  │        └─ settings.component.ts      # vault path, provider, anthropic key, model path
│  └─ assets/
└─ src-tauri/                             # ── RUST CORE ──
   ├─ Cargo.toml                          # §3
   ├─ Cargo.lock
   ├─ build.rs                            # tauri_build::build()
   ├─ tauri.conf.json                     # window, bundle id, devUrl, frontendDist
   ├─ capabilities/
   │  └─ default.json                     # core permissions (no fs/shell exposed to webview)
   ├─ icons/                              # placeholder app icons
   └─ src/
      ├─ main.rs                          # tauri::Builder, manage(AppState), command registry
      ├─ lib.rs                           # `pub mod` re-exports; `run()` entrypoint
      ├─ error.rs                         # AppError (thiserror) + Result alias; serde for IPC
      ├─ state.rs                         # AppState (Mutex-guarded handles), AppConfig
      ├─ commands.rs                      # ALL #[tauri::command] fns (§7)
      ├─ events.rs                        # event name constants + payload structs
      ├─ audio/
      │  ├─ mod.rs                        # pub use recorder::*, wav::*
      │  ├─ recorder.rs                   # Recorder (cpal capture → samples buffer)
      │  └─ wav.rs                        # write 16kHz mono WAV via hound + resample
      ├─ transcribe/
      │  ├─ mod.rs                        # pub use whisper::*, types::*
      │  ├─ types.rs                      # Segment, Transcript
      │  └─ whisper.rs                    # Transcriber (whisper-rs, Metal)
      ├─ summarize/
      │  ├─ mod.rs                        # trait + factory + re-exports
      │  ├─ provider.rs                   # SummarizerProvider trait, request/availability types
      │  ├─ claude_code.rs               # ClaudeCodeProvider (spawn `claude -p`)
      │  ├─ anthropic.rs                  # AnthropicProvider (reqwest → api.anthropic.com)
      │  ├─ ollama.rs                      # OllamaProvider (reqwest → localhost:11434)
      │  └─ template.rs                   # default note prompt template
      ├─ export/
      │  ├─ mod.rs                        # pub use obsidian::*
      │  └─ obsidian.rs                   # atomic .md write into vault
      ├─ storage/
      │  ├─ mod.rs                        # pub use db::*, models::*
      │  ├─ db.rs                         # Db (rusqlite Connection wrapper) + migrations
      │  └─ models.rs                     # Meeting, NoteRecord row structs + status enum
      ├─ secrets/
      │  ├─ mod.rs                        # pub use keychain::*
      │  └─ keychain.rs                   # get/set/delete api key via keyring
      ├─ settings/
      │  ├─ mod.rs                        # pub use config::*
      │  └─ config.rs                     # AppConfig load/save (settings table)
      └─ pipeline.rs                      # orchestrates capture→transcribe→summarize→export
```

---

## 3. `src-tauri/Cargo.toml` — pinned dependency list

```toml
[package]
name = "meetnotes"
version = "0.0.0"
description = "MeetNotes — local meeting capture, transcription, and AI notes for Obsidian"
edition = "2021"
rust-version = "1.77"

[lib]
name = "meetnotes_lib"
crate-type = ["staticlib", "cdylib", "rlib"]

[build-dependencies]
tauri-build = { version = "2.2", features = [] }

[dependencies]
# ── Tauri shell ──
tauri = { version = "2.11", features = ["macos-private-api"] }
tauri-plugin-dialog = "2.2"          # native folder/file picker for vault path + model file

# ── Audio capture + WAV ──
cpal = "0.16"                        # cross-platform mic capture (CoreAudio on macOS)
hound = "3.5"                        # WAV reader/writer (16-bit PCM)
rubato = "0.16"                      # sample-rate conversion → 16 kHz mono for Whisper

# ── Transcription ──
whisper-rs = { version = "0.16", features = ["metal"] }   # whisper.cpp bindings, Metal accel
                                                          # (pulls whisper-rs-sys; needs cmake+clang)

# ── HTTP (Anthropic + Ollama providers) ──
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls", "stream"] }

# ── Async runtime ──
tokio = { version = "1", features = ["rt-multi-thread", "macros", "process", "fs", "io-util", "time", "sync"] }
async-trait = "0.1"                  # for the SummarizerProvider trait

# ── Storage ──
rusqlite = { version = "0.32", features = ["bundled"] }   # bundled SQLite — no system dep

# ── Serde ──
serde = { version = "1", features = ["derive"] }
serde_json = "1"

# ── Errors ──
anyhow = "1"                         # internal error plumbing in modules
thiserror = "1"                      # typed AppError surfaced over IPC

# ── Secrets ──
keyring = { version = "3", features = ["apple-native"] }  # macOS Keychain (stay on v3, NOT v4)

# ── Misc ──
chrono = { version = "0.4", features = ["clock", "serde"] }   # timestamps / filename dates
dirs = "5"                           # locate default vault / app-data dirs
uuid = { version = "1", features = ["v4"] }                   # meeting ids
tracing = "0.1"                      # structured logging (NO PII — ids only)
tracing-subscriber = { version = "0.3", features = ["env-filter"] }

[features]
# default keeps Metal on for whisper; nothing else conditional in P0
default = []
```

**Crate-choice rationale (load-bearing):**
- `rusqlite` over `sqlx`: Phase 0 is synchronous, single-process, embedded; `bundled`
  feature compiles SQLite in (no system lib, no `cmake` for SQLite, no async runtime
  coupling). DESIGN allows "rusqlite or sqlx" — we pin **rusqlite**.
- `hound` + `rubato` instead of ffmpeg: pure-Rust WAV + resample; avoids bundling ffmpeg
  for a mic-only mono skeleton (see §1 note).
- `keyring` **v3** (not v4): v4 split into `keyring-core` + separate store crates with a
  different API; v3 with `apple-native` is the stable, documented macOS path.
- `reqwest` with `rustls-tls` (not native-tls): avoids OpenSSL system dependency.
- `tokio` `process` feature: required to spawn `claude -p` for `ClaudeCodeProvider`.

---

## 4. Frontend stack decision

**Chosen: Angular 18 (standalone components, zoneless change detection) + Vite-style
Angular CLI dev server, talking to the core via `@tauri-apps/api`.**

Justification (brief):
- The team knows **TypeScript/Angular** (explicit constraint) — zero ramp-up, and the
  house Angular conventions (signals-first, standalone, `inject()`, `@if`/`@for`) carry
  over directly. Phase 0 has 2 screens; Angular's overhead is negligible at this size and
  pays off as the UI grows (Library/Detail/Onboarding in later phases).
- Tauri is frontend-agnostic; it serves a static `dist/` and exposes `invoke`/`listen`.
  Angular CLI's dev server (HMR) is used as Tauri `devUrl` in dev, and `ng build` output
  (`dist/meetnotes/browser`) is the `frontendDist` for the bundle. No SSR.
- **Zoneless** (`provideZonelessChangeDetection`) keeps it light and signal-driven, which
  matches the event/`listen`-based status updates from the Rust core cleanly via signals.

### `package.json` (frontend + Tauri CLI) — pinned npm deps
```jsonc
{
  "name": "meetnotes",
  "version": "0.0.0",
  "scripts": {
    "start": "ng serve --port 1420",          // Tauri devUrl
    "build": "ng build",                       // → dist/meetnotes/browser
    "tauri": "tauri",
    "dev": "tauri dev",
    "bundle": "tauri build"
  },
  "dependencies": {
    "@angular/animations": "^18.2.0",
    "@angular/common": "^18.2.0",
    "@angular/compiler": "^18.2.0",
    "@angular/core": "^18.2.0",
    "@angular/forms": "^18.2.0",
    "@angular/platform-browser": "^18.2.0",
    "@angular/platform-browser-dynamic": "^18.2.0",
    "@angular/router": "^18.2.0",
    "rxjs": "^7.8.0",
    "tslib": "^2.6.0",
    "zone.js": "^0.15.0",                      // present but zoneless CD used; kept for CLI compat
    "@tauri-apps/api": "^2.0.0",
    "@tauri-apps/plugin-dialog": "^2.0.0"
  },
  "devDependencies": {
    "@angular/cli": "^18.2.0",
    "@angular/compiler-cli": "^18.2.0",
    "@angular-devkit/build-angular": "^18.2.0",
    "@tauri-apps/cli": "^2.0.0",
    "typescript": "~5.5.0"
  }
}
```
> Angular 18 (not 21) is pinned for Phase 0 because it is the proven-stable combination
> with Tauri 2's static-serve model and the team's existing tooling; upgrading is a
> mechanical later step. Conventions (signals, standalone, `@if`/`@for`) are identical.

`tauri.conf.json` key fields:
- `build.devUrl = "http://localhost:1420"`, `build.beforeDevCommand = "npm run start"`
- `build.frontendDist = "../dist/meetnotes/browser"`, `build.beforeBuildCommand = "npm run build"`
- `app.windows[0] = { title: "MeetNotes", width: 900, height: 680 }`
- `identifier = "com.meetnotes.app"`

---

## 5. Rust module public API (EXACT signatures — the interface contract)

> These signatures are **binding**. Each module is implementable in isolation against
> them. Cross-module calls use only what is listed here. Internal helpers are free.
> `Result<T>` below means `crate::error::Result<T>` unless qualified.

### 5.1 `error.rs`
```rust
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("audio capture error: {0}")]
    Audio(String),
    #[error("transcription error: {0}")]
    Transcribe(String),
    #[error("summarizer error: {0}")]
    Summarize(String),
    #[error("export error: {0}")]
    Export(String),
    #[error("storage error: {0}")]
    Storage(String),
    #[error("secrets error: {0}")]
    Secrets(String),
    #[error("config error: {0}")]
    Config(String),
    #[error("provider unavailable: {0}")]
    Unavailable(String),
    #[error("invalid argument: {0}")]
    InvalidArg(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, AppError>;

// IPC: AppError serializes to a string message so Tauri commands can return it.
impl Serialize for AppError {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error>;
}
```

### 5.2 `state.rs`
```rust
use std::sync::Mutex;
use crate::audio::Recorder;
use crate::storage::Db;
use crate::settings::AppConfig;

pub struct AppState {
    pub recorder: Mutex<Option<Recorder>>,   // Some while recording
    pub db: Db,                               // Db is internally Send+Sync (Mutex<Connection>)
    pub config: Mutex<AppConfig>,             // in-memory cache of settings table
    pub current_meeting: Mutex<Option<uuid::Uuid>>,
}

impl AppState {
    /// Open DB at app-data dir, run migrations, load config. Called once in main.rs.
    pub fn init() -> crate::error::Result<Self>;
}
```

### 5.3 `events.rs`
```rust
use serde::Serialize;

pub const EVENT_STATUS: &str = "meetnotes://status";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusPayload {
    pub stage: String,        // "idle" | "recording" | "transcribing" | "summarizing" | "exporting" | "done" | "error"
    pub message: String,      // human-readable, NO PII
    pub meeting_id: Option<String>,
}
```

### 5.4 `audio/recorder.rs`
```rust
use crate::error::Result;

/// Owns the cpal input stream and accumulates mono f32 samples at the device rate.
pub struct Recorder {
    // private: stream handle, shared sample buffer, source_sample_rate
}

impl Recorder {
    /// Open default input device, start capturing. Non-blocking; capture runs on cpal thread.
    pub fn start() -> Result<Self>;

    /// Stop the stream, return (mono_samples_at_source_rate, source_sample_rate_hz).
    pub fn stop(self) -> Result<(Vec<f32>, u32)>;

    /// Current peak level 0.0..=1.0 for the UI meter (best-effort, lock-free read).
    pub fn level(&self) -> f32;
}
```

### 5.5 `audio/wav.rs`
```rust
use std::path::Path;
use crate::error::Result;

pub const TARGET_RATE_HZ: u32 = 16_000;

/// Resample mono f32 @ src_rate to 16 kHz mono and write 16-bit PCM WAV to `path`.
pub fn write_wav_16k_mono(path: &Path, samples: &[f32], src_rate: u32) -> Result<()>;

/// Resample mono f32 @ src_rate to 16 kHz mono f32 (in-memory, for Whisper input).
pub fn resample_to_16k(samples: &[f32], src_rate: u32) -> Result<Vec<f32>>;
```

### 5.6 `transcribe/types.rs`
```rust
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Segment {
    pub idx: i64,
    pub start_s: f64,
    pub end_s: f64,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transcript {
    pub full_text: String,
    pub segments: Vec<Segment>,
    pub language: Option<String>,
}
```

### 5.7 `transcribe/whisper.rs`
```rust
use std::path::Path;
use crate::error::Result;
use crate::transcribe::types::Transcript;

/// Wraps a loaded whisper.cpp model (Metal). Construct once; reuse per transcription.
pub struct Transcriber {
    // private: WhisperContext
}

impl Transcriber {
    /// Load a GGUF model from `model_path`. Errors if file missing or load fails.
    pub fn load(model_path: &Path) -> Result<Self>;

    /// Transcribe 16 kHz mono f32 samples. `lang` = Some("en") or None for auto.
    pub fn transcribe(&self, samples_16k_mono: &[f32], lang: Option<&str>) -> Result<Transcript>;
}
```

### 5.8 `summarize/provider.rs`
```rust
use async_trait::async_trait;
use serde::{Serialize, Deserialize};
use crate::error::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingMeta {
    pub date_iso: String,          // "2026-06-24"
    pub title_hint: Option<String>,
    pub duration_s: i64,
    pub language: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SummarizeRequest {
    pub transcript: String,
    pub meta: MeetingMeta,
    pub template: String,          // note-format prompt (summarize/template.rs)
    pub vault_titles: Vec<String>, // existing note titles → [[link]] targets
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Availability {
    Available,
    Unavailable { reason: String },
}

#[async_trait]
pub trait SummarizerProvider: Send + Sync {
    /// Stable id: "claude_code" | "anthropic" | "ollama".
    fn id(&self) -> &str;

    /// Cheap, non-failing readiness probe (key set? ollama up? claude in PATH?).
    async fn availability(&self) -> Availability;

    /// Produce finished Obsidian-ready Markdown from the request.
    async fn summarize(&self, req: &SummarizeRequest) -> Result<String>;
}
```

### 5.9 `summarize/mod.rs` (factory)
```rust
use std::sync::Arc;
use crate::summarize::provider::SummarizerProvider;
use crate::settings::AppConfig;

pub mod provider;
pub mod claude_code;
pub mod anthropic;
pub mod ollama;
pub mod template;

pub use provider::{SummarizerProvider as _, MeetingMeta, SummarizeRequest, Availability};

/// Default provider id when settings unset.
pub const DEFAULT_PROVIDER_ID: &str = "claude_code";

/// Build a provider by id, wiring config + secrets. Unknown id → AppError::InvalidArg.
pub fn make_provider(id: &str, config: &AppConfig) -> crate::error::Result<Arc<dyn SummarizerProvider>>;

/// All three provider instances (for availability fan-out in Settings UI).
pub fn all_providers(config: &AppConfig) -> Vec<Arc<dyn SummarizerProvider>>;
```

### 5.10 `summarize/claude_code.rs`
```rust
use crate::summarize::provider::*;

pub struct ClaudeCodeProvider {
    // private: binary path (default "claude"), system prompt
}

impl ClaudeCodeProvider {
    pub fn new() -> Self;
    pub fn with_binary(path: String) -> Self;
}

#[async_trait::async_trait]
impl SummarizerProvider for ClaudeCodeProvider {
    fn id(&self) -> &str;                                   // "claude_code"
    async fn availability(&self) -> Availability;          // `which claude` reachable?
    async fn summarize(&self, req: &SummarizeRequest) -> crate::error::Result<String>;
    // spawns: claude -p --system-prompt <tpl> --disallowedTools <all> ; stdin = transcript;
    // validates stdout starts with "---" (front-matter) else AppError::Summarize.
}
```

### 5.11 `summarize/anthropic.rs`
```rust
use crate::summarize::provider::*;

pub struct AnthropicProvider {
    // private: reqwest::Client, model id, api_key (loaded from Keychain at construction)
}

impl AnthropicProvider {
    /// `api_key` already resolved from Keychain by the factory. model defaults to
    /// "claude-opus-4-8" if config value empty.
    pub fn new(api_key: Option<String>, model: String) -> Self;
}

#[async_trait::async_trait]
impl SummarizerProvider for AnthropicProvider {
    fn id(&self) -> &str;                                   // "anthropic"
    async fn availability(&self) -> Availability;          // key present? (no network call)
    async fn summarize(&self, req: &SummarizeRequest) -> crate::error::Result<String>;
    // POST https://api.anthropic.com/v1/messages, headers x-api-key + anthropic-version
    // "2023-06-01"; system=template, user=transcript+meta; returns content[0].text.
}
```

### 5.12 `summarize/ollama.rs`
```rust
use crate::summarize::provider::*;

pub struct OllamaProvider {
    // private: reqwest::Client, base_url (default http://localhost:11434), model
}

impl OllamaProvider {
    pub fn new(base_url: String, model: String) -> Self;   // model e.g. "llama3.1"
}

#[async_trait::async_trait]
impl SummarizerProvider for OllamaProvider {
    fn id(&self) -> &str;                                   // "ollama"
    async fn availability(&self) -> Availability;          // GET {base}/api/tags reachable?
    async fn summarize(&self, req: &SummarizeRequest) -> crate::error::Result<String>;
    // POST {base}/api/generate { model, prompt:(template+transcript), stream:false }
}
```

### 5.13 `summarize/template.rs`
```rust
use crate::summarize::provider::SummarizeRequest;

/// The canonical Obsidian note-format prompt (front-matter + sections), shared by all providers.
pub fn default_template() -> String;

/// Render the full prompt text a provider sends (template + meta + vault titles + transcript).
pub fn render_prompt(req: &SummarizeRequest) -> String;
```

### 5.14 `export/obsidian.rs`
```rust
use std::path::{Path, PathBuf};
use crate::error::Result;

/// Atomically write `markdown` into `vault_dir` (optionally `subfolder`) as a uniquely
/// named .md file derived from `title` + `date_iso`. Writes to a dotfile `.tmp` then
/// renames. On name collision appends " (N)". Returns the final path written.
pub fn write_note(
    vault_dir: &Path,
    subfolder: Option<&str>,
    title: &str,
    date_iso: &str,
    markdown: &str,
) -> Result<PathBuf>;

/// List existing note titles (file stems of *.md) in the vault for [[link]] suggestions.
pub fn list_vault_titles(vault_dir: &Path) -> Result<Vec<String>>;
```

### 5.15 `storage/models.rs`
```rust
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MeetingStatus { Draft, Recording, Transcribed, Summarized, Exported, Error }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Meeting {
    pub id: String,                 // uuid
    pub started_at: String,         // ISO 8601
    pub ended_at: Option<String>,
    pub title: Option<String>,
    pub duration_s: i64,
    pub audio_path: Option<String>,
    pub status: MeetingStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteRecord {
    pub meeting_id: String,
    pub provider_id: String,
    pub markdown: String,
    pub created_at: String,
    pub exported_path: Option<String>,
}
```

### 5.16 `storage/db.rs`
```rust
use std::path::Path;
use crate::error::Result;
use crate::storage::models::{Meeting, MeetingStatus, NoteRecord};
use crate::transcribe::types::Segment;

/// Thread-safe SQLite wrapper (internal Mutex<rusqlite::Connection>).
pub struct Db { /* private */ }

impl Db {
    pub fn open(path: &Path) -> Result<Self>;     // opens + runs migrations
    pub fn migrate(&self) -> Result<()>;          // idempotent CREATE TABLE IF NOT EXISTS

    pub fn insert_meeting(&self, m: &Meeting) -> Result<()>;
    pub fn update_meeting_status(&self, id: &str, status: MeetingStatus) -> Result<()>;
    pub fn finalize_meeting(&self, id: &str, ended_at: &str, duration_s: i64, audio_path: &str) -> Result<()>;
    pub fn set_meeting_title(&self, id: &str, title: &str) -> Result<()>;
    pub fn get_meeting(&self, id: &str) -> Result<Option<Meeting>>;
    pub fn latest_meeting(&self) -> Result<Option<Meeting>>;

    pub fn insert_segments(&self, meeting_id: &str, segments: &[Segment]) -> Result<()>;

    pub fn upsert_note(&self, note: &NoteRecord) -> Result<()>;
    pub fn get_note(&self, meeting_id: &str, provider_id: &str) -> Result<Option<NoteRecord>>;
    pub fn latest_note(&self) -> Result<Option<NoteRecord>>;
    pub fn set_note_exported_path(&self, meeting_id: &str, provider_id: &str, path: &str) -> Result<()>;

    // settings k/v table
    pub fn get_setting(&self, key: &str) -> Result<Option<String>>;
    pub fn set_setting(&self, key: &str, value: &str) -> Result<()>;
    pub fn all_settings(&self) -> Result<Vec<(String, String)>>;
}
```
SQL migrations (created by `migrate()`):
```sql
CREATE TABLE IF NOT EXISTS meetings (
  id TEXT PRIMARY KEY, started_at TEXT NOT NULL, ended_at TEXT,
  title TEXT, duration_s INTEGER NOT NULL DEFAULT 0,
  audio_path TEXT, status TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS segments (
  meeting_id TEXT NOT NULL, idx INTEGER NOT NULL,
  start_s REAL NOT NULL, end_s REAL NOT NULL, text TEXT NOT NULL,
  PRIMARY KEY (meeting_id, idx),
  FOREIGN KEY (meeting_id) REFERENCES meetings(id) ON DELETE CASCADE);
CREATE TABLE IF NOT EXISTS notes (
  meeting_id TEXT NOT NULL, provider_id TEXT NOT NULL,
  markdown TEXT NOT NULL, created_at TEXT NOT NULL, exported_path TEXT,
  PRIMARY KEY (meeting_id, provider_id),
  FOREIGN KEY (meeting_id) REFERENCES meetings(id) ON DELETE CASCADE);
CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
```

### 5.17 `secrets/keychain.rs`
```rust
use crate::error::Result;

pub const SERVICE: &str = "com.meetnotes.app";

/// account is the provider key name, e.g. "anthropic_api_key".
pub fn set_secret(account: &str, secret: &str) -> Result<()>;
pub fn get_secret(account: &str) -> Result<Option<String>>;   // None if not found
pub fn delete_secret(account: &str) -> Result<()>;             // Ok if already absent
```

### 5.18 `settings/config.rs`
```rust
use serde::{Serialize, Deserialize};
use crate::error::Result;
use crate::storage::Db;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    pub provider_id: String,           // default "claude_code"
    pub vault_path: Option<String>,
    pub vault_subfolder: Option<String>,
    pub whisper_model_path: Option<String>,
    pub language: Option<String>,      // "en" | None=auto
    pub anthropic_model: String,       // default "claude-opus-4-8"
    pub ollama_base_url: String,       // default "http://localhost:11434"
    pub ollama_model: String,          // default "llama3.1"
    pub claude_binary: String,         // default "claude"
}

impl Default for AppConfig {
    fn default() -> Self; // fills the defaults above
}

impl AppConfig {
    /// Read all known keys from the settings table, falling back to Default.
    pub fn load(db: &Db) -> Result<Self>;
    /// Persist every field into the settings table (NOT the api key — that's Keychain).
    pub fn save(&self, db: &Db) -> Result<()>;
}
```

### 5.19 `pipeline.rs` (orchestration)
```rust
use std::path::PathBuf;
use tauri::AppHandle;
use crate::error::Result;
use crate::state::AppState;

/// Full post-Stop pipeline: write WAV → transcribe → persist segments → summarize with
/// configured provider → persist note → export .md → update meeting status. Emits
/// StatusPayload on EVENT_STATUS at each stage. Returns the exported note path + markdown.
pub struct PipelineResult { pub note_markdown: String, pub exported_path: PathBuf, pub meeting_id: String }

pub async fn run_after_stop(
    app: &AppHandle,
    state: &AppState,
    meeting_id: &str,
    samples: Vec<f32>,
    src_rate: u32,
    duration_s: i64,
) -> Result<PipelineResult>;
```

### 5.20 `lib.rs` / `main.rs`
```rust
// lib.rs
pub mod error; pub mod state; pub mod events; pub mod commands; pub mod pipeline;
pub mod audio; pub mod transcribe; pub mod summarize; pub mod export;
pub mod storage; pub mod secrets; pub mod settings;
pub fn run();   // builds tauri::Builder, manages AppState, registers commands, runs app

// main.rs
fn main() { meetnotes_lib::run(); }
```

---

## 6. Frontend DTO mirrors (`src/app/core/models.ts`)

TypeScript interfaces mirroring the `camelCase`-serialized Rust types so the UI compiles
against the same contract:
```ts
export type Stage = 'idle'|'recording'|'transcribing'|'summarizing'|'exporting'|'done'|'error';
export interface StatusPayload { stage: Stage; message: string; meetingId: string|null; }
export type Availability = { Available: true } | { Unavailable: { reason: string } };
export interface ProviderStatus { id: string; available: boolean; reason?: string; }
export interface AppConfigDto {
  providerId: string; vaultPath: string|null; vaultSubfolder: string|null;
  whisperModelPath: string|null; language: string|null; anthropicModel: string;
  ollamaBaseUrl: string; ollamaModel: string; claudeBinary: string;
}
export interface NoteDto { meetingId: string; providerId: string; markdown: string; exportedPath: string|null; }
export interface StartResult { meetingId: string; }
export interface StopResult { meetingId: string; markdown: string; exportedPath: string; }
```

---

## 7. Tauri command functions (`commands.rs`) — exact signatures

All are `#[tauri::command]`, registered in `main.rs` `invoke_handler`. State injected via
`tauri::State<'_, AppState>` and `AppHandle`. Errors return `AppError` (serialized to a
string message; frontend `invoke` rejects).

```rust
/// Begin mic capture. Inserts a Meeting(Draft→Recording), stores Recorder in state,
/// sets current_meeting. Returns the new meeting id. Errors if already recording.
#[tauri::command]
async fn start_recording(app: AppHandle, state: State<'_, AppState>) -> Result<StartResult, AppError>;

/// Stop capture, then run the full pipeline (pipeline::run_after_stop). Returns the
/// exported note path + markdown. Emits status events throughout. Errors if not recording.
#[tauri::command]
async fn stop_recording(app: AppHandle, state: State<'_, AppState>) -> Result<StopResult, AppError>;

/// Current mic peak level 0.0..=1.0 for the meter (0.0 when idle). Cheap, polled by UI.
#[tauri::command]
fn recording_level(state: State<'_, AppState>) -> Result<f32, AppError>;

/// The most recent note (markdown + export path) for the last-note preview pane.
#[tauri::command]
fn get_last_note(state: State<'_, AppState>) -> Result<Option<NoteDto>, AppError>;

/// Read current config (settings table), without secrets.
#[tauri::command]
fn get_config(state: State<'_, AppState>) -> Result<AppConfigDto, AppError>;

/// Persist config to settings table + refresh in-memory cache. Does NOT touch Keychain.
#[tauri::command]
fn save_config(state: State<'_, AppState>, config: AppConfigDto) -> Result<(), AppError>;

/// Store/replace the Anthropic API key in Keychain (account "anthropic_api_key").
#[tauri::command]
fn set_anthropic_key(key: String) -> Result<(), AppError>;

/// Whether an Anthropic key is currently stored (UI shows "set"/"not set"; never the value).
#[tauri::command]
fn has_anthropic_key() -> Result<bool, AppError>;

/// availability() fan-out across all three providers for the Settings UI.
#[tauri::command]
async fn provider_statuses(state: State<'_, AppState>) -> Result<Vec<ProviderStatus>, AppError>;

/// Re-run summarize+export for an existing meeting with the configured provider (Detail
/// "re-summarize"/"re-export" seed — minimal use in P0, wired but UI optional).
#[tauri::command]
async fn resummarize(app: AppHandle, state: State<'_, AppState>, meeting_id: String) -> Result<StopResult, AppError>;
```

Frontend `IpcService` exposes one method per command plus `onStatus(cb)` wrapping
`listen<StatusPayload>(EVENT_STATUS, cb)`.

---

## 8. Build sequence (dependency-ordered; each step independently checkable)

> Steps 0–1 are prerequisites. Rust steps (3+) require `cargo`+`cmake` present (§1).
> Modules within a step have no inter-dependencies and can be authored in parallel.

0. **Toolchain**: install rustup/cargo, `cmake`, `pkg-config`, Tauri CLI (§1 commands).
   Verify `cargo --version`, `cmake --version`.
1. **Scaffold**: `npm create tauri-app` equivalent — create Angular workspace (`src/`) +
   `src-tauri/` skeleton; commit `Cargo.toml` (§3) and `package.json` (§4).
   Checkpoint: `npm run build` (frontend) succeeds; `cargo check` on an empty `lib.rs`.
2. **Foundations (parallel, no deps)**: `error.rs`, `events.rs`, `storage/models.rs`,
   `transcribe/types.rs`, `summarize/provider.rs` (trait + DTOs), `summarize/template.rs`.
   Checkpoint: `cargo check` — types compile in isolation.
3. **Storage + secrets + settings (parallel; depend on step 2)**: `storage/db.rs`
   (migrations + CRUD), `secrets/keychain.rs`, `settings/config.rs`.
   Checkpoint: a `cargo test` for `Db::open`+`migrate`+round-trip a setting.
4. **Audio (depends on error.rs)**: `audio/recorder.rs` (cpal), `audio/wav.rs`
   (hound+rubato). Checkpoint: unit test resamples a sine to 16 kHz; manual mic capture
   writes a playable WAV.
5. **Transcribe (depends on types + a GGUF model on disk)**: `transcribe/whisper.rs`.
   Checkpoint: transcribe a known 16 kHz WAV → non-empty `Transcript`. **Needs cmake.**
6. **Providers (parallel; depend on provider.rs + secrets + settings)**:
   `claude_code.rs`, `anthropic.rs`, `ollama.rs`, then `summarize/mod.rs` factory.
   Checkpoint: `availability()` returns the right verdict for each in dev env;
   `ClaudeCodeProvider::summarize` produces front-matter markdown from a sample transcript.
7. **Export (depends on error.rs)**: `export/obsidian.rs` atomic write + title listing.
   Checkpoint: writes `.md` atomically into a temp vault; collision → ` (1)` suffix.
8. **Orchestration**: `state.rs` (AppState::init), `pipeline.rs` (wires 3–7),
   `commands.rs` (§7), `lib.rs`/`main.rs` registration + `tauri.conf.json`.
   Checkpoint: `cargo check` whole crate; `tauri dev` launches the window.
9. **Frontend wiring**: `models.ts`, `ipc.service.ts`, `recorder.store.ts`,
   `record.component.ts`, `settings.component.ts`, routes/config.
   Checkpoint: `npm run build` + `tauri dev` — UI invokes commands, status updates render.
10. **E2E walking-skeleton verification (manual)**: launch app → set vault path + Whisper
    model path → pick `claude_code` → Record → speak → Stop → observe status stages →
    `.md` appears in vault → preview pane shows markdown → row in `meetings`/`notes`.
    Repeat once each with `ollama` and `anthropic` selected.

**Definition of Done (Phase 0):** step 10 passes for the default `ClaudeCode` provider
end-to-end on this machine, with all three providers compiling and selectable, lint/build
green, and a note file present in the vault.

---

## 9. Open risks carried into Phase 0
- `whisper-rs-sys` build **requires `cmake`** (currently missing) — install before step 5.
- GGUF model is **manually placed** in P0 (path is a setting); auto-download is Phase 3.
- `ClaudeCodeProvider` depends on `claude` being in PATH; `availability()` must degrade
  gracefully and the UI must surface "not available" without crashing the pipeline.
- Mic permission prompt (macOS `NSMicrophoneUsageDescription`) must be in `Info.plist`
  via `tauri.conf.json` bundle config — without it `cpal` capture fails silently.
