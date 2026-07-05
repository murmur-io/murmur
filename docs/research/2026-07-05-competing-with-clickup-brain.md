<!-- Generated 2026-07-05 via /research (1 code-grounding murmur-researcher + synthesis over 2026-07-02-clickup-brain-gap-analysis.md and 2026-07-05-connectors-live-vs-rag.md). ClickUp facts = point-in-time from the 07-02 report. -->
# Research: Competing with ClickUp Brain — the plan now that the brain + connectors decisions are in

## TL;DR / Verdict

**The three gaps the 2026-07-02 gap-analysis called "positioning-critical" are now CLOSED in shipped code (0.7.0–0.7.4).** Persistent user memory exists, is injected into all four answer surfaces, is auditable and lock-gated (`user_memory.rs`); threads persist (`assistant_interactions` + `thread_id`); Ask runs on the unified agentic loop with a deterministic floor (`commands.rs::ask_vault`, "PR G"); proactive/ambient ships twice over (zero-egress `proactive.rs` cards + `brain_reactions.rs` "Whisper"). The bake-off validated semantic retrieval (recall@5 1.00 vs 0.42, test corpus).

**What actually separates us from ClickUp Brain today is exactly ONE pillar: connected sources (their 9 vs our 3).** And the connectors research (2026-07-05) already settled the architecture: live, per-source, no RAG index. The competitive plan is therefore short:

1. **Fix the "dead on fresh install" trap first (S).** Memory extraction returns empty on the stub reasoner and semantic falls back to FTS until the user manually downloads models — so the two class-membership features we shipped are OFF for a fresh user. One-click / onboarding-prompted brain-model download is the cheapest, highest-leverage competitive move on the list.
2. **Ship the dev-stack connectors ClickUp does NOT cover: Jira → Slack → Linear (M each).** Brain MAX indexes Drive/Figma/GitHub/SharePoint/Slack/Dropbox but **not Jira, not Linear, not Notion, not Asana** — the dev/consultant niche is an open flank. Slack is the one overlap and the highest "what did we decide in #eng?" demand.
3. **Ship the connector-verify pass as the differentiator (M).** `> ✓ confirmed in PROJ-123 / ⧗ conflict` inline markers (deterministic judge, live source) is a feature ClickUp doesn't have and its complaint profile (hallucinations, "~70% accurate" standups) is weakest against.
4. **Don't chase:** 100+ connector breadth, artifact generation, team multiplayer, auto-executing write agents, multi-model routing.

**The claim we can now honestly earn** (after step 1): *"A real AI knowledge manager — persistent memory, cited answers, proactive recall, and live context from your Slack/Jira — entirely on your Mac, no per-seat AI fee."* ClickUp structurally cannot follow (their privacy is subprocessor contracts; ours is architecture).

## Co już mamy (code-grounded, post-0.7.4)

| Gap (2026-07-02 scoring) | Stan dziś w kodzie |
|---|---|
| **Memory 3/10** — "threads RAM-only, facts unconsumed, no user memory" | **CLOSED.** `user_memory.rs`: bitemporal user facts (`user_facts` table), deterministic `synthesize_brief` injected into in-meeting @brain (`transcribe/live.rs:580`), agentic Ask + floor (`commands.rs:2650/2587`), meeting-chat (`commands.rs:1658`); FE audit view `brain-memory/` + `forget_user_fact`/`clear_user_memory`; lock-gated (brief from visible facts only, purge-on-seal tested). Threads persist (`db.rs:355/534`, gated `list_assistant_threads_visible`, FE rehydrates). *Partial star:* entity facts surface via the `get_entity_dossier` tool + live cards, not an always-injected brief (by design). |
| **Ask 6/10** — "unify onto agentic loop; run bake-off" | **CLOSED (test corpus).** `ask_vault` → `run_agentic_loop` with gated tools + tool-trace to FE; deterministic corpus-pack floor RED-proven byte-identical. Bake-off: semantic recall@5 1.00 vs FTS 0.42. `semantic_search_enabled` default ON — **but e5 model is downloaded-not-bundled → fresh installs run FTS.** |
| **Proactive** — "pull forward" | **CLOSED.** `proactive.rs` (zero-egress D1 recall cards, UI `proactive-hint-card/`) + `brain_reactions.rs` (Whisper contradiction cards, on-device light reasoner + deterministic reconcile, UI `whisper-card/`). Postures are DERIVED (`postures.rs`) so "Fully Local" can never mislabel an egressing config. |
| **Connected sources 3/10** | **STILL THE GAP.** Live connectors = `web` + `calendar` only; Slack is a stub (`voice_action.rs:1317` "isn't available yet"). Architecture settled 2026-07-05: live per-source, no RAG index, RAM cache, pin-to-note, verify pass. |

