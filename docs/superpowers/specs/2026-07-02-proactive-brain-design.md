# Design spec — Proactive brain (zero-egress recall surfacing)

**Date:** 2026-07-02 · **Status:** draft for review · **Research basis:** `docs/research/2026-07-02-brain-full-analysis.md` (4-angle fan-out; roadmap item 5) · **Originating ask:** "może też proaktywny brain?" — the brain should volunteer relevant context instead of only answering when asked.

---

## 1. Goal & non-goals

**Goal.** The brain proactively surfaces what the user would have asked for, at the moment it matters, from **local reads only**:

- **(a) In-meeting recall cards** — while recording, periodically match the fresh `live_transcript` tail against the local substrates (entities, facts, open commitments, FTS over past meetings) and surface a dismissible card: *"You discussed ⟨Projekt X⟩ on Jun 12 → [[meeting]]"*, *"Open commitment: ⟨Osoba A⟩ owes the pricing doc (from Jun 20)"*.
- **(b) Post-meeting fact deltas** — after Stop, when the just-persisted meeting's facts **update or contradict** an earlier fact (the bitemporal reconcile already computes Add/Invalidate), surface *"this changes what was agreed on Jun 12"* in the detail view.

Prior art: Granola's pre-meeting context pull is the closest shipped analog — but it is cloud-ranked. **Nobody does this fully on-device; we can** (competitive finding, research §Tier 3).

**Non-goals (v1).**
- **No LLM in the loop, no cloud egress, no consent friction.** v1 candidate generation and ranking are deterministic local reads. A cloud re-rank is a P4 option behind the existing consent gate.
- No persistent "proactive log" table — cards are ephemeral RAM/UI state (this keeps v1 free of any new seal path).
- No auto-insertion into notes — a card is informational; the user may click through or open a @brain thread from it. The agent-proposes/you-accept contract is untouched.
- No calendar/pre-meeting trigger (the record screen's pre-meeting-brief surface is being reworked in a parallel branch; do not couple to it).
- Not gated on the RAG bake-off: v1 uses FTS + entities + facts + commitments only. The vector-similarity leg joins in P4 **iff** the bake-off promotes vectors.

**Decisions (confirm at review).**
1. **D1 — Deterministic, zero-egress matcher.** Candidates come from (i) known entity names (the `entities` table) matched against the newest live-tail delta, (ii) the highest-signal rare terms of the delta run through `search_visible` (FTS/BM25), (iii) open commitments + current facts (`valid_to IS NULL`) for matched entities. No `reason()` call, no provider, no redaction needed — nothing leaves the process. Rationale: it runs on a timer during a meeting; latency, predictability, and privacy all demand determinism.
2. **D2 — Noise is the #1 product risk; the throttle is part of the contract.** Hard rules: at most **1 card per 120 s cooldown**, session-level dedup by `(kind, target_id)`, a minimum relevance score, a one-click dismiss, and a **global mute** (`proactive_hints_enabled`, default **on** but conservative thresholds; flipping it off silences the event source in the backend, not just the UI).
3. **D3 — Every surfaced item is visibility-gated at the source.** All reads go through the existing gated helpers (`search_visible`, `list_facts_visible`, commitments/entity readers with `visibility_clause`). A sealed-not-unlocked meeting must never appear in a card — same bar as every other read path (`lock-security-reviewer` gate applies).
4. **D4 — Piggyback the live loop; emit an event, render in the conversation flow.** The matcher runs inside the existing live-caption loop (`transcribe/live.rs::run`) every K ticks (default ~10 ticks ≈ 30 s) over the tail **delta** since the last scan. Emission = one new typed event `EVENT_PROACTIVE_HINT` (`events.rs` helper, payload `{ kind: "past_meeting"|"open_commitment"|"fact", title, target_id, meeting_id?, score }` — IDs + titles only, no content bodies, no PII beyond what a card must show). FE renders it as a dismissible system-style card in the record-screen conversation flow; clicking opens the target (or seeds a @brain thread).
5. **D5 — Post-meeting deltas reuse the reconcile output.** `facts::reconcile` already returns Add/Invalidate decisions at Stop; (b) is a thin surfacing of `Invalidate` results (old fact + new fact + source meetings), read through `list_facts_visible`. No new computation, no new storage.

---

## 2. Matcher sketch (v1)

```
every K ticks in live.rs::run():
  delta = live_transcript[last_scan_offset..]           # new text only
  ents  = entities table names matched in delta          # case/diacritic-insensitive contains
  terms = top-N rare tokens/bigrams of delta             # len>=5, not stopwords, prefer capitalized
  cands = []
    for e in ents:  cands += open_commitments(e) + current_facts(e) + last_mention_meeting(e)
    for t in terms: cands += search_visible(t, limit 3)  # BM25, excludes current meeting
  score = specificity (entity exact > bigram > term) × recency decay × BM25 rank
  best  = max(cands) if score >= THRESHOLD and not seen(kind, target_id) and cooldown elapsed
  emit EVENT_PROACTIVE_HINT(best)
```

Perf envelope: entity-name scan over ~10³ entities on a ≤2 kB delta + ≤5 FTS queries every 30 s — negligible next to the 14 s whisper window already running in the same loop. Polish recall caveat: exact name matching misses inflections (*"z Markiem"* vs *"Marek"*); FTS's `remove_diacritics 2` tokenizer helps the term leg; entity aliases are a later improvement, not v1.

## 3. Phases & tests (RED-first)

- **P1 — backend matcher + event (headless).** Pure matcher fn + wiring into the live loop behind `proactive_hints_enabled`. RED tests: sealed meeting never surfaces (seed sealed + visible candidates → only visible emitted); cooldown + session dedup enforced; empty/unchanged tail → no event; entity match → commitment surfaced; threshold rejects weak matches; flag off → no scan runs.
- **P2 — FE card + mute setting.** Dismissible card in the conversation flow (opaque overlay rules if floating), Settings toggle, Playwright smoke with mocked IPC (card renders on event, dismiss works, mute stops rendering).
- **P3 — post-meeting fact deltas** in detail view (D5). RED test: invalidated fact renders old→new with both source links; sealed source → the delta is hidden.
- **P4 (post-bake-off / post-user-memory) —** vector-similarity leg via `search_hybrid_visible`; optional consent-gated cloud re-rank; user-memory-brief enrichment.

## 4. Lock model & constraints fit

- **New read paths:** all through existing gated helpers (D3); the event payload carries IDs/titles only. `lock-security-reviewer` is a required gate for P1 and P3.
- **No new seal path:** nothing persisted in v1 (cards are ephemeral; P3 reads already-purged-on-seal facts).
- **Zero egress:** no provider call anywhere in v1; the redaction firewall is not in scope because nothing leaves.
- **Zoneless FE:** event → signal in a store (`toSignal`/listen-once pattern per `RecorderStore.init()`), `@if`-rendered card, no timers in components (cooldown lives in the backend).
- **Honesty bar:** headless proves gating/throttle/matcher logic; whether the cards *feel* relevant vs annoying needs dogfooding on a real Mac with a real vault — plan a threshold-tuning pass from real sessions before defaulting louder.

## 5. Open questions

1. Default `proactive_hints_enabled` — ship default-on with conservative thresholds, or default-off for one release of dogfooding?
2. Card affordance: click-through to the past meeting vs seed a @brain thread with the card as context (thread seeding is cheap once thread persistence lands).
3. Should (b) fact deltas also export into the note `.md` (an Obsidian-visible "what changed" block)? That adds an export path → lock review; deferred until P3 proves value.
