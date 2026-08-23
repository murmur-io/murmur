//! Note storage surface — the authored-note (`documents(kind='note')`) CRUD + the per-provider
//! `notes`-table records + the note-root (`is_root`) bootstrap, originally extracted from
//! `storage::db` during the God-file split. The methods below are an inherent-impl split of
//! [`crate::storage::db::Db`] across files (Rust allows one type's inherent `impl` to live in
//! multiple files of the same crate). The VISIBILITY-GATED note readers (`list_notes_visible`,
//! `list_notes_visible_typed`, `note_markdown_if_visible`, `latest_note_visible`,
//! `get_note_if_visible`, `note_is_visible`) route through `visibility_clause` EXACTLY as on trunk —
//! a sealed-and-not-session-unlocked note stays invisible/`None`. The SEAL-ON-WRITE twins
//! (`insert_note_sealed`, `update_note_row_sealed`, `move_note_row_sealed`, `upsert_note_sealed`)
//! preserve the CALLER-side verify-before-destroy contract. `upsert_note_sealed` additionally keeps
//! its folder assignment behind the recording-generation lock backstop in the same transaction.
//! The seal-verify callers (`lock_folder`/`reseal_document_if_locked`) stay in `commands.rs`.
//! Shared db.rs module-level
//! helpers `map_err` / `visibility_clause` / `parse_front_matter` / `note_snippet` /
//! `coerce_property_value` are `pub(crate)`/`pub`; the `NoteRow` DTO stays in db.rs; the note row
//! mappers `row_to_note` / `row_to_note_row` (only ever used by these readers) moved along and stay
//! private to this module. The note-folder/document CRUD (`folder_by_path`, `get_note_folder_schema`,
//! `notes_with_active_share`) stays in its own module and is reached cross-file via `self.`.

use std::collections::HashSet;

use rusqlite::{OptionalExtension, Row, Transaction};

use crate::error::{AppError, Result};
use crate::storage::db::{
    coerce_property_value, map_err, note_snippet, parse_front_matter, visibility_clause, Db,
    NoteRow,
};
use crate::storage::models::{NoteRecord, NoteSummary, PropertyValue, TypedNoteRow};

