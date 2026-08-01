# Murmur development harness

The Harness is an opt-in, verifier-only control plane for risky or multi-step
changes. It creates one isolated worktree, derives checks and reviews from the
exact diff, records resumable evidence, writes the guarded commit, and archives
the task during cleanup.

It does not dispatch an implementation model and has no automatic repair loop.
The developer edits the worktree; a fresh reviewer owns the verdict.

```text
open -> edit isolated worktree -> plan -> verify/resume
     -> PASS -> commit -> push/PR/CI/merge -> clean
```

## Daily workflow

Run orchestration from the standalone driver clone
`../.murmur-agent-driver`, not the user's primary checkout:

```bash
scripts/agent-harness open attachment-loss \
  --kind bug \
  --prompt "Fix attachment loss after closing a note" \
  --owned src-tauri/src/storage/attachment_store.rs \
  --owned src-tauri/src/commands/attachments.rs
```

`open` prints the isolated task worktree. Implement only there and only in the
declared paths. Then use that worktree's checked-in runner:

```bash
scripts/agent-harness plan attachment-loss
scripts/agent-harness verify attachment-loss
scripts/agent-harness status attachment-loss

# Only if verification paused or evidence is incomplete:
scripts/agent-harness resume attachment-loss

# Only after PASS:
scripts/agent-harness commit attachment-loss \
  -m "fix(attachments): preserve files when closing a note"
```

Keep the task through push, PR checks, and merge. Afterwards:

```bash
scripts/agent-harness clean attachment-loss
```

To abandon a task, use `clean <task-id> --abandon`. Cleanup first archives every
Git-visible task byte and only then removes that task's worktree and branch.

## What is automatic

The current changed paths select canonical checks and reviews:

- Rust source/manifests: `cargo test --lib`.
- Angular source: lint and build.
- Browser behavior: Playwright.
- Sharing protocol: client and pinned server protocol tests.
- Lock, egress, and protocol paths: mandatory specialist review.
- Runtime and performance: explicit `--claim runtime|performance`.

The behavioral prompt cannot add shell commands. The derived plan is the sole
executable evidence profile. Reviewers are fresh, read-only, and tool-free.
They may request only a typed, allowlisted probe that the runner executes.

`review_authority` in `config.json` decides which review can forbid a PASS. A
kind set to `advisory` still runs, and its findings, proof gaps, and probe
requests are still recorded in the receipt, but on any plan that keeps another
gate they no longer gate the verdict and no longer spend a probe execution. Any
unconfigured or unknown review kind is blocking.

**All four planned kinds currently ship `blocking`,** including the `combined`
generalist, and `scripts/agent-config-audit` pins that. The generalist was
briefly demoted on the corpus measurement in
`docs/research/2026-08-01-reviewer-corpus-measurement.md`; that measurement
ranked reviewers by BLOCKER count, while the gate that forbids a PASS reads
`verifier.SEVERE_FINDINGS` (`MAJOR` + `BLOCKER`). Re-counted on that metric over
the same 232-review corpus, the generalist had the highest density of
PASS-forbidding findings, not the lowest — 116 over 105 reviews (1.10 each)
against 0.41 for `egress-security` and 0.13 for `lock-security` — so the
demotion was reverted. The demotion mechanism below stays live and tested on its
own config fixtures; changing the shipped decision means editing both the config
and its pin.

Demotion removes a gate; it must never remove the last one. Every PASS names at
least one gate that could have refused it, so `verifier.gating_review_kinds`
skips the demotion for a plan that derived no deterministic check and no
configured blocking review — docs-only, asset-only, and landing-only diffs,
whose only planned review is the generalist. There the generalist still gates,
spends its probe, and can still refuse. The receipt gate re-derives that same
set from the exact paths and the attested config, so a re-hashed `PASSED` on an
ungated plan is refused by the rule that produced it.

Findings from a demoted review are recorded in `evidence.advisory_findings`,
projected into task state, printed by `status`, carried in the `verify` status
JSON, and counted in the PASS reason, which then reads `all blocking checks and
reviews passed; N advisory finding(s) recorded (M MAJOR/BLOCKER)` instead of
claiming every review passed.

Green checkpoints for an unchanged exact diff survive interruption.
`NEEDS_FIX` means edit the worktree and verify the new diff.
`NEEDS_EVIDENCE`, `PAUSED_RETRYABLE`, and `INTERRUPTED` resume without throwing
away completed evidence.

## Protected control-plane changes

The Harness cannot certify changes to its own protected files. For
`.agents/harness`, hooks, rules, skills, CI, or receipt policy:

1. create a dedicated worktree outside the runner-owned
   `../.murmur-agent-tasks` root, for example
   `../.murmur-control-plane/<task-id>`;
