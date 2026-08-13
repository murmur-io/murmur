//! App key/value settings storage surface — the `settings` table get/set/list, extracted verbatim
//! from `storage::db` (God-file split, a PURE MOVE — no behavior change). The methods below are an
//! inherent-impl split of [`crate::storage::db::Db`] across files (Rust allows one type's inherent
//! `impl` to live in multiple files of the same crate); every method retains its EXACT prior body
//! and signature. These are opaque app-config key/value rows (toggles, defaults), not gated meeting
//! content — no lock/visibility gate applies. The `settings` schema stays inline in `Db::migrate()`
//! (it is created there with its seeded default rows, not via a standalone `migrate_*` fn); the
//! shared db.rs module-level helper `map_err` is `pub(crate)` for the sibling access.

use rusqlite::OptionalExtension;

use crate::error::Result;
use crate::storage::db::{map_err, Db};

fn delete_all_vector_partitions(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    // vec0 tables do not participate in the source tables' foreign-key cascades. Delete all four
    // model-dependent partitions explicitly; no plaintext/chunk row is read or removed.
    tx.execute("DELETE FROM vec_chunks", []).map_err(map_err)?;
    tx.execute("DELETE FROM topic_vec_chunks", [])
        .map_err(map_err)?;
    tx.execute("DELETE FROM doc_vec_chunks", [])
        .map_err(map_err)?;
    tx.execute("DELETE FROM org_vec_chunks", [])
        .map_err(map_err)?;
    Ok(())
}

impl Db {
    // ── settings k/v table ───────────────────────────────────────────────────

    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT value FROM settings WHERE key = ?1",
            rusqlite::params![key],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(map_err)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![key, value],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Current global Ask-dispatch authorization generation. Captured with the effective Ask
    /// config under the process lifecycle mutex and compared again at every dispatch/read CAS.
    pub(crate) fn ask_dispatch_generation(&self) -> Result<i64> {
        self.lock()
            .query_row(
                "SELECT generation FROM ask_dispatch_state WHERE singleton=1
                   AND typeof(generation)='integer' AND generation>=0",
                [],
                |row| row.get(0),
            )
            .map_err(map_err)
    }

    /// Advance the durable Ask-dispatch generation before a multi-row AppConfig save. A later
    /// config-save failure may conservatively over-invalidate, but can never leave an old provider
    /// authorization usable after a partially persisted change.
    pub(crate) fn advance_ask_dispatch_generation(&self) -> Result<i64> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        tx.execute(
            "UPDATE ask_dispatch_state SET generation=generation+1 WHERE singleton=1
               AND typeof(generation)='integer' AND generation>=0 AND generation<9223372036854775807",
            [],
        )
        .map_err(map_err)
        .and_then(|changed| {
            if changed == 1 {
                Ok(changed)
            } else {
                Err(crate::error::AppError::Storage(
                    "Ask dispatch generation is unavailable".into(),
                ))
            }
        })?;
        let generation = tx
            .query_row(
                "SELECT generation FROM ask_dispatch_state WHERE singleton=1
                   AND typeof(generation)='integer' AND generation>=0",
                [],
                |row| row.get(0),
            )
            .map_err(map_err)?;
        tx.commit().map_err(map_err)?;
        Ok(generation)
    }

    /// Persist cloud consent and rotate Ask authorization atomically. Callers compare the cached
    /// value first, so idempotent grant/revoke operations do not rotate the generation.
    pub(crate) fn set_cloud_egress_consent_and_advance_ask_dispatch(
        &self,
        consented: bool,
    ) -> Result<i64> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        tx.execute(
            "INSERT INTO settings(key,value) VALUES ('cloud_egress_consented',?1)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            [if consented { "true" } else { "false" }],
        )
        .map_err(map_err)?;
        tx.execute(
            "UPDATE ask_dispatch_state SET generation=generation+1 WHERE singleton=1
               AND typeof(generation)='integer' AND generation>=0 AND generation<9223372036854775807",
            [],
        )
        .map_err(map_err)
        .and_then(|changed| {
            if changed == 1 {
                Ok(changed)
            } else {
                Err(crate::error::AppError::Storage(
                    "Ask dispatch generation is unavailable".into(),
                ))
            }
        })?;
        let generation = tx
            .query_row(
                "SELECT generation FROM ask_dispatch_state WHERE singleton=1
                   AND typeof(generation)='integer' AND generation>=0",
                [],
                |row| row.get(0),
            )
            .map_err(map_err)?;
        tx.commit().map_err(map_err)?;
        Ok(generation)
    }

    /// Persist the selected embedding-model id and, when its resolved model changed, invalidate
    /// every vector partition in the SAME transaction. Chunks + FTS stay intact, so retrieval
    /// degrades to keyword-only until the explicit/background reindex repopulates vectors.
    ///
    /// Atomicity is load-bearing: saving model B and crashing before deleting model-A vectors
    /// would make the next launch encode B queries against an incompatible A index. The caller
    /// owns the process-wide embed-selection write barrier, so no guarded vector writer can land
    /// output between this invalidation and publication of B.
    pub fn set_embed_model_selection(
        &self,
        model_id: &str,
        invalidate_vectors: bool,
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        tx.execute(
            "INSERT INTO settings (key, value) VALUES ('embed_model_id', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![model_id],
        )
        .map_err(map_err)?;
        if invalidate_vectors {
            delete_all_vector_partitions(&tx)?;
        }
        tx.commit().map_err(map_err)?;
        Ok(())
    }

    /// Ask-aware embedding-model switch: setting, vector invalidation and dispatch-generation
    /// rotation commit atomically so retrieval can never resume with an old authorization.
    pub(crate) fn set_embed_model_selection_for_ask(
        &self,
        model_id: &str,
        invalidate_vectors: bool,
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        tx.execute(
            "INSERT INTO settings (key, value) VALUES ('embed_model_id', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![model_id],
        )
        .map_err(map_err)?;
        if invalidate_vectors {
            delete_all_vector_partitions(&tx)?;
        }
        let rotated = tx.execute(
            "UPDATE ask_dispatch_state SET generation=generation+1 WHERE singleton=1
               AND typeof(generation)='integer' AND generation>=0 AND generation<9223372036854775807",
            [],
        )
        .map_err(map_err)?;
        if rotated != 1 {
            return Err(crate::error::AppError::Storage(
                "Ask dispatch generation is unavailable".into(),
            ));
        }
        tx.commit().map_err(map_err)
    }

    /// Clear every model-dependent vector partition in one transaction while preserving all
    /// canonical source rows, chunks, FTS indexes, and Org feed cursors. Full reindex calls this
    /// before writing the first new vector, so an interrupted rebuild is new-model-or-empty rather
    /// than a silently mixed old/new index.
    pub fn invalidate_all_vector_embeddings(&self) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        delete_all_vector_partitions(&tx)?;
        tx.commit().map_err(map_err)?;
        Ok(())
    }

    pub fn all_settings(&self) -> Result<Vec<(String, String)>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT key, value FROM settings ORDER BY key")
            .map_err(map_err)?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }
}