impl Db {
    /// Insert an authored note row (`documents` with `kind='note'`). Separate from
    /// [`Db::insert_document`] only to persist the authoring columns (`title`/`updated_at`);
    /// `text_blob`/`exported_path` start NULL. The COMMAND layer gates the folder first.
    pub fn insert_note(
        &self,
        id: &str,
        folder_id: &str,
        name: &str,
        title: &str,
        text: &str,
        created_at: i64,
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO documents
               (id, folder_id, name, title, text, kind, text_blob, created_at, updated_at, exported_path)
             VALUES (?1, ?2, ?3, ?4, ?5, 'note', NULL, ?6, ?6, NULL)",
            rusqlite::params![id, folder_id, name, title, text, created_at],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Atomically birth a generated companion note in the always-open Notes root. The authored
    /// document body, structured `meeting_id`, and required companion edge either commit together
    /// or leave no rows. The command layer owns the lifecycle/root gate and attachment validation.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_companion_note_atomic(
        &self,
        id: &str,
        folder_id: &str,
        name: &str,
        title: &str,
        text: &str,
        meeting_id: &str,
        created_at: i64,
    ) -> Result<()> {
        self.insert_companion_note_atomic_core(
            id,
            folder_id,
            name,
            title,
            text,
            meeting_id,
            created_at,
            |_| Ok(()),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_companion_note_atomic_core(
        &self,
        id: &str,
        folder_id: &str,
        name: &str,
        title: &str,
        text: &str,
        meeting_id: &str,
        created_at: i64,
        mut checkpoint: impl FnMut(bool) -> Result<()>,
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let root_is_open = tx
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM folders
                     WHERE id = ?1 AND is_root = 1 AND locked = 0
                       AND COALESCE(kind, 'meeting') = 'note'
                 )",
                [folder_id],
                |row| row.get::<_, bool>(0),
            )
            .map_err(map_err)?;
        if !root_is_open {
            return Err(AppError::Locked(
                "companion notes can only be created in the open Notes root".into(),
            ));
        }
        let meeting_exists = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM meetings WHERE id = ?1)",
                [meeting_id],
                |row| row.get::<_, bool>(0),
            )
            .map_err(map_err)?;
        if !meeting_exists {
            return Err(AppError::InvalidArg(format!("no meeting {meeting_id}")));
        }
        tx.execute(
            "INSERT INTO documents
               (id, folder_id, name, title, text, kind, text_blob, created_at, updated_at,
                exported_path, meeting_id)
             VALUES (?1, ?2, ?3, ?4, ?5, 'note', NULL, ?6, ?6, NULL, ?7)",
            rusqlite::params![id, folder_id, name, title, text, created_at, meeting_id],
        )
        .map_err(map_err)?;
        checkpoint(false)?;
        Self::upsert_link_tx(
            &tx,
            "note",
            id,
            "meeting",
            meeting_id,
            "companion",
            1.0,
            "user",
            "active",
            created_at,
        )?;
        checkpoint(true)?;
        tx.commit().map_err(map_err)
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn insert_companion_note_atomic_failing_at(
        &self,
        id: &str,
        folder_id: &str,
        name: &str,
        title: &str,
        text: &str,
        meeting_id: &str,
        created_at: i64,
        fail_after_link: bool,
    ) -> Result<()> {
        self.insert_companion_note_atomic_core(
            id,
            folder_id,
            name,
            title,
            text,
            meeting_id,
            created_at,
            |after_link| {
                if after_link == fail_after_link {
                    Err(AppError::Storage("injected companion birth failure".into()))
                } else {
                    Ok(())
                }
            },
        )
    }

    /// BIRTH-SEAL twin of [`Db::insert_note`] for a session-unlocked LOCKED folder (2026-07-10
    /// residual W3): insert the row WITH its freshly-encrypted `text_blob` in ONE atomic INSERT, so
    /// there is never a blob-less plaintext row in a locked folder — not even transiently between an
    /// insert and a follow-up seal (the pre-fix shape, where a failed birth-seal left the plaintext
    /// row lingering). The CALLER must have verified the blob decrypts back byte-identical BEFORE
    /// calling this (verify-before-destroy), exactly like [`Db::update_note_row_sealed`].
    #[allow(clippy::too_many_arguments)]
    pub fn insert_note_sealed(
        &self,
        id: &str,
        folder_id: &str,
        name: &str,
        title: &str,
        text: &str,
        text_blob: &[u8],
        created_at: i64,
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO documents
               (id, folder_id, name, title, text, kind, text_blob, created_at, updated_at, exported_path)
             VALUES (?1, ?2, ?3, ?4, ?5, 'note', ?6, ?7, ?7, NULL)",
            rusqlite::params![id, folder_id, name, title, text, text_blob, created_at],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Read ONE note's raw row (every column the DTO builders need), or `None` if the id is unknown
    /// OR the row is not `kind='note'`. The COMMAND layer gates the folder before surfacing the text.
    pub fn get_note_row(&self, id: &str) -> Result<Option<NoteRow>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT id, folder_id, name, title, COALESCE(text, ''), created_at, updated_at,
                    exported_path, (text_blob IS NOT NULL)
               FROM documents WHERE id = ?1 AND kind = 'note'",
            rusqlite::params![id],
            row_to_note_row,
        )
        .optional()
        .map_err(map_err)
    }

    /// Content-free authorization anchor for one authored note. Commands use this BEFORE reading
    /// [`NoteRow`] so a locked row's title/body/export path never enters the process merely to find
    /// its governing folder. Timestamps are safe identity metadata used by the masked editor DTO.
    pub fn note_gate_anchor(&self, id: &str) -> Result<Option<(String, i64, Option<i64>)>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT folder_id, created_at, updated_at
               FROM documents WHERE id = ?1 AND kind = 'note'",
            rusqlite::params![id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(map_err)
    }

    /// Recording-time COMPANION NOTE — set the STRUCTURED `documents.meeting_id` link on a
    /// standalone note (`kind='note'`). Idempotent on an unknown id / non-note row (0 rows). The
    /// column is a non-content id (rides the SQLCipher-at-rest layer, never sealed/blanked). Callers
    /// set this immediately after `create_note_inner` births the companion note.
    pub fn set_document_meeting_id(&self, id: &str, meeting_id: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE documents SET meeting_id = ?2 WHERE id = ?1 AND kind = 'note'",
            rusqlite::params![id, meeting_id],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Recording-time COMPANION NOTE — the note id of the ONE companion note (`kind='note'`) linked
    /// to `meeting_id`, or `None` when none exists yet (the append command then lazily creates it).
    /// Structured lookup by the indexed `meeting_id` column — never a fragile title-string match.
    /// One-note-per-meeting is a model invariant (the append command is the only writer); should a
    /// duplicate ever exist, the newest-updated row wins (deterministic).
    pub fn companion_note_for_meeting(&self, meeting_id: &str) -> Result<Option<String>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT id FROM documents
               WHERE kind = 'note' AND meeting_id = ?1
               ORDER BY COALESCE(updated_at, created_at) DESC, id ASC
               LIMIT 1",
            rusqlite::params![meeting_id],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .map_err(map_err)
    }

    /// Update an authored note's `title` + `text` + `updated_at` (write path, OPEN folders only).
    /// Leaves `text_blob` alone. A write into a session-unlocked LOCKED folder must NOT come here —
    /// the relock reblank discards the plaintext and restores the stale blob (content loss) — it goes
    /// through the command layer's `reseal_document_if_locked` → [`Db::update_note_row_sealed`],
    /// which re-seals the fresh text into `text_blob` in the same write (2026-07-10 audit F1).
    /// Idempotent on an unknown id / non-note row (0 rows affected).
    pub fn update_note_row(
        &self,
        id: &str,
        title: &str,
        text: &str,
        updated_at: i64,
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let changed = tx.execute(
            "UPDATE documents SET title = ?2, text = ?3, updated_at = ?4
               WHERE id = ?1 AND kind = 'note'",
            rusqlite::params![id, title, text, updated_at],
        )
        .map_err(map_err)?;
        if changed != 0 {
            tx.execute(
            "UPDATE org_shares SET republish_dirty = republish_dirty + 1, republish_deferred=0
              WHERE document_id = ?1 AND state IN ('queued','uploaded','failed')",
            rusqlite::params![id],
            )
            .map_err(map_err)?;
        }
        tx.commit().map_err(map_err)
    }

    pub fn update_note_row_debounced(
        &self,
        id: &str,
        title: &str,
        text: &str,
        updated_at: i64,
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        tx.execute(
            "UPDATE documents SET title = ?2, text = ?3, updated_at = ?4
               WHERE id = ?1 AND kind = 'note'",
            rusqlite::params![id, title, text, updated_at],
        )
        .map_err(map_err)?;
        tx.execute(
            "UPDATE org_shares SET republish_deferred=1
              WHERE document_id=?1 AND state IN ('queued','uploaded','failed')",
            rusqlite::params![id],
        ).map_err(map_err)?;
        tx.commit().map_err(map_err)
    }

    /// Background auto-title CAS. It changes ONLY `title` + `updated_at`, and only while the exact
    /// body revision observed before inference is still in the same OPEN, unsealed folder and its
    /// title remains blank/`Untitled`. The single SQL statement closes editor-save and folder-seal
    /// TOCTOU windows; unlike [`Db::update_note_row`], it can never copy a stale plaintext body back
    /// over a freshly blanked sealed row.
    pub fn set_auto_title_if_unchanged_and_open(
        &self,
        id: &str,
        expected_folder_id: &str,
        expected_updated_at: Option<i64>,
        expected_text: &str,
        title: &str,
        updated_at: i64,
    ) -> Result<bool> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let changed = tx
            .execute(
                "UPDATE documents
                    SET title = ?5, updated_at = ?6
                  WHERE id = ?1
                    AND kind = 'note'
                    AND folder_id = ?2
                    AND updated_at IS ?3
                    AND text = ?4
                    AND text_blob IS NULL
                    AND COALESCE(TRIM(title), '') IN ('', 'Untitled')
                    AND EXISTS (
                        SELECT 1 FROM folders f
                         WHERE f.id = documents.folder_id AND f.locked = 0
                    )",
                rusqlite::params![
                    id,
                    expected_folder_id,
                    expected_updated_at,
                    expected_text,
                    title,
                    updated_at
                ],
            )
            .map_err(map_err)?;
        if changed != 0 {
            tx.execute(
                "UPDATE org_shares SET republish_dirty = republish_dirty + 1
                  WHERE document_id = ?1 AND state IN ('queued','uploaded','failed')",
                rusqlite::params![id],
            )
            .map_err(map_err)?;
        }
        tx.commit().map_err(map_err)?;
        Ok(changed == 1)
    }

    /// SEAL-ON-WRITE twin of [`Db::update_note_row`] for a session-unlocked LOCKED folder: persist
    /// the fresh plaintext (session-visible) AND its freshly-encrypted `text_blob` in ONE atomic
    /// statement, so the at-rest seal always matches the newest text (relock re-blanks the plaintext
    /// and the next unlock restores THIS write, never a stale lock-time copy). The CALLER must have
    /// verified the blob decrypts back byte-identical BEFORE calling this (verify-before-destroy) —
    /// exactly like [`Db::seal_document`]. Idempotent on an unknown id / non-note row.
    pub fn update_note_row_sealed(
        &self,
        id: &str,
        title: &str,
        text: &str,
        text_blob: &[u8],
        updated_at: i64,
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let changed = tx.execute(
            "UPDATE documents SET title = ?2, text = ?3, text_blob = ?4, updated_at = ?5
               WHERE id = ?1 AND kind = 'note'",
            rusqlite::params![id, title, text, text_blob, updated_at],
        )
        .map_err(map_err)?;
        if changed != 0 {
            tx.execute(
            "UPDATE org_shares SET republish_dirty = republish_dirty + 1, republish_deferred=0
              WHERE document_id = ?1 AND state IN ('queued','uploaded','failed')",
            rusqlite::params![id],
            )
            .map_err(map_err)?;
        }
        tx.commit().map_err(map_err)
    }

    pub fn update_note_row_sealed_debounced(
        &self,
        id: &str,
        title: &str,
        text: &str,
        text_blob: &[u8],
        updated_at: i64,
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        tx.execute(
            "UPDATE documents SET title = ?2, text = ?3, text_blob = ?4, updated_at = ?5
               WHERE id = ?1 AND kind = 'note'",
            rusqlite::params![id, title, text, text_blob, updated_at],
        )
        .map_err(map_err)?;
        tx.execute(
            "UPDATE org_shares SET republish_deferred=1
              WHERE document_id=?1 AND state IN ('queued','uploaded','failed')",
            rusqlite::params![id],
        ).map_err(map_err)?;
        tx.commit().map_err(map_err)
    }

    /// MOVE-INTO-LOCKED twin of [`Db::update_note_row_sealed`] (2026-07-10 residual W2): reassign an
    /// authored note to a (session-unlocked, LOCKED) target folder AND write its fresh `text_blob`
    /// (sealed under the TARGET folder's CK) in ONE atomic UPDATE — never a reassign-then-seal
    /// two-step, whose failure window left the note sitting in the locked target with a stale
    /// wrong-CK blob (undecryptable at the target's next unlock). `exported_path` is NULLed in the
    /// same statement (a note governed by a locked folder has no on-disk export). The CALLER must
    /// have verified the blob decrypts back byte-identical BEFORE calling this
    /// (verify-before-destroy). Idempotent on an unknown id / non-note row.
    pub fn move_note_row_sealed(
        &self,
        id: &str,
        folder_id: &str,
        title: &str,
        text: &str,
        text_blob: &[u8],
        updated_at: i64,
    ) -> Result<()> {
        let conn = self.lock();
        // `exported_hash` is NULLed with `exported_path` (path-coupled collision-guard baseline —
        // a note sealed into the locked target has no on-disk export to compare against).
        conn.execute(
            "UPDATE documents SET folder_id = ?2, title = ?3, text = ?4, text_blob = ?5,
                    updated_at = ?6, exported_path = NULL, exported_hash = NULL
               WHERE id = ?1 AND kind = 'note'",
            rusqlite::params![id, folder_id, title, text, text_blob, updated_at],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Persist (or clear with `None`) an authored NOTE's exported vault `.md` path. Set on
    /// export/unlock re-export; cleared (NULL) when the folder seals and the vault file is deleted.
    /// Clearing the path ALSO clears the path-coupled `exported_hash` baseline in the same
    /// statement (the export-collision guard has no file to compare once the `.md` is gone; the
    /// next export re-stamps both fresh). Setting a path leaves the hash to the caller's explicit
    /// stamp (`write_note_to_vault`). Named `_doc` to disambiguate from the MEETING-note
    /// [`Db::set_note_exported_path`].
    pub fn set_note_doc_exported_path(&self, id: &str, path: Option<&str>) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE documents
                SET exported_path = ?2,
                    exported_hash = CASE WHEN ?2 IS NULL THEN NULL ELSE exported_hash END
              WHERE id = ?1 AND kind = 'note'",
            rusqlite::params![id, path],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// The export-collision-guard baseline for an AUTHORED note (`documents(kind='note')`): the
    /// SHA-256 (lowercase hex) of the text Murmur last wrote to its exported vault `.md`. `None`
    /// for a legacy/never-exported row (grandfathered). Twin of [`Db::get_note_exported_hash`].
    pub fn get_note_doc_exported_hash(&self, id: &str) -> Result<Option<String>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT exported_hash FROM documents WHERE id = ?1 AND kind = 'note'",
            rusqlite::params![id],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()
        .map_err(map_err)
        .map(Option::flatten)
    }

    /// Persist (or clear with `None`) an authored NOTE's export-collision-guard baseline —
    /// refreshed on every vault (re)export. Twin of [`Db::set_note_exported_hash`].
    pub fn set_note_doc_exported_hash(&self, id: &str, hash: Option<&str>) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE documents SET exported_hash = ?2 WHERE id = ?1 AND kind = 'note'",
            rusqlite::params![id, hash],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// GATED list of note summaries. `folder_id = Some(fid)` scopes to one note-folder; `None` lists
    /// every VISIBLE note across all note-folders. The `visibility_clause` is applied IN THE QUERY
    /// (never a per-row skip) so a sealed-and-not-session-unlocked note's row is EXCLUDED entirely —
    /// its title/topic never leaks. Newest-updated first. Snippet/tags are derived from the (visible)
    /// plaintext markdown; a locked note is simply absent here, so no masking is needed at this layer.
    pub fn list_notes_visible(
        &self,
        folder_id: Option<&str>,
        unlocked: &HashSet<String>,
    ) -> Result<Vec<NoteSummary>> {
        // WP6 — the set of notes with an ACTIVE outbound share (one query; drives `shared`). Computed
        // BEFORE the main lock (it re-locks internally) so the connection guard is held only once.
        let shared_set = self.notes_with_active_share()?;
        let conn = self.lock();
        let visible = visibility_clause("f", unlocked);
        let folder_pred = if folder_id.is_some() {
            " AND d.folder_id = ?1"
        } else {
            ""
        };
        let sql = format!(
            "SELECT d.id, d.folder_id, d.name, d.title, COALESCE(d.text, ''),
                    d.created_at, d.updated_at
               FROM documents d
               JOIN folders f ON f.id = d.folder_id
              WHERE d.kind = 'note' AND {visible}{folder_pred}
              ORDER BY COALESCE(d.updated_at, d.created_at) DESC, d.id ASC"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let map_row = |r: &Row<'_>| -> rusqlite::Result<NoteSummary> {
            let id: String = r.get(0)?;
            let folder_id: String = r.get(1)?;
            let name: String = r.get(2)?;
            let title: Option<String> = r.get(3)?;
            let text: String = r.get(4)?;
            let created_at: i64 = r.get(5)?;
            let updated_at: Option<i64> = r.get(6)?;
            let (tags, _props) = parse_front_matter(&text);
            let shared = shared_set.contains(&id);
            Ok(NoteSummary {
                id,
                title: title.filter(|t| !t.is_empty()).unwrap_or(name),
                folder_id,
                snippet: note_snippet(&text),
                tags,
                updated_at: updated_at.unwrap_or(created_at),
                created_at,
                locked: false, // a visible note is unlocked by construction (gated in the query).
                shared,
            })
        };
        let rows = if let Some(fid) = folder_id {
            stmt.query_map(rusqlite::params![fid], map_row)
        } else {
            stmt.query_map([], map_row)
        }
        .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// List a note-folder's VISIBLE notes projected through its typed schema (Feature C — the
    /// Table/Board substrate). GATE: the visible rows come from the EXISTING gated
    /// [`Self::list_notes_visible`] (`visibility_clause` against `unlocked`), so a sealed-and-not-
    /// session-unlocked folder yields NO rows here (never a masked row) — a typed row can never carry
    /// sealed content. Per visible row: re-read its markdown through the SAME visibility gate
    /// (defense-in-depth — a row that somehow slipped the summary gate is still gated on the text
    /// read), `parse_front_matter` the raw `Record<String,String>`, and coerce each schema key's raw
    /// scalar via [`coerce_property_value`] against the folder schema. A `Select` value outside the
    /// declared `options` is PRESERVED as `Text` (never dropped). The front-matter parsers are
    /// untouched — typing is a pure read-time overlay.
    pub fn list_notes_visible_typed(
        &self,
        folder_id: &str,
        unlocked: &HashSet<String>,
    ) -> Result<Vec<TypedNoteRow>> {
        // The schema drives which keys are typed + how. Empty schema ⇒ rows carry no `values` (still
        // their id/title/tags), never an error.
        let schema = self.get_note_folder_schema(folder_id)?;
        // GATE 1 — only VISIBLE notes for this folder (sealed-not-unlocked ⇒ absent).
        let summaries = self.list_notes_visible(Some(folder_id), unlocked)?;
        let mut out = Vec::with_capacity(summaries.len());
        for s in summaries {
            // GATE 2 (defense-in-depth) — re-read the markdown through the SAME visibility gate; a
            // row that slipped the summary gate resolves to None and is dropped, never read plain.
            let Some(markdown) = self.note_markdown_if_visible(&s.id, unlocked)? else {
                continue;
            };
            let (tags, raw) = parse_front_matter(&markdown);
            let mut values: std::collections::BTreeMap<String, PropertyValue> =
                std::collections::BTreeMap::new();
            for field in &schema {
                if let Some(raw_val) = raw.get(&field.key) {
                    values.insert(
                        field.key.clone(),
                        coerce_property_value(raw_val, field.kind, &field.options),
                    );
                }
            }
            out.push(TypedNoteRow {
                id: s.id,
                title: s.title,
                folder_id: s.folder_id,
                values,
                tags,
                updated_at: s.updated_at,
            });
        }
        Ok(out)
    }

    /// Read ONE note's raw markdown ONLY when its owning folder is VISIBLE (open or session-unlocked)
    /// — the gated text read [`Self::list_notes_visible_typed`] uses per row. Applies the SAME
    /// `visibility_clause` JOIN as [`Self::list_notes_visible`]: a note in a sealed-and-not-unlocked
    /// folder resolves to `None` (never the stored/blanked text). `kind='note'` enforced.
    pub fn note_markdown_if_visible(
        &self,
        note_id: &str,
        unlocked: &HashSet<String>,
    ) -> Result<Option<String>> {
        let conn = self.lock();
        let visible = visibility_clause("f", unlocked);
        let sql = format!(
            "SELECT COALESCE(d.text, '')
               FROM documents d
               JOIN folders f ON f.id = d.folder_id
              WHERE d.id = ?1 AND d.kind = 'note' AND {visible}"
        );
        conn.query_row(&sql, rusqlite::params![note_id], |r| r.get::<_, String>(0))
            .optional()
            .map_err(map_err)
    }

    /// The ONE reserved, always-open note-folder that backs the "Notes" section root — the home for
    /// UNFILED new notes (2026-07-14). Idempotent; returns the existing `is_root` folder, else picks
    /// one: an UNLOCKED legacy path-`"Notes"` is flagged `is_root=1` in place (no note movement — the
    /// common/fresh case); a LOCKED legacy `"Notes"` can't be repurposed (would expose sealed content)
    /// nor reuse the UNIQUE "Notes" path, so a SEPARATE always-open root is created at the first free
    /// "Inbox" path (the locked "Notes" stays an ordinary folder); with no `"Notes"` folder at all
    /// (fresh install) the root is created at "Notes". Never moves user rows, never touches sealed
    /// content; the root can never be locked (`lock_folder` refuses `is_root`).
    pub fn ensure_notes_root(&self) -> Result<String> {
        if let Some(id) = self.note_root_id()? {
            return Ok(id);
        }
        match self.folder_by_path("Notes")? {
            Some(f) if !f.locked => {
                self.set_folder_is_root(&f.id)?;
                Ok(f.id)
            }
            // Resolved before either insert, and passed in, so the root is created already inside
            // the project rather than parented by a second write that can fail on its own. Its PATH
            // is unchanged either way, because the workspace project occupies the vault root.
            Some(_locked) => {
                // The free path is chosen INSIDE the insert's savepoint, not here. Choosing it
                // out here and passing it in leaves a window: another writer can claim that
                // path with an ordinary container — possibly a SEALED one — and the insert
                // would then hand back a row that is not the root and cannot be one.
                let project = self.workspace_project_id()?;
                self.insert_free_note_root(project.as_deref())
            }
            None => {
                let project = self.workspace_project_id()?;
                self.insert_note_root("Notes", project.as_deref())
            }
        }
    }

    /// The id of the reserved note-root (`is_root=1`), or `None` if it hasn't been created yet.
    pub fn note_root_id(&self) -> Result<Option<String>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT id FROM folders WHERE is_root = 1 AND COALESCE(kind,'meeting') = 'note' LIMIT 1",
            [],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .map_err(map_err)
    }

    /// Flag an existing (unlocked) note-folder as the reserved root. The `AND locked = 0` makes
    /// "is_root ⟹ never sealed" a SQL-enforced invariant even under a concurrent lock race — a locked
    /// folder can NEVER become the always-open root (lock-security review, 2026-07-14).
    fn set_folder_is_root(&self, id: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE folders SET is_root = 1 WHERE id = ?1 AND locked = 0",
            rusqlite::params![id],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// The first free "Inbox"-style path for a separate note-root (when the legacy "Notes" is locked).
    /// The chooser, run against a connection the caller already holds — so the choice and
    /// the write happen inside one savepoint instead of racing between two locks.
    fn first_free_note_root_path_in(conn: &rusqlite::Connection) -> Result<String> {
        for n in 0..1000 {
            let path = if n == 0 {
                "Inbox".to_string()
            } else {
                format!("Inbox {}", n + 1)
            };
            let taken: bool = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM folders WHERE path = ?1)",
                    rusqlite::params![path],
                    |r| Ok(r.get::<_, i64>(0)? != 0),
                )
                .map_err(map_err)?;
            if !taken {
                return Ok(path);
            }
        }
        Err(AppError::Storage(
            "could not allocate a notes-root path".into(),
        ))
    }


    /// Insert a fresh reserved note-root at `path` (name = `path`, `is_root=1`, unlocked, no parent).
    /// `INSERT OR IGNORE` on the UNIQUE path guards a race, then reads the id back.
    /// Insert the reserved note root at `path`, parented into `parent_id`, as ONE unit.
    ///
    /// The parenting is not a follow-up write. The tree renders from the projects down, so a root
    /// that exists without a parent is a root nobody can reach — and it holds every unfiled note. A
    /// failure between the two writes would leave exactly that, permanently: `ensure_notes_root`
    /// short-circuits on the row it finds, so no later launch would go back and repair it.
    /// Insert the reserved note root at the first free `Inbox`-style path, choosing that
    /// path inside the same savepoint that writes it.
    fn insert_free_note_root(&self, parent_id: Option<&str>) -> Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        let conn = self.lock();
        conn.execute_batch("SAVEPOINT note_root_insert")
            .map_err(map_err)?;
        let outcome = Self::first_free_note_root_path_in(&conn)
            .and_then(|path| Self::insert_note_root_in(&conn, &id, &path, parent_id));
        match &outcome {
            Ok(_) => conn.execute_batch("RELEASE note_root_insert"),
            Err(_) => conn.execute_batch("ROLLBACK TO note_root_insert; RELEASE note_root_insert"),
        }
        .map_err(map_err)?;
        outcome
    }

    fn insert_note_root(&self, path: &str, parent_id: Option<&str>) -> Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        let conn = self.lock();
        conn.execute_batch("SAVEPOINT note_root_insert")
            .map_err(map_err)?;
        let outcome = Self::insert_note_root_in(&conn, &id, path, parent_id);
        match &outcome {
            Ok(_) => conn.execute_batch("RELEASE note_root_insert"),
            Err(_) => conn.execute_batch("ROLLBACK TO note_root_insert; RELEASE note_root_insert"),
        }
        .map_err(map_err)?;
        outcome
    }

    /// The body of [`Db::insert_note_root`], always called inside its savepoint.
    fn insert_note_root_in(
        conn: &rusqlite::Connection,
        id: &str,
        path: &str,
        parent_id: Option<&str>,
    ) -> Result<String> {
        conn.execute(
            "INSERT OR IGNORE INTO folders (id, name, path, parent_id, locked, wrapped_key, created_at, kind, is_root)
             VALUES (?1, ?2, ?2, ?4, 0, NULL, ?3, 'note', 1)",
            rusqlite::params![id, path, chrono::Utc::now().to_rfc3339(), parent_id],
        )
        .map_err(map_err)?;
        // Ensure is_root=1 even if a same-path row pre-existed (the OR IGNORE kept the old one). The
        // `AND locked = 0` keeps the "is_root ⟹ never sealed" invariant under a concurrent race where
        // a LOCKED folder was created at this path between the free-path check and here.
        conn.execute(
            "UPDATE folders SET is_root = 1 WHERE path = ?1 AND locked = 0 \
               AND COALESCE(kind, 'meeting') = 'note'",
            rusqlite::params![path],
        )
        .map_err(map_err)?;
        // A same-path row may have pre-existed (the OR IGNORE kept it), and it can be the parentless
        // root a pre-hierarchy build created. Adopt it here for the same reason the insert carries a
        // parent: unreachable is indistinguishable from lost.
        if let Some(project) = parent_id {
            conn.execute(
                "UPDATE folders SET parent_id = ?2 WHERE path = ?1 AND parent_id IS NULL",
                rusqlite::params![path, project],
            )
            .map_err(map_err)?;
        }
        // Read back the row that IS the root, not merely the row sitting at this path. The
        // `is_root` update above deliberately refuses a locked row, so a container that
        // claimed the path in between would leave this returning a non-root — and every
        // caller treats what comes back as the always-open home for unfiled notes.
        conn.query_row(
            "SELECT id FROM folders WHERE path = ?1 AND COALESCE(is_root, 0) = 1",
            rusqlite::params![path],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .map_err(map_err)?
        .ok_or_else(|| {
            AppError::Storage("could not establish the reserved notes root".into())
        })
    }

    pub fn upsert_note(&self, note: &NoteRecord) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        tx.execute(
            "INSERT INTO notes
               (meeting_id, provider_id, markdown, created_at, exported_path,
                model_requested, model_served, gateway_host)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(meeting_id, provider_id) DO UPDATE SET
               markdown = excluded.markdown,
               created_at = excluded.created_at,
               exported_path = excluded.exported_path,
               model_requested = excluded.model_requested,
               model_served = excluded.model_served,
               gateway_host = excluded.gateway_host",
            rusqlite::params![
                note.meeting_id,
                note.provider_id,
                note.markdown,
                note.created_at,
                note.exported_path,
                note.model_requested,
                note.model_served,
                note.gateway_host,
            ],
        )
        .map_err(map_err)?;
        tx.execute(
            "UPDATE org_shares SET republish_dirty = republish_dirty + 1
              WHERE meeting_id = ?1 AND state IN ('queued','uploaded','failed')",
            rusqlite::params![note.meeting_id],
        )
        .map_err(map_err)?;
        tx.commit().map_err(map_err)
    }

    /// SEAL-ON-WRITE twin of [`Db::upsert_note`] for a meeting whose folder is LOCKED (and
    /// session-unlocked — the command layer gates first): persist the fresh markdown (session-
    /// visible) AND its freshly-encrypted `content_blob` AND the governing `folder_id` in ONE atomic
    /// statement. Setting `folder_id` on insert keeps a NEW provider row governed by the meeting's
    /// lock (a bare insert would leave it NULL → ungoverned → visible). The CALLER must have verified
    /// the blob decrypts back byte-identical BEFORE calling this (verify-before-destroy) — exactly
    /// like [`Db::seal_note`]. A new association with a LOCKED folder is refused transactionally
    /// while the meeting has a non-retired generation or pending legacy recovery: those plaintext
    /// recording artifacts are not governed by the folder seal. Re-sealing an existing row already
    /// associated with this folder remains valid. 2026-07-10 audit F1.
    pub fn upsert_note_sealed(
        &self,
        note: &NoteRecord,
        content_blob: &[u8],
        folder_id: &str,
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        guard_locked_folder_assignment_for_recording(
            &tx,
            &note.meeting_id,
            &note.provider_id,
            folder_id,
        )?;
        tx.execute(
            "INSERT INTO notes
               (meeting_id, provider_id, markdown, created_at, exported_path,
                model_requested, model_served, gateway_host, content_blob, folder_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(meeting_id, provider_id) DO UPDATE SET
               markdown = excluded.markdown,
               created_at = excluded.created_at,
               exported_path = excluded.exported_path,
               model_requested = excluded.model_requested,
               model_served = excluded.model_served,
               gateway_host = excluded.gateway_host,
               content_blob = excluded.content_blob,
               folder_id = excluded.folder_id",
            rusqlite::params![
                note.meeting_id,
                note.provider_id,
                note.markdown,
                note.created_at,
                note.exported_path,
                note.model_requested,
                note.model_served,
                note.gateway_host,
                content_blob,
                folder_id,
            ],
        )
        .map_err(map_err)?;
        tx.execute(
            "UPDATE org_shares SET republish_dirty = republish_dirty + 1
              WHERE meeting_id = ?1 AND state IN ('queued','uploaded','failed')",
            rusqlite::params![note.meeting_id],
        )
        .map_err(map_err)?;
        tx.commit().map_err(map_err)
    }

    pub fn get_note(&self, meeting_id: &str, provider_id: &str) -> Result<Option<NoteRecord>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT meeting_id, provider_id, markdown, created_at, exported_path,
                    model_requested, model_served, gateway_host
               FROM notes WHERE meeting_id = ?1 AND provider_id = ?2",
            rusqlite::params![meeting_id, provider_id],
            row_to_note,
        )
        .optional()
        .map_err(map_err)
    }

    pub fn latest_note(&self) -> Result<Option<NoteRecord>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT meeting_id, provider_id, markdown, created_at, exported_path,
                    model_requested, model_served, gateway_host
               FROM notes ORDER BY created_at DESC LIMIT 1",
            [],
            row_to_note,
        )
        .optional()
        .map_err(map_err)
    }

    /// The most recent VISIBLE note across all meetings (BLK-2b backing for `get_last_note`): a note
    /// whose folder is open/NULL or session-unlocked. A sealed-and-not-unlocked latest note is
    /// skipped so the recorder bar never surfaces its blanked (or, defensively, sealed) content.
    pub fn latest_note_visible(&self, unlocked: &HashSet<String>) -> Result<Option<NoteRecord>> {
        let conn = self.lock();
        let visible = visibility_clause("n", unlocked);
        let sql = format!(
            "SELECT n.meeting_id, n.provider_id, n.markdown, n.created_at, n.exported_path,
                    n.model_requested, n.model_served, n.gateway_host
               FROM notes n
               LEFT JOIN folders f ON f.id = n.folder_id
              WHERE {visible}
              ORDER BY n.created_at DESC LIMIT 1"
        );
        conn.query_row(&sql, [], row_to_note)
            .optional()
            .map_err(map_err)
    }

    /// The most recent note for a meeting across providers (Detail view).
    pub fn get_latest_note_for_meeting(&self, meeting_id: &str) -> Result<Option<NoteRecord>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT meeting_id, provider_id, markdown, created_at, exported_path,
                    model_requested, model_served, gateway_host
               FROM notes WHERE meeting_id = ?1
               ORDER BY created_at DESC, provider_id DESC LIMIT 1",
            rusqlite::params![meeting_id],
            row_to_note,
        )
        .optional()
        .map_err(map_err)
    }

    pub fn set_note_exported_path(
        &self,
        meeting_id: &str,
        provider_id: &str,
        path: &str,
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE notes SET exported_path = ?3
             WHERE meeting_id = ?1 AND provider_id = ?2",
            rusqlite::params![meeting_id, provider_id, path],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// The export-collision-guard baseline for a MEETING note: the SHA-256 (lowercase hex) of the
    /// markdown Murmur last wrote to this row's exported vault `.md`. `None` for a legacy row
    /// exported before the guard shipped (grandfathered — no sibling is preserved until the next
    /// Murmur write stamps a baseline) or when the row is unknown.
    pub fn get_note_exported_hash(
        &self,
        meeting_id: &str,
        provider_id: &str,
    ) -> Result<Option<String>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT exported_hash FROM notes WHERE meeting_id = ?1 AND provider_id = ?2",
            rusqlite::params![meeting_id, provider_id],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()
        .map_err(map_err)
        .map(Option::flatten)
    }

    /// Persist (or clear with `None`) a MEETING note's export-collision-guard baseline. Set after
    /// EVERY write Murmur makes to the exported `.md` (full overwrites AND appends), computed from
    /// the exact content written — a stale baseline causes false "external edit" siblings on the
    /// next overwrite.
    pub fn set_note_exported_hash(
        &self,
        meeting_id: &str,
        provider_id: &str,
        hash: Option<&str>,
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE notes SET exported_hash = ?3
             WHERE meeting_id = ?1 AND provider_id = ?2",
            rusqlite::params![meeting_id, provider_id, hash],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// The latest visible note for a meeting (MCP `get_meeting`); `None` if the meeting's note
    /// is sealed-and-not-session-unlocked.
    pub fn get_note_if_visible(
        &self,
        meeting_id: &str,
        unlocked: &HashSet<String>,
    ) -> Result<Option<NoteRecord>> {
        let conn = self.lock();
        let visible = visibility_clause("n", unlocked);
        let sql = format!(
            "SELECT n.meeting_id, n.provider_id, n.markdown, n.created_at, n.exported_path,
                    n.model_requested, n.model_served, n.gateway_host
               FROM notes n
               LEFT JOIN folders f ON f.id = n.folder_id
              WHERE n.meeting_id = ?1 AND {visible}
              ORDER BY n.created_at DESC LIMIT 1"
        );
        conn.query_row(&sql, rusqlite::params![meeting_id], row_to_note)
            .optional()
            .map_err(map_err)
    }

    /// Whether ONE authored note is VISIBLE (open or session-unlocked folder) — the lightweight
    /// existence twin of [`Db::note_markdown_if_visible`], used by the audit list's defensive
    /// re-filter. Fail-closed on an unknown id.
    pub fn note_is_visible(&self, note_id: &str, unlocked: &HashSet<String>) -> Result<bool> {
        let conn = self.lock();
        let visible = visibility_clause("f", unlocked);
        let sql = format!(
            "SELECT EXISTS(SELECT 1 FROM documents d
               JOIN folders f ON f.id = d.folder_id
              WHERE d.id = ?1 AND d.kind = 'note' AND {visible})"
        );
        conn.query_row(&sql, rusqlite::params![note_id], |r| r.get(0))
            .map_err(map_err)
    }
}

