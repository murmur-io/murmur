---
name: ship-feature
description: The agentic feature-shipping discipline for Murmur (Tauri 2.11 Rust core + Angular 18 zoneless). Scope → (Workflow) plan → build backend and/or FE under the project conventions → ADVERSARIAL verify (an adversarial-verifier pass, plus a lock-security review when the change touches the lock/crypto/visibility path) → gates green (cargo test --lib, ng lint, ng build, scripts/ci.sh) → QueaT commit → PR to the `murmur` trunk. Use whenever the user wants to build, implement, add, or ship a feature/fix in Murmur end-to-end. Encodes the verify-before-trust discipline that caught 7 bugs.
---

# /ship-feature — ship a Murmur feature end-to-end

You are shipping a change into **Murmur** (Rust crate `murmur` / bin `Murmur` / lib
`meetnotes_lib` + Angular 18 zoneless). The job is not "write code" — it is "land a
**verified** change behind the project's gates and identity rules." The non-obvious value
here is **adversarial verification**: the team distrusts AI-written, self-certified code, and
this loop's adversarial pass is what previously **caught 7 bugs** that the implementing agent
believed were already correct. Self-eval is systematically over-positive — an independent
verifier, not the author, decides "done."

## The pipeline (stages — each gates the next)

### 1. Scope
Restate the change in one sentence. Classify which layer(s) it touches — this picks the
builder and whether the lock-security review is mandatory:
- **Backend** — `src-tauri/src/`: commands (`commands.rs`, registered in the
  `generate_handler!` in `lib.rs`), state (`state.rs`: `AppState` = db / unlocked_folders /
  master_kek), storage (`storage/{db.rs,migration.rs,models.rs}`), crypto (`crypto.rs`),
  secrets (`secrets/keychain.rs`), audio/transcribe/summarize/mcp/pipeline.
- **Frontend** — `src/app/`: features (`features/{record,detail,library,folders,ask,graph,
  settings,onboarding,bar,analytics}`), services (`core/ipc.service.ts`, `folders.service.ts`,
  `toast.service.ts`, `screen-share.service.ts`), `core/models.ts`.
- **Lock / crypto / visibility-gated** — ANY change to `crypto.rs`, `secrets/keychain.rs`,
  `biometric.rs`, `screenshare.rs`, `storage/migration.rs`, the unlock/seal/visibility path,
  or content-read gating ⇒ the **lock-security review is MANDATORY** in stage 4.

### 2. Plan (optionally via the Workflow tooling)
For a multi-part change, write a short plan: the exact files/symbols, the IPC seam (new Tauri
command name + the `ipc.service.ts` call), data shape (`core/models.ts` ↔ Rust DTO), and the
verification you'll demand. An orchestrator may drive this with the **Workflow / agent-team
tooling** (`TaskCreate`/`TaskUpdate`/`Monitor`) — e.g. fan out a backend builder and an FE
builder in parallel when the seam is agreed up front, then converge on the verifier. Keep the
plan honest about what a dev run *can't* prove (Touch ID / lock-at-rest / screen-share need a
signed build — see `/tauri-dev`).

### 3. Build — to the project conventions (non-negotiable)
Dispatch the work to the matching builder role (today only `murmur-researcher` exists as a
standing agent; the builder/verifier roles below are dispatched as task subagents — give each
the conventions explicitly). Iterate with `/tauri-dev` (`MURMUR_DEV_DEK` recipe,
`cargo test --lib` loop).

**Inject prior lessons first.** Before dispatching a role, prepend that agent's curated
`## Recurring patterns` from `.claude/learnings/<agent>.md` to its prompt as *"Previous lessons
(binding — do NOT repeat these)"*. This is the compounding-lessons loop
(`.claude/learnings/README.md`) — cheap, and it stops the fleet re-paying a bug it already caught.

**Rust / Tauri rules:**
- `AppError` + `Result` everywhere (`error.rs`); new commands registered in the
  `generate_handler!` in `lib.rs`.
- Migrations are **guarded + ADDITIVE only** (`storage/migration.rs`,
  `add_column_if_missing`) — never destructive.
- **Seal = verify-before-destroy:** when locking content, encrypt AND prove it decrypts back
  BEFORE blanking the plaintext (`crypto.rs` `encrypt_file`/`decrypt_file`,
  `storage/migration.rs` SQLCipher encrypt-in-place + verify). Never blank plaintext you
  haven't proven recoverable.
- **Every content read is gated** by `meeting_is_unlocked` / the visibility clause — a
  sealed-but-not-session-unlocked meeting must leak NOTHING (masked DTO `locked: true`),
  including via MCP (`mcp.rs`).
- **Crash-safe macOS FFI:** prefer CoreGraphics/CoreFoundation C funcs; if `msg_send` is
  unavoidable, GUARD it (`respondsToSelector:` / `class_getInstanceMethod`). Rust can't catch
  ObjC exceptions — an unguarded bad selector ABORTS at launch (the `NSScreen.isCaptured`
  lesson).
- No PII in logs.

**Angular (zoneless) rules:** standalone + `OnPush` + signals/`computed`/`effect` +
`inject()` + `input()`/`output()`/`viewChild()`; `@if`/`@for`/`@switch` ONLY (no
`*ngIf`/`*ngFor`); `toSignal()` for IPC streams (NEVER subscribe-for-state);
`afterNextRender()`/`afterRenderEffect()` (NEVER `setTimeout` in components); inline
template + styles; `var(--token)` CSS (no hardcoded hex/px); ≤16 kB per-component style
budget; inline SVG icons; **no new npm packages without explicit user approval.** Banned:
`markForCheck`, `@Input`/`@Output`/`EventEmitter`, `@ViewChild`, `BehaviorSubject`-as-state,
constructor injection.

