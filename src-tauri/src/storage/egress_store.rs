//! Egress-ledger storage surface — the `egress_log` (per-LLM-call redaction/token bookkeeping) +
//! `share_egress_log` (content-free outbound-share host/byte ledger) writes and the aggregate
//! summary read, extracted verbatim from `storage::db` (God-file split, a PURE MOVE — no behavior
//! change). The methods below are an inherent-impl split of [`crate::storage::db::Db`] across files
//! (Rust allows one type's inherent `impl` to live in multiple files of the same crate); every
//! method retains its EXACT prior body and signature. These rows are CONTENT-FREE by construction
//! (counts, byte sizes, redaction tallies, model labels, hosts — never note/transcript text, a URL,
//! or ciphertext) — so no lock/visibility gate applies; `egress_summary` reads only aggregate
//! counts/tokens/redactions off `egress_log`. Both schemas stay inline in `Db::migrate()` (created
//! there, not via a standalone `migrate_*` fn); the shared db.rs module-level helper `map_err` and
//! the ledger DTO structs (`EgressLedger` / `EgressModelUsage` / `EgressDayUsage` /
//! `EgressRedactionTotals` / `EgressRecentRow`) are `pub`/`pub(crate)` for the sibling access.

use crate::error::Result;
use crate::storage::db::{
    map_err, Db, EgressDayUsage, EgressLedger, EgressModelUsage, EgressRecentRow,
    EgressRedactionTotals,
};

impl Db {
    // ── egress audit log ────────────────────────────────────────────────────

    /// Insert one content-free audit row into `egress_log`. Called by `DbEgressSink::record`.
    ///
    /// `ts` is a Unix epoch (seconds) computed by the caller (`SystemTime::now()`). The row
    /// carries ONLY counts, ids, labels, byte sizes, and token counts — NO content (§8).
    pub fn insert_egress(
        &self,
        ts: i64,
        e: &crate::summarize::egress_log::EgressEntry,
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO egress_log (
               ts, provider_id, destination, model_requested, model_served, call_kind,
               prompt_tokens, completion_tokens, total_tokens, cached_tokens,
               redactions_email, redactions_card, redactions_phone, redactions_name,
               system_bytes, user_bytes, meeting_id
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
            rusqlite::params![
                ts,
                e.provider_id,
                e.destination,
                e.model_requested,
                e.meta.model_served.as_deref(),
                e.call_kind,
                e.meta.prompt_tokens.map(|v| v as i64),
                e.meta.completion_tokens.map(|v| v as i64),
                e.meta.total_tokens.map(|v| v as i64),
                e.meta.cached_tokens.map(|v| v as i64),
                e.redactions.email as i64,
                e.redactions.card as i64,
                e.redactions.phone as i64,
                e.redactions.name as i64,
                e.system_bytes as i64,
                e.user_bytes as i64,
                e.meeting_id.as_deref(),
            ],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Append one CONTENT-FREE share-egress ledger row (§7 inv. 4): host + byte size + a `kind` label
    /// (`"share_create"` / `"share_revoke"` / `"account_login"` …). NEVER the URL, `L`, a title, or
    /// any note text. `ts` is a Unix epoch (seconds).
    pub fn insert_share_egress(
        &self,
        ts: i64,
        host: &str,
        kind: &str,
        byte_count: usize,
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO share_egress_log (ts, host, kind, byte_count) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![ts, host, kind, byte_count as i64],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Count content-free egress-ledger rows of a given `kind` (e.g. `org_share_publish` /
    /// `org_share_revoke`). Content-free (a count, never a body); used by the re-publish tests to
    /// assert a publish+revoke pair was ledgered on an edit-supersede.
    pub fn count_share_egress_by_kind(&self, kind: &str) -> Result<u64> {
        let conn = self.lock();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM share_egress_log WHERE kind = ?1",
                rusqlite::params![kind],
                |r| r.get(0),
            )
            .map_err(map_err)?;
        Ok(n as u64)
    }

    /// Aggregate the `egress_log` table over the last `days` calendar days and return a rich
    /// summary for the "Egress & Usage" Analytics panel.
    ///
    /// The time window is `[now_unix - days*86400, now_unix]`. A `days <= 0` value returns ALL
    /// rows. An empty table (no cloud calls yet) returns all-zero totals and empty vecs — never
    /// an error.
    ///
    /// Read-only: touches `egress_log` only; no content columns. (§6: egress_log has none.)
    pub fn egress_summary(&self, days: i64) -> Result<EgressLedger> {
        use std::time::{SystemTime, UNIX_EPOCH};

        let now_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let since = if days > 0 {
            now_unix - days * 86_400
        } else {
            0
        };

        let conn = self.lock();

        // ── total calls + total tokens ──────────────────────────────────────
        // Cast to u64 so an all-time SUM cannot wrap (i64→u64 is safe for non-negative sums).
        let (total_calls, total_tokens): (u64, u64) = conn
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(total_tokens),0)
                   FROM egress_log
                  WHERE ts >= ?1",
                rusqlite::params![since],
                |r| Ok((r.get::<_, i64>(0)? as u64, r.get::<_, i64>(1)? as u64)),
            )
            .map_err(map_err)?;

        // ── total redactions ────────────────────────────────────────────────
        let total_redactions: EgressRedactionTotals = conn
            .query_row(
                "SELECT COALESCE(SUM(redactions_email),0),
                        COALESCE(SUM(redactions_card),0),
                        COALESCE(SUM(redactions_phone),0),
                        COALESCE(SUM(redactions_name),0)
                   FROM egress_log
                  WHERE ts >= ?1",
                rusqlite::params![since],
                |r| {
                    Ok(EgressRedactionTotals {
                        email: r.get::<_, i64>(0)? as u64,
                        card: r.get::<_, i64>(1)? as u64,
                        phone: r.get::<_, i64>(2)? as u64,
                        name: r.get::<_, i64>(3)? as u64,
                    })
                },
            )
            .map_err(map_err)?;

        // ── by_model (GROUP BY model label, tokens DESC) ────────────────────
        let by_model = {
            let mut stmt = conn
                .prepare(
                    // NULLIF guards: an empty string '' in model_served or model_requested
                    // (the default when no model is sent by claude_code/anthropic) must bucket
                    // under '(unknown)' rather than producing a blank label in the Settings UI.
                    "SELECT COALESCE(NULLIF(model_served,''), NULLIF(model_requested,''), '(unknown)') AS model,
                            COUNT(*) AS calls,
                            COALESCE(SUM(total_tokens), 0) AS tokens
                       FROM egress_log
                      WHERE ts >= ?1
                      GROUP BY model
                      ORDER BY tokens DESC",
                )
                .map_err(map_err)?;
            let rows = stmt
                .query_map(rusqlite::params![since], |r| {
                    Ok(EgressModelUsage {
                        model: r.get(0)?,
                        calls: r.get::<_, i64>(1)? as u64,
                        tokens: r.get::<_, i64>(2)? as u64,
                    })
                })
                .map_err(map_err)?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row.map_err(map_err)?);
            }
            out
        };

