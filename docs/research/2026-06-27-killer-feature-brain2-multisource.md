<!-- Generated 2026-06-27 via /research (murmur-researcher fan-out, 5 angles). Pricing/funding/version = point-in-time. -->
# Research: The next killer feature — pushing the brain2 north star (multi-source second brain)

## TL;DR / Verdict

**Build the auto-updating cross-source Entity Dossier — "State of [[Project]] / [[Person]]" — as the flagship. Light it up with Calendar (EventKit) as the safe second source. Ship it on a hardened retrieval backbone (FTS5 now → hybrid vector after a spike), behind a fixed redaction firewall.**

This is the synthesis of two camps that *appear* to disagree but actually reconcile:

- The **"go multi-source"** camp (3 angles) is right that the genuinely uncontested ground is **local-first + file-owned (Obsidian/SQLite) + multi-source + a real entity graph + redaction** — Glean proves the "state of the project across every source" experience is high-value, but it's cloud/enterprise; every *local* peer (Khoj, Reor, Recall) is single-source or graph-less; every *multi-source-with-graph* peer (Mem, Tana, Saner, Granola) is cloud. No one owns the union.
- The **"don't bet the flagship on breadth"** camp (the demand/risk angle) is *also* right: the second-brain graveyard (Mem, Rewind→Limitless, Roam) died of one wound — **capture is easy, retrieval is hard** — and bolting on Slack/Gmail connectors is a perpetual integration treadmill that *contradicts the privacy moat* and deepens the landfill before retrieval is proven.

