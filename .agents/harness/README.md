# Murmur development agent harness

This is the vendor-neutral control plane around Claude Code and Codex. It is separate from Murmur's in-product AI evals.

The runner owns the lifecycle:

```text
contract -> isolated worktree -> writer -> deterministic checks
         -> fresh spec review -> fresh adversarial review
         -> risk review(s) -> bounded repair -> final checks
         -> hash-bound PASS attestation
```

The model never decides that the task is complete. `PASS` belongs to the runner and is valid only for the exact contract, base commit, worktree, staged binary diff, checks, and independent reviews recorded in the attestation. Any later edit invalidates it.

`instructions_sha256` is deterministic: sort the active instruction paths (`AGENTS.md`, `CLAUDE.md`, both clients' rules/agent adapters, and the harness config/prompts/schemas), then hash each UTF-8 path, a NUL byte, its raw contents, and another NUL byte. Changing instructions creates a new harness variant and invalidates an older task receipt.

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

The repository invariant is **Claude writer -> Codex reviewer** by default, or the exact reverse
when `--agent codex` is supplied. Writer and reviewer must be different real vendors. The `fake`
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
The machine's Cargo registry, advisory database, tool binaries, and Rustup toolchains are exposed
read-only; offline Cargo writes and build artifacts stay inside that one task.
Playwright and native runtime probes get loopback TCP only. The profile, environment-key set,
stdout/stderr and combined log are hash-bound into the attestation. If `sandbox-exec` is absent,
the task is `BLOCKED`; checks never fall back to an unsandboxed shell.

The default loop permits two repair rounds and has a two-hour task-wide deadline in addition to
per-process timeouts. Two consecutive repair rounds with the same staged diff and the same
failing-check/review signature stop as `BLOCKED/no progress`. Any terminal task stays terminal;
an abandoned nonterminal run lands `BLOCKED/interrupted` instead of silently receiving a fresh
repair budget. Retrying requires a new contract. A failed or
blocked run emits a content-free `learning-candidate.json` for explicit human curation; it never
edits binding learnings automatically.

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
only the two task worktrees and preserves the branch and evidence.

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

The repository policy is versioned in `remote-policy.json` and requires at least one approval plus
the app-bound full gate. CI deterministically selftests the policy evaluator, but the live network
audit stays a separate operator/scheduled preflight because harness checks intentionally have no
Internet and GitHub administration reads need an explicit read-only credential. A remote FAIL means
the development loop is locally functional but merge enforcement is not complete; changing branch
protection, rulesets, or secret scanning is an explicit operator action.

No production vault, MeetingNotes database, Keychain item, live microphone, ScreenCaptureKit session, or real cloud provider belongs in an automated trial. Signed-build-only behavior stays an explicit human/runtime gate.
