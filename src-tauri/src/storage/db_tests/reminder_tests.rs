use super::*;
use chrono::{Datelike, Local, TimeZone, Timelike};

use crate::reminder_audit::{build_candidates, ReminderAuditCandidate};
use crate::storage::models::{
    Folder, Meeting, MeetingStatus, NoteRecord, ReminderDraft, ReminderOrigin, ReminderRepeatUnit,
    ReminderSourceAnchor, ReminderState,
};
use crate::storage::reminder_store::ReminderSuggestionPromotion;

fn reminder_db() -> Db {
    register_vec_extension();
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    let db = Db {
        conn: Mutex::new(conn),
    };
    db.migrate().unwrap();
    db
}

fn draft(due_at: i64) -> ReminderDraft {
    ReminderDraft {
        title: "Review the launch plan".into(),
        details: Some("Bring the final checklist".into()),
        due_at,
        repeat_every: None,
        repeat_unit: None,
        sources: vec![],
    }
}

fn audit_hash(fill: char) -> String {
    fill.to_string().repeat(64)
}

fn audit_candidates(titles: &[&str]) -> Vec<ReminderAuditCandidate> {
    build_candidates(
        "",
        &titles
            .iter()
            .map(|title| (*title).to_string())
            .collect::<Vec<_>>(),
    )
}

fn derived_reminder_audit_count(db: &Db) -> i64 {
    db.lock()
        .query_row(
            "SELECT
               (SELECT COUNT(*) FROM reminder_audit_cache) +
               (SELECT COUNT(*) FROM reminder_pending_suggestions)",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

fn expect_source_invalidations(db: &Db, expected: &[(&str, &str)]) {
    let invalidations = db.peek_reminder_source_invalidations(64).unwrap();
    let actual = invalidations
        .iter()
        .map(|item| (item.kind.clone(), item.id.clone()))
        .collect::<Vec<_>>();
    let expected = expected
        .iter()
        .map(|(kind, id)| ((*kind).to_string(), (*id).to_string()))
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
    for invalidation in invalidations {
        assert!(
            db.ack_reminder_source_invalidation(&invalidation).unwrap(),
            "unchanged peeked revision must acknowledge"
        );
    }
}

fn seed_reminder_audit_source(db: &Db, folder_id: &str, meeting_id: &str) {
    db.insert_folder(&Folder {
        id: folder_id.into(),
        name: "Private".into(),
        path: "Private".into(),
        parent_id: None,
        locked: false,
        created_at: "2026-07-29T09:00:00Z".into(),
    })
    .unwrap();
    db.insert_meeting(&Meeting {
        id: meeting_id.into(),
        started_at: "2026-07-29T10:00:00Z".into(),
        ended_at: None,
        title: Some("Planning".into()),
        duration_s: 0,
        audio_path: None,
        status: MeetingStatus::Summarized,
        folder_id: None,
    })
    .unwrap();
    db.upsert_note(&NoteRecord {
        meeting_id: meeting_id.into(),
        provider_id: "local".into(),
        markdown: "- [ ] Ship the plan".into(),
        created_at: "2026-07-29T10:05:00Z".into(),
        exported_path: None,
        model_requested: None,
        model_served: None,
        gateway_host: None,
    })
    .unwrap();
    db.set_note_folder(meeting_id, Some(folder_id)).unwrap();
}

#[test]
fn reminder_migration_is_additive_and_idempotent() {
    let db = reminder_db();
    db.migrate().unwrap();
    db.migrate().unwrap();
    let conn = db.lock();
    for table in [
        "reminders",
        "reminder_sources",
        "reminder_due_occurrences",
        "reminder_audit_cache",
        "reminder_pending_suggestions",
        "reminder_suggestion_decisions",
        "reminder_source_invalidation_queue",
    ] {
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(
                   SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1
                 )",
                rusqlite::params![table],
                |row| row.get(0),
            )
            .unwrap();
        assert!(exists, "{table} missing after repeated migrate");
    }
    for trigger in [
        "reminder_derived_notes_projection_au",
        "reminder_derived_segments_projection_au",
        "reminder_derived_segments_echo_au",
        "reminder_derived_documents_anchor_au",
        "reminder_source_queue_notes_ai",
        "reminder_source_queue_notes_au",
        "reminder_source_queue_notes_ad",
        "reminder_source_queue_segments_ai",
        "reminder_source_queue_segments_au",
        "reminder_source_queue_segments_ad",
        "reminder_source_queue_meetings_au",
        "reminder_source_queue_meetings_ad",
        "reminder_source_queue_documents_ai",
        "reminder_source_queue_documents_au",
        "reminder_source_queue_documents_ad",
    ] {
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(
                   SELECT 1 FROM sqlite_master WHERE type='trigger' AND name=?1
                 )",
                rusqlite::params![trigger],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            exists,
            "{trigger} missing after repeated additive migration"
        );
    }
    let mut columns = conn
        .prepare("PRAGMA table_info(reminder_source_invalidation_queue)")
        .unwrap();
    let column_names = columns
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        column_names,
        vec!["source_kind", "source_id", "revision"],
        "durable outbox must persist no title, hash, or source content"
    );
}

#[test]
fn reminder_migration_upgrades_pre_encrypt_v7_source_shape_before_full_triggers() {
    register_vec_extension();
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         CREATE TABLE meetings(
           id TEXT PRIMARY KEY,
           started_at TEXT,
           title TEXT,
           audio_path TEXT
         );
         CREATE TABLE segments(meeting_id TEXT, idx INTEGER, text TEXT);
         CREATE TABLE notes(meeting_id TEXT, markdown TEXT);
         INSERT INTO meetings VALUES('m1','2026-07-01','Sync',NULL);
         INSERT INTO segments VALUES('m1',0,'legacy transcript');
         INSERT INTO notes VALUES('m1','# Legacy note');
         PRAGMA user_version=7;",
    )
    .unwrap();
    let db = Db {
        conn: Mutex::new(conn),
    };

    db.migrate()
        .expect("legacy pre-encrypt shape must accept the full reminder triggers");
    db.migrate()
        .expect("legacy compatibility migration must be idempotent");
    let note_projection: (String, String, String) = db
        .lock()
        .query_row(
            "SELECT markdown,provider_id,created_at FROM notes WHERE meeting_id='m1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        note_projection,
        ("# Legacy note".into(), "legacy".into(), String::new()),
        "guarded ALTERs must preserve content and backfill deterministic projection defaults"
    );
    let segment_projection: (String, f64, f64) = db
        .lock()
        .query_row(
            "SELECT text,start_s,end_s FROM segments WHERE meeting_id='m1' AND idx=0",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(segment_projection, ("legacy transcript".into(), 0.0, 0.0));

    db.lock()
        .execute(
            "UPDATE notes SET markdown='# Edited legacy note' WHERE meeting_id='m1'",
            [],
        )
        .unwrap();
    db.lock()
        .execute(
            "UPDATE segments SET text='edited legacy transcript'
              WHERE meeting_id='m1' AND idx=0",
            [],
        )
        .unwrap();
    expect_source_invalidations(&db, &[("meeting", "m1")]);
}

