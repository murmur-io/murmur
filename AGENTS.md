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

Full runbook: **[`.agents/skills/release-murmur`](.agents/skills/release-murmur/SKILL.md)** (supersedes `docs/RELEASE-CHECKLIST.md`). Pipeline: gates green → version bump (package.json + src-tauri/tauri.conf.json + src-tauri/Cargo.toml, then `(cd src-tauri && cargo update -p murmur --precise <ver>)`) → JakubGawr commit → PR-merge to `murmur` → `rustup target add aarch64-apple-darwin x86_64-apple-darwin` → stop dev → `npx tauri build --target universal-apple-darwin --bundles app` → Developer-ID sign → DMG → **notarize** → staple → `gh release create`/upload.

### Hard-won release rules — DO NOT repeat the 2026-06-27 mess

1. **NOTARIZATION IS MANDATORY for every published release — never ship signed-only.** Use the existing notarytool keychain profile **`murmur`**: `xcrun notarytool submit <dmg> --keychain-profile murmur --wait` → `xcrun stapler staple <dmg>` → confirm `spctl -a -vvv -t open --context context:primary-signature <dmg>` says *Notarized Developer ID* → `gh release upload v<ver> -R murmur-io/murmur <dmg> --clobber`. A signed-but-un-notarized DMG is Gatekeeper-blocked on macOS 15 (no right-click→Open anymore; only Settings → Privacy & Security → Open Anyway). v0.3.0/0.3.1 shipped un-notarized by mistake and blocked the user — never again.
2. **Sign INSIDE-OUT, NEVER `--deep` — prefer `scripts/macos-sign-notarize.sh`.** The Developer-ID identity HASH must be supplied by the user/operator; an agent must not derive it with the `security` CLI. **`codesign --deep` does NOT sign the bundled audio helpers in `Contents/Resources/` (`meetnotes-sysaudio` / `meetnotes-audiocap` / `meetnotes-aeccap`) → notarization comes back `Invalid` ("binary is not signed / no secure timestamp / no hardened runtime").** Sign each nested helper FIRST (`codesign --force --options runtime --timestamp --entitlements src-tauri/entitlements.plist --sign "$HASH" <helper>`), THEN seal the `.app` **without `--deep`**, THEN sign the DMG. On the codesign Developer-ID-key keychain prompt click **Always Allow / Allow** — clicking **Deny** gives `errSecInternalComponent` and leaves the bundle half-signed.
3. **NEVER run `security` / keychain CLI ops from the agent shell.** It can't surface the macOS auth dialog → the command HANGS → retries queue → many hung processes spamming the user (the 2026-06-27 loop was 11 `security` procs). Any keychain op needing auth (add / unlock / `notarytool store-credentials`) MUST be run by the **user** interactively (`!` in their terminal) or avoided. Even ACL/locked reads hang — never loop them. (Process kills like `pkill security` are fine; they don't touch the keychain.)
4. **Dev and release data are intentionally isolated.** Debug/dev resolves through `state::app_dir_name()` to `MeetNotes-dev`; release uses `MeetNotes`. Never copy, restore, delete, or re-key the release database as part of a dev test.
5. **A locked login keychain also breaks `git`/`gh` push** (the credential helper reads the GitHub token from it). `git push` → "could not read Username for https://github.com" means the keychain is locked, not that auth broke — the user unlocks it (`security unlock-keychain`, run BY THE USER) and you retry.
6. Merge to `murmur` **via a PR, never a direct push** (guard script: `.codex/hooks/block-bash.sh`); commits/PRs authored **only** by `JakubGawr <63911380+JakubGawr@users.noreply.github.com>`, **no AI co-author trailers**; `gh` account = `JakubGawr`; `com.meetnotes.app` immutable.
7. **Startup must never hard-crash on a keychain/DB failure** (v0.3.1 made it a graceful dialog + clean exit, DB untouched) — keep it; never reintroduce an `init().expect()` / `.unwrap()` on the keychain-or-DB-open path.

## Agents, skills, rules & hooks (this repo's `.codex/`)

