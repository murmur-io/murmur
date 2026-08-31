# CLAUDE.md — Murmur

Guidance for Claude Code when working in this repository. These instructions are **binding** and override default behavior.

## ⚠️ Binding rules — AUTO-LOADED every session

The four rule files below are **imported into context automatically** via the `@` references — you don't need to open, read, or invoke anything; treat them as part of these instructions. Every forbidden pattern in them is genuinely forbidden, no exceptions without explicit user approval.

@.claude/rules/rust-tauri.md
@.claude/rules/angular-zoneless.md
@.claude/rules/lock-model.md
@.claude/rules/agentic-workflow.md

*(Orientation: `rust-tauri` = errors/commands/SQLCipher/additive-migrations/verify-before-destroy/gate-every-read/crash-safe-FFI/`cargo test --lib` only; `angular-zoneless` = signals-first/standalone/`@if`-`@for`/IPC→signals/dir-per-component (ts+html+scss)/Liquid-Glass views/design-tokens-only/mur-* design-system/the traps; `lock-model` = gate every read + verify-before-destroy every seal + the `convertFileSrc` leak trap; `agentic-workflow` = weryfikuje inny model niż implementujący + `scripts/h`.)*

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
threat matrix §1.1, the one-way two-domain rule §9). Deploy / redeploy / logs / env are owned by the
**`deploy-murmur-server`** skill (runbook: `../murmur-server/DEPLOY.md`) — never hand-roll Railway ops.

## Common commands

```bash
# Dev (the MURMUR_DEV_DEK hatch avoids per-rebuild Keychain re-prompts; see .claude/skills/tauri-dev)
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
2. **Sign INSIDE-OUT, NEVER `--deep` — prefer `scripts/macos-sign-notarize.sh`.** The Developer-ID identity HASH must be supplied by the user/operator; an agent must not derive it with the `security` CLI. **`codesign --deep` does NOT sign the bundled audio helpers in `Contents/Resources/` (`meetnotes-sysaudio` / `meetnotes-audiocap` / `meetnotes-aeccap`) → notarization comes back `Invalid` ("binary is not signed / no secure timestamp / no hardened runtime").** Sign each nested helper FIRST (`codesign --force --options runtime --timestamp --entitlements src-tauri/entitlements.plist --sign "$HASH" <helper>`), THEN seal the `.app` **without `--deep`**, THEN sign the DMG. On the codesign Developer-ID-key keychain prompt click **Always Allow / Allow** — clicking **Deny** gives `errSecInternalComponent` and leaves the bundle half-signed.
3. **NEVER run `security` / keychain CLI ops from the agent shell.** It can't surface the macOS auth dialog → the command HANGS → retries queue → many hung processes spamming the user (the 2026-06-27 loop was 11 `security` procs). Any keychain op needing auth (add / unlock / `notarytool store-credentials`) MUST be run by the **user** interactively (`!` in their terminal) or avoided. Even ACL/locked reads hang — never loop them. (Process kills like `pkill security` are fine; they don't touch the keychain.)
4. **Dev and release data are intentionally isolated.** Debug/dev resolves through `state::app_dir_name()` to `MeetNotes-dev`; release uses `MeetNotes`. Never copy, restore, delete, or re-key the release database as part of a dev test.
5. **A locked login keychain also breaks `git`/`gh` push** (the credential helper reads the GitHub token from it). `git push` → "could not read Username for https://github.com" means the keychain is locked, not that auth broke — the user unlocks it (`security unlock-keychain`, run BY THE USER) and you retry.
6. Merge to `murmur` **via a PR, never a direct push** (enforced by `.claude/hooks/block-bash.sh`); commits/PRs authored **only** by `QueaT <kgm004a@gmail.com>`, **no Claude trailers**; `gh` account = `JakubGawr`; `com.meetnotes.app` immutable.
7. **Startup must never hard-crash on a keychain/DB failure** (v0.3.1 made it a graceful dialog + clean exit, DB untouched) — keep it; never reintroduce an `init().expect()` / `.unwrap()` on the keychain-or-DB-open path.

## Agents, skills, rules & hooks (this repo's `.claude/`)

- **Rules** (`.claude/rules/`): `rust-tauri`, `angular-zoneless`, `lock-model`, `agentic-workflow` — **always-on** (auto-imported at the top of this file); apply them without being asked.
- **Harness + hooks** (`.agents/h/`, `scripts/h`, `.claude/hooks/`): `scripts/h run` bierze zadanie, planuje, implementuje w izolowanym worktree, odpala checki zawężone do zmienionych ścieżek i weryfikuje innym modelem; przy `NIE_DZIALA` poprawia ten sam agent, max dwie rundy. Hooki to tania obrona w głąb; CI jest zdalną prawdą.
- **Skills** (`.claude/skills/`): **invoke these PROACTIVELY the moment a task matches — the user should NOT have to type the slash command:**
  - cutting a build / version bump / publishing a release → **`release-murmur`**
  - starting, iterating, or debugging the dev app → **`tauri-dev`**
  - shipping a feature or a bug fix → **`ship-feature`**
  - a "should we / can we / how would we add X" question → **`research`**
  - running / understanding / debugging the CI gate (`scripts/ci.sh` + GitHub Actions) → **`ci-maintenance`**
  - adding or changing a check in the CI gate → **`add-ci-gate`**
  - writing / tuning a GitHub Actions workflow → **`github-actions`**
  - a promo / demo / tour video, a landing hero loop, or footage of a new feature → **`promo-video`**
- **Agents** (`.claude/agents/`): `rust-tauri-dev`, `angular-zoneless-dev`, `adversarial-verifier`, `lock-security-reviewer`, `release-engineer`, `ci-cd-engineer` (designs & maintains CI — the local `scripts/ci.sh` gate + the GitHub Actions macOS PR-gate that wraps it; CD/notarized release stays with `release-engineer`), `murmur-researcher` — dispatch as subagents; the implementer never owns the verdict.
  **Every dispatched agent MUST read `.claude/learnings/<agent-name>.md` first when that file
  exists** (today: `adversarial-verifier`, `angular-zoneless-dev`, `lock-security-reviewer`,
  `murmur-researcher`, `release-engineer`, `rust-tauri-dev`). Its `## Recurring patterns` are
  binding imperatives distilled from failures this project already paid for, and they outrank the
  agent's own general guidance. That journal used to describe itself as a "compatibility mirror,
  not executable input" that no agent definition referenced, so 939 lines of bought-and-paid-for
  lessons were reaching nobody. It is now the **canonical** tree: `.codex/learnings/` is a
  generated byte mirror, regenerowane przez `.agents/h/mirror-check --fix` i pilnowane przez
  `.agents/h/mirror-check`. Never hand-edit the mirror — record a lesson with
  `/learn <agent>: <lesson>`, promote it with `/curate-learnings <agent>`, then sync.

