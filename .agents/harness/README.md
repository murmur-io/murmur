# Murmur development agent harness

This is the vendor-neutral control plane around Claude Code and Codex. It is separate from Murmur's in-product AI evals.

The runner owns the lifecycle:

```text
contract -> isolated worktree -> writer -> deterministic checks -> final checks
         -> fresh spec review -> fresh adversarial review
         -> risk review(s) -> bounded repair (re-runs checks + final checks first)
         -> hash-bound PASS attestation
```

The model never decides that the task is complete. `PASS` belongs to the runner and is valid only for the exact contract, base commit, worktree, staged binary diff, checks, and independent reviews recorded in the attestation. Any later edit invalidates it.

`instructions_sha256` is deterministic: sort the active instruction paths (`AGENTS.md`, `CLAUDE.md`, both clients' rules/agent adapters, canonical `.codex/learnings`, and the harness config/prompts/schemas), then hash each UTF-8 path, a NUL byte, its raw contents, and another NUL byte. Changing instructions creates a new harness variant and invalidates an older task receipt. Each dispatch receives a bounded, role-relevant extract of canonical `## Recurring patterns`; journal history is not injected.

## Daily use

Create an isolated task:

```bash
scripts/agent-harness init attachment-loss \
  --kind bug \
  --prompt "Fix attachment loss after closing a note" \
  --owned src-tauri/src/storage/attachment_store.rs \
  --owned src-tauri/src/commands/attachments.rs \
  --owned src-tauri/src/commands/tests/attachment_tests.rs \
  --risk lock \
  --final-check 'ci::MURMUR_CI_SKIP_E2E=1 bash scripts/ci.sh'

scripts/agent-harness run attachment-loss
scripts/agent-harness status attachment-loss
```

The writer/reviewer pair is configurable per task via `--agent` (writer) and `--reviewer`
(reviewer); both default to `config.json` `default_writer` / `default_reviewer` (shipped:
`claude` / `claude`). Any pair of real vendors is allowed, including same-vendor
(`claude/claude`, `codex/codex`) — the reviewer is always a fresh, independent session with no
writer context, so same-vendor is a procedurally independent adversarial review (though not
model-family-diverse). High-risk paths (`lock`/`egress`/`protocol`) auto-escalate a same-vendor
reviewer to the opposite vendor unless you pass `--allow-same-vendor-high-risk`. The `fake`
adapter is not accepted by public `init`, verification, hooks, or commit; it exists only behind the
in-process deterministic selftest interface.

`init` always starts from a committed `base_sha`; it never copies dirty changes from the primary checkout.

In normal conversation you do not have to type that contract yourself. A request such as
"fix attachment loss and ship it" activates the `ship-feature` workflow; the agent translates
the request into `init`, reports the owned paths and checks, and runs the same state machine. The
CLI remains available when you want an explicit/reproducible experiment or want to choose which
vendor writes and which vendor reviews.

Each task mirrors the real sibling-repository layout under one isolated root:

```text
../.murmur-agent-tasks/<task-id>/
├── meetnotes/       # branch agent/<task-id>; this is where the writer may edit
└── murmur-server/   # detached, clean, read-only-by-contract revision from .murmur-server-revision
```

The primary checkout and the operator's dirty `../murmur-server` checkout are never copied into
the trial. Manifests, logs, model streams, diffs, results, and attestations live under the shared
Git common directory at `.git/agent-harness/`.

UI checks receive a runner-owned `MURMUR_E2E_PORT` reservation for their full lifetime. The
Playwright config propagates that one value to its worker processes and refuses server reuse, so a
test cannot silently attach to another task's Angular server.

Every deterministic check also runs under a fail-closed macOS Seatbelt profile. It receives a
fixed allowlist environment (no ambient tokens/DEKs), can write only the isolated worktree and
explicit task-private runtime/cache leaves, and has no Internet access. The profile starts from
macOS's `allow default` capability baseline and then imposes explicit file/network/process/secret
denials; it is strong file/network containment, not a pure capability allowlist. Contracts, logs,
reviews, and attestations remain parent-**writable** only; checks may read runner inputs needed for
reproducibility but cannot create or alter evidence.
The machine's Cargo registry, advisory database, tool binaries, Rustup toolchains, and the
checksum-verified sherpa-onnx prebuilt archive are exposed read-only; offline Cargo writes and
build artifacts stay inside that one task. Native resolvers may enumerate only the literal ancestor
directories needed to reach an allowed leaf such as shared `node_modules`; those ancestors do not
gain subtree read access.
Playwright, native runtime probes, and Rust test commands get loopback TCP only because the
existing Rust suite owns ephemeral local HTTP listeners; ordinary build/lint checks remain
network-denied. The profile, complete sanitized
environment (keys and values), stdout/stderr and combined log are hash-bound into the attestation.
If `sandbox-exec` is absent,
the task is `BLOCKED`; checks never fall back to an unsandboxed shell.

The default loop permits two repair rounds and has a two-hour task-wide deadline in addition to
per-process timeouts. Two consecutive repair rounds with the same staged diff and the same
failing-check/review signature stop as `BLOCKED/no progress`. Any terminal task stays terminal;
an abandoned nonterminal run lands `BLOCKED/interrupted` instead of silently receiving a fresh
repair budget. Retrying requires a new contract. A failed or
blocked run emits a content-free `learning-candidate.json` for explicit human curation; it never
edits binding learnings automatically. Disposable task caches are pruned immediately after
`FAILED`/`BLOCKED`, while small sandbox profiles and all logs/evidence remain.

Risk evidence is runner-owned. Path classification automatically injects byte-exact canonical
commands from `config.json`; a caller cannot satisfy a lock or egress requirement with a label such
as `rust-lib::true`. Performance-sensitive paths add `perf-contracts`, which checks bounded audio,
spill, inference-lane and thermal lifecycle invariants. Noisy wall-clock/RSS/Metal measurements are
deliberately not PR gates; use the signed-Mac `scripts/measure-recording-ram.sh` protocol for them.

After `PASSED`, commit the exact attested index from the isolated worktree, then close it:

```bash
scripts/agent-harness status attachment-loss   # prints the exact worktree path
scripts/agent-harness verify-attestation attachment-loss
scripts/agent-harness commit attachment-loss \
  -m "fix(attachments): preserve files when closing a note"
git -C ../.murmur-agent-tasks/attachment-loss/meetnotes \
  push -u origin agent/attachment-loss
scripts/agent-harness close attachment-loss
```

`commit` re-verifies the receipt, requires the QueaT identity, rejects AI co-author trailers, and
creates one commit from exactly the attested index (an explicit empty commit for a valid
`--no-expected-change` task). `verify-attestation` and the defense-in-depth
commit hook fail after any post-review edit. If a manual commit is ever needed, use
`git -C <printed-worktree> commit ...` so the hook can resolve the task explicitly. `close` is
deliberately strict: only the runner's `COMMITTED` state is accepted; the commit receipt must match
the exact HEAD, sole parent, tree, message, timestamps, and QueaT author/committer. It then removes
only the two task worktrees, stores the exact task tip under `refs/agent-harness/archive/`, removes
the local task branch, prunes disposable caches, and preserves the evidence.

Changing the executable control-plane itself has one explicit bootstrap because `init` normally
freezes the auto-loaded instruction fingerprint before the writer runs. For a `--kind harness`
task only, an operator may copy a prepared patch into the new isolated worktree and run
`scripts/agent-harness seal-prepared <task-id>` **before any model or check**. The command requires
an actually changed protected path, stages the exact owned bytes, refuses dependency-pin changes,
rebinds the instruction fingerprint once, and writes `prepared.json`. From that point the new
instructions are immutable again and the ordinary writer, checks, fresh independent reviews,
attestation, commit, and close lifecycle is mandatory. Feature/product tasks cannot use this path.

Failed or blocked tasks can be cleaned without losing their work:

```bash
scripts/agent-harness reap attachment-loss
scripts/agent-harness gc --older-than-hours 168 --dry-run
scripts/agent-harness gc --older-than-hours 168
```

`reap` first writes every Git-visible tracked and untracked task byte to a hidden Git archive ref
(ignored dependency caches remain disposable), then removes
only the contract-bound client/server worktrees and local task branch. It refuses dirty sibling
server worktrees. `gc` applies the same operation to old `FAILED`/`BLOCKED`/legacy `CLOSED`
tasks and to stale abandoned `INITIALIZED`/`RUNNING`/`CHECKING`/`REVIEWING`/`REPAIRING`
tasks only after the age cutoff and only when no live task-run lock exists. The locked reaper
revalidates both conditions before converting an abandoned task to `BLOCKED/interrupted`.
When an older runner already lost/removed a task worktree, `gc` cannot invent a code
snapshot; it preserves the existing evidence/state and prunes only disposable runtime caches.
`--dry-run` lists reap and runtime-only targets separately.

For a change that claims native boot behavior, add the exclusive runtime evidence before review:

```bash
--check 'tauri-boot::scripts/harness-runtime-smoke'
```

The smoke uses an isolated temporary home and terminates only the process group it created. If the installed app or another dev task owns `:1420`/`:8765`, it returns `BLOCKED`; it never kills or reuses that process. Touch ID, ScreenCaptureKit/TCC, notarization and real capture still require recorded signed-Mac evidence.

## Agent evaluations

```bash
scripts/agent-harness eval list
scripts/agent-harness eval run --suite smoke --agent codex --trials 1
scripts/agent-harness eval run --suite smoke --agent claude --trials 1
scripts/agent-harness eval report <run-id>
```

Eval trials use disposable history-free snapshots. Hidden graders remain outside the writer's workspace. A normal trial is single-shot; repair rounds are an explicit, separately reported mode. The report includes pass-at-1, any-pass-at-k, all-pass-at-k, duration, failures, scope violations, and harness/infra errors.

Only deterministic harness/eval selftests belong in blocking PR CI. Live Codex/Claude capability
trials are manual or scheduled until a pinned task/configuration has a reviewed reference solution
and repeated near-100% reliability; one stochastic trial must never red-bar a product PR.

## Enforcement

- Claude/Codex hooks are fast defense-in-depth and call one canonical implementation.
- The canonical commit hook verifies the exact staged diff against the task attestation.
- `scripts/agent-config-audit` and `scripts/agent-harness selftest` run at the start of `scripts/ci.sh`.
- GitHub required checks remain the authoritative remote merge boundary; local hooks are bypassable by design.

Audit that boundary without mutating GitHub:

```bash
scripts/agent-remote-audit
```

The repository policy is versioned in `remote-policy.json`. Its merge scope requires an active
ruleset that actually applies to `murmur`, requires the exact GitHub Actions check
`gate (full ci.sh — release parity)` with strict-up-to-date checks, resolves every review thread,
and blocks deletion/non-fast-forward updates. The privileged monitoring scope separately requires
no bypass actors plus secret scanning and push protection.
The approval count is deliberately **zero** while Murmur has one operator: requiring the PR author
to obtain an independent approval would deadlock, not add separation. The no-bypass ruleset plus
the exact CI status is the honest enforceable boundary.

CI runs a live read-only audit first. Pull requests lend only their ordinary, least-privilege
GitHub Actions token and may return `PASS_MERGE_SCOPE` only when every merge-scope control passes.
The three admin-only controls are labeled `MONITOR_ONLY`;
that verdict explicitly does not attest them for the PR. Trusted schedule/dispatch runs require
the privileged audit with the repository-scoped `MURMUR_REMOTE_AUDIT_TOKEN`; a missing or
under-scoped secret fails closed and never falls back to the narrower PR scope. The secret is never
exposed to pull-request code. A token exists only in the `ci.sh` step and is unset immediately
after the audit, before any other repository command. Local runs execute the deterministic
evaluator selftest unless `MURMUR_CI_PUBLIC_REMOTE_AUDIT=1` or privileged
`MURMUR_CI_LIVE_REMOTE_AUDIT=1` is set. A remote FAIL means the development loop is locally
functional but the audited enforcement scope is incomplete; this audit never mutates GitHub.

Control-plane tasks declare the harness and hook meta-selftests as hash-bound checks. Their nested
fake checks inherit the already-applied outer no-network Seatbelt because macOS refuses a second
`sandbox_apply`. Inherited mode is accepted only for fake/selftest receipts and only after
`sandbox_check` proves the current process is kernel-sandboxed; a forged host environment marker
fails closed. Production task checks always record `sandbox_mode: direct`.

No production vault, MeetingNotes database, Keychain item, live microphone, ScreenCaptureKit session, or real cloud provider belongs in an automated trial. Signed-build-only behavior stays an explicit human/runtime gate.
