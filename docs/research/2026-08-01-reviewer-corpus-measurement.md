<!-- Measured 2026-08-01 from the harness evidence store at ../.murmur-agent-driver/.git/agent-harness. Method: parse every `review-checkpoint` event across 348 events.jsonl ledgers, follow each `record_path`, and join the review verdict to the deterministic-check outcomes of the same attempt. No sampling — the whole corpus. -->
# Reviewer corpus measurement — is the blocking LLM review earning its place?

> **The single highest-leverage unmeasured question in the harness**, asked in
> `2026-08-01-harness-plan-verification.md` §7 as U10 and left open there because
> "only Murmur's own corpus can settle it". This settles it. The data was already
> on disk; no new instrumentation was written.

## Verdict

**Split the answer by reviewer.** The two specialists earn their blocking authority; the
generalist does not.

| Reviewer | Reviews | PASS rate | Findings | of which BLOCKER | Model time | Share of time |
|---|---:|---:|---:|---:|---:|---:|
| `combined` (generalist) | 98 | **26%** | **333** | **1** | **226.9 min** | **76%** |
| `egress-security` | 65 | 69% | 17 | 5 | 38.8 min | 13% |
| `lock-security` | 53 | 75% | 11 | 6 | 33.0 min | 11% |
| **total** | **216** | 51% | 361 | 12 | **298.8 min** | 12.47M tokens |

- The **generalist** emits **3.4 findings per review, 73% of them INFO or MINOR** (122 INFO +
  120 MINOR of 333), and produced **one BLOCKER in 98 reviews**. It consumes three times the
  model time of both specialists combined and refuses 74% of attempts.
- The **specialists** emit **28 findings across 118 reviews**, of which **11 are BLOCKER** — a
  signal density two orders of magnitude higher, at a quarter of the cost.

This is the risk-triggered-specialist design working, and the generalist diluting it.

## The finding that decides it

Every non-PASS review in the corpus — **106 of 216 (49%)** — occurred in an attempt where
**every deterministic check was green**. So the reviewers are not duplicating the gates; they
fire on a disjoint class. The question was only ever whether what they find is real.

A verbatim `lock-security` BLOCKER, against `scripts/harness-runtime-smoke.py`:

> `ExternalEgressObserver.__init__` binds a new TCP listener to `(127.0.0.1, 0)`, while `_run`
> accepts every connection and reads up to 4096 bytes **without authenticating the caller**. The
> random port and private task directory are not an authorization check at the changed network
> sink. The health path only classifies a request after receipt; it does not authorize access.

No lint, type check, unit test or build catches an unauthenticated loopback sink. That is the
class of defect a specialist reviewer exists for, and there are eleven of them in the corpus.

## Consequences for the plan

1. **Keep `lock-security` and `egress-security` BLOCKING.** Measured on Murmur's own corpus, not
   inferred from external precision studies: 11 BLOCKERs in 118 reviews, all on green checks.
2. **Demote `combined` to advisory.** One BLOCKER in 98 reviews for 76% of the review budget, and
   a 74% refusal rate, is the documented over-triggering failure mode — a gap-hunting reviewer
   manufactures findings, and chasing all of them causes over-engineering. Advisory keeps the
   signal and removes the block.
3. **Expected saving: ~227 minutes of model time per corpus-equivalent volume**, plus the rework
   the 242 INFO/MINOR findings induced. The two specialists cost 72 min combined and stay.
4. **`proof_gaps` are dominated by the same reviewer** — 148 of 215, against 37 and 30 for the
   specialists. A proof gap blocks a PASS, so this is where the "eleven task ids for one feature"
   escalation pressure was manufactured.

## Correction to `2026-08-01-harness-plan-verification.md` §7

That document states: *"216 review model calls were made across the corpus and **none records a
duration** — `review-checkpoint` events carry `review_kind` and `verdict` but no `duration_ms`."*

**That is wrong.** The `review-checkpoint` event carries a `record_path`; the record it points at
carries `duration_ms`, `attempts[].duration_ms`, `attempts[].telemetry.usage` (input, output,
cached and reasoning tokens), `vendor`, `model`, and `cli_version`. Every number in the table
above was read from those records. Review cost was always measurable; nothing had ever read it.

The generalist's duration distribution is itself a signal: median **61.6 s**, max **591.8 s** —
a ~10× spread that the specialists do not show (max 65.5 s and 67.7 s, both close to their
medians). A reviewer whose runtime varies by an order of magnitude on a bounded diff bundle is
not doing a bounded amount of work.

