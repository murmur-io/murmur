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
    pub fn note_folder_by_id(&self, id: &str) -> Result<Option<NoteFolder>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT id, name, path, parent_id, locked, kind, COALESCE(is_root, 0)
               FROM folders WHERE id = ?1 AND kind = 'note'",
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

    /// Assign (or clear) a MEETING's folder. A meeting's folder = its note's folder, so this
    /// updates `folder_id` on EVERY provider row of the meeting (`WHERE meeting_id = ?1`) — the
    /// note moves as a unit and the seal/unlock lifecycle (which iterates provider rows) stays
    /// coherent (no row left in a stale folder). `None` clears the folder (move to vault root).
    /// A locked target additionally refuses any nonterminal generation or pending legacy recovery
    /// marker in the SAME transaction, so no caller can associate unmanaged plaintext audio behind
    /// a folder lock even if it bypasses the command-side recovery seam.
    pub fn set_meeting_folder(&self, meeting_id: &str, folder_id: Option<&str>) -> Result<()> {
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
        tx.execute(
            "UPDATE notes SET folder_id = ?2 WHERE meeting_id = ?1",
            rusqlite::params![meeting_id, folder_id],
        )
        .map_err(map_err)?;
        tx.commit().map_err(map_err)
    }

    /// Back-compat alias for [`Db::set_meeting_folder`] — a note's folder is the meeting's folder.
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

    /// Distinct meeting ids whose notes live in `folder_id` (the meetings governed by the
    /// folder's lock). Used to seal/unseal each meeting's transcript + timeline.
    pub fn meeting_ids_in_folder(&self, folder_id: &str) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT DISTINCT meeting_id FROM notes WHERE folder_id = ?1")
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

    /// The owning folder id for a meeting (its notes' `folder_id`), or `None` at the vault root.
    /// Drives the read-gate predicate `meeting_is_unlocked`.
    pub fn folder_for_meeting(&self, meeting_id: &str) -> Result<Option<String>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT folder_id FROM notes
              WHERE meeting_id = ?1 AND folder_id IS NOT NULL LIMIT 1",
            rusqlite::params![meeting_id],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()
        .map_err(map_err)
        .map(Option::flatten)
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
