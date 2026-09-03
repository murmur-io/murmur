---
name: murmur-mcp
description: Query Murmur's local read-only MCP server for gated, indexed knowledge — semantic + full-text search over meetings and notes, entity dossiers, structured transcripts, open-commitment rollups, and typed note-folder database queries. Prefer this over grepping raw vault files. Use when you need to find, retrieve, or cross-reference meeting/note content that Murmur has indexed. Read-only, loopback-only, excludes sealed content.
---

# Querying Murmur's local MCP server

Murmur runs a **local, read-only MCP server** over its indexed store. Prefer it over grepping
raw vault files whenever you need to **find or retrieve** meeting/note content, because it gives
you what a file grep can't:

- **Semantic + full-text retrieval** (meaning-based search fused with keyword search), not just
  literal string matching.
- **Entity dossiers** — everything about one person or project across every meeting, assembled.
- **Structured transcripts** with speaker labels (`Me` / `Others`) and timestamps.
- **Open-commitment rollups** — every unfinished `- [ ]` action item, with owner and due date.
- **Typed-property queries** over a note-folder's columns (status, owner, priority, …).
- **Lock-gating for free** — sealed-and-not-unlocked content is invisible to every tool, so you
  never accidentally surface something the user locked.

## Endpoint and auth (verified against the server)

- **Address:** `http://127.0.0.1:8765` — loopback only. It is not reachable off the machine, and
  it rejects any request whose `Host` isn't `127.0.0.1:8765`/`localhost:8765` or whose browser
  `Origin` is cross-site (DNS-rebinding protection).
- **Protocol:** MCP JSON-RPC 2.0 over HTTP POST (`initialize`, `tools/list`, `tools/call`). Type
  `http`.
- **Auth:** a **bearer token, required by default.** Every method — including `initialize` and
  `tools/list` — needs `Authorization: Bearer <token>` or the server answers error `-32001`
  (`"unauthorized: bearer token required"`). The token is minted and stored in the user's macOS
  Keychain.

### Getting the config (no guessing the token)

The user copies a ready-made config from Murmur: **Settings → Privacy → Local MCP server →
Copy config**. That block already contains the correct token. It looks like:

```json
{
  "mcpServers": {
    "murmur": {
      "type": "http",
      "url": "http://127.0.0.1:8765",
      "headers": { "Authorization": "Bearer <token>" }
    }
  }
}
```

Add it to your Claude Code MCP config and restart Claude. (If the user turned the token
requirement off in Settings, the `headers` block is omitted and the server serves
unauthenticated — but on by default is the norm.)

## The tool catalog (20 read-only tools)

Every tool returns text; every tool excludes sealed-and-not-unlocked content.

1. **`search_meetings`** — `{ query }` — full-text search across meeting titles, transcripts,
   notes, and imported documents. Returns matching meetings/documents with snippets and ids.
   *e.g.* `search_meetings({ "query": "pricing decision Q3" })`.

2. **`search_semantic`** — `{ query }` — meaning-based search fused with full-text; finds
   relevant content even without the exact words. Falls back to keyword-only (and says so) when
   semantic search is disabled in Murmur settings.
   *e.g.* `search_semantic({ "query": "who is unhappy about the roadmap" })`.

3. **`get_meeting`** — `{ meetingId, transcriptFormat? }` — a meeting's AI note plus its full
   transcript, by id (from a `meeting:…` search hit). The transcript is **structured** by default
   (one line per segment, `[<start>–<end>] <Speaker>: <text>`, `Me`/`Others`/`Unknown`); pass
   `transcriptFormat: "plain"` for the flat text.
   *e.g.* `get_meeting({ "meetingId": "m-abc123" })`.

4. **`get_document`** — `{ documentId }` — the full body of one standalone note or imported
   document, by id (from a `document:…` search hit). Use this, not `get_meeting`, for ids in a
   search result's DOCUMENTS section.
   *e.g.* `get_document({ "documentId": "doc-xyz" })`.

5. **`list_recent_meetings`** — `{ limit? }` — the most recent meetings (title, date, status,
   id). `limit` defaults to 20, clamped to 1–100.
   *e.g.* `list_recent_meetings({ "limit": 10 })`.

6. **`get_open_commitments`** — `{ owner? }` — rolls up every open action item (`- [ ]`, not
   done) across meetings, each with owner, due date, and source `[[Title]]`. Optional
   case-insensitive `owner` filter. Answers "what's still open / what did I promise".
   *e.g.* `get_open_commitments({ "owner": "anna" })`.

