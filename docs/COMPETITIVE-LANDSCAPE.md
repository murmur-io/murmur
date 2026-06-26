<!-- Generated 2026-06-26 via a multi-agent web-research sweep (WebSearch/WebFetch). Products + URLs verified live; treat pricing/funding as point-in-time. -->

# MURMUR Competitive Landscape (June 2026)

## 1. TL;DR Verdict

**Partially — the ingredients all exist, but no one ships Murmur's full bundle.** Every individual capability has a competitor: cloud note-takers (Granola, Fireflies, Otter, Fellow) own MCP + cross-meeting ask; local Mac apps (Meetily, Hyprnote/anarlog, Talat, Screenpipe) own on-device capture + Ollama; Obsidian plugins (tsheil's AI Meeting Notes, Smart Second Brain, MegaMem, MeetingMind) own vault-native output and graph-building. But the *union* — local-first capture **and** vault-native Obsidian output (front-matter + `[[wikilinks]]` + `obsidian://` block-refs) **and** a self-assembling Person/Project graph **and** cited cross-meeting Ask **and** action-items to Obsidian Tasks *and* Apple Reminders **and** a redaction firewall **and** a *local* MCP server exposing meetings — is unclaimed. The closest single product, **Talat**, fields the MCP+Obsidian pair but lacks the graph, redaction, dual-task-sink, and recipes/timeline layer. Murmur's moat is integration, not any one feature.

## 2. Closest Competitors

