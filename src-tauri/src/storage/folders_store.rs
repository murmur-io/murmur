//! Folder + note-folder storage surface — the `folders` / `note_folders` / `note_folder_schemas`
//! table CRUD + tree/reparent + document/meeting folder-membership reads, extracted verbatim from
//! `storage::db` (God-file split, a PURE MOVE — no behavior change). The methods below are an
//! inherent-impl split of [`crate::storage::db::Db`] across files (Rust allows one type's inherent
//! `impl` to live in multiple files of the same crate); every method retains its EXACT prior body
//! and signature. Folders are CONTAINERS, not gated meeting content: these methods read/write the
//! folder rows themselves (name/path/parent/kind, the `locked` COLUMN as a plain flag, per-folder
//! wrapped-key blob, note-folder property schema, folder membership of a doc/meeting) — NONE call
//! `visibility_clause` / `meeting_is_unlocked`, and NONE seal/encrypt content. The gated readers
//! and the seal-coupled folder methods (`documents_in_folder`, `notes_in_folder`, `delete_folder`,
//! `count_notes_per_folder`, `discard_folder_seal`, `blank_sealed_notes_in_folders`,
//! `reblank_locked_folders_at_rest`, `has_hidden_folders`, `active_*_shares_for_folder`) STAY in
//! db.rs beside the visibility/seal machinery. Shared db.rs module-level helpers `map_err` + `lock`
//! are `pub(crate)`; the `RawDocument` DTO and the `folders`/`note_folders` schema stay in db.rs
//! (created inline in `Db::migrate()`). The row mappers `row_to_folder` / `row_to_note_folder`
//! (only ever used by these readers) moved along.

use std::collections::HashSet;

use rusqlite::{OptionalExtension, Row};

use crate::error::{AppError, Result};
use crate::storage::db::{map_err, visibility_clause, Db, RawDocument};
use crate::storage::models::{Folder, NoteFolder, PropertySchemaField};

/// The name a default project takes when no vault directory is configured to name it after.
/// Neutral on purpose: it is an ordinary container the user can rename like any other.
pub(crate) const DEFAULT_PROJECT_NAME: &str = "Workspace";

pub(crate) type MeetingNoteExportRow = (String, String, String, Option<String>);
pub(crate) type NoteFolderCatalogRow = (NoteFolder, i64, Vec<PropertySchemaField>);

