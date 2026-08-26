//! Canonical SQLCipher-backed Murmur reminder store.
//!
//! Durable reminders are user-owned data and deliberately survive source lock/relock. Their source
//! anchors contain opaque ids only; command-layer title resolution is live-gated. The audit cache
//! and pending Smart suggestions are a separate DERIVED plaintext domain; unkeyed decision hashes
//! are purged with it on seal/relock/startup. Additive SQL triggers invalidate the plaintext rows
//! in the same transaction as every canonical meeting/note content insert, edit, seal-blank, or
//! delete.

use chrono::{Datelike, Local, NaiveDate, NaiveDateTime, TimeZone, Timelike};
use rusqlite::{Connection, OptionalExtension, Row, Transaction};
use std::borrow::Cow;

use crate::error::{AppError, Result};
use crate::reminder_audit::ReminderAuditCandidate;
use crate::storage::db::{map_err, meeting_visibility_clause, visibility_clause, Db};
use crate::storage::models::{
    ReminderDraft, ReminderOrigin, ReminderRepeatUnit, ReminderSourceAnchor, ReminderState,
    ReminderSuggestionGateAnchor, StoredReminder, StoredReminderOccurrence,
    StoredReminderSuggestion,
};
use crate::transcribe::types::Segment;

const MAX_PENDING_SUGGESTIONS: usize = 32;
const MAX_REMINDER_TITLE_CHARS: usize = 240;
const MAX_SOURCE_ID_CHARS: usize = 160;
const MAX_ENGINE_ID_CHARS: usize = 160;
const MAX_SOURCE_INVALIDATION_DRAIN: usize = 256;
const MAX_LOCAL_DATETIME_GAP_MINUTES: usize = 48 * 60;
pub(crate) const MIN_REMINDER_DUE_AT: i64 = 946_684_800_000; // 2000-01-01T00:00:00Z
pub(crate) const MAX_REMINDER_DUE_AT: i64 = 7_258_118_400_000; // 2200-01-01T00:00:00Z

/// Content-free Smart-card invalidation claimed from the durable SQLCipher queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReminderSourceInvalidation {
    pub kind: String,
    pub id: String,
    pub revision: i64,
}

/// Exact one-shot capability and user-edited draft promoted by the Smart Reminder accept flow.
/// Grouping the CAS anchors keeps callers from swapping or omitting one of the source bindings.
pub(crate) struct ReminderSuggestionPromotion<'a> {
    pub suggestion_id: &'a str,
    pub expected_source_kind: &'a str,
    pub expected_source_id: &'a str,
    pub expected_content_hash: &'a str,
    pub reminder_id: &'a str,
    pub draft: &'a ReminderDraft,
    pub now: i64,
}

type ReminderOccurrenceScheduleRow = (String, i64, i64, Option<i64>, Option<String>, String);

/// Exact canonical Smart-audit fingerprint shared by the gated command read and the transactional
/// storage CAS. Every meaning-bearing segment field is framed explicitly; floating-point values
/// use their stored IEEE bit pattern, preserving `None` vs zero and avoiding formatter drift.
pub(crate) fn canonical_reminder_source_hash(
    title: &str,
    markdown: &str,
    manual_notes: Option<&str>,
    segments: &[Segment],
) -> String {
    let mut parts = Vec::<Cow<'_, str>>::with_capacity(6 + segments.len() * 13);
    parts.extend([
        Cow::Borrowed("source-title"),
        Cow::Borrowed(title),
        Cow::Borrowed("markdown"),
        Cow::Borrowed(markdown),
        Cow::Borrowed("manual-notes"),
        Cow::Borrowed(manual_notes.unwrap_or("")),
    ]);
    for segment in segments {
        parts.extend([
            Cow::Borrowed("segment-idx"),
            Cow::Owned(segment.idx.to_string()),
            Cow::Borrowed("segment-start-bits"),
            Cow::Owned(format!("{:016x}", segment.start_s.to_bits())),
            Cow::Borrowed("segment-end-bits"),
            Cow::Owned(format!("{:016x}", segment.end_s.to_bits())),
            Cow::Borrowed("segment-text"),
            Cow::Borrowed(segment.text.as_str()),
            Cow::Borrowed("segment-speaker"),
            Cow::Borrowed(segment.speaker.as_deref().unwrap_or("")),
            Cow::Borrowed(if segment.speaker.is_some() {
                "speaker-present"
            } else {
                "speaker-absent"
            }),
            Cow::Borrowed("segment-confidence"),
            Cow::Owned(
                segment
                    .confidence
                    .map(|value| format!("present:{:08x}", value.to_bits()))
                    .unwrap_or_else(|| "absent".into()),
            ),
        ]);
    }
    crate::reminder_audit::content_hash(&parts.iter().map(|part| part.as_ref()).collect::<Vec<_>>())
}