### 4. ADVERSARIAL verify (the part that caught 7 bugs)
The author does **not** self-certify. Verify in **two sequential passes** (a spec review before a
code review catches "built the wrong thing" before code-quality noise buries it):

**4a — Spec review (fast):** does the diff implement what stage-1 scope / stage-2 plan actually
asked for? Nothing missing, nothing extra, the IPC seam and data shape as agreed. Only after this
signs off does the code/adversarial pass begin.

**4b — Adversarial-verifier pass** over the diff whose job is to make it FAIL:
- For each load-bearing claim ("it locks", "it's gated", "the migration is safe", "the signal
  updates"), ask **"what would make this false?"** and try it.
- Exercise the real seam, not mocks: run it under `/tauri-dev`, drive the IPC command, read
  `/tmp/murmur-dev.log` for panics/aborts, confirm the FE signal actually re-renders.
- Check the **negative**: a locked/sealed meeting leaks nothing through detail, segments,
  timeline, audio, OR MCP; an additive migration leaves existing rows intact.
- **If the change touches lock/crypto/visibility (stage 1), run the lock-security review TOO**
  — specifically: verify-before-destroy actually holds (no plaintext blanked pre-verify),
  every read path is visibility-gated, keychain ACL / `com.meetnotes.app` continuity
  untouched, no DEK/KEK/plaintext in logs, biometric gate not bypassed outside the dev hatch.

A verifier finding sends it BACK to stage 3. Do not advance on the author's say-so.

**Record the verdict as evidence (Phase-3 gate).** Each verifier writes a schema'd JSON to
`.claude/tmp/<task>/` (gitignored scratch) so the Definition-of-Done becomes machine-checkable, not
prose the committer eyeballs:
- adversarial-verifier → `adversarial-verify.json` = `{"verdict":"PASS"|"FAIL","findings":[…],"summary":"…"}`
- lock-security-reviewer (when stage-1 flagged lock/crypto/visibility) → `lock-security.json` (same
  shape) AND `touch .claude/tmp/<task>/.lock-touched` so the guard knows the lock gate is required.
- Optional observability: `.claude/lib/trace-span.sh <task> verify adversarial-verify PASS adversarial-verify.json`.

`.claude/hooks/finish-guard.sh` reads these on `git commit` (advisory by default; set
`MURMUR_FINISH_GUARD=enforce` to hard-block a commit whose gates aren't PASS).

### 5. Gates green
```bash
source "$HOME/.cargo/env"
( cd src-tauri && cargo test --lib )    # iterate loop
npx ng lint && npx ng build
bash scripts/ci.sh                      # full gate before commit (clippy + tests + lint + build + headless E2E)
```
`scripts/ci.sh` must end `✅ CI: all gates green`. (Reminder: `clippy --all-targets` belongs
in `ci.sh`, NOT the inner loop — see `/tauri-dev`.)

### 5b. Extract the lesson (close the loop)
If verification caught anything real — or the run confirmed a non-obvious approach that worked —
append ONE `## Run journal` entry to the relevant `.claude/learnings/<agent>.md` (or run
`/learn <agent>: <lesson>`), citing the artifact that revealed it. Periodically `/curate-learnings`
promotes repeat offenders into `## Recurring patterns`. This is what makes "every bug a permanent
lesson" instead of a re-paid one.

### 6. Commit as QueaT
```bash
git checkout -b feat/<slug>      # never commit on the murmur trunk (block-bash.sh refuses trunk push)
git add -A && git commit -m "<type>(<scope>): <subject>"
git log -1 --format='%an <%ae>'  # MUST be QueaT <kgm004a@gmail.com>
```
**No `Co-Authored-By: Claude` / no Claude trailers.** Conventional-commit style
(`feat`/`fix`/`chore(scope)`), matching the existing log.

### 7. PR to the `murmur` trunk (never direct push)
```bash
git push -u origin feat/<slug>
gh pr create -R murmur-io/murmur --base murmur --head feat/<slug> \
  --title "<type>(<scope>): <subject>" --body "<what + how verified>"
```
`gh` active account MUST be `JakubGawr`. Base is `murmur` (the trunk) via PR — direct
`git push origin murmur` is blocked by the environment guard. If this feature is a release,
hand off to **`/release-murmur`**.

## Rules

- **The verifier, not the author, says "done."** Self-eval over-reports. An independent
  adversarial pass (and a lock-security pass for lock/crypto/visibility changes) is the gate —
  this is the discipline that caught 7 bugs. No green-washing.
- **Conventions are binding** — the Rust (AppError/Result, additive migrations,
  verify-before-destroy, gated reads, crash-safe FFI) and Angular-zoneless (signals/`@if`/
  `toSignal`/no-`setTimeout`/no-new-deps) rules above are non-negotiable.
- **Identity interlocks:** commit author=QueaT (no Claude trailers), gh=JakubGawr, PR base
  =`murmur`, `com.meetnotes.app` immutable.
- **Honest about the signed-build boundary.** A dev run cannot prove Touch ID / lock-at-rest /
  screen-share — say so; only a Developer-ID build verifies them (`/release-murmur`).
- **No new npm packages or crates without explicit user approval.**
