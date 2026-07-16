# Recording-time companion note — design spec

Date: 2026-07-16
Status: approved (design), pre-implementation
Branch: `feat/recording-companion-note`

## Problem

During an active recording the "Notes" panel (`MeetingConversationComponent`) lets the user
either jot a plain note or type `@brain <question>` to open an agentic thread. Two defects:

1. **A plain jot is a dead end.** It is written to the meeting's `manual_notes` buffer
   (`save_manual_notes` → a `\n`-joined plaintext blob on the meeting row). It never becomes a
   real note: it is not in the Notes surface, not a `.md` in the Obsidian vault, not linkable,
   not a first-class citizen of the user's second brain. This is the "note that adds nothing".
2. **You cannot reliably link a recording from a note via `[[name]]`.** `resolve_wikilink` has a
   meeting leg, but (a) during recording the meeting often has no stable human title yet
   (auto-title lands on close), (b) the resolver tries the note leg first, and (c) there is no
   structured `meeting_id` on a standalone note — the link is a fragile title string.

## Goal

Make in-recording note-taking produce **real, linked notes**, keep the thread-based `@brain`
interaction, and make the recording↔note link robust — with a satisfying "saved to Notes"
confirmation carrying the reference.

## Decisions (locked with the user)

- **One living companion note per meeting** (not one-note-per-jot, not document-first).
- **Structured `meeting_id` link + a visible `[[Meeting]]` wikilink** (root-cause, not a
  title-string patch).

## Model — companion note + thread

- **One meeting ⇒ one companion note.** A row in `documents` (`kind='note'`), created **lazily on
  the first sent note or first accepted `@brain` draft** — no content, no note (this kills the
  empty "Untitled" note complaint at its root).
- **No title field shown** (satisfies "bez tytułu"). Under the hood the note carries a managed
  title equal to the meeting's display title, kept in sync; the user never edits a title on this
  surface. String-collision with the meeting is handled deterministically in the resolver (see
  §Linking, self-link avoidance) — the companion note is never the target of its own `[[Meeting]]`.
- **Folder:** the always-open Notes ROOT (unfiled) — no lock complications while recording.
- **Thread stays primary.** The panel remains a thread that interleaves, in time order: the user's
  sent notes (rendered as "saved note" cards) and `@brain` exchanges. Every send appends a block to
  the companion note.

## Composer — reuse, not duplicate

New design-system component `mur-markdown-composer` (`src/app/design-system/markdown-composer/`):
an auto-growing textarea with `/` slash-block menu, `[[` link picker, ⌘B/⌘I/⌘1-3 formatting, list
continuation, Enter = send / Shift+Enter = newline. It **reuses** the already-presentational
`LinkPickerComponent`, `MarkdownComponent` (for preview/cards), and IPC
`listLinkCandidates`/`resolveWikilink`. The shipped `NoteEditorComponent` (2284 lines) is **left
untouched** in this change; migrating its inline editor onto the shared composer is an explicit
**fast-follow** (stated openly — this change does not yet dedupe the two editors).

## Linking (root-cause)

Two link artifacts, both derived from one authoritative structured relation:

1. **App-level (authoritative):** new additive column `documents.meeting_id TEXT` (nullable,
   indexed), set on the companion note at creation. Drives navigation (by id, never by string),
   and the meeting's "Linked mentions"/backlinks. Survives meeting rename/auto-title.
2. **Vault-level (Obsidian-native, user-visible):** on export, the note's YAML front-matter carries
   the meeting wikilink (e.g. `meeting: "[[<meeting display name>]]"`). Kept in sync with the
   meeting's final title on close/rename.

**Self-link avoidance (firm rule):** the companion note's title equals its meeting's title, so a
user-typed `[[Meeting]]` could otherwise hit the companion note via the note-leg-first order. Fix:
in `resolve_wikilink`, a note carrying a non-null `meeting_id` is **excluded from the note-leg when
the queried title equals that note's own meeting's title** — so `[[Meeting]]` always resolves to the
meeting, never to its companion note. Companion→meeting navigation (the card chip) goes by
`meeting_id` directly and never relies on string resolution.

