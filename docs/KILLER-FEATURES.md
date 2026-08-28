<!-- 2026-06-26 batch write-up. See docs/README.md for what in docs/ is current. -->

# Murmur — Killer Features (mega-batch)

> **Historical (2026-06-26), and read by the `research` skill as "what already shipped" — so be
> precise about what it is.** Everything below did ship. But this is one batch's write-up from the
> 0.5-era, not the shipped feature set: it predates Spaces, dashboards, imports, tasks, receipts, the
> link graph, Shared Brain and the whole 2.0 shell. **For the current set, read
> [`README.md` → Status](../README.md#%EF%B8%8F-status).**

This batch turns Murmur from "record → summarize → save note" into a **meeting memory system**:
your whole vault becomes queryable, self-organizing, and reachable from other AI tools — all
on-device, no new cloud egress.

Each feature is shipped **backend + UI**, lint/clippy clean, unit-tested, and behind the existing
provider seam (Claude Code / Anthropic API / Ollama). The redaction firewall (below) means even
the cloud provider only ever sees scrubbed text.

---

## ✅ Shipped

| # | Feature | What it does | Where |
|---|---------|--------------|-------|
| 1 | **Recipes / Generate** | Run grounded prompt templates over a transcript (grounded-email, decision-log, ticket, 1:1, standup, sales, interview) + save your own. | Meeting viewer → Recipes panel |
| 2 | **Action Items → Obsidian Tasks** | Parse the note's checklist into structured items (owner + 📅 due-date), write them as Obsidian-Tasks checkboxes, and push any item to **Apple Reminders**. | Meeting viewer → Action Items panel |
| 3 | **Timeline scrubber + pin-moment** | Topic spans become clickable **chapters**, hover-scrub preview on the track, and 📌 **pin a moment** → writes a `^block-ref` into the note and copies an `obsidian://` deep link. | Meeting viewer → Timeline |
| 4 | **Self-assembling graph** | "Link people & projects" resolves the note's entities and writes `[[Person]]` / `[[Project]]` stub notes with a backlink to the meeting — your Obsidian graph builds itself. | Meeting viewer → Connect to graph |
| 5 | **Ask-My-Vault** | Full-page chat **across every meeting**, grounded only in your notes, with `[[Title]]` citations and clickable **source** chips. Provider-budgeted corpus from search-relevant meetings. | Top nav → **Ask** |
| 6 | **Weekly Digest** | Synthesize the last 7/30 days into one note — recurring themes, decisions, **open action items rolled forward by owner** — written to `Vault/Digests/`. | Analytics → Weekly digest |
| 7 | **Topic Threads** | Cluster topic spans across the whole library into cross-meeting threads (every time a topic came up, chronologically). Deterministic, no LLM. | Analytics → Topic threads |
| 8 | **Obsidian Canvas** | Export a meeting as a spatial `.canvas` board: a meeting node + a topic card per span. | Meeting viewer → Export Canvas |
| 9 | **Pre-Meeting Brief** | A grounded prep card for an upcoming meeting (who you've met, what's still open, talking points), auto-prefilled from your **Calendar** (best-effort) or typed. | Record screen → Prepare |
| 10 | **Speaker rename** | Manually relabel a speaker ("User 1" → "Sarah") across a meeting's timeline. | Meeting viewer → Timeline |
| 11 | **Redaction firewall** | Emails, card numbers and phone numbers are **automatically scrubbed** before any text reaches the Anthropic API and **restored** in the reply. Local providers send nothing to the cloud. | Automatic (Settings → Privacy) |
| 12 | **Localhost MCP server** | Murmur runs an MCP server on `127.0.0.1:8765` exposing read-only tools (`search_meetings`, `get_meeting`, `list_recent_meetings`) so **Claude Desktop / Claude Code** can use your meetings as context. | Settings → Local MCP server |
| — | **Auto thematic foldering** | Already shipped earlier as `auto_organize` / `organize::classify_subfolder` (Settings → "Organize notes into topic folders"). | Settings |

### Notes language
Summaries are written in the language you choose (Settings → Notes language: Auto / English /
Polski / …); front-matter keys stay English for Obsidian compatibility.

---

## ⚠️ Honestly deferred (and why)

Two items from the wishlist are **intentionally not shipped blind**, because doing them
half-tested would put real data or build stability at risk. They are dedicated follow-ups:

### Full at-rest vault encryption (column / SQLCipher)
Murmur's DB lives in the macOS app-data dir and the API key is in the **Keychain** (encrypted).
With **FileVault** on (default on modern Macs) the database file is already encrypted at rest.

App-managed encryption *on top of that* (per-column AES-GCM, or swapping rusqlite for SQLCipher)
touches **every** note/segment read and write and requires a one-time migration of existing
plaintext rows. That migration cannot be verified safely against your real meetings headlessly —
a mistake means unrecoverable data. So it is scoped as its own task with a backup + migration
test, rather than shipped untested into a database you're actively using.

**Mitigations already in place:** FileVault (OS-level at-rest), Keychain (secrets), and the
redaction firewall (PII never leaves the device in cloud-provider calls).

### Full voice-print speaker diarization (ONNX)
Automatic, cross-meeting speaker identification needs an on-device speaker-embedding model
(ONNX) and a vector store — a model-shipping + bundling project on its own. The **manual**
half (rename a speaker per meeting, feature #10) is shipped today.

---

## Verification

Every feature passed: `cargo clippy -D warnings`, `cargo test` (71 passing), `ng lint`,
`ng build` (per-component style budgets respected), and each UI lane went through an adversarial
verify pass. The full gate is `scripts/ci.sh`.

The MCP server's JSON-RPC protocol layer is unit-tested (initialize / tools/list / tools/call /
notifications / errors); end-to-end interop with a running Claude Desktop should be confirmed by
adding the config snippet (Settings → Local MCP server) and restarting Claude Desktop.