## Findings (synthesis — where the fight actually is)

### (a) What ClickUp Brain has that we don't — filtered by what matters for our ICP

From the 07-02 report (point-in-time), ClickUp's real leads: connected-source breadth (9/3), write-capable agents + artifacts (8/4), team multiplayer, multi-model routing. **For a solo dev/consultant on Obsidian, the only one that changes the felt experience is connected sources** — "the brain knows my tickets and my team's Slack." Artifacts/slides and team-ambient answers are org-shaped; multi-model routing is invisible plumbing (and our counter is sovereignty: a fully-local option they cannot offer). Their complaint profile (G2/reviews, medium conf): inconsistent answers on task data, ~70%-accurate standups, hallucinations on messy data, notetaker missing action items, per-seat billing resentment — i.e. **their weakness is trustworthiness, our verify-pass target.**

### (b) Where we win structurally (they cannot follow)

- **Privacy/ownership 10 vs 4** — on-device brain (Qwen light/heavy, model-presence activated), owned markdown in the user's vault, Touch-ID folder sealing, zero-retention by *architecture* vs their subprocessor *contracts*.
- **Voice/meetings 9 vs 5** — bot-free far-side capture (ScreenCaptureKit) + live transcription + Whisper reactions during the meeting; their notetaker is a $12/mo bot add-on with no live transcript.
- **Economics** — no per-seat AI fee; marginal cost = the user's Mac or BYO key. ClickUp charges $9–28/user/mo on every paid seat.
- **Permission-aware retrieval by construction** — `visibility_clause` everywhere; ClickUp had to acquire Qatalog ($25.4M) for the equivalent.
- **Single-user live connectors get ACL + freshness free** (2026-07-05 report) — the user's token IS the ACL; no mirrored-index treadmill.

### (c) The minimal set that closes the felt gap "brain draws from my tools"

1. **Brain-model onboarding (S)** — the prerequisite. Today `extract_user_fact_candidates` returns empty on `StubReasoner` (`user_memory.rs:226`) and semantic runs FTS until e5 is downloaded (`config.rs:1189`). A fresh user experiences neither memory nor semantic — the two features that justify "knowledge manager." One prominent onboarding step / one-click "Enable the brain (downloads ~2GB, stays on your Mac)" flips both on. Highest leverage per line of code on this list.
2. **Jira connector, live (M)** — strongest search API (JQL Lucene), mutable fields fit the verify model, and ClickUp doesn't cover Jira at all. First slice per the connectors report: `parse_results` fixture test → full wiring.
3. **Slack connector, live (M)** — the highest-demand ask ("what did we decide in #eng?"); replaces the shipped stub; `search.messages` with a pasted `xoxp-` user token (no OAuth server).
4. **Connector-verify pass (M)** — the differentiator: per-claim `> ✓ confirmed in PROJ-123 (In Progress)` / `> ⚠ not found` / `> ⧗ conflict` inline markers, LLM-extracts + deterministic-compare (reuses `reconcile_facts` + `annotate_unverified` mechanics), on-demand + consented, RAM-cached. Directly attacks ClickUp's trust complaints; nobody in the meeting-notes class has it.
5. **Linear connector, live (S–M)** — clean GraphQL, dev ICP, also absent from ClickUp's coverage. ClickUp-as-a-source stays last (weakest API; per the connectors report, the only candidate for a scoped index *iff* live underperforms — and "search your ClickUp from Murmur" is a fun competitive story but low demand until users ask).

