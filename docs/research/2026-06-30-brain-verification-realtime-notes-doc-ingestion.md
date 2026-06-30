<!-- Generated 2026-06-30 via /research (murmur-researcher fan-out, 3 angles). Pricing/versions = point-in-time. -->
# Research: What Murmur's "brain" actually is today + feasibility of (A) realtime @brain notes and (B) whole-brain view + doc ingestion

## TL;DR / Verdict

**Verification (the priority): the user's model — "our notes are turned into the brain = a vector DB" — is aspirationally true but operationally OFF by default.** A real on-device vector store exists and is well-built (`sqlite-vec` `vec0` KNN over `multilingual-e5-small` 384-d embeddings, hybrid RRF-fused with FTS5 + the entity graph), but it is **dormant on a fresh install** behind three independent switches. **Today's actual brain = FTS5/BM25 full-text + an LLM-extracted entity graph + a bitemporal facts layer + a (cloud-by-default) reasoner.** The vector layer is a real-but-opt-in, bring-your-own-model upgrade on top — and its real-world quality (incl. Polish) is **unproven** (the only smoke test is `#[ignore]`d; the RAG bake-off is unrun).

**Both planned features are feasible and well-supported by the existing code** — but both implicitly assume a *working* semantic brain, which today it is not. So the correct sequence is: **(0) make the vector brain real + proven, THEN (A) realtime @brain notes, THEN (B) doc ingestion, and DEFER the bespoke "brain-atlas" UI** (the useful 95% already exists as our entity directory; a global map is mostly eye-candy at our scale and needs a new FE dep).

## Co już mamy (the corrected brain map, code-grounded)

**The always-on substrate (no flag, no model needed):**
- **FTS5/BM25** over titles + segments + notes, trigger-maintained (`db.rs:423-494`). This is what powers search + the "notes compound" RAG note-grounding — which is **lexical, NOT vector** (`summarize/related_context.rs:1-8` "Uses the LIVE FTS5 retrieval … NO embedding model").
- **Entity graph** (`entities`/`entity_mentions`, `db.rs:240-258`), extracted by the **cloud summarizer LLM** (not local NER), best-effort (`summarize/graph.rs:24-34`, `commands.rs:1406-1421`).
- **Bitemporal facts** (entity·predicate·object triples, two time axes, supersession-not-delete; `facts.rs:114`), gated read via `list_facts_visible`.
- **Reasoner** defaults to **Cloud** (`config.rs:47,237`) through the `make_provider` redaction firewall.

**The vector layer — SHIPPED but GATED OFF by default (all real, not stubs):**
- Embedder: `multilingual-e5-small`, **384-dim**, multilingual incl. Polish, e5 `query:`/`passage:` prefixes (`embed.rs:30,75-92`; real candle impl `embed/candle_bert.rs:148-214`). **Downloaded-not-bundled** (`embed.rs:91,104`); with no model on disk, `active_embedder()` falls back to a hash-bag `StubEmbedder` that is **explicitly NOT semantic** (`embed.rs:111-113,146-161`).
- Store: `vec_chunks(chunk_id, embedding float[384])` `vec0` virtual table (`db.rs:384-388`), 1:1 with plaintext `note_chunks` (`db.rs:369-379`); real ANN KNN inside SQLCipher (`db.rs:997-1000`).
- Retrieval: hybrid `search_hybrid_visible` = RRF fusion of FTS ∪ vector-KNN ∪ entity-graph, each `visibility_clause`-gated (`db.rs:1138-1193`). Chunks **purged on seal** (`purge_chunks_tx`, `db.rs:942-958`).
- What's embedded: the **note markdown, chunked ~800 chars** (`embed.rs:214-238`) — NOT transcript segments. Auto only `if semantic_search_enabled` at note-finalize (`pipeline.rs:594-606`), else only via manual `reindex_embeddings` (`commands.rs:2676`, which refuses the stub: `commands.rs:2757`).

**Fresh-install default state** (`config.rs:234-241`): `semantic_search_enabled=false`, `brain_model_id=None`, `brain_backend=Cloud`, `web_search_enabled=false`, `cloud_egress_consented=false`, e5 absent. **⇒ the vector brain is dormant; the FTS+graph+facts brain is live.**

## Findings

### Gap vs the user's mental model (Angle 1 — VERIFY)
- "Notes → vector DB" is the *opt-in upgrade*, not the substrate. Out of the box the brain is **lexical + graph + facts + cloud LLM**.
- The single biggest correction: the always-on "make notes compound" grounding is **FTS, not embeddings** (`related_context.rs:86`).
- **Real bug found (medium-confidence, code-read not reproduced):** the pipeline auto-index gates on the flag but **NOT on `embed_model_present()`** (`pipeline.rs:594,600`) — so enabling the flag *without* the model writes **stub-hash vectors** into `vec_chunks` (while the manual backfill correctly refuses). This silently pollutes the index. ~3-line fix + RED-before-GREEN test.
- Real semantic quality / Metal / **Polish recall = UNVERIFIED** (ignored smoke test `candle_bert.rs:254`; bake-off `docs/RAG-BAKEOFF.md` unrun). Needs a signed build + e5 on a real Mac.

