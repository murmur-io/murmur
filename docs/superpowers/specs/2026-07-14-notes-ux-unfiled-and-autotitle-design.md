# Notes UX: unfiled-first notes + auto-title on close — design

**Date:** 2026-07-14 · **Status:** design (awaiting review) · **Target release:** 0.9.16 (with the
already-merged #321 unlock-gate + #322 create-message fixes).

## Problem

Two related note-UX pain points, both surfaced live by the user on 0.9.15:

1. **Redundant default folder + broken create.** "New note" from the Notes section defaults into a
   nested folder literally named **"Notes"** — redundant with the "Notes" section itself ("you're in
   Notes, and the note goes into Notes/Notes"). Worse: when that default folder is *locked*, create
   fails with "Couldn't create the note". Root cause: `create_note(None)` →
   `ensure_default_note_folder()` → the sealed "Notes" folder → the write-gate (`folder_is_unlocked`)
   refuses.
2. **Notes stay "Untitled".** New notes are created titled "Untitled" (`note-editor` line ~524,
   `notes-home.newNote`) and users don't retitle them.

## Feature A — unfiled-first notes (reserved always-open root)

### The constraint that shapes the approach (grounded, not assumed)

`documents.folder_id` is **`TEXT NOT NULL`** with `FOREIGN KEY (folder_id) REFERENCES folders(id) ON
DELETE CASCADE` (`db.rs` `CREATE TABLE documents`). SQLite has no `ALTER COLUMN`, so making it
nullable requires a **full table rebuild** (create-new → copy rows → drop → rename + recreate
indexes/FTS/triggers). That violates the binding rule *"migrations ADDITIVE only, never DROP / rewrite
user rows"* (`.claude/rules/rust-tauri.md` §4) and is risky given the FK, `text_blob`, and the notes
FTS/index. **Rejected.**

### Approach: a reserved, always-open "Notes root" folder

"Unfiled" is realized as a **reserved note-folder that can never be locked** and is presented as the
Notes section root — NOT as a nested folder. `folder_id` stays `NOT NULL` (satisfied by the root's
id). No schema rebuild, no NULL edge cases in any gate.

**Behavior:**
- A reserved note-folder (`kind='note'`, marked as root via an additive `is_root INTEGER` column or a
  reserved sentinel — see Open Questions) is ensured idempotently at startup. It is **always open** and
  `lock_folder` / the lock×shares flow **refuse** to seal it (a guard).
- `create_note(None)` targets the reserved root → **always succeeds** (the root is never locked).
- **FE does NOT render the reserved root as a nested tree row** — it *is* the "Notes" / "All notes"
  section. New notes are "unfiled" (they live at the root). Folders (e.g. "Tet") remain optional
  organization + the lock unit.
- **Privacy:** root/unfiled notes are open (plaintext) — the root can't be sealed. A subtle indicator
  ("Unfiled · not sealed") + a hint ("notes are private only inside a locked folder — move this note
  into one to seal it"). "Lock all" copy notes it does not cover unfiled notes. (User-approved: *"tak,
  to jest akceptowalne, można dać info że tylko w folderze notki są prywatne"*.)

**Existing "Notes" folder (legacy default):** left **untouched** — it becomes a normal, lockable
folder that keeps its notes; the user can rename/delete it (after unlocking, if sealed). No migration
of user rows. New reserved root is distinct from it. (For a fresh install there is no legacy "Notes"
folder, so the tree is clean; existing users see a one-time redundant "Notes" folder they can remove.)

### Backend
- **Migration (additive):** `add_column_if_missing(folders, "is_root", "INTEGER")` (default 0), OR a
  reserved id/path convention. `ensure_notes_root()` (idempotent) creates/returns the reserved root.
- `create_note(None)` → the reserved root (replace the `ensure_default_note_folder` default).
- `lock_folder` + the lock×shares probe: **refuse** to lock the reserved root (`AppError::InvalidArg`
  / a friendly refusal). "Lock all" (`relock_all` is relock-only, unaffected) — but a *lock* of the
  root must be impossible.
- **Visibility: NO change** — the root is a normal, always-unlocked folder, so `folder_is_unlocked` /
  `visibility_clause` already treat it correctly (this is the key win of the sentinel over nullable:
  zero new NULL/edge handling in the security gates).
- Vault export: reserved-root notes → `<vault>/Notes/<note>.md`; foldered notes unchanged.

### Frontend
- `notes-sidebar-tree`: filter the reserved root out of the rendered folder list (it's the section,
  not a child).
- `notes-home` / `note-editor`: show the folder name, or **"Unfiled"** for root notes; add a
  "Remove from folder" (→ move to root) action to the move menu.
- Privacy indicator + hint on unfiled notes.

### Review
- **lock-security review MANDATORY:** the reserved root is genuinely lock-refused; no gate is bypassed
  or wrongly masks; the legacy "Notes" folder still seals/unseals; no note is orphaned; the root note's
  openness is intended and cannot leak a *sealed* folder's content.

## Feature B — auto-title an "Untitled" note on close

### Trigger
On note **close** (`note-editor` destroy / `onTabBackgrounded` / navigate-away), if the note's title
is `"Untitled"` (or empty) **and** the body is non-empty → generate a title. (User: *"jak zamykasz
notkę to się generuje tytuł jak jest Untitled"*.)

### Model — local only
Use the on-device brain sidecar (`meetnotes-brain`, mistralrs) with a short prompt ("a 3–6 word title
for this note: <body excerpt>"). **Local-only, zero egress.**
- **Fallback** when the brain model isn't downloaded / the sidecar isn't ready: derive a title from the
  first non-empty line of the body (the `derive_artifact_title`-style heuristic already in the tree).
  If the body is empty → leave "Untitled".
- **Non-blocking:** fire on close in the background; the note may already be closed — update the DB row
  + the notes list/tab title when the result lands.

### Backend
- New command `suggest_note_title(note_id)` (or reuse a `note_assistant_action` "title" action if one
  already exists — verify during build): **gated** (a sealed-not-unlocked note refuses — read-gate),
  reads the note text, calls the local brain (fallback to first-line), and `update_note`s the title
  only if it is still "Untitled" (never clobber a user-set title — re-check under the write path).
- Reuses the existing note write-gate + local-brain seam; no new egress.

### Frontend
- `note-editor`: on close, if `title() === "Untitled"` and body non-empty, fire-and-forget
  `suggest_note_title`. Reflect the new title in the tab strip + notes list when it resolves.

### Out of scope (both features)
- Nullable `folder_id` / table rebuild (rejected above).
- Migrating existing notes out of the legacy "Notes" folder.
- Changing the per-folder lock model.
- Cloud-based title generation; bulk-retitling existing "Untitled" notes.

## Open questions for review
1. **Reserved-root marker:** an additive `folders.is_root` column (clean, explicit) vs a reserved
   id/path sentinel (no schema touch at all). Recommendation: `is_root` column — explicit + guardable.
2. **Legacy "Notes" folder:** leave untouched (recommended — no risky migration) vs auto-promote an
   *unlocked* legacy "Notes" folder to the reserved root (cleaner tree, more logic). Recommendation:
   leave untouched for now.
3. **Auto-title on a note in a locked folder:** the read-gate refuses → we simply skip (leave
   "Untitled"). Acceptable? (Recommendation: yes — don't auto-unlock for a title.)