2. run the complete control-plane selftests;
3. obtain a fresh independent review;
4. land through the base-anchored GitHub CI gate.

This is an explicit trust-boundary exception, not a weaker self-receipt.

## Commands

```text
open      create the isolated task
plan      print and bind the exact-diff evidence profile
verify    run or resume checks and fresh reviews
resume    continue missing/retryable evidence
status    inspect state and lock ownership
commit    create the exact PASS receipt commit
clean     archive and close/abandon the task
doctor    audit dependencies, ghosts, and orphan worktrees
metrics   summarize event ledgers plus reviewer PASS-rate and cost per accepted task
selftest  run lifecycle, fault, and metrics tests
```

There is no executable Harness v1. `scripts/verify-harness-attestation` retains
read-only support for historical v1 commit trailers so old Git history remains
auditable.

### `metrics` outcome tables

`metrics` reads only what the runner already wrote. It walks every ledger, and on
each `review-checkpoint` it loads the record at `record_path` and joins it to that
attempt's `checks/*.json`. The record — not the checkpoint event — carries
`duration_ms`, `result.findings`, `result.proof_gaps`, `vendor`, and
`attempts[].telemetry.usage`, so the record side is authoritative for review
outcomes. The event-derived `models:`/`reviews:` lines above them count model
invocations instead, including any that never produced a record; the two are
deliberately not the same number.

```bash
# this repo's own store
scripts/agent-harness metrics
# the driver clone that actually ran the reviews (repeatable; --limit is per run,
# and the default of 20 silently truncates a large store)
scripts/agent-harness metrics --limit 200 \
  --store ../.murmur-agent-driver/.git/agent-harness
```

`--store` accepts a `.git` dir, the `agent-harness` store, its `v2` dir, or the
task root itself, and reports what each argument resolved to — an unresolvable
store prints `UNRESOLVED` rather than contributing a silent zero, and a store
that resolves to an already-counted task root prints `DUPLICATE (already
counted)` and contributes nothing. That last case is easy to hit: run the
command above *from* the driver clone and `../.murmur-agent-driver/.git/...`
resolves back to the store `repo_context` already supplied, which would double
every absolute count while leaving every rate identical. Records are deduped by
`(store, task, attempt, reviewer)`: the corpus contains checkpoints that fire
twice against the same rewritten record, and counting events would double their
tokens and minutes.

`tokens` are billable tokens, and what counts as billable is decided per **usage
dialect**, not per field name — the two vendors disagree about what
`input_tokens` means:

| dialect | detected by | billable |
| --- | --- | --- |
| Anthropic | `cache_read_input_tokens` / `cache_creation_input_tokens` | `input + cache_read + cache_creation + output` (disjoint counters) |
| OpenAI | `cached_input_tokens` / `cache_write_input_tokens` | `input + output` (`cached` is a *subset* of `input`) |

A cached Anthropic prompt lives almost entirely in the cache pair: across the 31
`claude` reviews in the corpus, `input_tokens` totals **70** against 2.70M cache
tokens, so billing `input + output` alone scored them at 25% of what they
consumed while `codex` was billed at ~100% — enough to invert a cross-vendor
comparison. The dialect is read off the usage keys (with `vendor` only as a
fallback) and reported as `token dialects` so the normalization is visible.
`reasoning_output_tokens` is never billable in either dialect: it is a *subset*
of `output_tokens`, strictly smaller in all 199 corpus records that report it.
Every column carries its own coverage, and a pruned record stays `missing`
rather than becoming a zero.

`observed USD` is the vendor's own `attempts[].telemetry.cost_usd`, summed and
reported with coverage. It is measured rather than derived, so it beats any rate
card where it exists — today that is every `claude` record and no `codex` one.

The rate card is opt-in and never inferred. Add an optional top-level `pricing`
block to `.agents/harness/config.json` and the `rate-card USD` columns appear;
omit it and they do not exist at all:

```json
"pricing": {
  "codex":  { "input_per_mtok": 0.0, "output_per_mtok": 0.0 },
  "claude": { "input_per_mtok": 0.0, "output_per_mtok": 0.0 }
}
```

Rates are keyed by **vendor**, not model: every `codex` record in the corpus
carries `"model": "unspecified"`. They price exactly the tokens `tokens` counts,
which makes them a coarse *upper* bound for the Anthropic dialect (cache reads
bill at the fresh-input rate) — prefer `observed USD`. No block ships, because
the harness has no authoritative price to state.

## Self-verification

```bash
scripts/agent-harness selftest --ci
scripts/verify-harness-attestation --selftest
bash .codex/hooks/selftest.sh
scripts/agent-config-audit --ci
```

These prove the control plane, not Murmur product behavior. Real runtime,
signed-build, privacy, and content-loss claims still need their corresponding
application evidence.