## The development loop (binding — this is the default, not a suggestion)

**Track A — every change. Plan and implement in ONE session; do not hand a plan to a fresh agent
to implement.** Splitting those two loses every implicit decision made while exploring; divide work
by context boundary, not by task type.

```bash
# ISOLATED CHECKOUT FIRST — never `git checkout -b` in the primary checkout.
git worktree add -b <slug> ../.murmur-agent-tasks/<slug> origin/murmur
cd ../.murmur-agent-tasks/<slug>
# plan AND implement here
.agents/h/mirror-check                                 # 0.02s — odpalaj zawsze
(cd src-tauri && cargo test --lib) && npx ng lint && npx ng build
git commit && gh pr create -R murmur-io/murmur
# CI red? ANOTHER COMMIT ON THE SAME BRANCH — never a new task id.
# After the PR merges:  git worktree remove ../.murmur-agent-tasks/<slug>
```

**Why a worktree and not `git checkout -b`.** This block used to say `git checkout -b <slug>`,
which contradicted `ship-feature`'s own "ordinary low-risk fixes keep the normal isolated-worktree
route" — and since THIS file is the one loaded into every session, the unsafe half won by default.
The primary checkout is routinely shared: another agent session or the operator can be mid-change on
their own branch, with uncommitted work in the tree. `git checkout -b` there moves HEAD under them
and re-attributes their work to your new branch. That is not hypothetical — it happened on
2026-08-03, branching off a live `feat/dashboards` with 29 uncommitted entries in the tree
(recovered, nothing lost, because `checkout -b` commits nothing). `.agents/h/guard.py`
`_primary_branch_surgery_reason` now refuses branch selection in a dirty primary checkout;
`MURMUR_ALLOW_PRIMARY_BRANCH_SURGERY=1` is the deliberate override for when the tree is provably
yours or you are restoring the branch you moved off.

Zmiany w control-plane (`.claude/**`, `.codex/**`, `.agents/**`, `CLAUDE.md`, `AGENTS.md`, prompty) idą zwykłą ścieżką — o ich poprawności decyduje CI, nie harness.

