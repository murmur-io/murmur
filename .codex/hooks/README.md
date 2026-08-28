# `.codex/hooks/` — deterministic guardrail adapters

Codex and Claude call the same parser and verifier in
`.agents/h/guard.py`. The shell files here only translate the Codex hook
payload and exit contract; policy must not be reimplemented in an adapter.

## Active guards

| Adapter | Trigger | Enforced behavior |
| --- | --- | --- |
| `block-bash.sh` | Bash | Parses compound commands and common wrappers; blocks protected-branch pushes, agent-shell Keychain operations, inner-loop `cargo clippy --all-targets`, `codesign --deep`, recursive deletion of root/home, and unsupported executable indirection (`env -S`, `eval`, `source`, `exec`, `xargs`, `find -exec`, active shell substitutions). Quoted search text is not treated as a command. |
| `secret-scan.sh` | `git commit` | Scans every staged added line, including lockfiles and hook sources, for private keys, provider tokens, and DEK/KEK material. |
| `finish-guard.sh` | `git commit` | Fails closed unless the current linked worktree has a matching harness task and a schema-valid PASS attestation bound to the exact contract, instructions, dependencies, staged binary diff, checks, fresh reviewer sessions, and required risk reviews. |
| `autoformat.sh` | edits | Optional single-file Rust formatting when `MURMUR_AUTOFMT=1`. |

The authoritative task state is under the shared Git common directory:
`.git/h/<task-id>.json`. Task discovery matches the current linked
worktree; there is no concurrency-unsafe global current-task pointer.

## Verification

```bash
bash .codex/hooks/selftest.sh
.agents/h/mirror-check
```

The selftest runs the same canonical implementation against both Codex and Claude
payload shapes. The config audit proves both clients are wired to it and that the
harness schemas/configuration still match.

`MURMUR_FINISH_GUARD=enforce` is the repository default. It may be set to
`advisory` only for an explicit diagnostic rollout; CI treats advisory wiring as a
configuration failure. `MURMUR_ALLOW_SECRET=1` is a deliberate one-shot local
override and must never be stored in project configuration.

## Adding a guardrail

1. Change `.agents/h/guard.py`, not the vendor adapters.
2. Add a RED assertion for the bypass plus an ALLOW assertion for the closest safe
   command, then make them GREEN.
3. Run both commands above. Keep hooks fast; expensive proof belongs to the task
   runner or `scripts/ci.sh`.
