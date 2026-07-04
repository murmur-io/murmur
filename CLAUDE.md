# CLAUDE.md — Murmur

Guidance for Claude Code when working in this repository. These instructions are **binding** and override default behavior.

## ⚠️ Binding rules — AUTO-LOADED every session

The four rule files below are **imported into context automatically** via the `@` references — you don't need to open, read, or invoke anything; treat them as part of these instructions. Every forbidden pattern in them is genuinely forbidden, no exceptions without explicit user approval.

@.claude/rules/rust-tauri.md
@.claude/rules/angular-zoneless.md
@.claude/rules/lock-model.md
@.claude/rules/agentic-workflow.md

*(Orientation: `rust-tauri` = errors/commands/SQLCipher/additive-migrations/verify-before-destroy/gate-every-read/crash-safe-FFI/`cargo test --lib` only; `angular-zoneless` = signals-first/standalone/`@if`-`@for`/IPC→signals/the three traps; `lock-model` = gate every read + verify-before-destroy every seal + the `convertFileSrc` leak trap; `agentic-workflow` = Workflow tool + adversarial-verify discipline.)*

## What Murmur is

A **local-first macOS desktop app** that records meetings, transcribes on-device, turns the transcript into a clean note via a pluggable LLM provider, and lives inside the user's **Obsidian vault**. Currently shipped at **0.5.0**.

- **Stack:** Tauri 2.11 (Rust crate `murmur`, lib `meetnotes_lib`, bin `Murmur`) + Angular 18 **zoneless** (standalone, signals). IPC = Tauri commands (registered in `src-tauri/src/lib.rs` `generate_handler!`) + events. The FE talks to the backend through `src/app/core/ipc.service.ts` — **there is no NgRx**.
- **Pipeline:** capture (mic via `cpal` + system audio via a Swift **ScreenCaptureKit** sidecar) → **dual-stream** (transcribed separately, merged by wall-clock → `Me`/`Others`) → **whisper.cpp** (`whisper-rs`, Metal; default model **small**, ~470 MB — `tiny`…`large-v3` selectable) → segments → **SQLite (canonical source of truth, SQLCipher-encrypted)** → `SummarizerProvider` → note markdown → atomic **Obsidian `.md`** export.
- **Providers (one trait, swappable):** `claude_code` (default), `anthropic` (BYO key in Keychain), `ollama` (local). Cloud-bound text passes the **redaction firewall**.
- **Three consumption surfaces over one store:** the app UI, a local read-only **MCP server** (`127.0.0.1:8765`), and the Obsidian vault.

### Module map (verify against the tree — trust code, not docs)

**Rust** (`src-tauri/src/`): `commands.rs` (Tauri commands), `lib.rs` (handler registry + setup), `state.rs` (`AppState`), `error.rs` (`AppError`/`Result`), `events.rs`; `storage/` (`db.rs`, `migration.rs` = SQLCipher whole-DB encrypt-in-place, `models.rs`); `crypto.rs` (AES-256-GCM + `encrypt_file`/`decrypt_file`, verify-before-destroy); `secrets/keychain.rs` (keyring, service `com.meetnotes.app`, `MURMUR_DEV_DEK`/`MURMUR_DEV_KEK` debug hatches); `biometric.rs` (Touch ID); `screenshare.rs` (best-effort auto-relock, crash-safe `CGWindowList`); `audio/` (`recorder`, `system`, `mixer`, `merge`, `wav`, `listener`); `transcribe/` (`whisper`, `model`, `live`, `types`); `pipeline.rs`; `mcp.rs`; `summarize/`; `settings/config.rs`; `export/`.

**Angular** (`src/app/features/`): `analytics`, `ask`, `bar`, `detail`, `folders`, `graph`, `library`, `onboarding`, `record`, `settings`. Services: `core/ipc.service.ts`, `services/{folders,toast,screen-share}.service.ts`, `core/models.ts`.

## Common commands

