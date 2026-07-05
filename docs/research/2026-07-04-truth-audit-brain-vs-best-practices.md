<!-- Generated 2026-07-04 via /research Workflow (murmur-truth-audit-brain-deepdive): 29 agents, ~3.23M subagent tokens,
     861 tool calls, ~16 min. FOURTH deep pass (after the three 2026-07-03 reports) with two NEW axes the prior passes never ran:
     (1) a per-domain functional TRUTH-AUDIT — every user-facing promise classified VERIFIED/PARTIAL/DORMANT/BROKEN/MOCKED/
     UNVERIFIABLE_HEADLESS against the code, with a mock/stub/dead-code hunt; the top 17 critical findings each re-verified by an
     independent adversarial agent (17/17 confirmed REAL at high confidence — zero auditor hallucinations survived);
     (2) brain-vs-best-practices — RAG/memory/agentic architecture graded against 2026 SOTA (web-fetched) and ClickUp Brain².
     Trunk `murmur` @ ecf6c7a (post PRs #158–#170). Cite by SYMBOL; line numbers drift. Pricing/versions point-in-time. -->

# Research: Murmur truth-audit + brain deep-dive — does what we promise actually work, and is the brain a real brain?

## TL;DR / Verdict

**1. There are NO mocks presented as features.** The sweep for fake data came back clean in every domain: zero hardcoded/mocked
data rendered as real in the FE, all 113 invoked IPC commands registered, no fabricated counts, no lorem, no fake success paths.
Every stub that exists (StubEmbedder, StubReasoner, NoopNameRedactor, AFM-without-sidecar) is an honest, documented graceful
degrade — and the code is *unusually* honest about its own limits (thresholds labeled "placeholder", headers stating what a green
build does NOT prove). **The dishonesty runs the OTHER direction: the backend is ahead of the UI and the README is ahead of the
defaults.** The real failure classes are (a) **over-sold promises** (README sells an on-device agentic loop that only runs on
cloud connections; a pre-meeting brief whose UI was deleted in PR #108; "default large-v3" when the default is small), (b)
**silent-failure holes** (a 4-hour cap that keeps showing "Recording…" while discarding audio; voiceprint naming structurally
dead for 1:1 calls; StubReasoner's literal `[stub-reason]…` echo surfacing as a real @brain answer), (c) **one genuine privacy
leak** (`vault_titles` — every vault .md filename, including auto-created `[[Person Name]]` pages, egresses to cloud providers
past BOTH redaction layers on every cloud summarize), and (d) **shipped-but-unreachable features** (Trust-Layer grounding,
the Polish mmlw embedder picker, AFM — complete tested backends with no FE surface).

**2. The brain is a real brain — architecturally.** Hybrid 3-leg retrieval (FTS5/BM25 + vector + entity graph, textbook RRF k=60,
correct e5 `query:`/`passage:` prefixes), a bitemporal invalidate-not-delete fact store with provenance + forget UI, a bounded
model-driven tool loop with host-enforced gates, and a deterministic zero-egress proactive layer. The memory substrate is
"Zep/Graphiti-lite, but privacy-gated" — **no reference architecture (Letta, mem0, Zep) and no competitor pairs memory with
purge-on-seal**. What separates it from best-practice-*grade*: (i) the eval harnesses have **never produced a number** (the
exact "most expensive mistake of 2026" — orchestration before measured retrieval), (ii) **transcripts are never embedded**
(semantic recall covers note summaries + docs only; the `source_type` column anticipated it, the pipeline never writes it),
(iii) **no reranking stage at all**, (iv) the loop transport is pre-modern (JSON-in-prompt over stateless `complete()`,
full-context re-send per step, cloud-only). Honest marketing line: **"a private brain over your meetings" — defensible today;
"ClickUp-Brain-class" — not until connected breadth (MCP client) and actuation exist.**

**3. Are we breakthrough?** Still no — but the bundle survives another adversarial 12-app scan with zero counterexamples, and the
market moved *toward* us: Hyprnote dissolved into Char (cloud pivot), **Limitless is dead (Meta acquired + shut down Rewind —
the only privacy-positioned personal-memory rival orphaned its users)**, bot-free capture and MCP went commodity (so the
un-copyable pitch narrows to voiceprint × biometric lock × owned files × the redaction firewall — the last is uncontested across
all 12 apps scanned). The gating items are unchanged and mostly non-code: the name, the dark landing page, a demo, the DER run —
plus, new this pass: **fix the vault_titles leak before leaning on the privacy pitch**, because today the Settings copy
("NAMES are additionally masked before any redacted text leaves this Mac") is falsified by the code.

---

## 1. Truth-audit scoreboard (7 domains, 62 promises classified, 17/17 verified findings confirmed)

| Domain | Verdict (one line) | VERIFIED | PARTIAL | DORMANT | BROKEN | MOCKED | UNVERIF. |
|---|---|---|---|---|---|---|---|
| Capture/audio | Best-engineered DSP + 5-layer degradation; 2 lifecycle holes (cap, crash) | 5 | 2 | 1 | 1 | 0 | 1 |
| Transcribe/voiceprint | Real + radically honest; flagship naming double-OFF + dead for 1:1 calls | 4 | 1 | 2 | 1 | 0 | 0 |
| Brain retrieval | Genuinely well-engineered; "default-on semantic" is a flag, not a capability | 4 | 3 | 1 | 0 | 0 | 0 |
| Agentic brain | Real ReAct loop, fail-closed; "model decides" is cloud-gated vs README's on-device claim | 5 | 4 | 3 | 0 | 0 | 1 |
| Lock/MCP/export | Strongest domain: verify-before-destroy universal; edges leak metadata (vault residuals, list titles) | 7 | 3 | 1 | 0 | 0 | 1 |
| Providers/egress | Single choke-point, fail-closed, mid-flight revoke tested; ONE real leak (vault_titles) | 7 | 2 | 1 | 0 | 0 | 0 |
| FE surfaces | Zero mocked data; 4 complete backends unreachable from any UI | 7 | 2 | 3 | 1 | 0 | 0 |

Status key: DORMANT = shipped but inert by default (flag off / model absent / no UI switch); BROKEN = promised but the path
dead-ends; UNVERIF. = only provable on a signed build / real Mac (honestly so).

**The two BROKEN promises** (README-level): the calendar-aware **pre-meeting brief** (README `Timeline & more` bullet) — its
component was deleted in PR #108 (`c3282e7`) and never re-homed; 4 registered commands + 4 typed IPC methods are dead code
(only the agent's `calendar_lookup` tool survives). And **"default large-v3"** — the code default is `small`
(`config.rs` `model_size_defaults_to_small`, a deliberate RAM-safety change; README/CLAUDE.md never updated). A third
near-BROKEN: **crash recovery of in-flight recordings does not exist** (mic buffer is RAM-only ~0.7 GB/h; the reaper *deletes*
the surviving far-side scratch; `MeetingStatus::Recording` rows are never reconciled — the internal comment "leaves a
recoverable row" promises a mechanism nobody built). README doesn't explicitly promise it, so it's filed as an architectural
gap, but for a meeting recorder it is the worst failure class.

## 2. The confirmed critical findings (each independently re-verified against the code)

### (a) One genuine privacy leak — fix before any privacy-led marketing

- **`vault_titles` egresses un-redacted on every cloud note** — `RedactingProvider::summarize_with_meta` scrubs only
  `transcript`/`related_context`/`user_notes`; `SummarizeRequest.vault_titles` (a recursive scan of every `.md` stem in the
  vault, `export::list_vault_titles`) passes verbatim and `template.rs::render_user_content` embeds it in the cloud-bound
  prompt. The product auto-creates `[[Person Name]].md` pages, so the full people-roster (plus sensitive note titles) leaves
  on every cloud summarize, **even with the NER model installed** — the same side-channel class as the previously fixed
  `user_notes` and speaker-tag leaks, simply missed. Directly contradicts the Settings copy. *(verified REAL, high conf)*
- Corollaries: **web-search queries skip the NER layer and never hit the egress ledger**; the **startup GitHub update check**
  is the only un-toggleable, un-ledgered automatic network call in a "we tell you exactly what leaves" app.

### (b) Silent-failure holes (data loss / wrong answers masked as success)

- **4-hour cap is invisible**: `Recorder::cap_reached` — whose own doc promises a status-poll consumer — has **zero callers**;
  after the cap the capture thread self-stops, the UI timer keeps counting, and everything spoken past hour 4 is silently lost.
- **In-flight recordings are 100% unrecoverable on crash** (RAM-only mic buffer; scratch deleted, not salvaged; ghost
  RECORDING rows forever).
- **StubReasoner echo as a real answer**: with brain = Off, or Local mid-download, an @brain question that finds vault matches
  renders `[stub-reason] system=N chars…` as the assistant's answer, status ok — no `id()=="stub"` guard on the floor path
  (orchestrate.rs has one; the voice/@brain floor does not).
- **Voiceprint naming is structurally dead for 1:1 calls** (the most common meeting shape): ≤1 remote cluster keeps plain
  `others`, the suggestion DTO is keyed `others-{n}`, so the "Looks like Anna?" chip can never render and enroll-on-rename can
  never fire. Cross-meeting re-identification only functions with ≥2 remote speakers. A voiceprint row is still stored.
- **Live captions + live @brain context are mic-only** — on headphones the remote side is invisible until Stop (the system
  prompt honestly tells the model this; no user-facing surface does).
- **Meeting vectors are never rebuilt after unlock/remove-lock** (documents are) — silent, permanent semantic-coverage loss
  for locked folders until a manual full re-index; FTS recovers via trigger so the gap is invisible.
- Minor same-class: opt-in diarization failures degrade silently (log-only); note edits leave stale vectors/snippets;
  a stop-pipeline error leaves the wake-phrase listener dead until restart.

### (c) Over-sold vs the defaults (the README-vs-code gap)

- **"The same model-driven agentic loop" / "on-device brain answers"** — every agentic entry (`run_informational`,
  `ask_vault`) is gated `!is_reasoner_only()`: the downloaded GGUF (Bielik/Qwen — which the README tells users to get "to
  unlock the in-meeting brain"), AFM, and Off **never run the loop**; they get the deterministic keyword floor (grounded and
  cited, but no tool choice, no propose_note). Fully-local Ollama *does* get the loop — the mismatch is specifically the
  marketed on-device path. In-code rationale is honest ("local-GGUF multi-step reliability unproven").
- **"Default-on semantic" is a flag, not a capability**: model downloaded-not-bundled, no onboarding step, no auto-download,
  download does not chain the backfill re-index. One manual step to cover *future* meetings, two to backfill. Every retrieval
  leg silently degrades to FTS meanwhile (the /brain page does show a nudge). README meanwhile still says semantic is "off by
  default" — under-claiming a shipped default (drift in the safe direction, still drift).
- **`capture_system_audio` default-ON reaches only fresh installs** — onboarding's `persistConfig` round-trips every key, so
  the entire pre-#167 onboarded base has `false` persisted and stays mic-only (semantic got a sentinel migration; capture did
  not). Also: v0.6.4 shipped *before* #167 — the flip is trunk-only, unreleased.
- **AEC3 echo removal defaults OFF while the load-bearing comment in `start_recording` claims "default on"** (+ FE fallback
  drift `?? true` vs `?? false`) — default installs keep the double-voice in the archive audio.
- **User memory is silently empty on the out-of-box install** (Cloud backend + no consent ⇒ extraction returns Unavailable per
  meeting; nothing tells the user why Memory is empty).

### (d) Shipped-but-unreachable (complete, tested backends with no UI)

1. **Trust Layer grounding** (`ground_summary`) — default OFF, settings DTO preserve-only, zero FE references; the only flip
   path is a manual write into the SQLCipher settings DB. No confidence value is rendered anywhere (segments confidence DOES
   cross IPC — arrives untyped, unrendered). The live half is only the `[UNCLEAR]` feed marking.
2. **Embedder picker** — `list_embed_models`/`select_embed_model` registered + tested ("Feeds the embedder picker"), zero FE
   callers: the strategically-corrected Polish `mmlw-retrieval-e5-small` is unselectable by any real user.
3. **AFM** — hardened Rust seam, 13 tests, stub-never-cloud dispatch; the Swift sidecar deliberately unwritten (macOS-26 Mac
   deferral), `BrainBackend` FE type has no `"apple"`, `afm_available` never invoked. Internal-only; no user is promised it.
4. **Pre-meeting brief** (see BROKEN above) + orphaned registered commands: `entity_dossier` (old cloud-egressing dossier —
   prune it), `brain_model_present`, and dead FE method shells (`askAssistantText`, calendar quartet, `getDocument` — imported
   documents can't be opened/viewed in-app at all).
5. Lock-domain edges: **vault residuals survive lock** (entity stubs `People/<Name>.md` with `[[sealed title]]` backlinks,
   Canvas exports, pre-lock digests — no removal API exists); **sealed titles cross IPC raw** in `list_meetings`/by-tag
   (masking is FE-only; `get_meeting_detail` masks backend-side — inconsistent posture); the **Privacy Receipt evaporates** on
   any in-app note edit or lock→remove-lock cycle (vault-write-only by design) and is stripped from the in-app Detail view
   (frontmatter strip) — the moat artifact is visible only in Obsidian.

## 3. Brain deep-dive — is it a real brain by 2026 standards?

### Retrieval grade card (web-grounded vs 2026 SOTA)

| Area | Best practice | Murmur | Grade |
|---|---|---|---|
| Hybrid retrieval + fusion | BM25 + dense, RRF | 3-leg RRF k=60 (FTS+vector+entity), gated | **A** |
| Keyword leg | FTS + language handling | FTS5/BM25, diacritics-folded, seal-purged triggers | **A** |
| Embedding usage | asymmetric prefixes | `query:`/`passage:` both sides, capture-tested | **A** |
| Facts/memory layer | temporal knowledge | bitemporal invalidate-not-delete + provenance + forget | **A** (beyond typical) |
| Graph augmentation | entity-anchored, cheap | deterministic GraphRAG-lite leg | **A−** (exact-token only; PL declension blind) |
| Agentic + floor | adaptive, bounded | 6-step loop + deterministic floor | **A−** |
| Eval harness | recall@k/nDCG/MRR + labels | built, gated, fixtured | **A design / F execution — never run** |
| Chunking | token-aware, 10–20% overlap, turn-aware | char paragraph-merge, no overlap, notes-only | **C** |
| Corpus coverage | index what users ask about | **transcripts not embedded** (FTS only) | **D** |
| Reranking | local cross-encoder top-N | none (zero matches for "rerank") | **F** |
| Citation faithfulness | verify against sources | resolved, never verified (machinery exists in `grounding.rs`, unapplied to Ask) | **C−** |

Notes: RRF k=60 is TREC-scale — k=10–20 counts more at vault scale (measure first). sqlite-vec brute-force KNN is the right
call at 10³–10⁵ chunks. Full GraphRAG would be a mistake (mixed-to-negative 2026 evidence); the lite leg is correct.
Up to 57% of LLM citations are post-rationalized (SIGIR/ICTIR 2025) — our Ask citations are resolved to visible meetings but
never checked against the cited content; `annotate_unverified` token-coverage machinery could close this deterministically.

### Memory + loop vs reference architectures (Letta/MemGPT, mem0, Zep/Graphiti, LangGraph, Anthropic guidance)

- **Where we match or beat the field:** bitemporal facts (3 of Zep's 4 timestamps, deterministic reconcile); background-write
  policy (the *recommended* pattern); tool specs pass Anthropic's "self-contained, robust, extremely clear" bar; and the
  unique part — **lock-gated memory** (sealed sources purge their derived facts and vanish from every prompt) + per-fact
  provenance + forget UI + a deterministic floor so the brain degrades honestly instead of hallucinating. No reference
  architecture attempts any of that.
- **The three deltas vs best practice:** (1) **loop transport** — JSON-in-prompt over stateless `complete()`, whole-context
  re-send per step, no native tool-use API, no parallel calls, 4 live steps; (2) **memory retrieval policy** — always-inject
  the whole 2k brief (no per-query relevance), exact-key reconcile can't merge paraphrases, no mid-conversation
  `remember_fact` (an explicit "zapamiętaj…" lands only at meeting finalize — and never on a stub install); (3) **context
  hygiene** — only crude 4k truncation, no compaction.
- **The three deltas vs ClickUp Brain²** (GA June 2026; Qatalog ActionQuery; MCP-based connectors; per-step model routing;
  Super Agents): (1) connected breadth — 100+ permission-aware sources vs our calendar+web; (2) actuation — their agents
  produce artifacts, ours is read-only propose-then-accept (write tools dormant by design); (3) dynamic model routing vs our
  static per-role. Our `visibility_clause` is a genuine ActionQuery-analog — over one silo. Notion AI (13–14 connectors,
  Turbopuffer-hosted embeddings, $20+/seat) is now the breadth benchmark, not ClickUp.
- **Defensible line:** *"A private brain: it remembers what you tell it, shows you exactly what it knows and where it learned
  it, forgets on command — and locked meetings are invisible even to its own memory."* Do NOT claim connected-search breadth
  or autonomous execution.

## 4. Competitive refresh (July 2026, all point-in-time, fetched)

- **Commoditized in ~5 weeks:** bot-free desktop capture (Fathom, Otter Desktop, Krisp, Fireflies, tl;dv botless) and MCP
  (universal; Otter is now MCP client *and* server). People/CRM views = confirmed table-stakes (Granola Sept 2025, Circleback
  Apr 2026) — our Person page is parity; the differentiator must be the voice-verification + lock layer on top.
- **Market exits in our favor:** Hyprnote → Char (cloud "AI Chief of Staff", no local-first mention; anarlog remnant adds
  *cloud* STT); **Limitless/Rewind dead** (Meta acquisition Dec 2025, app disabled, region exits) — the privacy-positioned
  personal-memory category leader orphaned its users; owned-markdown + on-device is the structural answer.
- **The real fast-mover: Screenpipe** (v2.5.91 shipped the same day as this audit; per-process meeting capture, meeting
  detection, MCP OAuth ×10, "encrypted at rest", on-device PII redaction claims) — marketing collision with two of our lines,
  though neither is a biometric per-folder lock nor a scrub→cloud→restore firewall. **The redaction firewall remains
  uncontested across all 12 apps scanned — our cleanest one-liner.**
- **Table-stakes we lack:** audio-file import (grep-verified absent — S, headless, feeds every layer), meeting auto-detection
  → "record?" nudge (the biggest "it just works" gap), consent UX (Notion ships *enforced audible consent* since Mar 2026 —
  no longer hypothetical), live auto-notes (near-parity via captions+@brain).
- **Timing wedge:** the Otter privacy-class-action MTD hearing was reset to **July 15, 2026** — the free news cycle for
  "no bot, nothing leaves, consent recorded and honored on-device" is ~2 weeks out.
- **Emerging surface we lack entirely:** screen-content awareness (Circleback ships it cloud-side; Screenpipe is built on it);
  a local OCR-of-shared-slides via our existing SCK sidecar + already-granted TCC permission is a real integration edge (L).

## 5. Delta vs the three 2026-07-03 reports (what actually changed)

- **"Defaults bury the moat" → ~70% closed** (#158 speaker-aware note, #159/#160 semantic flag ON + installed-base migration,
  #167 capture ON, #168 brain-reveal, honest onboarding posture) — residual: e5/NER/GGUF absent on fresh installs (no
  auto-download), diarize/voiceprint OFF (legally deliberate), brain default Cloud, capture flip misses the installed base.
- **"Measured proof never RUN" → unchanged as a run, fully closed as harness availability.** Both harnesses now exist
  (`eval/bakeoff.rs`, `eval/diarization.rs` — 1,249 lines, real NIST DER with a RED-able swapped-labels test); **zero result
  artifacts anywhere in the repo**. The retrieval proof is headless-runnable today (direct-DEK path, no Touch ID); only
  DER/voiceprint genuinely needs labeled audio + a Mac.
- **"Distribution/name" → unchanged**: Pages 404 re-verified live, homepage still points at releases, name decision untaken;
  launch plan authored (#169) and ready to fire.
- **Prior-report claims the code now DISPROVES** (do not re-quote them): V3's "onboarding mentions zero differentiators"
  (posture step exists, shipped in #153, pre-dating V3 — wrong when written); V3's "one-line flatten" (the real fix was ~200
  lines + a redaction extension); AM's "mmlw-e5-small zero-migration drop-in" (wrong variant, corrected by #157); REFRESH's
  "measured proof is real-Mac-only" (headless forward-pass ran); V3's "DER harness does not exist" (falsified by #166);
  LAUNCH's "the moat is mostly on by default" (over-read — flags on, models absent).

## 6. Strengths to keep saying out loud (all code-verified this pass)

- **Verify-before-destroy is universal** across seven seal paths, purge-on-seal covers every derived table in one transaction
  (vectors, doc chunks, corrections, threads, facts, user facts, voiceprints), AAD context-binding defeats blob-swap with a
  never-brick legacy ladder, and relock zeroizes the cached KEK + truncates the WAL. 876/876 lib tests green (re-run).
- **The egress envelope is one choke point** (`make_provider_resolved`): fail-closed consent, adversarial classification
  (remote Ollama = cloud, loopback gateway = still cloud, unparseable = cloud), always-on redaction wrap, content-free ledger,
  mid-flight revoke proven RED-before-GREEN. `claude_code` child hygiene (env_clear, DEK-strip even under inherit, binary
  vetting, `--disallowedTools`) is exemplary.
- **The agentic core is defensively engineered**: host-enforced allowlist re-checked per call, live unlocked-set re-read
  per call (mid-loop screen-share relock honored), a scripted-exfiltration test proving a malicious brain leaks nothing,
  propose-then-accept structurally unreachable writes, deterministic byte-identical floors.
- **FE↔BE contract discipline**: one typed method per command, zero direct `invoke()` in components, event names 1:1, honest
  empty/error states, no decorative toggles (every audited flag has a real backend consumer).
- **Radical in-code honesty** — thresholds labeled placeholders, headless-unverifiable claims annotated at the source, stubs
  that refuse to poison indexes. The audit found doc-drift, not deceit.

## 7. Options & ranked recommendation

**Tier 0 — truth-up + close the holes (S each, headless, this week):**
1. **Fix `vault_titles`**: scrub (regex + NER) inside `summarize_with_meta` like the three fields already covered — plus a
   RED-before-GREEN capture-provider test (the exact pattern of the fixed `user_notes` leak). Consider redacting the rendered
   egress string instead of an allowlist of fields, so the *next* field can't slip through the same way.
2. **Guard the StubReasoner floor** (`id()=="stub"` → "no model available" message, mirroring orchestrate.rs).
3. **Fix the 1:1 voiceprint dead path** (suggest/enroll on the plain `others` lane when exactly one remote cluster exists).
4. **Wire `cap_reached`** (toast + auto-finalize) and reconcile ghost RECORDING rows at startup.
5. **README truth-up**: delete/restore the pre-meeting-brief bullet, fix "default large-v3", fix "semantic off by default",
   scope the "on-device agentic loop" claim (or close it — see Tier 2), fix the `post_aec_enabled` comment + FE fallbacks.
6. Chain **download→auto-reindex**, add the missing **capture_system_audio sentinel migration** for the installed base, and
   re-embed meeting chunks on unlock/remove-lock (the documents path already does).

**Tier 1 — run the proofs (the standing gate, unchanged):** label ~15–20 real PL/EN queries, run the `#[ignore]`d bake-off
for FTS/semantic/hybrid × {e5-small, mmlw-retrieval-e5-small} (headless today), commit the numbers; decision rule: recall
fails → ship **transcript embeddings** (schema anticipated it — `source_type` exists; speaker-turn windows ~15% overlap);
rank order fails → the **reranker spike** (bge-reranker-v2-m3, candle feasibility unverified); FTS ≈ hybrid → stop investing
in the dense layer. DER stays the gate on the #1 killer demo (needs labeled audio + a Mac).

**Tier 2 — make the brain feel real (S/M):** (a) **memory v2** — `remember_fact` tool (propose-then-accept) + per-query
relevance retrieval of facts; the "I told it once and it knew" demo; (b) **embedder picker FE** + Trust-Layer toggle FE
(both backends are done and tested); (c) **native tool-use transport** behind the provider seam (Anthropic/gateway native,
JSON-in-prompt fallback) — reliability + cost, prerequisite for raising step budgets; (d) **global ⌘-hotkey "Ask my brain"**
(the `bar` window plumbing exists — the cheapest "it's a real brain, always there" perception jump); (e) **MCP client** as
consent-gated live tools — the one move that makes "connected brain" honest.

**Tier 3 — market timing:** audio-file import (S, table-stake, feeds the moat); meeting auto-detection nudge (M); **Consent &
Retention Passport before ~July 15** (deterministic, only we can bind it to a biometric-locked voiceprint + the shipped
Privacy Receipt); then the unchanged non-code blockers — the NAME, deploy `landing/`, the 90-second demo.

**First step:** Tier 0 item 1 (the vault_titles fix) — it is the only place where the code currently *falsifies* the privacy
copy, it is small, headless, and it protects the exact pitch every other recommendation leans on.

## 8. Open questions / honestly unverifiable headless

- Real dual-stream capture, TCC prompt flow, Bluetooth-headphone far-side survival, Touch ID, screen-share auto-relock on a
  live Zoom/Meet share, wake-phrase precision, local-GGUF inference quality, AFM zero-egress — all need a signed build on a
  real Mac (each is annotated as such at the source).
- Retrieval quality (semantic vs FTS, mmlw vs e5 on Polish) and diarization/voiceprint accuracy (DER/EER, the 0.5 threshold)
  — harnesses exist, zero numbers exist.
- candle feasibility of an XLM-R cross-encoder (reranker) on Metal — needs a spike.
- Whether `CandleBertEmbedder` silently truncates >512-token paragraphs (tail unindexed) — one look during transcript-embed work.
- Krisp's on-device transcription scope; Screenpipe's current speaker-ID state; Char's real feature set; the Otter MTD outcome
  (hearing ~July 15); whether Apple's free-PCC small-business tier permits our BYO architecture.

## 9. Sources

Web (all fetched by the researching agents, 2026-07-03/04): granola.ai/updates + /pricing · otter.ai blog (Conversational
Knowledge Engine) + CourtListener docket 5:25-cv-06911 · fireflies.ai/pricing · fathom.ai · circleback.ai/releases · krisp.ai ·
notion.com/releases/2026-03-12 + /help/notion-ai-connectors · char.com (hyprnote.com 301) · anarlog.so/changelog ·
github.com/screenpipe/screenpipe/releases + screenpipe.com · meetily.ai · clickup.com/blog/brain-2-launch + /brain/max ·
techcrunch.com 2025-12-05 (Meta/Limitless) · get.mem.ai/pricing · reflect.academy/artificial-intelligence ·
smartconnections.app · obsidiancopilot.com/en/pricing · apple.com/newsroom WWDC26 + developer.apple.com/wwdc26 ·
anthropic.com/engineering/effective-context-engineering-for-ai-agents · docs.letta.com (MemGPT) · docs.langchain.com
(LangGraph memory) · arxiv.org/abs/2501.13956 (Zep) · arxiv.org/abs/2412.18004 (citation faithfulness, SIGIR/ICTIR) ·
huggingface.co sdadas/mmlw-retrieval-e5-small + BAAI/bge-reranker-v2-m3 · github.com/asg017/sqlite-vec · plus the RAG/chunking
practice references cited in the underlying briefs.

Code (cite by symbol; grep before trusting a line): `summarize/redact.rs::summarize_with_meta` + `template.rs::render_user_content`
+ `export/obsidian.rs::list_vault_titles` (the leak) · `audio/recorder.rs::cap_reached` + `Shared.samples` ·
`transcribe/diarize.rs::relabel_others` + `VOICEPRINT_MATCH_THRESHOLD` · `transcribe/live.rs::run_informational` (cloud-only
gate; mic-only context) · `reason.rs::StubReasoner` + `ReasonerCell::current_for` · `pipeline.rs::should_auto_index` +
`build_transcript_feed` + `privacy_receipt_facts` · `storage/db.rs::search_hybrid_visible` + `purge_chunks_for_meetings` +
`semantic_default_v1` · `embed.rs::rrf_fuse` + `EMBED_MODELS` · `commands.rs::lock_folder_inner` + `list_meetings` (ungated
titles) + `select_embed_model` (no FE caller) · `settings/config.rs` defaults · `eval/bakeoff.rs` + `eval/diarization.rs` ·
`agent.rs::run_agentic_loop` · `tools.rs::GatedToolExecutor` · `user_memory.rs` + `facts.rs::reconcile_facts` · `proactive.rs` ·
`mcp.rs` · onboarding/settings FE per the audit sections above. Prior baselines: the three `docs/research/2026-07-03-*` reports
+ `2026-07-02-clickup-brain-gap-analysis.md` + `docs/COMPETITIVE-LANDSCAPE.md`.