`mirror-check` jest pierwszy, bo to najtanszy check w repo i jedyny, ktory widzi cala klase defektow niewidoczna dla reszty. Measured on PR #535: `cargo test --lib`, `ng lint`
and `ng build` were all green while the diff contained (a) `.claude/`↔`.codex/` binding-rule drift
and (b) a Bash-hook change that silently disabled secret scanning and the commit finish-guard. The
audit caught both, in **0.1 s**, but it only ran remotely — so the feedback arrived one CI
round-trip later instead of before the commit. Run it unconditionally; deciding whether a diff
"touches the control plane" costs more thought than the check costs time.

GitHub Actions running `scripts/ci.sh` is the **only** merge authority.

**Sięgnij po `scripts/h`, gdy diff dotyka lock/crypto/egress/protocol albo gdy zadanie jest wieloetapowe.** Test jest mechaniczny — nigdy nie zgaduj, czy praca „wydaje się ryzykowna", bo tak jedna funkcja zjadła jedenaście task-id. Przekazuj realne zadanie w `--prompt`: weryfikator sprawdza diff wobec kryteriów akceptacji z planu, więc jednozdaniowe zadanie daje jednozdaniową weryfikację.

Implementujący edytuje izolowany worktree, ale nigdy nie wydaje werdyktu — weryfikuje inny model.

**Track B — the scaffold improves only through oracles.** When a bug class reaches a user, the fix
is not done until a deterministic oracle for it exists in `src-tauri/src/**/tests/` or `e2e/**`.
The four shipped classes and their oracles: seal content loss →
`db_tests/lock_tests.rs::seal_transcript_timeline_round_trips_byte_identical`; sealed-content leak →
`commands/tests/lock_read_gate_tests.rs`; macOS FFI abort at launch →
`scripts/harness-runtime-smoke.py`; packaged-WebKit CSP style loss →
`e2e/render/csp-style-src.spec.ts`. A rule or skill you cannot express as an oracle is a rule whose
effect you are not measuring.

**Track C — a scaffold edit is not done until it is measured.** When a diff touches
`.claude/rules/**`, `.codex/rules/**`, `.claude/skills/**`, `.agents/skills/**`, `CLAUDE.md`,
`AGENTS.md`, or a reviewer prompt, run the comparison and put its table in the PR body:

```bash
python3 eval/agents/matrix.py \
  --agent 'claude=claude -p --permission-mode acceptEdits' \
  --scaffold none --scaffold full --repeat 3 --seed 1 \
  --json eval/agents/results/<slug>.json
```

The control arm gets no scaffold, the treatment arm gets the real always-on envelope, so the delta
is the edit's effect and nothing else. It costs live model calls, which is why CI runs only
`--mode fake` (that arm proves the GRADERS still reject a wrong answer — a grader that has lost its
teeth accepts both arms and every later measurement silently reports success).

Two honest limits, both recorded per task in `eval/agents/README.md`: of the eight tasks only
`additive-migration` is currently `CAN_MEASURE`; the rest ceiling out because a competent model
already knows the answer. And `files_changed: []` on an `expected_change: true` task means the run
never reached the behaviour under test — that is an instrument failure, not a wrong answer. A delta
of zero across ceiling tasks says nothing about the edit; say so rather than reporting it as
evidence the rule works.

**Zanim zaczniesz się spierać o koszt, zmierz go.** Każde twierdzenie, że jakiś check albo reviewer zarabia na swoje utrzymanie, należy do danych z realnych uruchomień, nie do wrażenia. Poprzedni harness miał 38 226 linii i cztery typy reviewerów; pomiar na 205 zadaniach pokazał, że specjaliści wnosili coś ponad generalistę w 5,7% przypadków — i dlatego ich nie ma.

## Harness (`scripts/h`) — opt-in

Zwykłe commity idą normalnie; zawsze aktywne są tylko `secret-scan` i ochrona
przed bezpośrednim pushem na `murmur`. Po rygor sięgaj świadomie:

```bash
scripts/h run <task-id> --prompt "co ma być zrobione"
```

`worktree → plan → implementacja → checki + weryfikacja → max 2 poprawki → koniec`.
Weryfikuje inny model niż ten, który pisał (domyślnie kod: claude, weryfikacja:
codex). Weryfikator odpowiada wyłącznie na pytanie **czy ta funkcjonalność
działa** — nie recenzuje kodu spoza zadania i nie żąda dodatkowych dowodów.
Po dwóch nieudanych poprawkach staje i pyta człowieka.

Sięgaj po niego przy zmianach w lock/crypto/egress/protocol albo wszędzie tam,
gdzie chcesz świeżej pary oczu. Pomijaj przy dokumentacji i drobiazgach.

Szczegóły i granice: `.agents/h/README.md`.