**General wikilink fix (the user's explicit concern):** make `[[Meeting Title]]` reliably resolve
to a meeting: (a) ensure meetings are surfaced in `list_link_candidates` so the `[[` picker offers
them, (b) give the in-progress meeting a **stable provisional name at record start** if it has none,
so the link is meaningful immediately, (c) confirm the meeting leg of `resolve_wikilink` is reached
for meeting-kind targets even when a same-named note exists (structured link wins for the companion
case; for user-typed links, disambiguate by candidate kind chosen in the picker).

## Confirmation card ("fajne przedstawienie")

Each sent note renders in the thread as a card: the rendered markdown of the entry + a subtle
footer bar: `✓ Zapisano w Notatkach` (primary click → opens the companion note in a new tab via
`TabsService`) and a `🔗 [[Meeting]] →` chip (navigates to the meeting by `meeting_id`). Same card
shape for an accepted `@brain` draft.

## `@brain` + backward compat

- `@brain` thread flow unchanged. Its "Add to notes" (`acceptIntoNotes`) routes the accepted draft
  through the same append-to-companion-note path and shows the same card.
- **`manual_notes` is preserved additively.** Each send still updates `manual_notes` (the summary /
  "enhance my notes → outline" pipeline reads it) AND appends to the companion note. No change to
  the enhance feature; the companion note is the new durable, linked, vault-exported artifact.

## Technical seams

### Backend (`src-tauri/src/`)
- **Schema:** `add_column_if_missing(documents, meeting_id TEXT)` in `Db::migrate()` (additive,
  idempotent) + an index on `meeting_id`.
- **Command:** `append_to_companion_note(meeting_id, markdown) -> { note_id, meeting_wikilink }` —
  lazily gets-or-creates the companion note (in Notes ROOT, meeting_id set, managed title), appends
  the markdown block atomically (single-writer, no FE read-modify-write race), writes/refreshes the
  front-matter `[[Meeting]]` link, re-exports the vault `.md`, returns the id + the display wikilink
  for the card. Registered in `lib.rs generate_handler!`.
- **Wikilink:** extend `list_link_candidates` to include meetings; ensure the meeting leg of
  `resolve_wikilink` is reliable; ensure a stable provisional meeting name at record start.
- **Backlinks:** a note with `meeting_id` surfaces under that meeting's `get_backlinks`/"Linked
  mentions" (lock-gated as today).
- **Title sync:** when a meeting is (auto-)titled/renamed, refresh the companion note's managed
  title + front-matter wikilink target.

### Frontend (`src/app/`)
- `design-system/markdown-composer/` — the `mur-markdown-composer` (reuses `LinkPickerComponent`).
- `MeetingConversationComponent` — swap the plain composer textarea for `mur-markdown-composer`.
- `MeetingConversationStore.addNote()` and `acceptIntoNotes()` — call `appendToCompanionNote`, and
  render the returned reference in the saved-note card. `NoteItem` gains fields for the saved
  companion note id + meeting wikilink.
- `IpcService.appendToCompanionNote(meetingId, markdown)` — one typed method; result model
  `CompanionAppendResult { noteId: string; meetingWikilink: string }` in `core/models.ts`.
- `note-item.component` — render the saved-note card footer (open-note + meeting chip).

## Security / lock model

The companion note is user-authored content in the always-open Notes ROOT — it does not contain the
sealed transcript, only what the user typed + accepted drafts. Still routed through
**lock-security-reviewer**: every content read/export gated (`meeting_is_unlocked` /
`visibility_clause`), backlinks lock-gated, no PII in logs, the `meeting_id` column adds no leak
path. Schema change is additive (no `DROP`/`DELETE`); nothing is destroyed so verify-before-destroy
does not apply, but the append command must not blank prior content on failure.

## Verification (Definition of Done)

- Static: `cargo test --lib` + `npx ng lint` + `npx ng build` green; final `scripts/ci.sh`.
- RED→GREEN regression for the wikilink meeting-leg fix and for the append/lazy-create path.
- Runtime: Playwright against `:1420` with mocked Tauri `invoke` — send a note → card appears with
  the reference; `@brain` accept → same card; no stale-result/leak/abort.
- Adversarial-verifier owns PASS/FAIL; lock-security-reviewer is a required gate (touches
  reads/exports/backlinks/schema).

## Build phases (Workflow)

1. **Backend** — column + `append_to_companion_note` + wikilink/candidates fix + stable meeting
   name + backlinks + title sync; `cargo test --lib`.
2. **FE composer** — `mur-markdown-composer` reusing the link picker.
3. **FE rewire** — store/surface/card + IPC + models.
4. **Verify** — lock-security-reviewer + adversarial-verifier + `scripts/ci.sh`; QueaT commit; PR to
   `murmur`.

## Explicit non-goals / fast-follows

- Migrating `NoteEditorComponent`'s inline editor onto `mur-markdown-composer` (fast-follow).
- Backfilling companion notes for pre-existing meetings' `manual_notes` (new behavior applies going
  forward).
- Streaming `@brain` answers (separate Phase-9 work).