        // ── by_day (GROUP BY UTC date, ascending) ──────────────────────────
        let by_day = {
            let mut stmt = conn
                .prepare(
                    "SELECT date(ts, 'unixepoch') AS day,
                            COALESCE(SUM(total_tokens), 0) AS tokens
                       FROM egress_log
                      WHERE ts >= ?1
                      GROUP BY day
                      ORDER BY day ASC",
                )
                .map_err(map_err)?;
            let rows = stmt
                .query_map(rusqlite::params![since], |r| {
                    Ok(EgressDayUsage {
                        day: r.get(0)?,
                        tokens: r.get::<_, i64>(1)? as u64,
                    })
                })
                .map_err(map_err)?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row.map_err(map_err)?);
            }
            out
        };

        // ── recent rows (last ≤20, newest first) ───────────────────────────
        let recent = {
            let mut stmt = conn
                .prepare(
                    "SELECT ts, provider_id, destination, model_served, total_tokens,
                            redactions_email, redactions_card, redactions_phone, redactions_name
                       FROM egress_log
                      WHERE ts >= ?1
                      ORDER BY ts DESC
                      LIMIT 20",
                )
                .map_err(map_err)?;
            let rows = stmt
                .query_map(rusqlite::params![since], |r| {
                    Ok(EgressRecentRow {
                        ts: r.get(0)?,
                        provider_id: r.get(1)?,
                        destination: r.get(2)?,
                        model_served: r.get(3)?,
                        total_tokens: r.get::<_, Option<i64>>(4)?.map(|v| v as u32),
                        redactions: EgressRedactionTotals {
                            email: r.get::<_, i64>(5)? as u64,
                            card: r.get::<_, i64>(6)? as u64,
                            phone: r.get::<_, i64>(7)? as u64,
                            name: r.get::<_, i64>(8)? as u64,
                        },
                    })
                })
                .map_err(map_err)?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row.map_err(map_err)?);
            }
            out
        };

        Ok(EgressLedger {
            total_calls,
            total_tokens,
            by_model,
            by_day,
            total_redactions,
            recent,
        })
    }
}