- **Rules** (`.codex/rules/`): `rust-tauri`, `angular-zoneless`, `lock-model`, `agentic-workflow` — binding local references; read the relevant one before changing that surface.
- **Harness + hooks** (`.agents/h/`, `scripts/h`, `.codex/hooks/`): the verifier-only runner owns isolated worktrees, exact-diff checks, independent reviews, hash-bound PASS receipts, guarded `commit`, resumable evidence, and lossless `clean`. It has no writer or automatic repair loop. Hooks are fast defense-in-depth; CI is the remote truth. Run `- **Skills** (`.agents/skills/`): **invoke these PROACTIVELY the moment a task matches — the user should NOT have to type the slash command:**
  - cutting a build / version bump / publishing a release → **`release-murmur`**
  - starting, iterating, or debugging the dev app → **`tauri-dev`**
  - shipping a feature or a bug fix → **`ship-feature`**
  - a "should we / can we / how would we add X" question → **`research`**
  - running / understanding / debugging the CI gate (`scripts/ci.sh` + GitHub Actions) → **`ci-maintenance`**
  - adding or changing a check in the CI gate → **`add-ci-gate`**
  - writing / tuning a GitHub Actions workflow → **`github-actions`**
  - a promo / demo / tour video, a landing hero loop, or footage of a new feature → **`promo-video`**
  - divergent product ideation / "dream up something for Murmur" → **`dreaming`**
  - recording or curating the lessons loop → **`murmur-learn`**, **`murmur-curate-learnings`**
- **Agents** (`.codex/agents/*.toml`): `rust-tauri-dev`, `angular-zoneless-dev`, `adversarial-verifier`, `lock-security-reviewer`, `release-engineer`, `ci-cd-engineer` (designs & maintains CI — the local `scripts/ci.sh` gate + the GitHub Actions macOS PR-gate that wraps it; CD/notarized release stays with `release-engineer`), `murmur-researcher` — spawn as custom subagents; the implementer never owns the verdict.
  **Every dispatched agent MUST read `.claude/learnings/<agent-name>.md` first when that file
  exists.** That tree is CANONICAL for both vendors — `.codex/learnings/` is a generated byte
  mirror, regenerated by `.agents/h/mirror-check --fix` and parity-enforced by
  `.agents/h/mirror-check`, so never hand-edit it. Its `## Recurring patterns` are binding
  imperatives distilled from failures this project already paid for and they outrank the agent's
  own general guidance.

## The development loop (binding — this is the default, not a suggestion)

**Track A — every change. Plan and implement in ONE session; do not hand a plan to a fresh agent
to implement.** Splitting those two loses every implicit decision made while exploring; divide work
by context boundary, not by task type.

```bash
# ISOLATED CHECKOUT FIRST — never `git checkout -b` in the primary checkout.
git worktree add -b <slug> ../.murmur-agent-tasks/<slug> origin/murmur
cd ../.murmur-agent-tasks/<slug>
# plan AND implement here
.agents/h/mirror-check                        # 0.1s — run it every time
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

Control-plane changes (`.claude/**`, `.codex/**`, `.agents/**`, `CLAUDE.md`, `AGENTS.md`, reviewer
prompts) cannot be certified by the Harness and go in a worktree **outside** the runner-owned
`../.murmur-agent-tasks` root — for example `../.murmur-control-plane/<slug>`.

`mirror-check` jest pierwszy, bo to najtanszy check w repo i jedyny, ktory widzi cala klase defektow niewidoczna dla reszty. Measured on PR #535: `cargo test --lib`, `ng lint`
and `ng build` were all green while the diff contained (a) `.claude/`↔`.codex/` binding-rule drift
and (b) a Bash-hook change that silently disabled secret scanning and the commit finish-guard. The
audit caught both, in **0.1 s**, but it only ran remotely. Run it unconditionally; deciding whether
a diff "touches the control plane" costs more thought than the check costs time.

GitHub Actions running `scripts/ci.sh` is the **only** merge authority.

**Escalate to `scripts/h` only when the diff touches a path in
`.agents/h/checks.json` `risk_classification.{lock,egress,protocol}`.** That test is
mechanical — never a judgement call about whether the work "feels risky", which is how one feature
came to consume eleven task ids. When you do escalate, pass the real plan:
**`--prompt-file plan.md`, never `--prompt "<one sentence>"`** — the combined reviewer checks the
diff against the acceptance contract it is given, so a one-line contract produces a one-line review.

The implementer edits the isolated worktree but never owns the verdict. For protected
Harness/control-plane changes, which cannot self-certify, use a dedicated worktree outside the
runner-owned `../.murmur-agent-tasks` root (for example `../.murmur-control-plane/<task-id>`), the
complete control-plane selftests, a fresh independent review, and the base-anchored CI gate.

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

**Read the cost before arguing about it.** `scripts/h metrics --store
../.murmur-agent-driver/.git/h` reports reviewer PASS-rate, findings by severity, model
minutes and tokens per accepted task. Every claim about a reviewer earning or not earning its place
belongs to that command, not to an impression.

## Opt-in harness (`/harness`)

The harness is **opt-in**. Normal commits run freely; only `secret-scan` and
direct-push-to-`murmur` protection are always on. Reach for rigor deliberately:

- Invoke the harness directly:
  `scripts/h open <task-id> --prompt "…" --owned <path>`, implement in the printed worktree, then `plan`, `verify`/`resume`, `commit`, and finally `clean` after merge.
- Use it for lock/crypto/egress/protocol changes or anything you want a fresh
  adversarial reviewer to verify. Skip it for docs/chores/low-risk edits.
  `codex`. The reviewer has no developer-session context and no local tools.
  `lock`/`egress`/`protocol` specialist reviews prefer the opposite vendor; when
  that vendor is unavailable, bind the explicit
  `--allow-same-vendor-high-risk` exception instead of silently spending it.
