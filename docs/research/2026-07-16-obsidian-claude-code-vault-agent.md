<!-- Generated 2026-07-16 via /research (5 murmur-researcher angles, each adversarially verified by an independent agent against the live tree ~v0.9.17 + fetched primary sources). Star counts / pricing / repo states = point-in-time 2026-07-16. -->
# Research: The "Obsidian + Claude Code second brain" trend (Karpathy LLM Wiki + kepano skills) — does Murmur already do this, is it worth building, and how to do it better

## TL;DR / Verdict

**The trend is real and durable — and it is Murmur's biggest distribution opportunity, not a threat, provided we move in weeks.** The viral posts conflate two independent artifacts (kepano's *format* skills, Jan 2026; Karpathy's *LLM Wiki* gist, Apr 2026) and embellish heavily ("deletes stale notes" — false; "41k stars in weeks" — 42.2k over 6.5 months; the "China 15k-note vault" story — untraceable). The durable core is the **ingest → consult → lint** triad over a folder of markdown, with the agent as wiki *maintainer*.

Against that triad, Murmur today: **CONSULT ~80% done and better-gated than raw Claude Code** (agentic loop + hybrid FTS5/vector retrieval + `[[Title]]` citations + visibility gating); **AUDIT ~40% done as substrate** (bitemporal facts = deterministic contradictions, Re-Truth supersede callouts, four background loops incl. a weekly-scoped one, the `brief_runner` propose→accept chassis, and — as of PR #335 — a shipped note↔note backlinks reader) but **no vault-audit loop is wired**; **INGEST is the real gap** (`.md`/`.txt` one-shot + typed text only; no URL/PDF/atomic splitting).

**One ticking bomb found:** Murmur's export is one-way and **clobbers external edits** — a user running the viral workflow over a vault containing Murmur notes will silently lose their agent's edits on the next `overwrite_note`. Fix before the trend reaches our users.

**Recommendation (tiered, "embrace + out-execute"):**
1. **S — export-collision guard** (hash-check before overwrite; conflict sibling instead of clobber).
2. **S — Murmur skill-pack + "use Murmur with your agent" docs** (ride the wave now; the 9-tool read-only MCP is already built).
3. **M — native "Vault Audit"**: deterministic-first (zero tokens, zero egress) findings — orphans, staleness, contradiction candidates, unlinked mentions — into a briefs-style propose→accept inbox; weekly schedule on the existing tick pattern. **Never auto-delete.**
4. **M/L — later**: on-device NLI demoter tier (1-day @Mac spike gates it), ingest expansion (URL/PDF/atomic split with dedup-before-create), MCP proposal tools.

---

## What we already have (verified in code, ~v0.9.17, symbols spot-checked by independent verifiers)

### The triad map

| Viral capability | State | Evidence (file:symbol) |
|---|---|---|
| INGEST: drop a file | PARTIAL | `commands.rs::import_document_inner` — `.md`/`.txt` only, one `documents` row; `import_text_inner` covers "pasted thought". Chunked+embedded via `db.rs::index_document_chunks` (FTS always, e5 vectors when model present). |
| INGEST: URL / PDF / video / audio-file | MISSING | No parsing crate, no URL-ingest path into documents (verified; the `reqwest::get` sites are model downloads in `redact.rs`/`embed.rs`, not ingest). |
| INGEST: split into atomic linked vault pages | MISSING | DB chunks are retrieval substrate, never materialized as vault notes. `embed.rs::segment_topics` (TextTiling-style) exists for transcripts — a reusable splitter core. |
| INGEST: auto-plug into existing knowledge | PARTIAL (meetings only) | Anti-hallucination linking: `export/obsidian.rs::list_vault_titles` ("link only these") fed to the prompt; `entity_stub.rs::ensure_entity_backlink`; post-export `commands.rs::link_related_notes_inner`. Imported documents get **none** of this. |
| CONSULT: ask, answered from own notes, with citations | HAVE | `agent.rs::run_agentic_loop` + `tools.rs::GatedToolExecutor` (search FTS+semantic, get_meeting/get_document, dossiers, commitments, `query_database`); citation discipline in `vault_chat.rs::agentic_system(_jit)`. Hybrid retrieval ≫ "Claude Code greps .md". |
| AUDIT: contradictions | PARTIAL | Deterministic + shipped: `facts.rs::reconcile_facts` (bitemporal, supersede-not-delete) + Re-Truth `build_supersession_rows` → `preview/apply/undo_supersessions` → `[!superseded]`/`[!supersedes]` callouts (`append_supersession_callout`). Event-driven per new meeting, human-gated — **not a sweep**, meetings only. |
| AUDIT: staleness | PARTIAL (ingredients) | `memory.rs::compute_recency` (0.995^h) + `proactive.rs::recency` (90-day half-life); `valid_to IS NOT NULL` = superseded by construction. **No note-level staleness signal, no last-opened tracking.** |
| AUDIT: orphans + reconnection | PARTIAL (new) | Backlinks reader **shipped** (PR #335): `db.rs::extract_wikilink_titles` + `backlinks_for_visible` (fail-closed dual gates) + `get_backlinks` command. On-demand scan, no persisted link table → degree-0 orphan sweep possible but O(n·scan) weekly. No orphan pass exists yet. |
| AUDIT: scheduled background job | HAVE (pattern), MISSING (job) | Four loops in `lib.rs` setup: hourly `memory::consolidation_tick` (already contains a once-per-ISO-week rollup scope), 60s `brief_runner::brief_tick` (schedule table, `should_fire` `>=` = catch-up-on-launch, claim-before-run), org sync, screenshare watcher. **No vault-audit tick.** |
| SURFACE for external agents | HAVE | `mcp.rs`: loopback, token-gated, **9 read-only tools** (incl. structured transcripts, `get_document`, `query_database`) — plan doc features D + C-tool already shipped. No write/proposal tools, no skill-pack. |

### Safety machinery a gardener inherits for free
- **Lock posture for batch jobs** (lock-audited precedent): corpus via `visibility_clause` readers with the **EMPTY unlock set** (`brief_runner.rs`, `memory.rs` header: "a background job must never see session-unlocked plaintext"); `purge_pending_brief_runs_tx` called from inside every seal transaction; `seal_epoch` TOCTOU abort in `run_consolidation_pass`. Window close **relocks + zeroizes the KEK** (`relock_and_zeroize_on_lifecycle`) before hiding to tray — consistent with empty-unlock-set jobs.
- **Safe vault writes**: `apply_supersessions` re-gates at apply time (TOCTOU), stores pristine pre-images durably before writing, appends idempotent managed callouts (`RETRUTH_SECTION`), full undo. `verify.rs::apply_verify_callout` same family. Atomic `write_note` (collision → `" (N)"`), `overwrite_note` (tmp+rename).
- **Egress plumbing**: `cloud_egress_consented` fail-closed, redaction firewall wraps **every** cloud provider including `claude_code` (CONFIRMED at symbol level: `summarize/mod.rs::egress_is_cloud` + `make_provider_resolved` + test `cloud_providers_are_redaction_wrapped`), egress ledger.
- **Sealed folders' vault `.md` files are deleted on seal** — the vault surface is *already privacy-filtered for third-party agents*. A kepano-style Claude Code pass physically cannot see sealed content. This is a marketable guarantee no raw-vault workflow can make.

### The ticking bomb (critical finding)
**One-way export clobbers external edits.** Every note mutation writes DB-derived markdown over the file: `update_note_inner` → `overwrite_note` (lock-gated, but no content-hash/conflict check), `link_related_notes_inner`, enrich/recipe/verify appends, re-exports. `write_note`'s `" (N)"` collision logic protects only at create time. External edits never reach the DB either, so Ask/MCP answer from pre-edit content — silent two-brain divergence. Only `apply_supersessions` (read-then-append) respects external edits today. **Any user adopting the viral workflow on a vault with Murmur notes will lose agent edits.**

---

## Findings

### 1. The phenomenon — fact vs hype (confidence: HIGH, primary sources fetched)

- **Karpathy's actual artifact**: gist "LLM Wiki — A pattern for building personal knowledge bases using LLMs" (Apr 4, 2026; fetched in full). Three layers — immutable `raw/` sources, an LLM-owned `wiki/` of markdown pages, a `CLAUDE.md` schema — and three operations: **Ingest** ("a single source might touch 10–15 wiki pages"; he prefers one-at-a-time *with him in the loop*), **Query** (answers filed back as new pages — explorations compound), **Lint** ("periodically… contradictions between pages, stale claims that newer sources have superseded, orphan pages with no inbound links, missing cross-references, data gaps"). Plus `index.md` (read first) and append-only `log.md`. "Obsidian is the IDE; the LLM is the programmer; the wiki is the codebase" is verbatim his.
- **Deviations in the viral posts**: lint **flags, never deletes**; there is **no weekly automation** (manual, interactive); "400k words" appears nowhere in the gist ("~100 sources, ~hundreds of pages" does); at larger scale Karpathy himself points to [qmd](https://github.com/tobi/qmd) (local hybrid BM25+vector CLI/MCP) — **i.e. he concedes that at scale you need exactly the retrieval stack Murmur already ships**.
- **kepano's actual artifact**: [kepano/obsidian-skills](https://github.com/kepano/obsidian-skills) — created 2026-01-02, **42,210 stars on 2026-07-16** (GitHub API), MIT. Exactly **5 skills, all format teachers**: `obsidian-markdown` (wikilinks/embeds/callouts/properties), `obsidian-bases`, `json-canvas`, `obsidian-cli`, `defuddle` (web→clean markdown). **No ingest/consult/audit workflow ships in the repo; no broken-link checker.** "41k stars in weeks" compresses 6.5 months. Obsidian also shipped an agent-oriented official CLI (1.12, Feb 2026; search-verified) — the platform we live inside is actively building for agents.
- **Community reality** (HN 296-pt + 260-pt threads, 3.3k-star [obsidian-second-brain](https://github.com/eugeniughelbur/obsidian-second-brain) with 44 commands + cron-scheduled agents): the three-command trio is convergent and real; "runs quietly weekly" is **cron hacks, nobody has a daemon** — Murmur's in-process loops are architecturally ahead. What works: genuine cross-source synthesis (68 files → "210 concept pages, 4,597 cross-references"). What fails: **cost** (~12M tokens for a 155k-word ingest; the ecosystem's most-shared content is cost-mitigation — "95% token reduction" patterns), **drift/error accumulation**, **context bloat**, **pollution** (advice: keep generated content separate), and **safety = "use git and pray"** — no verify-before-destroy anywhere.
- **Hype table**: "killed the second brain industry" = engagement bait fusing two unrelated artifacts; "deletes stale ones" = FALSE at the source; "China guy revived a 15k-note vault" = **no primary source exists**; "21M read the gist" = unverifiable. **Durability = HIGH**: Jan→Jul artifact lineage, official Obsidian endorsement, multiple 1k+-star productizations — the July posts are the *third* wave of the same trend.

### 2. The math — how to audit a vault without destroying trust (confidence: HIGH on structure, sources fetched)

- **Contradiction precision is the trend's Achilles heel.** LegalWiz (arXiv 2510.03418): naive pairwise NLI = **16% precision**; LLM-only ≈ 32%; a hybrid **blocking → NLI → LLM-judge** pipeline = 66% precision / 77% recall. ContraDoc (2311.09182): even GPT-4 unreliable on nuanced self-contradictions. SummaC (2111.09525): NLI must run at sentence/claim granularity, never whole documents. Implication: **never surface raw NLI labels; a weekly digest of false contradictions kills the feature in one run.**
- **Murmur's structural edge**: contradiction over the bitemporal facts store is **O(1), deterministic and citable** — same `(entity, subject, predicate)`, different object, overlapping validity — vs the viral loop re-inferring contradictions every pass at 16–32%-precision cost. `reconcile_facts` already does this at ingest; a *sweep* is a SQL query.
- **Recommended tier ladder (contradictions)**: (1) deterministic facts reconcile [have] → (2) blocking = shared-entity join + e5 cosine [have the data] → (3) NLI cross-encoder as a **demoter/filter, never a verdict** — candle-transformers 0.10.2 (our pinned version) already ships `DebertaV2SeqClassificationModel` next to the DeBERTa NER we run; candidate model `MoritzLaurer/mDeBERTa-v3-base-xnli-multilingual-nli-2mil7` (0.3B, MIT, Polish in fine-tune set; ~0.6 GB fp16) — **latency/PL-quality UNVERIFIED, needs the 1-day @Mac spike** → (4) local-LLM judge in the existing sidecar with the `rerank.rs` budget/degrade pattern → (5) optional per-finding cloud via provider seam → (6) human accept via Re-Truth. First NLI deployment = demoting paraphrase false-positives ("shipped"→"released") *out of* the existing supersession queue — the safest possible debut, it only *reduces* false positives.
- **Staleness = deterministic, no ML justified**: `stale(note) = w1·(1−0.5^(age/90d)) + w2·superseded_ratio + w3·inbound-ref decay + w4·entity_drift`. The `superseded_ratio` term (share of a note's facts with `valid_to` closed) is the killer nobody else has: **explainable staleness with a citation** ("Atlas status changed in [[2026-07-02 sync]]").
- **Orphans + reconnection**: degree-0 over the (now shipped) wikilink scan; reconnection = RRF-fuse of three signals we already store — exact title-mention (highest precision), entity-Jaccard, e5 kNN (use *rank*, not absolute cosine — e5 scores compress into a narrow band). Beats Smart Connections (5.3k stars, cosine-only) on ranking signals.
- **Atomic splitting (ingest)**: embedding-breakpoint segmentation (LlamaIndex `SemanticSplitter`-style percentile method) ≈ 150 LoC over the shipped e5 embedder + `segment_topics` cousin; sidecar titling per-chunk (respects the <512 B GGUF schema limit); **dedup-before-create** (kNN against `note_chunks` → link instead of create) is what prevents the viral demo's "2,000 notes, mostly noise" failure mode.
- **Eval**: extend the shipped bake-off harness (`eval/mod.rs` `LabeledSet`) — contradiction fixture with a *paraphrase* class (gate NLI on >~0.8 paraphrase-demotion precision); link suggestions via hide-20%-of-existing-wikilinks recall@k (no labeling needed). Weekly audit cost for a 1k-note vault: blocking = SQL (sub-second); ~1–3k NLI pairs + ~50–100 judge calls ≈ **<10 min background**, inside the thermal governor + heavy-inference semaphore.

### 3. Architecture — the gardener is a fifth loop, not new infrastructure (confidence: HIGH)

- **Scheduling**: ship **"Audit now" first** (trust + verifiability), then weekly via the `brief_runner` semantics — `should_fire`'s `>=` + `last_run_at` claim-before-run already give catch-up-on-launch and storm-proofing. Reject launchd/headless (would need its own SQLCipher DEK release outside the app session — keychain ACL risk). Inherit the launch-freeze lessons verbatim: first tick a full interval after launch, `spawn_blocking`, RAM gate, heavy-inference semaphore, skip at thermal ≥ Serious.
- **Lock model**: clone the `brief_runs` posture wholesale — EMPTY unlock set by construction, `purge_pending_audit_findings_tx` inside **every** seal transaction (enumerate all `purge_pending_brief_runs_tx` call sites; lock-security owns that audit), `seal_epoch` abort mid-pass, accepted findings blank their evidence on consume.
- **Egress tiers**: Tier 0 deterministic (broken links / orphans / facts-SQL contradictions / staleness) = zero LLM; Tier 1 local light reasoner ("NEVER cloud" — the `consolidation_tick` posture); Tier 2 cloud **per-finding on explicit click** through provider seam + redaction + ledger. A silent weekly cloud sweep would violate the loud-egress rule — never build it.
- **Actuation**: findings inbox = `features/audit/` cloned from `features/briefs/`; accepted fixes = append-only managed callouts (`[!stale]`/`[!contradicts]`/`[!orphan]` under a managed `## Audit` section, Re-Truth idempotent shape), `[[links]]` restricted to `list_vault_titles`, front-matter flags, and **archive-move as the human-approved "delete"**. Deletion is not an action. This converts the trend's scariest behavior into Murmur's trust story.
- **Vault co-editing seam**: (a) skill-pack now — teach the user's Claude Code to query **our MCP** instead of grepping, and to treat `## Re-Truth updates`/`## Audit` + Murmur front-matter as managed regions (complements kepano's format skills, which cover none of this); (b) at most one narrow MCP **proposal** tool (`propose_audit_finding` → pending row, never content mutation) — keeps MCP effectively read-only, rides the consent gate; (c) full vault watcher = the real two-way-sync problem — **defer**; bounded version = an audit pass that mtime-diffs Murmur-owned `exported_path` files and raises `kind='external_edit'` findings ("re-import?") instead of silently syncing. SQLite stays canonical under all three.

### 4. Positioning — opportunity, decisively (confidence: HIGH)

- **What the trend cannot touch**: content *creation* (far-side capture → on-device ASR → auto-linked atomic notes — their loop starts at documents, ours at spoken conversations); privacy gating (Claude Code over plaintext sees *everything, always* — community guidance is literally "be careful"; our sealed folders are invisible even on disk); deterministic audit substrate (bitemporal facts); zero-marginal-cost judgment (their 5k-note deep audit ≈ $25–50/pass API or a week's subscription quota; our deterministic pass = $0, and judgment runs over a *small findings corpus*, not the whole vault); E2EE sharing/org brain.
- **What it does better than us today (honest)**: whole-vault coverage (we index only Murmur-made + imported content), arbitrary file editing, the audit loop as a product, user-editable behavior (SKILL.md vs compiled prompts), frontier-model judgment quality (mitigated: `claude_code` is our default provider, redaction-wrapped — CONFIRMED).
- **Category precedent**: Granola shipped a **hosted** MCP server (2026-02-04) marketed as "your meeting context comes with you into Claude Code" — a *local* MCP + skill-pack is the same story with a strictly better privacy posture, and ours is already built. Skill-pack vendor precedent exists (Anthropic marketplace); no precedent for writing skills *into* the user's vault — default to a repo + docs page.
- **Messaging (respecting the no-competitor-name-drops rule; platform names OK)**: "Meetings become atomic, linked notes — no ingest step." / "The audit that doesn't bill you: zero tokens, zero egress, every week." / "Locked means locked — even from the AI." / "Bring your own agent" (skills + local MCP). Trend/personality references live on social only, never product copy.

---

## Fit with Murmur's constraints

| Constraint | Verdict |
|---|---|
| Local-first | Default audit = zero egress (Tiers 0–1). Cloud = per-finding, consented, redacted, ledgered. Skill-pack egress happens in the *user's own* Claude Code — document loudly. |
| Obsidian-native | Outputs are plain callouts/wikilinks/front-matter; skill-pack is plain files; aligning exports with kepano's conventions makes Murmur notes maximally legible to any agent. |
| SQLite-canonical | Findings are rows; vault callouts are projections of accepted rows; MCP writes (if any) are proposals into SQLite. The collision guard protects the projection, not a second truth. |
| Lock model | EMPTY unlock set + purge-on-seal + seal-epoch abort — all three mechanisms shipped and lock-audited; audit copies them. New derived-plaintext table + new seal hook ⇒ **lock-security-reviewer mandatory**. |
| Additive migrations | `audit_findings` (+ optional schedule rows) = `CREATE TABLE IF NOT EXISTS`. |
| No new deps | Scheduler = house sleep-loop pattern; NLI tier needs **no new crate** (candle class exists), one new *model download* (user-approved); no `notify` crate for slices 1–3. |
| Verify-before-destroy | Trivially satisfied by never destroying: flag-only + human accept + archive-move + undo. |
| CI honesty | Pure cores (due-logic, scoring, purge tx, callout idempotence) headless; NLI latency/PL quality/e5 thresholds = `#[ignore]` @Mac, same split as embedder/NER. |

## Options & tradeoffs

| # | Option | Effort | Risk | Unlocks |
|---|---|---|---|---|
| 0 | **Export-collision guard** — store content-hash of last export; on mutation, if on-disk ≠ hash → conflict sibling / re-import prompt, never clobber | S | Low | Safe coexistence with ANY external agent; prerequisite for credibly courting the trend's users |
| 1 | **Skill-pack + MCP marketing** — 2–3 SKILL.md (Murmur vault conventions, managed regions, MCP usage) + docs page | S | Reputational if sloppy; user-initiated egress must be documented | Distribution into the exact ICP at trend peak; the Granola story, but local |
| 2 | **Vault Audit v1 (deterministic)** — `audit_findings` + `run_audit_pass` (broken links, orphans, staleness incl. `superseded_ratio`, facts-SQL contradiction candidates, unlinked mentions) + "Audit now" + briefs-clone inbox + purge-on-seal | M | Low tech risk; covers Murmur-made content only until whole-vault indexing lands | "Your vault reads itself" with zero tokens/egress; UX skeleton + telemetry for later tiers |
| 3 | **Weekly schedule + local judge tier** — `should_fire` semantics + sidecar contradiction/staleness judging under the v0.9.12 resource gates; per-finding cloud "explain" | M | Background LLM load (gated) | The actual "never sleeps" promise, fully local |
| 4 | **NLI demoter** — mDeBERTa-NLI via `DebertaV2SeqClassificationModel`; first as paraphrase-demoter on the supersession queue, then blocked cross-note pairs | M | PL quality/latency unmeasured → the spike + eval fixture gate it | Contradictions beyond exact predicate matches — what the viral posts promise and can't deliver precisely |
| 5 | **Ingest expansion** — URL (defuddle-style) / PDF / atomic-split (embedding-breakpoint) + dedup-before-create + link-back through `list_vault_titles` | L | New parsing deps (approval), vault-write UX | The full "drop anything in" leg — last, because the trend already commoditized it and our capture is the differentiated ingest |
| — | Ignore the trend | — | High: 42k stars, Obsidian shipped an agent CLI, a competitor shipped MCP-for-meetings — "AI over notes" is repricing to $0 | — |

## Recommendation & first step

**Build, in order 0 → 1 → 2 → 3, with 4–5 gated behind their spikes.** The durable insight of the trend is "the agent *maintains* the wiki"; Murmur already owns the maintenance substrate the community is hacking together with cron and prayer — bitemporal facts, supersede-not-delete, propose→accept, gated batch reads, four background loops. The two honest gaps are ingest breadth (defer; our capture IS the differentiated ingest) and the vault-facing audit loop (build; mostly assembly).

**Smallest verifiable first slice (one PR):** the `audit_findings` migration + `run_audit_pass` with two deterministic passes (broken `[[links]]` via `list_vault_titles` diff; orphan notes via in/out-degree over `documents(kind='note')` bodies read with the EMPTY unlock set) + `purge_pending_audit_findings_tx` wired into the same seal transactions as the brief purge + RED→GREEN pair: (1) a sealed folder contributes zero findings, (2) sealing purges its pending findings. That de-risks the only novel invariant (findings-vs-seal); everything after is pattern-cloning.

**Parallel de-risking spikes:** (a) run kepano's skills + Claude Code over a real Murmur-exported vault and record what breaks (validates the skill-pack + export-convention alignment + the collision guard's urgency); (b) the 1-day @Mac NLI spike (load mDeBERTa-NLI in candle, measure per-pair Metal latency + RAM, score a ~100-pair PL+EN contradiction/paraphrase/unrelated fixture).

## Open questions / what could not be verified

- Per-pair NLI latency + RAM on Apple Silicon via candle, and Polish NLI transfer quality — needs the @Mac spike (no published benchmark exists).
- Whether orphan detection needs a persisted `note_links` table for O(1) degree queries vs the shipped on-demand scan run weekly — profile at real vault sizes first.
- Whether `brief_schedules` should generalize (a `job_kind` column) vs a sibling `audit_schedules` table — decide during implementation.
- Real Pro/Max quota consumption of a full-vault lint (community reports imply pain; no hard numbers).
- Do *our* users run Claude Code at all? A lightweight demand signal would re-rank option 1 vs 2.
- Viral-post specifics that remain unverifiable: the "China 15k-note vault" story (no source), "21M read the gist" (secondary only), kepano's exact star-growth curve.
- Whether the FE standalone-note update path mirrors the meeting-note clobber exactly (highly likely; read it end-to-end when building option 0).

## Sources

**Primary (fetched):**
1. https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f — Karpathy, "LLM Wiki" (Apr 4, 2026); the canonical ingest/query/lint pattern.
2. https://github.com/kepano/obsidian-skills — 5 format skills, MIT; 42,210 stars on 2026-07-16 (GitHub API); `obsidian-markdown/SKILL.md` read in full.
3. https://news.ycombinator.com/item?id=47640875 (296 pts) + https://news.ycombinator.com/item?id=47963913 — working/failing reports, the ~12M-token ingest datapoint.
4. https://github.com/eugeniughelbur/obsidian-second-brain — 3.3k stars; 44 commands, cron-scheduled agents; the most complete community productization.
5. https://arxiv.org/html/2510.03418v2 (LegalWiz — the 16%→66% contradiction-precision ladder); https://arxiv.org/abs/2311.09182 (ContraDoc); https://arxiv.org/abs/2111.09525 (SummaC); https://arxiv.org/abs/2304.03442 (Generative Agents recency).
6. https://docs.rs/candle-transformers/0.10.2/candle_transformers/models/debertav2/index.html — `DebertaV2SeqClassificationModel` in our pinned candle.
7. https://huggingface.co/MoritzLaurer/mDeBERTa-v3-base-xnli-multilingual-nli-2mil7 — the NLI candidate (0.3B, MIT, PL in fine-tune set).
8. https://github.com/brianpetro/obsidian-smart-connections; https://developers.llamaindex.ai/python/examples/node_parsers/semantic_chunking/ — reconnection/splitting prior art.
9. https://www.granola.ai/blog/granola-mcp — the category's hosted-MCP precedent (2026-02-04).
10. https://platform.claude.com/docs/en/about-claude/pricing — cost model for the weekly-audit economics (fetched 2026-07-16).

**Secondary (search-verified, not fetched):** Obsidian CLI GA in 1.12.4; aimaker.substack.com `/ingest-url`+`/process-inbox`+`/lint-wiki` walkthrough; mindstudio.ai 95%-token-reduction case study; starmorph.com separation guidance; the viral X posts (unfetchable).

**Key code (all opened by agents, symbols re-verified by independent verifiers):** `src-tauri/src/facts.rs` (`reconcile_facts`), `commands.rs` (`import_document_inner`, `import_text_inner`, `build_supersession_rows`, `preview/apply/undo_supersessions`, `link_related_notes_inner`, `update_note_inner`), `export/obsidian.rs` (`write_note`, `overwrite_note`, `list_vault_titles`, `RETRUTH_SECTION`, `append_supersession_callout`), `export/entity_stub.rs`, `storage/db.rs` (bitemporal facts, `backlinks_for_visible`, `extract_wikilink_titles`, `brief_runs` + `purge_pending_brief_runs_tx`, `list_notes_visible` family), `memory.rs` (`consolidation_tick`, `compute_recency`, seal-epoch guard), `proactive.rs`, `brief_runner.rs` (`should_fire`, claim-before-run, EMPTY unlock set), `agent.rs`/`tools.rs`/`summarize/vault_chat.rs` (the Ask loop), `mcp.rs` (9 read-only tools), `embed.rs` (`segment_topics`, `rrf_fuse`, e5), `summarize/ner_deberta.rs` (candle DeBERTa precedent), `rerank.rs` (budgeted judge pattern), `reason.rs` (sidecar), `eval/mod.rs` (bake-off harness), `summarize/mod.rs` (`egress_is_cloud`, `make_provider_resolved` — redaction wraps `claude_code`, CONFIRMED), `lib.rs` (the four background loops; `relock_and_zeroize_on_lifecycle` on window close).