## Method

```python
# every review-checkpoint event in every ledger, joined to its attempt's check outcomes
for ledger in (evidence_store).rglob('events.jsonl'):
    for event in ledger:
        if event['event'] == 'review-checkpoint':
            record  = json.load(open(event['record_path']))
            checks  = attempt_dir(record)/'checks'/'*.json'
```

No sampling: 348 ledgers, 216 review checkpoints, 100% of `record_path`s resolved. The corpus
spans 2026-07-28 → 2026-07-31 and both vendors (`codex` and `claude`).

### This is now a command, not an archaeology expedition

The join above shipped as `scripts/agent-harness metrics --store <evidence-store>`
(`.agents/harness/metrics.py`, `_review_rows` → `_review_outcomes` / `_task_outcomes`). Re-running
it against the same store with the corpus restricted to `created_at < 2026-08-01` reproduces this
table cell-for-cell for `egress-security` (65 / 69% / 17 findings / 5 BLOCKER / 38.8 min / 13%) and
`lock-security` (53 / 75% / 11 / 6 / 33.0 min / 11%), and reproduces the headline **76%**
generalist share of model time. Three arithmetic corrections fall out of that replay:

1. **216 is an event count; there are 214 distinct reviews.** Two `combined` checkpoints —
   `continuity-research-docs-20260728` and `dependency-rollup-v2-final-r2-20260728` — fire twice
   against the same rewritten record. They contribute exactly the 2 reviews, 8 findings and 5.4
   minutes by which this table exceeds a record-deduplicated count. The command deduplicates by
   `(store, task, attempt, reviewer)` and prints how many events it skipped.
2. **12.47M double-counts reasoning tokens.** `12,474,552` is
   `input + output + reasoning_output_tokens` over the 216 events. `reasoning_output_tokens` is a
   *subset* of `output_tokens` — measured strictly smaller in all 199 corpus records that report
   it — so it must not be added.
3. **`input_tokens` is not the prompt in both dialects, and treating it as one under-counted every
   `claude` review by 75%.** Anthropic reports `input_tokens`, `cache_read_input_tokens` and
   `cache_creation_input_tokens` as DISJOINT counters and puts a cached prompt almost entirely in
   the cache pair: across the 31 `claude` records here, `input_tokens` totals **70** — per-record
   values are literally 2 or 4 — against 2.23M cache-creation and 0.47M cache-read tokens. OpenAI
   reports `cached_input_tokens` as a *subset* of `input_tokens`. So billable is per dialect, not
   per field name: `input + cache_read + cache_creation + output` for Anthropic,
   `input + output` for OpenAI (`.agents/harness/metrics.py`, `BILLABLE_LABELS_BY_DIALECT`).
   Corrected billable is **14,305,563** over the 214 distinct reviews at this cutoff.

None of the three moves the verdict; the generalist's share of model time is unchanged. Correction 3
does move the token columns materially, so any per-reviewer token figure predating it is 75% low
for every `claude` row and must not be compared against a `codex` row.

The full store has since grown past this snapshot (232 reviews, 15.54M billable tokens, 343.4 min),
and the growth is concentrated in nine `claude` `egress-security` reviews run on 2026-08-01 that
alone account for 75 of that reviewer's now-92 findings and roughly 40 of its 78.9 minutes. The
vendor split is **not** aligned with the reviewer split — the other 22 `claude` reviews are
`combined` runs from 2026-07-28/29 — so a `combined`-vs-specialist comparison drawn from the
current store is confounded by vendor at both ends; this table is the clean snapshot.

Where a `claude` record carries `telemetry.cost_usd` the command now reports it as `observed USD`.
It is measured rather than derived and needs no rate card: **$44.95** over the 31 `claude` records
in the full store, and it exists on none of the 201 `codex` records.

## What this does NOT establish

- **False-positive rate.** "MAJOR" is the reviewer's own label. Classifying each of the 90
  generalist MAJORs as genuine-vs-manufactured needs a human read of the finding against the diff,
  which this measurement did not do. The BLOCKER count is the conservative floor, and it is what
  the recommendation rests on.
- **Escape rate.** Nothing here says what the reviewers *missed*. Demoting the generalist trades a
  measured cost for an unmeasured change in escapes; the containment is that trunk is not shipped
  — releases are cut manually with the full gate green.
- **Attribution across vendors.** Both vendors are pooled. A per-vendor split is one more `groupby`
  on the same records if it ever matters.
