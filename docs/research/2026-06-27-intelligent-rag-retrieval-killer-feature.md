<!-- Generated 2026-06-27 via /research (murmur-researcher fan-out, 5 angles + 3 adversarial verifiers). Pricing/funding/version/competitor facts = point-in-time 2026-06-27. -->
# Research: Intelligent RAG / semantic retrieval as Murmur's next leverage — what's valuable, the killer-feature gap, the lock-model wiring, and where we actually stand vs competitors

## TL;DR / Verdict

**The killer feature is not "a vector RAG engine" — it is a *trustworthy, cited, cross-history Ask/Dossier that is local + encrypted + Obsidian-owned*, and at our current corpus size the cheapest path to it does not need vectors at all.** Five research angles converged on "FTS5 now → spike vectors → hybrid + GraphRAG-lite + reranker → deliver via Dossier/MCP." A hard adversarial red-team (plus two fact-check passes) survives that consensus only **in its cheap half** and refutes its expensive half:

1. **Ship now (S, zero new deps, fully headless-verifiable):** replace the broken `LIKE` search with **FTS5/BM25**; **fix a code-confirmed correctness bug** where Ask-My-Vault and the Pre-Meeting Brief silently omit every *session-unlocked* folder; raise the context budget; deliver a **cited** Ask that packs top-N whole notes. This is ~80–90% of the real-user value and it can't segfault or break on an OS bump.
2. **Defer the vector/embeddings/reranker layer behind a *decision-first* spike**, not the "which vector model wins" spike all five briefs proposed. At an ~18-meeting corpus (≈100–250K tokens — at/just past Anthropic's "<200K tokens ⇒ no RAG needed" threshold) the synthesis LLM bridges paraphrase for free if you just let it read more notes. Build vectors **only if** FTS5 + whole-note-packing demonstrably fails to "read like a brain" on the real DB.
3. **The moat fights the feature, and four of five briefs missed it.** The privacy-conscious buyer is exactly the user who *locks folders* — so "search my whole brain" is structurally capped at "search my **unlocked** subset." A dossier that silently drops locked-folder mentions is misleading. This must reshape the pitch ("search your open notes; locked vaults stay dark") and **kill** the "second brain that knows everything" framing.
4. **"Uncontested ground" is false.** Meetily (MIT, Rust, macOS — a near-twin) *markets* the identical local+encrypted+semantic union (though its README shows semantic-search + encryption are still roadmap); Anarlog and Hapi already ship local + cross-meeting + markdown. Our **defensible, narrower** wedge is the **security depth** (SQLCipher-encrypted-at-rest + per-folder lock model) **+ entity-graph-joined retrieval** — the single leg *no* shipping or roadmapped competitor holds.
5. **The most differentiated technical bet is GraphRAG-lite over the entity graph we already ship** — and even that can be approximated cheaply (entity-anchored FTS candidate selection) with no embeddings at current scale.

**The single most decision-relevant unknown gating everything below: the real corpus size.** The whole "defer vectors" case is contingent on a small single-user corpus (the ~18-meeting figure comes from the briefs; the live DB is encrypted and unread). If it's 500+ meetings the vector case strengthens. Confirm this first.

---

## Co już mamy (z repo, z file:line — zweryfikowane w kodzie tej sesji, branch `murmur`, v0.4.0)

**Two facts that are NEWER/STRONGER than the prior research doc (`docs/research/2026-06-27-killer-feature-brain2-multisource.md`) assumed:**

- **The redaction firewall is FIXED.** `is_cloud()` now covers **both** `claude_code` AND `anthropic` (`summarize/mod.rs:47-48`); both are wrapped in `RedactingProvider` and a **fail-closed `cloud_egress_consented` gate** refuses to even construct a cloud provider without one-time consent (`mod.rs:61-99`). The prior doc's "decisive caveat — `claude_code` bypasses redaction → any aggregated-context feature must wait" is **CLOSED**. Aggregated-context retrieval (dossiers, rollups) is unblocked on the privacy floor *now*. (Residual: the regex scrubs emails/cards/phones, **not names** — `redact.rs:5-6`. With the firewall now actually wired, this is the load-bearing residual leak.)
- **An ONNX runtime is already statically linked in-process** via `sherpa-onnx` (diarization) + `whisper-rs`/Metal — so on-device inference is a proven pattern. *But* sherpa's API is ASR/speaker-specific, **not** a general text-embedding API, so a text-embedding model still needs its own path (Ollama or a new `fastembed`/`ort` dep).

**Shipped and reusable as the retrieval read-layer:**
- **Entity graph in SQLite** — `entities` + `entity_mentions` (FK-cascade + 3 indexes, `db.rs:194-212`); readers `list_entities_visible` / `entity_mentions_visible` / `entity_neighbors_visible` / `build_entity_detail` (`db.rs:1712-2024`), **all visibility-gated**. `build_entity_detail` is ~80% of a Dossier data layer. Extraction is LLM-based, **people + projects only, exact-case-insensitive dedup, no typed relations** (`summarize/graph.rs:16-21,52-64`) — a node + co-occurrence graph, enough to "walk neighbours."
- **Aggregation readers to generalize (not rebuild):** Ask-My-Vault (`vault_context.rs`, `vault_chat.rs`), Weekly Digest with a "who owes what" rollup (`digest.rs:22`), Pre-Meeting Brief with "Still open" (`brief.rs:15`), cross-meeting Topic Threads (`threads.rs:19-50`).
- **Action-item bridge** (per-meeting, lossy): `parse_action_items` extracts `- [ ]` + owner + ISO due-date (`action_items.rs:8-26`); `add_reminder` shells `osascript` to Apple Reminders but **only writes `name` — it drops the due date** (`commands.rs:877`); `patch_note_tasks` → Obsidian Tasks (`commands.rs:829`).
- **Local, egress-free MCP server** — read-only, token-gated, bound to `127.0.0.1`, 3 tools (`search_meetings`, `get_meeting`, `list_recent_meetings`; `mcp.rs:220-330`). Trivially extensible (`tools_spec()` + a match arm).
- **Capture is genuinely strong:** default `large-v3` multilingual, dual-stream me/others, and **per-speaker diarization of "others" SHIPPED** (opt-in `config.diarize_others`, pyannote-seg + CAM++, `pipeline.rs:331-413`, `transcribe/diarize.rs`).
- **Whole DB is SQLCipher-encrypted** (`bundled-sqlcipher-vendored-openssl`); FTS5 is **compiled in, available, unused** (zero new dep). Every content read is gated by `meeting_is_unlocked` / `visibility_clause`; seal = verify-before-destroy (`db.rs:1155/1373/1427`, `crypto.rs:115-135`).

**Broken / the actual work:**
- **Search is still `LIKE '%q%'`** — both `search()` (`db.rs:406-445`) and `search_visible()` (`db.rs:1521-1569`). Word-order-sensitive, no ranking, no semantics. The retrieval muscle a "second brain" lives on is the weakest link.
- **CONFIRMED correctness bug (verified this session, not just from the briefs):** `ask_vault` (`commands.rs:1104`) and `pre_meeting_brief` (`commands.rs:1323`) call the **3-arg `build_vault_context`**, which delegates with an **empty unlock set** (`vault_context.rs:38-44`, TODO `:34-37`). It is *fail-closed* (safe, no-leak), but the effect is that **Ask-My-Vault and the Brief silently omit every folder the user has session-unlocked** — they return confidently-incomplete answers right now. The fix is trivial: pass `&state.unlocked_folders` (already available; used at `commands.rs:1809`, `1973`, `2051`, …). Plus, when search returns nothing it dumps "30 most recent" (`vault_context.rs:68-69`).
- **No cross-meeting commitment rollup, no `.md` dossier export, no semantic/entity MCP tool, no dictation path.**

---

## Findings (per angle; each claim carries confidence; adversarial corrections folded in inline)

### A. The intelligent-RAG stack — three layers, not six (and the vector layer is for *quality*, not *scale*)

- **At our scale, "RAG" is barely necessary.** Anthropic: *"if your knowledge base is smaller than 200,000 tokens (~500 pages), you can just include the entire knowledge base in the prompt"* [high] (https://www.anthropic.com/news/contextual-retrieval). 18 meetings × ~5–15K tokens ≈ **100–250K tokens — at/just past that threshold**. So the problem is **candidate-selection-under-budget**, not search-at-scale. The current budget is only **80K chars (~20K tokens)** (`vault_context.rs:25`) — *raising it toward the model's real context window is itself a cheap lever* that removes most of the retrieval-precision pressure.
- **Hybrid retrieval** (FTS5/BM25 ∪ dense-vector KNN fused by **RRF**) reliably beats either alone, *especially for Polish + paraphrase + spoken-vs-written vocabulary* where lexical search structurally fails ("ship it" ↔ "we agreed to release") [high] (https://alexgarcia.xyz/blog/2024/sqlite-vec-hybrid-search/index.html). **But** the red-team's counter holds: if the whole corpus fits in the prompt, the **synthesis LLM bridges that paraphrase for free** — the vector layer's recall win only materializes once you're forced to retrieve a strict subset.
- **`sqlite-vec`** = `vec0` virtual table inside the same SQLite file, brute-force KNN sub-ms at our scale, MIT/Apache. **Tech-verifier correction (important): the build risk is *lower* than the briefs feared** — `load_extension` is compiled in (`-DSQLITE_ENABLE_LOAD_EXTENSION=1`), the repo's **rusqlite 0.32.1** is on the *good* side of the 0.34 API break, and encryption is transparent to a virtual-table module. The genuine residual risk is **not SQLCipher** but the **macOS `sqlite3_auto_extension` deprecation** (warning today; per-connection fallback "segfaults" for the maintainer) [med-high] (https://github.com/asg017/sqlite-vec/issues/169, https://github.com/rusqlite/rusqlite/blob/master/libsqlite3-sys/build.rs).
- **Reranking** adds +5–15 nDCG over bi-encoder cosine [high], but the on-device cross-encoders are heavy (bge-reranker-v2-m3 = 568M, ~200ms+ CPU for 20–50 candidates). **Tech-verifier correction: the "fastembed gives BGE-M3 Metal" claim is false as written** — fastembed's `metal`/`accelerate` features gate *candle* models, not the BGE-M3/reranker ONNX path (quantized default = CPU-only); Apple-Silicon accel needs a custom FP16 export + ort's `coreml` EP yourself.
- **Skip:** Anthropic-style LLM-per-chunk *contextual retrieval* (egress + cost explosion for thousands of chunks; use **late chunking** or a deterministic `title·date·speaker` header instead — ~80% of the benefit, zero LLM calls); **agentic/iterative RAG** (multi-hop loops degrade ranking precision and each hop is a cloud round-trip) [high] (https://jina.ai/news/late-chunking-in-long-context-embedding-models/, https://arxiv.org/abs/2606.05658v1).
- **Polish embedding model:** PIRB ranks **BGE-M3 / multilingual-e5-large** top *on clean text*; **ASR-transcript Polish is unmeasured** [med] (https://arxiv.org/html/2402.13350v2). BGE-M3 uniquely emits dense+sparse+ColBERT in one pass (embedding + lexical + late-interaction rerank in one model) but is ~560M (**+1–2GB bundle**; briefs disagree with the standing "+300MB" by 4–6× — *nobody has measured it*).

### B. RAG ⨯ the lock model — the most Murmur-specific constraint, and a real new leak surface

- **Stored embeddings are recoverable-enough to leak entities, and a local app *bundles the model* — so the attacker precondition is met for anyone who has the app.** [high on direction] **Verifier correction:** the strongest specific claim ("gray-box, *no query access needed*") is **refuted** — vec2text needs ~50 query-rounds through the model and is demonstrated only at 32 tokens; GEIA's decoder must be *trained on the victim model's outputs*. All numbers are **English-only and model-specific** (NERR 27.6%–55.6% across SBERT variants; **zero** results for e5/BGE-M3/Polish), and the proposed *long-chunk* design softens the attack. Net: **treat sealed-folder embeddings as PII-equivalent — the conclusion survives; the justification was overstated** (https://thegradient.pub/text-embedding-inversion/, https://arxiv.org/html/2504.16609v1).
- **Therefore (binding invariants for any future vector index):**
  1. **Gate before KNN, not after.** `segments`/`notes` carry no folder column — lock state is derived via `notes.folder_id → folders f` inside `visibility_clause` (`db.rs:2057`). A `vec0` table is folder-blind; semantic search MUST resolve the visible-meeting-id set first and KNN *within* it (mirror `search_visible`, `db.rs:1521`). No code path may touch `vec0` except through a `search_semantic_visible` — the embeddings analog of the `convertFileSrc`/`asset:` bypass.
  2. **Prefer "embed-on-unlock, ephemeral" (Option C) over "seal embeddings at rest" (Option A).** The safest sealed embedding is the one that *doesn't exist on disk*: never embed locked content; materialize an in-memory `vec0` on session-unlock, drop on relock. This has a strictly smaller attack surface than a sealed `vec_blob` + verify-before-destroy lifecycle, and sidesteps mirroring three seal paths. (Re-embed cost is sub-second at our scale.)
  3. **Embeddings MUST be generated on-device on un-redacted text** (cloud embedding APIs are forbidden — they'd ship the exact PII the firewall scrubs, and the returned vector is itself recoverable). If `fastembed` is used, the model must be **bundled**, not silently fetched from HuggingFace on first run (that's an undocumented egress event).
  4. **If the `vec0` schema stores chunk text in auxiliary columns, that's a second plaintext copy to seal** — avoidable by schema design (aux columns are opt-in).

### C. Where else to leverage the capture→note→RAG infra (the user's explicit "gdzie") — ranked

1. **Cross-meeting "open commitments / what do I owe" rollup + follow-through (S–M, lead).** Aggregate `parse_action_items` across visible notes by owner; **fix the dropped due-date in `add_reminder` (`commands.rs:877`)**; add a "draft follow-up email" recipe (rides `complete()` + redaction). Reuses `digest.rs`'s "who owes what." Granola markets exactly this ("What did I promise to do this week?") — *capability is table-stakes, but local-first + Obsidian Tasks + Apple Reminders you own is the differentiation* [high]. **Caveat (competitive-verifier): this is "demand-adjacent," not "demand-proven" — Granola saying it's valuable ≠ users demanding it.**
2. **Living entity Dossier as an owned `.md` (M).** Generalize `build_entity_detail` → cited `.md` (Overview · 🕑 Timeline · ⏳ Open commitments · 🧭 Last said / next step), every claim → `[[Title]]`. This is also where "retrieval-augmented note generation" (ground a new note in related prior notes — "last time you decided X") and "what did we decide about X" study-mode actually live. **Most lock-constrained surface — see the tension below.**
3. **Expand the LOCAL MCP server with semantic/dossier/commitments tools (S, near-free, structurally unique).** Granola/Otter/Fathom are racing to be MCP hubs — but theirs are **cloud** servers (your data round-trips their cloud); **ours is local + egress-free**. Adding `search_semantic` / `get_entity_dossier` / `get_open_commitments` (thin re-exports of already-gated readers) makes Claude Desktop/Code a client of your *encrypted local memory* with zero new egress. No competitor can match this without abandoning their cloud model [high].
- **"Ask anything across my history, cited" = table-stakes, already shipped** — it IS the existing Ask-My-Vault with its broken `LIKE`+blind-spot fixed. Don't position as a flagship; it's the floor.
- **Voice-memo / dictation = cheap utility**, raises daily-open frequency but commodity and doesn't leverage RAG — defer.
- **Ambient / always-on capture = TRAP, skip.** Two independent kills: the local-memory graveyard (Limitless→Meta wind-down) and the legal third rail (*Brewer v. Otter.ai*, N.D. Cal. Aug 2025 — surreptitious-recording suit; "biggest reason buyers are leaving Otter in 2026"). Local encryption helps the *privacy* story, does nothing for the *consent-of-the-other-party* story [high] (https://natlawreview.com/article/take-note-new-wave-privacy-litigation-targets-ai-notetaker-otterai).

### D. Competitive retrieval landscape (point-in-time 2026-06-27) — "chat with meetings" is commoditized; the gap is architecture

| Tool | Retrieval | Cited? | Runs | Polish | Conf. |
|---|---|---|---|---|---|
| **Granola** | Cross-meeting chat; People/Companies; Spaces (folder query) | **Yes — explicit inline citations** (verifier upgrade) | **Cloud** | unconfirmed | high |
| **Otter** | AI Chat across conversations + connected apps; now an MCP **client** + server | grounded; mechanism unstated | **Cloud** (no on-device) | **no Polish** | high |
| **Fathom** | "Ask Fathom" across entire history | **Yes — citations to the exact transcript moment** | **Cloud** | unstated | high |
| **Fireflies (AskFred)** | NL Q&A across all meetings; timestamps + speaker attribution | yes | **Cloud** | **Yes — "pl" #82** (verifier upgrade, HIGH) | high |
| **Limitless (ex-Rewind)** | (local digital-memory chat/search) | — | **Meta acquihire Dec 2025; Rewind app winding down; chat/search survival UNCONFIRMED** | — | high |
| **NotebookLM** | **Gold standard: chunk→embed→vector→retrieve→synthesize, inline passage citations** | **Yes — best-in-class** | **Cloud (Google)** | strong | high |
| **Khoj / Reor / Smart Connections** (local peers) | local-embedding RAG over docs/vault | RAG | **Local** | model-dependent | high |
| **Meetily** (MIT, Rust, macOS) | *markets* "SQLite + VectorDB semantic search, encrypted at rest, diarization" | — | **Local** | unstated | **README shows semantic+encryption are ROADMAP** |
| **Anarlog / Hapi** (local, OSS) | **ship local + cross-meeting AI chat + markdown today** (no encryption-at-rest / diarization) | varies | **Local** | unstated | high |

- **The MCP-direction wedge is real:** every competitor's MCP routes through their cloud; **Murmur's is local + egress-free** (`mcp.rs`).
- **The "uncontested ground" framing is REFUTED** (competitive-verifier, the single most important correction). The honest claim: *an actively-forming category — Meetily markets the identical union, Anarlog/Hapi already ship 3 of its legs locally — and Murmur's defensible edge is the **encryption-at-rest + per-folder lock model + entity-graph depth** they roadmap or lack, **not** "local semantic search" per se.* **GraphRAG-lite / entity-graph-joined retrieval is the one leg no shipping or roadmapped competitor holds** — elevate it as the lead differentiator.

### E. Demand & the killer-feature gap — honest read

- **Demand for "better retrieval" is *inferred from vendor/expert narrative, not demonstrated from primary user threads.*** The cited "-18% decision-carryover despite Granola" is **unsourced vendor content marketing** (remio.ai); the loudest *real* Reddit complaints are **free-tier pricing**, then **summary quality / multi-speaker capture** — not "search isn't semantic" [competitive-verifier, high]. The robust consensus that *is* well-sourced: "capture is easy, retrieval is hard, vaults become landfills," and the 2026 fix is **retrieval-first design + an automated processing layer between capture and retrieval** — which is a *prompt + scheduler + `.md` writer* problem (the Dossier/Brief delivery skin), **not** a vector-similarity problem.
- **"My own files" pull is real revealed-preference but enthusiast loud-minority** (Granola→Obsidian sync hacks: ≥3 GitHub plugins, but the flagship forum thread is 3 replies/2.5 months, motivated by workflow not privacy). Treat as a **retention amplifier + trust wedge, not a mass purchase driver.**
- **NotebookLM defines the loved experience AND the wedge:** "grounded, cited synthesis over *your own* corpus" is what people love; "for sensitive data, policy promises aren't enough — you need architectural guarantees." **That is Murmur's uncontested-by-architecture ground: NotebookLM's cited-synthesis wow, but local + SQLCipher-encrypted + in your `.md` files.** [high]

---

## The decisive tension the consensus missed: the moat fights the feature

The competitive pitch is "search your whole brain, privately." But **the user who cares enough to buy a local-encrypted app is exactly the user who locks folders** — and sealed-not-unlocked content is invisible to *all* retrieval by design and by rule. So:

- "Search my whole brain" → in practice "search my **unlocked subset**." The more a user uses the headline privacy feature (locking), the more hollow the headline retrieval feature becomes.
- **The Dossier is the worst-hit:** a `[[Anna]]` dossier spanning 5 meetings, 2 in locked folders, either **silently drops half her history** (misleading — worse than no answer for commitments) or can't generate while locked. A "living dossier" that flickers in completeness by session-unlock state is a *confusing* artifact, not a brain.
- This is **not a bug to fix — it's an inherent tension** that should reshape the product language: **"search your open notes; locked vaults stay dark,"** and surface the gap honestly ("3 mentions in locked folders — unlock to include"). **Kill the "second brain that knows everything" framing.**

---

## Fit z ograniczeniami Murmur

| Constraint | FTS5 + cited Ask (ship now) | Vector/GraphRAG/rerank (deferred) | Dossier / commitments / MCP delivery |
|---|---|---|---|
| **Local-first / privacy** | ✅ strictly stronger (no cloud retrieval); zero deps | ✅ on-device only, **embed-on-unlock**; ⚠️ never a cloud embed API; model must be bundled | ✅ rollup local; only synthesis egresses (now firewalled + consented) |
| **Obsidian-native / owned files** | ✅ output `.md` + `[[cites]]` | ✅ index inside the encrypted DB (no 2× sidecar bloat like Smart Connections) | ✅ best fit — `[[Person]].md` + backlinks |
| **SQLite-canonical** | ✅ FTS5 = derived index | ✅ `vec0` in the same SQLCipher file | ✅ `build_entity_detail` already canonical |
| **Provider seam + redaction** | ✅ beside the seam | ✅ beside the seam | ⚠️ **name-dense → the unredacted-names hole (`redact.rs`) is now load-bearing**; default aggregated dossiers to Ollama or a louder consent until on-device NER lands |
| **Lock model (load-bearing)** | ✅ reuse `visibility_clause`; **fix the empty-unlock-set bug** | ⚠️ gate-before-KNN + embed-on-unlock + (any aux text sealed) — the `lock-security-reviewer` gate | ⚠️ partial/flickering dossier UX (the tension above) — surface locked gaps honestly |
| **macOS / CI honesty** | ✅ FTS5 + RRF deterministic, fully `cargo test`-able | ⚠️ Polish recall, sqlite-vec macOS auto_extension (#169), embed/rerank/Metal latency — **all need a real Mac, NOT provable by `cargo test`** | ✅ prompt builders unit-testable like `digest.rs`; ⚠️ Reminders write needs a real Mac + TCC |

---

## Opcje i tradeoffy

| Option | Effort | Risk | Unlocks |
|---|---|---|---|
| **A. FTS5/BM25 + fix the session-unlock blind-spot + raise budget + cited Ask** | **S** (zero deps) | low | Correct, complete, ranked retrieval across UI + MCP + Ask + Brief. ~80–90% of real-user value. The non-negotiable prerequisite. |
| **B. Open-commitments rollup + fix dropped due-date + draft-follow-up recipe** | **S–M** | low | The "shows up done" loop that beats the abandonment-prone chat box; `get_open_commitments` MCP tool for free. |
| **C. Entity Dossier as owned `.md` (entity-anchored FTS candidate selection — no embeddings)** | **M** | med (entity fragmentation; partial-while-locked UX; name-redaction) | The differentiated synthesis artifact + RAG-note-generation + study-mode. |
| **D. Local MCP semantic/dossier/commitments tools** | **S** (once B/C exist) | low | The privacy-correct inverse of Granola/Otter's cloud MCP — structurally unique. |
| **E. Hybrid vector + reranker + GraphRAG-lite (sqlite-vec + embeddings)** | **L** | med-high | Real cross-lingual semantic recall — **only if A fails the spike AND the corpus grows.** macOS auto_extension (#169) + bundle cost + least-verifiable layer. |
| **F. Per-chunk contextual retrieval / agentic-iterative RAG / ambient capture** | L+ | high | ❌ **REJECT** — egress/cost/latency for negative ROI; ambient capture is the lawsuit/graveyard trap. |

---

## Rekomendacja i pierwszy krok

**Build A → B → C → D. Treat E as *deferred and conditional*, F as rejected. Retrieval is the killer-feature *direction*; the killer feature itself is a trustworthy cited Ask/Dossier that is local + encrypted + owned — and FTS5 + whole-note-packing delivers it at our scale without vectors.** Lean the *differentiation* on the **security depth + entity-graph-joined retrieval** (the contested leg is "local semantic search"; the uncontested one is "the *secure* version of it"). Reshape the pitch to "search your open notes; locked vaults stay dark."

**Smallest verifiable first slice (this week, S, zero deps, no Mac needed):**
1. Replace both `LIKE` paths with **FTS5/BM25** behind the existing `search()` / `search_visible()` signatures (`db.rs:406`, `1521`). RED-before-GREEN: `"A B"` and `"B A"` both return the doc containing both words.
2. **Flip `ask_vault` (`commands.rs:1104`) and `pre_meeting_brief` (`commands.rs:1323`) to `build_vault_context_visible(..., &state.unlocked_folders…)`** — close the confirmed blind spot. RED-before-GREEN: a session-unlocked folder reappears in Ask. (Lock-security-review this — it changes a visibility path.)
3. Raise the non-Ollama context budget (`vault_context.rs:25`) toward the model's real window, and make Ask emit explicit `[[Title]]` citations.

**The decision-first spike (do this BEFORE adding any crate — it is *cheaper* and asks the right question than the four-way bake-off all five briefs proposed):**
> On the real DB, answer ~15–20 realistic PL+EN cross-meeting questions **two** ways only: **(1) current state**, and **(2) FTS5 + unlock-fix + pack top-N whole notes, cited**. Score "reads like a brain" vs "fragmented/wrong." **No embeddings, no sqlite-vec, no model download.** Escalate to the full four-way vector bake-off **only if (2) demonstrably fails.** Bet: at ~18 meetings, (2) is indistinguishable from the full stack.

**Two cheap, high-leverage spikes the consensus under-prioritized:**
- **Measure Polish ASR accuracy** on a real recorded meeting (read a known script, diff the transcript) — an afternoon. **Garbage-in caps the entire pipeline**: no retrieval layer can find facts the transcriber mangled. If Polish transcripts are degraded, *that* is the killer-feature work, not reranking.
- **Confirm the real corpus size** — the whole "defer vectors" case is contingent on it.

---

## Otwarte pytania / czego nie udało się zweryfikować

- **Real corpus size** — the load-bearing contingency; the ~18-meeting figure is from the briefs (the live DB is encrypted, unread). 500+ meetings would strengthen the vector case. **Confirm first.**
- **Whether FTS5 + whole-note-packing actually "reads like a brain"** on the real DB — asserted on the <200K-token threshold, not measured (the two-way spike).
- **Polish ASR accuracy** — unmeasured by everyone; the cheapest high-leverage unknown.
- **Polish embedding recall + which model + bundle cost (+300MB vs +1–2GB — briefs disagree 4–6×)** — entirely unmeasured; real-Mac bake-off.
- **`sqlite-vec` on our exact tree** — tech-verifier says *lower* risk than the briefs feared (rusqlite 0.32 good side of the 0.34 break, load_extension compiled in); the real test is the **macOS auto_extension deprecation** (#169), not SQLCipher. Still a ~1-day build proof, not a `cargo test`.
- **On-device rerank + Metal latency** — unproven; fastembed's `metal` feature does **not** accelerate the BGE-M3/reranker path (needs custom export + ort `coreml` EP).
- **Embedding-inversion magnitude for e5/BGE-M3/Polish specifically** — direction high, magnitude unmeasured; "no query access" claim refuted; long chunks soften it. Embed-on-unlock remains the right call on attack-surface grounds.
- **Name-redaction hole** (`redact.rs` does emails/cards/phones, not names) — now the load-bearing residual leak for name-dense aggregated dossiers; severity unmeasured; on-device NER (gline-rs) is the fix, Polish NER quality unmeasured.
- **Limitless chat/search survival** — "winding down post-Meta-acquihire"; the primary source does not confirm search specifically died. Use "winding down," not "shut down."
- All competitor pricing/funding/version/feature facts are **point-in-time 2026-06-27** on fast-moving products.

---

## Sources

**External (fetched/searched 2026-06-27; verifier-corrected):**
- Retrieval theory: https://www.anthropic.com/news/contextual-retrieval (<200K = no-RAG threshold; contextual retrieval) · https://alexgarcia.xyz/blog/2024/sqlite-vec-hybrid-search/index.html (hybrid RRF) · https://jina.ai/news/late-chunking-in-long-context-embedding-models/ · https://arxiv.org/abs/2606.05658v1 (query decomposition degrades ranking) · https://arxiv.org/html/2502.11371v3 (RAG vs GraphRAG complementary)
- Backbone tech: https://github.com/asg017/sqlite-vec · https://alexgarcia.xyz/sqlite-vec/rust.html · **https://github.com/asg017/sqlite-vec/issues/169 (macOS auto_extension deprecation — the real risk)** · https://github.com/asg017/sqlite-vec/issues/206 (rusqlite 0.34 API break) · https://github.com/rusqlite/rusqlite/blob/master/libsqlite3-sys/build.rs (load_extension compiled in) · https://github.com/Anush008/fastembed-rs (+ Cargo.toml: metal=candle-not-ort) · https://arxiv.org/html/2402.13350v2 (PIRB Polish) · https://bge-model.com/bge/bge_m3.html
- Embedding inversion: https://thegradient.pub/text-embedding-inversion/ · https://arxiv.org/abs/2310.06816 (vec2text — query access, 32-token) · https://arxiv.org/html/2504.16609v1 (GEIA — decoder trained on victim queries; English-only) · https://ironcorelabs.com/docs/cloaked-ai/embedding-attacks/
- Competitors: https://www.granola.ai/blog/chat-with-meetings-search-analyze-ai-2026 (inline citations, cloud) · https://github.com/Zackriya-Solutions/meetily + https://meetily.ai/ (the near-twin; README vs marketing gap) · https://anarlog.so/blog/open-source-meeting-transcription-software/ · https://speakhapi.com/blog/otter-ai-privacy-alternative (Hapi; Otter cloud-only) · https://www.fathom.ai/overview · https://guide.fireflies.ai/articles/2973706448-learn-about-fireflies-supported-languages (Polish #82) · https://techcrunch.com/2025/12/05/meta-acquires-ai-device-startup-limitless/ · https://www.digitalocean.com/resources/articles/what-is-notebooklm · https://medium.com/deepcite/notebooklm-is-great-but-is-it-right-for-your-most-sensitive-data-81e206aecb04 · https://github.com/brianpetro/obsidian-smart-connections (issue #356 — 2× embedding bloat)
- Demand / risk: https://www.remio.ai/post/reddit-users-are-turning-granola-into-the-new-meeting-attention-debate (**unsourced "-18%" — vendor content**) · https://www.aitooldiscovery.com/guides/granola-ai-reddit (top complaint = pricing) · https://www.mindstudio.ai/blog/ai-second-brain-knowledge-base-automated-processing-2 (capture works, retrieval doesn't) · https://natlawreview.com/article/take-note-new-wave-privacy-litigation-targets-ai-notetaker-otterai (Brewer v. Otter) · https://github.com/dannymcc/Granola-to-Obsidian · https://forum.obsidian.md/t/plugin-granola-meetings-simple-sync/111950

**Code (this repo, branch `murmur`, v0.4.0 — verified this session):**
- Redaction FIXED (both cloud providers + fail-closed consent): `summarize/mod.rs:41-99`; name hole `summarize/redact.rs:5-6`
- Broken `LIKE` search: `storage/db.rs:406-445`, `db.rs:1521-1569`
- **Confirmed Ask blind-spot bug:** `summarize/vault_context.rs:38-44` (3-arg empty-unlock shim, TODO `:34-37`, budget `:25`, recency fallback `:68-69`); callers `commands.rs:1104` (`ask_vault`), `commands.rs:1323` (`pre_meeting_brief`); fix uses `state.unlocked_folders` (cf. `commands.rs:1809`, `1973`, `2051`)
- Entity graph (Dossier/GraphRAG substrate): `db.rs:194-212`, readers `db.rs:1712-2024` (all `visibility_clause`-gated), `build_entity_detail` `db.rs:2002-2024`; extraction `summarize/graph.rs:16-21,52-64`
- Lock/seal to mirror: `visibility_clause` `db.rs:2057`; seal `db.rs:1155/1373/1427`, verify-before-destroy `crypto.rs:115-135`; rules `.claude/rules/lock-model.md`
- Action-item bridge (dropped due-date): `summarize/action_items.rs:8-26`, `commands.rs:867-877`, `:829`
- MCP (3 tools, extensible): `mcp.rs:220-330`; SQLCipher/FTS5-free: `src-tauri/Cargo.toml`; ONNX already linked: `sherpa-onnx` + `whisper-rs`
- Prior doc updated/challenged: `docs/research/2026-06-27-killer-feature-brain2-multisource.md`