### (A) Realtime @brain inline notes — feasible, foundation ~90% ready
- The **@brain-ask half is shipped:** `ask_assistant_text` (`commands.rs:610`) already routes a typed question through the gated, redacted, consent-gated agentic brain (`run_assistant_query`, `live.rs:318`); the record screen already has a text composer (the just-merged unified assistant, `assistant-actions.component.ts:113`).
- **New work = a persistent typed-notes store** + wiring into (a) live brain context (exactly like the 0.6.2 `live_transcript` injection, `live.rs:417-422,891-908`) and (b) the finalized `notes.markdown`.
- **Latent home + a lock gap:** the unused `notes_asides` table (`db.rs:271`, written only by the voice "note aside" intent, `list_note_asides` has **zero non-test callers**) — but it is **NOT sealed/purged on lock** (`db.rs:900-916`), so an aside's plaintext **survives a folder seal**. Typed notes are *primary user content* → they need **seal-and-restore** (like `notes.markdown`), not purge. **Cleanest:** fold typed notes into `notes.markdown` (`## My notes`) at finalize so they inherit the correct `seal_note` path, and use `notes_asides` only as the transient live buffer (+ close its lock gap).
- **Prior art:** Granola (your typed notes *steer* the summarizer; your text black, AI gray for attribution); ClickUp `@Brain` (private-until-insert); Notion `/ai` (`@` reserved for context-mention). The differentiated combo: typed emphasis as first-class context for a **local-first** brain + `@`-invoke insert. Category itself is table-stakes.
- **Fully headless-verifiable** (persistence + injection + finalize + seal round-trip = `cargo test --lib`) — rare for content work.

### (B) Whole-brain view + doc ingestion — two halves, very different value
- **Doc ingestion = the high-value half (build it).** ~80% exists: `chunk_note` (`embed.rs:214`) generalizes to any text; `active_embedder` + `note_chunks`/`vec_chunks` + `reindex_embeddings` reuse directly. `note_chunks.source_type` (`db.rs:374`) already anticipates multi-source but is **write-only today** ('voice', never read). md/txt = **zero new deps**; **PDF = a new crate (needs approval)** → defer.
  - **Storage fork:** synthetic-meeting row (cheap, pollutes Library/graph) vs a clean `documents`/`sources` table + source-polymorphic chunks (brain2-native, matches the "source-agnostic from day one" memory). Lean clean; the FK `note_chunks.meeting_id NOT NULL ON DELETE CASCADE` (`db.rs:167,377`) is the load-bearing constraint.
  - **Lock fit (load-bearing):** embeddings are **invertible to text** (vec2text) → doc chunks MUST be `visibility_clause`-gated + purged-on-lock. Cheapest safe model: **assign each doc to a folder** → the existing per-folder lock + purge + gating cover it. lock-security-reviewer gated. **Zero new egress** (local parse + local embed).
- **The "great-UI whole-brain map" = the lower-value half (defer).** Prior art is decisive: global node-link graphs are "beautiful but useless" past a few hundred nodes; our **entity directory already does the useful 95%** and beats Obsidian (we have typed nodes + typed edges). A 2D embedding atlas is a gadget at our ~10²–10³-chunk scale (already concluded in `docs/research/2026-06-29-embedding-visualization-graph-tab.md`; raw embeddings must never leave Rust). A polished animated force-graph needs a **new npm dep** (force-graph/d3/sigma → your approval) and fights the zoneless no-rAF rule. Escape hatch if we do it: compute layout in **Rust**, render **static SVG** (no deps, headless-testable).

## Fit z ograniczeniami Murmur
- **Local-first / privacy:** embedding + ingestion are on-device; e5 download is inbound-only. The default-cloud reasoner + entity/facts extraction already egress note text through the redaction firewall + consent — existing behavior, no new egress class added by either feature.
- **SQLite-canonical:** vectors/chunks/entities/facts/typed-notes all in the one SQLCipher DB; `vec0` registered before `PRAGMA key` (encryption intact).
- **Lock model:** every semantic/graph/facts read is `visibility_clause`-gated + purged-on-seal. **Both features are lock-touching** (typed notes = seal-and-restore; doc chunks = folder-gated + purge) → **lock-security-reviewer mandatory**.
- **Obsidian-native:** typed notes fold into `.md`; ingested docs can export as `.md` — owned files, no lock-in.
- **CI honesty:** ingestion + typed-notes + gating are fully headless-testable; **semantic quality + Polish recall are NOT** (need a real Mac + e5 + the bake-off).

