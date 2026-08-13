//! MCP-server config storage surface — the `mcp_servers` table CRUD + its idempotent schema,
//! extracted verbatim from `storage::db` (God-file split, a PURE MOVE — no behavior change). The
//! methods below are an inherent-impl split of [`crate::storage::db::Db`] across files (Rust allows
//! one type's inherent `impl` to live in multiple files of the same crate); every method retains its
//! EXACT prior body and signature. This is Brain v2 L5 connection config only — never query text or
//! results — so no lock/visibility gate applies (mcp_servers rows are NOT meeting content). Shared
//! db.rs module-level helpers (`map_err`) and the schema fn `migrate_mcp_servers` are `pub(crate)`
//! for the sibling access; `migrate()` in db.rs still calls `Self::migrate_mcp_servers(&conn)`
//! unchanged. `row_to_mcp_server` (only ever used by these two readers) moved along with them.

use rusqlite::{Connection, OptionalExtension, Row};

use crate::error::{AppError, Result};
use crate::storage::db::{map_err, Db};

impl Db {
    /// Brain v2 L5 — idempotent MCP-SERVER config schema. One row per user-configured external MCP
    /// server: transport + endpoint + args + the ENABLED and per-server CONSENTED flags (consent
    /// default 0 — fail-closed; flipped only by the dedicated consent commands). Connection config
    /// only — never query text or results.
    pub(crate) fn migrate_mcp_servers(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS mcp_servers (
               id TEXT PRIMARY KEY,
               label TEXT NOT NULL,
               transport TEXT NOT NULL,
               endpoint TEXT NOT NULL,
               args TEXT NOT NULL DEFAULT '[]',
               enabled INTEGER NOT NULL DEFAULT 1,
               consented INTEGER NOT NULL DEFAULT 0,
               created_at TEXT NOT NULL
             );",
        )
        .map_err(map_err)
    }

    // ── Brain v2 L5 — MCP server config rows ────────────────────────────────────────────────────

    /// All configured MCP servers (connection config only — never query text or results).
    pub fn list_mcp_servers(&self) -> Result<Vec<crate::storage::models::McpServer>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, label, transport, endpoint, args, enabled, consented, created_at \
                   FROM mcp_servers ORDER BY created_at ASC, id ASC",
            )
            .map_err(map_err)?;
        let rows = stmt.query_map([], row_to_mcp_server).map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// One MCP server by id.
    pub fn get_mcp_server(&self, id: &str) -> Result<Option<crate::storage::models::McpServer>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT id, label, transport, endpoint, args, enabled, consented, created_at \
               FROM mcp_servers WHERE id = ?1",
            [id],
            row_to_mcp_server,
        )
        .optional()
        .map_err(map_err)
    }

    /// Insert one MCP server row (caller validates transport/endpoint — see `add_mcp_server`).
    pub fn insert_mcp_server(&self, s: &crate::storage::models::McpServer) -> Result<()> {
        let args = serde_json::to_string(&s.args)
            .map_err(|e| AppError::Storage(format!("mcp args serialize: {e}")))?;
        let conn = self.lock();
        conn.execute(
            "INSERT INTO mcp_servers (id, label, transport, endpoint, args, enabled, consented, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                s.id,
                s.label,
                s.transport,
                s.endpoint,
                args,
                s.enabled as i64,
                s.consented as i64,
                s.created_at,
            ],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Remove one MCP server row (revokes its tool exposure on the next registry/spec build).
    pub fn delete_mcp_server(&self, id: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute("DELETE FROM mcp_servers WHERE id = ?1", [id])
            .map_err(map_err)?;
        Ok(())
    }

    /// Remove a server and rotate Ask authorization in the same transaction only when the server
    /// was consented (and therefore part of the effective durable-Ask tool surface).
    pub(crate) fn delete_mcp_server_for_ask(&self, id: &str) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let consented = tx
            .query_row("SELECT consented FROM mcp_servers WHERE id=?1", [id], |row| {
                row.get::<_, i64>(0)
            })
            .optional()
            .map_err(map_err)?
            == Some(1);
        tx.execute("DELETE FROM mcp_servers WHERE id=?1", [id])
            .map_err(map_err)?;
        if consented {
            let rotated = tx.execute(
                "UPDATE ask_dispatch_state SET generation=generation+1 WHERE singleton=1
                   AND typeof(generation)='integer' AND generation>=0 AND generation<9223372036854775807",
                [],
            )
            .map_err(map_err)?;
            if rotated != 1 {
                return Err(AppError::Storage(
                    "Ask dispatch generation is unavailable".into(),
                ));
            }
        }
        tx.commit().map_err(map_err)
    }

    /// Flip one MCP server's per-server egress consent (the ONLY writer of `consented`).
    pub fn set_mcp_server_consented(&self, id: &str, consented: bool) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE mcp_servers SET consented = ?2 WHERE id = ?1",
            rusqlite::params![id, consented as i64],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Flip MCP consent and rotate Ask authorization atomically. Idempotent writes do neither.
    pub(crate) fn set_mcp_server_consented_for_ask(
        &self,
        id: &str,
        consented: bool,
    ) -> Result<bool> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let changed = tx
            .execute(
                "UPDATE mcp_servers SET consented=?2 WHERE id=?1 AND consented<>?2",
                rusqlite::params![id, consented as i64],
            )
            .map_err(map_err)?;
        if changed == 1 {
            let rotated = tx.execute(
                "UPDATE ask_dispatch_state SET generation=generation+1 WHERE singleton=1
                   AND typeof(generation)='integer' AND generation>=0 AND generation<9223372036854775807",
                [],
            )
            .map_err(map_err)?;
            if rotated != 1 {
                return Err(AppError::Storage(
                    "Ask dispatch generation is unavailable".into(),
                ));
            }
        }
        tx.commit().map_err(map_err)?;
        Ok(changed == 1)
    }
}

/// Map an `mcp_servers` row (id, label, transport, endpoint, args JSON, enabled, consented,
/// created_at) to a [`crate::storage::models::McpServer`]. Malformed `args` JSON degrades to no
/// args (fail-quiet on config, never a crash).
fn row_to_mcp_server(row: &Row<'_>) -> rusqlite::Result<crate::storage::models::McpServer> {
    let args_json: String = row.get(4)?;
    Ok(crate::storage::models::McpServer {
        id: row.get(0)?,
        label: row.get(1)?,
        transport: row.get(2)?,
        endpoint: row.get(3)?,
        args: serde_json::from_str(&args_json).unwrap_or_default(),
        enabled: row.get::<_, i64>(5)? != 0,
        consented: row.get::<_, i64>(6)? != 0,
        created_at: row.get(7)?,
    })
}
