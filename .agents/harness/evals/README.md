# Development-agent evals

This suite measures the complete development envelope: model, CLI version, prompt, repository
instructions, tools, timeout, and repair policy. It does not evaluate Murmur's in-product models.

```bash
scripts/agent-harness eval list --suite smoke
scripts/agent-harness eval doctor
scripts/agent-harness eval selftest          # fake agents only
scripts/agent-harness eval run --suite smoke --agent codex --trials 1
scripts/agent-harness eval run --suite smoke --agent claude --trials 1
```

Normal trials are single-shot. Use `--repair-rounds N` only for a separately reported repair-mode
experiment. Use `--model ID` when comparing pinned models. Every invocation has a wall timeout.
Agent and grader processes receive a minimal environment allowlist. Ambient API keys, tokens,
passwords, cloud credentials, development DEKs/KEKs, and SSH agent sockets are not inherited; use
the Codex/Claude CLI credential store for authentication. Traces record only names of stripped
variables, never their values.

## Task contract

Tasks live in `tasks/*.json` and suites only list task IDs. A task declares:

- `source`: a minimal fixture, or a committed repo revision exported with `git archive`;
- `allowed_paths`: the hard write boundary;
- `expected_change`: `false` for analysis and valid no-op tasks;
- one or more hidden deterministic graders;
- optional fake good/bad overlays used exclusively by harness selftests.

Fixture initial state is the only eval content copied into a fixture workspace. Repo trials contain
the committed snapshot but no `.git`, history, task manifests, fake solutions, or graders. Graders
run from `graders/`, outside the disposable trial workspace.

## Results

The taxonomy is `PASS`, `AGENT_FAIL`, `SCOPE_FAIL`, `HARNESS_FAIL`, `TIMEOUT`, and aggregate
`FLAKE`. Reports include pass-at-1, any-pass-at-k, all-pass-at-k, false-green count/rate, duration,
and per-task outcomes. The trace bundle stores prompts, raw CLI streams, invocation metadata,
before/after hashes, changed-file copies, grader contexts/results, and the final report under the
repository's shared Git directory (`.git/agent-harness/evals/`) unless `--output-dir` is supplied.

`PASS` comes only from the runner after deterministic grading. An agent's zero exit code is merely
a claimed success and counts as false-green when the graders reject it.
