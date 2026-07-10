# Agent memory — bitemporal facts + generative-agents consolidation

Deep reference for Murmur's fact store (`facts.rs` / `user_memory.rs`) and the consolidation job
(`memory.rs`). These are the differentiators to PRESERVE, and they are lock-touching — grep the symbols
and read `.claude/rules/lock-model.md` before changing anything here.

## Bitemporal facts — `facts.rs` (the Zep invalidate-not-delete pattern)

A `Fact` carries TWO time axes:
- `valid_from` / `valid_to` — **valid time**: when the fact was true in the world. `valid_to == None` ⇒
  currently valid; `Some(t)` ⇒ closed (superseded) at `t`.
- (transaction/recorded time is tracked alongside for provenance.)

**We never DELETE a superseded fact — we CLOSE it** (`valid_to`), preserving history. `reconcile_facts(existing:
&[Fact], candidates: &[FactCandidate], at: &str) -> Vec<FactOp>` is a **PURE** function (no LLM, no DB, no
clock — `at` is injected), which makes it headless-testable. For each candidate matched by
`(entity_id, norm(subject), norm(predicate))` over the OPEN facts (`valid_to IS NULL` — closed facts are
ignored):
- **no match** → `FactOp::Add` (`valid_from = at`, open);
- **match, SAME object** → no-op (idempotent — re-ingesting the same fact is free);
- **match, DIFFERENT object** → `FactOp::Invalidate { id, valid_to: at }` the old **AND** `FactOp::Add` the
  new (`valid_from = at`, open). The old fact STAYS, closed, so history survives.

`set_meeting_id(ops, meeting_id)` stamps the source meeting AFTER reconcile (the pure core never needs it).
`extract_fact_candidates` pulls candidates from a note (LLM-backed; empty on the stub reasoner). The
`meeting_id` on a fact is the **gating anchor** — a fact learned from a sealed-not-unlocked meeting must be
invisible (see the visibility gates below).

`user_memory.rs` is the PARALLEL store for facts about "me" (there is no entity): `user_facts` table,
`synthesize_brief` renders the injected memory brief, `list_user_facts_visible` / `search_user_facts_visible`
are the gated readers, `purge_user_facts_tx` purges on seal. User facts and entity facts never cross:
an entity read can never surface a user fact and vice-versa.

## Consolidation — `memory.rs` (L2.1, the generative-agents recipe)

`run_consolidation_pass(...)` (driven hourly by `consolidation_tick`, `CONSOLIDATION_INTERVAL_SECS = 3600`):

1. **Score** every open VISIBLE user fact into `memory_scores`:
   - `compute_recency(valid_from, now) = 0.995^hours_since` (clamped `[0,1]`; a `now` before `valid_from`
     clamps age to 0 → recency 1.0; unparseable timestamps → deterministic default).
   - `composite_score(recency, importance, relevance) = 0.4·recency + 0.4·(importance/10) + 0.2·relevance`
     (named weight consts; every input clamped to range first, so a junk model reply can't produce an
     out-of-range score). **Importance** is a batch LLM assessment (1–10), assigned ONCE per fact and
     persisted; steady-state passes are **LLM-free** (only never-scored facts are assessed), defaulting to
     `DEFAULT_IMPORTANCE = 5.0` on the stub or any parse failure. **Relevance** is a QUERY-TIME term — the
     job stores the baseline `0.0`.
2. **Reflect** eligible scopes into `memory_rollups` (a light-reasoner synthesis, ≤ token cap,
   wall-clock-bounded). An entity qualifies at N open visible facts OR when any open fact's assessed
   importance ≥ `IMPORTANT_FACT_MIN = 7.0` (a single critical fact rolls up). A `weekly:<YYYY-WNN>` rollup
   synthesizes the user's own memory once per ISO week. **THE STUB NEVER WRITES A ROLLUP** — a stub pass
   scores facts (default importance) and produces ZERO rollups (tested); rollups are exported to the vault,
   and a stub "synthesis" is a debug echo.
3. **GC / regenerate** every existing rollup against its scope's CURRENT visible fact set, keyed by
   `fact_set_hash(fact_ids)` — an **FNV-1a 64 over the SORTED open fact ids** (dependency-free, deterministic;
   the hash changes exactly when the content a rollup was synthesized from changes: a supersede closes+adds,
   a seal deletes). Ineligible scope ⇒ DELETED (row + exported `.md`); changed set ⇒ re-reflected + re-exported.
   This ages out superseded/forgotten facts EVEN WITHOUT a seal.
4. **Export** each un-exported rollup as an atomic `.md` under `<vault>/brain/memory/`.

`PassStats` reports `scored` / `rollups` / `rollups_gcd`. Tune the weights, `IMPORTANT_FACT_MIN`, the entity
eligibility threshold, or the token caps — as named consts, measured against whether the brief/rollups stay
useful and cheap (per-pass Metal time is bounded by a max-rollups-per-pass cap; the rest catch up next pass).

## The lock invariants (LOAD-BEARING — a reviewer gates on these)

1. **Consolidation reads with the EMPTY unlock set.** `run_consolidation_pass` builds
   `let no_unlocks: HashSet<String> = HashSet::new();` and passes it to `list_facts_visible` /
   `list_user_facts_visible`. Derived memory (scores, rollups, the injected brief) must NEVER surface content
   from a sealed folder — even in a session where the user has unlocked it. **Keep the set empty.** Passing
   the live session unlock set here would leak sealed facts into a cross-meeting rollup exported to the vault.
2. **Rollups purge on EVERY seal path.** `memory_rollups` are CROSS-MEETING synthesis with no single source
   meeting, so they can't cascade off a meeting FK. They get TWO protections: (a) `Db::purge_memory_rollups_tx`
   runs INSIDE every seal transaction (`lock_folder` chain, relock, startup reconcile, `delete_meeting`) and
   the caller deletes the exported vault `.md`s — rollups are cheap to regenerate, so purge-then-rebuild is
   safe; (b) the per-pass `fact_set_hash` GC regenerates them against the current visible set. Any NEW
   cross-meeting derived artifact you add MUST get the same treatment.
3. **`memory_scores` are CONTENT-FREE** (fact ids + floats) and cascade off `user_facts`
   (`ON DELETE CASCADE`), so the purge-on-seal / delete-meeting paths drop them transitively. Keep new
   score-like tables content-free + FK-cascaded, or gate + purge them explicitly.
4. **The memory brief is DERIVED, never sealed, always gated.** `synthesize_brief` is built only from VISIBLE
   facts and is injected into the agentic system prompt — it egresses on a cloud provider, so a sealed fact
   reaching it is a leak. Every user-facing read is visibility-gated (`list_user_facts_visible`).

## Gotchas

- **The stub produces facts=empty and rollups=zero.** On the stub reasoner `extract_fact_candidates` /
  `extract_user_fact_candidates` return empty and no rollup is written. A "memory is dead on fresh install"
  report is the stub, not a bug — memory activates when the on-device reasoner model is present.
- **`reconcile_facts` is idempotent by design.** Re-ingesting the same note must not duplicate or churn
  facts — a same-object match is a no-op. If you change matching, preserve idempotency (there's a test).
- **Never widen the consolidation read.** The single most dangerous edit here is swapping the empty unlock
  set for the live one. That is a lock-security FAIL every time.
- **Rollup export writes to the vault** — treat it like any owned-file export: atomic write, and the seal
  path must delete it. No PII in logs (fact subject/predicate/object, entity names) — counts only.