**1. Talat (https://talat.app)** — Fully-offline macOS meeting app ($49 one-time, bundled local model + Ollama). **Overlaps:** on-device transcription, local-first, Obsidian Markdown auto-export, and a **local MCP server over meeting history** — the single closest match to Murmur's MCP+Obsidian combo. **Falls short:** no self-assembling `[[Person]]/[[Project]]` graph with backlinks, no true vault-native wikilinks/block-ref deep links, no action-items→Obsidian Tasks/Reminders, no redaction firewall, calendar = reminders only (not a built brief), no recipes/timeline. Closed source.

**2. Granola (https://www.granola.ai)** — The closest *cloud* competitor; bot-free Mac capture, calendar-aware **Brief**, cross-meeting **Chat** with source-linked citations, and an official **MCP server** (Feb 2026) feeding Claude/ChatGPT/Cursor. Raised $125M at $1.5B (Mar 2026). **Falls short on Murmur's core promise:** not local-first (transcription/AI/sync are cloud); no native Obsidian export (the Obsidian crowd's favorite, but users still copy/paste or rig Shortcuts); MCP is cloud-hosted over server-stored data, not a local server over on-device data; no owned-file knowledge graph; no redaction firewall; no Ollama/local-LLM summary path.

**3. Hyprnote / anarlog (https://anarlog.so)** — Closest on *philosophy and stack*: local-first, MIT, on-device Whisper + Ollama, Obsidian export, AI chat across notes, pre-meeting context from Apple Calendar. **Falls short:** no MCP server (the inverse of Murmur's "be the memory other tools query"), no knowledge graph, no action-items→Tasks/Reminders, no redaction, and Obsidian is a flat export (no front-matter + `[[wikilinks]]` + block-refs). Also fragmenting into commercial **Char** vs OSS **anarlog**.

**4. MegaMem (https://github.com/C-Bjorn/MegaMem)** — Closest on Murmur's most *distinctive* combo: **Obsidian-native + local + auto knowledge-graph + MCP server** in one tool, including "ask Claude what I know about X" across the vault. **Falls short:** it is a memory/graph layer over an *existing* vault — no audio capture, no transcription, no brief, no redaction, no action-items→Reminders. Requires running Neo4j/FalkorDB (heavy setup); still public beta.

**5. tsheil "AI Meeting Notes" plugin (https://github.com/tsheil/obsidian_plugin_AI_meeting_notes)** — Closest *capture* analog inside Obsidian: local Whisper + Ollama, mic + system audio, writes Markdown summary + action items + front-matter into the vault. **Falls short:** stops at "write a note" — no wikilink/graph building, no cross-meeting Ask, no MCP, no redaction, no brief, no Reminders. One feature of Murmur, not the system.

*(Runners-up: **Fireflies/Fellow/Otter** — official cloud MCP + cross-meeting ask, but cloud-stored data, no Obsidian/local/graph/redaction; **Screenpipe** — the only other local+MCP, but a 24/7 screen-recorder, not Obsidian-native, no graph; **Smart Second Brain** — best match to cited Ask-My-Vault but no capture at all.)*

## 3. Feature Matrix

Legend: ✅ native/real · 🟡 partial/via-cloud/manual/roadmap · ❌ absent

| Capability | Murmur | Talat | Granola | Hyprnote/anarlog | MegaMem | tsheil plugin | Fireflies |
|---|---|---|---|---|---|---|---|
| Local transcription (on-device) | ✅ Whisper/Metal | ✅ bundled model | ❌ cloud | ✅ Whisper | ❌ (no capture) | ✅ local Whisper CLI | ❌ cloud bot |
| Local LLM summaries (Ollama) | ✅ Ollama/Claude Code | ✅ Ollama | ❌ cloud only | ✅ Ollama/LM Studio | 🟡 graph-only | ✅ Ollama | ❌ cloud + AI credits |
| Obsidian-native export (`[[wikilinks]]`+front-matter) | ✅ | 🟡 flat MD export | ❌ no vault export | 🟡 flat export | ✅ vault-native | ✅ front-matter+links | ❌ |
| Cross-meeting Ask + citations | ✅ `[[Title]]` cites | ✅ via MCP | ✅ source-linked | ✅ chat across notes | ✅ cross-vault | ❌ | ✅ AskFred (no wikilink cites) |
| Action-items → Tasks **and** Reminders | ✅ Obsidian Tasks + Apple Reminders | ❌ | 🟡 Slack/Notion/Zapier | 🟡 summary text only | ❌ | 🟡 text list only | 🟡 Notion/CRM, not Obsidian/Reminders |
| Self-assembling `[[Person]]/[[Project]]` graph | ✅ | ❌ | ❌ | ❌ | ✅ Neo4j/Graphiti | ❌ | ❌ |
| Pre-meeting brief (calendar-built) | ✅ | 🟡 reminders only | ✅ Brief | ✅ calendar context | ❌ | ❌ | ❌ |
| Weekly digest / topic threads | ✅ | ❌ | 🟡 chat-derived | ❌ | ❌ | ❌ | ❌ |
| Obsidian Canvas (.canvas) export | ✅ | ❌ | ❌ | ❌ | 🟡 graph view (not .canvas) | ❌ | ❌ |
| Redaction firewall (scrub→restore) | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ |
| MCP server (exposes meetings) | ✅ **local**, on-device | ✅ **local** | 🟡 cloud-hosted | ❌ | ✅ local (graph) | ❌ | 🟡 cloud-hosted, paid |

### 3b. The LOCAL + OBSIDIAN-NATIVE + MCP triple

**No single competitor combines all three.** The closest:

- **Talat** = local ✅ + Obsidian 🟡 (flat export, not vault-native wikilinks/block-refs) + MCP ✅ (local). It's the *only* product fielding local-capture + a local MCP + Obsidian output together — but its Obsidian is a flat Markdown dump, not the graph/wikilink/block-ref system Murmur writes, and it has no auto-graph.
- **MegaMem** = local ✅ + Obsidian ✅ (vault-native) + MCP ✅ (local) — but **no meeting capture/transcription at all**, so it isn't a meeting app.
- **Granola/Fireflies** = MCP ✅ but **cloud**, not local, and not Obsidian.
- **tsheil plugin / Hyprnote** = local ✅ + Obsidian ✅ but **no MCP**.

So the triple splits cleanly: whoever has the local MCP either dumps flat Markdown (Talat) or doesn't capture meetings (MegaMem); whoever is vault-native + local has no MCP. **Murmur is the only product fusing local capture + vault-native Obsidian + a local MCP server in one system.**

## 4. Where Murmur Is Genuinely Differentiated vs Not Unique

**Genuinely differentiated (defensible):**
- **Redaction firewall** (scrub PII → cloud-LLM call → restore). *Not a single meeting/notes app across all four briefs ships this* — it exists only as generic pipeline tooling (pii-redactor, Presidio). Strongest single differentiator.
- **Action-items → Obsidian Tasks *and* Apple Reminders.** No competitor routes to either Obsidian Tasks *or* Reminders, let alone both.
- **The full integrated bundle** of vault-native Obsidian + auto knowledge-graph + cited cross-meeting Ask + local MCP — split today across MegaMem (graph+MCP, no capture) and Talat/tsheil (capture, no graph/MCP).
- **Vault-native fidelity**: front-matter + `[[wikilinks]]` + `obsidian://` block-ref deep links + timeline pin-a-moment + `.canvas` export. Competitors that touch Obsidian do flat exports.
- **Local MCP server over on-device meeting data** — Talat is the only peer; everyone else is cloud-hosted (Granola/Fireflies/Otter/Fellow) or non-Obsidian (Screenpipe).
- **Self-assembling `[[Person]]/[[Project]]` graph of owned files** — only MegaMem does graph-building, and it can't capture meetings.

**NOT unique (commodity — be candid):**
- **On-device transcription** (Whisper/Metal): commodity. MacWhisper, Superwhisper, Aiko, Vibe, whisper.cpp, plus Meetily/Hyprnote/Talat all do it.
- **Local AI summaries via Ollama**: commodity. Meetily, Hyprnote, Talat, tsheil plugin, MacWhisper all offer it.
- **Cross-meeting "Ask" / MCP server existence**: now *table-stakes* among cloud tools (Brief 1's headline) — the differentiator is *local + on-device data*, not the feature itself.
- **Pre-meeting brief**: Granola, Reflect, Hyprnote, Claryti, Brief My Meeting all ship one; Murmur's edge is only that it's vault-written and local.
- **Speaker rename / in-app Markdown rendering**: undifferentiated table-stakes.

## 5. Risks / Who Could Close the Gap Fast

- **Granola (highest risk).** $1.5B war chest, already has MCP + Brief + cited cross-meeting chat, and the Obsidian crowd is *begging* for native export (a long-standing open request). One "Export to Obsidian vault" feature + a local-processing mode would erase most of Murmur's positioning gap overnight. Its only structural blocker is the cloud-first architecture.
- **Talat (closest feature-fit).** Already ships local MCP + Obsidian + on-device model. Adding a knowledge graph, block-ref fidelity, and a Reminders/Tasks bridge is incremental — it's the most likely to accidentally become Murmur.
- **MegaMem + a capture plugin.** MegaMem already has the hard part (local Obsidian graph + 23-tool MCP). Bolting on the tsheil-style capture plugin (both local, both Obsidian) would assemble ~80% of Murmur in the OSS ecosystem — and it's free/open.
- **Hyprnote/anarlog.** OSS, local, Obsidian-exporting; adding an MCP server is a weekend feature given the stack. Fragmentation (Char vs anarlog) is its current drag.
- **Lower risk:** Fireflies/Fellow/Otter (cloud-locked, no Obsidian DNA); Screenpipe (screen-recorder identity, not Obsidian).

**Positioning suggestion:** *"Murmur is the only meeting recorder that lives inside your Obsidian vault — fully local, with a redaction firewall and a local MCP server so Claude can query your meeting memory without anything leaving your Mac."* Lead with **redaction + local-MCP-over-your-vault** (the genuinely uncontested ground); treat transcription and summaries as cost-of-entry, not the pitch.
