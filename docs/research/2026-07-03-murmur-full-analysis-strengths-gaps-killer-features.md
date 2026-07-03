<!-- Generated 2026-07-03 via /research (Workflow: 8 murmur-researcher agents + 1 completeness critic; ~954k subagent tokens, 187 web/code tool calls). Pricing/funding/versions/lawsuits = point-in-time mid-2026. Trust code over docs; symbols grep-confirmed where cited. -->
# Research: Murmur — full-app analysis (strengths, weaknesses), competitive gaps, killer features, and "is it breakthrough?"

## TL;DR / Verdict

**On the cusp of breakthrough — not there yet, and the reason is not the build, it's three things: the defaults hide the moat, the "brain" is unproven, and distribution is ~0.**

Murmur (v0.6.4, macOS) has the **deepest private-AI architecture in its entire category** — no surveyed competitor, cloud *or* local, combines all five of: per-folder biometric encryption-at-rest (Touch-ID KEK → AES-256-GCM content key, verify-before-destroy), a redaction firewall that lets you run a **frontier cloud model on scrubbed text**, Obsidian-owned `.md` files, a local lock-gated MCP server, and a **live in-meeting agentic brain**. That bundle is architecture, not policy, so a $192M-funded incumbent structurally cannot copy it without cannibalizing its own business.

But "breakthrough" is a **market** verdict, and by that measure Murmur is a category-leading *build* with three gaps:

1. **Its own defaults read as a cloud notetaker.** Out of the box the brain runs on cloud `claude_code`, diarization is OFF, semantic vectors are OFF, NER name-redaction is OFF. Every architectural moat is opt-in configuration. *The single cheapest strategic win is flipping/spotlighting defaults, not building features.*
2. **The "ClickUp-Brain-class local knowledge manager" ambition rests on an unproven core.** The semantic-retrieval leg (e5 vectors) is dormant-by-default, the embedder is mis-chosen for Polish (55.2 vs 67.5 PL-MTEB), the RAG bake-off is unrun, and *there is no eval harness to run it with*. Cross-meeting persistent memory doesn't exist yet (per-meeting threads DO now persist — see corrections). Until "ask my last 6 months, get a cited correct answer, fully on-device" provably beats the cloud incumbents on the user's own data, "second brain" is aspirational.
3. **Distribution ≈ 0 in a suddenly-crowded lane.** anarlog/Hyprnote (YC S25, 8.8k★), Meetily (13.5k★), Screenpipe (YC S26, 19.6k★), Talat (local-by-default), and Basil (on-device, already selling to lawyers/therapists) all exist. "Local + owned markdown + an MCP" is now **table-stakes**, not a moat.

Two structural forces the roadmap must respect: **the OS is commoditizing the core loop from below** (Apple Intelligence/Notes ships free on-device record→transcribe→summarize on M1+/8GB; Microsoft Recall does Windows ambient memory) — so stop selling transcription/summary as the value. And **the ambient/always-on frontier is a graveyard** (Rewind→Meta, Bee→Amazon, live wiretap litigation) — don't go there.

**The one feature everyone converges on** as the differentiated, half-built, legally-defensible move: **on-device cross-meeting speaker voiceprint identity** ("this is Anna again," across your whole history). It is the category's #1 unmet demand; Otter/Fireflies ship it in the cloud and are being **sued under BIPA** for exactly that; Murmur already computes the CAM++ embeddings and can persist them sealed-at-rest — the one way the market legally can't.

**Verdict in one line:** *Murmur is what people wish Granola were, engineered better than anyone in the lane — but it's a superb instrument nobody's holding yet. It becomes breakthrough when it (a) makes the local moat the default and visible, (b) proves the brain, and (c) ships voiceprint identity — then picks ONE wedge: a regulated vertical, or E2EE multi-device sync.*

---

## Corrections to the standing record (trust code, not docs)

The sweep grep-confirmed several facts that older docs/memory got wrong. Fix these before they misdirect the roadmap:

| Claim in old docs/memory | Reality (code-confirmed 2026-07-03) |
|---|---|
| "shipped 0.6.3" | **0.6.4** in `src-tauri/Cargo.toml` + `package.json` on trunk. |
| "@brain threads are FE RAM-only, no `thread_id`, no cross-session memory" | **Per-meeting thread persistence is SHIPPED**: `thread_id` on `assistant_interactions` (`db.rs:436`), backend UUIDv4 when FE omits it (`commands.rs:759`), gated `list_assistant_threads_visible` (`db.rs:3770`), purge-on-seal + round-trip tests (`db.rs:5538/5591`). The **real** remaining gap is *cross-meeting* memory and *whole-vault Ask* (still RAM-only). |
| "no on-device diarization; me/others only" | **N-way on-device diarization IS shipped** (sherpa-onnx pyannote-3.0 + WeSpeaker CAM++, `diarize.rs`, wired `pipeline.rs:424`) — but **default OFF** (`config.rs:302`), **system-stream only** (mic never diarized), **per-meeting only** (no cross-meeting identity), and **quality unverified**. |
| "redaction is regex-only; names not redacted" | A real **multilingual mDeBERTa-v3 NER name-redactor exists** (`redact.rs`, `ner-mdeberta-v3-multilingual`) — but **downloaded-not-bundled, presence-gated, no-op fallback**. Default installs redact only emails/cards/phones by regex. Selling it as "compliance-grade name scrubbing" without the model present is a **false-safety liability**. |
| "STATUS.md is current" | `docs/STATUS.md` is **stale** (dated 2026-06-24: base.en model, 31 tests, "Phase 2 unverified"). Rules still cite `biometric.rs`, which **no longer exists** (Touch-ID/LAContext folded into `secrets/keychain.rs`). Regenerate or delete it. |
| "proactive brain is a spec" | **In-meeting proactive recall is SHIPPED** (`proactive.rs`, zero-egress matcher → dismissible cards, `EVENT_PROACTIVE_HINT`). Post-meeting fact-deltas deferred (spec P3). |

---

## Co już mamy — current state, code-cited (v0.6.4)

Murmur is far past "record → summarize → note." Verified substrates:

- **Privacy/lock engineering (strongest in category):** whole-DB SQLCipher (DEK) + a *second* per-folder AES-256-GCM content-key wrapped by a Touch-ID-released master KEK; **verify-before-destroy** on every seal; **every content read gated** by `meeting_is_unlocked`/`visibility_clause`; screen-share auto-relock; the `convertFileSrc`/`asset:` leak closed by nulling `audio_path` in the masked DTO. (`.claude/rules/lock-model.md`; `commands.rs:4780,6125`; `db.rs:4555`.)
- **Redaction firewall + egress ledger + revocable fail-closed consent:** `RedactingProvider` wraps *every* cloud provider across pipeline/orchestrate/reason/voice_action, scrubbing PII before egress and restoring in the reply; content-free egress ledger; ollama path = zero egress. (`redact.rs`; `tools.rs:375-424`; `lib.rs:100-111`.)
- **Live in-meeting agentic brain:** `run_agentic_loop` (`agent.rs:72`) answers `@brain` threads against the rolling live transcript; **read-only by construction** (`allow_writes:false`), propose-accept, gated tools re-read the unlock set every call. (`voice_action.rs`, `tools.rs`.)
- **The "brain" data layer (all derived indexes over the one SQLCipher DB):** FTS5 (always-on) + e5 vectors via sqlite-vec/RRF (**dormant by default**) + entity Person/Project graph + **bitemporal facts** (Graphiti-lite, invalidate-not-delete) + document ingestion + a `/brain` page. (`db.rs`, `facts.rs`, `embed.rs`.)
- **Crash-safety, incident-driven:** Metal residency abort-guard at process entry (aborts in RELEASE too), graceful native dialog + clean exit on DB/keychain failure, startup reaping of capture helpers orphaned to launchd, lifecycle relock+KEK-zeroize+WAL-checkpoint. Crash-safe FFI (CoreGraphics C-funcs only, zero `msg_send`). (`lib.rs:54,189-352`; `screenshare.rs:16-41`.)
- **On-device N-way diarization** (opt-in), a **two-profile tuned Whisper** (Fast greedy / Accurate beam-5 + temperature ladder + anti-hallucination, tuned for inflected Polish; default large-v3), **~733 in-tree tests** + a supply-chain CI gate (clippy `-D warnings` + cargo audit + cargo deny + E2E). (`whisper.rs`, `diarize.rs`, `scripts/ci.sh`.)
- **Owned output + no-per-seat economics:** Obsidian `.md` + front-matter + `[[wikilinks]]` + `obsidian://` block-refs + `.canvas`; **zero payment code**, AGPL-3.0, BYO-key or local Ollama.
- **Local read-only MCP** (6 gated tools on `127.0.0.1:8765`, fail-closed token). (`mcp.rs`.)
- **Polished non-brain surfaces** (detail 3.3k-line view + timeline scrubber, library, analytics with egress-ledger/digest/threads, graph, 10-section settings + 5-component AI hub); a calm 5-step onboarding wizard; the standout **`@brain` grammar** (one token splits verbatim-note vs thread) and **propose→accept** trust identity.

---

## Findings — the honest picture

### Strengths (defensible vs table-stakes)

