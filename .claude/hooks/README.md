# `.claude/hooks/` — deterministic guardrails

Prose rules (CLAUDE.md, `.claude/rules/`) are **advisory**: an LLM interprets them at
runtime and can be argued out of them under context pressure. These hooks are the layer
it **cannot** talk its way past — they pattern-match the command/diff and return exit 2 to
block. Every guardrail here encodes an incident that has already cost the project.

Wired in [`.claude/settings.json`](../settings.json). Contract for every hook: read the tool
JSON on stdin, `exit 0` = allow, `exit 2` + stderr = block (stderr is shown to the model as
the reason).

> History note: CLAUDE.md and the rules cited a `block-bash` hook for months — but the file
> never existed (a docs-drift bug in the very layer whose motto is "trust code, not docs").
> This directory makes the claim real. `selftest.sh` exists so it can never go phantom again.
> Full audit: `docs/research/2026-07-02-claude-setup-audit.md`.

## The hooks

| Hook | Event | What it does | Default |
| --- | --- | --- | --- |
| `block-bash.sh` | PreToolUse(Bash) | Blocks: direct push to the `murmur`/`main`/`master` trunk (incl. force); the macOS `security`/keychain CLI + `notarytool store-credentials` (hang on the auth dialog → runaway procs); `cargo clippy --all-targets` (openssl/sqlcipher profile thrash); `codesign --deep` (skips nested Resources/ helpers → notarization Invalid); `rm -rf /` \| `$HOME`. | **active** |
| `secret-scan.sh` | PreToolUse(Bash) | On `git commit`, scans the **staged additions** for PEM keys, vendor tokens (`sk-ant-`, `ghp_`, `AKIA…`, Slack `xox…`, `sk-…`), and 64-hex DEK/KEK material. Excludes lockfiles + `.claude/hooks/` (known secret-shaped fixtures) and the documented dev DEK. Override a false positive with `MURMUR_ALLOW_SECRET=1`. | **active** |
| `finish-guard.sh` | PreToolUse(Bash) | On `git commit` for an active task (`.claude/tmp/<task>/`), requires `adversarial-verify.json` (+ `lock-security.json` if `.lock-touched`) with `verdict: PASS`. Turns the binding DoD ("the verifier owns the verdict") into a machine check. | **advisory** (`MURMUR_FINISH_GUARD=enforce` to block) |
| `autoformat.sh` | PostToolUse(Edit\|Write) | `rustfmt` the edited `.rs` file. FE formatting stays with `npx ng lint` (the zoneless rules — dir-per-component ts/html/scss, per-component style budget — a blind prettier pass would fight them). | **off** (`MURMUR_AUTOFMT=1` to enable) |

Helper: [`../lib/trace-span.sh`](../lib/trace-span.sh) appends one JSONL observability span per
gate to `.claude/tmp/<task>/trace.jsonl` (best-effort, never blocks).

## Verify they still enforce

```bash
bash .claude/hooks/selftest.sh    # exit 0 = all guardrails block as expected
```

This is the meta-test (the "test the workflow itself" pattern). Run it standalone or from
`scripts/ci.sh`; if a guardrail stops enforcing, it goes red.

## Toggles (env, defaulted in `settings.json`)

- `MURMUR_FINISH_GUARD` = `advisory` (default) | `enforce`
- `MURMUR_AUTOFMT` = `0` (default) | `1`
- `MURMUR_ALLOW_SECRET=1` — one-shot escape hatch for a `secret-scan.sh` false positive.

## Adding a guardrail

1. Write the check the incident-avoidance way: **pattern-match the string, don't infer intent**;
   keep it under ~200 ms; prefer a **narrow allow-list of subcommands** over a broad word match
   (see how `security` is matched only before its keychain subcommands, so `pkill security` passes).
2. Add BLOCK **and** ALLOW assertions to `selftest.sh` (RED-before-GREEN: prove it blocks the bad
   case and lets the good case through).
3. Roll out **advisory first** (warn, exit 0) for anything with false-positive risk; flip to block
   once you've watched it. Reserve CLAUDE.md for workflows/principles; reserve hooks for
   technical enforcement boundaries.
