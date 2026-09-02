---
name: ship-feature
description: Ship a Murmur feature through scope, isolated implementation, fresh adversarial and required security review, project gates, a JakubGawr commit, and a PR to `murmur`. Use the verifier-only Harness for risky, multi-step, or explicitly requested work; ordinary low-risk fixes keep the normal isolated-worktree route.
---

# /ship-feature — ship a Murmur feature end-to-end

You are shipping a change into **Murmur** (Rust crate `murmur` / bin `Murmur` / lib
`meetnotes_lib` + Angular 22 zoneless). The job is not "write code" — it is "land a
**verified** change behind the project's gates and identity rules." The non-obvious value
here is **adversarial verification**: the team distrusts AI-written, self-certified code, and
this loop's adversarial pass is what previously **caught 7 bugs** that the implementing agent
believed were already correct. Self-eval is systematically over-positive — an independent
verifier, not the author, decides "done."

## The pipeline (stages — each gates the next)

### 1. Scope
Restate the change in one sentence. Classify which layer(s) it touches — this picks the
builder and whether the lock-security review is mandatory:
- **Backend** — `src-tauri/src/`: commands (`commands/mod.rs` plus domain modules, registered in the
  `generate_handler!` in `lib.rs`), state (`state.rs`: `AppState` = db / unlocked_folders /
  master_kek), storage (`storage/{db.rs,*_store.rs,migration.rs,models.rs}`), crypto (`crypto.rs`),
  secrets (`secrets/keychain.rs`), audio/transcribe/summarize/mcp/pipeline.
- **Frontend** — `src/app/`: features (`features/{record,detail,library,folders,ask,graph,
  settings,onboarding,bar,analytics}`), services (`core/ipc.service.ts`, `folders.service.ts`,
  `toast.service.ts`, `screen-share.service.ts`), `core/models.ts`.
- **Lock / crypto / visibility-gated** — ANY change to `crypto.rs`, `secrets/keychain.rs`,
  `screenshare.rs`, `storage/migration.rs`, the unlock/seal/visibility path,
  or content-read gating ⇒ the **lock-security review is MANDATORY** in stage 4.

### 2. Choose the smallest honest route

The harness is opt-in. A small docs/chore/mechanical/low-risk fix uses a normal
isolated feature worktree, fresh independent review, the relevant project gates,
and a normal JakubGawr commit. Do not add a Harness receipt merely because this
skill was invoked.

Use the Harness for lock/crypto/egress/protocol work, multi-step changes, or
when the operator explicitly requests the receipt. It must not own protected
control-plane paths. Change those in a dedicated worktree outside the
runner-owned `../.murmur-agent-tasks` root, for example
`../.murmur-control-plane/<task-id>`. Run the complete control-plane
selftests, obtain a fresh independent review, and rely on the base-anchored
CI gate.

### 2a. Create the v2 executable task contract when that route applies
For a multi-part change, write a short plan: the exact files/symbols, the IPC seam (new Tauri
command name + the `ipc.service.ts` call), data shape (`core/models.ts` ↔ Rust DTO), and the
behavioral outcomes and invariants the implementation must satisfy. Do not put
imperative check commands or requests to report command output in the contract:
the derived plan is the sole executable evidence profile. Open Harness v2 with
explicit owned paths and only real runtime or performance claims:

```bash
scripts/h run <task-id> \
  --prompt "<scope and acceptance criteria>" \
  --owned <path> [--owned <path> ...] \
  [--claim <runtime|performance>] [--reviewer <codex|claude>]
```

Run this from a dedicated standalone driver clone, not the shared primary
checkout or a linked driver worktree. The harness creates and prints the
isolated worktree. Assign exactly one implementer to that worktree, then use
that worktree's own runner. V2 does not dispatch a writer or repair code.
The current exact diff derives canonical checks and actual lock/egress/protocol reviews, so do not
substitute caller-selected risk labels or weaker commands. Keep the contract honest about what a
dev run cannot prove (Touch ID / lock-at-rest / screen-share need a signed build).

### 3. Build — to the project conventions (non-negotiable)
Dispatch implementation to the matching custom role (`rust-tauri-dev` and/or
`angular-zoneless-dev`) and keep the verifier in a fresh, separate session.
Iterate with `/tauri-dev` (`MURMUR_DEV_DEK` recipe, `cargo test --lib` loop).

**Prior lessons are executable input.** Read the binding `.codex/rules/` file for each surface
before editing and apply relevant curated `## Recurring patterns` from `.claude/learnings/`.
Harness v2 verifies the resulting diff; it does not inject implementation context or repair it.

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
`afterNextRender()`/`afterRenderEffect()` (NEVER `setTimeout` in components); **directory
per component with split `ts` + `html` + `scss` files** (`templateUrl`/`styleUrl` — no inline
template/styles); **Liquid Glass design language** for every new view (glass tokens, aurora
ambient, neutral chrome — see `angular-zoneless.md` §6b); `var(--token)` CSS with every
variable living in `src/design-tokens/` (a missing value = add a token there, incl. its light
override — never a raw hex/px in component scss); reusable/atomic components belong in
`src/app/design-system/` under the `mur-` prefix (form controls as CVAs); ≤16 kB
per-component style budget; inline SVG icons or `<mur-icon>`; **no new npm packages without
explicit user approval.** Banned:
`markForCheck`, `@Input`/`@Output`/`EventEmitter`, `@ViewChild`, `BehaviorSubject`-as-state,
constructor injection.

