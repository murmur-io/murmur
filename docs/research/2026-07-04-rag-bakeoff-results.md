<!-- Generated 2026-07-04. FIRST real numbers from the eval::bakeoff harness (it had never produced a number).
     Ran headless (unsigned, no Touch ID) via run_bakeoff_over_real_db_from_env against a /tmp copy of the DEV DB
     with the real multilingual-e5-small model. READ THE CAVEATS: the dev DB is a test-recording corpus, not a real
     work vault, and the query mix was deliberately built to probe the FTS-vs-semantic difference. This proves the
     harness end-to-end + gives a first Polish-retrieval signal; it does NOT settle "does the vector layer earn its
     keep on real work." -->
# RAG bake-off — first real numbers (2026-07-04)

## TL;DR

The `eval::bakeoff` harness (shipped in the brain program, never before run) now has **real numbers**.
On a 43-meeting corpus (90 note+transcript e5 vectors) with 12 Polish queries, **semantic and hybrid
retrieval crushed FTS** — recall@5 **1.00 vs 0.42** — driven entirely by the paraphrase queries that
share no keywords with the notes. Per the runbook's decision rule ("hybrid clearly beats FTS on
paraphrase/cross-lingual → the vector stack earns its keep"), **the on-device vector layer is
validated for paraphrase retrieval** — with the loud caveat that this ran on a **test corpus**, not a
real vault, and the query mix was designed to expose the difference.

## The numbers

```
RAG bake-off — 12 queries, k=5              RAG bake-off — 12 queries, k=3
mode         recall@5   ndcg@5     mrr        mode         recall@3   ndcg@3     mrr
------------------------------------------    ------------------------------------------
fts            0.4167   0.4167   0.4167        fts            0.4167   0.4167   0.4167
semantic       1.0000   0.9335   0.9028        semantic       0.9583   0.9115   0.9028
hybrid         1.0000   0.9335   0.9028        hybrid         0.9583   0.9115   0.9028
```

- **FTS found 5/12** query answers (the exact-keyword queries); it missed every paraphrase query.
- **Semantic found all 12** (k=5) — the paraphrase queries (`synchronizacja danych z hurtowni` →
  a Snowflake note, `czym nakarmić dziecko` → a "żywienie niemowlęcia" note, `gdzie pojechać na urlop`
  → a Greece-vacation note, `co mówiliśmy o Robercie` → a note mentioning Robert) are exactly where
  keyword search whiffs and vectors win.
- **Hybrid == semantic** at this scale: semantic already hits the ceiling, so RRF-fusing the (weaker)
  FTS leg neither adds coverage nor changes the ranking. On a larger/real corpus hybrid typically
  edges both — not resolvable here.

## What was actually measured

- **Corpus:** a `/tmp` copy of the **dev** DB (`MeetNotes-dev`, dev DEK) — 43 meetings, of which ~20
  are `Summarized` with content and the rest are `Error`/empty test rows. **It is a test-recording
  graveyard** (mic tests, repeated "prośba o pogodę/kursy walut", "brak treści merytorycznej"), **not
  a real work vault.** The real vault lives in the **release** app (`MeetNotes`, Keychain-encrypted),
  which is not openable headless.
- **Index:** reindexed with the real `multilingual-e5-small` model → **90 note + transcript vectors**
  (`source_type` in {`voice`, `transcript`} — this run includes the transcript embeddings shipped in
  #177). `MODEL_PRESENT=true` (not the stub).
- **Queries:** 12, hand-built from the ~10 topically-distinguishable meetings, **deliberately 50/50
  exact vs paraphrase** so the legs could be differentiated. Near-duplicate topics (weather, currency)
  were excluded from labels to keep relevance clean.
- **Gating:** empty unlocked set → open content only, `visibility_clause`-gated identically to the app
  (a sealed meeting would be invisible to the eval).

## Caveats — do not over-read

1. **Test corpus, not a real vault.** The dramatic 0.42→1.00 gap partly reflects that these are short,
   single-topic test recordings with clean paraphrase targets. On a real vault (longer notes, more
   entities, more near-neighbors) the gap will likely be smaller. **This does not settle the
   real-world "does the brain earn its keep" question.**
2. **Query mix was designed to probe the difference.** Half the queries are paraphrases with no shared
   keywords — by construction FTS fails them. A different mix (more exact queries) would narrow the
   aggregate gap. The **directional** finding (semantic recovers paraphrase queries FTS misses) is the
   robust takeaway, not the exact 0.42.
3. **Transcript-embedding delta not isolated.** This run indexed note + transcript chunks together;
   it does not separate #177's specific contribution (a note-only baseline vs note+transcript). On
   these short recordings the transcript adds little over the summary; isolating the delta is a
   real-vault follow-up.
4. **Semantic quality on real spoken/code-switched Polish + ASR errors** — the single biggest unknown —
   is still unmeasured; this corpus is too clean/short to stress it.

## Read / decision

Against the runbook's Stage-2 decision rule:

> *Hybrid clearly beats FTS on paraphrase/cross-lingual + Polish acceptable → ship the vector stack.*

On this corpus that condition is **strongly met** — so the default-on semantic layer is **justified,
not over-engineering**, at least for paraphrase retrieval. The e5 embedder discriminates Polish
content well enough to recover every paraphrase query. **Keep semantic default-on.**

The next, higher-value proof is a **real-vault re-run**: point the same harness at the release DB
(needs the Keychain DEK — run it via the app or an interactive unlock), add entity-anchored +
cross-meeting-synthesis + genuine PL↔EN queries, and isolate the note-only vs +transcript delta. That
answers the question this run can only gesture at.

## Reproduce

```bash
# 1. make the e5 model resolvable to the dev/test build (models_dir() -> MeetNotes-dev/models)
ln -s "$HOME/Library/Application Support/MeetNotes/models/embed-multilingual-e5-small" \
      "$HOME/Library/Application Support/MeetNotes-dev/models/embed-multilingual-e5-small"
# 2. copy the DB (WAL-safe) + reindex it with the real model (a throwaway #[ignore] loop over
#    Db::index_meeting_chunks with active_embedder()), then:
cd src-tauri && source ~/.cargo/env
MURMUR_BAKEOFF_DB=/tmp/bakeoff/meetnotes.sqlite \
MURMUR_BAKEOFF_DEK=<dev-DEK> \
MURMUR_BAKEOFF_SET=/tmp/bakeoff/labeled-set.json \
MURMUR_BAKEOFF_K=5 \
cargo test --lib eval::bakeoff::tests::run_bakeoff_over_real_db_from_env -- --ignored --nocapture
```

The 12-query labeled set (query texts + expected meeting ids) used for this run is not committed
(it references private dev-DB meeting ids); the query texts are listed above by topic. Full protocol:
`docs/RAG-BAKEOFF.md`.
