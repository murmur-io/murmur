# Design spec — Cross-meeting user memory ("the brain remembers you — locally")

**Date:** 2026-07-02 · **Status:** draft for review; **implementation deliberately next batch** (depends on PR D thread persistence) · **Research basis:** `docs/research/2026-07-02-brain-full-analysis.md` (roadmap item 4) + `docs/research/2026-07-02-clickup-brain-gap-analysis.md` (memory = the marketing-critical gap; scored 8 vs 3) · **Originating ask:** "konkurent ClickUp Brain" — Brain²'s entire pitch is *persistent context*; Murmur cannot claim the AI-knowledge-manager class while its brain forgets every conversation.

---

## 1. Goal & non-goals

**Goal.** The brain accumulates durable, **user-scoped** knowledge across meetings and conversations, and grounds every answer in it:

1. **User facts** — "facts about ME/my work" (preferences, commitments, recurring context: *"pracuję nad Projektem X"*, *"wolę odpowiedzi po polsku"*, *"deadline Q3 to 15.09"*) extracted at Stop from transcripts AND (new, via PR D) from persisted @brain thread turns — telling the brain *"zapamiętaj, że…"* must stick.
2. **The memory brief** — a small, synthesized, **auditable** markdown document (the Claude-memory pattern: "read it like a brief about yourself"), regenerated from current user facts and injected into agent grounding exactly like `live_transcript` is today.
3. **The audit view** — a "Memory" section on the Brain page: every current user fact with provenance links and a one-click **forget** (bitemporal invalidate, never silent delete).

**Non-goals (v1).** No agent auto-writes to memory mid-turn (extraction is a pipeline step, not a tool — keeps the loop read-only); no org/team memory; no vault `.md` export of the brief (P4 — a cross-meeting artifact whose sources can be individually sealed is an unresolved lock/product call); no LLM-judge dedup (deterministic reconcile only); no embedding of the brief (it's small and always-injected).

**Decisions (confirm at review).**
1. **D1 — Reuse the bitemporal facts substrate; add a scope, not a system.** `facts.rs` already implements exactly the right memory semantics (valid_from/valid_to, invalidate-not-delete, deterministic reconcile — the Zep/Graphiti pattern, validated in the 2026-07-02 analysis). User memory = facts with a **user scope** (additive `scope TEXT` column or a reserved subject kind — implementer picks the cleaner fit with the existing `(entity, subject, predicate)` key), extracted by the same consent-gated + redacted reasoner path as today's entity facts. New substrate = zero; new invariants = zero.
2. **D2 — The brief is REGENERATED, never incrementally mutated.** Stored in one small table (`memory_brief`: content, generated_at, source_meeting_ids JSON) with per-fact provenance riding the facts rows. Regenerate when new user facts land at Stop (cheap — only then), and lazily on first use after invalidation. Injection: ≤2k chars into `assistant_system_prompt` next to the live-transcript section, same fail-closed shape.
3. **D3 — Purge-and-regenerate on seal (the lock-model crux).** User facts carry their source `meeting_id`; sealing a folder purges its meetings' facts in the seal tx (this already happens — `purge_facts_tx`) AND marks the brief stale; the next read regenerates it from the remaining **visible** sources only. RED test #1 of the whole feature: a fact whose only source is a sealed meeting never appears in the injected brief nor the audit view. Session-relock (screen-share) ⇒ same staleness path.
4. **D4 — Auditable and user-editable is not optional.** The audit view ships in the same batch as injection (P2+P3 together if needed): memory the user can't see and forget is a liability, not a feature — and "what the brain knows about you, inspectable, on your Mac" is the counter-pitch to ClickUp's "admins cannot access Brain² chats" *assertion*.
5. **D5 — Threads are an extraction source (PR D dependency).** Extraction runs over (a) the meeting transcript at Stop (as facts do today) and (b) that meeting's persisted thread turns — an explicit *"zapamiętaj…"* in a thread is the highest-signal memory there is. Extraction from post-Stop/global chat is out of scope v1 (those turns aren't persisted).

## 2. Dataflow sketch

```
Stop → pipeline: facts extraction (existing, consent-gated, redacted)
         ├─ entity facts (existing)
         └─ user-scoped facts (NEW: transcript + persisted thread turns)
               → facts table (scope='user', source meeting_id, bitemporal reconcile)
               → memory_brief marked stale
next assistant turn / Ask:
  brief stale? → regenerate from CURRENT user facts WHERE source meeting visible
  → inject ≤2k chars into assistant_system_prompt (fail-closed: no visible facts ⇒ no section)
Brain page → Memory section: list_user_facts_visible (gated) + forget → invalidate + brief stale
seal/relock → purge_facts_tx (existing) + brief stale → regenerated WITHOUT sealed sources
```

## 3. Phases & tests (RED-first)

- **P1 — schema + extraction + gated readers.** Additive columns/scope, extraction prompt extension, `list_user_facts_visible`. RED: sealed-source fact invisible through the reader; reconcile invalidates a contradicted user fact (predicate-key reuse); extraction is a no-op without consent (stub reasoner ⇒ empty, existing pattern).
- **P2 — brief synthesis + injection.** Deterministic assembly (facts → sections by predicate class; newest-valid first; char budget), staleness lifecycle, injection behind the same visibility snapshot as `gated_live_context`. RED: brief excludes sealed sources after seal (regenerate path); byte-budget enforced; empty memory ⇒ no prompt section; redaction still scrubs the injected brief on egress (it rides the system prompt through RedactingProvider — assert the seam, not re-test redact.rs).
- **P3 — audit UI + forget.** Brain-page Memory section (signals/zoneless; opaque overlays for any popover), per-fact provenance link + forget (invalidate → brief stale). Playwright smoke with mocked IPC.
- **P4 (deferred) — vault export.** Owned `Memory.md` in the vault — needs the lock/product decision on cross-meeting artifacts (delete/regenerate on any source seal, like note `.md`s) + lock-security review of the export path.

## 4. Lock model & constraints fit

- **Every read gated:** `list_user_facts_visible` + the brief regeneration query both filter by source-meeting visibility (`visibility_clause`); the injected brief is derived exclusively from visible facts (D3). `lock-security-reviewer` gates P1 and P2.
- **No new seal path:** facts already purge in the seal tx; the brief is derived data — regenerated, never sealed (it must never contain sealed-source content in the first place, which D3's RED test pins).
- **Local-first:** extraction rides the existing consent-gated reasoner (cloud default, local when GGUF present); brief assembly and injection are pure local reads; the brief egresses only inside already-redacted prompts.
- **SQLite-canonical:** facts + brief live in the one DB; the audit view is a thin reader; the (P4) vault export would be a thin exporter.
- **Honesty bar:** whether synthesized memory *helps or pollutes* answers has no public benchmark — plan a dogfooding period with the forget control before defaulting the injection ON (ship behind `user_memory_enabled`, default on only after the bake-off-style self-eval).

## 5. Open questions

1. Scope representation: `scope` column on facts vs a reserved subject — implementer proposal at P1 review.
2. Should thread-sourced facts require the user's explicit *"zapamiętaj"* (imperative-only extraction) or accept implicit preferences too? v1 lean: imperative + high-confidence only (precision over recall — wrong memories are worse than missing ones).
3. Brief injection into `ask_vault`/Ask (PR G unifies it onto the loop — then it inherits injection for free) — confirm ordering: G before or after P2.
4. `user_memory_enabled` default: on with conservative extraction, or off for one dogfood release?