With 1–4 shipped, the user-felt sentence becomes true: *ask anything, the brain answers from your meetings + notes + memory + your team's Slack/Jira, cites everything, verifies your notes against reality, and none of it leaves your Mac except the redacted queries you consented to.* That is class membership plus a moat, not feature parity.

### (d) What NOT to chase (honest)

- **Connector breadth (100+ / Brain MAX six)** — unwinnable and partly incompatible with local-first; 3 well-chosen dev-stack connectors beat 100 shallow ones for our ICP. ClickUp's own survey: 77.5% of workers indifferent/relieved if half their AI tools disappeared.
- **Artifact generation** (slides/dashboards/websites) — org-shaped, off-ICP.
- **Team multiplayer / org knowledge** — contradicts single-user local-first; sharing (0.7.x E2EE) covers the "show a note to someone" case.
- **Auto-executing write agents** — their complaint magnet; our propose-accept stance is a feature. (Write-out to Jira/Linear via propose-accept is a *later* track, per the 07-01 report — read-first.)
- **Multi-model auto-routing** — invisible; our posture system already answers the real user question ("what runs where").

## Fit z ograniczeniami Murmur

All five plan items ride existing seams: connectors clone `web.rs` (consent + redaction + ledger free; no new deps), verify reuses `facts.rs`/`grounding.rs`, model onboarding reuses the existing model-download machinery. Nothing touches the lock model beyond already-audited patterns; connector data stays off-disk (live + RAM cache + pin-to-note only). Egress honesty: connector queries are redacted + ledgered + consented per source; verify never enters the zero-egress proactive path.

## Rekomendacja i sekwencja

**Sequence: (1) brain-model onboarding → (2) Jira → (3) Slack → (4) verify pass → (5) Linear → positioning copy.**
Rationale: 1 activates the already-shipped class-membership features for real users (cheapest win); 2–3 close the only pillar ClickUp actually leads on, aimed at the flank they don't cover (Jira/Linear) plus the highest-demand overlap (Slack); 4 converts their weakness (trust) into our headline; 5 rounds out the dev stack. Then update `COMPETITIVE-LANDSCAPE.md`/README with the earned claim — and resolve the **brain2 vs Brain² naming collision** before any public copy (flagged 07-02, still open).

**First step (smallest verifiable slice):** the onboarding audit — reproduce the fresh-install experience (no models downloaded): confirm memory stays empty and Ask runs FTS, then spec the one-click "Enable the brain" flow. In parallel or next: `JiraConnector::parse_results(fixture_json)` RED-before-GREEN per the connectors report.

## Otwarte pytania

- Does the solo AI-knowledge-manager buyer actually cross-shop ClickUp Brain? (Frame is the user's strategic choice; demand evidence not gathered — unchanged from 07-02.)
- Real-vault semantic quality (bake-off was a designed test corpus) and user-memory extraction quality (needs a real Mac + model) — both "needs recorded evidence".
- ClickUp Brain² feature velocity since 06-17 GA — the 07-02 snapshot is 3 days old; re-check before public positioning copy.
- Slack `search.messages` longevity (legacy API) — migration target exists (`assistant.search.context`) if removed.

## Sources

**Internal:** `docs/research/2026-07-02-clickup-brain-gap-analysis.md` (ClickUp facts + gap matrix, all external URLs there) · `docs/research/2026-07-05-connectors-live-vs-rag.md` (live-vs-RAG verdict + per-source plan) · `docs/research/2026-07-04-rag-bakeoff-results.md` · code: `user_memory.rs` (brief synthesis/injection/audit; stub-empty at `:226`), `commands.rs` (`ask_vault` 2528–2833, memory gates 2329–2424), `transcribe/live.rs:580`, `storage/db.rs:355/534/4777` (thread persistence), `proactive.rs`, `brain_reactions.rs`, `settings/postures.rs`, `settings/config.rs:462/1189` (semantic default-ON, FTS fallback), `voice_action.rs:1317` (Slack stub), `tools.rs:37-38`, `connectors/mod.rs`, FE `brain-memory/`, `whisper-card/`, `proactive-hint-card/`.