**Genuinely differentiated / architecture-level (hard to copy):**
- **Content-level encryption-at-rest with biometric unlock.** *No surveyed competitor has it* — cloud tools store plaintext transcripts on their servers; local tools (Talat/Meetily/Screenpipe/anarlog) store a plaintext local SQLite. Murmur's single most differentiated axis. (C1, C2, K2 — high.)
- **The redaction firewall.** A category no one occupies: cloud tools send everything; local tools avoid cloud (worse models). Murmur uniquely runs a *frontier* model on *redacted* text. (C1, C2, K2 — high.)
- **The integrated bundle + brain depth.** Every cohort member has 1–2 of {local capture, vault-native Obsidian, local MCP, graph/facts, lock, redaction, live agentic}; none has 5+. The moat is the *integration*, not any single feature. (C2 — high.)
- **Live agentic in-meeting AI** — ahead of the cohort's post-meeting-only AI (though narrow and being crept on). (C1, C2 — high.)
- **Crash-safety + crash-safe FFI maturity** — incident-driven, above category norm. (A1 — high.)

**Commodity / table-stakes (be candid):** on-device Whisper, local Ollama summaries, "an Ask," "has an MCP" (Fellow/Fireflies/Otter/Granola/Screenpipe all ship one now), local + owned-markdown output. These are cost-of-entry, not the pitch.

### Weaknesses & risks (ranked by strategic weight)

