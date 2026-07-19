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