### 4. ADVERSARIAL verify (the part that caught 7 bugs)
The author does **not** self-certify. On the v2 route, inspect the plan and run
the verifier:

```bash
```

The fresh combined reviewer checks both scope/spec fidelity and adversarial correctness. Its job is
to make the exact diff FAIL:
- For each load-bearing claim ("it locks", "it's gated", "the migration is safe", "the signal
  updates"), ask **"what would make this false?"** and try it.
- Demand evidence from the real seam, not merely mocks. The v2 reviewer itself
  is tool-free: it receives the immutable runner-built bundle and has no
  filesystem or shell access. It may request only a typed runner-owned probe.
  Live `/tauri-dev`, IPC, log, or browser evidence is produced outside that
  reviewer session and named honestly.
- Check the **negative**: a locked/sealed meeting leaks nothing through detail, segments,
  timeline, audio, OR MCP; an additive migration leaves existing rows intact.
- **If the change touches lock/crypto/visibility (stage 1), run the lock-security review TOO**
  — specifically: verify-before-destroy actually holds (no plaintext blanked pre-verify),
  every read path is visibility-gated, keychain ACL / `com.meetnotes.app` continuity
  untouched, no DEK/KEK/plaintext in logs, biometric gate not bypassed outside the dev hatch.

A verifier finding sends it BACK to stage 3; rerun `verify` on the new exact diff. Do not advance
on the author's say-so. Actual lock/egress/protocol paths add the corresponding cross-vendor
specialist automatically.

For a bug fix, require a focused regression test and the green language suite.
Do not demand a prose reconstruction of historical RED from the developer. Empirical
RED is required only when a runner-owned artifact actually performed and
recorded that proof.

On the normal low-risk route, dispatch a fresh read-only adversarial verifier
outside the author session and retain its concrete verdict in the PR handoff.

**The runner records the verdict; reviewers do not write their own PASS files.** V2 evidence lives
under `.git/h/<task>.json`. It binds contract,
base, exact binary diff/tree, plan, protocol, check/probe artifacts, reviewer invocation metadata,
findings and telemetry. Any edit or protocol drift invalidates PASS. A reviewer may request only a
typed allowlisted probe; arbitrary shell access is forbidden.

### 5. Selected gates green; integration stays remote

On the v2 route, do not manually rerun checks that the exact-diff plan already
recorded. A changed diff gets a new plan and `verify`; an unchanged PASS proceeds
to commit and PR.

On the normal low-risk route, run each relevant local gate once:

- Rust source: `scripts/agent-resource-run --chdir src-tauri -- cargo test --lib`.
- Angular source: `npx ng lint` and
  `scripts/agent-resource-run -- npx ng build`.
- Behavioral UI work: the relevant Playwright smoke.

The full `scripts/ci.sh` is the GitHub PR/release-parity integration gate, not a
second local repair-loop pass. Run it locally only when the operator explicitly
asks or as release preflight. (`clippy --all-targets` belongs inside `ci.sh`,
never the inner loop.)

### 5b. Extract the lesson (close the loop)
If verification caught anything real — or the run confirmed a non-obvious approach that worked —
append ONE `## Run journal` entry to the relevant `.claude/learnings/<agent>.md` (or run
`/learn <agent>: <lesson>`), citing the artifact that revealed it. Periodically `/curate-learnings`
promotes repeat offenders into `## Recurring patterns`. This is what makes "every bug a permanent
lesson" instead of a re-paid one.

### 6. Commit as JakubGawr
```bash
# V2 route: commit through the runner; its durable intent survives a crash.
git -C <worktree> commit -m \"<type>(<scope>): <subject>\"
git -C ../.murmur-agent-tasks/v2/<task-id>/meetnotes log -1 --format='%an <%ae>'
# MUST be JakubGawr <63911380+JakubGawr@users.noreply.github.com>
```
Never use `git add -A` in a shared repository. **No AI co-author trailers.** Conventional-commit style
(`feat`/`fix`/`chore(scope)`), matching the existing log.

For the normal low-risk route, stage only the explicit owned files and create a
normal JakubGawr commit without Harness trailers. Use a conventional `fix/<slug>`,
`feat/<slug>`, or `chore/<slug>` branch so the remote `agent/*` receipt gate does
not mistake it for a harness task. If an operator deliberately keeps a
non-harness commit on an `agent/*` branch, the PR description must declare the
explicit Lane-B handoff on its own line:

```text
```

Lane B is valid only before any receipt exists on an ordinary non-v2
`agent/*` branch. Never use it on `agent/v2/*`, after any receipt, or to
paper over a failed receipt check; repair or re-verify the receipted lifecycle
instead.

### 7. PR to the `murmur` trunk (never direct push)
```bash
git -C ../.murmur-agent-tasks/v2/<task-id>/meetnotes push -u origin agent/v2/<task-id>
gh pr create -R murmur-io/murmur --base murmur --head agent/v2/<task-id> \
  --title "<type>(<scope>): <subject>" --body "<what + how verified>"

# Keep the task/worktree through push, PR creation, and CI. After the PR merges
# (or the operator explicitly accepts an archived handoff), clean it:
scripts/h clean <task-id>
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
- **Identity interlocks:** commit author=JakubGawr (no AI trailers), gh=JakubGawr, PR base
  =`murmur`, `com.meetnotes.app` immutable.
- **Honest about the signed-build boundary.** A dev run cannot prove Touch ID / lock-at-rest /
  screen-share — say so; only a Developer-ID build verifies them (`/release-murmur`).
- **No new npm packages or crates without explicit user approval.**
