# AGENTS.md — Murmur

Guidance for Codex when working in this repository. These instructions are **binding** and override default behavior.

## Binding Rules

`AGENTS.md` is the Codex autoloaded project instruction file. The detailed rules live under
`.codex/rules/`; treat them as binding. Before editing a matching surface, read the relevant
rule file:

- `.codex/rules/rust-tauri.md`
- `.codex/rules/angular-zoneless.md`
- `.codex/rules/lock-model.md`
- `.codex/rules/agentic-workflow.md`

*(Orientation: `rust-tauri` = errors/commands/SQLCipher/additive-migrations/verify-before-destroy/gate-every-read/crash-safe-FFI/`cargo test --lib` only; `angular-zoneless` = signals-first/standalone/`@if`-`@for`/IPC→signals/dir-per-component (ts+html+scss)/Liquid-Glass views/design-tokens-only/mur-* design-system/the traps; `lock-model` = gate every read + verify-before-destroy every seal + the `convertFileSrc` leak trap; `agentic-workflow` = executable harness + adversarial-verify discipline.)*

## What Murmur is

A **local-first macOS desktop app** that records meetings, transcribes on-device, turns the transcript into a clean note via a pluggable LLM provider, and lives inside the user's **Obsidian vault**. Treat the manifests and GitHub releases as the version source of truth.

- **Stack:** Tauri 2.11 (Rust crate `murmur`, lib `meetnotes_lib`, bin `Murmur`) + Angular 22 **zoneless** (standalone, signals). IPC = Tauri commands (registered in `src-tauri/src/lib.rs` `generate_handler!`) + events. The FE talks to the backend through `src/app/core/ipc.service.ts` — **there is no NgRx**.
- **Pipeline:** capture (mic via `cpal` + system audio via a Swift **ScreenCaptureKit** sidecar) → **dual-stream** (transcribed separately, merged by wall-clock → `Me`/`Others`) → **whisper.cpp** (`whisper-rs`, Metal; selectable local model) → segments → **SQLite (canonical source of truth, SQLCipher-encrypted)** → `SummarizerProvider` → note markdown → atomic **Obsidian `.md`** export.
- **Providers (one trait, swappable):** `claude_code` (default), `anthropic` (BYO key in Keychain), `ollama` (local). Cloud-bound text passes the **redaction firewall**.
- **Three consumption surfaces over one store:** the app UI, a local read-only **MCP server** (`127.0.0.1:8765`), and the Obsidian vault.

### Module map (verify against the tree — trust code, not docs)

**Rust** (`src-tauri/src/`): `commands/` (`mod.rs` plus domain modules; Tauri commands), `lib.rs` (handler registry + setup), `state.rs` (`AppState`), `error.rs` (`AppError`/`Result`), `events.rs`; `storage/` (`db.rs` plus domain `*_store.rs`, `migration.rs` = SQLCipher whole-DB encrypt-in-place, `models.rs`); `crypto.rs` (AES-256-GCM + `encrypt_file`/`decrypt_file`, verify-before-destroy); `secrets/keychain.rs` (keyring + Security.framework user-presence-gated KEK/MK reads, service `com.meetnotes.app`, `MURMUR_DEV_DEK`/`MURMUR_DEV_KEK` debug hatches); `screenshare.rs` (best-effort auto-relock, crash-safe `CGWindowList`); `audio/` (`recorder`, `system`, `mixer`, `merge`, `wav`, `listener`); `transcribe/` (`whisper`, `model`, `live`, `types`); `reason.rs` + `reason/` (`sidecar.rs`, `afm.rs`) with the killable helper crate at `crates/murmur-brain/`; `pipeline.rs`; `mcp.rs`; `summarize/`; `settings/config.rs`; `export/`.

**Angular** (`src/app/features/`): `analytics`, `ask`, `bar`, `detail`, `folders`, `graph`, `library`, `onboarding`, `record`, `settings`. Services: `core/ipc.service.ts`, `services/{folders,toast,screen-share}.service.ts`, `core/models.ts`.

## Backend server — a SEPARATE repo at `../murmur-server/`

The accounts + sharing backend is **not in this repo** — it lives in the sibling checkout
`../murmur-server/` (GitHub `murmur-io/murmur-server`). Murmur is local-first and fully usable with
**no account**; this server is an **opt-in Tier 1** zero-knowledge relay that unlocks E2EE note +
Org "Shared Brain" sharing. It stores only **ciphertext blobs, wrapped keys, and public keys** —
never plaintext.

**When a task touches the backend/server** — accounts (OPAQUE login), link-share, Murmur↔Murmur
invites, Org sync, the sharing wire format, or anything the app calls over HTTPS — **read
`../murmur-server/` for the real server-side implementation**; do not reason from the client alone.
It is a Rust workspace:
- `crates/murmur-protocol` — the shared E2EE envelope + wire format, **compiled into BOTH the Tauri
  client (this repo) and the server**, so a format change must land in both or it's a compile error.
  A client-side sharing change usually has a server-side counterpart here (`MIT OR Apache-2.0`).