**The reconciliation:** the killer feature is a *generated synthesis artifact* (the dossier), not a chat box or a Slack connector. It delivers the full "second brain" wow **on voice alone first**, and becomes multi-source *for free* when **Calendar** — the one source that is local-first by construction (EventKit, zero OAuth, zero egress) — lands behind the same `source_type` axis. We keep multi-source as the 18-month *narrative* (it's why the SQLite-canonical, `source_type`/`owner_id`, trait-seam architecture is correct) while the *next release* is retrieval + synthesis depth, not adapter breadth.

**One decisive caveat that gates everything:** the privacy claim is currently partly theater — the default `claude_code` provider bypasses the redaction firewall (`summarize/mod.rs:47-70`; default in `config.rs`). Any feature that sends *aggregated* multi-entity context to the cloud must wait on that fix.

---

## Co już mamy (z repo, z file:line)

The repo on `feat/phase1-inapp-graph` is **further along than the brain2 spec assumes** — verified in code, not docs:

- **Entity graph is persisted in SQLite (SHIPPED).** `entities(id, name, name_ci, kind, …)` + `entity_mentions(entity_id, meeting_id, …)` with FK cascade + indexes (`src-tauri/src/storage/db.rs:172-190`); `upsert_entity` (`db.rs:1406`), `add_mention` (`db.rs:1433`); pipeline persists per-meeting via `build_and_persist_entities` (`pipeline.rs:337-344`, `commands.rs:795-871`); co-occurrence edges between entities sharing a meeting (`storage/models.rs:46`); `get_entity_detail` command (`commands.rs:897`). **The brain2 spec §2.1/§3 "no entities table / entities live only in the vault" is STALE** (git `3c84acd feat(graph): … entities/mentions + dual-sink`). The vault stub (`export/entity_stub.rs`) is now a *mirror* (Sink B), not the source of truth.
- **Four aggregation readers already exist** over the canonical store — Ask-My-Vault (`summarize/vault_context.rs:21-69`, `vault_chat.rs`), Weekly Digest (`summarize/digest.rs:10-30`, already rolls "who owes what" forward), Topic Threads (deterministic cross-meeting chronological clustering, `summarize/threads.rs:19-50`), entity graph. Spec instruction: *generalize their source axis, don't rebuild*.
- **Pre-Meeting Brief takes a generic corpus + subject** (`summarize/brief.rs:7-25`); calendar is wired best-effort.
- **Calendar is already a (weak) second source.** `next_calendar_event()` shells `osascript` → returns **title only** (`commands.rs:1139-1172`; model `storage/models.rs:289-296`; consumed in `record.component.ts:1020`). So a real calendar adapter is an *upgrade*, not greenfield.
- **AppleScript-bridge + Swift-sidecar patterns both exist** to copy: Reminders write via `osascript` (`commands.rs:694-722`); ScreenCaptureKit sidecar `sysaudio.swift` (the EventKit pattern).
- **SQLCipher-encrypted at rest** (`Cargo.toml:43` `bundled-sqlcipher-vendored-openssl`; key-first `db.rs:91-95`) → FTS5 is **compiled in and available today, zero new deps**, but unused.
- **MCP server** exposes 3 read tools (`search_meetings`, `get_meeting`, `list_recent_meetings`, `mcp.rs:143-160`). No entity/dossier tool. We are a server, **not** an MCP client (no OAuth lib, no MCP-client code).

**Known weak points (the real work):**
- **`search()` is `LIKE`, not full-text — proven broken live** (`"test nagrywania"`→5 hits, reversed→0). Two paths: `db.rs:366-408` and `db.rs:1257-1282`. The retrieval muscle a "second brain" lives or dies on is broken.
- **Redaction firewall wraps only `anthropic`** (`summarize/mod.rs:47-70`); redacts emails/cards/phones but **not names** (`redact.rs:5-6`). Default `claude_code` = unscrubbed cloud relay.
- **No `source_type`/`owner_id` columns** yet (`db.rs:114-122`).
- **Entity resolution = exact case-insensitive only** (`upsert_entity` dedups on `name.to_lowercase()`, `db.rs:1409`; within-note `graph.rs:59`). "Anna K." ≠ "Anna Kowalska" ≠ "anna@acme.com".
- **No versioned schema-migration runner** (encryption-only migrator in `migration.rs`; `migrate()` is additive `CREATE IF NOT EXISTS`). Needed before any column add to the real ~18-meeting DB.
- **Voice source #1 is not yet excellent:** system-audio runtime-unverified, mix-to-mono destroys diarization, Polish quality unmeasured.

---

## Findings (per angle; each with URL or file:line + confidence)

### Angle 1 — Prior art: who owns "local-first multi-source second brain"? (nobody)
- **The valuable framing is proven — but only cloud.** Glean's whole pitch is the "system of context / context graph": aggregate email/meetings/tickets/Slack/code into a unified per-project/customer view — exactly the brain2 north star, and it's enterprise SaaS, multi-tenant, cloud. [high] https://www.glean.com/product/system-of-context , https://www.glean.com/blog/context-data-platform
- **The category just had a death + an exit:** Rewind/Limitless (flagship "local digital memory") was absorbed by Meta and **shut down end-2025** — validates demand, vacates the brand. [high] https://rewind.ai/what-happened-to-rewind/ , https://screenpi.pe/blog/rewind-ai-alternative-2026
- **Local peers are single-source or graph-less:** Khoj = closest local multi-source brain but **no Slack/mail/calendar connectors, no entity graph** (https://github.com/khoj-ai/khoj); Reor = fully local but **zero connectors** (https://github.com/reorproject/reor); MS Recall = on-device but **screen-only, Windows-only** (https://learn.microsoft.com/en-us/windows/client-management/manage-recall). [high]
- **Multi-source-with-graph peers are all cloud:** Mem 2.0, Tana, Saner.ai. [high] https://outliner.tana.inc/articles/tana-current-april-2026 , https://www.saner.ai/blogs/second-brain-app
- **Granola = the watch-it competitor:** post-Series C, has Spaces + MCP (Feb 2026) + Slack/Notion/HubSpot + Zapier(Linear) — walking *from* meeting-notes *toward* the multi-source hub, from the cloud side. [high, point-in-time] https://www.granola.ai/blog/granola-integrations-complete-guide-connecting-meeting-tools

### Angle 2 — Which second source? Calendar wins (EventKit, local-first)
- **Ranking (magic ÷ cost, judged local-first): 1. Calendar  2. Apple Notes/Files  3. Linear/Jira/GitHub  4. Slack  5. Gmail  6. browser/screen.**
- **Calendar's non-obvious lever:** EventKit attendees carry name + `mailto:` URL → **attendee email becomes a stable canonical entity ID**, which retires the §9-deferred entity-resolution problem ("Anna K." = "Anna Kowalska") for free. No other candidate gives a clean identity primary key this cheaply. [high on leverage; med on whether non-iCloud providers populate emails — that's the spike]
- **AppleScript has hit its ceiling** — reading attendee emails is a known dead end (https://mjtsai.com/blog/2024/10/23/the-sad-state-of-mac-calendar-scripting/). **EventKit is the right local API**: `EKEventStore.requestFullAccessToEvents` (macOS 14+) + `NSCalendarsFullAccessUsageDescription`; `EKEvent.attendees` ([EKParticipant] with name/role/`mailto:`), organizer, start/end, recurrence, notes. Mirrors `sysaudio.swift` exactly. [high] https://developer.apple.com/documentation/eventkit/ekeventstore/requestfullaccesstoevents(completion:) , https://developer.apple.com/documentation/EventKit/accessing-calendar-using-eventkit-and-eventkitui
- **Why cloud sources rank lower:** Gmail `gmail.readonly` is a **restricted scope → mandatory CASA security audit** (annual, $thousands) to ship to non-test users — a hard distribution blocker for a local-first indie app (https://developers.google.com/identity/protocols/oauth2/production-readiness/restricted-scope-verification , https://deepstrike.io/blog/google-casa-security-assessment-2025). Slack/Linear need OAuth + necessarily egress data to their cloud — local-first is impossible by definition for those sources. [high on Gmail; med-high on Slack]
- **MCP-as-ingest (use our env's Google/Slack MCP connectors as a client): REJECTED for source #2.** It breaks local-first for exactly the source we'd pick (connectors are cloud/OAuth'd), MCP is request/response not a sync engine (no cursor/delta/backfill), and it adds an MCP-client + OAuth to *avoid* a Swift file we already know how to write. Keep MCP as our egress-free *server*. [high]

### Angle 3 — The killer synthesis experience: a generated dossier, not a chat box
- **The cross-source entity dossier is the proven "magic moment":** Clay/Mesh auto-aggregates email/calendar/LinkedIn into a per-person profile and proactively surfaces it (https://me.sh/ [high]); Granola added a People/Companies view auto-organizing notes by relationship (https://www.granola.ai/blog/chat-with-meetings-search-analyze-ai-2026 [med]); personal-CRM (Monica/Dex) converges on "one auto-maintained page per person" (https://getdex.com/guides/finding-the-right-personal-crm/ [med]).
- **Generated artifacts beat chat for synthesis:** NotebookLM Studio's pre-built generations (Briefing Doc, **Timeline**) "often produce better results than asking in chat," each claim cited. [med-high] https://www.solidaitech.com/2026/06/notebooklm-complete-guide.html
- **Demand is "across meetings, what do I owe / what's still open"** — Granola's own canonical query is *"What did I promise to do in my meetings this week?"* — a rollup by owner/entity (a dossier/digest output, not a chat affordance). [high] https://www.granola.ai/blog/chat-with-meetings-search-analyze-ai-2026
- **The bar for proactive briefs:** generated overnight, surfaced as you join, 2-3 cited bullets, shows its provenance, zero setup. [high] https://www.granola.ai/blog/briefs-prepare-you-for-your-next-meeting-as-you-join ; category moving to morning digests (Gemini Daily Brief, https://gemini.google/overview/daily-brief/).
- **Obsidian power-user interaction model = note-centric, owned files, not a dashboard:** favor a generated `.md` they own (backlink/embed/graph) + MCP tools, over a centralized dashboard. [med] https://github.com/brianpetro/obsidian-smart-connections , https://community.obsidian.md/plugins/copilot
- **Why NOT a chat box as flagship:** PKM tools die from non-review ("a vault you never review is a landfill"; the fantasy is "a tool that did the work for them"). Ask-My-Vault (you must remember to open it and phrase a question) is structurally abandonment-prone; the durable wow *shows up already done*. [med, consensus] https://curtismchale.ca/2023/01/22/pkm-in-retrospect-pkm-weekly-jan-22-2023-issue-053/

### Angle 4 — Technical backbone: feasible local-first, no blockers
- **FTS5+BM25 alone is enough for voice-only, NOT for cross-source synthesis** — heterogeneous vocabularies ("ship it 🚀" vs "we agreed to release") break keyword recall exactly where aggregation lives. The answer is **hybrid: FTS5(BM25) ∪ vector KNN fused with RRF**. [high that hybrid > FTS for cross-source; med on magnitude for our corpus → spike] https://alexgarcia.xyz/blog/2024/sqlite-vec-hybrid-search/index.html
- **The vector layer is cheap and fits:** `sqlite-vec` (MIT/Apache) is a `vec0` virtual table in the *same* SQLite file; brute-force KNN does **100k vectors @ 384/768-dim in <75ms** — our corpus is low-thousands of chunks, sub-ms. Registers via static `sqlite3_auto_extension()` (NOT runtime `load_extension`, which SQLCipher blocks). [high; the one integration risk = confirm the static-link path under our SQLCipher build] https://github.com/asg017/sqlite-vec , https://alexgarcia.xyz/sqlite-vec/rust.html
- **Embeddings on-device:** `fastembed-rs` (Apache, ONNX via `ort`, offline cache). **Polish caveat (load-bearing):** default bge-small is English-only; PL recall needs a **multilingual** model (multilingual-e5-small / bge-m3) — bigger bundle, slower embed, **unmeasured = biggest quality unknown.** [high that en-only is wrong; med on which model] https://github.com/Anush008/fastembed-rs
- **Entity-resolution floor is NOT embeddings** — it's layered & mostly deterministic: (1) add email/handle/alias columns (deterministic cross-source join key, no ML), (2) blocking+normalization, (3) embedding cosine tiebreak on the fuzzy tail, (4) LLM adjudication only on the few ambiguous pairs (bounded cost). [high]
- **On-device NER 2-for-1:** `gline-rs` (GLiNER, ~188MB ONNX, shares the `ort` runtime) gives a local NER fast path *and* unblocks **name redaction** (the firewall's current hole). [high on availability; med on PL NER quality] https://github.com/fbilhaut/gline-rs
- **Resolution must run on-device:** the firewall replaces emails with tokens *before* any cloud call, so a cloud LLM can never see the real email to link identity — canonicalization belongs on-device, on un-redacted text. [high]
- **Incremental/cost:** content-hash/mtime watermark per item; embeddings table keyed `(item_id, chunk_idx, content_hash)`; batch embeds; route chatty sources to on-device NER first, LLM only for relation/summary. [high on pattern; cost unmodeled until source #2] https://smartconnections.app/smart-connections/

### Angle 5 — Demand & risk: go on direction, no-go on breadth-as-flagship
- **Demand is bifurcated.** Broad/paying demand is for *meeting notes*, and the market **segmented by depth-in-a-lane, not aggregation** (https://www.useluminix.com/reports/industry-analysis/ai-meeting-notes-comparison-granola-vs-otter-vs-fireflies-vs-fathom-2026 [high]). The **demonstrated** pull is "get my cloud notes into local Obsidian" (Granola-sync plugins, reverse-engineering, forum threads) — validating the *owned-files* thesis more than the multi-source one (https://www.obsidianstats.com/plugins/granola-sync , https://josephthacker.com/hacking/2025/05/08/reverse-engineering-granola-notes.html [high]). The "aggregate Slack+mail+meetings locally" demand is real but **enthusiast/developer loud-minority** (Khoj 34k★, Anytype) [med].
- **The graveyard, one cause of death:** Mem ($110M, full 2.0 rebuild, integration bugs); **Rewind→Limitless** abandoned local-first because "most paying users didn't prioritize the desktop context features or local-only transcription" + perf ("turned MacBooks into a toaster") → pivoted to cloud+wearable; Roam declined as Obsidian won on local plain-text/no-lock-in. [high] https://andrewschreiber.substack.com/p/an-early-adopters-thoughts-on-rewindais , https://techcrunch.com/2024/04/17/a16z-backed-rewind-pivots-to-build-ai-powered-pendant-to-record-your-conversations/
- **Retention truth:** "capture without retrieval is hoarding… a write-only archive." Adding sources multiplies capture and dilutes retrieval → a *broken `LIKE` search is existential, not cosmetic.* [high, consensus] https://medium.com/@ann_p/your-second-brain-is-broken-why-most-pkm-tools-waste-your-time-76e41dfc6747
- **Privacy is a trust-amplifier + a competitor-hypocrisy wedge, not a broad purchase driver.** The privacy paradox is robust (stated concern ≠ willingness to pay) [high: https://www.nber.org/system/files/working_papers/w23488/w23488.pdf], but Recall's backlash shows it's a real trust lever [high: https://thehackernews.com/2024/06/microsoft-revamps-controversial-ai.html], and Granola markets "local" while sending audio to Deepgram/AssemblyAI + summaries to OpenAI/Anthropic, default-opting users into training — a sharp, truthful wedge **only if our `claude_code` default is actually routed through redaction.** [med-high] https://basilai.app/articles/2026-06-20-granola-vs-basil-bot-free-vs-on-device-privacy-architecture.html
- **Adversarial (strongest case against breadth-as-flagship):** integration treadmill a solo app can't maintain; multi-source contradicts the privacy moat; breadth = landfill, depth = retention; *retrieval, not aggregation, is the actual product* — and retrieval needs **no** second source. [the deciding argument for sequencing]

---

## Fit z ograniczeniami Murmur

| Constraint | Dossier (flagship) | Calendar/EventKit (source #2) | Vector+entity backbone |
|---|---|---|---|
| **Local-first / privacy** | ✅ runs over SQLite; only synthesis call egresses — **gated on redaction-for-all-providers fix** | ✅ EventKit = zero OAuth/egress (the *only* candidate that preserves it); emails must be scrubbed before cloud | ✅ fastembed/gline-rs/sqlite-vec all on-device; NER *improves* privacy (redacts names) |
| **Obsidian-native / owned files** | ✅ best-in-class fit — dossier = `.md` + front-matter + `[[backlinks]]` + block-refs; beats cloud Granola | ✅ attendees → `[[Person]]` entities | ✅ vector index lives *inside* the encrypted DB; no second store |
| **SQLite-canonical** | ✅ entities table already exists; needs `source_type` + migration runner | ✅ events land as `source_type='calendar'` items | ✅ embeddings = derived index over canonical rows |
| **Provider seam + redaction** | rides `complete(system,user)`; **must fix `claude_code` bypass first** | ingest-only, untouched | beside the seam, not on it; LLM adjudication still via firewall |
| **macOS / CI honesty** | ✅ pure text synthesis — fully headless-testable (unit-test prompt builder like `digest.rs`) | ⚠️ needs real Mac + Calendars TCC grant + recorded evidence (attendee-email reliability) | ✅ deterministic vectors unit-testable; PL recall/NER need a measured lane |

---

## Opcje i tradeoffy

| Option | Effort | Risk | Unlocks |
|---|---|---|---|
| **A. FTS5+BM25 now** (rewrite both `LIKE` paths behind `db.search()` signature) | **S** (zero new deps) | low | Correct lexical search; MCP/Ask/UI inherit it free. Spec M1. The retrieval-foundation prerequisite. |
| **B. Entity Dossier "State of [[X]]"** (generalize `vault_context` to a source-tagged corpus → emit cited `.md`; +MCP tool #4 `get_entity_dossier`) | **M** | med (entity-resolution fragmentation; gen cost) | The flagship; the visible 1→N-source payoff; the uncontested local+owned+graph+redaction ground. |
| **C. Calendar via EventKit** (Swift sidecar; attendees→entities; threading) | **S** spike → **M** fusion | med (attendee-email reliability across providers; TCC UX) | Honest "multi-source" with zero egress; ground-truth entity IDs that fix resolution. |
| **D. Proactive Daily Brief → Obsidian daily note** (reuse `brief.rs`+`digest.rs` + scheduler + daily-note writer) | **S–M** | low | Granola-Brief-class "shows up done"; highest *certainty of repeat use*. The delivery skin for B. |
| **E. Hybrid vector + entity resolution + on-device NER** (sqlite-vec + fastembed + gline-rs) | **L** | med-high (PL recall unknown; bundle +~300MB; SQLCipher static-link proof) | Real cross-source "what do I know about X"; trustworthy identity; closes name-redaction hole. |
| **F. Slack/Gmail connectors as flagship now** | **L+** & perpetual | **high** | ❌ REJECT — integration treadmill, privacy contradiction, landfill-before-retrieval (the Mem/Rewind failure path). |

---

## Rekomendacja i pierwszy krok

**Flagship = the Entity Dossier (B), delivered via the Daily Brief (D), on voice first, lit up by Calendar (C), atop FTS5 now (A) and hybrid vector later (E). Keep Slack/mail (F) as narrative, not next-release.**

This satisfies both camps: the demand/risk angle gets its "depth-and-retrieval-first, no integration treadmill, no privacy contradiction" sequencing; the multi-source angles get their genuinely-differentiated flagship that *becomes* multi-source the moment Calendar lands — with no cloud OAuth and no new egress.

**Sequenced first steps (smallest verifiable slices, in dependency order):**

1. **Fix the privacy floor (prerequisite, S):** route the default `claude_code` provider through `RedactingProvider`; reclassify all non-Ollama providers as cloud (`summarize/mod.rs:47-70`). Without this, any aggregated-context feature is the same "local" theater we're criticizing.
2. **FTS5+BM25 (A, S):** replace both `LIKE` paths (`db.rs:366-408`, `db.rs:1257-1282`) behind the existing signature. Regression test: `"A B"` and `"B A"` both return the doc with both words.
3. **Migration runner + product-safe columns (S–M):** versioned `PRAGMA user_version` transactional runner + auto pre-migration backup, tested against a **seeded old-schema fixture** (the real ~18-meeting DB, not a fresh one); add `source_type TEXT DEFAULT 'voice'` + `owner_id TEXT DEFAULT 'local'`.
4. **Dossier MVP (B, M):** `build_entity_dossier(db, entity_id)` mirroring `vault_context.rs` → emit a structured cited `.md`: **Overview · 🕑 Timeline of mentions · ⏳ Open commitments · 🧭 Last said / next step**, every claim citing `[[Title]]`; regenerate incrementally on new-meeting-link; expose as MCP tool #4. Unit-test the prompt builder like `digest.rs`.
5. **Calendar spike (C, S) — runs in parallel:** standalone Swift helper (sysaudio pattern): `requestFullAccessToEvents` → events in `[now-7d, now+7d]` → JSON with attendee `{name,email,role}`; persist nothing. **The pass/fail that matters: do attendee emails populate for a real Google-Workspace calendar?** If yes → Calendar is unambiguously source #2 and the entity-ID lever is real. If no → magic collapses to title+time; reopen the ranking.

**The single most decision-relevant de-risking spike** (do this before committing to E): take the real ~18-meeting DB, generate dossiers for the top 3 people and run ~15-20 realistic Polish+English cross-meeting questions three ways (FTS-only / vector-only / RRF-hybrid). This answers in one shot: (a) does retrieval read as "a brain" or as fragmented noise (entity-resolution debt), (b) is FTS5 enough or is the vector layer worth +300MB, (c) which multilingual embedding model wins on Polish. If retrieval scores low even *within voice*, that is decisive proof to harden voice before any source #2.

---

## Otwarte pytania / czego nie udało się zweryfikować

- **EKParticipant email reliability across non-iCloud providers** — load-bearing for the Calendar lever; needs the real-Mac spike. Apple docs confirm the property *exists*, not that it's populated for every CalDAV/Exchange/Google account.
- **Polish embedding recall + Polish GLiNER NER quality** — entirely unmeasured; the bake-off resolves it (mirrors the unmeasured-Polish-ASR caveat).
- **`sqlite-vec` static `auto_extension` against our exact `bundled-sqlcipher-vendored-openssl` link** — high confidence, unproven on this tree; a ~1-day build proof.
- **Entity-resolution severity on the real DB** — exact-string dedup confirmed in code; how badly it fragments real people is unmeasured (the dossier spike measures it).
- **Whether our Ask-My-Vault retrieval is actually good today** — unmeasured; the single biggest unknown; needs a real Mac + the live user DB.
- **No primary Reddit/HN user-voice threads** captured for "aggregate Slack+mail+meetings locally" — the loud-minority read on multi-source demand is med confidence; a direct r/PKMS / r/ObsidianMD / r/selfhosted scrape would raise it.
- All competitor pricing/funding/architecture facts are **point-in-time 2026-06-27** on fast-moving products (Granola/Glean/Khoj especially).

---

## Sources

**External (live URLs, fetched/searched 2026-06-27):**
- Glean system-of-context: https://www.glean.com/product/system-of-context · https://www.glean.com/blog/context-data-platform
- Rewind→Limitless death/pivot: https://rewind.ai/what-happened-to-rewind/ · https://andrewschreiber.substack.com/p/an-early-adopters-thoughts-on-rewindais · https://techcrunch.com/2024/04/17/a16z-backed-rewind-pivots-to-build-ai-powered-pendant-to-record-your-conversations/
- Local peers: https://github.com/khoj-ai/khoj · https://github.com/reorproject/reor · https://learn.microsoft.com/en-us/windows/client-management/manage-recall
- Cloud multi-source-with-graph: https://outliner.tana.inc/articles/tana-current-april-2026 · https://www.saner.ai/blogs/second-brain-app · https://www.fahimai.com/how-to-use-mem-ai
- Granola: https://www.granola.ai/blog/granola-integrations-complete-guide-connecting-meeting-tools · https://www.granola.ai/blog/chat-with-meetings-search-analyze-ai-2026 · https://www.granola.ai/blog/briefs-prepare-you-for-your-next-meeting-as-you-join · https://www.granola.ai/security
- EventKit: https://developer.apple.com/documentation/eventkit/ekeventstore/requestfullaccesstoevents(completion:) · https://developer.apple.com/documentation/EventKit/accessing-calendar-using-eventkit-and-eventkitui · https://mjtsai.com/blog/2024/10/23/the-sad-state-of-mac-calendar-scripting/
- Gmail CASA wall: https://developers.google.com/identity/protocols/oauth2/production-readiness/restricted-scope-verification · https://deepstrike.io/blog/google-casa-security-assessment-2025
- Synthesis prior art: https://me.sh/ · https://getdex.com/guides/finding-the-right-personal-crm/ · https://www.solidaitech.com/2026/06/notebooklm-complete-guide.html · https://gemini.google/overview/daily-brief/
- Obsidian model + abandonment: https://github.com/brianpetro/obsidian-smart-connections · https://community.obsidian.md/plugins/copilot · https://curtismchale.ca/2023/01/22/pkm-in-retrospect-pkm-weekly-jan-22-2023-issue-053/
- Backbone tech: https://github.com/asg017/sqlite-vec · https://alexgarcia.xyz/blog/2024/sqlite-vec-hybrid-search/index.html · https://alexgarcia.xyz/sqlite-vec/rust.html · https://github.com/Anush008/fastembed-rs · https://github.com/fbilhaut/gline-rs · https://smartconnections.app/smart-connections/
- Demand/risk: https://www.useluminix.com/reports/industry-analysis/ai-meeting-notes-comparison-granola-vs-otter-vs-fireflies-vs-fathom-2026 · https://www.obsidianstats.com/plugins/granola-sync · https://josephthacker.com/hacking/2025/05/08/reverse-engineering-granola-notes.html · https://medium.com/@ann_p/your-second-brain-is-broken-why-most-pkm-tools-waste-your-time-76e41dfc6747 · https://medium.com/@theo-james/mem-ai-the-40m-second-brain-failure-burning-the-worlds-money-5f3176a34cbd · https://www.nber.org/system/files/working_papers/w23488/w23488.pdf · https://thehackernews.com/2024/06/microsoft-revamps-controversial-ai.html · https://basilai.app/articles/2026-06-20-granola-vs-basil-bot-free-vs-on-device-privacy-architecture.html

**Code (this repo, `feat/phase1-inapp-graph`):**
- Entities shipped: `src-tauri/src/storage/db.rs:172-190`, `db.rs:1406-1443`; `pipeline.rs:337-344`; `commands.rs:795-871,897`; `storage/models.rs:46`
- Broken search: `db.rs:366-408`, `db.rs:1257-1282`
- Redaction gap: `summarize/mod.rs:47-70`, `redact.rs:5-6,53,80-117`
- Schema/no source_type: `db.rs:114-122`; SQLCipher key `db.rs:91-95`; `Cargo.toml:43`
- Calendar today: `commands.rs:1139-1172`, `storage/models.rs:289-296`, `record.component.ts:1020`; bridge precedents `commands.rs:694-722`, `sysaudio.swift`
- Aggregation readers: `summarize/vault_context.rs:21-69`, `vault_chat.rs`, `digest.rs:10-30`, `threads.rs:19-50`, `brief.rs:7-25`, `provider.rs`; MCP `mcp.rs:143-160`; entity stub `export/entity_stub.rs`
- Spec (partly stale): `docs/superpowers/specs/2026-06-26-brain2-voice-design.md` · baseline `docs/COMPETITIVE-LANDSCAPE.md` · `docs/KILLER-FEATURES.md`