#[test]
fn reminder_source_outbox_is_bounded_replayable_and_cas_preserves_racing_edits() {
    let db = reminder_db();
    {
        let mut conn = db.lock();
        let tx = conn.transaction().unwrap();
        for idx in 0..300 {
            tx.execute(
                "INSERT OR IGNORE INTO reminder_source_invalidation_queue(source_kind,source_id)
                 VALUES ('meeting',?1)",
                rusqlite::params![format!("m-{idx:03}")],
            )
            .unwrap();
        }
        tx.execute(
            "INSERT INTO reminder_source_invalidation_queue(source_kind,source_id,revision)
             VALUES ('meeting','m-000',1)
             ON CONFLICT(source_kind,source_id) DO UPDATE SET revision=revision+1",
            [],
        )
        .unwrap();
        tx.commit().unwrap();
    }
    let count: i64 = db
        .lock()
        .query_row(
            "SELECT COUNT(*) FROM reminder_source_invalidation_queue",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 300, "duplicate source edits must coalesce");

    let first = db.peek_reminder_source_invalidations(usize::MAX).unwrap();
    assert_eq!(first.len(), 256, "peek must enforce its internal bound");
    assert_eq!(first[0].id, "m-000");
    assert_eq!(first[0].revision, 2);
    assert_eq!(
        db.peek_reminder_source_invalidations(usize::MAX).unwrap(),
        first,
        "a crash or failed emit before acknowledgement must replay the same batch"
    );

    db.lock()
        .execute(
            "INSERT INTO reminder_source_invalidation_queue(source_kind,source_id,revision)
             VALUES ('meeting','m-000',1)
             ON CONFLICT(source_kind,source_id) DO UPDATE SET revision=revision+1",
            [],
        )
        .unwrap();
    assert!(
        !db.ack_reminder_source_invalidation(&first[0]).unwrap(),
        "a stale acknowledgement must not delete a newer edit"
    );
    for item in &first[1..] {
        assert!(db.ack_reminder_source_invalidation(item).unwrap());
    }

    let second = db.peek_reminder_source_invalidations(256).unwrap();
    assert_eq!(second.len(), 45);
    assert!(
        second
            .iter()
            .any(|item| item.id == "m-000" && item.revision == 3),
        "a mutation racing the emit must remain queued at its newer revision"
    );
    for item in &second {
        assert!(db.ack_reminder_source_invalidation(item).unwrap());
    }
    assert!(db.peek_reminder_source_invalidations(1).unwrap().is_empty());
}

#[test]
fn reminder_source_outbox_covers_every_canonical_smart_source_mutation() {
    let db = reminder_db();
    seed_reminder_audit_source(&db, "f1", "m1");
    db.insert_folder(&Folder {
        id: "f2".into(),
        name: "Moved".into(),
        path: "Moved".into(),
        parent_id: None,
        locked: false,
        created_at: "2026-07-29T09:01:00Z".into(),
    })
    .unwrap();
    db.insert_meeting(&Meeting {
        id: "m2".into(),
        started_at: "2026-07-29T11:00:00Z".into(),
        ended_at: None,
        title: Some("Second".into()),
        duration_s: 0,
        audio_path: None,
        status: MeetingStatus::Summarized,
        folder_id: None,
    })
    .unwrap();
    expect_source_invalidations(&db, &[("meeting", "m1")]);

    db.upsert_note(&NoteRecord {
        meeting_id: "m1".into(),
        provider_id: "provider-b".into(),
        markdown: "provider note".into(),
        created_at: "2026-07-29T10:06:00Z".into(),
        exported_path: None,
        model_requested: None,
        model_served: None,
        gateway_host: None,
    })
    .unwrap();
    expect_source_invalidations(&db, &[("meeting", "m1")]);
    for update in [
        "UPDATE notes SET provider_id='provider-c'
          WHERE meeting_id='m1' AND provider_id='provider-b'",
        "UPDATE notes SET markdown='changed'
          WHERE meeting_id='m1' AND provider_id='provider-c'",
        "UPDATE notes SET created_at='2026-07-29T10:07:00Z'
          WHERE meeting_id='m1' AND provider_id='provider-c'",
        "UPDATE notes SET folder_id='f2'
          WHERE meeting_id='m1' AND provider_id='provider-c'",
    ] {
        db.lock().execute(update, []).unwrap();
        expect_source_invalidations(&db, &[("meeting", "m1")]);
    }
    db.lock()
        .execute(
            "UPDATE notes SET meeting_id='m2'
              WHERE meeting_id='m1' AND provider_id='provider-c'",
            [],
        )
        .unwrap();
    expect_source_invalidations(&db, &[("meeting", "m1"), ("meeting", "m2")]);
    db.lock()
        .execute(
            "DELETE FROM notes WHERE meeting_id='m2' AND provider_id='provider-c'",
            [],
        )
        .unwrap();
    expect_source_invalidations(&db, &[("meeting", "m2")]);

    db.replace_segments(
        "m1",
        &[crate::transcribe::types::Segment {
            idx: 0,
            start_s: 0.0,
            end_s: 1.0,
            text: "follow up".into(),
            speaker: None,
            confidence: None,
        }],
    )
    .unwrap();
    expect_source_invalidations(&db, &[("meeting", "m1")]);
    let segment_rowid: i64 = db
        .lock()
        .query_row(
            "SELECT rowid FROM segments WHERE meeting_id='m1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    for update in [
        "UPDATE segments SET idx=idx+1 WHERE rowid=?1",
        "UPDATE segments SET start_s=start_s+0.25 WHERE rowid=?1",
        "UPDATE segments SET end_s=end_s+0.25 WHERE rowid=?1",
        "UPDATE segments SET text='changed text' WHERE rowid=?1",
        "UPDATE segments SET speaker='me' WHERE rowid=?1",
        "UPDATE segments SET confidence=0.5 WHERE rowid=?1",
        "UPDATE segments SET echo_suppressed=1 WHERE rowid=?1",
    ] {
        db.lock()
            .execute(update, rusqlite::params![segment_rowid])
            .unwrap();
        expect_source_invalidations(&db, &[("meeting", "m1")]);
    }
    db.lock()
        .execute(
            "UPDATE segments SET meeting_id='m2' WHERE rowid=?1",
            rusqlite::params![segment_rowid],
        )
        .unwrap();
    expect_source_invalidations(&db, &[("meeting", "m1"), ("meeting", "m2")]);
    db.lock()
        .execute(
            "DELETE FROM segments WHERE rowid=?1",
            rusqlite::params![segment_rowid],
        )
        .unwrap();
    expect_source_invalidations(&db, &[("meeting", "m2")]);

    db.lock()
        .execute("UPDATE meetings SET title='Renamed' WHERE id='m1'", [])
        .unwrap();
    expect_source_invalidations(&db, &[("meeting", "m1")]);
    db.lock()
        .execute("UPDATE meetings SET manual_notes='typed' WHERE id='m1'", [])
        .unwrap();
    expect_source_invalidations(&db, &[("meeting", "m1")]);

    db.insert_note("n1", "f1", "Fallback", "Title", "Text", 1)
        .unwrap();
    expect_source_invalidations(&db, &[("note", "n1")]);
    for update in [
        "UPDATE documents SET text='changed' WHERE id='n1'",
        "UPDATE documents SET title='Renamed' WHERE id='n1'",
        "UPDATE documents SET name='renamed.md' WHERE id='n1'",
        "UPDATE documents SET folder_id='f2' WHERE id='n1'",
        "UPDATE documents SET kind='document' WHERE id='n1'",
        "UPDATE documents SET kind='note' WHERE id='n1'",
    ] {
        db.lock().execute(update, []).unwrap();
        expect_source_invalidations(&db, &[("note", "n1")]);
    }
    db.lock()
        .execute("DELETE FROM documents WHERE id='n1'", [])
        .unwrap();
    expect_source_invalidations(&db, &[("note", "n1")]);

    db.lock()
        .execute("DELETE FROM meetings WHERE id='m2'", [])
        .unwrap();
    expect_source_invalidations(&db, &[("meeting", "m2")]);
}