## Opcje i tradeoffy
- **Step 0 — make the vector brain real (S, ~½ day):** (a) gate auto-index on `embed_model_present()` (`pipeline.rs:594`) + RED test; (b) run the e5 bake-off on a real Mac (does vector beat FTS5 at our scale; Polish recall). De-risks A and B before UX work.
- **Feature A:** A1 persistent typed-notes + injection + finalize (S–M, headless) → A2 inline `@brain` autocomplete + attributed insert (M, FE) → skip A3 (full `@`-everything).
- **Feature B:** I-1 md/txt ingestion, folder-gated, reuse pipeline, surface in existing `/graph` (S–M) → I-2 clean `documents` table (M–L) → I-3 PDF (needs dep approval). Viz: V-1 static node-link in `/graph` (M, no deps) ≫ V-2 animated atlas (needs dep) / V-3 embedding map (defer).

## Rekomendacja i pierwszy krok
1. **First, fix + prove the foundation (Step 0).** Close the stub-vector gap and run the bake-off. Cheap, and everything downstream assumes "semantic actually works."
2. **Then Feature A1** (typed notes → live brain context + into the `.md`) — highest value/effort ratio, fully testable, and it's the Granola-style differentiator delivered locally. Fold into `notes.markdown` to inherit the correct seal path; close the `notes_asides` lock gap.
3. **Then Feature B I-1** (md/txt doc ingestion, folder-gated) — makes the brain demonstrably know more, reusing `embed.rs` end-to-end.
4. **Defer the bespoke brain-atlas UI** (V-1+) until there's enough content and a passed bake-off; the entity directory + a future "Documents" tab cover the real need first.

Each of A1 / I-1 is its own ship-feature cycle (plan → build → adversarial-verify + lock-security-reviewer → PR), batched toward a release.

## Otwarte pytania / czego nie udało się zweryfikować
- **Real semantic quality + Polish recall** — unproven by design; needs a signed build + e5 on a real Mac + the bake-off.
- **Stub-vector-on-flag-without-model** — read in code, not runtime-reproduced (medium confidence); also unconfirmed whether the Settings toggle is disabled until the model is present.
- **`vec0` read-back** (`SELECT embedding`) — only the KNN path is used in-tree (`db.rs:1047` avoids reading vectors back); matters only for an embedding-map viz, needs a 1-line spike.
- **Storage fork ripple** — decoupling `note_chunks` from the meeting FK touches every `visibility_clause` join (`note_chunks → meetings → folders`); contained but needs a careful pass.
- **ClickUp `@Brain` exact insert/attribution mechanics** — deep help page 403'd; corroborated from the section index.

## Sources
**Code (current tree):** `embed.rs:30,75-92,104,146-161,214-238`; `embed/candle_bert.rs:148-214,254`; `storage/db.rs:240-315,369-390,423-494,831-916,942-958,983-1041,1138-1193,167,377,271-278,2499,2511`; `pipeline.rs:594-606`; `tools.rs:165-204,458-528`; `agent.rs:72-156`; `summarize/related_context.rs:1-8,86`; `summarize/graph.rs:24-34`; `summarize/redact.rs:344-357`; `facts.rs:114,231-244`; `reason.rs:257-297,462-468`; `settings/config.rs:47,234-241`; `commands.rs:610,669,1389-1606,2676,2710,2757`; `transcribe/live.rs:318-454,417-422,891-908`; `state.rs:122`; `voice_action.rs:531-552,1093`; FE `features/record/assistant-actions.component.ts:113`, `features/graph/graph.component.ts:25-38`, `entity-neighborhood.component.ts:330-372`, `core/ipc.service.ts:478-553`.
**Web:** Granola (wondertools.substack.com/p/granolaguide; granola.ai); ClickUp @Brain (help.clickup.com/.../20658787666071); Notion AI (notion.com/help/guides/using-slash-commands; storylane.io); Obsidian graph critique (codeculture.store/.../obsidian-graph-view-useful; eleanorkonik.com); Nomic Atlas / UMAP (docs.nomic.ai/.../how-to-visualize-embeddings); Apple Embedding Atlas (arxiv.org/abs/2505.06386); graph perf (pmc.ncbi.nlm.nih.gov/articles/PMC12061801; medium.com/neo4j d3+PIXI).
**Internal prior art:** `docs/research/2026-06-29-embedding-visualization-graph-tab.md`; `docs/RAG-BAKEOFF.md` (unrun).
