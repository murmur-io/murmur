# CLAUDE.md — Murmur

Guidance for Claude Code when working in this repository. These instructions are **binding** and override default behavior.

## ⚠️ MANDATORY READING BEFORE ANY CODE

Before touching `src-tauri/` (Rust) or `src/app/` (Angular), read the binding rules — every forbidden pattern in them is genuinely forbidden, no exceptions without explicit user approval:

- [`.claude/rules/rust-tauri.md`](.claude/rules/rust-tauri.md) — Rust/Tauri backend: errors, commands, SQLCipher, additive migrations, **verify-before-destroy**, **gate every content read**, **crash-safe macOS FFI**, `cargo test --lib` only.
- [`.claude/rules/angular-zoneless.md`](.claude/rules/angular-zoneless.md) — Angular 18 **zoneless** FE: signals-first, standalone/OnPush, `@if/@for`, IPC→signals (no NgRx, no subscribe-for-state), and the three Murmur traps (NG0600, recursive-component `forwardRef` cycle, opaque overlays).
- [`.claude/rules/lock-model.md`](.claude/rules/lock-model.md) — the per-folder encryption/lock **invariants** (gate every read; verify-before-destroy every seal; the `convertFileSrc` asset-path leak trap).
- [`.claude/rules/agentic-workflow.md`](.claude/rules/agentic-workflow.md) — how to use the Workflow tool + the **adversarial-verify** discipline that has caught every real bug here.

## What Murmur is

A **local-first macOS desktop app** that records meetings, transcribes on-device, turns the transcript into a clean note via a pluggable LLM provider, and lives inside the user's **Obsidian vault**. Currently shipped at **v0.3.0**.

- **Stack:** Tauri 2.11 (Rust crate `murmur`, lib `meetnotes_lib`, bin `Murmur`) + Angular 18 **zoneless** (standalone, signals). IPC = Tauri commands (registered in `src-tauri/src/lib.rs` `generate_handler!`) + events. The FE talks to the backend through `src/app/core/ipc.service.ts` — **there is no NgRx**.
- **Pipeline:** capture (mic via `cpal` + system audio via a Swift **ScreenCaptureKit** sidecar) → **dual-stream** (transcribed separately, merged by wall-clock → `Me`/`Others`) → **whisper.cpp** (`whisper-rs`, Metal; default model **large-v3**) → segments → **SQLite (canonical source of truth, SQLCipher-encrypted)** → `SummarizerProvider` → note markdown → atomic **Obsidian `.md`** export.
- **Providers (one trait, swappable):** `claude_code` (default), `anthropic` (BYO key in Keychain), `ollama` (local). Cloud-bound text passes the **redaction firewall**.
- **Three consumption surfaces over one store:** the app UI, a local read-only **MCP server** (`127.0.0.1:8765`), and the Obsidian vault.

### Module map (verify against the tree — trust code, not docs)

**Rust** (`src-tauri/src/`): `commands.rs` (Tauri commands), `lib.rs` (handler registry + setup), `state.rs` (`AppState`), `error.rs` (`AppError`/`Result`), `events.rs`; `storage/` (`db.rs`, `migration.rs` = SQLCipher whole-DB encrypt-in-place, `models.rs`); `crypto.rs` (AES-256-GCM + `encrypt_file`/`decrypt_file`, verify-before-destroy); `secrets/keychain.rs` (keyring, service `com.meetnotes.app`, `MURMUR_DEV_DEK`/`MURMUR_DEV_KEK` debug hatches); `biometric.rs` (Touch ID); `screenshare.rs` (best-effort auto-relock, crash-safe `CGWindowList`); `audio/` (`recorder`, `system`, `mixer`, `merge`, `wav`, `listener`); `transcribe/` (`whisper`, `model`, `live`, `types`); `pipeline.rs`; `mcp.rs`; `summarize/`; `settings/config.rs`; `export/`.

