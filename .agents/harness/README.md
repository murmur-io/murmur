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
  --agent codex \
  --reviewer claude \
  --prompt "Fix attachment loss after closing a note" \
  --owned src-tauri/src/storage/attachment_store.rs \
  --owned src-tauri/src/commands/attachments.rs \
  --owned src-tauri/src/commands/tests/attachment_tests.rs \
  --risk lock \
  --check 'rust::(cd src-tauri && cargo test --lib)' \
  --final-check 'ci::MURMUR_CI_SKIP_E2E=1 bash scripts/ci.sh'

scripts/agent-harness run attachment-loss
scripts/agent-harness status attachment-loss
```

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

The default loop permits two repair rounds. The default reviewer is the other vendor. A task becomes `PASSED`, `FAILED`, or `BLOCKED`; it never retries forever.

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
creates one commit from exactly the attested index. `verify-attestation` and the defense-in-depth
commit hook fail after any post-review edit. If a manual commit is ever needed, use
`git -C <printed-worktree> commit ...` so the hook can resolve the task explicitly. `close` is
deliberately strict: it accepts one clean commit whose parent is the recorded base and whose tree is
byte-for-byte the attested tree, removes only the two task worktrees, and preserves the branch and
evidence.

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

## Enforcement

- Claude/Codex hooks are fast defense-in-depth and call one canonical implementation.
- The canonical commit hook verifies the exact staged diff against the task attestation.
- `scripts/agent-config-audit` and `scripts/agent-harness selftest` run at the start of `scripts/ci.sh`.
- GitHub required checks remain the authoritative remote merge boundary; local hooks are bypassable by design.

Audit that boundary without mutating GitHub:

```bash
scripts/agent-remote-audit
```

The repository policy is versioned in `remote-policy.json`. A remote FAIL means the development loop is locally functional but merge enforcement is not complete; changing branch protection or secret scanning is an explicit operator action.

No production vault, MeetingNotes database, Keychain item, live microphone, ScreenCaptureKit session, or real cloud provider belongs in an automated trial. Signed-build-only behavior stays an explicit human/runtime gate.