/// Storage-level backstop shared by every sealed-note upsert outcome (INSERT and conflict UPDATE).
/// The check and the folder-id write must remain in one transaction: a future caller must not be
/// able to move a meeting with unmanaged recording artifacts behind a folder lock by bypassing
/// `Db::set_meeting_folder`. The exact existing provider row is exempt only when it is already
/// governed by this target folder, because that path is a content re-seal rather than an assignment.
fn guard_locked_folder_assignment_for_recording(
    tx: &Transaction<'_>,
    meeting_id: &str,
    provider_id: &str,
    folder_id: &str,
) -> Result<()> {
    let blocked = tx
        .query_row(
            "SELECT EXISTS(
                 SELECT 1
                   FROM folders f
                  WHERE f.id = ?3
                    AND f.locked = 1
                    AND (
                        EXISTS(
                            SELECT 1 FROM recording_generations rg
                             WHERE rg.meeting_id = ?1 AND rg.state != 'RETIRED'
                        ) OR EXISTS(
                            SELECT 1 FROM legacy_recording_recovery lr
                             WHERE lr.meeting_id = ?1
                        )
                    )
                    AND NOT EXISTS(
                        SELECT 1
                          FROM notes n
                         WHERE n.meeting_id = ?1
                           AND n.provider_id = ?2
                           AND n.folder_id = ?3
                    )
             )",
            rusqlite::params![meeting_id, provider_id, folder_id],
            |row| row.get::<_, bool>(0),
        )
        .map_err(map_err)?;
    if blocked {
        return Err(AppError::Locked(
            "meeting has plaintext recording artifacts that are not governed by the locked folder"
                .into(),
        ));
    }
    Ok(())
}