**Angular** (`src/app/features/`): `analytics`, `ask`, `bar`, `detail`, `folders`, `graph`, `library`, `onboarding`, `record`, `settings`. Services: `core/ipc.service.ts`, `services/{folders,toast,screen-share}.service.ts`, `core/models.ts`.

## Common commands

```bash
# Dev (the MURMUR_DEV_DEK hatch avoids per-rebuild Keychain re-prompts; see .claude/skills/tauri-dev)
source ~/.cargo/env
MURMUR_DEV_DEK=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef npm run dev
#   → ng on http://localhost:1420, MCP on 127.0.0.1:8765

# Quality gates
( cd src-tauri && cargo test --lib )   # the test loop — NEVER `cargo clippy --all-targets` (openssl/sqlcipher profile thrash)
npx ng lint
npx ng build
bash scripts/ci.sh                      # full: clippy -D warnings + tests + lint + build + headless E2E
```

## Non-negotiable constraints

1. **Local-first / privacy.** Audio + transcript stay on device. Ollama / `claude_code` = nothing leaves; `anthropic` = only the redacted transcript leaves. New cloud egress must be loud + justified.
2. **Obsidian-native, owned files.** Output is plain `.md` (front-matter, `[[wikilinks]]`, `obsidian://` block-refs, `.canvas`). No lock-in.
3. **SQLite is canonical.** UI / MCP / Obsidian are thin readers/exporters — never three diverging copies of the truth.
4. **macOS-first.** Touch ID, ScreenCaptureKit, Keychain, notarization — don't assume cross-platform for free. `com.meetnotes.app` **MUST NOT change** (TCC + Keychain ACL continuity).
5. **Provider seam + redaction firewall stay intact** for any new AI capability.
6. **The lock model is load-bearing security** — see `.claude/rules/lock-model.md`. Every new content read/export MUST be gated; every new seal MUST verify-before-destroy.

## Definition of Done (binding)

A change is DONE only when verified — not when "code is written". Self-eval is systematically over-positive, so the verdict belongs to the **adversarial-verifier** (and the **lock-security-reviewer** for any lock-touching change), not the implementer.

1. **Static:** `cargo test --lib` + `npx ng lint` + `npx ng build` green (or `scripts/ci.sh`).
2. **Runtime:** the change live-reproduced (Playwright against `:1420` with a mocked `window.__TAURI_INTERNALS__.invoke`; or the dev app boots with no abort) — leaks/crashes/content-loss actively hunted, RED-before-GREEN for any bug fix.
3. **No abort / no leak / no loss:** the dev app launches clean; sealed-not-unlocked content stays masked across every read path; seal round-trips byte-identical.

## Release / deployment

The full step-by-step runbook is **[`.claude/skills/release-murmur`](.claude/skills/release-murmur/SKILL.md)** (supersedes `docs/RELEASE-CHECKLIST.md`). Key interlocks: sign **by identity HASH, not the name** (the cert CN has a Polish `ń`); merge to the `murmur` trunk **via a PR, never a direct push** (a `block-bash` hook forbids pushing to main/master); commits/PRs are authored **only** by `QueaT <kgm004a@gmail.com>` with **no Claude trailers**; `gh` active account = `JakubGawr`; a Developer-ID-signed (even un-notarized) build is shippable and enough to live-test Touch ID/lock.

## Agents, skills & rules (this repo's `.claude/`)

- **Agents** (`.claude/agents/`): `rust-tauri-dev`, `angular-zoneless-dev`, `adversarial-verifier`, `lock-security-reviewer`, `release-engineer`, `murmur-researcher`.
- **Skills** (`.claude/skills/`): `release-murmur` (deploy runbook), `tauri-dev` (run/iterate dev), `ship-feature` (the plan→build→adversarial-verify→PR pattern), `research`.
- **Rules** (`.claude/rules/`): `rust-tauri`, `angular-zoneless`, `lock-model`, `agentic-workflow`.

When a task is multi-step (a feature, a refactor, a release), reach for the **Workflow tool** and let an independent **adversarial-verifier** own the verdict — see `.claude/rules/agentic-workflow.md`.