1. **The defaults bury the moat.** Cloud `claude_code` brain by default; diarization/vectors/NER all OFF. Out-of-box Murmur reads as a cloud notetaker; the whole local-first story is opt-in. Talat/Meetily default to a *local* reasoner and thus *out-privacy* Murmur's default config. (Critique headline; A1, C1 — high.)
2. **The brain is unproven, and the proof is blocked on unbuilt infrastructure.** No bake-off harness, no labeled PL+EN dataset, no retrieval metric, no diarization-DER harness — *and* the embedder is mis-chosen for Polish. Proving the brain is a build, not a weekend spike. This gates the entire ClickUp-Brain-class ambition. (Critique; T1, K2 — high.)
3. **Distribution ≈ 0; AGPL is a double-edge.** Solo/small team vs YC-funded, high-star, multi-platform competitors. AGPL-3.0 (a) lets a funded competitor *fork* the lock/redaction/facts code that is the moat, and (b) is *banned by many enterprise/regulated IT departments* — directly undercutting the regulated-vertical GTM. Dual-licensing is unaddressed. (Critique; C2, K2 — high.)
4. **Single-device / macOS-only / no team/mobile/sync** — a hard ceiling that directly caps the "knowledge-manager" ambition and loses to *every* competitor on reach. Closing it means either breaking local-first or building E2EE sync of the SQLCipher store (large effort). (A1, C1, K2 — high.)
5. **Default-ON offline AEC3 rides a 5-month-old v0.1.0 crate** (`sonora`, Feb-2026, 55★) on every recording with a system stream, and its real efficacy (ERLE on genuine speaker→mic echo) is unverifiable headless. For a compliance pitch, shipping a transcript-corrupting-if-wrong, real-Mac-unverified default-ON dependency into a lawyer's privileged record is a trust risk. **Consider default-OFF until real-Mac-proven.** (A1 — high code / medium efficacy.)
6. **Diarization solves the easier half** — system-stream only, no in-room mic split, **no cross-meeting voiceprint identity** (the actual #1 ask). (A1, T1 — high.)
7. **First-run friction & weak-Mac risk:** the default provider needs the `claude` CLI in PATH; the backend default model is large-v3 (~3GB/3–4GB RAM, swaps on 8GB Macs) while onboarding preselects "small" — any path bypassing onboarding hits the 3GB surprise; the model-download step is a blocking gate. (A1, K3 — high.)
8. **The regulated-vertical GTM is more fragile than it reads:** single-user Touch-ID-local KEK vs multi-seat/MDM/audit; AGPL IT bans; dormant NER = false-compliance; bus-factor/SLA credibility for a solo project. Promising wedge, needs real de-risking. (Critique — high.)
9. **Surface sprawl** — 7 nav tabs, THREE chat surfaces sharing one grammar, `/graph` duplicated inside `/brain`: the exact "AI sprawl" 77.5% of workers say they'd be relieved to lose, *inside a tool that sells consolidation*. (K3 — high.)
10. **Maintainability tax:** `commands.rs`/`db.rs` are 8k-line monoliths; STATUS.md drift. Review-surface + merge-conflict risk, not a correctness bug. (A1 — high.)

### The competitive landscape, mid-2026 (fresh)

**Cloud/mainstream has caught up to the "knowledge engine" framing.** Otter's **Conversational Knowledge Engine** (28 Apr 2026): longitudinal knowledge graph + agentic workflows + **bidirectional MCP** (Otter as both client and server). Read.ai cross-channel personal knowledge graph; Granola Chat + cloud MCP + public REST API ($1.5B, $192M raised, pivoting to "enterprise memory layer"); Sembly multi-meeting chat + AI Artifacts; Notion AI native meeting notes + Enterprise connectors; Zoom AI Companion 3.0 cross-platform (81.35% in independent benchmarking, ahead of Copilot). **Cross-meeting "ask your history" and MCP are now commodity** in this tier. All are cloud, none Obsidian-native, none on-device.

**The local-first cohort is crowded and moving fast:**
- **fastrepl/anarlog** (ex-Hyprnote, YC S25, MIT, 8.8k★, near-daily releases) — *Murmur's exact stack twin* (Tauri+Rust+local Whisper+Ollama). Already shipped **persistent cross-meeting chat**, dual calendar sync, assisted speaker assignment. Missing only: MCP, Obsidian export, graph/facts, the lock model. **The single most dangerous "could become Murmur."**
- **Meetily** (Zackriya, MIT, 13.5k★, mac/Win/Linux) — system-audio capture, Parakeet/Whisper, Ollama; diarization + Obsidian + chat all "coming soon."
- **Screenpipe** (YC S26, 19.6k★) — 24/7 local screen+audio → SQLite + MCP; structurally closest to a "multi-source local context store," but surveillance-DNA, not vault/meeting-native, no lock.
- **Talat** ($49→$99, TechCrunch Mar-2026) — local Mac (ANE/Parakeet or Ollama) + Obsidian + MCP + dual-side capture, **local reasoner by default**. The near-exact positional clone; lacks lock/redaction/facts/live-AI.
- **Basil** — fully on-device Mac/iPhone, **already selling to lawyers/therapists** — proves the regulated wedge, at shallower feature depth.
- **MegaMem** (54★) — Obsidian→Graphiti graph + MCP, but no capture; the cheap "assemble ~80% of Murmur's brain" supplier.

**The Obsidian community-plugin channel is empty** — no meeting-brain has taken it (top hits <25★), yet Murmur already writes owned `.md`/`.canvas`/block-refs. A real distribution wedge into Obsidian's ~1.5M MAU.

**Trust wounds to weaponize (fresh, citable):** Otter faces an Aug-2025 federal **wiretapping/consent class action** (Brewer v. Otter, N.D. Cal., MTD hearing 20 May 2026); Fireflies faces two BIPA voiceprint suits (Cruz Dec-2025, Fricker Jan-2026). Murmur *structurally cannot* train on you (on-device / redacted-only egress) vs incumbents that merely *promise* not to.

### The un-modeled structural threat: OS commoditization from below

Only surfaced by the critic. **Apple Notes on Mac** (M1+, 8GB+, macOS 15.1+) *already* records audio → on-device Apple-Intelligence transcript **summary**, free; the Phone app does on-device call transcription; **Microsoft Recall** does Windows ambient memory. The `record→transcribe→summarize` loop is being eaten from below. Murmur's durable value is precisely what the OS does *not* do: **system-audio capture from Zoom/Meet/Teams, a cross-meeting brain, the lock model, Obsidian-owned files, MCP.** Stop selling transcription/summary as the value.

### The ambient/always-on frontier: a trap with a graveyard (do NOT enter)

Rewind (the local-first pioneer) abandoned local-first ("turned MacBooks into a toaster," ~11GB/mo), pivoted to a cloud pendant, and was **absorbed by Meta (Dec 2025), deleting EU users' data**. **Bee → Amazon (Jul 2025).** The one profitable winner, **Plaud** (~$250M annualized, 2M+ users), is **intentful** press-to-record — essentially Murmur's meeting-scoped model with hardware, not always-on. The legal third rail (Brewer v. Otter, all-party consent) *worsens* with bystander capture, and local encryption fixes data-at-rest but does nothing for *other-party consent*. **Keep meeting/session-scoped capture as the trust boundary** — but **claim the narrative the incumbents vacated**: a "bring your life-log home" import adapter (Limitless/Plaud/Bee/Otter/Rewind exports → owned SQLCipher markdown) and a **companion posture** ("your pendant records, Murmur remembers, on *your* Mac"), zero hardware, zero ambient liability.

### The on-device model frontier (what's now feasible)

- **Cross-meeting voiceprint identity is now shippable and half-built.** CAM++/WeSpeaker embeddings already compute for opt-in per-meeting diarization; FluidAudio (Apache-2.0, ANE, pyannote-3.1, speaker-enrollment + speaker database, 10.6% DER on AMI-SDM) is the reference design. Persist a CK-sealed embedding table + cosine-match = "who said what" across every future meeting, on-device.
- **The embedder is the wrong choice for Polish** — multilingual-e5-small scores 55.21 PL-MTEB vs 67.50 for a Polish-native 124M (mmlw-roberta-base). Running the bake-off on e5-small would make the semantic leg lose to BM25 *for the wrong reason*. **Fix the embedder first** (mmlw-e5-small is a zero-migration 384-dim drop-in), *then* run the bake-off.
- **Whisper large-v3 is already near-SOTA for Polish** (~7.3% FLEURS, comparable to Parakeet-v3's 7.31%). An ASR migration is a **speed/battery** play (Parakeet on ANE ~10x RTF, frees the GPU), *not* an accuracy upgrade — keep large-v3 as the accurate PL + code-switch default.
- **A fully-local *reasoner* is close-but-not-there.** The seam exists (Qwen3-14B/Bielik-11B/Qwen2.5-3B GGUF registry), but it runs *prompt-instructed* JSON with **no constrained decoding** (JsonSchema overflowed Bielik's 32K context, `reason/mistral.rs:133`), open models top ~75% on *single-turn* BFCL with a steep unmeasured multi-turn drop, and a 14B on Metal competes with Whisper for the GPU (seconds-to-tens-of-seconds/turn). Ship it as an opt-in **"airplane-mode brain,"** not the default; the cheap reliability win is re-enabling constrained decoding.
- **Architectural unlock:** a FluidAudio-style CoreML/ANE sidecar for ASR+diarization would move that workload off the Metal GPU onto the Neural Engine, freeing the GPU for a local reasoner — the integration edge that makes "fully-local transcription + diarization + reasoning at once" tractable on one M-chip.

---

## Killer features — ranked (novel × moat-fit × feasibility)

| # | Feature | Why it's a breakthrough lever | Fit / effort | Verdict |
|---|---|---|---|---|
| **1** | **On-device cross-meeting speaker voiceprint identity** | Category's #1 unmet demand; Otter/Fireflies do it in cloud and are **sued (BIPA)** for it; Granola deletes audio. Half-built (CAM++ already computes embeddings). | Persist CK-sealed embedding table + cosine-match; **M**, risk = sherpa binding exposes the vector + cross-mic accuracy + *untested* on-device biometric-consent law. | **BUILD FIRST.** The differentiated, legally-defensible, half-built moat. |
| **2** | **Cross-session persistent memory ("memory of you")** | ClickUp Brain²'s entire pitch is persistent context (we scored 3 vs 8). Parity-critical for *class membership*. Spec-ready; reuses `facts.rs` with a user scope. | **M**, risk = does synthesized memory help or pollute (needs the same eval). Ship behind flag + forget/audit control. | **BUILD SECOND.** The foundation #1/#3 lean on. |
| **3** | **Auto-built Personal CRM / relationship graph** | Genuine whitespace — Monica is manual, Dex is cloud/LinkedIn; *no* meeting-AI auto-builds a relationship graph from transcripts. Primitives all exist (entities + facts + `get_entity_dossier` + `list_open_commitments`). | A `/people` synthesis surface + a couple gated readers; **S–M**. | **BUILD THIRD.** The most demo-able payoff of #1+#2. |
| **4** | **Provenance-linked notes** ("Anna said this, at 12:04") | Kills the #1 AI-notes trust objection (hallucinated summaries); Granola's enhance is *not* provenance-linked. Enabled uniquely by SQLite-canonical + block-refs + timeline. | **M**; supercharged by #1's real names. | **ADD.** Differentiated trust feature, cheap. |
| **5** | **⌘K "talk to your brain" global command bar** | The single highest *perceived-magic* lever (K3): summon anywhere (even over Zoom), cited answer across the vault, one keystroke to save-as-note/open-thread. Collapses the 7-tab sprawl; reuses the floating-bar window + `ask_vault` + agentic loop. | **M** (window+composer+hotkey; intelligence exists). Keep BOTH fuzzy-jump AND chat (Dia's cautionary tale). | **SHIP.** Makes the second-brain omnipresent, not a destination. |
| **6** | **Murmur as the local private-memory MCP for your whole AI stack** | Positioning/platform play — *but the MCP substrate is now commodity* (ai-memory-mcp, MemPalace, Granola cloud MCP). Differentiated only as **local + lock-gated + voice-sourced**. | One-click installer registering Murmur into Claude Desktop/Cursor config + expose the dormant propose-accept *write* tool under an approval gate (lock-security required); **S–M**. | **SHIP, don't lead with it.** |
| **7** | **Proactive post-meeting fact-deltas** ("this updates a fact from 2 weeks ago") | In-meeting recall already ships; this finishes it. Zero-egress proactive is uncontested. | **S–M**; compounds with #1/#2. | **FINISH** after #1/#2. |
| **8** | **"Bring your life-log home" import adapter** (Limitless/Plaud/Bee/Otter/Rewind exports → owned SQLCipher `.md`) | Rides the wearable wave with **zero** capture/consent/thermal risk; monetizes acquisition anxiety + Granola 30-day-paywall churn; fits the pluggable-ingest thesis. | **S–M**. | **STRONG cheap wedge.** |
| — | Prove + default the **semantic RAG** (fix embedder → bake-off) | *Enabler, not a headline.* Gates ask-anything quality that #2/#3/#6 depend on. | **S** (embedder swap) + a real-Mac eval build. | **UNBLOCK as a dependency.** |
| **↓** | Agentic **write-back to Slack/Linear/Jira** | *Contested* — ClickUp Super Agents already ship approval-gated write-back; means new egress + per-connector OAuth (strains local-first). | — | **DEFER.** |
| **↓** | **Multi-device DB sync** | Real demand, but collides with the Touch-ID-local KEK + whole-DB SQLCipher. Interim: notes are *already* owned `.md` → Obsidian Sync/iCloud/Syncthing over the vault gives multi-device read today. | E2EE zero-knowledge sync = **L** | **DEFER true DB sync;** document the vault-sync interim. |
| ✗ | "Redaction-firewall-as-a-service" | Off-mission; firewall is best-effort → reselling invites a false-safety claim. | — | **SKIP.** |

---

## Fit with Murmur's constraints

- **Local-first / redaction:** #1 (voiceprints), #2 (memory), #3 (CRM), #7 (deltas) are **zero new egress** (local reads only). #5 (⌘K) and #6 (MCP write) route any cloud text through the existing firewall. #8 (import) is pure local ingest. All on-thesis.
- **Lock model:** #1 (voiceprints are PII-at-rest), #2 (memory), #8 (imported content), and any new FTS-over-documents / cross-meeting memory table are **mandatory `lock-security-reviewer` gates** — seal + purge-on-lock + gate-every-read, copying the `assistant_interactions`/facts purge pattern.
- **SQLite-canonical:** every proposed substrate is an additive gated table over the one DB — no second copy of content.
- **Provider seam:** memory-brief injection mirrors the shipped `live_transcript` pattern; the local-reasoner and ANE-ASR sidecar both slot behind existing seams.
- **CI honesty (needs a real Mac):** voiceprint re-ID accuracy, diarization DER, AEC3 ERLE, the RAG bake-off, local tool-calling reliability + latency, live diarization, Touch-ID/screen-share-relock. Headless proves plumbing + gating only.

---

## Options & trade-offs — the strategic fork

The critic frames the real decision, and it's a fork a solo team must pick, not drift through:

**A) Double down as the single-user privacy-sovereign "meeting brain" for Mac power-users.** Win the Obsidian/LocalLLaMA niche; monetize the **proven Obsidian model** (app free + owned files; one-time Pro license $49–99 — proven by Superwhisper $249/MacWhisper €59/VoiceInk; a voluntary $50/user/yr commercial license; later an optional E2EE sync $4–8/mo). Lower risk, defensible niche, near-zero marginal cost. Distribution via HN Launch (Hyprnote hit 270 pts), the empty Obsidian plugin channel, r/LocalLLaMA (764k) + r/ObsidianMD (339k), MCP directory.

**B) Make the harder bet: a defensible multi-seat compliance product for ONE regulated vertical** (lawyers via ABA Op 512 / therapists via HIPAA — highest WTP, Granola architecturally cannot enter). *But* this collides with the single-user Touch-ID-local KEK (no multi-seat/MDM), AGPL (IT bans + fork risk on the very moat), and the dormant NER (false-compliance). It needs multi-seat provisioning, audit logs, a support SLA, and dual-licensing — real de-risking, not the frictionless path K2 implied. Validate WTP with a landing page + a compliance one-pager + one bar/therapy-community beta *before* building vertical features.

Both paths share the same **first three builds** (voiceprint, memory, brain-proof) and the same **default-flip**. The fork is really about the *fourth* investment: **Pro-license + E2EE sync (A)** vs **multi-seat + compliance packaging (B)**. Recommendation below leans A-first because it's lower-risk, funds the team, and keeps B open — but the founder's ClickUp-Brain-class ambition points at B, so treat it as an explicit choice, not a default.

---

## Recommendation & first step

**Sequence (each independently shippable, RED-before-GREEN, lock-reviewed where it touches content):**

1. **Flip/spotlight the defaults so the moat is visible out-of-box** (S, this week). Make the fully-local path (Ollama + local brain) a first-class, prominent onboarding choice; align the backend default Whisper model with onboarding's "small"; **default `post_aec` OFF** until real-Mac-proven; regenerate/delete stale `docs/STATUS.md`. *The cheapest strategic win — it converts "reads as a cloud notetaker" into "local-first by default."*
2. **Fix the embedder, then run the RAG bake-off** (S + a real-Mac eval). Swap multilingual-e5-small → **mmlw-e5-small** (zero-migration 384-dim). Build the missing eval harness + a labeled PL+EN dataset + a retrieval metric. *This unblocks the entire knowledge-manager ambition; without it "second brain" stays aspirational.*
3. **Ship on-device cross-meeting voiceprint identity** (M) — **the differentiated, half-built moat.** *De-risk with a 1-day spike first:* does the sherpa-onnx Rust binding expose the per-span CAM++ embedding **vector** standalone (not just internal clustering)? + a small real-Mac accuracy eval on 2–3 speakers across mics. Then: enroll-on-rename → CK-sealed embedding row → "This looks like Anna?" suggestion next meeting. Get counsel on the on-device-biometric-consent UX before default-on.
4. **Ship cross-session user memory** (M, spec-ready) behind a flag with a forget/audit view, then **the `/people` Personal CRM** (S–M) over the existing dossier/commitments/facts. Class membership + the most demo-able payoff.
5. **Ship the ⌘K "talk to your brain" bar + de-sprawl** (M) — merge `/graph` into `/brain`, unify the three chat surfaces under one store, background-prefetch the model + a canned demo meeting for onboarding-to-first-value. The felt "magic" lever.
6. **In parallel, validate ONE wedge with zero code** — either a Pro-license page (path A) or a compliance one-pager + one regulated-community beta (path B) — to learn WTP before committing the fourth investment.

**Smallest verifiable first slice (do this next):** the **default-flip + embedder swap** (steps 1–2 up to the eval), because they're headless-shippable, unblock everything downstream, and turn the biggest self-inflicted weakness (defaults hide the moat; brain unproven for the wrong reason) into momentum — while the voiceprint spike de-risks the flagship build in a day.

---

## Open questions / couldn't verify (needs a real Mac or is point-in-time)

- **Legal:** does an on-device, never-egressed, single-user voiceprint capturing a non-consenting remote participant still trigger BIPA/CIPA? Almost certainly far safer than the cloud-collection theory the Otter/Fireflies suits turn on, but **untested law** — needs counsel + a consent-UX decision before default-on. ("Granola *structurally cannot* match voiceprint" is slightly overstated — they *could* store an embedding without audio; they won't because their model is cloud.)
- **Voiceprint feasibility spike:** does the sherpa binding expose the standalone CAM++ vector, or only cluster internally? Bounds M-vs-L effort.
- **RAG quality (PL, names, paraphrase)** is unmeasured — only the (embedder-fixed) bake-off answers whether the semantic leg earns default-on.
- **AEC3 ERLE / diarization DER / voiceprint re-ID accuracy / local-reasoner latency & multi-turn reliability / live-caption cadence** — all real-Mac + recorded-evidence bars; green `cargo test` is not proof.
- **Regulated-vertical WTP & IT-procurement reality** (MDM/notarization/multi-seat for a free AGPL indie app) — inferred, not measured. Basil's actual depth/pricing came from snippets, not a fetched product page.
- **Competitor velocity** (anarlog star-growth, managed-cloud pricing; whether Meetily's diarization shipped post-v0.4.0; whether Talat's Obsidian/MCP is shipped vs "planned") — point-in-time snapshots; the real threat is their *rate*, under-measured here.
- **Market-size TAM ($3.5B→$21–29B)** — analyst estimates that habitually inflate; directional only. **Plaud "~$250M ARR, profitable"** — single-source press; directional.
- **E2EE zero-knowledge sync feasibility** on the owned-`.md` + SQLCipher + Touch-ID-KEK model — un-spiked; the recurring-revenue thesis depends on it being tractable.

## Sources

**Internal (key symbols — grep-confirmed):** `src-tauri/Cargo.toml` (v0.6.4); `src/lib.rs:34,54,100-111,189-352`; `commands.rs:759,781,4780,6125` + `db.rs:436,3770,4555,5538`; `redact.rs` (NER `ner-mdeberta-v3-multilingual`); `tools.rs:375-424`; `agent.rs:72`, `voice_action.rs`, `proactive.rs`; `transcribe/diarize.rs`, `pipeline.rs:424-495`, `transcribe/model.rs:25-26`, `whisper.rs:16-57`; `reason/mistral.rs:133-142`, `reason.rs:56-84`; `embed.rs:26-104`, `embed/candle_bert.rs`; `settings/config.rs:302,316`; `mcp.rs`; `.claude/rules/lock-model.md`; `docs/STATUS.md` (stale). Prior briefs: `docs/research/2026-07-02-{murmur-vs-granola,clickup-brain-gap-analysis,brain-full-analysis,audio-echo-full-remediation}.md`, `docs/COMPETITIVE-LANDSCAPE.md`, `docs/RAG-BAKEOFF.md`.

**External (fetched, point-in-time mid-2026):**
- Otter Conversational Knowledge Engine + bidirectional MCP (28 Apr 2026): otter.ai/blog/otter-ai-evolves-from-ai-notetaker...
- Talat local Mac + Obsidian + MCP, local-by-default: techcrunch.com/2026/03/24/talats-ai-meeting-notes-stay-on-your-machine...
- MCP landscape (Fellow Anthropic-verified, Fireflies, Otter, Screenpipe): meetingnotes.com/blog/ai-meeting-notes-with-mcp...
- anarlog changelog (persistent cross-meeting chat, calendar, speaker assignment): anarlog.so/changelog · github.com/fastrepl/anarlog (8.8k★) · Hyprnote Launch HN (270 pts): news.ycombinator.com/item?id=44725306
- Meetily: github.com/Zackriya-Solutions/meetily (13.5k★) · Screenpipe: github.com/screenpipe/screenpipe (19.6k★) · MegaMem: github.com/C-Bjorn/MegaMem (54★)
- Otter wiretapping class action: npr.org/2025/08/15/g-s1-83087/otter-ai-transcription-class-action-lawsuit · BIPA voiceprint suits (Cruz/Fricker v. Fireflies, Brewer v. Otter): topreviewed.ai/blog/voiceprint-lawsuit-reprice-meeting-ai
- Otter/Fireflies cross-meeting speaker ID: help.otter.ai/hc/en-us/articles/21665587209367 · summarizemeeting.com/en/faq/does-fireflies-have-speaker-identification
- Apple Intelligence on Mac (on-device Notes record+summary): support.apple.com/guide/mac-help/...mchl2102c2ae · shadow.do/blog/apple-intelligence-on-mac-2026
- Ambient graveyard: Meta acquires Limitless (Dec 2025): mlq.ai/news/meta-acquires-ai-wearables-startup-limitless... · Amazon acquires Bee (Jul 2025): techcrunch.com/2025/07/22/amazon-acquires-bee... · Rewind local-first post-mortem: andrewschreiber.substack.com/p/an-early-adopters-thoughts-on-rewindais · Plaud profitable ~$250M: forbes.com/sites/iainmartin/2025/09/02/...
- On-device models: BFCL V4: gorilla.cs.berkeley.edu/leaderboard.html · FluidAudio (ANE diarization + enrollment, 10.6% DER): github.com/FluidInference/FluidAudio + Benchmarks.md · Parakeet-v3 Polish 7.31% FLEURS: huggingface.co/nvidia/parakeet-tdt-0.6b-v3 · PL-MTEB embeddings (e5-small 55.21 vs mmlw-roberta-base 67.50): arxiv.org/html/2405.10138v2 · Bielik v3: arxiv.org/pdf/2505.02550
- Market/moat/monetization: Granola $125M/$1.5B: techcrunch.com/2026/03/25/granola-raises-125m... · Obsidian $25M ARR/7-ppl/zero-VC + pricing: finance.biggo.com/news/iVboYp0Bga3fZL9MJEv_ + obsidian.md/pricing · ABA Op 512 / on-device lawyers: basilai.app/articles/2026-06-10-ai-notetaker-lawyers... · therapists+AI: npr.org/2026/05/26/nx-s1-5826943 · Mac transcription one-time licenses: getvoibe.com/resources/macwhisper-pricing, spokenly.app/blog/superwhisper-pricing
- UX: ClickUp AI-sprawl survey (77.5%/46.5%/2.78x): clickup.com/blog/ai-sprawl-survey · notification budget (3–5/day): tianpan.co/blog/2026-05-13-background-agents-notification-budget... · Dia chat-vs-launcher: supasidebar.com/blog/dia-browser-mac-review-2026 · Superhuman 60-sec wow: flowjam.com/blog/superhuman-onboarding-teardown... · Cursor accept/reject diff: dev.to/vikram_ray/i-reverse-engineered-cursors-ai-agent...