fn row_to_note(row: &Row<'_>) -> rusqlite::Result<NoteRecord> {
    Ok(NoteRecord {
        meeting_id: row.get(0)?,
        provider_id: row.get(1)?,
        markdown: row.get(2)?,
        created_at: row.get(3)?,
        exported_path: row.get(4)?,
        model_requested: row.get(5)?,
        model_served: row.get(6)?,
        gateway_host: row.get(7)?,
    })
}

/// Column order: `id, folder_id, name, title, text, created_at, updated_at, exported_path, sealed`.
fn row_to_note_row(row: &Row<'_>) -> rusqlite::Result<NoteRow> {
    Ok(NoteRow {
        id: row.get(0)?,
        folder_id: row.get(1)?,
        name: row.get(2)?,
        title: row.get(3)?,
        text: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
        exported_path: row.get(7)?,
        sealed: row.get::<_, i64>(8)? != 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::models::{
        Folder, Meeting, MeetingStatus, NoteFolder, RecordingGenerationKey,
        RecordingMicAssertion,
    };

    const TEST_DEK: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn db() -> Db {
        Db::open_with_key(std::path::Path::new(":memory:"), TEST_DEK).unwrap()
    }

    fn seed_meeting(db: &Db) -> String {
        let id = uuid::Uuid::new_v4().hyphenated().to_string();
        db.insert_meeting(&Meeting {
            id: id.clone(),
            started_at: "2026-07-22T12:00:00Z".into(),
            ended_at: None,
            title: None,
            duration_s: 0,
            audio_path: None,
            status: MeetingStatus::Draft,
            folder_id: None,
        })
        .unwrap();
        id
    }

    fn seed_locked_folder(db: &Db) -> String {
        let id = uuid::Uuid::new_v4().hyphenated().to_string();
        db.insert_folder(&Folder {
            id: id.clone(),
            name: "Private".into(),
            path: id.clone(),
            parent_id: None,
            locked: false,
            created_at: "2026-07-22T12:00:00Z".into(),
        })
        .unwrap();
        db.set_folder_locked(&id, true, Some(b"wrapped-key"))
            .unwrap();
        id
    }

    fn note(meeting_id: &str, markdown: &str) -> NoteRecord {
        NoteRecord {
            meeting_id: meeting_id.into(),
            provider_id: "test".into(),
            markdown: markdown.into(),
            created_at: "2026-07-22T12:00:01Z".into(),
            exported_path: None,
            model_requested: None,
            model_served: None,
            gateway_host: None,
        }
    }

    fn seed_active_generation(db: &Db, meeting_id: &str) {
        let key = RecordingGenerationKey::fresh(meeting_id).unwrap();
        let mic = RecordingMicAssertion::for_generation(&key, 48_000, 7, 11).unwrap();
        let _lease = db.prepare_recording_generation(&key, &mic, 60_000).unwrap();
    }

    fn stored_note_seal(db: &Db, meeting_id: &str) -> (String, Option<Vec<u8>>, Option<String>) {
        db.lock()
            .query_row(
                "SELECT markdown, content_blob, folder_id
                   FROM notes
                  WHERE meeting_id = ?1 AND provider_id = 'test'",
                [meeting_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap()
    }

    #[test]
    fn companion_birth_rolls_back_document_and_link_at_each_checkpoint() {
        for fail_after_link in [false, true] {
            let db = db();
            let meeting_id = seed_meeting(&db);
            let folder_id = db.ensure_notes_root().unwrap();
            let note_id = if fail_after_link {
                "fail-after-link"
            } else {
                "fail-after-document"
            };

            let result = db.insert_companion_note_atomic_failing_at(
                note_id,
                &folder_id,
                "generated-note",
                "Generated note",
                "---\nmeeting: \"[[Meeting]]\"\n---\n\nGenerated body",
                &meeting_id,
                42,
                fail_after_link,
            );

            assert!(matches!(result, Err(AppError::Storage(_))));
            assert!(db.get_note_row(note_id).unwrap().is_none());
            assert_eq!(db.companion_note_for_meeting(&meeting_id).unwrap(), None);
            let conn = db.lock();
            let document_rows: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM documents WHERE id = ?1",
                    [note_id],
                    |row| row.get(0),
                )
                .unwrap();
            let link_rows: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM links
                      WHERE src_kind = 'note' AND src_id = ?1
                        AND dst_kind = 'meeting' AND dst_id = ?2
                        AND edge_type = 'companion'",
                    rusqlite::params![note_id, meeting_id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(document_rows, 0, "failure must leave no orphan note row");
            assert_eq!(link_rows, 0, "failure must leave no partial companion edge");
        }
    }

    #[test]
    fn companion_birth_commits_body_meeting_id_and_required_link_together() {
        let db = db();
        let meeting_id = seed_meeting(&db);
        let folder_id = db.ensure_notes_root().unwrap();
        let markdown = "---\nmeeting: \"[[Meeting]]\"\n---\n\nGenerated body";

        db.insert_companion_note_atomic(
            "companion-ok",
            &folder_id,
            "generated-note",
            "Generated note",
            markdown,
            &meeting_id,
            42,
        )
        .unwrap();

        let row = db.get_note_row("companion-ok").unwrap().unwrap();
        assert_eq!(row.text, markdown);
        assert_eq!(
            db.companion_note_for_meeting(&meeting_id).unwrap().as_deref(),
            Some("companion-ok")
        );
        let link_rows: i64 = db
            .lock()
            .query_row(
                "SELECT COUNT(*) FROM links
                  WHERE src_kind = 'note' AND src_id = 'companion-ok'
                    AND dst_kind = 'meeting' AND dst_id = ?1
                    AND edge_type = 'companion'",
                [&meeting_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(link_rows, 1);
    }

    #[test]
    fn companion_birth_cannot_broaden_nonempty_locked_folder_birth() {
        let db = db();
        let meeting_id = seed_meeting(&db);
        db.insert_note_folder(
            &NoteFolder {
                id: "ordinary-folder".into(),
                name: "Ordinary".into(),
                path: "Ordinary".into(),
                parent_id: None,
                locked: false,
                unlocked: false,
                is_root: false,
                kind: "note".into(),
            },
            "2026-07-22T12:00:00Z",
        )
        .unwrap();
        db.set_folder_locked("ordinary-folder", true, Some(b"wrapped-key"))
            .unwrap();

        let result = db.insert_companion_note_atomic(
            "must-not-exist",
            "ordinary-folder",
            "generated-note",
            "Generated note",
            "non-empty generated body",
            &meeting_id,
            42,
        );

        assert!(matches!(result, Err(AppError::Locked(_))));
        assert!(db.get_note_row("must-not-exist").unwrap().is_none());
    }

    #[test]
    fn auto_title_cas_never_overwrites_new_body_or_crosses_folder_lock() {
        let db = db();
        let folder_id = uuid::Uuid::new_v4().hyphenated().to_string();
        db.insert_folder(&Folder {
            id: folder_id.clone(),
            name: "Notes".into(),
            path: folder_id.clone(),
            parent_id: None,
            locked: false,
            created_at: "2026-07-22T12:00:00Z".into(),
        })
        .unwrap();

        db.insert_note("auto-ok", &folder_id, "auto-ok", "Untitled", "old body", 1)
            .unwrap();
        let observed = db.get_note_row("auto-ok").unwrap().unwrap();
        assert!(db
            .set_auto_title_if_unchanged_and_open(
                "auto-ok",
                &folder_id,
                observed.updated_at,
                &observed.text,
                "Generated title",
                2,
            )
            .unwrap());
        let titled = db.get_note_row("auto-ok").unwrap().unwrap();
        assert_eq!(titled.title.as_deref(), Some("Generated title"));
        assert_eq!(titled.text, "old body");

        db.insert_note(
            "auto-stale",
            &folder_id,
            "auto-stale",
            "Untitled",
            "observed body",
            10,
        )
        .unwrap();
        let stale = db.get_note_row("auto-stale").unwrap().unwrap();
        db.update_note_row("auto-stale", "User title", "new editor body", 11)
            .unwrap();
        assert!(!db
            .set_auto_title_if_unchanged_and_open(
                "auto-stale",
                &folder_id,
                stale.updated_at,
                &stale.text,
                "Stale generated title",
                12,
            )
            .unwrap());
        let current = db.get_note_row("auto-stale").unwrap().unwrap();
        assert_eq!(current.title.as_deref(), Some("User title"));
        assert_eq!(current.text, "new editor body");

        db.insert_note(
            "auto-locked",
            &folder_id,
            "auto-locked",
            "Untitled",
            "private body",
            20,
        )
        .unwrap();
        let before_lock = db.get_note_row("auto-locked").unwrap().unwrap();
        db.set_folder_locked(&folder_id, true, Some(b"wrapped-key"))
            .unwrap();
        assert!(!db
            .set_auto_title_if_unchanged_and_open(
                "auto-locked",
                &folder_id,
                before_lock.updated_at,
                &before_lock.text,
                "Must not land",
                21,
            )
            .unwrap());
        assert_eq!(
            db.get_note_row("auto-locked")
                .unwrap()
                .unwrap()
                .title
                .as_deref(),
            Some("Untitled")
        );
    }

    #[test]
    fn sealed_upsert_rejects_active_recording_reassignment_to_locked_folder() {
        let db = db();
        let meeting_id = seed_meeting(&db);
        let folder_id = seed_locked_folder(&db);
        db.upsert_note(&note(&meeting_id, "before")).unwrap();
        seed_active_generation(&db, &meeting_id);

        let result = db.upsert_note_sealed(&note(&meeting_id, "after"), b"after-blob", &folder_id);

        assert!(matches!(result, Err(AppError::Locked(_))));
        assert_eq!(
            stored_note_seal(&db, &meeting_id),
            ("before".into(), None, None),
            "the rejected assignment must leave the existing note unchanged"
        );
    }

    #[test]
    fn sealed_upsert_rejects_pending_legacy_recovery_reassignment_to_locked_folder() {
        let db = db();
        let meeting_id = seed_meeting(&db);
        let folder_id = seed_locked_folder(&db);
        db.upsert_note(&note(&meeting_id, "before")).unwrap();
        db.mark_legacy_recording_recovery_pending(&meeting_id)
            .unwrap();

        let result = db.upsert_note_sealed(&note(&meeting_id, "after"), b"after-blob", &folder_id);

        assert!(matches!(result, Err(AppError::Locked(_))));
        assert_eq!(
            stored_note_seal(&db, &meeting_id),
            ("before".into(), None, None),
            "the rejected assignment must leave the existing note unchanged"
        );
    }

    #[test]
    fn sealed_upsert_allows_new_locked_org_ingest_without_recording_generation() {
        let db = db();
        let meeting_id = seed_meeting(&db);
        let folder_id = seed_locked_folder(&db);

        db.upsert_note_sealed(&note(&meeting_id, "shared"), b"shared-blob", &folder_id)
            .unwrap();

        assert_eq!(
            stored_note_seal(&db, &meeting_id),
            (
                "shared".into(),
                Some(b"shared-blob".to_vec()),
                Some(folder_id)
            )
        );
    }

    #[test]
    fn sealed_upsert_allows_reseal_already_associated_with_locked_folder() {
        let db = db();
        let meeting_id = seed_meeting(&db);
        let folder_id = seed_locked_folder(&db);
        db.upsert_note_sealed(&note(&meeting_id, "before"), b"before-blob", &folder_id)
            .unwrap();
        seed_active_generation(&db, &meeting_id);

        db.upsert_note_sealed(&note(&meeting_id, "after"), b"after-blob", &folder_id)
            .unwrap();

        assert_eq!(
            stored_note_seal(&db, &meeting_id),
            (
                "after".into(),
                Some(b"after-blob".to_vec()),
                Some(folder_id)
            )
        );
    }
}