```bash
# Dev (the MURMUR_DEV_DEK hatch avoids per-rebuild Keychain re-prompts; see .claude/skills/tauri-dev)
# No --features needed: the on-device brain/embedder/NER are ALWAYS compiled and activate at runtime
# on model-presence. `MISTRALRS_METAL_PRECOMPILE=0` is baked into src-tauri/.cargo/config.toml [env]
# (this Mac has only the Command Line Tools, not full Xcode → defer Metal-shader compile to first run),
# so `npm run dev` just works.
source ~/.cargo/env
MURMUR_DEV_DEK=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef npm run dev
#   → ng on http://localhost:1420, MCP on 127.0.0.1:8765

# Quality gates
( cd src-tauri && cargo test --lib )   # the test loop — NEVER `cargo clippy --all-targets` (openssl/sqlcipher profile thrash)
#   NOTE: the heavy mistralrs/candle ML tree is now ALWAYS compiled (no feature gate), so a COLD
#   first build is slow (hundreds of MB of ML deps); the incremental loop stays fast once warm.
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

Full runbook: **[`.claude/skills/release-murmur`](.claude/skills/release-murmur/SKILL.md)** (supersedes `docs/RELEASE-CHECKLIST.md`). Pipeline: gates green → version bump (package.json + src-tauri/tauri.conf.json + src-tauri/Cargo.toml, then `(cd src-tauri && cargo update -p murmur --precise <ver>)`) → QueaT commit → PR-merge to `murmur` → `rustup target add aarch64-apple-darwin x86_64-apple-darwin` → stop dev → `npx tauri build --target universal-apple-darwin --bundles app` → Developer-ID sign → DMG → **notarize** → staple → `gh release create`/upload.

### Hard-won release rules — DO NOT repeat the 2026-06-27 mess

1. **NOTARIZATION IS MANDATORY for every published release — never ship signed-only.** Use the existing notarytool keychain profile **`murmur`**: `xcrun notarytool submit <dmg> --keychain-profile murmur --wait` → `xcrun stapler staple <dmg>` → confirm `spctl -a -vvv -t open --context context:primary-signature <dmg>` says *Notarized Developer ID* → `gh release upload v<ver> -R murmur-io/murmur <dmg> --clobber`. A signed-but-un-notarized DMG is Gatekeeper-blocked on macOS 15 (no right-click→Open anymore; only Settings → Privacy & Security → Open Anyway). v0.3.0/0.3.1 shipped un-notarized by mistake and blocked the user — never again.
2. **Sign INSIDE-OUT, NEVER `--deep` — prefer `scripts/macos-sign-notarize.sh`.** Get the identity by HASH (the cert CN has a Polish `ń` "Gawroński" → name matching fails "no identity found"): `HASH=$(security find-identity -v -p codesigning | grep 'Developer ID Application' | head -1 | awk '{print $2}')`. **`codesign --deep` does NOT sign the bundled audio helpers in `Contents/Resources/` (`meetnotes-sysaudio` / `meetnotes-audiocap` / `meetnotes-aeccap`) → notarization comes back `Invalid` ("binary is not signed / no secure timestamp / no hardened runtime").** Sign each nested helper FIRST (`codesign --force --options runtime --timestamp --entitlements src-tauri/entitlements.plist --sign "$HASH" <helper>`), THEN seal the `.app` **without `--deep`**, THEN sign the DMG. `scripts/macos-sign-notarize.sh` already does exactly this — use it instead of hand-rolling codesign (the 2026-06-27 v0.4.0 notarization first failed because a manual `codesign --deep` skipped the three Resources/ helpers). On the codesign Developer-ID-key keychain prompt click **Always Allow / Allow** — clicking **Deny** gives `errSecInternalComponent` and leaves the bundle half-signed.
3. **NEVER run `security` / keychain CLI ops from the agent shell.** It can't surface the macOS auth dialog → the command HANGS → retries queue → many hung processes spamming the user (the 2026-06-27 loop was 11 `security` procs). Any keychain op needing auth (add / unlock / `notarytool store-credentials`) MUST be run by the **user** interactively (`!` in their terminal) or avoided. Even ACL/locked reads hang — never loop them. (Process kills like `pkill security` are fine; they don't touch the keychain.)
4. **`MURMUR_DEV_DEK` (dev) and the release keychain DEK collide on a shared DB.** The dev hatch encrypts the DB with the env DEK; the release build (no debug hatch) uses a Keychain DEK → on the same DB path the keys differ → the release build can't open a dev-keyed DB → startup fails. When testing a release on a dev machine, expect this; recover by restoring the plaintext `~/Library/Application Support/MeetNotes/meetnotes.sqlite.pre-encrypt.bak` so the release re-encrypts fresh (preserve the encrypted one first). Fresh-install real users are unaffected.
5. **A locked login keychain also breaks `git`/`gh` push** (the credential helper reads the GitHub token from it). `git push` → "could not read Username for https://github.com" means the keychain is locked, not that auth broke — the user unlocks it (`security unlock-keychain`, run BY THE USER) and you retry.
6. Merge to `murmur` **via a PR, never a direct push** (enforced by `.claude/hooks/block-bash.sh`); commits/PRs authored **only** by `QueaT <kgm004a@gmail.com>`, **no Claude trailers**; `gh` account = `JakubGawr`; `com.meetnotes.app` immutable.
7. **Startup must never hard-crash on a keychain/DB failure** (v0.3.1 made it a graceful dialog + clean exit, DB untouched) — keep it; never reintroduce an `init().expect()` / `.unwrap()` on the keychain-or-DB-open path.

## Agents, skills, rules & hooks (this repo's `.claude/`)

- **Rules** (`.claude/rules/`): `rust-tauri`, `angular-zoneless`, `lock-model`, `agentic-workflow` — **always-on** (auto-imported at the top of this file); apply them without being asked.
- **Hooks** (`.claude/hooks/`, wired in `.claude/settings.json`): **deterministic guardrails the agent cannot talk itself out of** — `block-bash.sh` (PreToolUse) refuses direct trunk pushes, the hang-prone `security`/keychain CLI, `cargo clippy --all-targets`, and `codesign --deep`; `secret-scan.sh` blocks a `git commit` whose staged diff carries key/token/DEK material; `finish-guard.sh` gates the commit on the DoD verdicts (advisory by default); `autoformat.sh` is opt-in (`MURMUR_AUTOFMT=1`). Full reference + the advisory→enforce toggles: **[`.claude/hooks/README.md`](.claude/hooks/README.md)**. `bash .claude/hooks/selftest.sh` proves they still block (guards against another phantom-hook).
- **Skills** (`.claude/skills/`): **invoke these PROACTIVELY the moment a task matches — the user should NOT have to type the slash command:**
  - cutting a build / version bump / publishing a release → **`release-murmur`**
  - starting, iterating, or debugging the dev app → **`tauri-dev`**
  - shipping a feature or a bug fix → **`ship-feature`**
  - a "should we / can we / how would we add X" question → **`research`**
  - running / understanding / debugging the CI gate (`scripts/ci.sh` + GitHub Actions) → **`ci-maintenance`**
  - adding or changing a check in the CI gate → **`add-ci-gate`**
  - writing / tuning a GitHub Actions workflow → **`github-actions`**
- **Agents** (`.claude/agents/`): `rust-tauri-dev`, `angular-zoneless-dev`, `adversarial-verifier`, `lock-security-reviewer`, `release-engineer`, `ci-cd-engineer` (designs & maintains CI — the local `scripts/ci.sh` gate + the GitHub Actions macOS PR-gate that wraps it; CD/notarized release stays with `release-engineer`), `murmur-researcher` — dispatch as subagents; the implementer never owns the verdict.

When a task is multi-step (a feature, a refactor, a release), reach for the **Workflow tool** and let an independent **adversarial-verifier** own the verdict.
