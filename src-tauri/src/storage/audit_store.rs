//! Vault Audit storage surface — the `audit_findings` + `audit_runs` table CRUD + their idempotent
//! schema, extracted verbatim from `storage::db` (God-file split, a PURE MOVE — no behavior change).
//! The methods below are an inherent-impl split of [`crate::storage::db::Db`] across files (Rust
//! allows one type's inherent `impl` to live in multiple files of the same crate); every method
//! retains its EXACT prior body and signature. These are RAW row reads/writes — the COMMAND layer
//! (see `crate::audit`) re-filters each finding's SOURCE/TARGET visibility against the live session
//! unlock set before returning, and every seal path purges the pending rows via the `_tx` helpers
//! below — so no per-method visibility gate lives here (the store is the staging substrate, the gate
//! is the reader on top). Shared db.rs module-level helpers `map_err` + `add_column_if_missing` are
//! `pub(crate)` for the sibling access; the schema fn `migrate_audit` moved here and is `pub(crate)`
//! so `migrate()` in db.rs still calls `Self::migrate_audit(&conn)` unchanged, and the seal-path
//! `_tx` purge fns are still called `Self::purge_*_pending_audit_findings_tx(&tx, ..)` cross-file
//! from db.rs. The row mapper `row_to_audit_finding` (only ever used by these readers) moved along.

use rusqlite::{Connection, OptionalExtension, Row};

use crate::error::Result;
use crate::storage::db::{map_err, Db};

