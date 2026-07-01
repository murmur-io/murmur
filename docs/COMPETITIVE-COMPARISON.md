<!--
  Competitive comparison for issue #9 (Amie · Vexa · Meetily · MeetingBaas · Otter · Fireflies + other AI notes).
  Researched 2026-07-01 via a multi-agent web sweep (WebSearch/WebFetch); competitor facts are point-in-time —
  pricing/features/incidents were verified against live vendor pages + GitHub on that date, and flagged where
  a claim is vendor self-assertion or could not be verified. Murmur's own capabilities are grounded in this
  repo's code/README, not marketing. Companion doc: docs/COMPETITIVE-LANDSCAPE.md (OSS + Obsidian-plugin ecosystem).
-->

# Murmur — Competitive Comparison (July 2026)

**Scope:** the competitors named in issue #9 — [Amie](https://amie.so/), [Vexa](https://vexa.ai/),
[Meetily](https://meetily.ai/), [MeetingBaas](https://www.meetingbaas.com/en), [Otter](https://otter.ai/),
[Fireflies](https://fireflies.ai/) — plus a short "other AI notes" round-up. For the OSS / Obsidian-plugin
side of the market (Granola, Talat, Hyprnote, MegaMem, tsheil) see [`COMPETITIVE-LANDSCAPE.md`](./COMPETITIVE-LANDSCAPE.md).

> **Honesty note.** Competitor data is point-in-time (2026-07-01) and partly from third-party reviews;
> vendor privacy/security claims are self-assertions unless independently sourced. Murmur rows reflect what
> the code actually ships today (v0.5.x), including honest gaps.

---

## 1. TL;DR

**Murmur is the only tool in this set that is simultaneously (a) a finished single-user macOS app, (b) truly
on-device for both transcription *and* AI, (c) Obsidian-native with owned `.md` files, and (d) shipping both
per-folder Touch-ID encryption *and* a local MCP server.** Every rival owns one or two of those; none owns all
four. The market splits along two axes and Murmur sits in a corner almost no one else occupies:

- **Capture:** *bot-in-the-meeting* (Otter, Fireflies, Vexa, MeetingBaas) vs *bot-free native/device capture* (Murmur, Amie, Meetily).
- **Data:** *cloud* (Otter, Fireflies, Amie, MeetingBaas-hosted) vs *self-hosted server* (Vexa) vs *on-device* (Murmur, Meetily).

The only true **bot-free + on-device** peers are **Meetily** (our closest head-to-head) and — with a big asterisk —
**Amie** (bot-free capture, but its AI runs in a **US cloud LLM**, so it is *not* on-device). Everyone else is cloud
and/or bot-based. Murmur's defensible moat is not any single feature — it's the **integrated bundle** + **privacy-as-architecture**.

### Podsumowanie (PL)

Murmur to jedyne narzędzie z tej grupy, które naraz jest: gotową appką na macOS, w pełni **on-device** (transkrypcja
*i* AI lokalnie), **natywne dla Obsidiana** (pliki `.md`, które są Twoje) oraz ma **szyfrowanie per-folder z Touch ID**
i **lokalny serwer MCP**. Konkurenci mają po jednej–dwie z tych cech, nikt nie ma wszystkich. Nasza najmocniejsza
pozycja to **prywatność wpisana w architekturę** (nic nie opuszcza Maca) + **kompletny, zintegrowany zestaw** funkcji.
Największe braki: tylko macOS, brak nagrywania botem zdalnych spotkań, brak zespołów/współdzielenia, mała skala/rozpoznawalność.

---

## 2. Positioning map

```
                         ON-DEVICE / LOCAL
                                 ▲
                  Meetily ●      │      ● MURMUR
             (OSS, bot-free,     │   (bot-free, on-device brain,
              cross-platform)    │    Obsidian-native, local MCP)
                                 │
   BOT-FREE ────────────────────┼──────────────────── BOT-IN-MEETING
   (native/device capture)      │        (a bot joins the call)
                                 │
                     Amie ●      │      ● Otter   ● Fireflies
             (bot-free capture,  │   (cloud, bot+native)  ● Vexa (OSS, self-host bots)
              but CLOUD US LLM)  │                        ● MeetingBaas (cloud bot API)
                                 ▼
                            CLOUD / SERVER
```

*Amie is plotted on the bot-free side but low (cloud AI); Vexa/MeetingBaas are developer infrastructure, not end-user apps.*

---

## 3. Feature matrix — Murmur vs the six

Legend: ✅ native/real · 🟡 partial / roadmap / cloud-only / enterprise-gated · ❌ absent · — n/a

| Capability | **Murmur** | Amie | Vexa | Meetily | MeetingBaas | Otter | Fireflies |
|---|---|---|---|---|---|---|---|
| Product type | macOS app | Productivity suite (app) | Dev API / infra | macOS/Win/Linux app | Dev API / infra | Cloud app | Cloud app |
| Capture model | Bot-free (mic + system audio) | Bot-free (device) | Bot-in-meeting | Bot-free (system audio) | Bot-in-meeting | Bot **+** native | Bot (mostly) |
| On-device transcription | ✅ Whisper/Metal | ❌ cloud LLM | 🟡 Whisper in self-host bot | ✅ Whisper/Parakeet | ❌ cloud (Gladia) | ❌ cloud | ❌ cloud |
| Local LLM / **nothing leaves** | ✅ on-device brain + Ollama | ❌ US cloud LLM | 🟡 only if self-hosted | ✅ Ollama offline | ❌ | ❌ | ❌ |
| Trains on your data | ❌ never (on-device) | 🟡 undisclosed | — (your infra) | ❌ (claim) | — (your data) | 🟡 **yes, default** | 🟡 no-by-default (policy) |
| Cross-meeting Ask (cited) | ✅ Ask-My-Vault `[[cites]]` | 🟡 "chat with notes" | ❌ (API only) | 🟡 "Chat" coming soon | ❌ | ✅ Otter AI Chat | ✅ AskFred |
| Self-assembling Person/Project graph | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ |
| Obsidian-native owned `.md` | ✅ wikilinks/block-refs/`.canvas` | ❌ proprietary | ❌ Postgres/API | 🟡 roadmap only | ❌ | ❌ | ❌ |
| Action items → Reminders **and** Tasks | ✅ Apple Reminders + Obsidian Tasks | 🟡 Notion/Todoist/Slack | ❌ | 🟡 in summary | ❌ | 🟡 assignable | 🟡 task mgr/CRM |
| Redaction firewall (PII scrub) | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ |
| Encryption at rest + Touch-ID lock | ✅ SQLCipher + per-folder AES + Touch ID | 🟡 E2E claim, unspecified | 🟡 infra-level | 🟡 marketing claim | 🟡 self-host | 🟡 AWS AES-256 (cloud) | 🟡 AES-256 (cloud) |
| MCP server | ✅ **local** `127.0.0.1` | ❌ | ✅ (agentic infra) | ❌ | ❌ | 🟡 **cloud** OAuth | 🟡 **cloud** |
| Per-remote-speaker diarization | 🟡 Me/Others (+opt. others-diarize) | 🟡 speaker labels | ✅ full diarization | 🟡 planned/Pro | ✅ | ✅ | ✅ |
| Conversation intelligence (sentiment/talk-time) | ❌ | ❌ | ❌ | ❌ | ❌ | 🟡 | ✅ |
| Team / collaboration / sharing | ❌ (roadmap) | 🟡 | 🟡 (infra) | 🟡 Enterprise | ✅ (API) | ✅ | ✅ |
| Open source | ❌ | ❌ | ✅ Apache-2.0 | ✅ MIT | 🟡 claimed, unverified | ❌ | ❌ |
| Platforms | macOS | macOS/iOS/Win | API/self-host | macOS/Win/Linux | API/self-host | Web/Mac/Win/iOS/Android | Web/Mac/Win/iOS/Android |
| Pricing (2026-07-01) | Free (Pro/Team roadmap) | Free / ~$15 Pro / Ent | Self-host free / $12 / PAYG | Free (MIT) / $10 Pro / Ent | $0–$299/mo + ~$0.44–0.63/hr | Free / $8.33 / $19.99 / Ent | Free / $10 / $19 / $39 |

---

## 4. Competitor profiles (condensed)

### Amie — https://amie.so/
All-in-one **calendar + tasks + bot-free AI notes** for macOS/iOS/Windows; meeting notes are one pillar of a
productivity suite. **Bot-free device recording** across Zoom/Meet/Teams/Slack, speaker separation, action items
to Notion/Todoist/Slack, AI day-planning. **The catch:** transcription/summaries run through a **US-hosted
third-party cloud LLM** — so it is *not* local-first; the privacy policy doesn't disclose storage location,
retention, at-rest encryption, or a training opt-out. Closed-source, no self-host, no Obsidian output.
**Wins vs us:** breadth (calendar+tasks+AI), cross-platform, polish. **We win:** true on-device AI, owned
Obsidian files, SQLCipher + Touch-ID lock, local MCP, redaction firewall.

### Vexa — https://vexa.ai/
**Open-source (Apache-2.0), bot-based meeting-transcription API/infra** — send a bot to Meet/Teams (Zoom
coming) and get real-time, speaker-diarized transcripts over WebSocket in 100+ languages; ships an **MCP server**
for agents; self-hostable (Docker/K8s) so data can stay on your infra. **Not an end-user app** — no notes UI, no
summaries, no calendar. 2.2k★, actively maintained. **Wins vs us:** OSS, real-time streaming, true per-speaker
diarization of remote participants, self-host at scale. **We win:** it's a finished single-user product — bot-free
capture, on-device brain, Obsidian-native files, Touch-ID encryption; Vexa is infra you must assemble.

### Meetily — https://meetily.ai/ · [GitHub](https://github.com/Zackriya-Solutions/meetily)
**Our closest head-to-head:** open-source (MIT), local-first, **bot-free** on-device meeting notes for
macOS/Win/Linux; Whisper + Parakeet transcription, **Ollama** offline summaries, 13.2k★, v0.4.0 (2026-06-05).
Same category, same stack philosophy (Tauri + Rust). **But** its **Obsidian integration, cross-meeting "Chat,"
and diarization are roadmap-only**, exports are flat PDF/DOCX/MD, "encryption at rest" is marketing (not in the
OSS code), install is heavier (Python/FastAPI/FFmpeg service), and there's **no local MCP**. **Wins vs us:**
cross-platform, big community, multi-engine transcription speed. **We win:** shipped second-brain layer
(Ask/graph/threads/digest), Obsidian-native owned files, per-folder Touch-ID lock, on-device brain, local MCP.

### MeetingBaas — https://www.meetingbaas.com/en
A **developer API** ("meeting-bots-as-a-service") — programmatically send recording/**speaking** bots into
Zoom/Meet/Teams, get transcripts (Gladia), calendar APIs, webhooks, 8+ SDKs; cloud by default with a self-host
option. **Not an end-user competitor** — it's the infrastructure you'd use to *build* a meeting product. Usage
priced: $0–$299/mo subscription **plus** ~$0.44–0.63/hr tokens; 3–30-day retention. **Contrast of approach, not a
rival:** cloud + bot + API pole, opposite Murmur's local + native + app pole. Complementary, not a substitute.

### Otter.ai — https://otter.ai/
Mature **cloud** assistant pivoting to an enterprise "system of record": OtterPilot bot auto-joins (or bot-free
desktop record), live transcription, summaries, **Otter AI Chat** across meetings, vertical AI agents, deep
CRM integrations, and a **cloud** OAuth **MCP server**. **Privacy is its weak flank:** cloud-only (AWS), **trains
on user data by default with no clear consumer opt-out**, and it carries **documented incidents** — the Sept-2024
"bot kept listening" leak (auto-emailed private post-meeting audio) and an **Aug-2025 class-action** over consent
and training. Free/$8.33/$19.99/Enterprise. **Wins vs us:** scale, polish, integrations, agents, multi-platform,
bot capture of remote calls. **We win:** the entire privacy story — nothing leaves the device, no training, owned
files, local MCP. *Otter's incident history is our single strongest talking point.*

### Fireflies.ai — https://fireflies.ai/
The **closest philosophical competitor** because it *also* leads with privacy. Cloud, mostly bot-based; strong
**conversation intelligence** (sentiment, talk-time, topic trackers), **AskFred** cross-meeting query, 200+ AI
Skills, ATS/CRM depth, API + **cloud MCP**. Genuinely better privacy than Otter: **no training by default (all
tiers)**, **0-day LLM retention**, and enterprise **BYO-bucket** storage — but these are **policy self-attestations**,
and it's still cloud + bot + no file ownership. Free/$10/$19/$39. **Wins vs us:** cloud bot capture of any remote
call, conversation analytics, team features, CRM/ATS, price-driven adoption, cross-platform. **We win:** the line
Fireflies can't match structurally — *"there's nothing to promise: it never leaves your Mac."* Their privacy is
**policy**; ours is **architecture**. Plus owned Obsidian files, Touch-ID encryption, local MCP.

### Other AI notes (round-up)
- **Granola** — closest *cloud* rival: bot-free Mac capture, calendar Brief, cited cross-meeting chat, official MCP; **but cloud, no Obsidian, no local processing.** ($1.5B-backed — highest long-term risk.)
- **Talat** — closest feature-fit: local, on-device model, Obsidian export **and a local MCP**; lacks the graph, redaction, Reminders/Tasks bridge, recipes/timeline. Closed source.
- **Hyprnote / anarlog** — OSS, local, Whisper + Ollama, Obsidian export, chat across notes; **no MCP, no graph.**
- **MegaMem** — local Obsidian graph + MCP, but **no meeting capture at all.**

*(Detail + a fuller matrix for these four: [`COMPETITIVE-LANDSCAPE.md`](./COMPETITIVE-LANDSCAPE.md).)*

---

## 5. What we HAVE (grounded in the code)

- **Bot-free dual-stream capture** — mic (`cpal`) + system audio (ScreenCaptureKit / Core Audio tap), transcribed separately and merged into **Me / Others** by wall-clock.
- **Fully on-device intelligence** — Whisper (Metal) transcription, an **on-device reasoning brain** (Bielik/Qwen GGUF via `mistralrs`), and **semantic search** (e5 vectors + `sqlite-vec`). With Ollama / `claude_code`, **nothing leaves the device**.
- **In-meeting voice assistant** — ask a question mid-call, get a grounded, cited answer live; recording never stops.
- **Ask-My-Vault** — cross-meeting Q&A grounded in your notes with `[[wikilink]]` + clickable source citations.
- **Self-assembling Person/Project knowledge graph** written as owned vault stubs with backlinks.
- **Obsidian-native output you own** — atomic `.md` with front-matter, `[[wikilinks]]`, `obsidian://` block-refs, `.canvas` export. No lock-in.
- **Recipes, weekly digest, deterministic Topic Threads, timeline (pin-a-moment), calendar-aware pre-meeting brief** (on-device EventKit).
- **Action items → Obsidian Tasks *and* Apple Reminders.**
- **Redaction firewall** (emails/phones/card-numbers scrubbed before any cloud call; NER person-name redaction when the model is present) + **fail-closed** cloud egress behind one-time consent.
- **Privacy-as-architecture at rest** — whole-DB **SQLCipher**, **per-folder AES-256-GCM** content keys wrapped by a master KEK released only by **Touch ID**, **verify-before-destroy** seals, **screen-share auto-relock**.
- **Local read-only MCP server** on `127.0.0.1:8765` — Claude Desktop/Code can query your meeting memory with **zero egress** (sealed meetings stay invisible).

## 6. What we DON'T have (honest gaps)

- **macOS only** — no Windows/Linux/mobile/web. (Meetily is cross-platform; Otter/Fireflies/Amie are multi-platform.)
- **No bot capture of remote/cloud-only meetings** — Murmur captures audio playing through the local Mac; it can't send a bot to a call you're not in, or capture a meeting on someone else's machine. Otter/Fireflies/Vexa/MeetingBaas can.
- **Coarse remote diarization** — the "Others" stream collapses all remote speakers into one label (optional diarization is best-effort). Vexa/Otter/Fireflies split every speaker.
- **No teams / collaboration / sharing / multi-user / org management** — single-user today (paid Pro/Team tiers are roadmap, not shipped).
- **No cross-device sync or cloud backup.**
- **No conversation intelligence analytics** (sentiment, talk-time) — Fireflies leads here.
- **Thin third-party integration surface** — no native Salesforce/HubSpot/Slack/ATS; competitors have deep CRM/ATS automations.
- **Not open-source, small footprint** — Meetily (13.2k★) and Vexa (2.2k★) have real communities; Murmur is early-stage with little brand recognition yet.

## 7. Strongest position (the moat)

1. **Privacy is structural, not a policy.** Otter trains on your data by default (with documented incidents + a class-action); Amie routes AI to a US cloud; Fireflies *promises* not to train. Murmur's answer is categorically different: **the audio and transcript never leave the Mac**, so there is nothing to promise, leak, subpoena, or train on. *Fireflies' privacy is a policy; Murmur's is an architecture.*
2. **The integrated bundle no one else ships.** Bot-free on-device capture **+** on-device brain **+** Obsidian-native owned files **+** self-assembling graph **+** cited cross-meeting Ask **+** local MCP **+** redaction firewall **+** per-folder Touch-ID encryption — every rival has at most a subset. Meetily is closest but its Obsidian/Chat/diarization are roadmap; Amie isn't on-device; the cloud players own none of the ownership/local layer.
3. **You own the files.** Plain Markdown in *your* Obsidian vault — unique across all six. Everyone else locks content in their cloud (or a Postgres/API).
4. **Local MCP over on-device data.** Otter and Fireflies expose *cloud* transcripts over OAuth; Murmur's MCP is local and read-only. "MCP" is now table-stakes — *where the data sits* is the differentiator.
5. **Per-folder Touch-ID encryption + lock model + screen-share auto-relock** — uncontested in this set.

## 8. Risks / who could close the gap

- **Meetily (closest).** Already local + bot-free + OSS with momentum. If it ships its roadmap Obsidian export + cross-meeting Chat + diarization + an MCP, it erodes most of our technical differentiation. Our durable edges then: the graph, redaction firewall, Touch-ID lock model, and product polish.
- **Granola (deep-pocketed).** $1.5B, already has Brief + cited chat + MCP; one "export to Obsidian vault" + a local-processing mode would erase much of our positioning. Structural blocker: cloud-first architecture.
- **Fireflies (messaging).** Owns the privacy narrative in the cloud tier; can't go on-device without abandoning its model, but can keep out-marketing us on features/price.
- **Amie.** If it moved AI on-device (or added a real no-training guarantee + owned export), its breadth would be hard to beat — but that contradicts its cloud model.

## 9. Positioning recommendation

Lead with the two uncontested claims and treat transcription/summaries/MCP as cost-of-entry:

> **"Murmur is the only meeting app whose brain lives on your Mac — bot-free capture, an on-device AI you can ask
> live, owned Markdown in your Obsidian vault, and per-folder Touch-ID encryption. Nothing leaves the device, so
> there's nothing to leak, train on, or subpoena."**

Against **Otter** → privacy + incidents + ownership. Against **Fireflies** → *architecture beats policy* + ownership.
Against **Meetily** → the shipped second-brain bundle + Obsidian-native fidelity + Touch-ID lock. Against **Amie** →
*bot-free isn't local — ours actually is.* Against **Vexa/MeetingBaas** → we're a finished product, not infra to assemble.

---

## Sources

Competitor facts verified 2026-07-01 against live vendor pages, GitHub, and corroborating reviews:
Amie ([amie.so](https://amie.so/), [/privacy](https://amie.so/privacy)), Vexa ([vexa.ai](https://vexa.ai/), [github.com/Vexa-ai/vexa](https://github.com/Vexa-ai/vexa)),
Meetily ([meetily.ai](https://meetily.ai/), [github.com/Zackriya-Solutions/meetily](https://github.com/Zackriya-Solutions/meetily)),
MeetingBaas ([meetingbaas.com](https://www.meetingbaas.com/en), [/pricing](https://www.meetingbaas.com/en/pricing)),
Otter ([otter.ai](https://otter.ai/), [/privacy-security](https://otter.ai/privacy-security), [MCP docs](https://help.otter.ai/hc/en-us/articles/35287607569687-Otter-MCP-Server), [class-action — NPR](https://www.npr.org/2025/08/15/g-s1-83087/otter-ai-transcription-class-action-lawsuit)),
Fireflies ([fireflies.ai](https://fireflies.ai/), [/security](https://fireflies.ai/security), [Private Storage](https://fireflies.ai/blog/fireflies-private-storage/)).
Murmur rows are grounded in this repo (README, `docs/KILLER-FEATURES.md`, and the `src-tauri/`/`src/` code). Companion: [`COMPETITIVE-LANDSCAPE.md`](./COMPETITIVE-LANDSCAPE.md).