impl Db {
    /// Additive, idempotent schema for durable reminders, source anchors, due occurrences, and the
    /// derived Smart-audit cache/pending domain.
    pub(crate) fn migrate_reminders(conn: &Connection) -> Result<()> {
        // Keep the outbox table/column migration ahead of trigger creation. A development database
        // may already have the first two-column draft; adding the revision is additive and lets the
        // companion triggers below compile against both fresh and previously-opened databases.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS reminder_source_invalidation_queue (
               source_kind TEXT NOT NULL,
               source_id   TEXT NOT NULL,
               revision    INTEGER NOT NULL DEFAULT 1,
               PRIMARY KEY (source_kind, source_id),
               CHECK (source_kind IN ('meeting','note'))
             );",
        )
        .map_err(map_err)?;
        Self::add_column_if_missing(
            conn,
            "reminder_source_invalidation_queue",
            "revision",
            "INTEGER NOT NULL DEFAULT 1",
        )?;
        // Pre-SQLCipher-v7 databases used the original one-note/three-column transcript shape:
        // `notes(meeting_id,markdown)` and `segments(meeting_id,idx,text)`. Core migration already
        // adds every other field referenced by the reminder triggers, but these four canonical
        // projection columns predate its guarded-ALTER list. Add deterministic legacy defaults
        // BEFORE creating any trigger so both NEW/OLD aliases are valid on first open and every
        // database receives the same full invalidation trigger set (never a reduced compatibility
        // trigger that could persist after a later upgrade).
        Self::add_column_if_missing(
            conn,
            "notes",
            "provider_id",
            "TEXT NOT NULL DEFAULT 'legacy'",
        )?;
        Self::add_column_if_missing(conn, "notes", "created_at", "TEXT NOT NULL DEFAULT ''")?;
        Self::add_column_if_missing(conn, "segments", "start_s", "REAL NOT NULL DEFAULT 0")?;
        Self::add_column_if_missing(conn, "segments", "end_s", "REAL NOT NULL DEFAULT 0")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS reminders (
               id            TEXT PRIMARY KEY,
               title         TEXT NOT NULL,
               details       TEXT,
               due_at        INTEGER NOT NULL,
               repeat_every  INTEGER,
               repeat_unit   TEXT,
               state         TEXT NOT NULL DEFAULT 'active',
               origin        TEXT NOT NULL DEFAULT 'manual',
               created_at    INTEGER NOT NULL,
               updated_at    INTEGER NOT NULL,
               completed_at  INTEGER,
               CHECK (length(title) BETWEEN 1 AND 240),
               CHECK (details IS NULL OR length(details) <= 4000),
               CHECK (repeat_every IS NULL OR repeat_every BETWEEN 1 AND 365),
               CHECK ((repeat_every IS NULL) = (repeat_unit IS NULL)),
               CHECK (repeat_unit IS NULL OR repeat_unit IN ('days','weeks','months','years')),
               CHECK (state IN ('active','completed')),
               CHECK (origin IN ('manual','smart'))
             );
             CREATE INDEX IF NOT EXISTS idx_reminders_state_due
               ON reminders(state, due_at, id);

             CREATE TABLE IF NOT EXISTS reminder_sources (
               reminder_id TEXT NOT NULL,
               source_kind TEXT NOT NULL,
               source_id   TEXT NOT NULL,
               PRIMARY KEY (reminder_id, source_kind, source_id),
               FOREIGN KEY (reminder_id) REFERENCES reminders(id) ON DELETE CASCADE,
               CHECK (source_kind IN ('meeting','note'))
             );
             CREATE INDEX IF NOT EXISTS idx_reminder_sources_source
               ON reminder_sources(source_kind, source_id);

             CREATE TABLE IF NOT EXISTS reminder_due_occurrences (
               id           TEXT PRIMARY KEY,
               reminder_id  TEXT NOT NULL,
               due_at       INTEGER NOT NULL,
               status       TEXT NOT NULL DEFAULT 'unread',
               created_at   INTEGER NOT NULL,
               resolved_at  INTEGER,
               UNIQUE (reminder_id, due_at),
               FOREIGN KEY (reminder_id) REFERENCES reminders(id) ON DELETE CASCADE,
               CHECK (status IN ('unread','dismissed','completed'))
             );
             CREATE INDEX IF NOT EXISTS idx_reminder_occurrences_status_due
               ON reminder_due_occurrences(status, due_at, id);

             -- Derived Smart-audit state. These tables are populated only after the audit's
             -- post-inference re-gate/hash check. Their plaintext is disposable source-derived
             -- content, unlike the independent durable reminder domain above.
             CREATE TABLE IF NOT EXISTS reminder_audit_cache (
               source_kind  TEXT NOT NULL,
               source_id    TEXT NOT NULL,
               content_hash TEXT NOT NULL,
               engine_id    TEXT NOT NULL,
               audited_at   INTEGER NOT NULL,
               PRIMARY KEY (source_kind, source_id),
               CHECK (source_kind IN ('meeting','note'))
             );
             CREATE TABLE IF NOT EXISTS reminder_pending_suggestions (
               id               TEXT PRIMARY KEY,
               source_kind      TEXT NOT NULL,
               source_id        TEXT NOT NULL,
               content_hash     TEXT NOT NULL,
               engine_id        TEXT NOT NULL,
               candidate_key    TEXT NOT NULL,
               title            TEXT NOT NULL,
               suggested_due_at INTEGER,
               created_at       INTEGER NOT NULL,
               UNIQUE (source_kind, source_id, content_hash, candidate_key),
               CHECK (source_kind IN ('meeting','note')),
               CHECK (length(title) BETWEEN 1 AND 240)
             );
             CREATE INDEX IF NOT EXISTS idx_reminder_suggestions_source
               ON reminder_pending_suggestions(source_kind, source_id);

             -- Session-durable, plaintext-free user decisions. Engine identity is deliberately
             -- absent, but the unkeyed source/candidate hashes are purged on seal/relock/startup
             -- with the derived audit domain.
             CREATE TABLE IF NOT EXISTS reminder_suggestion_decisions (
               source_kind  TEXT NOT NULL,
               source_id    TEXT NOT NULL,
               content_hash TEXT NOT NULL,
               candidate_key TEXT NOT NULL,
               decision     TEXT NOT NULL,
               reminder_id  TEXT,
               decided_at   INTEGER NOT NULL,
               PRIMARY KEY (source_kind, source_id, content_hash, candidate_key),
               FOREIGN KEY (reminder_id) REFERENCES reminders(id) ON DELETE CASCADE,
               CHECK (source_kind IN ('meeting','note')),
               CHECK (decision IN ('accepted','dismissed')),
               CHECK ((decision = 'accepted') = (reminder_id IS NOT NULL))
             );
             CREATE INDEX IF NOT EXISTS idx_reminder_decisions_reminder
               ON reminder_suggestion_decisions(reminder_id);

             -- Durable content-free bridge from canonical DB mutations to the FE. Triggers enqueue
             -- only the source discriminator + opaque id; repeated edits coalesce until claimed.
             CREATE TABLE IF NOT EXISTS reminder_source_invalidation_queue (
               source_kind TEXT NOT NULL,
               source_id   TEXT NOT NULL,
               revision    INTEGER NOT NULL DEFAULT 1,
               PRIMARY KEY (source_kind, source_id),
               CHECK (source_kind IN ('meeting','note'))
             );

             -- MEETING NOTE changes (including seal/relock blanking).
             CREATE TRIGGER IF NOT EXISTS reminder_derived_notes_ai
             AFTER INSERT ON notes BEGIN
               DELETE FROM reminder_pending_suggestions
                WHERE source_kind = 'meeting' AND source_id = new.meeting_id;
               DELETE FROM reminder_audit_cache
                WHERE source_kind = 'meeting' AND source_id = new.meeting_id;
             END;
             CREATE TRIGGER IF NOT EXISTS reminder_derived_notes_au
             AFTER UPDATE OF meeting_id, provider_id, markdown, created_at, folder_id ON notes
             WHEN old.meeting_id IS NOT new.meeting_id
               OR old.provider_id IS NOT new.provider_id
               OR old.markdown IS NOT new.markdown
               OR old.created_at IS NOT new.created_at
               OR old.folder_id IS NOT new.folder_id BEGIN
               DELETE FROM reminder_pending_suggestions
                WHERE source_kind = 'meeting' AND source_id = new.meeting_id;
               DELETE FROM reminder_audit_cache
                WHERE source_kind = 'meeting' AND source_id = new.meeting_id;
               DELETE FROM reminder_pending_suggestions
                WHERE source_kind = 'meeting' AND source_id = old.meeting_id;
               DELETE FROM reminder_audit_cache
                WHERE source_kind = 'meeting' AND source_id = old.meeting_id;
             END;
             -- Additive companion for databases that already created the earlier markdown-only
             -- trigger: latest-note selection and lock ownership also change on these fields.
             CREATE TRIGGER IF NOT EXISTS reminder_derived_notes_projection_au
             AFTER UPDATE OF meeting_id, provider_id, created_at, folder_id ON notes
             WHEN old.meeting_id IS NOT new.meeting_id
               OR old.provider_id IS NOT new.provider_id
               OR old.created_at IS NOT new.created_at
               OR old.folder_id IS NOT new.folder_id BEGIN
               DELETE FROM reminder_pending_suggestions
                WHERE source_kind = 'meeting' AND source_id = new.meeting_id;
               DELETE FROM reminder_audit_cache
                WHERE source_kind = 'meeting' AND source_id = new.meeting_id;
               DELETE FROM reminder_pending_suggestions
                WHERE source_kind = 'meeting' AND source_id = old.meeting_id;
               DELETE FROM reminder_audit_cache
                WHERE source_kind = 'meeting' AND source_id = old.meeting_id;
             END;
             CREATE TRIGGER IF NOT EXISTS reminder_derived_notes_ad
             AFTER DELETE ON notes BEGIN
               DELETE FROM reminder_pending_suggestions
                WHERE source_kind = 'meeting' AND source_id = old.meeting_id;
               DELETE FROM reminder_audit_cache
                WHERE source_kind = 'meeting' AND source_id = old.meeting_id;
             END;

             -- Transcript changes are canonical meeting-content changes too. The triggers are
             -- cheap no-ops unless this source currently has a pending audit.
             CREATE TRIGGER IF NOT EXISTS reminder_derived_segments_ai
             AFTER INSERT ON segments BEGIN
               DELETE FROM reminder_pending_suggestions
                WHERE source_kind = 'meeting' AND source_id = new.meeting_id;
               DELETE FROM reminder_audit_cache
                WHERE source_kind = 'meeting' AND source_id = new.meeting_id;
             END;
             CREATE TRIGGER IF NOT EXISTS reminder_derived_segments_au
             AFTER UPDATE OF meeting_id, idx, start_s, end_s, text, speaker, confidence ON segments
             WHEN old.meeting_id IS NOT new.meeting_id
               OR old.idx IS NOT new.idx
               OR old.start_s IS NOT new.start_s
               OR old.end_s IS NOT new.end_s
               OR old.text IS NOT new.text
               OR old.speaker IS NOT new.speaker
               OR old.confidence IS NOT new.confidence BEGIN
               DELETE FROM reminder_pending_suggestions
                WHERE source_kind = 'meeting' AND source_id = new.meeting_id;
               DELETE FROM reminder_audit_cache
                WHERE source_kind = 'meeting' AND source_id = new.meeting_id;
               DELETE FROM reminder_pending_suggestions
                WHERE source_kind = 'meeting' AND source_id = old.meeting_id;
               DELETE FROM reminder_audit_cache
                WHERE source_kind = 'meeting' AND source_id = old.meeting_id;
             END;
             -- Additive companion for databases that already created the earlier text-only
             -- trigger: every other visible segment field is part of the canonical fingerprint.
             CREATE TRIGGER IF NOT EXISTS reminder_derived_segments_projection_au
             AFTER UPDATE OF meeting_id, idx, start_s, end_s, speaker, confidence ON segments
             WHEN old.meeting_id IS NOT new.meeting_id
               OR old.idx IS NOT new.idx
               OR old.start_s IS NOT new.start_s
               OR old.end_s IS NOT new.end_s
               OR old.speaker IS NOT new.speaker
               OR old.confidence IS NOT new.confidence BEGIN
               DELETE FROM reminder_pending_suggestions
                WHERE source_kind = 'meeting' AND source_id = new.meeting_id;
               DELETE FROM reminder_audit_cache
                WHERE source_kind = 'meeting' AND source_id = new.meeting_id;
               DELETE FROM reminder_pending_suggestions
                WHERE source_kind = 'meeting' AND source_id = old.meeting_id;
               DELETE FROM reminder_audit_cache
                WHERE source_kind = 'meeting' AND source_id = old.meeting_id;
             END;
             -- Presentation-only echo provenance changes membership in the canonical merged
             -- transcript used by Smart audit. Keep this as a dedicated additive trigger so a
             -- database first opened by an earlier build also gains the invalidation rule.
             CREATE TRIGGER IF NOT EXISTS reminder_derived_segments_echo_au
             AFTER UPDATE OF echo_suppressed ON segments
             WHEN old.echo_suppressed IS NOT new.echo_suppressed BEGIN
               DELETE FROM reminder_pending_suggestions
                WHERE source_kind = 'meeting' AND source_id = new.meeting_id;
               DELETE FROM reminder_audit_cache
                WHERE source_kind = 'meeting' AND source_id = new.meeting_id;
             END;
             CREATE TRIGGER IF NOT EXISTS reminder_derived_segments_ad
             AFTER DELETE ON segments BEGIN
               DELETE FROM reminder_pending_suggestions
                WHERE source_kind = 'meeting' AND source_id = old.meeting_id;
               DELETE FROM reminder_audit_cache
                WHERE source_kind = 'meeting' AND source_id = old.meeting_id;
             END;

             -- Manual meeting notes/title changes and meeting deletion.
             CREATE TRIGGER IF NOT EXISTS reminder_derived_meetings_au
             AFTER UPDATE OF manual_notes, title ON meetings
             WHEN old.manual_notes IS NOT new.manual_notes
               OR old.title IS NOT new.title BEGIN
               DELETE FROM reminder_pending_suggestions
                WHERE source_kind = 'meeting' AND source_id = new.id;
               DELETE FROM reminder_audit_cache
                WHERE source_kind = 'meeting' AND source_id = new.id;
             END;
             CREATE TRIGGER IF NOT EXISTS reminder_derived_meetings_ad
             AFTER DELETE ON meetings BEGIN
               DELETE FROM reminder_pending_suggestions
                WHERE source_kind = 'meeting' AND source_id = old.id;
               DELETE FROM reminder_audit_cache
                WHERE source_kind = 'meeting' AND source_id = old.id;
               DELETE FROM reminder_sources
                WHERE source_kind = 'meeting' AND source_id = old.id;
               DELETE FROM reminder_suggestion_decisions
                WHERE source_kind = 'meeting' AND source_id = old.id;
             END;

             -- Authored-note edits, seal/relock blanking, and deletion.
             CREATE TRIGGER IF NOT EXISTS reminder_derived_documents_ai
             AFTER INSERT ON documents WHEN new.kind = 'note' BEGIN
               DELETE FROM reminder_pending_suggestions
                WHERE source_kind = 'note' AND source_id = new.id;
               DELETE FROM reminder_audit_cache
                WHERE source_kind = 'note' AND source_id = new.id;
             END;
             CREATE TRIGGER IF NOT EXISTS reminder_derived_documents_au
             AFTER UPDATE OF text, title, name, kind, folder_id ON documents
             WHEN (old.kind = 'note' OR new.kind = 'note')
               AND (old.text IS NOT new.text
                    OR old.title IS NOT new.title
                    OR old.name IS NOT new.name
                    OR old.folder_id IS NOT new.folder_id
                    OR old.kind IS NOT new.kind) BEGIN
               DELETE FROM reminder_pending_suggestions
                WHERE source_kind = 'note' AND source_id = new.id;
               DELETE FROM reminder_audit_cache
                WHERE source_kind = 'note' AND source_id = new.id;
               DELETE FROM reminder_sources
                WHERE source_kind = 'note' AND source_id = new.id
                  AND new.kind != 'note';
               DELETE FROM reminder_suggestion_decisions
                WHERE source_kind = 'note' AND source_id = new.id
                  AND new.kind != 'note';
             END;
             -- Additive companion for databases that already created the earlier trigger:
             -- fallback display names are hashed, and folder moves change lock visibility.
             CREATE TRIGGER IF NOT EXISTS reminder_derived_documents_anchor_au
             AFTER UPDATE OF name, folder_id ON documents
             WHEN (old.kind = 'note' OR new.kind = 'note')
               AND (old.name IS NOT new.name OR old.folder_id IS NOT new.folder_id) BEGIN
               DELETE FROM reminder_pending_suggestions
                WHERE source_kind = 'note' AND source_id = new.id;
               DELETE FROM reminder_audit_cache
                WHERE source_kind = 'note' AND source_id = new.id;
             END;
             CREATE TRIGGER IF NOT EXISTS reminder_derived_documents_ad
             AFTER DELETE ON documents WHEN old.kind = 'note' BEGIN
               DELETE FROM reminder_pending_suggestions
                WHERE source_kind = 'note' AND source_id = old.id;
               DELETE FROM reminder_audit_cache
                WHERE source_kind = 'note' AND source_id = old.id;
               DELETE FROM reminder_sources
                WHERE source_kind = 'note' AND source_id = old.id;
               DELETE FROM reminder_suggestion_decisions
                WHERE source_kind = 'note' AND source_id = old.id;
             END;

             -- Content-free Smart-card refresh queue. These are NEW companion triggers, so an
             -- existing database gains live invalidation without replacing any prior purge trigger.
             CREATE TRIGGER IF NOT EXISTS reminder_source_queue_notes_ai
             AFTER INSERT ON notes BEGIN
               INSERT INTO reminder_source_invalidation_queue(source_kind,source_id,revision)
               VALUES ('meeting',new.meeting_id,1)
               ON CONFLICT(source_kind,source_id) DO UPDATE SET revision=revision+1;
             END;
             CREATE TRIGGER IF NOT EXISTS reminder_source_queue_notes_au
             AFTER UPDATE OF meeting_id, provider_id, markdown, created_at, folder_id ON notes
             WHEN old.meeting_id IS NOT new.meeting_id
               OR old.provider_id IS NOT new.provider_id
               OR old.markdown IS NOT new.markdown
               OR old.created_at IS NOT new.created_at
               OR old.folder_id IS NOT new.folder_id BEGIN
               INSERT INTO reminder_source_invalidation_queue(source_kind,source_id,revision)
               VALUES ('meeting',old.meeting_id,1)
               ON CONFLICT(source_kind,source_id) DO UPDATE SET revision=revision+1;
               INSERT INTO reminder_source_invalidation_queue(source_kind,source_id,revision)
               VALUES ('meeting',new.meeting_id,1)
               ON CONFLICT(source_kind,source_id) DO UPDATE SET revision=revision+1;
             END;
             CREATE TRIGGER IF NOT EXISTS reminder_source_queue_notes_ad
             AFTER DELETE ON notes BEGIN
               INSERT INTO reminder_source_invalidation_queue(source_kind,source_id,revision)
               VALUES ('meeting',old.meeting_id,1)
               ON CONFLICT(source_kind,source_id) DO UPDATE SET revision=revision+1;
             END;

             CREATE TRIGGER IF NOT EXISTS reminder_source_queue_segments_ai
             AFTER INSERT ON segments BEGIN
               INSERT INTO reminder_source_invalidation_queue(source_kind,source_id,revision)
               VALUES ('meeting',new.meeting_id,1)
               ON CONFLICT(source_kind,source_id) DO UPDATE SET revision=revision+1;
             END;
             CREATE TRIGGER IF NOT EXISTS reminder_source_queue_segments_au
             AFTER UPDATE OF meeting_id, idx, start_s, end_s, text, speaker, confidence,
                             echo_suppressed ON segments
             WHEN old.meeting_id IS NOT new.meeting_id
               OR old.idx IS NOT new.idx
               OR old.start_s IS NOT new.start_s
               OR old.end_s IS NOT new.end_s
               OR old.text IS NOT new.text
               OR old.speaker IS NOT new.speaker
               OR old.confidence IS NOT new.confidence
               OR old.echo_suppressed IS NOT new.echo_suppressed BEGIN
               INSERT INTO reminder_source_invalidation_queue(source_kind,source_id,revision)
               VALUES ('meeting',old.meeting_id,1)
               ON CONFLICT(source_kind,source_id) DO UPDATE SET revision=revision+1;
               INSERT INTO reminder_source_invalidation_queue(source_kind,source_id,revision)
               VALUES ('meeting',new.meeting_id,1)
               ON CONFLICT(source_kind,source_id) DO UPDATE SET revision=revision+1;
             END;
             CREATE TRIGGER IF NOT EXISTS reminder_source_queue_segments_ad
             AFTER DELETE ON segments BEGIN
               INSERT INTO reminder_source_invalidation_queue(source_kind,source_id,revision)
               VALUES ('meeting',old.meeting_id,1)
               ON CONFLICT(source_kind,source_id) DO UPDATE SET revision=revision+1;
             END;

             CREATE TRIGGER IF NOT EXISTS reminder_source_queue_meetings_au
             AFTER UPDATE OF manual_notes, title ON meetings
             WHEN old.manual_notes IS NOT new.manual_notes
               OR old.title IS NOT new.title BEGIN
               INSERT INTO reminder_source_invalidation_queue(source_kind,source_id,revision)
               VALUES ('meeting',new.id,1)
               ON CONFLICT(source_kind,source_id) DO UPDATE SET revision=revision+1;
             END;
             CREATE TRIGGER IF NOT EXISTS reminder_source_queue_meetings_ad
             AFTER DELETE ON meetings BEGIN
               INSERT INTO reminder_source_invalidation_queue(source_kind,source_id,revision)
               VALUES ('meeting',old.id,1)
               ON CONFLICT(source_kind,source_id) DO UPDATE SET revision=revision+1;
             END;

             CREATE TRIGGER IF NOT EXISTS reminder_source_queue_documents_ai
             AFTER INSERT ON documents WHEN new.kind = 'note' BEGIN
               INSERT INTO reminder_source_invalidation_queue(source_kind,source_id,revision)
               VALUES ('note',new.id,1)
               ON CONFLICT(source_kind,source_id) DO UPDATE SET revision=revision+1;
             END;
             CREATE TRIGGER IF NOT EXISTS reminder_source_queue_documents_au
             AFTER UPDATE OF text, title, name, kind, folder_id ON documents
             WHEN (old.kind = 'note' OR new.kind = 'note')
               AND (old.text IS NOT new.text
                    OR old.title IS NOT new.title
                    OR old.name IS NOT new.name
                    OR old.kind IS NOT new.kind
                    OR old.folder_id IS NOT new.folder_id) BEGIN
               INSERT INTO reminder_source_invalidation_queue(source_kind,source_id,revision)
               VALUES ('note',new.id,1)
               ON CONFLICT(source_kind,source_id) DO UPDATE SET revision=revision+1;
             END;
             CREATE TRIGGER IF NOT EXISTS reminder_source_queue_documents_ad
             AFTER DELETE ON documents WHEN old.kind = 'note' BEGIN
               INSERT INTO reminder_source_invalidation_queue(source_kind,source_id,revision)
               VALUES ('note',old.id,1)
               ON CONFLICT(source_kind,source_id) DO UPDATE SET revision=revision+1;
             END;",
        )
        .map_err(map_err)
    }

    /// Peek a bounded batch of content-free Smart-card invalidations without deleting it. Rows stay
    /// durable across process crashes and emit failures; the worker CAS-acknowledges only after a
    /// successful event delivery.
    pub(crate) fn peek_reminder_source_invalidations(
        &self,
        limit: usize,
    ) -> Result<Vec<ReminderSourceInvalidation>> {
        let limit = limit.min(MAX_SOURCE_INVALIDATION_DRAIN);
        if limit == 0 {
            return Ok(Vec::new());
        }
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT source_kind,source_id,revision
                   FROM reminder_source_invalidation_queue
                  ORDER BY source_kind,source_id
                  LIMIT ?1",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![limit as i64], |row| {
                Ok(ReminderSourceInvalidation {
                    kind: row.get(0)?,
                    id: row.get(1)?,
                    revision: row.get(2)?,
                })
            })
            .map_err(map_err)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(map_err)
    }

    /// Delete exactly the revision that was successfully emitted. If a canonical mutation raced
    /// the emit and incremented the row, the CAS fails and the newer invalidation remains queued.
    pub(crate) fn ack_reminder_source_invalidation(
        &self,
        invalidation: &ReminderSourceInvalidation,
    ) -> Result<bool> {
        let conn = self.lock();
        conn.execute(
            "DELETE FROM reminder_source_invalidation_queue
              WHERE source_kind=?1 AND source_id=?2 AND revision=?3",
            rusqlite::params![&invalidation.kind, &invalidation.id, invalidation.revision],
        )
        .map(|changed| changed != 0)
        .map_err(map_err)
    }

    pub fn reminder_audit_cache_matches(
        &self,
        source_kind: &str,
        source_id: &str,
        content_hash: &str,
        engine_id: &str,
    ) -> Result<bool> {
        validate_audit_identity(source_kind, source_id, content_hash, engine_id)?;
        let conn = self.lock();
        conn.query_row(
            "SELECT EXISTS(
               SELECT 1 FROM reminder_audit_cache
                WHERE source_kind=?1 AND source_id=?2 AND content_hash=?3 AND engine_id=?4
             )",
            rusqlite::params![source_kind, source_id, content_hash, engine_id],
            |row| row.get(0),
        )
        .map_err(map_err)
    }

    /// Deterministic canonical note projection for Smart meeting audits. This intentionally lives
    /// beside the transactional CAS query: both order by provider as a stable tie-break after the
    /// persisted timestamp. Callers must pass the meeting visibility gate before using the text.
    pub(crate) fn latest_reminder_audit_markdown(
        &self,
        meeting_id: &str,
    ) -> Result<Option<String>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT markdown FROM notes
              WHERE meeting_id=?1
              ORDER BY created_at DESC, provider_id DESC
              LIMIT 1",
            rusqlite::params![meeting_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(map_err)
    }

    /// Return only rows from the caller's current source revision and engine, capped independently
    /// of caller input. Requiring the matching cache row makes orphaned/stale candidate plaintext
    /// fail closed.
    pub fn list_pending_reminder_suggestions(
        &self,
        source_kind: &str,
        source_id: &str,
        content_hash: &str,
        engine_id: &str,
        limit: usize,
    ) -> Result<Vec<StoredReminderSuggestion>> {
        validate_audit_identity(source_kind, source_id, content_hash, engine_id)?;
        let limit = limit.min(MAX_PENDING_SUGGESTIONS);
        if limit == 0 {
            return Ok(Vec::new());
        }
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT s.id,s.source_kind,s.source_id,s.content_hash,s.engine_id,
                        s.candidate_key,s.title,s.suggested_due_at,s.created_at
                   FROM reminder_pending_suggestions s
                  WHERE s.source_kind=?1 AND s.source_id=?2
                    AND s.content_hash=?3 AND s.engine_id=?4
                    AND EXISTS (
                      SELECT 1 FROM reminder_audit_cache c
                       WHERE c.source_kind=s.source_kind AND c.source_id=s.source_id
                         AND c.content_hash=s.content_hash AND c.engine_id=s.engine_id
                    )
                  ORDER BY s.created_at ASC, s.id ASC
                  LIMIT ?5",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(
                rusqlite::params![
                    source_kind,
                    source_id,
                    content_hash,
                    engine_id,
                    limit as i64
                ],
                stored_suggestion_from_row,
            )
            .map_err(map_err)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(map_err)
    }

    /// Content-free pre-gate lookup for accept/dismiss. The suggestion title and due time are
    /// deliberately absent, so a locked source cannot cause derived plaintext to enter the command
    /// process merely to discover which lifecycle gate governs it.
    pub fn get_pending_reminder_suggestion_gate_anchor(
        &self,
        id: &str,
    ) -> Result<Option<ReminderSuggestionGateAnchor>> {
        if id.is_empty() || id.chars().count() > 96 {
            return Err(AppError::InvalidArg(
                "reminder suggestion id is invalid".into(),
            ));
        }
        let conn = self.lock();
        conn.query_row(
            "SELECT id,source_kind,source_id
               FROM reminder_pending_suggestions WHERE id=?1",
            rusqlite::params![id],
            |row| {
                Ok(ReminderSuggestionGateAnchor {
                    id: row.get(0)?,
                    source_kind: row.get(1)?,
                    source_id: row.get(2)?,
                })
            },
        )
        .optional()
        .map_err(map_err)
    }

    /// Atomically replace one source's successful audit cache and pending candidate rows. An empty
    /// candidate slice is still cached, preventing repeated audits of unchanged content. The
    /// authoritative canonical hash is recomputed while this transaction owns the DB mutex; a
    /// transcript/note writer racing the command's post-inference read therefore makes this CAS
    /// return `false` rather than allowing stale derived plaintext to be inserted after its purge
    /// trigger already fired.
    pub(crate) fn replace_reminder_audit_results(
        &self,
        source_kind: &str,
        source_id: &str,
        content_hash: &str,
        engine_id: &str,
        candidates: &[ReminderAuditCandidate],
        now: i64,
    ) -> Result<bool> {
        validate_audit_identity(source_kind, source_id, content_hash, engine_id)?;
        validate_audit_candidates(candidates)?;

        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        if canonical_reminder_audit_hash_tx(&tx, source_kind, source_id)?.as_deref()
            != Some(content_hash)
        {
            return Ok(false);
        }
        replace_reminder_audit_results_tx(
            &tx,
            source_kind,
            source_id,
            content_hash,
            engine_id,
            candidates,
            now,
        )?;
        tx.commit().map_err(map_err).map(|_| true)
    }

    /// Storage-domain fixture hook. Production callers must use the CAS method above.
    #[cfg(test)]
    pub(crate) fn replace_reminder_audit_results_unchecked(
        &self,
        source_kind: &str,
        source_id: &str,
        content_hash: &str,
        engine_id: &str,
        candidates: &[ReminderAuditCandidate],
        now: i64,
    ) -> Result<()> {
        validate_audit_identity(source_kind, source_id, content_hash, engine_id)?;
        validate_audit_candidates(candidates)?;
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        replace_reminder_audit_results_tx(
            &tx,
            source_kind,
            source_id,
            content_hash,
            engine_id,
            candidates,
            now,
        )?;
        tx.commit().map_err(map_err)
    }

    /// Dismiss exactly the source revision the caller re-gated and remember that plaintext-free
    /// candidate identity until the source is sealed/relocked or startup reconciliation withdraws
    /// the unlocked visibility snapshot.
    pub fn dismiss_pending_reminder_suggestion(
        &self,
        id: &str,
        source_kind: &str,
        source_id: &str,
        content_hash: &str,
    ) -> Result<bool> {
        validate_source(source_kind, source_id)?;
        validate_hash(content_hash)?;
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let candidate_key: Option<String> = tx
            .query_row(
                "SELECT s.candidate_key
                   FROM reminder_pending_suggestions s
                   JOIN reminder_audit_cache c
                     ON c.source_kind=s.source_kind AND c.source_id=s.source_id
                    AND c.content_hash=s.content_hash AND c.engine_id=s.engine_id
                  WHERE s.id=?1 AND s.source_kind=?2 AND s.source_id=?3
                    AND s.content_hash=?4",
                rusqlite::params![id, source_kind, source_id, content_hash],
                |row| row.get(0),
            )
            .optional()
            .map_err(map_err)?;
        let Some(candidate_key) = candidate_key else {
            return Ok(false);
        };
        let decided = tx
            .execute(
                "INSERT OR IGNORE INTO reminder_suggestion_decisions
                   (source_kind,source_id,content_hash,candidate_key,decision,reminder_id,decided_at)
                 VALUES (?1,?2,?3,?4,'dismissed',NULL,?5)",
                rusqlite::params![source_kind, source_id, content_hash, candidate_key, chrono::Utc::now().timestamp_millis()],
            )
            .map_err(map_err)?;
        if decided != 1 {
            return Ok(false);
        }
        let consumed = tx
            .execute(
                "DELETE FROM reminder_pending_suggestions
                  WHERE id=?1 AND source_kind=?2 AND source_id=?3 AND content_hash=?4
                    AND candidate_key=?5",
                rusqlite::params![id, source_kind, source_id, content_hash, candidate_key],
            )
            .map_err(map_err)?;
        if consumed != 1 {
            return Err(AppError::Storage(
                "reminder suggestion changed during dismissal".into(),
            ));
        }
        tx.commit().map_err(map_err)?;
        Ok(true)
    }

    /// Explicitly promote one still-current pending suggestion into the independent reminder
    /// domain. The candidate row is the one-shot capability: it is consumed in the same transaction
    /// as the `origin='smart'` insert, so replay cannot create another row. The cache and sibling
    /// candidates remain authoritative so the user can accept several suggestions from one audit.
    pub(crate) fn promote_pending_reminder_suggestion(
        &self,
        promotion: ReminderSuggestionPromotion<'_>,
    ) -> Result<bool> {
        let ReminderSuggestionPromotion {
            suggestion_id,
            expected_source_kind,
            expected_source_id,
            expected_content_hash,
            reminder_id,
            draft,
            now,
        } = promotion;
        validate_source(expected_source_kind, expected_source_id)?;
        validate_hash(expected_content_hash)?;
        if reminder_id.is_empty() || reminder_id.chars().count() > MAX_SOURCE_ID_CHARS {
            return Err(AppError::InvalidArg("reminder id is invalid".into()));
        }

        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let candidate_key: Option<String> = tx
            .query_row(
                "SELECT s.candidate_key
                   FROM reminder_pending_suggestions s
                   JOIN reminder_audit_cache c
                     ON c.source_kind=s.source_kind AND c.source_id=s.source_id
                    AND c.content_hash=s.content_hash AND c.engine_id=s.engine_id
                  WHERE s.id=?1 AND s.source_kind=?2 AND s.source_id=?3
                    AND s.content_hash=?4",
                rusqlite::params![
                    suggestion_id,
                    expected_source_kind,
                    expected_source_id,
                    expected_content_hash
                ],
                |row| row.get(0),
            )
            .optional()
            .map_err(map_err)?;
        let Some(candidate_key) = candidate_key else {
            return Ok(false);
        };

        tx.execute(
            "INSERT INTO reminders
               (id,title,details,due_at,repeat_every,repeat_unit,state,origin,created_at,updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,'active','smart',?7,?7)",
            rusqlite::params![
                reminder_id,
                draft.title,
                draft.details,
                draft.due_at,
                draft.repeat_every,
                draft.repeat_unit.map(ReminderRepeatUnit::as_str),
                now,
            ],
        )
        .map_err(map_err)?;
        insert_sources_tx(&tx, reminder_id, &draft.sources)?;
        tx.execute(
            "INSERT OR IGNORE INTO reminder_sources(reminder_id,source_kind,source_id)
             VALUES (?1,?2,?3)",
            rusqlite::params![reminder_id, expected_source_kind, expected_source_id],
        )
        .map_err(map_err)?;

        let decided = tx
            .execute(
                "INSERT OR IGNORE INTO reminder_suggestion_decisions
                   (source_kind,source_id,content_hash,candidate_key,decision,reminder_id,decided_at)
                 VALUES (?1,?2,?3,?4,'accepted',?5,?6)",
                rusqlite::params![
                    expected_source_kind,
                    expected_source_id,
                    expected_content_hash,
                    candidate_key,
                    reminder_id,
                    now
                ],
            )
            .map_err(map_err)?;
        if decided != 1 {
            return Ok(false);
        }
        let consumed = tx
            .execute(
                "DELETE FROM reminder_pending_suggestions
                  WHERE id=?1 AND source_kind=?2 AND source_id=?3 AND content_hash=?4
                    AND candidate_key=?5",
                rusqlite::params![
                    suggestion_id,
                    expected_source_kind,
                    expected_source_id,
                    expected_content_hash,
                    candidate_key
                ],
            )
            .map_err(map_err)?;
        if consumed != 1 {
            return Err(AppError::Storage(
                "reminder suggestion changed during promotion".into(),
            ));
        }
        tx.commit().map_err(map_err)?;
        Ok(true)
    }

    /// Purge all disposable source-derived reminder audit plaintext and unkeyed decision
    /// fingerprints inside an existing lock/seal transaction. Canonical accepted reminders and
    /// their opaque source anchors are untouched.
    pub(crate) fn purge_all_reminder_derived_tx(tx: &Transaction<'_>) -> Result<()> {
        tx.execute("DELETE FROM reminder_pending_suggestions", [])
            .map_err(map_err)?;
        tx.execute("DELETE FROM reminder_audit_cache", [])
            .map_err(map_err)?;
        tx.execute("DELETE FROM reminder_suggestion_decisions", [])
            .map_err(map_err)?;
        Ok(())
    }

    /// Publish the durable visibility gate for a fresh folder seal and withdraw every reminder
    /// audit capability/fingerprint authorized by the formerly-open snapshot in the SAME
    /// transaction. The caller has already verified all note ciphertext round-trips; this method
    /// deliberately does not touch sealed blobs or plaintext.
    pub(crate) fn publish_fresh_folder_lock_and_purge_reminder_derived(
        &self,
        folder_id: &str,
        wrapped_key: &[u8],
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let updated = tx
            .execute(
                "UPDATE folders SET locked = 1, wrapped_key = ?2
                  WHERE id = ?1 AND locked = 0",
                rusqlite::params![folder_id, wrapped_key],
            )
            .map_err(map_err)?;
        if updated != 1 {
            return Err(AppError::Storage(
                "folder disappeared or was no longer open while publishing fresh lock".into(),
            ));
        }
        Self::purge_all_reminder_derived_tx(&tx)?;
        // The lock publication is the exact durable visibility reduction. Ask v1 conversations
        // are global-derived, so purge them here too; this also covers an otherwise-empty folder.
        Self::purge_all_ask_conversations_tx(&tx)?;
        tx.commit().map_err(map_err)
    }

    pub fn create_reminder(
        &self,
        id: &str,
        draft: &ReminderDraft,
        origin: ReminderOrigin,
        now: i64,
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        tx.execute(
            "INSERT INTO reminders
               (id,title,details,due_at,repeat_every,repeat_unit,state,origin,created_at,updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,'active',?7,?8,?8)",
            rusqlite::params![
                id,
                draft.title,
                draft.details,
                draft.due_at,
                draft.repeat_every,
                draft.repeat_unit.map(ReminderRepeatUnit::as_str),
                origin.as_str(),
                now
            ],
        )
        .map_err(map_err)?;
        insert_sources_tx(&tx, id, &draft.sources)?;
        tx.commit().map_err(map_err)
    }

    pub fn update_reminder(&self, id: &str, draft: &ReminderDraft, now: i64) -> Result<bool> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let existing_schedule: Option<(i64, Option<i64>, Option<String>)> = tx
            .query_row(
                "SELECT due_at,repeat_every,repeat_unit FROM reminders WHERE id=?1",
                rusqlite::params![id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(map_err)?;
        let Some((old_due_at, old_repeat_every, old_repeat_unit)) = existing_schedule else {
            return Ok(false);
        };
        let new_repeat_every = draft.repeat_every.map(i64::from);
        let new_repeat_unit = draft
            .repeat_unit
            .map(ReminderRepeatUnit::as_str)
            .map(str::to_owned);
        let schedule_changed = old_due_at != draft.due_at
            || old_repeat_every != new_repeat_every
            || old_repeat_unit != new_repeat_unit;
        let changed = tx
            .execute(
                "UPDATE reminders
                    SET title=?2, details=?3, due_at=?4, repeat_every=?5, repeat_unit=?6,
                        updated_at=?7
                  WHERE id=?1",
                rusqlite::params![
                    id,
                    draft.title,
                    draft.details,
                    draft.due_at,
                    draft.repeat_every,
                    draft.repeat_unit.map(ReminderRepeatUnit::as_str),
                    now
                ],
            )
            .map_err(map_err)?;
        debug_assert_eq!(changed, 1);
        tx.execute(
            "DELETE FROM reminder_sources WHERE reminder_id=?1",
            rusqlite::params![id],
        )
        .map_err(map_err)?;
        insert_sources_tx(&tx, id, &draft.sources)?;
        if schedule_changed {
            // The `(reminder_id,due_at)` unique key is the generation authority. Clear ALL old
            // generations only when the schedule changes so a deliberately reused timestamp can
            // materialize once, while title/details/source-only edits preserve unread/dismissed
            // inbox state exactly.
            tx.execute(
                "DELETE FROM reminder_due_occurrences WHERE reminder_id=?1",
                rusqlite::params![id],
            )
            .map_err(map_err)?;
        }
        tx.commit().map_err(map_err)?;
        Ok(true)
    }

    pub fn delete_reminder(&self, id: &str) -> Result<bool> {
        let conn = self.lock();
        conn.execute("DELETE FROM reminders WHERE id=?1", rusqlite::params![id])
            .map(|n| n != 0)
            .map_err(map_err)
    }

    /// Idempotently materialize every active reminder whose due time has arrived. The unique
    /// `(reminder_id,due_at)` key is the dedupe authority across scheduler ticks and restarts.
    pub fn materialize_due_reminders(&self, now: i64) -> Result<u64> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let due = {
            let mut stmt = tx
                .prepare(
                    "SELECT id,due_at FROM reminders
                      WHERE state='active' AND due_at <= ?1
                      ORDER BY due_at,id",
                )
                .map_err(map_err)?;
            let rows = stmt
                .query_map(rusqlite::params![now], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })
                .map_err(map_err)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(map_err)?
        };
        let mut inserted = 0_u64;
        for (reminder_id, due_at) in due {
            inserted += tx
                .execute(
                    "INSERT OR IGNORE INTO reminder_due_occurrences
                       (id,reminder_id,due_at,status,created_at)
                     VALUES (?1,?2,?3,'unread',?4)",
                    rusqlite::params![uuid::Uuid::new_v4().to_string(), reminder_id, due_at, now],
                )
                .map_err(map_err)? as u64;
        }
        tx.commit().map_err(map_err)?;
        Ok(inserted)
    }

    pub fn due_reminder_count(&self) -> Result<u64> {
        let conn = self.lock();
        conn.query_row(
            "SELECT COUNT(*) FROM reminder_due_occurrences WHERE status='unread'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map(|n| n.max(0) as u64)
        .map_err(map_err)
    }

    pub fn unread_reminder_occurrences(&self) -> Result<Vec<StoredReminderOccurrence>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, reminder_id, due_at
                   FROM reminder_due_occurrences
                  WHERE status='unread'
                  ORDER BY due_at ASC, id ASC",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map([], |row| {
                Ok(StoredReminderOccurrence {
                    id: row.get(0)?,
                    reminder_id: row.get(1)?,
                    due_at: row.get(2)?,
                })
            })
            .map_err(map_err)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(map_err)
    }

    pub fn list_stored_reminders(&self) -> Result<Vec<StoredReminder>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id,title,details,due_at,repeat_every,repeat_unit,state,origin,
                        created_at,updated_at,completed_at
                   FROM reminders
                  ORDER BY CASE state WHEN 'active' THEN 0 ELSE 1 END,
                           CASE state WHEN 'active' THEN due_at ELSE completed_at END ASC,
                           id ASC",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map([], stored_reminder_from_row)
            .map_err(map_err)?;
        let mut reminders = Vec::new();
        for row in rows {
            reminders.push(row.map_err(map_err)??);
        }
        drop(stmt);
        for reminder in &mut reminders {
            reminder.sources = list_sources(&conn, &reminder.id)?;
        }
        Ok(reminders)
    }

    /// Dashboard/agent projection that authorizes every Smart-reminder anchor in SQL BEFORE
    /// hydrating its title. Manual reminders with no anchors are user-authored and remain valid;
    /// Smart reminders with no anchors, unknown kinds, deleted sources, or any sealed source fail
    /// closed. The normal reminder writer still accepts only note/meeting anchors; the document
    /// arm keeps imported/future rows on the same typed visibility gate instead of silently
    /// dropping a readable source. Returns only the two fields the dashboard renders.
    pub(crate) fn list_dashboard_reminders_visible(
        &self,
        unlocked: &std::collections::HashSet<String>,
    ) -> Result<Vec<(String, i64)>> {
        let conn = self.lock();
        // `visibility_clause` deliberately uses the canonical `f` folder alias. Keep each
        // correlated source subquery scoped to that alias instead of assuming its parameter
        // rewrites the SQL identifier (it does not).
        let folder_visible = visibility_clause("f", unlocked);
        let meeting_visible = meeting_visibility_clause("m", unlocked);
        let sql = format!(
            "SELECT r.title, r.due_at
               FROM reminders r
              WHERE r.state='active'
                AND (r.origin='manual' OR EXISTS (
                      SELECT 1 FROM reminder_sources present WHERE present.reminder_id=r.id))
                AND NOT EXISTS (
                  SELECT 1 FROM reminder_sources rs
                   WHERE rs.reminder_id=r.id AND (
                     (rs.source_kind='meeting' AND NOT EXISTS (
                       SELECT 1 FROM meetings m
                        WHERE m.id=rs.source_id AND {meeting_visible}))
                     OR (rs.source_kind='note' AND NOT EXISTS (
                       SELECT 1 FROM documents d
                       JOIN folders f ON f.id=d.folder_id
                       WHERE d.id=rs.source_id AND d.kind='note' AND {folder_visible}))
                     OR (rs.source_kind='document' AND NOT EXISTS (
                       SELECT 1 FROM documents d
                       JOIN folders f ON f.id=d.folder_id
                       WHERE d.id=rs.source_id AND d.kind='document' AND {folder_visible}))
                     OR rs.source_kind NOT IN ('meeting','note','document')
                   ))
              ORDER BY r.due_at ASC, r.id ASC"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_err)?;
        Ok(rows)
    }

    pub fn get_stored_reminder(&self, id: &str) -> Result<Option<StoredReminder>> {
        let conn = self.lock();
        let mut reminder = conn
            .query_row(
                "SELECT id,title,details,due_at,repeat_every,repeat_unit,state,origin,
                        created_at,updated_at,completed_at
                   FROM reminders WHERE id=?1",
                rusqlite::params![id],
                stored_reminder_from_row,
            )
            .optional()
            .map_err(map_err)?
            .transpose()?;
        if let Some(row) = &mut reminder {
            row.sources = list_sources(&conn, id)?;
        }
        Ok(reminder)
    }

    /// Complete exactly the schedule generation represented by `expected_due_at`. Replaying the
    /// same UI action after a recurring reminder advanced is a no-op, not a second advancement.
    pub fn complete_reminder(&self, id: &str, expected_due_at: i64, now: i64) -> Result<bool> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let row: Option<(i64, Option<i64>, Option<String>, String)> = tx
            .query_row(
                "SELECT due_at, repeat_every, repeat_unit, state
                   FROM reminders WHERE id=?1",
                rusqlite::params![id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(map_err)?;
        let Some((due_at, repeat_every, repeat_unit, state)) = row else {
            return Ok(false);
        };
        if due_at != expected_due_at || state != "active" {
            return Ok(false);
        }

        let occurrence_id = uuid::Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO reminder_due_occurrences
               (id,reminder_id,due_at,status,created_at,resolved_at)
             VALUES (?1,?2,?3,'completed',?4,?4)
             ON CONFLICT(reminder_id,due_at) DO UPDATE SET
               status='completed', resolved_at=excluded.resolved_at",
            rusqlite::params![occurrence_id, id, due_at, now],
        )
        .map_err(map_err)?;

        match (repeat_every, repeat_unit) {
            (Some(every), Some(unit)) => {
                let unit = ReminderRepeatUnit::parse(&unit)?;
                match next_recurrence_after(due_at, every as u32, unit, now)? {
                    Some(next_due) => {
                        tx.execute(
                            "UPDATE reminders
                                SET due_at=?2, state='active', completed_at=NULL, updated_at=?3
                              WHERE id=?1 AND due_at=?4 AND state='active'",
                            rusqlite::params![id, next_due, now, due_at],
                        )
                        .map_err(map_err)?;
                    }
                    None => {
                        // No representable future occurrence remains before the exclusive 2200
                        // horizon. End the series cleanly rather than persisting an unsupported
                        // date or making every subsequent read fail validation.
                        tx.execute(
                            "UPDATE reminders
                                SET state='completed', completed_at=?2, updated_at=?2
                              WHERE id=?1 AND due_at=?3 AND state='active'",
                            rusqlite::params![id, now, due_at],
                        )
                        .map_err(map_err)?;
                    }
                }
            }
            (None, None) => {
                tx.execute(
                    "UPDATE reminders
                        SET state='completed', completed_at=?2, updated_at=?2
                      WHERE id=?1 AND due_at=?3 AND state='active'",
                    rusqlite::params![id, now, due_at],
                )
                .map_err(map_err)?;
            }
            _ => {
                return Err(AppError::Storage(
                    "reminder recurrence columns are inconsistent".into(),
                ));
            }
        }
        tx.commit().map_err(map_err)?;
        Ok(true)
    }

    /// Dismiss one exact unread occurrence. For a recurring series, dismissal acknowledges the
    /// current schedule generation and advances it exactly once to the first future recurrence.
    /// The occurrence status and schedule CAS share one transaction, so replaying the same UI
    /// action cannot advance the series twice. A one-off reminder remains active and visible as
    /// overdue after its Inbox occurrence is dismissed.
    pub fn dismiss_reminder_occurrence(&self, occurrence_id: &str, now: i64) -> Result<bool> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let row: Option<ReminderOccurrenceScheduleRow> = tx
            .query_row(
                "SELECT o.reminder_id, o.due_at, r.due_at, r.repeat_every, r.repeat_unit, r.state
                   FROM reminder_due_occurrences o
                   JOIN reminders r ON r.id=o.reminder_id
                  WHERE o.id=?1 AND o.status='unread'",
                rusqlite::params![occurrence_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .optional()
            .map_err(map_err)?;
        let Some((
            reminder_id,
            occurrence_due_at,
            reminder_due_at,
            repeat_every,
            repeat_unit,
            state,
        )) = row
        else {
            return Ok(false);
        };

        let dismissed = tx
            .execute(
                "UPDATE reminder_due_occurrences
                SET status='dismissed', resolved_at=?2
              WHERE id=?1 AND status='unread'",
                rusqlite::params![occurrence_id, now],
            )
            .map_err(map_err)?;
        if dismissed != 1 {
            return Err(AppError::Storage(
                "reminder occurrence changed during dismissal".into(),
            ));
        }

        // A stale occurrence cannot mutate a newer schedule generation. It is still safe to
        // dismiss the exact Inbox row the user acted on.
        if state == "active" && reminder_due_at == occurrence_due_at {
            match (repeat_every, repeat_unit) {
                (Some(every), Some(unit)) => {
                    let unit = ReminderRepeatUnit::parse(&unit)?;
                    match next_recurrence_after(reminder_due_at, every as u32, unit, now)? {
                        Some(next_due) => {
                            let advanced = tx
                                .execute(
                                    "UPDATE reminders
                                        SET due_at=?2, state='active', completed_at=NULL,
                                            updated_at=?3
                                      WHERE id=?1 AND due_at=?4 AND state='active'",
                                    rusqlite::params![reminder_id, next_due, now, reminder_due_at],
                                )
                                .map_err(map_err)?;
                            if advanced != 1 {
                                return Err(AppError::Storage(
                                    "reminder changed during occurrence dismissal".into(),
                                ));
                            }
                        }
                        None => {
                            let terminalized = tx
                                .execute(
                                    "UPDATE reminders
                                        SET state='completed', completed_at=?2, updated_at=?2
                                      WHERE id=?1 AND due_at=?3 AND state='active'",
                                    rusqlite::params![reminder_id, now, reminder_due_at],
                                )
                                .map_err(map_err)?;
                            if terminalized != 1 {
                                return Err(AppError::Storage(
                                    "reminder changed during occurrence dismissal".into(),
                                ));
                            }
                        }
                    }
                }
                (None, None) => {}
                _ => {
                    return Err(AppError::Storage(
                        "reminder recurrence columns are inconsistent".into(),
                    ));
                }
            }
        }

        tx.commit().map_err(map_err)?;
        Ok(true)
    }
}

fn validate_source(source_kind: &str, source_id: &str) -> Result<()> {
    if !matches!(source_kind, "meeting" | "note")
        || source_id.is_empty()
        || source_id.chars().count() > MAX_SOURCE_ID_CHARS
    {
        return Err(AppError::InvalidArg(
            "reminder audit source is invalid".into(),
        ));
    }
    Ok(())
}

fn validate_hash(value: &str) -> Result<()> {
    let valid = value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    if !valid {
        return Err(AppError::InvalidArg(
            "reminder audit content hash is invalid".into(),
        ));
    }
    Ok(())
}

fn validate_audit_identity(
    source_kind: &str,
    source_id: &str,
    content_hash: &str,
    engine_id: &str,
) -> Result<()> {
    validate_source(source_kind, source_id)?;
    validate_hash(content_hash)?;
    if engine_id.is_empty() || engine_id.chars().count() > MAX_ENGINE_ID_CHARS {
        return Err(AppError::InvalidArg(
            "reminder audit engine id is invalid".into(),
        ));
    }
    Ok(())
}

fn validate_audit_candidates(candidates: &[ReminderAuditCandidate]) -> Result<()> {
    if candidates.len() > MAX_PENDING_SUGGESTIONS {
        return Err(AppError::InvalidArg(
            "too many reminder audit candidates".into(),
        ));
    }
    let mut ids = std::collections::HashSet::with_capacity(candidates.len());
    let mut keys = std::collections::HashSet::with_capacity(candidates.len());
    for candidate in candidates {
        if candidate.id.is_empty()
            || candidate.id.chars().count() > 32
            || !ids.insert(candidate.id.as_str())
            || candidate.title.trim().is_empty()
            || candidate.title.chars().count() > MAX_REMINDER_TITLE_CHARS
            || validate_hash(&candidate.candidate_key).is_err()
            || !keys.insert(candidate.candidate_key.as_str())
        {
            return Err(AppError::InvalidArg(
                "reminder audit candidate is invalid".into(),
            ));
        }
    }
    Ok(())
}

fn reminder_suggestion_id(
    source_kind: &str,
    source_id: &str,
    content_hash: &str,
    candidate_key: &str,
) -> String {
    format!(
        "rs:{}",
        crate::reminder_audit::content_hash(&[source_kind, source_id, content_hash, candidate_key])
    )
}

fn delete_reminder_audit_source_tx(
    tx: &Transaction<'_>,
    source_kind: &str,
    source_id: &str,
) -> Result<()> {
    tx.execute(
        "DELETE FROM reminder_pending_suggestions
          WHERE source_kind=?1 AND source_id=?2",
        rusqlite::params![source_kind, source_id],
    )
    .map_err(map_err)?;
    tx.execute(
        "DELETE FROM reminder_audit_cache WHERE source_kind=?1 AND source_id=?2",
        rusqlite::params![source_kind, source_id],
    )
    .map_err(map_err)?;
    Ok(())
}

fn replace_reminder_audit_results_tx(
    tx: &Transaction<'_>,
    source_kind: &str,
    source_id: &str,
    content_hash: &str,
    engine_id: &str,
    candidates: &[ReminderAuditCandidate],
    now: i64,
) -> Result<()> {
    delete_reminder_audit_source_tx(tx, source_kind, source_id)?;
    {
        let mut stmt = tx
            .prepare(
                "INSERT INTO reminder_pending_suggestions
                   (id,source_kind,source_id,content_hash,engine_id,candidate_key,title,
                    suggested_due_at,created_at)
                 SELECT ?1,?2,?3,?4,?5,?6,?7,?8,?9
                  WHERE NOT EXISTS (
                    SELECT 1 FROM reminder_suggestion_decisions d
                     WHERE d.source_kind=?2 AND d.source_id=?3
                       AND d.content_hash=?4 AND d.candidate_key=?6
                  )",
            )
            .map_err(map_err)?;
        for candidate in candidates {
            let id = reminder_suggestion_id(
                source_kind,
                source_id,
                content_hash,
                &candidate.candidate_key,
            );
            stmt.execute(rusqlite::params![
                id,
                source_kind,
                source_id,
                content_hash,
                engine_id,
                candidate.candidate_key,
                candidate.title,
                candidate.suggested_due_at,
                now,
            ])
            .map_err(map_err)?;
        }
    }
    tx.execute(
        "INSERT INTO reminder_audit_cache
           (source_kind,source_id,content_hash,engine_id,audited_at)
         VALUES (?1,?2,?3,?4,?5)",
        rusqlite::params![source_kind, source_id, content_hash, engine_id, now],
    )
    .map_err(map_err)?;
    Ok(())
}

/// Raw canonical hash used only inside the command-authorized audit CAS transaction. This helper
/// must never back an IPC reader: it intentionally bypasses visibility because the command already
/// holds the lifecycle gate, and keeping these reads on the same DB transaction is what closes the
/// transcript-writer race between post-inference verification and derived insertion.
fn canonical_reminder_audit_hash_tx(
    tx: &Transaction<'_>,
    source_kind: &str,
    source_id: &str,
) -> Result<Option<String>> {
    match source_kind {
        "meeting" => {
            let row: Option<(Option<String>, String)> = tx
                .query_row(
                    "SELECT title, COALESCE(manual_notes, '') FROM meetings WHERE id=?1",
                    rusqlite::params![source_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(map_err)?;
            let Some((title, manual_notes)) = row else {
                return Ok(None);
            };
            let title = title
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| "Untitled meeting".into());
            let markdown = tx
                .query_row(
                    "SELECT markdown FROM notes
                      WHERE meeting_id=?1
                      ORDER BY created_at DESC, provider_id DESC
                      LIMIT 1",
                    rusqlite::params![source_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(map_err)?
                .unwrap_or_default();
            let mut segments = Vec::new();
            let mut stmt = tx
                .prepare(
                    "SELECT idx,start_s,end_s,text,speaker,confidence,echo_suppressed
                       FROM segments WHERE meeting_id=?1 ORDER BY idx",
                )
                .map_err(map_err)?;
            let rows = stmt
                .query_map(rusqlite::params![source_id], |row| {
                    Ok((
                        Segment {
                            idx: row.get(0)?,
                            start_s: row.get(1)?,
                            end_s: row.get(2)?,
                            text: row.get(3)?,
                            speaker: row.get(4)?,
                            confidence: row.get(5)?,
                        },
                        row.get::<_, i64>(6)? != 0,
                    ))
                })
                .map_err(map_err)?;
            for row in rows {
                let (segment, echo_suppressed) = row.map_err(map_err)?;
                if !echo_suppressed {
                    segments.push(segment);
                }
            }
            Ok(Some(canonical_reminder_source_hash(
                &title,
                &markdown,
                Some(&manual_notes),
                &segments,
            )))
        }
        "note" => {
            let row: Option<(String, Option<String>, String)> = tx
                .query_row(
                    "SELECT name,title,COALESCE(text,'') FROM documents
                      WHERE id=?1 AND kind='note'",
                    rusqlite::params![source_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()
                .map_err(map_err)?;
            let Some((name, title, text)) = row else {
                return Ok(None);
            };
            let title = title
                .filter(|value| !value.trim().is_empty())
                .unwrap_or(name);
            Ok(Some(canonical_reminder_source_hash(
                &title,
                &text,
                None,
                &[],
            )))
        }
        _ => Err(AppError::InvalidArg(
            "reminder audit source is invalid".into(),
        )),
    }
}

fn stored_suggestion_from_row(row: &Row<'_>) -> rusqlite::Result<StoredReminderSuggestion> {
    Ok(StoredReminderSuggestion {
        id: row.get(0)?,
        source_kind: row.get(1)?,
        source_id: row.get(2)?,
        content_hash: row.get(3)?,
        engine_id: row.get(4)?,
        candidate_key: row.get(5)?,
        title: row.get(6)?,
        suggested_due_at: row.get(7)?,
        created_at: row.get(8)?,
    })
}

fn insert_sources_tx(
    tx: &Transaction<'_>,
    reminder_id: &str,
    sources: &[ReminderSourceAnchor],
) -> Result<()> {
    let mut stmt = tx
        .prepare(
            "INSERT OR IGNORE INTO reminder_sources(reminder_id,source_kind,source_id)
             VALUES (?1,?2,?3)",
        )
        .map_err(map_err)?;
    for source in sources {
        stmt.execute(rusqlite::params![reminder_id, source.kind, source.id])
            .map_err(map_err)?;
    }
    Ok(())
}

fn list_sources(conn: &Connection, reminder_id: &str) -> Result<Vec<ReminderSourceAnchor>> {
    let mut stmt = conn
        .prepare(
            "SELECT source_kind, source_id FROM reminder_sources
              WHERE reminder_id=?1 ORDER BY source_kind, source_id",
        )
        .map_err(map_err)?;
    let rows = stmt
        .query_map(rusqlite::params![reminder_id], |row| {
            Ok(ReminderSourceAnchor {
                kind: row.get(0)?,
                id: row.get(1)?,
            })
        })
        .map_err(map_err)?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(map_err)
}

fn stored_reminder_from_row(row: &Row<'_>) -> rusqlite::Result<Result<StoredReminder>> {
    let repeat_unit: Option<String> = row.get(5)?;
    let state: String = row.get(6)?;
    let origin: String = row.get(7)?;
    let repeat_unit = match repeat_unit
        .as_deref()
        .map(ReminderRepeatUnit::parse)
        .transpose()
    {
        Ok(value) => value,
        Err(error) => return Ok(Err(error)),
    };
    let state = match ReminderState::parse(&state) {
        Ok(value) => value,
        Err(error) => return Ok(Err(error)),
    };
    let origin = match ReminderOrigin::parse(&origin) {
        Ok(value) => value,
        Err(error) => return Ok(Err(error)),
    };
    Ok(Ok(StoredReminder {
        id: row.get(0)?,
        title: row.get(1)?,
        details: row.get(2)?,
        due_at: row.get(3)?,
        repeat_every: row.get::<_, Option<i64>>(4)?.map(|n| n as u32),
        repeat_unit,
        state,
        origin,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
        completed_at: row.get(10)?,
        sources: Vec::new(),
    }))
}

/// Advance an overdue recurring series directly to its first future occurrence. A single user
/// completion therefore acknowledges all missed cycles. The loop is bounded by the supported
/// 200-year domain (daily cadence is < 74k steps); month/year transitions progressively clamp the
/// day to the target month's last valid day (Jan 31 → Feb 28/29). DST gaps advance to the first
/// valid local minute and ambiguous fall-back times choose the earlier occurrence.
fn next_recurrence_after(
    due_at: i64,
    every: u32,
    unit: ReminderRepeatUnit,
    now: i64,
) -> Result<Option<i64>> {
    if !(MIN_REMINDER_DUE_AT..MAX_REMINDER_DUE_AT).contains(&due_at) {
        return Err(AppError::InvalidArg(
            "reminder due time is out of range".into(),
        ));
    }
    let mut current = due_at;
    for _ in 0..=74_000 {
        let next = advance_calendar_due_raw(current, every, unit)?;
        if next >= MAX_REMINDER_DUE_AT {
            return Ok(None);
        }
        if next > now {
            return Ok(Some(next));
        }
        current = next;
    }
    Err(AppError::Storage(
        "recurring reminder did not advance within the supported horizon".into(),
    ))
}

fn advance_calendar_due_raw(due_at: i64, every: u32, unit: ReminderRepeatUnit) -> Result<i64> {
    if every == 0 || every > 365 {
        return Err(AppError::InvalidArg(
            "reminder repeat interval is out of range".into(),
        ));
    }
    let local = Local
        .timestamp_millis_opt(due_at)
        .single()
        .ok_or_else(|| AppError::InvalidArg("reminder due time is out of range".into()))?;
    let date = local.date_naive();
    let target_date = match unit {
        ReminderRepeatUnit::Days => date.checked_add_signed(chrono::Duration::days(every as i64)),
        ReminderRepeatUnit::Weeks => date.checked_add_signed(chrono::Duration::days(
            (every as i64).checked_mul(7).ok_or_else(|| {
                AppError::InvalidArg("reminder repeat interval is out of range".into())
            })?,
        )),
        ReminderRepeatUnit::Months => add_months_clamped(date, every),
        ReminderRepeatUnit::Years => add_months_clamped(
            date,
            every.checked_mul(12).ok_or_else(|| {
                AppError::InvalidArg("reminder repeat interval is out of range".into())
            })?,
        ),
    }
    .ok_or_else(|| AppError::InvalidArg("recurring reminder date is out of range".into()))?;
    let naive = NaiveDateTime::new(
        target_date,
        chrono::NaiveTime::from_hms_nano_opt(
            local.hour(),
            local.minute(),
            local.second(),
            local.nanosecond(),
        )
        .ok_or_else(|| AppError::InvalidArg("reminder time is invalid".into()))?,
    );
    resolve_local_datetime(naive)
        .map(|dt| dt.timestamp_millis())
        .ok_or_else(|| AppError::InvalidArg("recurring reminder time is invalid locally".into()))
}

fn add_months_clamped(date: NaiveDate, months: u32) -> Option<NaiveDate> {
    let zero_based = date.year() as i64 * 12 + date.month0() as i64 + months as i64;
    let year = i32::try_from(zero_based.div_euclid(12)).ok()?;
    let month = u32::try_from(zero_based.rem_euclid(12)).ok()? + 1;
    let last = last_day_of_month(year, month)?;
    NaiveDate::from_ymd_opt(year, month, date.day().min(last))
}

fn last_day_of_month(year: i32, month: u32) -> Option<u32> {
    let (next_year, next_month) = if month == 12 {
        (year.checked_add(1)?, 1)
    } else {
        (year, month + 1)
    };
    NaiveDate::from_ymd_opt(next_year, next_month, 1)?
        .pred_opt()
        .map(|date| date.day())
}

fn resolve_local_datetime(naive: NaiveDateTime) -> Option<chrono::DateTime<Local>> {
    resolve_local_datetime_with(naive, |candidate| {
        Local.from_local_datetime(&candidate).earliest()
    })
}

fn resolve_local_datetime_with<T>(
    mut naive: NaiveDateTime,
    mut resolve: impl FnMut(NaiveDateTime) -> Option<T>,
) -> Option<T> {
    for _ in 0..=MAX_LOCAL_DATETIME_GAP_MINUTES {
        if let Some(value) = resolve(naive) {
            return Some(value);
        }
        naive = naive.checked_add_signed(chrono::Duration::minutes(1))?;
    }
    None
}

#[cfg(test)]
mod recurrence_resolution_tests {
    use super::*;

    #[test]
    fn resolver_crosses_a_skipped_civil_day() {
        let skipped_start = NaiveDate::from_ymd_opt(2011, 12, 30)
            .unwrap()
            .and_hms_opt(9, 0, 0)
            .unwrap();
        let first_valid = NaiveDate::from_ymd_opt(2011, 12, 31)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap();

        let resolved = resolve_local_datetime_with(skipped_start, |candidate| {
            (candidate >= first_valid).then_some(candidate)
        });

        assert_eq!(resolved, Some(first_valid));
    }
}