#[test]
fn reminder_crud_round_trips_sources_and_delete_cascades() {
    let db = reminder_db();
    let mut input = draft(2_000_000_000_000);
    input.sources = vec![
        ReminderSourceAnchor {
            kind: "meeting".into(),
            id: "m1".into(),
        },
        ReminderSourceAnchor {
            kind: "note".into(),
            id: "n1".into(),
        },
    ];
    db.create_reminder("r1", &input, ReminderOrigin::Manual, 10)
        .unwrap();
    let stored = db.get_stored_reminder("r1").unwrap().unwrap();
    assert_eq!(stored.title, input.title);
    assert_eq!(stored.sources.len(), 2);
    assert_eq!(stored.state, ReminderState::Active);

    let mut edited = input.clone();
    edited.title = "Review the final plan".into();
    edited.details = None;
    edited.sources.truncate(1);
    assert!(db.update_reminder("r1", &edited, 11).unwrap());
    let stored = db.get_stored_reminder("r1").unwrap().unwrap();
    assert_eq!(stored.title, "Review the final plan");
    assert_eq!(stored.details, None);
    assert_eq!(stored.sources.len(), 1);
    assert_eq!(stored.updated_at, 11);

    assert!(db.delete_reminder("r1").unwrap());
    assert!(db.get_stored_reminder("r1").unwrap().is_none());
    let source_count: i64 = db
        .lock()
        .query_row(
            "SELECT COUNT(*) FROM reminder_sources WHERE reminder_id='r1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(source_count, 0);
}

#[test]
fn due_materialization_is_idempotent_across_repeated_ticks() {
    let db = reminder_db();
    db.create_reminder("r1", &draft(1_000_000_000_000), ReminderOrigin::Manual, 1)
        .unwrap();
    assert_eq!(db.materialize_due_reminders(999_999_999_999).unwrap(), 0);
    assert_eq!(db.materialize_due_reminders(1_000_000_000_000).unwrap(), 1);
    assert_eq!(db.materialize_due_reminders(1_000_000_000_001).unwrap(), 0);
    assert_eq!(db.due_reminder_count().unwrap(), 1);
    let occurrences = db.unread_reminder_occurrences().unwrap();
    assert_eq!(occurrences.len(), 1);
    assert_eq!(occurrences[0].reminder_id, "r1");
    assert!(
        uuid::Uuid::parse_str(&occurrences[0].id).is_ok(),
        "occurrence identity must be an opaque UUID, not a reminder/due composite"
    );
    assert!(!occurrences[0].id.contains("r1"));
}

#[test]
fn dismiss_is_idempotent_and_keeps_the_reminder_active() {
    let db = reminder_db();
    db.create_reminder("r1", &draft(10), ReminderOrigin::Manual, 1)
        .unwrap();
    db.materialize_due_reminders(10).unwrap();
    let occurrence = db.unread_reminder_occurrences().unwrap().remove(0);
    assert!(db.dismiss_reminder_occurrence(&occurrence.id, 20).unwrap());
    assert!(!db.dismiss_reminder_occurrence(&occurrence.id, 21).unwrap());
    assert_eq!(db.due_reminder_count().unwrap(), 0);
    assert_eq!(
        db.get_stored_reminder("r1").unwrap().unwrap().state,
        ReminderState::Active
    );
}

#[test]
fn recurring_dismiss_advances_exactly_once_and_next_due_materializes_later() {
    let db = reminder_db();
    let due = Local
        .with_ymd_and_hms(2028, 1, 31, 9, 45, 0)
        .earliest()
        .unwrap()
        .timestamp_millis();
    let mut recurring = draft(due);
    recurring.repeat_every = Some(1);
    recurring.repeat_unit = Some(ReminderRepeatUnit::Months);
    db.create_reminder("r1", &recurring, ReminderOrigin::Manual, 1)
        .unwrap();
    db.materialize_due_reminders(due).unwrap();
    let occurrence = db.unread_reminder_occurrences().unwrap().remove(0);

    assert!(db
        .dismiss_reminder_occurrence(&occurrence.id, due + 1)
        .unwrap());
    let advanced_due = db.get_stored_reminder("r1").unwrap().unwrap().due_at;
    let advanced = Local.timestamp_millis_opt(advanced_due).earliest().unwrap();
    assert_eq!(
        (advanced.year(), advanced.month(), advanced.day()),
        (2028, 2, 29)
    );
    assert!(
        !db.dismiss_reminder_occurrence(&occurrence.id, due + 2)
            .unwrap(),
        "replaying the dismissed occurrence must not advance the series twice"
    );
    assert_eq!(
        db.get_stored_reminder("r1").unwrap().unwrap().due_at,
        advanced_due
    );
    assert_eq!(db.materialize_due_reminders(advanced_due - 1).unwrap(), 0);
    assert_eq!(db.materialize_due_reminders(advanced_due).unwrap(), 1);
    let next_occurrence = db.unread_reminder_occurrences().unwrap().remove(0);
    assert_eq!(next_occurrence.reminder_id, "r1");
    assert_eq!(next_occurrence.due_at, advanced_due);
}

#[test]
fn one_off_completion_is_idempotent_and_terminal() {
    let db = reminder_db();
    db.create_reminder("r1", &draft(10), ReminderOrigin::Manual, 1)
        .unwrap();
    assert!(db.complete_reminder("r1", 10, 20).unwrap());
    assert!(!db.complete_reminder("r1", 10, 21).unwrap());
    let stored = db.get_stored_reminder("r1").unwrap().unwrap();
    assert_eq!(stored.state, ReminderState::Completed);
    assert_eq!(stored.completed_at, Some(20));
    assert_eq!(db.due_reminder_count().unwrap(), 0);
}

#[test]
fn recurring_completion_advances_once_and_months_clamp_to_valid_dates() {
    let db = reminder_db();
    let due = Local
        .with_ymd_and_hms(2028, 1, 31, 9, 45, 0)
        .earliest()
        .unwrap()
        .timestamp_millis();
    let mut recurring = draft(due);
    recurring.repeat_every = Some(1);
    recurring.repeat_unit = Some(ReminderRepeatUnit::Months);
    db.create_reminder("r1", &recurring, ReminderOrigin::Manual, 1)
        .unwrap();

    assert!(db.complete_reminder("r1", due, due + 1).unwrap());
    assert!(
        !db.complete_reminder("r1", due, due + 2).unwrap(),
        "replaying the old schedule generation must not advance twice"
    );
    let stored = db.get_stored_reminder("r1").unwrap().unwrap();
    assert_eq!(stored.state, ReminderState::Active);
    let next = Local
        .timestamp_millis_opt(stored.due_at)
        .earliest()
        .unwrap();
    assert_eq!((next.year(), next.month(), next.day()), (2028, 2, 29));
    assert_eq!((next.hour(), next.minute()), (9, 45));
    assert_eq!(
        db.lock()
            .query_row(
                "SELECT COUNT(*) FROM reminder_due_occurrences
                  WHERE reminder_id='r1' AND status='completed'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
}

#[test]
fn smart_audit_cache_replace_is_idempotent_revision_scoped_and_bounded() {
    let db = reminder_db();
    let hash_a = audit_hash('a');
    let hash_b = audit_hash('b');
    let candidates = audit_candidates(&["Ship the plan", "Call the customer"]);

    db.replace_reminder_audit_results_unchecked(
        "meeting",
        "m1",
        &hash_a,
        "stub-local",
        &candidates,
        10,
    )
    .unwrap();
    assert!(db
        .reminder_audit_cache_matches("meeting", "m1", &hash_a, "stub-local")
        .unwrap());
    assert!(!db
        .reminder_audit_cache_matches("meeting", "m1", &hash_a, "other-engine")
        .unwrap());
    let first = db
        .list_pending_reminder_suggestions("meeting", "m1", &hash_a, "stub-local", usize::MAX)
        .unwrap();
    assert_eq!(first.len(), 2);

    db.replace_reminder_audit_results_unchecked(
        "meeting",
        "m1",
        &hash_a,
        "stub-local",
        &candidates,
        11,
    )
    .unwrap();
    let repeated = db
        .list_pending_reminder_suggestions("meeting", "m1", &hash_a, "stub-local", usize::MAX)
        .unwrap();
    assert_eq!(
        repeated
            .iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        first.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
        "same revision produces stable rows rather than duplicates"
    );
    assert!(repeated.iter().all(|row| row.created_at == 11));

    db.replace_reminder_audit_results_unchecked(
        "meeting",
        "m1",
        &hash_b,
        "stub-local",
        &audit_candidates(&["Revised task"]),
        12,
    )
    .unwrap();
    assert!(!db
        .reminder_audit_cache_matches("meeting", "m1", &hash_a, "stub-local")
        .unwrap());
    assert!(db
        .list_pending_reminder_suggestions("meeting", "m1", &hash_a, "stub-local", 32)
        .unwrap()
        .is_empty());
    assert_eq!(
        db.list_pending_reminder_suggestions("meeting", "m1", &hash_b, "stub-local", 32)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn dismissing_one_smart_suggestion_keeps_the_cache_and_sibling() {
    let db = reminder_db();
    let hash = audit_hash('a');
    db.replace_reminder_audit_results_unchecked(
        "meeting",
        "m1",
        &hash,
        "stub-local",
        &audit_candidates(&["First task", "Second task"]),
        10,
    )
    .unwrap();
    let rows = db
        .list_pending_reminder_suggestions("meeting", "m1", &hash, "stub-local", 32)
        .unwrap();
    let gate_anchor = db
        .get_pending_reminder_suggestion_gate_anchor(&rows[0].id)
        .unwrap()
        .expect("content-free accept/dismiss anchor");
    assert_eq!(gate_anchor.id, rows[0].id);
    assert_eq!(gate_anchor.source_kind, "meeting");
    assert_eq!(gate_anchor.source_id, "m1");

    assert!(db
        .dismiss_pending_reminder_suggestion(
            &gate_anchor.id,
            &gate_anchor.source_kind,
            &gate_anchor.source_id,
            &hash,
        )
        .unwrap());
    assert!(!db
        .dismiss_pending_reminder_suggestion(&rows[0].id, "meeting", "m1", &hash)
        .unwrap());
    assert!(db
        .reminder_audit_cache_matches("meeting", "m1", &hash, "stub-local")
        .unwrap());
    let remaining = db
        .list_pending_reminder_suggestions("meeting", "m1", &hash, "stub-local", 32)
        .unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].id, rows[1].id);
}

#[test]
fn smart_suggestions_promote_once_and_siblings_can_promote_sequentially() {
    let db = reminder_db();
    let hash = audit_hash('a');
    db.replace_reminder_audit_results_unchecked(
        "meeting",
        "m1",
        &hash,
        "stub-local",
        &audit_candidates(&["First task", "Second task"]),
        10,
    )
    .unwrap();
    let rows = db
        .list_pending_reminder_suggestions("meeting", "m1", &hash, "stub-local", 32)
        .unwrap();
    let mut first_draft = draft(2_000_000_000_000);
    first_draft.title = "Edited before accepting".into();
    first_draft.sources.push(ReminderSourceAnchor {
        kind: "note".into(),
        id: "n-extra".into(),
    });

    assert!(db
        .promote_pending_reminder_suggestion(ReminderSuggestionPromotion {
            suggestion_id: &rows[0].id,
            expected_source_kind: "meeting",
            expected_source_id: "m1",
            expected_content_hash: &hash,
            reminder_id: "r-smart-1",
            draft: &first_draft,
            now: 20,
        })
        .unwrap());
    assert!(!db
        .promote_pending_reminder_suggestion(ReminderSuggestionPromotion {
            suggestion_id: &rows[0].id,
            expected_source_kind: "meeting",
            expected_source_id: "m1",
            expected_content_hash: &hash,
            reminder_id: "r-replay",
            draft: &first_draft,
            now: 21,
        })
        .unwrap());
    assert!(db
        .reminder_audit_cache_matches("meeting", "m1", &hash, "stub-local")
        .unwrap());
    let after_first = db
        .list_pending_reminder_suggestions("meeting", "m1", &hash, "stub-local", 32)
        .unwrap();
    assert_eq!(after_first.len(), 1, "the sibling remains reviewable");

    let mut second_draft = draft(2_000_000_100_000);
    second_draft.title = "Second accepted task".into();
    assert!(db
        .promote_pending_reminder_suggestion(ReminderSuggestionPromotion {
            suggestion_id: &after_first[0].id,
            expected_source_kind: "meeting",
            expected_source_id: "m1",
            expected_content_hash: &hash,
            reminder_id: "r-smart-2",
            draft: &second_draft,
            now: 22,
        })
        .unwrap());
    assert!(db
        .list_pending_reminder_suggestions("meeting", "m1", &hash, "stub-local", 32)
        .unwrap()
        .is_empty());

    let first = db.get_stored_reminder("r-smart-1").unwrap().unwrap();
    assert_eq!(first.origin, ReminderOrigin::Smart);
    assert_eq!(first.title, "Edited before accepting");
    assert!(first.sources.contains(&ReminderSourceAnchor {
        kind: "meeting".into(),
        id: "m1".into(),
    }));
    assert!(first.sources.contains(&ReminderSourceAnchor {
        kind: "note".into(),
        id: "n-extra".into(),
    }));
    assert_eq!(
        db.lock()
            .query_row("SELECT COUNT(*) FROM reminders", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        2,
        "replay must not create a third reminder"
    );
}

#[test]
fn canonical_source_edits_purge_only_derived_suggestions_not_user_reminders() {
    let db = reminder_db();
    db.insert_meeting(&Meeting {
        id: "m1".into(),
        started_at: "2026-07-29T10:00:00Z".into(),
        ended_at: None,
        title: Some("Planning".into()),
        duration_s: 0,
        audio_path: None,
        status: MeetingStatus::Summarized,
        folder_id: None,
    })
    .unwrap();
    db.upsert_note(&NoteRecord {
        meeting_id: "m1".into(),
        provider_id: "local".into(),
        markdown: "- [ ] Ship the plan".into(),
        created_at: "2026-07-29T10:05:00Z".into(),
        exported_path: None,
        model_requested: None,
        model_served: None,
        gateway_host: None,
    })
    .unwrap();
    let mut reminder = draft(2_000_000_000_000);
    reminder.sources.push(ReminderSourceAnchor {
        kind: "meeting".into(),
        id: "m1".into(),
    });
    db.create_reminder("r1", &reminder, ReminderOrigin::Smart, 1)
        .unwrap();
    db.replace_reminder_audit_results_unchecked(
        "meeting",
        "m1",
        &audit_hash('a'),
        "stub-local",
        &audit_candidates(&["Ship the plan"]),
        1,
    )
    .unwrap();

    db.upsert_note(&NoteRecord {
        meeting_id: "m1".into(),
        provider_id: "local".into(),
        markdown: "- [ ] Ship the revised plan".into(),
        created_at: "2026-07-29T10:06:00Z".into(),
        exported_path: None,
        model_requested: None,
        model_served: None,
        gateway_host: None,
    })
    .unwrap();

    let derived = derived_reminder_audit_count(&db);
    assert_eq!(
        derived, 0,
        "source edit must transactionally purge derived plaintext"
    );
    let reminders: i64 = db
        .lock()
        .query_row("SELECT COUNT(*) FROM reminders WHERE id='r1'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        reminders, 1,
        "an accepted/user-owned reminder is an independent domain"
    );
}

#[test]
fn relock_enqueues_empty_meeting_and_authored_note_sources_without_content_updates() {
    let db = reminder_db();
    db.insert_folder(&Folder {
        id: "f-private".into(),
        name: "Private".into(),
        path: "Private".into(),
        parent_id: None,
        locked: false,
        created_at: "2026-07-29T09:00:00Z".into(),
    })
    .unwrap();
    db.insert_meeting(&Meeting {
        id: "m-title-only".into(),
        started_at: "2026-07-29T10:00:00Z".into(),
        ended_at: None,
        title: Some("Title only".into()),
        duration_s: 0,
        audio_path: None,
        status: MeetingStatus::Summarized,
        folder_id: None,
    })
    .unwrap();
    db.upsert_note(&NoteRecord {
        meeting_id: "m-title-only".into(),
        provider_id: "local".into(),
        markdown: String::new(),
        created_at: "2026-07-29T10:05:00Z".into(),
        exported_path: None,
        model_requested: None,
        model_served: None,
        gateway_host: None,
    })
    .unwrap();
    db.set_note_folder("m-title-only", Some("f-private"))
        .unwrap();
    db.insert_note("n-empty", "f-private", "Empty.md", "Empty", "", 1)
        .unwrap();
    expect_source_invalidations(&db, &[("meeting", "m-title-only"), ("note", "n-empty")]);

    db.set_folder_locked("f-private", true, Some(b"wrapped"))
        .unwrap();
    let mut folders = std::collections::HashSet::new();
    folders.insert("f-private".to_string());
    db.blank_sealed_notes_in_folders(&folders).unwrap();

    expect_source_invalidations(&db, &[("meeting", "m-title-only"), ("note", "n-empty")]);
}

#[test]
fn relock_and_startup_reconcile_purge_derived_audits_but_keep_reminders() {
    let db = reminder_db();
    seed_reminder_audit_source(&db, "f-private", "m1");
    let mut accepted = draft(2_000_000_000_000);
    accepted.sources.push(ReminderSourceAnchor {
        kind: "meeting".into(),
        id: "m1".into(),
    });
    db.create_reminder("r-accepted", &accepted, ReminderOrigin::Smart, 1)
        .unwrap();
    db.replace_reminder_audit_results_unchecked(
        "meeting",
        "m1",
        &audit_hash('a'),
        "stub-local",
        &audit_candidates(&["Pending before relock"]),
        10,
    )
    .unwrap();
    assert_eq!(derived_reminder_audit_count(&db), 2);
    db.lock()
        .execute(
            "INSERT INTO reminder_suggestion_decisions
               (source_kind,source_id,content_hash,candidate_key,decision,reminder_id,decided_at)
             VALUES ('meeting','m1',?1,?2,'dismissed',NULL,10)",
            rusqlite::params![audit_hash('a'), audit_hash('d')],
        )
        .unwrap();

    db.set_folder_locked("f-private", true, Some(b"wrapped"))
        .unwrap();
    let mut folders = std::collections::HashSet::new();
    folders.insert("f-private".to_string());
    db.blank_sealed_notes_in_folders(&folders).unwrap();
    assert_eq!(
        derived_reminder_audit_count(&db),
        0,
        "relock transaction must purge cache and suggestion"
    );
    assert_eq!(
        db.lock()
            .query_row(
                "SELECT COUNT(*) FROM reminder_suggestion_decisions",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0,
        "relock transaction must purge unkeyed decision fingerprints"
    );
    assert!(db.get_stored_reminder("r-accepted").unwrap().is_some());

    // Simulate candidates derived while the folder was session-unlocked immediately before a
    // process crash. The on-disk folder remains locked, so startup reconciliation must purge them.
    db.replace_reminder_audit_results_unchecked(
        "meeting",
        "m1",
        &audit_hash('b'),
        "stub-local",
        &audit_candidates(&["Pending at crash"]),
        11,
    )
    .unwrap();
    assert_eq!(derived_reminder_audit_count(&db), 2);
    db.lock()
        .execute(
            "INSERT INTO reminder_suggestion_decisions
               (source_kind,source_id,content_hash,candidate_key,decision,reminder_id,decided_at)
             VALUES ('meeting','m1',?1,?2,'dismissed',NULL,11)",
            rusqlite::params![audit_hash('b'), audit_hash('e')],
        )
        .unwrap();
    db.reblank_locked_folders_at_rest().unwrap();
    assert_eq!(
        derived_reminder_audit_count(&db),
        0,
        "startup reconcile must purge crash-window audit plaintext"
    );
    assert_eq!(
        db.lock()
            .query_row(
                "SELECT COUNT(*) FROM reminder_suggestion_decisions",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0,
        "startup reconcile must purge crash-window decision fingerprints"
    );
    assert!(db.get_stored_reminder("r-accepted").unwrap().is_some());
}

#[test]
fn reminder_audit_cas_rejects_a_stale_canonical_revision() {
    let db = reminder_db();
    seed_reminder_audit_source(&db, "f1", "m1");
    let before_hash = crate::storage::reminder_store::canonical_reminder_source_hash(
        "Planning",
        "- [ ] Ship the plan",
        Some(""),
        &[],
    );
    assert!(db
        .replace_reminder_audit_results(
            "meeting",
            "m1",
            &before_hash,
            "stub-local",
            &audit_candidates(&["Ship the plan"]),
            10,
        )
        .unwrap());

    db.upsert_note(&NoteRecord {
        meeting_id: "m1".into(),
        provider_id: "local".into(),
        markdown: "- [ ] Ship the revised plan".into(),
        created_at: "2026-07-29T10:06:00Z".into(),
        exported_path: None,
        model_requested: None,
        model_served: None,
        gateway_host: None,
    })
    .unwrap();
    assert!(!db
        .replace_reminder_audit_results(
            "meeting",
            "m1",
            &before_hash,
            "stub-local",
            &audit_candidates(&["Ship the plan"]),
            11,
        )
        .unwrap());
    assert_eq!(
        derived_reminder_audit_count(&db),
        0,
        "a stale post-inference revision must not be reinserted after its edit trigger purged it"
    );
}

#[test]
fn smart_decision_fingerprints_are_purged_while_accepted_reminders_survive() {
    let db = reminder_db();
    let hash = audit_hash('a');
    let candidates = audit_candidates(&["Accepted task", "Dismissed task"]);
    db.replace_reminder_audit_results_unchecked(
        "meeting",
        "m1",
        &hash,
        "engine-a",
        &candidates,
        10,
    )
    .unwrap();
    let rows = db
        .list_pending_reminder_suggestions("meeting", "m1", &hash, "engine-a", 32)
        .unwrap();
    let accepted = rows
        .iter()
        .find(|row| row.title == "Accepted task")
        .unwrap();
    let dismissed = rows
        .iter()
        .find(|row| row.title == "Dismissed task")
        .unwrap();
    assert!(db
        .promote_pending_reminder_suggestion(ReminderSuggestionPromotion {
            suggestion_id: &accepted.id,
            expected_source_kind: "meeting",
            expected_source_id: "m1",
            expected_content_hash: &hash,
            reminder_id: "r-accepted",
            draft: &draft(2_000_000_000_000),
            now: 20,
        })
        .unwrap());
    assert!(db
        .dismiss_pending_reminder_suggestion(&dismissed.id, "meeting", "m1", &hash)
        .unwrap());

    {
        let mut conn = db.lock();
        let tx = conn.transaction().unwrap();
        Db::purge_all_reminder_derived_tx(&tx).unwrap();
        tx.commit().unwrap();
    }
    assert_eq!(
        db.list_stored_reminders().unwrap().len(),
        1,
        "purging derived fingerprints must preserve the accepted canonical reminder"
    );
    let decisions: i64 = db
        .lock()
        .query_row(
            "SELECT COUNT(*) FROM reminder_suggestion_decisions",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        decisions, 0,
        "unkeyed content-hash and candidate-key fingerprints must not survive a seal/relock purge"
    );
    db.replace_reminder_audit_results_unchecked(
        "meeting",
        "m1",
        &hash,
        "engine-b",
        &candidates,
        30,
    )
    .unwrap();
    let regenerated = db
        .list_pending_reminder_suggestions("meeting", "m1", &hash, "engine-b", 32)
        .unwrap();
    assert_eq!(
        regenerated.len(),
        2,
        "purged decision fingerprints must not survive to suppress a later unlocked audit"
    );

    assert!(db.delete_reminder("r-accepted").unwrap());
    {
        let mut conn = db.lock();
        let tx = conn.transaction().unwrap();
        Db::purge_all_reminder_derived_tx(&tx).unwrap();
        tx.commit().unwrap();
    }
    db.replace_reminder_audit_results_unchecked(
        "meeting",
        "m1",
        &hash,
        "engine-c",
        &candidates,
        40,
    )
    .unwrap();
    let regenerated = db
        .list_pending_reminder_suggestions("meeting", "m1", &hash, "engine-c", 32)
        .unwrap();
    assert_eq!(regenerated.len(), 2);
}

#[test]
fn identical_source_updates_keep_audit_but_real_edits_purge_it() {
    let db = reminder_db();
    seed_reminder_audit_source(&db, "f1", "m1");
    db.replace_segments(
        "m1",
        &[crate::transcribe::types::Segment {
            idx: 0,
            start_s: 0.0,
            end_s: 1.0,
            text: "We should follow up".into(),
            speaker: None,
            confidence: None,
        }],
    )
    .unwrap();
    let hash = audit_hash('a');
    let candidates = audit_candidates(&["Ship the plan"]);

    for identical_update in [
        "UPDATE notes SET markdown=markdown WHERE meeting_id='m1'",
        "UPDATE segments SET text=text WHERE meeting_id='m1'",
        "UPDATE meetings SET manual_notes=manual_notes,title=title WHERE id='m1'",
    ] {
        db.replace_reminder_audit_results_unchecked(
            "meeting",
            "m1",
            &hash,
            "engine",
            &candidates,
            10,
        )
        .unwrap();
        db.lock().execute(identical_update, []).unwrap();
        assert_eq!(
            derived_reminder_audit_count(&db),
            2,
            "identical update unexpectedly invalidated: {identical_update}"
        );
    }
    db.lock()
        .execute(
            "UPDATE segments SET start_s=start_s+0.25 WHERE meeting_id='m1' AND idx=0",
            [],
        )
        .unwrap();
    assert_eq!(
        derived_reminder_audit_count(&db),
        0,
        "a non-text canonical segment edit must invalidate derived suggestions"
    );

    db.replace_reminder_audit_results_unchecked("meeting", "m1", &hash, "engine", &candidates, 11)
        .unwrap();
    db.lock()
        .execute(
            "UPDATE segments SET echo_suppressed=1 WHERE meeting_id='m1' AND idx=0",
            [],
        )
        .unwrap();
    assert_eq!(
        derived_reminder_audit_count(&db),
        0,
        "a suppression-only presentation edit must invalidate derived suggestions"
    );

    db.replace_reminder_audit_results_unchecked("meeting", "m1", &hash, "engine", &candidates, 11)
        .unwrap();
    db.lock()
        .execute(
            "UPDATE meetings SET manual_notes='changed' WHERE id='m1'",
            [],
        )
        .unwrap();
    assert_eq!(derived_reminder_audit_count(&db), 0);

    db.insert_note("n1", "f1", "Note", "Note", "- [ ] Call", 1)
        .unwrap();
    db.replace_reminder_audit_results_unchecked("note", "n1", &hash, "engine", &candidates, 10)
        .unwrap();
    db.lock()
        .execute(
            "UPDATE documents SET text=text,title=title,kind=kind WHERE id='n1'",
            [],
        )
        .unwrap();
    assert_eq!(derived_reminder_audit_count(&db), 2);
    db.lock()
        .execute("UPDATE documents SET title='Renamed' WHERE id='n1'", [])
        .unwrap();
    assert_eq!(derived_reminder_audit_count(&db), 0);
}

#[test]
fn latest_note_winner_changes_and_authored_note_fallback_renames_invalidate() {
    let db = reminder_db();
    seed_reminder_audit_source(&db, "f1", "m1");
    db.upsert_note(&NoteRecord {
        meeting_id: "m1".into(),
        provider_id: "z-provider".into(),
        markdown: "- [ ] Follow the newer provider".into(),
        created_at: "2026-07-29T10:06:00Z".into(),
        exported_path: None,
        model_requested: None,
        model_served: None,
        gateway_host: None,
    })
    .unwrap();
    assert_eq!(
        db.latest_reminder_audit_markdown("m1").unwrap().as_deref(),
        Some("- [ ] Follow the newer provider")
    );
    let winner_hash = crate::storage::reminder_store::canonical_reminder_source_hash(
        "Planning",
        "- [ ] Follow the newer provider",
        Some(""),
        &[],
    );
    assert!(db
        .replace_reminder_audit_results(
            "meeting",
            "m1",
            &winner_hash,
            "engine",
            &audit_candidates(&["Follow the newer provider"]),
            10,
        )
        .unwrap());

    db.lock()
        .execute(
            "UPDATE notes SET created_at='2026-07-29T10:07:00Z'
              WHERE meeting_id='m1' AND provider_id='local'",
            [],
        )
        .unwrap();
    assert_eq!(
        db.latest_reminder_audit_markdown("m1").unwrap().as_deref(),
        Some("- [ ] Ship the plan"),
        "timestamp-only edit must deterministically switch the canonical provider winner"
    );
    assert_eq!(
        derived_reminder_audit_count(&db),
        0,
        "winner-switch update must purge the old provider's derived plaintext"
    );
    assert!(!db
        .replace_reminder_audit_results(
            "meeting",
            "m1",
            &winner_hash,
            "engine",
            &audit_candidates(&["Follow the newer provider"]),
            11,
        )
        .unwrap());

    db.insert_note("n1", "f1", "Fallback name", "", "- [ ] Call", 1)
        .unwrap();
    let note_hash = crate::storage::reminder_store::canonical_reminder_source_hash(
        "Fallback name",
        "- [ ] Call",
        None,
        &[],
    );
    assert!(db
        .replace_reminder_audit_results(
            "note",
            "n1",
            &note_hash,
            "engine",
            &audit_candidates(&["Call"]),
            12,
        )
        .unwrap());
    db.lock()
        .execute(
            "UPDATE documents SET name='Renamed fallback' WHERE id='n1'",
            [],
        )
        .unwrap();
    assert_eq!(
        derived_reminder_audit_count(&db),
        0,
        "fallback-name-only edit must invalidate authored-note suggestions"
    );
    assert!(!db
        .replace_reminder_audit_results(
            "note",
            "n1",
            &note_hash,
            "engine",
            &audit_candidates(&["Call"]),
            13,
        )
        .unwrap());
}

#[test]
fn meeting_and_authored_note_folder_moves_purge_pending_plaintext() {
    let db = reminder_db();
    seed_reminder_audit_source(&db, "f1", "m1");
    db.insert_folder(&Folder {
        id: "f2".into(),
        name: "Moved".into(),
        path: "Moved".into(),
        parent_id: None,
        locked: false,
        created_at: "2026-07-29T09:01:00Z".into(),
    })
    .unwrap();
    let hash = audit_hash('f');
    let candidates = audit_candidates(&["Move-safe"]);
    db.replace_reminder_audit_results_unchecked("meeting", "m1", &hash, "engine", &candidates, 10)
        .unwrap();
    db.lock()
        .execute("UPDATE notes SET folder_id='f2' WHERE meeting_id='m1'", [])
        .unwrap();
    assert_eq!(derived_reminder_audit_count(&db), 0);

    db.insert_note("n1", "f1", "Note", "Note", "- [ ] Move-safe", 1)
        .unwrap();
    db.replace_reminder_audit_results_unchecked("note", "n1", &hash, "engine", &candidates, 11)
        .unwrap();
    db.lock()
        .execute("UPDATE documents SET folder_id='f2' WHERE id='n1'", [])
        .unwrap();
    assert_eq!(
        derived_reminder_audit_count(&db),
        0,
        "folder ownership changes must purge source-derived reminder plaintext"
    );
}

#[test]
fn non_schedule_edits_preserve_dismissed_occurrences() {
    let db = reminder_db();
    let mut original = draft(10);
    original.sources.push(ReminderSourceAnchor {
        kind: "meeting".into(),
        id: "m1".into(),
    });
    db.create_reminder("r1", &original, ReminderOrigin::Manual, 1)
        .unwrap();
    db.materialize_due_reminders(10).unwrap();
    let occurrence = db.unread_reminder_occurrences().unwrap().remove(0);
    db.dismiss_reminder_occurrence(&occurrence.id, 11).unwrap();

    let mut edited = original;
    edited.title = "New title".into();
    edited.sources.push(ReminderSourceAnchor {
        kind: "note".into(),
        id: "n1".into(),
    });
    assert!(db.update_reminder("r1", &edited, 12).unwrap());
    assert_eq!(db.materialize_due_reminders(20).unwrap(), 0);
    let status: String = db
        .lock()
        .query_row(
            "SELECT status FROM reminder_due_occurrences WHERE reminder_id='r1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(status, "dismissed");
}

#[test]
fn schedule_change_clears_history_and_reused_due_materializes_once() {
    let db = reminder_db();
    let original = draft(10);
    db.create_reminder("r1", &original, ReminderOrigin::Manual, 1)
        .unwrap();
    db.materialize_due_reminders(10).unwrap();
    let occurrence = db.unread_reminder_occurrences().unwrap().remove(0);
    db.dismiss_reminder_occurrence(&occurrence.id, 11).unwrap();

    let mut changed = original.clone();
    changed.due_at = 20;
    db.update_reminder("r1", &changed, 12).unwrap();
    db.update_reminder("r1", &original, 13).unwrap();
    assert_eq!(db.materialize_due_reminders(13).unwrap(), 1);
    assert_eq!(db.materialize_due_reminders(14).unwrap(), 0);
    assert_eq!(db.due_reminder_count().unwrap(), 1);
}

#[test]
fn overdue_recurrence_skips_missed_cycles_to_first_future_due() {
    let db = reminder_db();
    let due = Local
        .with_ymd_and_hms(2026, 1, 1, 9, 0, 0)
        .earliest()
        .unwrap()
        .timestamp_millis();
    let now = Local
        .with_ymd_and_hms(2026, 2, 1, 12, 0, 0)
        .earliest()
        .unwrap()
        .timestamp_millis();
    let mut recurring = draft(due);
    recurring.repeat_every = Some(1);
    recurring.repeat_unit = Some(ReminderRepeatUnit::Days);
    db.create_reminder("r1", &recurring, ReminderOrigin::Manual, 1)
        .unwrap();
    assert!(db.complete_reminder("r1", due, now).unwrap());
    let next = db.get_stored_reminder("r1").unwrap().unwrap().due_at;
    assert!(next > now);
    let local = Local.timestamp_millis_opt(next).earliest().unwrap();
    assert_eq!((local.year(), local.month(), local.day()), (2026, 2, 2));
}

#[test]
fn recurrence_terminates_cleanly_at_supported_horizon() {
    let db = reminder_db();
    let due = Local
        .with_ymd_and_hms(2199, 12, 31, 9, 0, 0)
        .earliest()
        .unwrap()
        .timestamp_millis();
    let mut recurring = draft(due);
    recurring.repeat_every = Some(1);
    recurring.repeat_unit = Some(ReminderRepeatUnit::Years);
    db.create_reminder("r1", &recurring, ReminderOrigin::Manual, 1)
        .unwrap();
    assert!(db.complete_reminder("r1", due, due + 1).unwrap());
    let stored = db.get_stored_reminder("r1").unwrap().unwrap();
    assert_eq!(stored.state, ReminderState::Completed);
    assert_eq!(stored.completed_at, Some(due + 1));
}