- `crates/murmur-server` — the axum + Postgres service (`AGPL-3.0`), deployed on **Railway**.

Authoritative design spec lives in THIS repo:
`docs/superpowers/specs/2026-07-04-murmur-server-spec.md` (accounts via OPAQUE, modes A/B, the
threat matrix §1.1, the one-way two-domain rule §9). Deploy / redeploy / logs / env: follow the
runbook `../murmur-server/DEPLOY.md` (Railway, GraphQL API not CLI) — never hand-roll ops.

## Common commands

```bash
# Dev (the MURMUR_DEV_DEK hatch avoids per-rebuild Keychain re-prompts; see .agents/skills/tauri-dev)
# No --features needed: the on-device brain/embedder/NER are ALWAYS compiled and activate at runtime
# on model-presence. `MISTRALRS_METAL_PRECOMPILE=0` is baked into the workspace `.cargo/config.toml` [env]
# (this Mac has only the Command Line Tools, not full Xcode → defer Metal-shader compile to first run),
# so the supervised dev command just works without monopolizing the shared Cargo lane.
source ~/.cargo/env
MURMUR_DEV_DEK=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef scripts/agent-dev-run -- npm run dev
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

1. **Local-first / privacy.** Audio + transcript stay on device. On-device providers and loopback Ollama are local. `claude_code`, Anthropic, Gateway, remote Ollama, and unknown providers are cloud-classified and must pass explicit consent, redaction where applicable, and the egress ledger. New cloud egress must be loud + justified.
2. **Obsidian-native, owned files.** Output is plain `.md` (front-matter, `[[wikilinks]]`, `obsidian://` block-refs, `.canvas`). No lock-in.
3. **SQLite is canonical.** UI / MCP / Obsidian are thin readers/exporters — never three diverging copies of the truth.
4. **macOS-first.** Touch ID, ScreenCaptureKit, Keychain, notarization — don't assume cross-platform for free. `com.meetnotes.app` **MUST NOT change** (TCC + Keychain ACL continuity).
5. **Provider seam + redaction firewall stay intact** for any new AI capability.
6. **The lock model is load-bearing security** — see `.codex/rules/lock-model.md`. Every new content read/export MUST be gated; every new seal MUST verify-before-destroy.

## Definition of Done (binding)

A change is DONE only when verified — not when "code is written". Self-eval is systematically over-positive, so the verdict belongs to the **adversarial-verifier** (and the **lock-security-reviewer** for any lock-touching change), not the implementer.

1. **Static:** `cargo test --lib` + `npx ng lint` + `npx ng build` green (or `scripts/ci.sh`).
2. **Runtime:** the change live-reproduced (Playwright against `:1420` with a mocked `window.__TAURI_INTERNALS__.invoke`; or the dev app boots with no abort) — leaks/crashes/content-loss actively hunted, RED-before-GREEN for any bug fix.
3. **No abort / no leak / no loss:** the dev app launches clean; sealed-not-unlocked content stays masked across every read path; seal round-trips byte-identical.

## Release / deployment

Full runbook: **[`.agents/skills/release-murmur`](.agents/skills/release-murmur/SKILL.md)** (supersedes `docs/RELEASE-CHECKLIST.md`). Pipeline: gates green → version bump (package.json + src-tauri/tauri.conf.json + src-tauri/Cargo.toml, then `(cd src-tauri && cargo update -p murmur --precise <ver>)`) → QueaT commit → PR-merge to `murmur` → `rustup target add aarch64-apple-darwin x86_64-apple-darwin` → stop dev → `npx tauri build --target universal-apple-darwin --bundles app` → Developer-ID sign → DMG → **notarize** → staple → `gh release create`/upload.

### Hard-won release rules — DO NOT repeat the 2026-06-27 mess