impl Db {
    /// The owning folder id for a document, or `None` if unknown. The folder-lock gate anchor.
    pub fn folder_for_document(&self, id: &str) -> Result<Option<String>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT folder_id FROM documents WHERE id = ?1",
            rusqlite::params![id],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .map_err(map_err)
    }

    /// Distinct document ids governed by a folder's lock (its `documents` rows). Used to seal/unseal/
    /// purge each document's text + chunks.
    pub fn document_ids_in_folder(&self, folder_id: &str) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT id FROM documents WHERE folder_id = ?1")
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![folder_id], |r| r.get::<_, String>(0))
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// Raw `(text, text_blob)` for every document in a folder — the seal/unseal source-of-truth read
    /// (mirrors [`Db::raw_manual_notes`]). `text` is "" once sealed; `text_blob` carries the sealed
    /// copy. Used by the seal (encrypt+verify the plaintext), unseal (decrypt the blob), and reblank
    /// (re-blank only WHERE the blob exists) paths.
    pub fn raw_documents_in_folder(&self, folder_id: &str) -> Result<Vec<RawDocument>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT id, COALESCE(text, ''), text_blob FROM documents WHERE folder_id = ?1")
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![folder_id], |r| {
                Ok(RawDocument {
                    id: r.get(0)?,
                    text: r.get(1)?,
                    blob: r.get(2)?,
                })
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// Reparent a note-folder + rewrite the `path` of it and EVERY descendant (a prefix rewrite, so
    /// the subtree moves as a unit and `path` stays UNIQUE). Additive UPDATE only. `old_path` /
    /// `new_path` are the folder's vault-relative paths; `parent_id` is the new parent (NULL for a
    /// root note-folder). All in one tx.
    pub fn reparent_note_folder(
        &self,
        id: &str,
        old_path: &str,
        new_path: &str,
        parent_id: Option<&str>,
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        // Descendants first: rewrite every `<old_path>/…` prefix to `<new_path>/…`.
        // NOTES-3 (2026-07-11 audit): SQLite `substr()` counts CHARACTERS, not bytes, so the
        // suffix offset MUST be the char count of `old_path` (+1 for the following '/'), never the
        // Rust byte length — a multi-byte prefix (Polish "Sprzedaż") would otherwise slice mid-path
        // and corrupt every descendant. `char_length(old_path) + 1` picks up at the '/' after the
        // old prefix, so the leading slash survives into the rewritten path.
        let like = format!(
            "{}/%",
            old_path
                .replace('!', "!!")
                .replace('%', "!%")
                .replace('_', "!_")
        );
        tx.execute(
            "UPDATE folders
                SET path = ?1 || substr(path, ?2)
              WHERE kind = 'note' AND path LIKE ?3 ESCAPE '!'",
            rusqlite::params![new_path, (old_path.chars().count() + 1) as i64, like],
        )
        .map_err(map_err)?;
        // Then the folder itself: its own path + parent link.
        tx.execute(
            "UPDATE folders SET path = ?2, parent_id = ?3 WHERE id = ?1 AND kind = 'note'",
            rusqlite::params![id, new_path, parent_id],
        )
        .map_err(map_err)?;
        // NOTES-2 (2026-07-11 audit): rewrite every affected authored note's `documents.exported_path`
        // so it reflects the NEW on-disk vault path (the FS `.md` was physically moved by
        // `reparent_note_folder_paths`). Without this the path stays STALE and a later `lock_folder`
        // deletes nothing at that stale path — leaving the real plaintext `.md` on disk in a sealed
        // folder (a sealed-content leak). We replace the OLD path prefix with the NEW one for the moved
        // folder AND its descendants. `replace()` is byte-safe (substring, not offset) and multi-byte
        // safe. Scoped to notes whose folder is now under the new path (folders were rewritten above).
        tx.execute(
            "UPDATE documents
                SET exported_path = replace(exported_path, ?1, ?2)
              WHERE kind = 'note' AND exported_path IS NOT NULL
                AND instr(exported_path, ?1) > 0
                AND folder_id IN (
                    SELECT id FROM folders
                     WHERE kind = 'note' AND (path = ?2 OR path LIKE ?3 ESCAPE '!')
                )",
            rusqlite::params![
                old_path,
                new_path,
                format!(
                    "{}/%",
                    new_path
                        .replace('!', "!!")
                        .replace('%', "!%")
                        .replace('_', "!_")
                ),
            ],
        )
        .map_err(map_err)?;
        tx.commit().map_err(map_err)?;
        Ok(())
    }

    /// The vault `.md` paths of every authored NOTE in a folder that has one (`exported_path`
    /// non-NULL). Captured BEFORE seal so `lock_folder` can delete the on-disk `.md` (mirrors the
    /// meeting-notes `.md` deletion). No text — just paths (never PII-in-a-log; the caller doesn't
    /// log these).
    pub fn note_exported_paths_in_folder(&self, folder_id: &str) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT exported_path FROM documents
                   WHERE folder_id = ?1 AND kind = 'note' AND exported_path IS NOT NULL",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![folder_id], |r| r.get::<_, String>(0))
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// `(id, exported_path)` of every authored NOTE in a folder that has an on-disk export
    /// (2026-07-10 residual W5): the id lets the relock cleanup clear each note's `exported_path`
    /// INDIVIDUALLY, only after its `.md` was actually deleted (or is already absent) — a failed
    /// delete keeps the path recorded so the next relock/startup pass retries. No text — ids +
    /// paths only (the caller never logs the paths; they embed note titles).
    pub fn note_exported_path_rows_in_folder(
        &self,
        folder_id: &str,
    ) -> Result<Vec<(String, String)>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, exported_path FROM documents
                   WHERE folder_id = ?1 AND kind = 'note' AND exported_path IS NOT NULL",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![folder_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// Content-free cleanup authority for meeting-note vault exports in one folder. No markdown,
    /// title, transcript, or audio path is selected, so interrupted-lock repair can locate governed
    /// files before biometric authorization without pulling residual plaintext into the process.
    pub fn meeting_note_export_rows_in_folder(
        &self,
        folder_id: &str,
    ) -> Result<Vec<MeetingNoteExportRow>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT meeting_id, provider_id, exported_path, exported_hash
                   FROM notes
                  WHERE folder_id = ?1 AND exported_path IS NOT NULL",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![folder_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(map_err)?);
        }
        Ok(out)
    }

    /// NULL the `exported_path` (+ its path-coupled `exported_hash` baseline) of every authored
    /// NOTE in a folder (called on seal, after the vault `.md` files are deleted — a sealed note
    /// has no on-disk export). Both re-set on unlock re-export.
    pub fn clear_note_exported_paths_in_folder(&self, folder_id: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE documents SET exported_path = NULL, exported_hash = NULL
               WHERE folder_id = ?1 AND kind = 'note'",
            rusqlite::params![folder_id],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Ids of every authored NOTE in a folder (its `documents(kind='note')` rows). Used to re-export
    /// each note's vault `.md` on unlock/remove-lock. Mirrors [`Db::document_ids_in_folder`] but
    /// scoped to notes.
    pub fn note_ids_in_folder(&self, folder_id: &str) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT id FROM documents WHERE folder_id = ?1 AND kind = 'note'")
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![folder_id], |r| r.get::<_, String>(0))
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// Reassign an authored NOTE to a different note-folder (the gate/seal anchor). The COMMAND
    /// layer gates both the source and target folder. Idempotent on an unknown id / non-note row.
    /// Named `_doc` to disambiguate from the MEETING-note [`Db::set_note_folder`].
    pub fn set_note_doc_folder(&self, id: &str, folder_id: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE documents SET folder_id = ?2 WHERE id = ?1 AND kind = 'note'",
            rusqlite::params![id, folder_id],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Read a note-folder's declared property SCHEMA (Feature C). Returns the parsed field list, or
    /// an EMPTY vec when the folder has no schema row. Content-free metadata: the schema declares
    /// column names/types, never any note content. NOT lock-gated at this layer — the COMMAND layer
    /// gates on the folder's session-unlock state (a locked folder's schema is deliberately not
    /// exposed). A malformed `schema_json` (should never happen — we only ever write it via
    /// [`Self::set_note_folder_schema`]) degrades to an empty vec rather than erroring the read.
    pub fn get_note_folder_schema(&self, folder_id: &str) -> Result<Vec<PropertySchemaField>> {
        let conn = self.lock();
        let json: Option<String> = conn
            .query_row(
                "SELECT schema_json FROM note_folder_schemas WHERE folder_id = ?1",
                rusqlite::params![folder_id],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(map_err)?;
        match json {
            None => Ok(Vec::new()),
            Some(s) => Ok(serde_json::from_str::<Vec<PropertySchemaField>>(&s).unwrap_or_default()),
        }
    }

    /// UPSERT a note-folder's property schema (Feature C). Serializes `fields` to the `schema_json`
    /// column, inserting a new row or replacing the existing one via `ON CONFLICT(folder_id)`. The
    /// COMMAND layer validates the fields (count/key/options) and gates the write on the folder's
    /// session-unlock state BEFORE calling this — this is the raw persistence.
    pub fn set_note_folder_schema(
        &self,
        folder_id: &str,
        fields: &[PropertySchemaField],
    ) -> Result<()> {
        let schema_json = serde_json::to_string(fields)
            .map_err(|e| AppError::Storage(format!("schema serialize failed: {e}")))?;
        let now = chrono::Utc::now().timestamp_millis();
        let conn = self.lock();
        conn.execute(
            "INSERT INTO note_folder_schemas (folder_id, schema_json, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(folder_id) DO UPDATE SET schema_json = ?2, updated_at = ?3",
            rusqlite::params![folder_id, schema_json, now],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Ensure the root note-folder exists (name "Notes", `kind='note'`, path "Notes") and return its
    /// id. Idempotent: on the SECOND+ call it finds the existing row by its unique `path` and returns
    /// that id (never a duplicate). Every note anchors on a note-folder (the gate anchor).
    pub fn ensure_default_note_folder(&self) -> Result<String> {
        if let Some(f) = self.folder_by_path("Notes")? {
            return Ok(f.id);
        }
        let id = uuid::Uuid::new_v4().to_string();
        let conn = self.lock();
        // Guard against a race / a pre-existing "Notes" path created between the check and here:
        // INSERT OR IGNORE on the UNIQUE path, then read the id back (ours or the winner's).
        conn.execute(
            "INSERT OR IGNORE INTO folders (id, name, path, parent_id, locked, wrapped_key, created_at, kind)
             VALUES (?1, 'Notes', 'Notes', NULL, 0, NULL, ?2, 'note')",
            rusqlite::params![id, chrono::Utc::now().to_rfc3339()],
        )
        .map_err(map_err)?;
        conn.query_row("SELECT id FROM folders WHERE path = 'Notes'", [], |r| {
            r.get::<_, String>(0)
        })
        .map_err(map_err)
    }

    /// True when this folder is the reserved always-open note-root (`is_root=1`). Drives the
    /// `lock_folder` refusal so the root can never be sealed.
    pub fn folder_is_root(&self, id: &str) -> Result<bool> {
        let conn = self.lock();
        conn.query_row(
            "SELECT COALESCE(is_root, 0) FROM folders WHERE id = ?1",
            rusqlite::params![id],
            |r| Ok(r.get::<_, i64>(0)? != 0),
        )
        .optional()
        .map(|o| o.unwrap_or(false))
        .map_err(map_err)
    }

    /// Insert a note-folder (`kind='note'`). Mirrors [`Db::insert_folder`] but stamps the kind.
    pub fn insert_note_folder(&self, f: &NoteFolder, created_at: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO folders (id, name, path, parent_id, locked, wrapped_key, created_at, kind)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, 'note')",
            rusqlite::params![
                f.id,
                f.name,
                f.path,
                f.parent_id,
                f.locked as i64,
                created_at,
            ],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// All note-folders (`kind='note'`), creation order. The Notes tree; the Meetings tree
    /// (`kind != 'note'`) is served by [`Db::list_folders`], which is unchanged.
    pub fn list_note_folders(&self) -> Result<Vec<NoteFolder>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, name, path, parent_id, locked, kind, COALESCE(is_root, 0)
                   FROM folders WHERE kind = 'note' ORDER BY created_at, name",
            )
            .map_err(map_err)?;
        let rows = stmt.query_map([], row_to_note_folder).map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// The local-MCP note-folder discovery catalog: every VISIBLE note folder, its visible record
    /// count, and its typed schema. This is the single resolver input for `list_note_folders` and
    /// `query_database`, so an exact locked name/id, the alternatives list, the count, and schema all
    /// share one lock decision.
    ///
    /// LOCK MODEL: a sealed-and-not-session-unlocked folder is absent from this result entirely.
    /// Its name, id, row count, and schema are all sensitive metadata and must remain
    /// indistinguishable from an unknown folder. No note body is read here.
    pub fn list_note_folder_catalog_visible(
        &self,
        unlocked: &HashSet<String>,
    ) -> Result<Vec<NoteFolderCatalogRow>> {
        let conn = self.lock();
        let visible = visibility_clause("f", unlocked);
        let sql = format!(
            "SELECT f.id, f.name, f.path, f.parent_id, f.locked, f.kind,
                    COALESCE(f.is_root, 0),
                    (SELECT COUNT(*)
                       FROM documents d
                      WHERE d.folder_id = f.id AND d.kind = 'note') AS record_count,
                    COALESCE(nfs.schema_json, '[]') AS schema_json
               FROM folders f
               LEFT JOIN note_folder_schemas nfs ON nfs.folder_id = f.id
              WHERE f.kind = 'note' AND {visible}
              ORDER BY f.created_at, f.name COLLATE NOCASE, f.id"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map([], |row| {
                let mut folder = row_to_note_folder(row)?;
                folder.unlocked = folder.locked && unlocked.contains(&folder.id);
                let record_count: i64 = row.get(7)?;
                let schema_json: String = row.get(8)?;
                let schema = serde_json::from_str::<Vec<PropertySchemaField>>(&schema_json)
                    .unwrap_or_default();
                Ok((folder, record_count, schema))
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(map_err)?);
        }
        Ok(out)
    }

    /// A note-folder by id (`kind='note'` enforced), or `None`. Used to gate note-folder ops so a
    /// meeting-folder id can't be driven through a note-folder command.
    /// A note folder by id.
    ///
    /// Excludes any container whose path is the VAULT ROOT. Note folders live under the note root, so
    /// one at the vault root is not a note folder in any useful sense — and this is the resolution
    /// point `move_note_folder_inner` goes through before composing `src`/`dst` from a container's own
    /// path. An empty path resolves to the vault directory itself, so allowing it here would let that
    /// move relocate the user's whole vault. Refusing at the resolver closes it for every caller of
    /// this method at once, whatever shape the row arrived in (this migration creates a meeting-kind
    /// project there, but an import could produce a note-kind one).
    pub fn note_folder_by_id(&self, id: &str) -> Result<Option<NoteFolder>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT id, name, path, parent_id, locked, kind, COALESCE(is_root, 0)
               FROM folders WHERE id = ?1 AND kind = 'note' AND path <> ''",
            rusqlite::params![id],
            row_to_note_folder,
        )
        .optional()
        .map_err(map_err)
    }

    /// Resolve a note-folder by NAME (case-insensitive, over [`Self::list_note_folders`]) OR by exact
    /// id (Feature C — the `query_database` brain tool's `folder` argument). Name match is tried
    /// first (the FIRST match wins on a name collision); if no name matches, an exact id is tried.
    /// `Ok(None)` when neither resolves — the tool turns that into a friendly "no note folder named X".
    /// Note-folders only (`kind='note'`), so a meeting folder can never be driven through the tool.
    pub fn note_folder_by_name_or_id(&self, folder: &str) -> Result<Option<NoteFolder>> {
        let needle = folder.trim();
        if needle.is_empty() {
            return Ok(None);
        }
        for f in self.list_note_folders()? {
            if f.name.eq_ignore_ascii_case(needle) {
                return Ok(Some(f));
            }
        }
        self.note_folder_by_id(needle)
    }

    /// The `kind` of a folder (or `None` if unknown). Lets the command layer reject cross-tree ops.
    pub fn folder_kind(&self, id: &str) -> Result<Option<String>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT COALESCE(kind, 'meeting') FROM folders WHERE id = ?1",
            rusqlite::params![id],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .map_err(map_err)
    }

    /// Insert a folder row. `path` is the vault-relative folder path (UNIQUE).
    /// Insert a container that is SEALED from its first instant.
    ///
    /// One statement, so there is no moment at which the row exists unsealed. Creating it
    /// open and locking it afterwards is two writes with a window between them, and that
    /// window is the exact state the creation guard exists to prevent: an open container
    /// inside a sealed one. No undo can close it — an undo that itself fails leaves the row
    /// behind — so it is not opened.
    pub(crate) fn insert_sealed_folder(&self, f: &Folder, wrapped_key: &[u8]) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO folders (id, name, path, parent_id, locked, wrapped_key, created_at, kind)
             VALUES (?1, ?2, ?3, ?4, 1, ?5, ?6, ?7)",
            rusqlite::params![f.id, f.name, f.path, f.parent_id, wrapped_key, f.created_at, "meeting"],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// The note-kind twin of [`Db::insert_sealed_folder`].
    pub(crate) fn insert_sealed_note_folder(
        &self,
        f: &NoteFolder,
        created_at: &str,
        wrapped_key: &[u8],
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO folders (id, name, path, parent_id, locked, wrapped_key, created_at, kind)
             VALUES (?1, ?2, ?3, ?4, 1, ?5, ?6, 'note')",
            rusqlite::params![f.id, f.name, f.path, f.parent_id, wrapped_key, created_at],
        )
        .map_err(map_err)?;
        Ok(())
    }

    pub fn insert_folder(&self, f: &Folder) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO folders (id, name, path, parent_id, locked, wrapped_key, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6)",
            rusqlite::params![
                f.id,
                f.name,
                f.path,
                f.parent_id,
                f.locked as i64,
                f.created_at,
            ],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Insert a peer top-level Space. Unlike the migration-owned default project, a user-created
    /// Space owns its own vault-relative directory and therefore has a non-empty unique path.
    pub(crate) fn insert_space(&self, f: &Folder) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO folders
               (id, name, path, parent_id, locked, wrapped_key, created_at, kind, level)
             VALUES (?1, ?2, ?3, NULL, 0, NULL, ?4, 'meeting', 'project')",
            rusqlite::params![f.id, f.name, f.path, f.created_at],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// All folders (creation order). The tree is assembled by the caller.
    pub fn list_folders(&self) -> Result<Vec<Folder>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, name, path, parent_id, locked, created_at
                   FROM folders
                  WHERE COALESCE(kind, 'meeting') != 'task'
                  ORDER BY created_at, name",
            )
            .map_err(map_err)?;
        let rows = stmt.query_map([], row_to_folder).map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// Every folder's `kind` (`"meeting"` | `"note"`), keyed by id — so the FE can render ONLY meeting
    /// folders in the Meetings tree (note folders share the `folders` table and would otherwise leak
    /// into it). Legacy rows with a NULL kind default to `"meeting"`. (2026-07-14.)
    pub fn folder_kinds(&self) -> Result<std::collections::HashMap<String, String>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, COALESCE(kind, 'meeting') FROM folders
                  WHERE COALESCE(kind, 'meeting') != 'task'",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(map_err)?;
        let mut out = std::collections::HashMap::new();
        for r in rows {
            let (id, kind) = r.map_err(map_err)?;
            out.insert(id, kind);
        }
        Ok(out)
    }

    /// One-time adoption of every existing container into a default PROJECT.
    ///
    /// # Why this moves nothing on disk
    ///
    /// `folders.path` is a real Obsidian directory and the UNIQUE key `folder_by_path` resolves;
    /// three columns hold absolute paths derived from it (`notes.exported_path`,
    /// `documents.exported_path`, the attachment replicas). Re-rooting every container under a
    /// project would mean moving the user's actual vault directories AND rewriting every stored
    /// absolute path, across two systems with no shared transaction — and a stale `exported_path`
    /// means `lock_folder` deletes nothing and the real plaintext survives inside a folder the app
    /// reports as sealed (the NOTES-2 leak, already paid for). A locked subtree cannot be moved at
    /// all (`ensure_folder_subtree_unlocked` refuses), so such a migration could not even complete.
    ///
    /// The default project therefore takes the vault ROOT as its path — the empty string.
    /// `create_folder` composes a child path as `parent.path + '/' + name` only when the parent path
    /// is non-empty, and `assert_in_vault` resolves an empty relative path to the vault root, so
    /// **every existing container keeps its path byte-identical** and nothing is created, moved or
    /// removed on disk. `path` being UNIQUE is exactly right: only one row may be the vault root.
    ///
    /// # Why the guard is "no project exists" rather than a flag
    ///
    /// A flag records that the migration RAN; the check below records that its RESULT is present. If
    /// a user later deletes their last project, a flag would leave every container orphaned with no
    /// parent and no project, and they would vanish from the tree with nothing to repair them. This
    /// form self-heals, and it is equally idempotent: with any project present it is a no-op. It is
    /// also what lets the migration DECLINE to adopt (below) and simply retry on the next launch.
    ///
    /// # What it never touches
    ///
    /// No `path`, no `locked`, no `wrapped_key`, no `*_blob`, and no system container — not written,
    /// not read. A database with sealed containers migrates with no key and no biometric prompt. The
    /// whole adoption inherits `Db::migrate`'s transaction, so a crash leaves a database either
    /// wholly adopted or untouched.
    pub(crate) fn migrate_hierarchy_v1(conn: &rusqlite::Connection) -> Result<()> {
        // Same exclusion the adoption itself uses: a machine-owned container is never adopted, so it
        // must not count as an orphan either — otherwise its mere presence would make every database
        // look incomplete forever.
        const NOT_SYSTEM: &str = "COALESCE(kind, 'meeting') IN ('meeting', 'note')
             AND COALESCE(path, '') <> '.murmur'
             AND COALESCE(path, '') NOT LIKE '.murmur/%'";
        // Completion is recognised from the FULL result, not from a fragment of it: a meeting-kind
        // container at project level occupying the vault root, AND no eligible container still
        // sitting outside it. The shape alone is not enough — an import or a manual edit can produce
        // exactly that row while leaving other containers unadopted, and treating it as done would
        // report a database complete while its containers stayed orphaned, on this launch and every
        // later one.
        //
        // A partial result cannot come from this function: the adoption below runs inside its own
        // savepoint. So a project WITH orphans beside it is an occupant, and takes the same decline
        // path as any other occupant — no writes, and a later launch retries once the vault root is
        // free. Since `path` is UNIQUE, requiring `path = ''` bounds the project half to one row.
        let project_at_root: bool = conn
            .query_row(
                "SELECT EXISTS(
                   SELECT 1 FROM folders
                    WHERE COALESCE(level, 'folder') = 'project'
                      AND path = ''
                      AND COALESCE(kind, 'meeting') = 'meeting')",
                [],
                |r| Ok(r.get::<_, i64>(0)? != 0),
            )
            .map_err(map_err)?;
        let orphans_remain: bool = conn
            .query_row(
                &format!(
                    "SELECT EXISTS(
                       SELECT 1 FROM folders
                        WHERE parent_id IS NULL AND path <> ''
                          AND COALESCE(level, 'folder') <> 'project' AND {NOT_SYSTEM})"
                ),
                [],
                |r| Ok(r.get::<_, i64>(0)? != 0),
            )
            .map_err(map_err)?;
        if project_at_root && !orphans_remain {
            return Ok(());
        }
        conn.execute_batch("SAVEPOINT hierarchy_v1")
            .map_err(map_err)?;
        let outcome = Self::adopt_containers_into_default_project(conn);
        match &outcome {
            Ok(()) => conn.execute_batch("RELEASE hierarchy_v1"),
            // Roll the whole adoption back, then release the savepoint so the enclosing migration
            // transaction is left exactly as it was found.
            Err(_) => conn.execute_batch("ROLLBACK TO hierarchy_v1; RELEASE hierarchy_v1"),
        }
        .map_err(map_err)?;
        outcome
    }

    /// The adoption itself, always called inside [`Db::migrate_hierarchy_v1`]'s savepoint.
    fn adopt_containers_into_default_project(conn: &rusqlite::Connection) -> Result<()> {
        // A SAVEPOINT, not a transaction: `Db::migrate` already runs every step inside one, so a
        // nested BEGIN would simply fail — but the adoption below is four separate writes, and its
        // completion guard is RESULT-based, so a failure after the project row exists and before the
        // re-parent passes would leave containers orphaned AND make every later launch short-circuit
        // at the guard with nothing to repair them. Owning the boundary here makes "either wholly
        // adopted or untouched" a property of this function rather than an inherited assumption.

        // Every statement below carries this. A system container must be neither adopted, nor
        // re-parented, nor re-levelled, nor even READ — the reserved container is guarded by
        // RAISE(ABORT) triggers and this runs inside `migrate()`, before any content surface, so a
        // write against it is not untidiness but a failure to START the app.
        //
        // `COALESCE(path, '')` because `path NOT LIKE …` evaluates to NULL for a NULL path, which
        // would silently drop such a row from adoption with no statement of intent; and the prefix
        // guard covers the reserved directory ITSELF as well as its descendants.
        const NOT_SYSTEM: &str = "COALESCE(kind, 'meeting') IN ('meeting', 'note')
             AND COALESCE(path, '') <> '.murmur'
             AND COALESCE(path, '') NOT LIKE '.murmur/%'";

        // ── the vault-root slot ──────────────────────────────────────────────────────────────────
        //
        // Exactly one row may hold `path = ''` (UNIQUE), so if something already occupies it, either
        // that row becomes the project or there is no project this run.
        // ── the vault-root slot ─────────────────────────────────────────────────────────────────
        //
        // Exactly one row may hold `path = ''` (UNIQUE), so if anything already occupies it there is
        // nowhere to put the project. This DECLINES rather than promoting the occupant, and rather
        // than aborting.
        //
        // Not promoting is the point. Promotion looked harmless — it is a user's own container, and
        // the level does not affect gating — but it produced a row that is a project AND carries the
        // occupant's kind and contents, and two things then followed from that. The legacy-tree shim
        // hides project rows, so a promoted container and everything filed in it would vanish from
        // the shipped sidebar, which is exactly what the compatibility shim exists to prevent. And a
        // promoted note-kind row is a note folder at the vault root, which the note-folder move
        // resolves and would then compose filesystem work from — against the vault root itself.
        // Declining removes both, and keeps every project this migration produces a freshly created
        // meeting-kind row, which is what the rest of the safety argument assumes.
        //
        // Aborting is not an option either: this runs inside `migrate()`, before any content surface,
        // so an abort is a failure to START the app. Declining completes: the legacy shim keeps the
        // shipped sidebar rendering exactly what it renders today, and because completion is
        // recognised from the RESULT rather than a flag, a later launch adopts as soon as the slot is
        // free.
        //
        // `path = ''` exactly, never `COALESCE(path, '')`: a UNIQUE index does not de-duplicate NULLs,
        // so a NULL-path row could coexist with a genuine one and be picked by the planner.
        //
        // No shipped creation path can produce a container at the vault root — the single name
        // sanitiser both container kinds use refuses everything that reduces to an empty component —
        // so this branch is defensive against an import or a manual edit, not a supported flow. See
        // `no_creation_path_can_produce_a_vault_root_container`.
        let occupied: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM folders WHERE path = '')",
                [],
                |r| Ok(r.get::<_, i64>(0)? != 0),
            )
            .map_err(map_err)?;
        if occupied {
            tracing::warn!(
                target: "storage",
                "the vault root is already occupied; leaving the hierarchy un-adopted"
            );
            return Ok(());
        }

        // Named after the vault directory when one is configured; on a fresh database none is, and a
        // neutral name is used. The user can rename it like any container.
        let vault: Option<String> = conn
            .query_row(
                "SELECT value FROM settings WHERE key = 'vault_path'",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(map_err)?;
        let name = vault
            .as_deref()
            .filter(|v| !v.trim().is_empty())
            .and_then(|v| std::path::Path::new(v).file_name())
            .map(|s| s.to_string_lossy().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_PROJECT_NAME.to_string());
        let project_id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO folders
               (id, name, path, parent_id, locked, wrapped_key, created_at, kind, level)
             VALUES (?1, ?2, '', NULL, 0, NULL, ?3, 'meeting', 'project')",
            rusqlite::params![project_id, name, chrono::Utc::now().to_rfc3339()],
        )
        .map_err(map_err)?;

        // ── note containers go under the note root their PATH is actually stamped under ──────────
        //
        // `create_note_folder_inner` stamps every note container's path under a note root while
        // leaving its parent link NULL, so the parent tree and the path tree disagree for every one
        // of them today. `rename_folder_inner` recomposes a container's path from its CURRENT
        // parent, so adopting them straight into the project would make the next rename silently
        // relocate the real vault directory — leaving `exported_path` stale, which is the named
        // sealed-content leak: the seal deletes the plaintext `.md` at the recorded path, so it
        // deletes nothing and the real file survives inside a sealed folder.
        //
        // Choosing the root by `is_root` alone is not enough. `ensure_notes_root` deliberately
        // creates a SEPARATE root when the existing one is locked (its "Inbox N" fallback), so a real
        // database can hold more than one — and picking the wrong one produces exactly the broken
        // composition described above. So each container is matched to the root whose path composes
        // ITS path: `container.path == root.path || '/' || container.name`. A container that matches
        // no root keeps a NULL parent here and is adopted by the project pass below, which is the
        // safe fallback — its composition is no more broken than it already is today with a NULL
        // parent, and no vault directory moves either way.
        const NOTE_ROOT_MATCH: &str = "SELECT r.id FROM folders r
              WHERE COALESCE(r.is_root, 0) = 1
                AND COALESCE(r.kind, 'meeting') = 'note'
                AND COALESCE(r.path, '') <> ''
                AND COALESCE(r.kind, 'meeting') IN ('meeting', 'note')
                AND COALESCE(r.path, '') <> '.murmur'
                AND COALESCE(r.path, '') NOT LIKE '.murmur/%'
                AND folders.path = r.path || '/' || folders.name
              ORDER BY r.path, r.id
              LIMIT 1";
        conn.execute(
            &format!(
                "UPDATE folders
                    SET parent_id = ({NOTE_ROOT_MATCH})
                  WHERE parent_id IS NULL
                    AND id <> ?1
                    AND COALESCE(is_root, 0) = 0
                    AND COALESCE(kind, 'meeting') = 'note'
                    AND {NOT_SYSTEM}
                    AND EXISTS ({NOTE_ROOT_MATCH})"
            ),
            rusqlite::params![project_id],
        )
        .map_err(map_err)?;

        // ── everything else that was a root becomes a child of the project ───────────────────────
        //
        // Including the note root itself, and any note container the composition match above
        // declined. Containers that already had a parent keep it: existing depth is preserved.
        conn.execute(
            &format!(
                "UPDATE folders SET parent_id = ?1
                  WHERE parent_id IS NULL AND id <> ?1
                    AND COALESCE(level, 'folder') <> 'project' AND {NOT_SYSTEM}"
            ),
            rusqlite::params![project_id],
        )
        .map_err(map_err)?;

        // Every non-project container states its level explicitly, so the tree never has to infer one
        // from a NULL parent. Carries the same exclusion as the two statements above: a system row is
        // inert here only by accident today (its level coalesces out of the predicate), and any future
        // import that stamped a non-'folder' level on it would turn this into a write against a
        // trigger-guarded row during startup.
        conn.execute(
            &format!(
                "UPDATE folders SET level = 'folder'
                  WHERE id <> ?1 AND COALESCE(level, 'folder') NOT IN ('project', 'folder')
                    AND {NOT_SYSTEM}"
            ),
            rusqlite::params![project_id],
        )
        .map_err(map_err)?;

        Ok(())
    }

    /// Remove a container row that was created moments ago and must not survive.
    ///
    /// Deliberately NOT `delete_folder`: that one carries the whole user-facing delete contract —
    /// refuse a non-empty subtree, unseal a sealed folder first, reparent authored notes. This is the
    /// undo half of "create, then seal", where the row is empty because nothing has had the chance to
    /// reference it. Leaving it behind is what would be dangerous: an OPEN container inside a sealed
    /// one, which the at-rest re-blank sweep will never visit because that sweep keys off containers
    /// marked locked.
    ///
    /// "Nothing references it" is ENFORCED here rather than left to the call site — for the
    /// three tables that can name a container within this window. Nothing else can: the row
    /// is seconds old and has never been returned to a caller, so no attachment, share or
    /// meeting row can name it yet. The guard is a backstop against the call site being
    /// reused later, NOT a complete referential check; a future caller outside the
    /// create-then-seal window must extend it rather than assume it. Deleting a
    /// container that anything still points at would orphan those rows out from under
    /// `visibility_clause`, which resolves their visibility through the folder — turning a tidy-up
    /// into exactly the unreachable-content failure the rest of this change exists to prevent. A
    /// referenced row is left alone and the caller is told, because refusing to delete is always the
    /// safe direction.
    pub(crate) fn delete_freshly_created_folder(&self, id: &str) -> Result<()> {
        let conn = self.lock();
        let removed = conn
            .execute(
                "DELETE FROM folders
                  WHERE id = ?1
                    AND NOT EXISTS (SELECT 1 FROM folders WHERE parent_id = ?1)
                    AND NOT EXISTS (SELECT 1 FROM notes WHERE folder_id = ?1)
                    AND NOT EXISTS (SELECT 1 FROM meetings WHERE folder_id = ?1)
                    AND NOT EXISTS (SELECT 1 FROM documents WHERE folder_id = ?1)",
                rusqlite::params![id],
            )
            .map_err(map_err)?;
        if removed == 0 {
            return Err(AppError::Storage(
                "refusing to remove a container that something still references".into(),
            ));
        }
        Ok(())
    }

    /// Is any container at `path`, or beneath it, sealed?
    ///
    /// By PATH PREFIX, deliberately — the same rule the re-parent's own rewrite uses
    /// (`reparent_note_folder_paths` matches `path LIKE '<old>/%'`). A guard that
    /// enumerated by parent link instead would disagree with the operation it guards on
    /// exactly the rows this step exists to repair: every note container a shipped
    /// build created has a correct path and a NULL parent link, so a locked descendant
    /// would be invisible to the guard and still moved by the rewrite.
    pub(crate) fn subtree_has_sealed_container(&self, path: &str) -> Result<bool> {
        let conn = self.lock();
        let like = format!(
            "{}/%",
            path.replace('!', "!!").replace('%', "!%").replace('_', "!_")
        );
        conn.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM folders
                  WHERE locked = 1 AND (path = ?1 OR path LIKE ?2 ESCAPE '!'))",
            rusqlite::params![path, like],
            |r| Ok(r.get::<_, i64>(0)? != 0),
        )
        .map_err(map_err)
    }

    /// The container a creation with NO explicit parent belongs to: the project at the vault root.
    ///
    /// Before the hierarchy, a container created with no parent was a root and the sidebar showed
    /// it. The tree now renders from the projects down, so such a container has no place in it and
    /// simply does not appear — which is why every creation path resolves this instead of leaving a
    /// NULL parent. Because the project's path IS the vault root, composing a child path from it
    /// yields exactly the path that container gets today.
    ///
    /// `None` only on a database the adoption declined (a machine-owned container occupies the vault
    /// root).
    ///
    /// Nothing constrains the table to ONE project at the vault root — `path` is unique, so a second
    /// one cannot exist today, but that is a property of a column rather than a stated invariant of
    /// this predicate. The ordering makes the answer deterministic regardless: the oldest row wins,
    /// so every caller and every later launch resolve the same project even if the shape ever
    /// changes.
    ///
    /// Callers do NOT share one response to that, and the difference is deliberate.
    /// `require_workspace_project` (the two creates) refuses, because a container created without a
    /// project is invisible in a tree rendered from the projects down — and so is everything the
    /// user then puts in it. `ensure_notes_root` passes the `None` through instead, because the
    /// reserved note root must exist either way: refusing there would make an un-adopted database
    /// unable to hold an unfiled note at all, which is worse than a root that is momentarily
    /// unparented and gets adopted by the next migration run.
    pub fn workspace_project_id(&self) -> Result<Option<String>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT id FROM folders
              WHERE COALESCE(level, 'folder') = 'project'
                AND path = ''
                AND COALESCE(kind, 'meeting') IN ('meeting', 'note')
              ORDER BY created_at ASC, id ASC
              LIMIT 1",
            [],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .map_err(map_err)
    }

    /// Every folder's hierarchy LEVEL (`"project"` or `"folder"`), keyed by id.
    ///
    /// The twin of [`Db::folder_kinds`], and used the same way: `build_folder_tree` folds it in
    /// rather than widening the `Folder` struct, which would force every struct-literal construction
    /// in the crate (and every test fixture) to change for a field only the tree needs.
    pub fn folder_levels(&self) -> Result<std::collections::HashMap<String, String>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT id, COALESCE(level, 'folder') FROM folders")
            .map_err(map_err)?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(map_err)?;
        let mut out = std::collections::HashMap::new();
        for r in rows {
            let (id, level) = r.map_err(map_err)?;
            out.insert(id, level);
        }
        Ok(out)
    }

    pub fn folder_by_id(&self, id: &str) -> Result<Option<Folder>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT id, name, path, parent_id, locked, created_at FROM folders WHERE id = ?1",
            rusqlite::params![id],
            row_to_folder,
        )
        .optional()
        .map_err(map_err)
    }

    /// Look up a folder by its vault-relative `path` (the `path` column is `NOT NULL UNIQUE`). Used
    /// by the auto-organize seam to map a classifier-chosen subfolder name back to its folder row —
    /// so a note auto-filed into a LOCKED folder's on-disk dir is sealed/rejected (it would
    /// otherwise land plaintext with `folder_id = NULL`, which `lock_folder` + the at-rest reconcile
    /// both key off `folder_id` and miss).
    pub fn folder_by_path(&self, path: &str) -> Result<Option<Folder>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT id, name, path, parent_id, locked, created_at FROM folders WHERE path = ?1",
            rusqlite::params![path],
            row_to_folder,
        )
        .optional()
        .map_err(map_err)
    }

    /// Whether a user-visible vault directory already occupies `path` under macOS-style
    /// case-insensitive matching. `folders.path` keeps its exact UNIQUE constraint as the final
    /// same-spelling race guard; Space creation additionally holds the lifecycle mutex while this
    /// Unicode-lowercase scan and insert run, preventing `Work`/`work` siblings in one process.
    pub(crate) fn user_folder_path_exists_case_insensitive(&self, path: &str) -> Result<bool> {
        let conn = self.lock();
        let mut statement = conn
            .prepare("SELECT path FROM folders WHERE id <> ?1")
            .map_err(map_err)?;
        let rows = statement
            .query_map(
                rusqlite::params![crate::storage::tasks_store::TASK_FOLDER_ID],
                |row| row.get::<_, String>(0),
            )
            .map_err(map_err)?;
        let needle = path.to_lowercase();
        for row in rows {
            if row.map_err(map_err)?.to_lowercase() == needle {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Set a folder's `locked` flag + its KEK-wrapped content key (`Some` when sealing,
    /// `None` to clear on permanent remove-lock).
    pub fn set_folder_locked(
        &self,
        id: &str,
        locked: bool,
        wrapped_key: Option<&[u8]>,
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE folders SET locked = ?2, wrapped_key = ?3 WHERE id = ?1",
            rusqlite::params![id, locked as i64, wrapped_key],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// The KEK-wrapped content key for a sealed folder (`None` if the column is NULL).
    pub fn folder_wrapped_key(&self, id: &str) -> Result<Option<Vec<u8>>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT wrapped_key FROM folders WHERE id = ?1",
            rusqlite::params![id],
            |r| r.get::<_, Option<Vec<u8>>>(0),
        )
        .optional()
        .map_err(map_err)
        .map(Option::flatten)
    }

    /// Does ANY sealed folder exist? Drives the master-KEK mint guard: while sealed content exists,
    /// a missing KEK keychain item must NEVER be silently replaced by a freshly-minted one (the
    /// fresh KEK cannot unwrap the existing folders' content keys — 2026-07-05 field incident).
    pub fn any_locked_folder(&self) -> Result<bool> {
        let conn = self.lock();
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM folders WHERE locked = 1)",
            [],
            |r| r.get::<_, bool>(0),
        )
        .map_err(map_err)
    }

    /// Direct CHILD folders of `parent_id` (one level only — not transitive). Used by
    /// `rename_folder`/`delete_folder` to walk the subtree so a rename can re-prefix descendant
    /// paths and a delete can refuse a non-empty tree.
    pub fn child_folders(&self, parent_id: &str) -> Result<Vec<Folder>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, name, path, parent_id, locked, created_at
                   FROM folders WHERE parent_id = ?1 ORDER BY created_at, name",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![parent_id], row_to_folder)
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// Rename a folder's display `name` AND its vault-relative `path` in one statement. The new
    /// `path` is composed by the caller (parent path + sanitized name), so this is a pure column
    /// update — it does NOT touch the on-disk vault dir or any note's `exported_path` (those are the
    /// caller's responsibility, sequenced so a crash can never lose content). Leaves `locked` /
    /// `wrapped_key` untouched: a locked-folder rename is metadata-only and never reaches sealed
    /// content.
    pub fn rename_folder(&self, id: &str, new_name: &str, new_path: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE folders SET name = ?2, path = ?3 WHERE id = ?1",
            rusqlite::params![id, new_name, new_path],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Assign (or clear) a meeting's canonical `meetings.folder_id`, synchronizing EVERY provider
    /// note row (`WHERE meeting_id = ?1`) so legacy note-level readers and the seal lifecycle stay
    /// coherent. `None` clears the canonical placement (move to the unfiled inbox).
    /// A locked target additionally refuses any nonterminal generation or pending legacy recovery
    /// marker in the SAME transaction, so no caller can associate unmanaged plaintext audio behind
    /// a folder lock even if it bypasses the command-side recovery seam.
    pub fn set_meeting_folder(&self, meeting_id: &str, folder_id: Option<&str>) -> Result<()> {
        self.set_meeting_folder_with_restore_progress(meeting_id, folder_id, None)
    }

    /// Folder-trash restore twin of [`Db::set_meeting_folder`]. The placement and its durable
    /// per-member progress witness commit in ONE transaction, so a crash can never publish the
    /// restored placement without also teaching a retry not to overwrite a later user move.
    pub(crate) fn set_meeting_folder_for_trash_restore(
        &self,
        entry_id: &str,
        meeting_id: &str,
        folder_id: &str,
    ) -> Result<()> {
        self.set_meeting_folder_with_restore_progress(
            meeting_id,
            Some(folder_id),
            Some(entry_id),
        )
    }

    fn set_meeting_folder_with_restore_progress(
        &self,
        meeting_id: &str,
        folder_id: Option<&str>,
        restore_entry_id: Option<&str>,
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let blocked: bool = match folder_id {
            Some(folder_id) => tx
                .query_row(
                    "SELECT EXISTS(
                         SELECT 1
                           FROM folders f
                          WHERE f.id=?2 AND f.locked=1 AND (
                                EXISTS(
                                    SELECT 1 FROM recording_generations rg
                                     WHERE rg.meeting_id=?1 AND rg.state!='RETIRED'
                                ) OR EXISTS(
                                    SELECT 1 FROM legacy_recording_recovery lr
                                     WHERE lr.meeting_id=?1
                                )
                          )
                     )",
                    rusqlite::params![meeting_id, folder_id],
                    |row| row.get(0),
                )
                .map_err(map_err)?,
            None => false,
        };
        if blocked {
            return Err(AppError::Locked(
                "meeting has plaintext recording artifacts that are not governed by the locked folder"
                    .into(),
            ));
        }
        let changed = tx.execute(
            "UPDATE meetings SET folder_id = ?2 WHERE id = ?1",
            rusqlite::params![meeting_id, folder_id],
        )
        .map_err(map_err)?;
        if changed != 1 {
            return Err(AppError::InvalidArg(format!("no meeting {meeting_id}")));
        }
        tx.execute(
            "UPDATE notes SET folder_id = ?2 WHERE meeting_id = ?1",
            rusqlite::params![meeting_id, folder_id],
        )
        .map_err(map_err)?;
        if let Some(entry_id) = restore_entry_id {
            let progress = tx
                .execute(
                    "INSERT INTO trash_folder_restore_members(entry_id,member_kind,member_id)
                     SELECT ?1,'meeting',?2
                      WHERE EXISTS(
                        SELECT 1 FROM trash_items
                         WHERE id=?1 AND kind IN ('folder','noteFolder')
                      )",
                    rusqlite::params![entry_id, meeting_id],
                )
                .map_err(map_err)?;
            if progress != 1 {
                return Err(AppError::Storage(
                    "folder restore lost its recovery journal before recording meeting progress"
                        .into(),
                ));
            }
        }
        tx.commit().map_err(map_err)
    }

    /// Back-compat alias for [`Db::set_meeting_folder`]; canonical placement is meeting-owned.
    pub fn set_note_folder(&self, meeting_id: &str, folder_id: Option<&str>) -> Result<()> {
        self.set_meeting_folder(meeting_id, folder_id)
    }

    /// Folder ids that are sealed (`locked=1`) — used to re-blank every sealed note on relock-all.
    pub fn locked_folder_ids(&self) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT id FROM folders WHERE locked = 1")
            .map_err(map_err)?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// Distinct meeting ids canonically filed in `folder_id`, plus legacy note-owned rows that have
    /// not yet been assigned canonically. This COMPLETE enumeration is the lock boundary for a
    /// pre-note recording's transcript/timeline/manual-notes/audio.
    pub fn meeting_ids_in_folder(&self, folder_id: &str) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id FROM meetings WHERE folder_id = ?1
                 UNION
                 SELECT n.meeting_id FROM notes n
                  JOIN meetings m ON m.id = n.meeting_id
                  WHERE n.folder_id = ?1 AND m.folder_id IS NULL",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![folder_id], |r| r.get::<_, String>(0))
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// Every folder that governs a meeting's visibility.
    ///
    /// New rows have one canonical `meetings.folder_id`. The note-derived branch is retained only
    /// for legacy rows that could not be canonicalized safely. Returning *all* of those legacy
    /// folders lets the command-layer gate require every governing folder to be readable instead
    /// of accidentally choosing an unlocked sibling beside a locked one.
    pub fn folders_for_meeting(&self, meeting_id: &str) -> Result<Vec<String>> {
        let conn = self.lock();
        let canonical = conn
            .query_row(
            "SELECT folder_id FROM meetings WHERE id = ?1",
            rusqlite::params![meeting_id],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()
        .map_err(map_err)?
        .flatten();
        if let Some(folder_id) = canonical {
            return Ok(vec![folder_id]);
        }

        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT folder_id FROM notes
                  WHERE meeting_id = ?1 AND folder_id IS NOT NULL
                  ORDER BY folder_id",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![meeting_id], |r| r.get::<_, String>(0))
            .map_err(map_err)?;
        let mut folders = Vec::new();
        for row in rows {
            folders.push(row.map_err(map_err)?);
        }
        Ok(folders)
    }

    /// The single owning folder id for a meeting, or `None` when unfiled.
    ///
    /// Ambiguous legacy ownership is deliberately not collapsed to an arbitrary folder. Read gates
    /// use [`Self::folders_for_meeting`] and require every governing folder to be visible; callers
    /// that need one mutation target must first resolve the ambiguity explicitly.
    pub fn folder_for_meeting(&self, meeting_id: &str) -> Result<Option<String>> {
        let folders = self.folders_for_meeting(meeting_id)?;
        match folders.as_slice() {
            [] => Ok(None),
            [folder_id] => Ok(Some(folder_id.clone())),
            _ => Err(crate::error::AppError::Locked(
                "legacy meeting belongs to multiple folders".into(),
            )),
        }
    }
}

/// Maps `(id, name, path, parent_id, locked, created_at)` → `Folder`.
fn row_to_folder(row: &Row<'_>) -> rusqlite::Result<Folder> {
    Ok(Folder {
        id: row.get(0)?,
        name: row.get(1)?,
        path: row.get(2)?,
        parent_id: row.get(3)?,
        locked: row.get::<_, i64>(4)? != 0,
        created_at: row.get(5)?,
    })
}

/// Column order: `id, name, path, parent_id, locked, kind`.
fn row_to_note_folder(row: &Row<'_>) -> rusqlite::Result<NoteFolder> {
    Ok(NoteFolder {
        id: row.get(0)?,
        name: row.get(1)?,
        path: row.get(2)?,
        parent_id: row.get(3)?,
        locked: row.get::<_, i64>(4)? != 0,
        // DB-only view: the session-unlock state is not a column. The `list_note_folders` command
        // fills this in from the live session set; every other reader gets `false` (safe default).
        unlocked: false,
        kind: row
            .get::<_, Option<String>>(5)?
            .unwrap_or_else(|| "note".into()),
        is_root: row.get::<_, i64>(6)? != 0,
    })
}