7. **`get_entity_dossier`** — `{ entity }` — assembles a dossier on one person or project across
   all meetings: a timeline of mentions, that entity's open commitments, and co-occurring
   people/projects, each citing its source `[[Title]]`. Pass a name or id; you synthesize the
   "state of X" from the returned gated material.
   *e.g.* `get_entity_dossier({ "entity": "Project Atlas" })`.

8. **`query_database`** — `{ folder, filter? }` — queries the **typed properties** of a
   note-folder's notes as a small database (its Table/Board columns: status, owner, due, priority,
   …). `filter` is `key op value` clauses joined by `AND`/`OR`, with `op` one of
   `= != > < >= <= contains`; empty filter = every row.
   *e.g.* `query_database({ "folder": "Roadmap", "filter": "status=Open AND priority=High" })`.

9. **`org_search`** — `{ query }` — a **fallback** for when `search_meetings` / `search_semantic`
   find nothing in the user's own vault and they've joined an org: searches the organization's
   Shared Brain (notes colleagues explicitly shared, synced and decrypted locally — nothing
   leaves the device). Results are attributed `[org · <author>]` and must be cited as coming from
   that colleague. Returns nothing when no org is joined.
   *e.g.* `org_search({ "query": "onboarding checklist" })`.

10. **`search_transcript`** — `{ query, meetingId?, limit?, maxPerMeeting?, channel? }` — located
    lexical search INSIDE transcript segments. Each hit carries a timestamp, a stable segment id,
    the speaker, and character offsets in the same channel `get_meeting` accepts — so you can quote
    a moment, not just a meeting.
    *e.g.* `search_transcript({ "query": "we agreed to postpone", "limit": 10 })`.

11. **`get_meeting_chapters`** — `{ meetingId, channel? }` — the topic map for one meeting, each
    topic with a character range in the transcript. Use it to jump to a section instead of pulling
    the whole transcript.
    *e.g.* `get_meeting_chapters({ "meetingId": "m-abc123" })`.

12. **`get_document_outline`** — `{ documentId }` — the heading/section map and page numbers of one
    note or imported document. Read this before `get_document` when the document is long: it tells
    you which part is worth fetching.
    *e.g.* `get_document_outline({ "documentId": "doc-xyz" })`.

13. **`knowledge_diff`** — `{ entity, … }` — the DECISION LEDGER for one person or project: how
    what was known about it CHANGED over time, from the bitemporal fact store. Answers "when did
    this change, and what did it replace", which a dossier alone does not.
    *e.g.* `knowledge_diff({ "entity": "Project Atlas" })`.

14. **`list_entities`** — `{}` — every visible person and project with its exact id, type, and
    visible mention count. Call this BEFORE `get_entity_dossier` rather than guessing a name.
    *e.g.* `list_entities({})`.

15. **`list_note_folders`** — `{}` — visible note folders with the exact id/name `query_database`
    accepts, their record counts, and their typed columns. Call this before guessing a folder name
    or a column.
    *e.g.* `list_note_folders({})`.

16. **`list_workspace_hierarchy`** — `{}` — the workspace shape: every visible project, the folders
    inside it, and how many meetings / notes / tasks / dashboards each holds. The right first call
    when you do not yet know how this vault is organised.
    *e.g.* `list_workspace_hierarchy({})`.

17. **`list_dashboards`** — `{}` — the boards the user composed BY HAND out of meetings, notes,
    documents, people and derived views. These encode what the user thinks matters.
    *e.g.* `list_dashboards({})`.

18. **`get_dashboard`** — `{ dashboardId }` — one dashboard with every tile already resolved: the
    notes and recordings on it, who is on it, which values drifted, what was promised.
    *e.g.* `get_dashboard({ "dashboardId": "d-123" })`.

19. **`list_tasks`** — `{}` — SHARED ORG tasks the user and colleagues track together (title, id,
    status, due date, org). Empty when no org is joined.
    *e.g.* `list_tasks({})`.

20. **`get_task`** — `{ taskId }` — one shared org task: description, status, due date, assignee,
    subtask checklist, and the shared org notes it points at.
    *e.g.* `get_task({ "taskId": "t-456" })`.

## Limits — know these before you rely on it

- **Read-only.** There is no write, create, edit, or delete tool. To change a note, edit the
  vault file (following the `murmur-vault` skill's coexistence rules) — the MCP server never
  mutates anything.
- **Sealed content is invisible.** A locked-and-not-session-unlocked meeting or note-folder
  returns no results from any tool. If you know something exists but can't find it, it may be
  locked — don't work around the gate.
- **Loopback only, no egress.** The server is `127.0.0.1` only and makes no cloud calls; every
  answer is computed on-device from Murmur's local database.