1. **NOTARIZATION IS MANDATORY for every published release — never ship signed-only.** Use the existing notarytool keychain profile **`murmur`**: `xcrun notarytool submit <dmg> --keychain-profile murmur --wait` → `xcrun stapler staple <dmg>` → confirm `spctl -a -vvv -t open --context context:primary-signature <dmg>` says *Notarized Developer ID* → `gh release upload v<ver> -R murmur-io/murmur <dmg> --clobber`. A signed-but-un-notarized DMG is Gatekeeper-blocked on macOS 15 (no right-click→Open anymore; only Settings → Privacy & Security → Open Anyway). v0.3.0/0.3.1 shipped un-notarized by mistake and blocked the user — never again.
2. **Sign INSIDE-OUT, NEVER `--deep` — prefer `scripts/macos-sign-notarize.sh`.** The Developer-ID identity HASH must be supplied by the user/operator; an agent must not derive it with the `security` CLI. **`codesign --deep` does NOT sign the bundled audio helpers in `Contents/Resources/` (`meetnotes-sysaudio` / `meetnotes-audiocap` / `meetnotes-aeccap`) → notarization comes back `Invalid` ("binary is not signed / no secure timestamp / no hardened runtime").** Sign each nested helper FIRST (`codesign --force --options runtime --timestamp --entitlements src-tauri/entitlements.plist --sign "$HASH" <helper>`), THEN seal the `.app` **without `--deep`**, THEN sign the DMG. On the codesign Developer-ID-key keychain prompt click **Always Allow / Allow** — clicking **Deny** gives `errSecInternalComponent` and leaves the bundle half-signed.
3. **NEVER run `security` / keychain CLI ops from the agent shell.** It can't surface the macOS auth dialog → the command HANGS → retries queue → many hung processes spamming the user (the 2026-06-27 loop was 11 `security` procs). Any keychain op needing auth (add / unlock / `notarytool store-credentials`) MUST be run by the **user** interactively (`!` in their terminal) or avoided. Even ACL/locked reads hang — never loop them. (Process kills like `pkill security` are fine; they don't touch the keychain.)
4. **Dev and release data are intentionally isolated.** Debug/dev resolves through `state::app_dir_name()` to `MeetNotes-dev`; release uses `MeetNotes`. Never copy, restore, delete, or re-key the release database as part of a dev test.
5. **A locked login keychain also breaks `git`/`gh` push** (the credential helper reads the GitHub token from it). `git push` → "could not read Username for https://github.com" means the keychain is locked, not that auth broke — the user unlocks it (`security unlock-keychain`, run BY THE USER) and you retry.
6. Merge to `murmur` **via a PR, never a direct push** (guard script: `.codex/hooks/block-bash.sh`); commits/PRs authored **only** by `QueaT <kgm004a@gmail.com>`, **no AI co-author trailers**; `gh` account = `JakubGawr`; `com.meetnotes.app` immutable.
7. **Startup must never hard-crash on a keychain/DB failure** (v0.3.1 made it a graceful dialog + clean exit, DB untouched) — keep it; never reintroduce an `init().expect()` / `.unwrap()` on the keychain-or-DB-open path.

## Agents, skills, rules & hooks (this repo's `.codex/`)

- **Rules** (`.codex/rules/`): `rust-tauri`, `angular-zoneless`, `lock-model`, `agentic-workflow` — binding local references; read the relevant one before changing that surface.
- **Harness + hooks** (`.agents/harness/`, `scripts/agent-harness`, `.codex/hooks/`): the neutral runner owns isolated worktrees, bounded repair, checks, independent reviews, hash-bound PASS attestations, and the exact `commit`/`close` lifecycle. Hooks are fast defense-in-depth; CI is the remote truth. Run `scripts/agent-harness selftest` and `scripts/agent-config-audit`.
- **Skills** (`.agents/skills/`): **invoke these PROACTIVELY the moment a task matches — the user should NOT have to type the slash command:**
  - cutting a build / version bump / publishing a release → **`release-murmur`**
  - starting, iterating, or debugging the dev app → **`tauri-dev`**
  - shipping a feature or a bug fix → **`ship-feature`**
  - a "should we / can we / how would we add X" question → **`research`**
  - running / understanding / debugging the CI gate (`scripts/ci.sh` + GitHub Actions) → **`ci-maintenance`**
  - adding or changing a check in the CI gate → **`add-ci-gate`**
  - writing / tuning a GitHub Actions workflow → **`github-actions`**
  - divergent product ideation / "dream up something for Murmur" → **`dreaming`**
  - recording or curating the lessons loop → **`murmur-learn`**, **`murmur-curate-learnings`**
- **Agents** (`.codex/agents/*.toml`): `rust-tauri-dev`, `angular-zoneless-dev`, `adversarial-verifier`, `lock-security-reviewer`, `release-engineer`, `ci-cd-engineer` (designs & maintains CI — the local `scripts/ci.sh` gate + the GitHub Actions macOS PR-gate that wraps it; CD/notarized release stays with `release-engineer`), `murmur-researcher` — spawn as custom subagents; the implementer never owns the verdict.

When a task mutates the repository, use `scripts/agent-harness`: one isolated writer, deterministic checks, fresh independent reviewers, bounded repair and a hash-bound attestation. The implementer never owns the verdict.

## Opt-in harness (`/harness`)

The harness is **opt-in**. Normal commits run freely; only `secret-scan` and
direct-push-to-`murmur` protection are always on. Reach for rigor deliberately:

- **Codex has no skills mechanism** — invoke the harness directly:
  `scripts/agent-harness init <task-id> --prompt "…" --owned <path> && scripts/agent-harness run <task-id> && scripts/agent-harness commit <task-id> -m "…"`.
- Use it for lock/crypto/egress/protocol changes or anything you want a fresh
  adversarial reviewer to verify. Skip it for docs/chores/low-risk edits.
- Guard behavior is identical across vendors (same `hook_guard.py`): a commit in
  a worktree with **no** active task is allowed; a worktree **with** a task
  enforces the full hash-bound attestation.