impl Db {
    /// Vault Audit v1 — idempotent audit schema (see `crate::audit`). `audit_findings` stages one
    /// propose→accept finding per row; `evidence_md`/`accept_action`/BOTH TITLES are DERIVED
    /// PLAINTEXT that only PENDING rows may hold (resolve blanks all four; every seal path purges
    /// pending rows whose source or target seals — `purge_pending_audit_findings_tx`, the
    /// brief-runs purge class). `dedupe_key` is the stable cross-run identity and OUTLIVES
    /// resolve, so its variable part is HASHED (title-free — `crate::audit::dedupe_disc`): an
    /// existing PENDING or DISMISSED twin suppresses re-creation (dismissed = don't nag again);
    /// an ACCEPTED one may recur. Enforced in code (`insert_audit_finding_if_new`), not by a
    /// UNIQUE constraint — accepted twins must be able to coexist with a recurring pending row.
    /// `audit_runs` is content-free bookkeeping (id + timestamps + per-kind counts).
    pub(crate) fn migrate_audit(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS audit_findings (
               id TEXT PRIMARY KEY,
               kind TEXT NOT NULL,
               source_kind TEXT NOT NULL,
               source_id TEXT NOT NULL,
               source_title TEXT NOT NULL,
               target_title TEXT,
               target_id TEXT,
               evidence_md TEXT NOT NULL,
               accept_action TEXT NOT NULL DEFAULT '',
               dedupe_key TEXT NOT NULL,
               status TEXT NOT NULL DEFAULT 'pending',
               run_id TEXT NOT NULL,
               created_at INTEGER NOT NULL,
               resolved_at INTEGER
             );
             CREATE INDEX IF NOT EXISTS idx_audit_findings_status ON audit_findings(status);
             CREATE INDEX IF NOT EXISTS idx_audit_findings_dedupe ON audit_findings(dedupe_key);
             CREATE INDEX IF NOT EXISTS idx_audit_findings_source ON audit_findings(source_id);
             CREATE INDEX IF NOT EXISTS idx_audit_findings_target ON audit_findings(target_id);
             CREATE TABLE IF NOT EXISTS audit_runs (
               id TEXT PRIMARY KEY,
               started_at INTEGER NOT NULL,
               finished_at INTEGER,
               counts_json TEXT NOT NULL DEFAULT '{}'
             );",
        )
        .map_err(map_err)?;
        // `target_kind` ("meeting" | "note") rides with `target_id` so the list layer can re-gate
        // the TARGET side against the right table (lock review, same branch). Guarded for dev DBs
        // that created the table before the column existed.
        Self::add_column_if_missing(conn, "audit_findings", "target_kind", "TEXT")?;
        // Weekly schedule (Phase 3): `scheduled = 1` marks a run row staged by the WEEKLY runner —
        // both the claim row inserted BEFORE the pass (crash-safe claim-before-run, the
        // brief-runner discipline) and nothing else. Due-ness reads MAX(finished_at) over
        // scheduled rows only, so manual runs never push the weekly cadence. Additive + guarded.
        Self::add_column_if_missing(conn, "audit_runs", "scheduled", "INTEGER NOT NULL DEFAULT 0")?;
        Ok(())
    }

    // ── Vault Audit v1 (see `crate::audit`) ─────────────────────────────────────────────────────

    /// Stage one audit finding UNLESS a PENDING or DISMISSED twin (same `dedupe_key`) already
    /// exists — pending = already surfaced, dismissed = the user said "don't nag again". An
    /// ACCEPTED twin does NOT suppress (the evidence may legitimately recur later). Returns
    /// whether a row was inserted.
    pub fn insert_audit_finding_if_new(
        &self,
        f: &crate::audit::NewAuditFinding,
        run_id: &str,
        created_at: i64,
    ) -> Result<bool> {
        let conn = self.lock();
        let suppressed: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM audit_findings
                   WHERE dedupe_key = ?1 AND status IN ('pending', 'dismissed'))",
                rusqlite::params![f.dedupe_key],
                |r| r.get(0),
            )
            .map_err(map_err)?;
        if suppressed {
            return Ok(false);
        }
        conn.execute(
            "INSERT INTO audit_findings
               (id, kind, source_kind, source_id, source_title, target_title, target_id,
                target_kind, evidence_md, accept_action, dedupe_key, status, run_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'pending', ?12, ?13)",
            rusqlite::params![
                uuid::Uuid::new_v4().to_string(),
                f.kind,
                f.source_kind,
                f.source_id,
                f.source_title,
                f.target_title,
                f.target_id,
                f.target_kind,
                f.evidence_md,
                f.accept_action,
                f.dedupe_key,
                run_id,
                created_at,
            ],
        )
        .map_err(map_err)?;
        Ok(true)
    }

    /// All findings with `status`, newest first. RAW row read — the COMMAND layer defensively
    /// re-filters each row's SOURCE visibility against the live session unlock set before
    /// returning (belt-and-braces on top of purge-on-seal).
    pub fn list_audit_finding_rows(
        &self,
        status: &str,
    ) -> Result<Vec<crate::audit::AuditFindingRow>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, kind, source_kind, source_id, source_title, target_title, target_id,
                        target_kind, evidence_md, accept_action, dedupe_key, status, run_id,
                        created_at, resolved_at
                   FROM audit_findings WHERE status = ?1
                  ORDER BY created_at DESC, id ASC",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![status], row_to_audit_finding)
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// One finding by id (any status).
    pub fn get_audit_finding(&self, id: &str) -> Result<Option<crate::audit::AuditFindingRow>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT id, kind, source_kind, source_id, source_title, target_title, target_id,
                    target_kind, evidence_md, accept_action, dedupe_key, status, run_id,
                    created_at, resolved_at
               FROM audit_findings WHERE id = ?1",
            rusqlite::params![id],
            row_to_audit_finding,
        )
        .optional()
        .map_err(map_err)
    }

    /// Flip a finding to `accepted`/`dismissed` and BLANK the derived plaintext — `evidence_md`,
    /// `accept_action` AND BOTH TITLES — in the SAME statement (the brief-runs consume-on-accept
    /// posture). Titles are content material too (lock review, 2026-07-16): resolved rows survive
    /// every purge (pending-only) and the list's source re-gate, so a resolved row keeping a
    /// later-sealed target's title would serve it forever. Only PENDING rows carry ANY
    /// title/evidence material at rest; ids/kind/timestamps are all a resolved row keeps.
    pub fn resolve_audit_finding_row(
        &self,
        id: &str,
        status: &str,
        resolved_at: i64,
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE audit_findings
                SET status = ?2, resolved_at = ?3, evidence_md = '', accept_action = '',
                    source_title = '', target_title = NULL
              WHERE id = ?1",
            rusqlite::params![id, status, resolved_at],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Delete every PENDING row a given audit run staged — the end-of-pass seal-epoch
    /// reconciliation (see `crate::audit::run_audit_pass`): when the epoch is observed advanced
    /// at pass end, the whole run's staged rows are withdrawn in ONE statement, turning the
    /// residual insert-vs-seal-purge race into a no-op. Pending-only by construction (a
    /// just-staged run's rows cannot be resolved yet; the filter is belt-and-braces).
    pub fn delete_pending_audit_findings_for_run(&self, run_id: &str) -> Result<usize> {
        let conn = self.lock();
        let n = conn
            .execute(
                "DELETE FROM audit_findings WHERE run_id = ?1 AND status = 'pending'",
                rusqlite::params![run_id],
            )
            .map_err(map_err)?;
        Ok(n)
    }

    /// Count of pending findings (the summary/event payload — a count, never content).
    pub fn count_pending_audit_findings(&self) -> Result<usize> {
        let conn = self.lock();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM audit_findings WHERE status = 'pending'",
                [],
                |r| r.get(0),
            )
            .map_err(map_err)?;
        Ok(n as usize)
    }

    /// Record one audit run (content-free bookkeeping: id + timestamps + per-kind counts JSON).
    pub fn insert_audit_run(
        &self,
        id: &str,
        started_at: i64,
        finished_at: i64,
        counts_json: &str,
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO audit_runs (id, started_at, finished_at, counts_json)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![id, started_at, finished_at, counts_json],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Weekly runner CLAIM row — inserted BEFORE the scheduled pass runs (the brief runner's
    /// claim-before-run discipline): once this row exists, `weekly_due` holds for the next 7 days
    /// even if the pass itself crashes/fails, so a persistently-failing pass can never become an
    /// hourly storm. Content-free (`scheduled = 1`, empty counts); the pass still records its own
    /// normal (unscheduled) bookkeeping row on completion.
    pub fn insert_scheduled_audit_run_claim(&self, id: &str, claimed_at: i64) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO audit_runs (id, started_at, finished_at, counts_json, scheduled)
             VALUES (?1, ?2, ?2, '{}', 1)",
            rusqlite::params![id, claimed_at],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// `finished_at` of the newest SCHEDULED audit run (the weekly claim rows) — the due-ness
    /// anchor. Manual runs (`scheduled = 0`) never count, so running the audit by hand does not
    /// push the weekly cadence.
    pub fn last_scheduled_audit_run_finished_at(&self) -> Result<Option<i64>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT MAX(finished_at) FROM audit_runs WHERE scheduled = 1",
            [],
            |r| r.get::<_, Option<i64>>(0),
        )
        .map_err(map_err)
    }

    /// Judge-tier DEMOTE: delete ONE pending finding outright (NOT dismiss — a dismissed row's
    /// `dedupe_key` suppresses re-creation forever; deletion lets a real issue re-stage on the
    /// next pass). Pending-only by construction: a row the user resolved (or a seal purged)
    /// between the judge's read and this delete is left alone. Returns whether a row was deleted.
    pub fn delete_pending_audit_finding(&self, id: &str) -> Result<bool> {
        let conn = self.lock();
        let n = conn
            .execute(
                "DELETE FROM audit_findings WHERE id = ?1 AND status = 'pending'",
                rusqlite::params![id],
            )
            .map_err(map_err)?;
        Ok(n > 0)
    }

    /// Vault Audit DELETE-SAFETY: delete every PENDING `audit_findings` row whose SOURCE or
    /// TARGET is any of `ids`, within an EXISTING transaction. This precise id-matched purge is
    /// for the DELETE paths ONLY (`delete_meeting` / `delete_document`) — a delete invalidates
    /// nothing else, so unrelated findings survive. SEAL paths use
    /// [`Db::purge_all_pending_audit_findings_tx`] instead (a pending finding may cite
    /// THIRD-PARTY titles no id can match). RESOLVED rows are left alone (blanked on resolve —
    /// ids + kind only).
    pub(crate) fn purge_pending_audit_findings_tx(
        tx: &rusqlite::Transaction<'_>,
        ids: &[String],
    ) -> Result<()> {
        for id in ids {
            tx.execute(
                "DELETE FROM audit_findings
                  WHERE status = 'pending' AND (source_id = ?1 OR target_id = ?1)",
                rusqlite::params![id],
            )
            .map_err(map_err)?;
        }
        Ok(())
    }

    /// Vault Audit LOCK-SAFETY (adversarial HIGH, 2026-07-16): on ANY lock-surface mutation
    /// (seal, relock, startup reconcile, discard, move-into-locked), purge ALL pending findings —
    /// the memory-rollups posture, not the per-id brief-runs one. A pending finding's
    /// `evidence_md` may cite THIRD-PARTY titles (a stale finding's `see [[superseding note]]`,
    /// an orphan's suggested `[[titles]]`) carried with `target_id = NULL`, which an id-matched
    /// purge can never cover — and a seal anywhere invalidates the pass's whole visibility
    /// snapshot. Findings are cheap re-derivable rows: the next manual run re-stages everything
    /// still true over the post-seal corpus. RESOLVED rows survive (blanked on resolve).
    pub(crate) fn purge_all_pending_audit_findings_tx(
        tx: &rusqlite::Transaction<'_>,
    ) -> Result<()> {
        tx.execute("DELETE FROM audit_findings WHERE status = 'pending'", [])
            .map_err(map_err)?;
        Ok(())
    }

    /// Standalone-transaction wrapper of [`Db::purge_all_pending_audit_findings_tx`] for the
    /// lock-surface call sites with no open transaction of their own (the note move-into-locked
    /// seal in `move_note_doc_inner`).
    pub fn purge_all_pending_audit_findings(&self) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        Self::purge_all_pending_audit_findings_tx(&tx)?;
        tx.commit().map_err(map_err)?;
        Ok(())
    }
}

fn row_to_audit_finding(row: &Row<'_>) -> rusqlite::Result<crate::audit::AuditFindingRow> {
    Ok(crate::audit::AuditFindingRow {
        id: row.get(0)?,
        kind: row.get(1)?,
        source_kind: row.get(2)?,
        source_id: row.get(3)?,
        source_title: row.get(4)?,
        target_title: row.get(5)?,
        target_id: row.get(6)?,
        target_kind: row.get(7)?,
        evidence_md: row.get(8)?,
        accept_action: row.get(9)?,
        dedupe_key: row.get(10)?,
        status: row.get(11)?,
        run_id: row.get(12)?,
        created_at: row.get(13)?,
        resolved_at: row.get(14)?,
    })
}
