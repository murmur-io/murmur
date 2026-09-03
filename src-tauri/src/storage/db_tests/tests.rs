use super::*;
use crate::storage::models::{Meeting, MeetingStatus, NoteRecord, SourceKind};

fn mem_db() -> Db {
    // In-memory DB shares the same open/migrate path as on-disk. The vec0 module must be
    // auto-registered BEFORE the connection is opened (migrate() creates the vec_chunks vtab),
    // exactly as `open_with_key` does for real handles.
    register_vec_extension();
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    let db = Db {
        conn: Mutex::new(conn),
    };
    db.migrate().unwrap();
    db
}

fn sample_meeting(id: &str, started_at: &str) -> Meeting {
    Meeting {
        id: id.to_string(),
        started_at: started_at.to_string(),
        ended_at: None,
        title: None,
        duration_s: 0,
        audio_path: None,
        status: MeetingStatus::Draft,
        folder_id: None,
    }
}

#[test]
fn migrate_is_idempotent() {
    let db = mem_db();
    db.migrate().unwrap();
    db.migrate().unwrap();

    // Feature B (saved views): the additive `saved_views` table must exist after migrate and
    // STILL exist (idempotently) after a re-migrate — `CREATE TABLE IF NOT EXISTS` never drops.
    let has_saved_views = |db: &Db| -> bool {
        db.lock()
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'saved_views'",
                [],
                |_| Ok(()),
            )
            .optional()
            .unwrap()
            .is_some()
    };
    assert!(
        has_saved_views(&db),
        "saved_views table missing after migrate"
    );
    db.migrate().unwrap();
    assert!(
        has_saved_views(&db),
        "saved_views table missing after a second migrate (idempotency broken)"
    );

    // Feature C (typed properties): the additive `note_folder_schemas` table must exist after
    // migrate and STILL exist idempotently after a re-migrate — additive, never destructive.
    let has_schemas = |db: &Db| -> bool {
        db.lock()
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'note_folder_schemas'",
                [],
                |_| Ok(()),
            )
            .optional()
            .unwrap()
            .is_some()
    };
    assert!(
        has_schemas(&db),
        "note_folder_schemas table missing after migrate"
    );
    db.migrate().unwrap();
    assert!(
        has_schemas(&db),
        "note_folder_schemas table missing after a second migrate (idempotency broken)"
    );

    // Export-collision guard: the additive `exported_hash` column must exist on BOTH exporting
    // tables after migrate and still be there (idempotently) after the re-migrates above —
    // `add_column_if_missing` never duplicates or drops.
    let has_column = |db: &Db, table: &str| -> bool {
        db.lock()
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .filter_map(|r| r.ok())
            .any(|c| c == "exported_hash")
    };
    assert!(has_column(&db, "notes"), "notes.exported_hash missing");
    assert!(
        has_column(&db, "documents"),
        "documents.exported_hash missing"
    );
}

/// A late migration failure must roll back the entire schema/data upgrade, not leave an earlier
/// additive table or backfill installed. A deliberately malformed attachment table survives the
/// failed attempt byte-for-byte; after removing that external obstruction, the same connection
/// migrates successfully and remains idempotent.
#[test]
fn migrate_rolls_back_every_earlier_step_when_a_late_attachment_step_fails() {
    register_vec_extension();
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         CREATE TABLE note_attachments (sentinel TEXT NOT NULL);
         INSERT INTO note_attachments(sentinel) VALUES ('keep-me');",
    )
    .unwrap();
    let db = Db {
        conn: Mutex::new(conn),
    };
    let schema_snapshot = |db: &Db| -> Vec<(String, String, String)> {
        let conn = db.lock();
        let mut stmt = conn
            .prepare(
                "SELECT type, name, COALESCE(sql, '')
                   FROM sqlite_master
                  WHERE name NOT LIKE 'sqlite_%'
                  ORDER BY type, name",
            )
            .unwrap();
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .map(|row| row.unwrap())
            .collect()
    };
    let before = schema_snapshot(&db);

    assert!(
        db.migrate().is_err(),
        "the malformed late attachment substrate must reject migration"
    );
    assert_eq!(
        schema_snapshot(&db),
        before,
        "a late failure must roll back every earlier schema step"
    );
    assert_eq!(
        db.lock()
            .query_row("SELECT sentinel FROM note_attachments", [], |row| row
                .get::<_, String>(0),)
            .unwrap(),
        "keep-me",
        "the pre-existing obstruction must remain untouched"
    );

    db.lock()
        .execute_batch("DROP TABLE note_attachments;")
        .unwrap();
    db.migrate().unwrap();
    db.migrate().unwrap();
    assert!(
        db.lock()
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='note_attachments'",
                [],
                |_| Ok(()),
            )
            .optional()
            .unwrap()
            .is_some(),
        "the unobstructed retry must install the late schema"
    );
}

/// Dispatch correlation is an additive upgrade: legacy share rows remain valid with NULL stamps,
/// while both tables gain the nullable column and the ledger gains a partial unique index.
#[test]
fn migrate_adds_dispatch_correlation_to_legacy_share_tables() {
    register_vec_extension();
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         CREATE TABLE share_egress_log (
           id         INTEGER PRIMARY KEY AUTOINCREMENT,
           ts         INTEGER NOT NULL,
           host       TEXT NOT NULL,
           kind       TEXT NOT NULL,
           byte_count INTEGER NOT NULL DEFAULT 0
         );
         INSERT INTO share_egress_log(ts, host, kind, byte_count)
           VALUES (1, 'relay.example', 'legacy_one', 10),
                  (2, 'relay.example', 'legacy_two', 20);
         CREATE TABLE org_shares (
           id             TEXT PRIMARY KEY,
           org_id         TEXT NOT NULL,
           meeting_id     TEXT,
           document_id    TEXT,
           kind           TEXT NOT NULL,
           title          TEXT,
           rev            INTEGER NOT NULL DEFAULT 1,
           generation     INTEGER NOT NULL DEFAULT 1,
           content_sha256 BLOB,
           item_id        TEXT,
           scrub          INTEGER NOT NULL DEFAULT 1 CHECK(scrub IN (0,1)),
           state          TEXT NOT NULL DEFAULT 'queued'
                          CHECK (state IN ('queued','uploaded','failed','revoke_pending','revoked')),
           last_error     TEXT,
           created_at     TEXT NOT NULL,
           updated_at     TEXT NOT NULL
         );
         INSERT INTO org_shares(
           id, org_id, kind, state, created_at, updated_at
         ) VALUES (
           'legacy-share', 'legacy-org', 'note', 'queued', '2026-08-13T00:00:00Z',
           '2026-08-13T00:00:00Z'
         );",
    )
    .unwrap();
    let db = Db {
        conn: Mutex::new(conn),
    };

    db.migrate().unwrap();
    db.migrate().unwrap();

    let conn = db.lock();
    for table in ["org_shares", "share_egress_log"] {
        let dispatch_columns: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = 'dispatch_id'",
                rusqlite::params![table],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(dispatch_columns, 1, "{table}.dispatch_id must be additive");
    }
    let legacy_null_rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM share_egress_log WHERE dispatch_id IS NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(legacy_null_rows, 2, "legacy NULL ledger rows must coexist");
    let legacy_org_dispatch: Option<String> = conn
        .query_row(
            "SELECT dispatch_id FROM org_shares WHERE id = 'legacy-share'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(legacy_org_dispatch, None);
    let (unique, partial): (i64, i64) = conn
        .query_row(
            "SELECT \"unique\", partial
               FROM pragma_index_list('share_egress_log')
              WHERE name = 'idx_share_egress_log_dispatch_id'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((unique, partial), (1, 1));
    let indexed_column: String = conn
        .query_row(
            "SELECT name FROM pragma_index_info('idx_share_egress_log_dispatch_id')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(indexed_column, "dispatch_id");
}

#[test]
fn share_egress_dispatch_rejects_duplicate_non_null_id_but_allows_legacy_nulls() {
    let db = mem_db();
    let mut conn = db.lock();
    conn.execute(
        "INSERT INTO share_egress_log(ts, host, kind, byte_count) VALUES (1, 'relay', 'legacy', 0)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO share_egress_log(ts, host, kind, byte_count) VALUES (2, 'relay', 'legacy', 0)",
        [],
    )
    .unwrap();
    {
        let tx = conn.transaction().unwrap();
        insert_share_egress_dispatch_tx(&tx, 3, "relay", "org_share_publish", 42, "dispatch-1")
            .unwrap();
        tx.commit().unwrap();
    }
    {
        let tx = conn.transaction().unwrap();
        let duplicate =
            insert_share_egress_dispatch_tx(&tx, 4, "relay", "org_share_publish", 43, "dispatch-1");
        assert!(matches!(duplicate, Err(crate::error::AppError::Storage(_))));
        tx.rollback().unwrap();
    }

    let (null_rows, stamped_rows): (i64, i64) = conn
        .query_row(
            "SELECT SUM(dispatch_id IS NULL), SUM(dispatch_id = 'dispatch-1')
               FROM share_egress_log",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((null_rows, stamped_rows), (2, 1));
}

#[test]
fn share_egress_dispatch_transaction_rollback_leaves_no_ledger_row() {
    let db = mem_db();
    let mut conn = db.lock();
    let row_id = {
        let tx = conn.transaction().unwrap();
        let row_id = insert_share_egress_dispatch_tx(
            &tx,
            10,
            "relay",
            "org_share_publish",
            99,
            "dispatch-rollback",
        )
        .unwrap();
        let visible_inside_tx: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM share_egress_log WHERE id = ?1",
                rusqlite::params![row_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(visible_inside_tx, 1);
        tx.rollback().unwrap();
        row_id
    };
    let persisted: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM share_egress_log WHERE id = ?1",
            rusqlite::params![row_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(persisted, 0);
}

#[test]
fn outbound_content_and_delete_dispatch_rollback_before_permit_mint() {
    let db = mem_db();
    let share_id = "11111111-1111-4111-8111-111111111111";
    let owner = "22222222-2222-4222-8222-222222222222";
    assert!(db
        .insert_outbound_share_attempt(
            share_id,
            Some("dispatch-meeting"),
            None,
            "link",
            1,
            owner,
            "2026-08-21T00:00:00Z",
        )
        .unwrap());
    db.lock()
        .execute_batch(
            "CREATE TRIGGER reject_outbound_dispatch_ledger
             BEFORE INSERT ON share_egress_log
             BEGIN SELECT RAISE(ABORT, 'dispatch ledger fault'); END;",
        )
        .unwrap();

    assert!(db
        .persist_outbound_content_dispatch(
            share_id,
            owner,
            "link",
            1,
            "content-dispatch-failed",
            &[3; 32],
            &[4; 32],
            1,
            "relay",
            "share_create",
            3,
        )
        .is_err());
    let (state, dispatch): (String, Option<String>) = db
        .lock()
        .query_row(
            "SELECT state,dispatch_id FROM outbound_shares WHERE share_id=?1",
            [share_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(state, "create_pending");
    assert_eq!(dispatch, None);

    db.lock()
        .execute_batch("DROP TRIGGER reject_outbound_dispatch_ledger;")
        .unwrap();
    assert!(db
        .persist_outbound_content_dispatch(
            share_id,
            owner,
            "link",
            1,
            "content-dispatch-ok",
            &[3; 32],
            &[4; 32],
            2,
            "relay",
            "share_create",
            3,
        )
        .unwrap());
    db.lock()
        .execute_batch(
            "CREATE TRIGGER reject_outbound_delete_ledger
             BEFORE INSERT ON share_egress_log
             BEGIN SELECT RAISE(ABORT, 'delete ledger fault'); END;",
        )
        .unwrap();
    assert!(db
        .persist_outbound_delete_dispatch(
            share_id,
            owner,
            "link",
            1,
            "delete-dispatch-failed",
            3,
            "relay",
        )
        .is_err());
    let (state, dispatch): (String, Option<String>) = db
        .lock()
        .query_row(
            "SELECT state,dispatch_id FROM outbound_shares WHERE share_id=?1",
            [share_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(state, "create_pending");
    assert_eq!(dispatch.as_deref(), Some("content-dispatch-ok"));
}

#[test]
fn org_recovery_read_dispatch_binds_exact_content_free_page_witness() {
    let db = mem_db();
    let mut conn = db.lock();
    let tx = conn.transaction().unwrap();
    let row_id = insert_org_read_egress_dispatch_tx(
        &tx,
        11,
        "relay.example",
        "org_document_history_read",
        "read-dispatch-1",
        "org-1",
        "doc-1",
        200,
        200,
    )
    .unwrap();
    tx.commit().unwrap();
    let witness: (String, String, String, String, i64, i64, i64) = conn
        .query_row(
            "SELECT host, kind, dispatch_id, org_id, since_seq, page_limit, byte_count
               FROM share_egress_log WHERE id = ?1 AND doc_id = 'doc-1'",
            rusqlite::params![row_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        witness,
        (
            "relay.example".into(),
            "org_document_history_read".into(),
            "read-dispatch-1".into(),
            "org-1".into(),
            200,
            200,
            0,
        )
    );
}

#[test]
fn share_egress_dispatch_rejects_blank_identifiers_and_labels() {
    let db = mem_db();
    let mut conn = db.lock();
    let tx = conn.transaction().unwrap();
    for (host, kind, dispatch_id) in [
        ("relay", "org_share_publish", "  "),
        ("\t", "org_share_publish", "dispatch-host"),
        ("relay", "\n", "dispatch-kind"),
    ] {
        assert!(matches!(
            insert_share_egress_dispatch_tx(&tx, 1, host, kind, 0, dispatch_id),
            Err(crate::error::AppError::InvalidArg(_))
        ));
    }
    tx.rollback().unwrap();
    let rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM share_egress_log", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(rows, 0);
}

/// MODEL-SELECTION ATOMICITY: publishing a genuinely different embedder must never leave
/// vectors produced by the previous model in any retrieval partition. Source chunks/FTS are
/// intentionally retained by the production method; this focused test exercises the four
/// FK-less vec0 tables plus the setting written in the same transaction.
#[test]
fn embed_model_switch_invalidates_every_vector_partition() {
    let db = mem_db();
    let zero_vector = [0.0; crate::embed::EMBED_DIM];
    let f32_blob = crate::embed::vec_to_blob(&zero_vector);
    let i8_blob = crate::embed::vec_to_int8_blob(&zero_vector);
    {
        let conn = db.lock();
        for table in ["vec_chunks", "topic_vec_chunks", "doc_vec_chunks"] {
            conn.execute(
                &format!("INSERT INTO {table}(chunk_id, embedding) VALUES (1, ?1)"),
                rusqlite::params![&f32_blob],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO org_vec_chunks(chunk_id, embedding) VALUES (1, vec_int8(?1))",
            rusqlite::params![&i8_blob],
        )
        .unwrap();
    }

    db.set_embed_model_selection("model-b", true).unwrap();

    assert_eq!(
        db.get_setting("embed_model_id").unwrap().as_deref(),
        Some("model-b")
    );
    let conn = db.lock();
    for table in [
        "vec_chunks",
        "topic_vec_chunks",
        "doc_vec_chunks",
        "org_vec_chunks",
    ] {
        let count: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0, "model switch left stale vectors in {table}");
    }
}

/// Export-collision guard: the `exported_hash` helpers round-trip on both exporting tables,
/// and a legacy row (exported before the guard) reads back `None` (grandfathered).
#[test]
fn exported_hash_round_trips_and_legacy_rows_read_none() {
    let db = mem_db();

    // MEETING note (`notes` table).
    db.insert_meeting(&sample_meeting("m1", "2026-07-16T09:00:00Z"))
        .unwrap();
    db.upsert_note(&NoteRecord {
        meeting_id: "m1".to_string(),
        provider_id: "claude_code".to_string(),
        markdown: "# hello".to_string(),
        created_at: "2026-07-16T09:05:00Z".to_string(),
        exported_path: Some("/tmp/x.md".to_string()),
        model_requested: None,
        model_served: None,
        gateway_host: None,
    })
    .unwrap();
    // Legacy shape: exported but never hashed → None.
    assert_eq!(
        db.get_note_exported_hash("m1", "claude_code").unwrap(),
        None
    );
    db.set_note_exported_hash("m1", "claude_code", Some("abc123"))
        .unwrap();
    assert_eq!(
        db.get_note_exported_hash("m1", "claude_code").unwrap(),
        Some("abc123".to_string())
    );
    // A row-preserving upsert (same PK) must NOT wipe the baseline (the hash column is owned
    // by the export writes, not the note upsert).
    db.upsert_note(&NoteRecord {
        meeting_id: "m1".to_string(),
        provider_id: "claude_code".to_string(),
        markdown: "# hello v2".to_string(),
        created_at: "2026-07-16T09:06:00Z".to_string(),
        exported_path: Some("/tmp/x.md".to_string()),
        model_requested: None,
        model_served: None,
        gateway_host: None,
    })
    .unwrap();
    assert_eq!(
        db.get_note_exported_hash("m1", "claude_code").unwrap(),
        Some("abc123".to_string()),
        "upsert_note leaves the guard baseline in place"
    );
    db.set_note_exported_hash("m1", "claude_code", None)
        .unwrap();
    assert_eq!(
        db.get_note_exported_hash("m1", "claude_code").unwrap(),
        None
    );
    // Unknown row → None, never an error.
    assert_eq!(db.get_note_exported_hash("nope", "x").unwrap(), None);

    // AUTHORED note (`documents(kind='note')`).
    let folder_id = db.ensure_default_note_folder().unwrap();
    db.insert_note("n1", &folder_id, "n1", "T", "body", 1)
        .unwrap();
    assert_eq!(db.get_note_doc_exported_hash("n1").unwrap(), None);
    db.set_note_doc_exported_hash("n1", Some("def456")).unwrap();
    assert_eq!(
        db.get_note_doc_exported_hash("n1").unwrap(),
        Some("def456".to_string())
    );
    db.set_note_doc_exported_hash("n1", None).unwrap();
    assert_eq!(db.get_note_doc_exported_hash("n1").unwrap(), None);
    assert_eq!(db.get_note_doc_exported_hash("nope").unwrap(), None);
}

/// 2026-07-13 perf audit: `meetings.started_at` (every list/search sorts on it) and
/// `notes.folder_id` (every folder lock/unlock full-scans on it) previously had no index
/// beyond each table's PK — confirms both indices actually exist after migrate(), not just
/// that the SQL didn't error.
#[test]
fn migrate_creates_the_new_perf_indices() {
    let db = mem_db();
    let conn = db.lock();
    let names: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'index'")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    assert!(
        names.contains(&"idx_meetings_started_at".to_string()),
        "missing idx_meetings_started_at, got indices: {names:?}"
    );
    assert!(
        names.contains(&"idx_notes_folder_id".to_string()),
        "missing idx_notes_folder_id, got indices: {names:?}"
    );
}

// ── M6 Shared Brain — org ingest + retrieval ────────────────────────────────────────────────

fn sha32(tag: u8) -> Vec<u8> {
    vec![tag; 32]
}

const STABLE_ORG_ID: &str = "11111111-1111-4111-8111-111111111111";
const STABLE_DOC_ID: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const STABLE_DOC_RACE_ID: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";

/// Join `org_id` locally (enabled by default) — the retrieval legs INNER JOIN `org_state`
/// (per-instance org toggle), so any test that ingests `org_items` directly must seed this first,
/// mirroring how production content can never exist without a prior join.
fn seed_org_state(db: &Db, org_id: &str) {
    db.upsert_org_state(&crate::storage::OrgState {
        org_id: org_id.to_string(),
        name: "Acme".to_string(),
        role: "member".to_string(),
        joined_at: "2026-07-10T00:00:00Z".to_string(),
        consented: true,
        last_seq: 0,
        generation: 1,
        context_enabled: true,
    })
    .unwrap();
}

fn seed_task_dashboard(db: &Db, id: &str) {
    db.insert_dashboard(id, "Task board", None, None, "2026-08-21T09:00:00Z")
        .unwrap();
}

/// Org Tasks share the encrypted feed/cursor but remain a separate SQLCipher projection: they must
/// never leak into the generic Org Brain/Ask readers, and a tombstone must remove the Task row plus
/// its device-private refs in the same transaction.
#[test]
fn task_projection_is_stable_context_gated_task_free_for_ask_and_tombstone_atomic() {
    let db = mem_db();
    let org_id = STABLE_ORG_ID;
    let doc_id = STABLE_DOC_ID;
    let item_id = "22222222-2222-4222-8222-222222222222";
    seed_org_state(&db, org_id);
    let envelope = crate::share::task_envelope::TaskEnvelope {
        version: crate::share::task_envelope::TASK_ENVELOPE_VERSION,
        org_id: org_id.to_string(),
        title: "Ship the nebula task view".into(),
        description: "This text must never enter Ask retrieval".into(),
        status: crate::share::task_envelope::TaskStatus::InProgress,
        due_at: Some("2026-08-23T09:00:00Z".into()),
        assignee_user_id: None,
        created_at: "2026-08-21T09:00:00Z".into(),
        subtasks: vec![],
        org_refs: vec![],
        images: vec![],
    };
    let json = envelope.to_canonical_json(org_id).unwrap();
    let prepared =
        Db::prepare_org_item_index(&envelope.title, &envelope.created_at, &json, None).unwrap();
    db.commit_local_org_replica_with_metadata(
        item_id,
        org_id,
        7,
        "owner",
        &envelope.title,
        &json,
        &envelope.created_at,
        1,
        1,
        &sha32(42),
        Some("task"),
        Some("owner"),
        &prepared,
        None,
        Some(doc_id),
        "edit",
        Some("owner"),
        true,
    )
    .unwrap();

    let tasks = db.list_org_tasks(None).unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].id, format!("{org_id}:{doc_id}"));
    assert_eq!(tasks[0].envelope_json, json);
    assert!(db.visible_org_task_ref(org_id, doc_id).unwrap());
    assert!(!db
        .visible_org_task_ref(org_id, "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb")
        .unwrap());
    assert!(!db
        .visible_org_task_ref("99999999-9999-4999-8999-999999999999", doc_id,)
        .unwrap());

    let note_doc_id = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
    db.upsert_org_item(
        "note-ref-target",
        org_id,
        8,
        "owner",
        "A note, not a Task",
        "note body",
        "2026-08-21T09:00:00Z",
        1,
        1,
        &sha32(43),
        Some("document"),
        Some("owner"),
        None,
    )
    .unwrap();
    db.set_org_item_document_metadata("note-ref-target", Some(note_doc_id), "edit", Some("owner"))
        .unwrap();
    assert!(
        !db.visible_org_task_ref(org_id, note_doc_id).unwrap(),
        "a live Note document is not a valid Task reference"
    );
    assert!(db.get_org_item(item_id).unwrap().is_none());
    assert!(db.list_org_items(org_id).unwrap().is_empty());
    assert!(db
        .search_org_chunks_fts("nebula task view", 10)
        .unwrap()
        .is_empty());
    let task_chunk_count: i64 = db
        .lock()
        .query_row(
            "SELECT COUNT(*) FROM org_chunks WHERE item_id=?1",
            [item_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(task_chunk_count, 0);
    assert_eq!(db.count_org_items(org_id).unwrap(), 0);
    assert!(db.org_item_vector_batch(item_id).unwrap().is_none());
    assert!(db
        .resolve_wikilink(&envelope.title, &std::collections::HashSet::new())
        .unwrap()
        .is_none());
    assert!(db
        .org_link_target_visible(&format!("{org_id}:{doc_id}"))
        .unwrap()
        .is_none());
    assert!(db
        .org_link_doc_id_for_item_visible(item_id)
        .unwrap()
        .is_none());

    seed_task_dashboard(&db, "board-1");
    db.replace_task_local_refs(
        &tasks[0].id,
        &[crate::storage::tasks_store::TaskLocalRefRow {
            kind: "dashboard".into(),
            ref_id: "board-1".into(),
            position: 0,
        }],
    )
    .unwrap();
    assert_eq!(db.dashboard_task_rows("board-1").unwrap().len(), 1);
    assert!(db
        .replace_task_local_refs(
            &tasks[0].id,
            &[crate::storage::tasks_store::TaskLocalRefRow {
                kind: "note".into(),
                ref_id: "missing-note".into(),
                position: 0,
            }],
        )
        .is_err());
    assert_eq!(
        db.task_local_refs(&tasks[0].id).unwrap(),
        vec![crate::storage::tasks_store::TaskLocalRefRow {
            kind: "dashboard".into(),
            ref_id: "board-1".into(),
            position: 0,
        }],
        "invalid replacement must preserve the previous local references atomically"
    );
    assert!(db.delete_dashboard("board-1").unwrap());
    assert!(
        db.task_local_refs(&tasks[0].id).unwrap().is_empty(),
        "a deleted local target must never remain visible through Task detail"
    );
    db.replace_task_local_refs(&tasks[0].id, &[]).unwrap();
    seed_task_dashboard(&db, "board-1");
    db.replace_task_local_refs(
        &tasks[0].id,
        &[crate::storage::tasks_store::TaskLocalRefRow {
            kind: "dashboard".into(),
            ref_id: "board-1".into(),
            position: 0,
        }],
    )
    .unwrap();

    db.set_org_context_enabled(org_id, false).unwrap();
    assert!(!db.visible_org_task_ref(org_id, doc_id).unwrap());
    assert!(db.list_org_tasks(None).unwrap().is_empty());
    assert!(db.dashboard_task_rows("board-1").unwrap().is_empty());
    assert!(db.replace_task_local_refs(&tasks[0].id, &[]).is_err());
    assert_eq!(db.task_local_refs(&tasks[0].id).unwrap().len(), 1);
    db.set_org_context_enabled(org_id, true).unwrap();

    assert!(db.evict_org_item(item_id).unwrap());
    assert!(!db.visible_org_task_ref(org_id, doc_id).unwrap());
    assert!(db
        .get_org_task(&format!("{org_id}:{doc_id}"))
        .unwrap()
        .is_none());
    assert!(db
        .task_local_refs(&format!("{org_id}:{doc_id}"))
        .unwrap()
        .is_empty());
}

#[test]
fn incoming_task_org_refs_are_deferred_and_only_live_task_targets_are_exposed() {
    use crate::share::task_envelope::{
        TaskEnvelope, TaskOrgRef, TaskStatus, TASK_ENVELOPE_VERSION,
    };

    let db = mem_db();
    let org_id = STABLE_ORG_ID;
    let source_doc_id = STABLE_DOC_ID;
    let note_doc_id = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
    let missing_doc_id = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";
    let future_task_doc_id = "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee";
    seed_org_state(&db, org_id);

    db.upsert_org_item(
        "note-org-ref-target",
        org_id,
        1,
        "owner",
        "A Note target",
        "not a Task",
        "2026-08-21T09:00:00Z",
        1,
        1,
        &sha32(51),
        Some("document"),
        Some("owner"),
        None,
    )
    .unwrap();
    db.set_org_item_document_metadata(
        "note-org-ref-target",
        Some(note_doc_id),
        "edit",
        Some("owner"),
    )
    .unwrap();

    let source = TaskEnvelope {
        version: TASK_ENVELOPE_VERSION,
        org_id: org_id.into(),
        title: "Source Task".into(),
        description: String::new(),
        status: TaskStatus::Todo,
        due_at: None,
        assignee_user_id: None,
        created_at: "2026-08-21T09:00:00Z".into(),
        subtasks: vec![],
        org_refs: vec![
            TaskOrgRef {
                org_id: org_id.into(),
                doc_id: note_doc_id.into(),
            },
            TaskOrgRef {
                org_id: org_id.into(),
                doc_id: missing_doc_id.into(),
            },
            TaskOrgRef {
                org_id: org_id.into(),
                doc_id: future_task_doc_id.into(),
            },
        ],
        images: vec![],
    };
    let source_json = source.to_canonical_json(org_id).unwrap();
    let source_prepared = Db::prepare_org_item_index_for_kind(
        crate::share::org_envelope::OrgItemKind::Task,
        &source.title,
        &source.created_at,
        &source_json,
        None,
    )
    .unwrap();
    db.commit_local_org_replica_with_metadata(
        "source-task-item",
        org_id,
        2,
        "owner",
        &source.title,
        &source_json,
        &source.created_at,
        1,
        1,
        &sha32(52),
        Some("task"),
        Some("owner"),
        &source_prepared,
        None,
        Some(source_doc_id),
        "edit",
        Some("owner"),
        true,
    )
    .unwrap();
    let source_task_id = format!("{org_id}:{source_doc_id}");
    assert!(db.visible_task_org_refs(&source_task_id).unwrap().is_empty());

    let target = TaskEnvelope {
        org_id: org_id.into(),
        title: "Later Task".into(),
        org_refs: vec![],
        ..source.clone()
    };
    let target_json = target.to_canonical_json(org_id).unwrap();
    let target_prepared = Db::prepare_org_item_index_for_kind(
        crate::share::org_envelope::OrgItemKind::Task,
        &target.title,
        &target.created_at,
        &target_json,
        None,
    )
    .unwrap();
    db.commit_local_org_replica_with_metadata(
        "future-task-item",
        org_id,
        3,
        "owner",
        &target.title,
        &target_json,
        &target.created_at,
        1,
        1,
        &sha32(53),
        Some("task"),
        Some("owner"),
        &target_prepared,
        None,
        Some(future_task_doc_id),
        "edit",
        Some("owner"),
        true,
    )
    .unwrap();
    assert_eq!(
        db.visible_task_org_refs(&source_task_id).unwrap(),
        vec![TaskOrgRef {
            org_id: org_id.into(),
            doc_id: future_task_doc_id.into(),
        }],
        "feed order may defer a real Task target, but Note and nonexistent docs stay hidden"
    );

    assert!(db.evict_org_item("future-task-item").unwrap());
    assert!(db.visible_task_org_refs(&source_task_id).unwrap().is_empty());
}

#[test]
fn task_and_dashboard_lists_are_bounded_with_total_ordering() {
    use crate::storage::tasks_store::{DASHBOARD_TASK_LIMIT, TASK_LIST_LIMIT};

    let db = mem_db();
    let org_id = STABLE_ORG_ID;
    seed_org_state(&db, org_id);
    seed_task_dashboard(&db, "board-bounded");
    let envelope = crate::share::task_envelope::TaskEnvelope {
        version: crate::share::task_envelope::TASK_ENVELOPE_VERSION,
        org_id: org_id.to_string(),
        title: "Bounded task".into(),
        description: String::new(),
        status: crate::share::task_envelope::TaskStatus::Todo,
        due_at: None,
        assignee_user_id: None,
        created_at: "2026-08-21T09:00:00Z".into(),
        subtasks: vec![],
        org_refs: vec![],
        images: vec![],
    };
    let json = envelope.to_canonical_json(org_id).unwrap();
    let count = TASK_LIST_LIMIT + 17;
    let mut conn = db.lock();
    let tx = conn.transaction().unwrap();
    for index in 0..count {
        let task_id = format!("task-{index:04}");
        tx.execute(
            "INSERT INTO org_tasks
               (id,org_id,doc_id,item_id,source_document_id,envelope_json,status,due_at,
                assignee_user_id,access,author_user_id,owner_user_id,rev,generation,seq,updated_at)
             VALUES(?1,?2,?3,?4,NULL,?5,'todo',NULL,NULL,'edit','author','owner',1,1,?6,?7)",
            rusqlite::params![
                task_id,
                org_id,
                format!("doc-{index:04}"),
                format!("item-{index:04}"),
                json,
                index,
                "2026-08-21T09:00:00Z",
            ],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO task_local_refs(task_id,kind,ref_id,position)
             VALUES(?1,'dashboard','board-bounded',?2)",
            rusqlite::params![task_id, count - index],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    drop(conn);

    let tasks = db.list_org_tasks(Some(org_id)).unwrap();
    assert_eq!(tasks.len(), TASK_LIST_LIMIT as usize);
    assert_eq!(tasks.first().unwrap().id, "task-0000");
    assert_eq!(
        tasks.last().unwrap().id,
        format!("task-{:04}", TASK_LIST_LIMIT - 1)
    );

    let dashboard = db.dashboard_task_rows("board-bounded").unwrap();
    assert_eq!(dashboard.len(), DASHBOARD_TASK_LIMIT as usize);
    assert_eq!(
        dashboard.first().unwrap().id,
        format!("task-{:04}", count - 1)
    );
    assert_eq!(
        dashboard.last().unwrap().id,
        format!("task-{:04}", count - DASHBOARD_TASK_LIMIT)
    );
}

#[test]
fn task_feed_sequence_is_single_claim_and_tombstone_cannot_resurrect() {
    let db = mem_db();
    let org_id = STABLE_ORG_ID;
    let doc_id = STABLE_DOC_ID;
    let item_id = "22222222-2222-4222-8222-222222222222";
    seed_org_state(&db, org_id);
    let envelope = crate::share::task_envelope::TaskEnvelope {
        version: crate::share::task_envelope::TASK_ENVELOPE_VERSION,
        org_id: org_id.to_string(),
        title: "One claim".into(),
        description: "Never resurrect".into(),
        status: crate::share::task_envelope::TaskStatus::Todo,
        due_at: None,
        assignee_user_id: None,
        created_at: "2026-08-21T09:00:00Z".into(),
        subtasks: vec![],
        org_refs: vec![],
        images: vec![],
    };
    let json = envelope.to_canonical_json(org_id).unwrap();
    let prepared = Db::prepare_org_item_index_for_kind(
        crate::share::org_envelope::OrgItemKind::Task,
        &envelope.title,
        &envelope.created_at,
        &json,
        None,
    )
    .unwrap();
    let commit = || {
        db.commit_org_feed_item_with_metadata(
            item_id,
            org_id,
            1,
            "author",
            &envelope.title,
            &json,
            &envelope.created_at,
            1,
            1,
            &sha32(9),
            Some("task"),
            Some("author"),
            &prepared,
            Some(doc_id),
            "edit",
            Some("owner"),
            true,
        )
        .unwrap()
    };

    assert!(commit().changed);
    assert_eq!(db.org_last_seq_for(org_id).unwrap(), 1);
    assert_eq!(db.list_org_tasks(Some(org_id)).unwrap().len(), 1);
    assert!(
        !commit().changed,
        "the same feed sequence is never claimed twice"
    );

    assert!(db.commit_org_feed_tombstone(org_id, item_id, 2).unwrap());
    assert_eq!(db.org_last_seq_for(org_id).unwrap(), 2);
    assert!(db.list_org_tasks(Some(org_id)).unwrap().is_empty());
    assert!(
        !commit().changed,
        "a stale live replay cannot move the cursor or resurrect Task data"
    );
    assert_eq!(db.org_last_seq_for(org_id).unwrap(), 2);
    assert!(db.list_org_tasks(Some(org_id)).unwrap().is_empty());
    assert!(db.org_replica_state(item_id).unwrap().unwrap().tombstoned);
}

#[test]
fn task_membership_withdrawal_purges_detail_list_dashboard_and_local_refs_atomically() {
    let db = mem_db();
    let org_id = STABLE_ORG_ID;
    let doc_id = STABLE_DOC_ID;
    let item_id = "33333333-3333-4333-8333-333333333333";
    seed_org_state(&db, org_id);
    seed_task_dashboard(&db, "board-withdrawn");
    let envelope = crate::share::task_envelope::TaskEnvelope {
        version: crate::share::task_envelope::TASK_ENVELOPE_VERSION,
        org_id: org_id.to_string(),
        title: "Withdraw membership".into(),
        description: "Purged plaintext".into(),
        status: crate::share::task_envelope::TaskStatus::InProgress,
        due_at: None,
        assignee_user_id: None,
        created_at: "2026-08-21T09:00:00Z".into(),
        subtasks: vec![],
        org_refs: vec![],
        images: vec![],
    };
    let json = envelope.to_canonical_json(org_id).unwrap();
    let prepared = Db::prepare_org_item_index_for_kind(
        crate::share::org_envelope::OrgItemKind::Task,
        &envelope.title,
        &envelope.created_at,
        &json,
        None,
    )
    .unwrap();
    assert!(
        db.commit_org_feed_item_with_metadata(
            item_id,
            org_id,
            1,
            "author",
            &envelope.title,
            &json,
            &envelope.created_at,
            1,
            1,
            &sha32(11),
            Some("task"),
            Some("author"),
            &prepared,
            Some(doc_id),
            "view",
            Some("owner"),
            true,
        )
        .unwrap()
        .changed
    );
    let task_id = format!("{org_id}:{doc_id}");
    db.replace_task_local_refs(
        &task_id,
        &[crate::storage::tasks_store::TaskLocalRefRow {
            kind: "dashboard".into(),
            ref_id: "board-withdrawn".into(),
            position: 0,
        }],
    )
    .unwrap();
    assert_eq!(db.dashboard_task_rows("board-withdrawn").unwrap().len(), 1);

    assert!(db.delete_org_state(org_id).unwrap());
    assert!(!db.visible_org_task_for_item(item_id).unwrap());
    assert!(db.list_org_tasks(Some(org_id)).unwrap().is_empty());
    assert!(db.get_org_task(&task_id).unwrap().is_none());
    assert!(db
        .dashboard_task_rows("board-withdrawn")
        .unwrap()
        .is_empty());
    assert!(db.task_local_refs(&task_id).unwrap().is_empty());
    let plaintext_rows: i64 = db
        .lock()
        .query_row(
            "SELECT COUNT(*) FROM org_tasks WHERE org_id=?1",
            [org_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(plaintext_rows, 0);
}

#[test]
fn task_source_commit_marks_republish_dirty_atomically() {
    let db = mem_db();
    let source_id = "55555555-5555-4555-8555-555555555555";
    db.create_task_source(source_id, "Task A", r#"{"title":"Task A"}"#, 1)
        .unwrap();
    assert!(db
        .list_folders()
        .unwrap()
        .iter()
        .all(|folder| folder.id != crate::storage::tasks_store::TASK_FOLDER_ID));
    let unlocked = std::collections::HashSet::new();
    assert!(db.get_document(source_id).unwrap().is_none());
    assert!(db
        .get_document_if_visible(source_id, &unlocked)
        .unwrap()
        .is_none());
    assert!(db
        .documents_in_folder(crate::storage::tasks_store::TASK_FOLDER_ID)
        .unwrap()
        .is_empty());
    assert!(!db
        .visible_document_ids(&unlocked)
        .unwrap()
        .contains(&source_id.to_string()));
    assert!(db.delete_document(source_id).is_err());
    assert!(db.task_source(source_id).unwrap().is_some());
    assert!(db
        .lock()
        .execute(
            "UPDATE folders SET name='Visible Tasks' WHERE id=?1",
            [crate::storage::tasks_store::TASK_FOLDER_ID],
        )
        .is_err());
    db.insert_org_share(
        "task-share",
        STABLE_ORG_ID,
        None,
        Some(source_id),
        "task",
        Some("Task A"),
        1,
        1,
        &sha32(1),
        "2026-08-21T00:00:00Z",
    )
    .unwrap();
    db.set_org_share_uploaded("task-share", "task-item", "2026-08-21T00:00:01Z")
        .unwrap();

    let before = db.org_share_source_counters("task-share").unwrap();
    assert!(db
        .update_task_source(source_id, "Task B", r#"{"title":"Task B"}"#, 2)
        .unwrap());
    let committed = db.org_share_source_counters("task-share").unwrap();
    assert_eq!(committed, (before.0 + 1, before.1 + 1));

    db.lock()
        .execute_batch(
            "CREATE TRIGGER fail_task_dirty BEFORE UPDATE OF republish_dirty ON org_shares
               BEGIN SELECT RAISE(ABORT, 'fail task dirty'); END;",
        )
        .unwrap();
    assert!(db
        .update_task_source(source_id, "Task C", r#"{"title":"Task C"}"#, 3)
        .is_err());
    let (title, payload, updated_at) = db.task_source(source_id).unwrap().unwrap();
    assert_eq!(
        (title.as_str(), payload.as_str(), updated_at),
        ("Task B", r#"{"title":"Task B"}"#, 1)
    );
    assert_eq!(
        db.org_share_source_counters("task-share").unwrap(),
        committed
    );
}

#[test]
fn republish_projection_is_full_witness_monotonic_and_purges_all_older_plaintext() {
    let db = mem_db();
    seed_org_state(&db, STABLE_ORG_ID);
    db.insert_org_share(
        "share-projection",
        STABLE_ORG_ID,
        None,
        Some("source-projection"),
        "note",
        Some("Title"),
        3,
        1,
        &sha32(3),
        "2026-08-13T00:00:00Z",
    )
    .unwrap();
    db.lock()
        .execute(
            "UPDATE org_shares SET state='uploaded', item_id='head-r3', doc_id=?2,
                    access='edit', dispatch_id='dispatch-r3',
                    expected_actor_user_id='owner', expected_owner_user_id='owner'
              WHERE id=?1",
            rusqlite::params!["share-projection", STABLE_DOC_ID],
        )
        .unwrap();
    for (item, seq, rev) in [("head-r1", 1_u64, 1_u32), ("head-r2", 2, 2)] {
        db.upsert_org_item(
            item,
            STABLE_ORG_ID,
            seq,
            "owner",
            item,
            &format!("plaintext-{item}"),
            "2026-08-13T00:00:00Z",
            rev,
            1,
            &sha32(rev as u8),
            None,
            Some("owner"),
            None,
        )
        .unwrap();
        db.set_org_item_document_metadata(item, Some(STABLE_DOC_ID), "edit", Some("owner"))
            .unwrap();
    }
    db.repair_org_reconcile_metadata(
        "head-r2",
        STABLE_ORG_ID,
        1,
        Some(STABLE_DOC_ID),
        "edit",
        Some("owner"),
        true,
    )
    .unwrap();
    let prepared = Db::prepare_org_item_index("head-r3", "t", "plaintext-r3", None).unwrap();
    let projection = crate::storage::org_store::OrgRepublishProjection {
        item_id: "head-r3",
        seq: 3,
        author_hint: "owner",
        title: "head-r3",
        markdown: "plaintext-r3",
        created_at: "t",
        source_kind: None,
        author_user_id: Some("owner"),
        prepared: &prepared,
        attachments: &[],
    };
    assert!(
        db.commit_org_republish_projection_if_current(
            "share-projection",
            "dispatch-r3",
            STABLE_ORG_ID,
            STABLE_DOC_ID,
            "edit",
            3,
            1,
            &sha32(3),
            "owner",
            "owner",
            None,
            None,
            false,
            &projection,
        )
        .unwrap()
        .changed
    );
    for old in ["head-r1", "head-r2"] {
        let (tombstoned, title, markdown): (i64, String, String) = db
            .lock()
            .query_row(
                "SELECT tombstoned,title,markdown FROM org_items WHERE item_id=?1",
                [old],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            (tombstoned, title, markdown),
            (1, String::new(), String::new())
        );
    }

    let newer = Db::prepare_org_item_index("head-r4", "t", "newer", None).unwrap();
    db.commit_local_org_replica_with_metadata(
        "head-r4",
        STABLE_ORG_ID,
        4,
        "owner",
        "head-r4",
        "newer",
        "t",
        4,
        1,
        &sha32(4),
        None,
        Some("owner"),
        &newer,
        None,
        Some(STABLE_DOC_ID),
        "edit",
        Some("owner"),
        true,
    )
    .unwrap();
    assert!(
        !db.commit_org_republish_projection_if_current(
            "share-projection",
            "dispatch-r3",
            STABLE_ORG_ID,
            STABLE_DOC_ID,
            "edit",
            3,
            1,
            &sha32(3),
            "owner",
            "owner",
            None,
            None,
            false,
            &projection,
        )
        .unwrap()
        .changed
    );
    assert_eq!(
        db.current_org_document_status(STABLE_ORG_ID, STABLE_DOC_ID)
            .unwrap()
            .unwrap()
            .0,
        "head-r4"
    );
}

#[test]
fn republish_projection_repairs_same_seq_attachment_bundle_before_clearing_journal() {
    let db = mem_db();
    seed_org_state(&db, STABLE_ORG_ID);
    db.insert_org_share(
        "share-repair",
        STABLE_ORG_ID,
        None,
        Some("source"),
        "note",
        Some("Title"),
        2,
        1,
        &sha32(2),
        "t",
    )
    .unwrap();
    db.lock().execute(
        "UPDATE org_shares SET state='failed',last_error='projection_pending',item_id='head-r2',
          doc_id=?2,access='edit',dispatch_id='dispatch-r2',expected_actor_user_id='owner',
          expected_owner_user_id='owner' WHERE id=?1",
        rusqlite::params!["share-repair",STABLE_DOC_ID],
    ).unwrap();
    let prepared = Db::prepare_org_item_index("Title", "t", "body", None).unwrap();
    db.upsert_org_item(
        "head-r2",
        STABLE_ORG_ID,
        2,
        "owner",
        "Title",
        "body",
        "t",
        2,
        1,
        &sha32(2),
        None,
        Some("owner"),
        None,
    )
    .unwrap();
    db.set_org_item_document_metadata("head-r2", Some(STABLE_DOC_ID), "edit", Some("owner"))
        .unwrap();
    db.replace_org_item_attachment_bundle(
        "head-r2",
        &[crate::storage::IncomingAttachment {
            id: "stale".into(),
            mime_type: "image/png".into(),
            extension: "png".into(),
            width: 1,
            height: 1,
            sha256: [1; 32],
            data: vec![1],
        }],
    )
    .unwrap();
    db.lock()
        .execute(
            "UPDATE org_items SET projection_sha256=NULL WHERE item_id='head-r2'",
            [],
        )
        .unwrap();
    let exact = crate::storage::IncomingAttachment {
        id: "exact".into(),
        mime_type: "image/png".into(),
        extension: "png".into(),
        width: 2,
        height: 2,
        sha256: [2; 32],
        data: vec![2, 2],
    };
    let exact_attachments = vec![exact];
    let projection = crate::storage::org_store::OrgRepublishProjection {
        item_id: "head-r2",
        seq: 2,
        author_hint: "owner",
        title: "Title",
        markdown: "body",
        created_at: "t",
        source_kind: None,
        author_user_id: Some("owner"),
        prepared: &prepared,
        attachments: &exact_attachments,
    };
    assert!(
        db.commit_org_republish_projection_if_current(
            "share-repair",
            "dispatch-r2",
            STABLE_ORG_ID,
            STABLE_DOC_ID,
            "edit",
            2,
            1,
            &sha32(2),
            "owner",
            "owner",
            Some("head-r2"),
            Some("projection_pending"),
            false,
            &projection,
        )
        .unwrap()
        .changed
    );
    let conn = db.lock();
    assert_eq!(
        conn.query_row(
            "SELECT group_concat(id) FROM note_attachments WHERE org_item_id='head-r2'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "exact"
    );
    assert!(conn
        .query_row(
            "SELECT projection_sha256=?2 FROM org_items WHERE item_id=?1",
            rusqlite::params!["head-r2", sha32(2)],
            |r| r.get::<_, bool>(0)
        )
        .unwrap());
    assert_eq!(
        conn.query_row(
            "SELECT state FROM org_shares WHERE id='share-repair'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "uploaded"
    );
}

#[test]
fn org_source_counters_are_atomic_row_local_and_debounce_only_invalidates() {
    let db = mem_db();
    seed_folder(&db, "f-source", "Source");
    db.insert_note("n-source", "f-source", "source", "Title", "A", 1)
        .unwrap();
    for id in ["share-a", "share-b"] {
        db.insert_org_share(
            id,
            STABLE_ORG_ID,
            None,
            Some("n-source"),
            "note",
            Some("Title"),
            1,
            1,
            &sha32(1),
            "2026-08-13T00:00:00Z",
        )
        .unwrap();
        db.set_org_share_uploaded(id, &format!("item-{id}"), "t")
            .unwrap();
    }
    db.update_note_row_debounced("n-source", "Title", "draft", 2)
        .unwrap();
    for id in ["share-a", "share-b"] {
        assert_eq!(db.org_share_source_counters(id).unwrap(), (1, 0));
    }
    db.update_note_row("n-source", "Title", "committed", 3)
        .unwrap();
    for id in ["share-a", "share-b"] {
        assert_eq!(db.org_share_source_counters(id).unwrap(), (2, 1));
    }

    db.lock()
        .execute_batch(
            "CREATE TRIGGER fail_dirty BEFORE UPDATE OF republish_dirty ON org_shares
               BEGIN SELECT RAISE(ABORT, 'fail dirty'); END;",
        )
        .unwrap();
    assert!(db
        .update_note_row("n-source", "Title", "must-roll-back", 4)
        .is_err());
    assert_eq!(
        db.get_note_row("n-source").unwrap().unwrap().text,
        "committed"
    );
}

#[test]
fn attachment_updates_advance_old_and_new_source_witnesses_atomically() {
    let db = mem_db();
    seed_folder(&db, "f-attachment-update", "Attachment update");
    db.insert_note(
        "n-attachment-old",
        "f-attachment-update",
        "source",
        "Old",
        "body",
        1,
    )
    .unwrap();
    db.insert_note(
        "n-attachment-new",
        "f-attachment-update",
        "source",
        "New",
        "body",
        1,
    )
    .unwrap();
    db.lock()
        .execute(
            "INSERT INTO note_attachments
           (id,document_id,mime_type,extension,byte_len,width,height,sha256,data,created_at)
         VALUES('attachment-update','n-attachment-old','image/png','png',1,1,1,?1,?2,1)",
            rusqlite::params![sha32(1), vec![1_u8]],
        )
        .unwrap();

    for (share_id, document_id) in [
        ("share-attachment-old", "n-attachment-old"),
        ("share-attachment-new", "n-attachment-new"),
    ] {
        db.insert_org_share(
            share_id,
            STABLE_ORG_ID,
            None,
            Some(document_id),
            "note",
            Some("Title"),
            1,
            1,
            &sha32(1),
            "t",
        )
        .unwrap();
        db.set_org_share_uploaded(share_id, &format!("item-{share_id}"), "t")
            .unwrap();
    }

    db.lock()
        .execute(
            "UPDATE note_attachments SET data=?2,sha256=?3,byte_len=2 WHERE id=?1",
            rusqlite::params!["attachment-update", vec![2_u8, 2], sha32(2)],
        )
        .unwrap();
    assert_eq!(
        db.org_share_source_counters("share-attachment-old")
            .unwrap(),
        (1, 1)
    );
    assert_eq!(
        db.org_share_source_counters("share-attachment-new")
            .unwrap(),
        (0, 0)
    );

    db.lock().execute(
        "UPDATE note_attachments SET document_id='n-attachment-new' WHERE id='attachment-update'",
        [],
    ).unwrap();
    assert_eq!(
        db.org_share_source_counters("share-attachment-old")
            .unwrap(),
        (2, 2)
    );
    assert_eq!(
        db.org_share_source_counters("share-attachment-new")
            .unwrap(),
        (1, 1)
    );

    db.lock()
        .execute_batch(
            "CREATE TRIGGER fail_attachment_source_witness
           BEFORE UPDATE OF source_version ON org_shares
           WHEN NEW.id='share-attachment-new'
           BEGIN SELECT RAISE(ABORT, 'fail attachment witness'); END;",
        )
        .unwrap();
    assert!(db
        .lock()
        .execute(
            "UPDATE note_attachments SET mime_type='image/jpeg' WHERE id='attachment-update'",
            [],
        )
        .is_err());
    assert_eq!(
        db.lock()
            .query_row(
                "SELECT mime_type FROM note_attachments WHERE id='attachment-update'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        "image/png"
    );
}

/// RED before UUID admission: malformed stable identities advanced the feed cursor and persisted a
/// plaintext replica whose later link identity could never be constructed. Validation must happen
/// before either mutation, while the historical no-docId feed shape remains ingestible.
#[test]
fn stable_org_document_uuid_admission_precedes_storage_mutation() {
    const ORG_ID: &str = "11111111-1111-4111-8111-111111111111";
    const DOC_ID: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

    let db = mem_db();
    seed_org_state(&db, ORG_ID);
    let prepared = Db::prepare_org_item_index("Title", "t", "body", None).unwrap();
    let malformed_doc = db.commit_org_feed_item_with_metadata(
        "bad-doc-item",
        ORG_ID,
        1,
        "owner",
        "Title",
        "body",
        "2026-08-13T00:00:00Z",
        1,
        1,
        &sha32(1),
        None,
        Some("owner"),
        &prepared,
        Some("not-a-uuid"),
        "view",
        Some("owner"),
        true,
    );
    assert!(matches!(
        malformed_doc,
        Err(crate::error::AppError::InvalidArg(_))
    ));
    assert_eq!(db.org_last_seq_for(ORG_ID).unwrap(), 0);
    assert!(db.org_replica_state("bad-doc-item").unwrap().is_none());

    seed_org_state(&db, "not-an-org-uuid");
    let malformed_org = db.commit_org_feed_item_with_metadata(
        "bad-org-item",
        "not-an-org-uuid",
        1,
        "owner",
        "Title",
        "body",
        "2026-08-13T00:00:00Z",
        1,
        1,
        &sha32(2),
        None,
        Some("owner"),
        &prepared,
        Some(DOC_ID),
        "view",
        Some("owner"),
        true,
    );
    assert!(matches!(
        malformed_org,
        Err(crate::error::AppError::InvalidArg(_))
    ));
    assert_eq!(db.org_last_seq_for("not-an-org-uuid").unwrap(), 0);
    assert!(db.org_replica_state("bad-org-item").unwrap().is_none());

    db.insert_org_share(
        "share-uuid-admission",
        ORG_ID,
        None,
        Some("local-note"),
        "note",
        Some("Local"),
        1,
        1,
        &sha32(3),
        "2026-08-13T00:00:00Z",
    )
    .unwrap();
    assert!(matches!(
        db.set_org_share_document_metadata("share-uuid-admission", "not-a-uuid", "edit"),
        Err(crate::error::AppError::InvalidArg(_))
    ));
    let stored_share_doc: Option<String> = db
        .lock()
        .query_row(
            "SELECT doc_id FROM org_shares WHERE id = 'share-uuid-admission'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored_share_doc, None);

    assert!(
        db.commit_org_feed_item_with_metadata(
            "valid-stable-item",
            ORG_ID,
            1,
            "owner",
            "Title",
            "body",
            "2026-08-13T00:00:00Z",
            1,
            1,
            &sha32(3),
            None,
            Some("owner"),
            &prepared,
            Some(DOC_ID),
            "view",
            Some("owner"),
            true,
        )
        .unwrap()
        .changed
    );
    assert_eq!(db.org_last_seq_for(ORG_ID).unwrap(), 1);
    assert_eq!(
        db.org_item_edit_ctx("valid-stable-item")
            .unwrap()
            .unwrap()
            .doc_id
            .as_deref(),
        Some(DOC_ID)
    );
    assert!(matches!(
        db.set_org_item_document_metadata(
            "valid-stable-item",
            Some("not-a-uuid"),
            "edit",
            Some("owner")
        ),
        Err(crate::error::AppError::InvalidArg(_))
    ));
    let unchanged = db.org_item_edit_ctx("valid-stable-item").unwrap().unwrap();
    assert_eq!(unchanged.doc_id.as_deref(), Some(DOC_ID));
    assert_eq!(unchanged.access, "view");

    assert!(
        db.commit_org_feed_item_with_metadata(
            "legacy-item",
            "not-an-org-uuid",
            1,
            "owner",
            "Legacy",
            "body",
            "2026-08-13T00:00:01Z",
            1,
            1,
            &sha32(4),
            None,
            Some("owner"),
            &Db::prepare_org_item_index("Legacy", "t", "body", None).unwrap(),
            None,
            "view",
            None,
            false,
        )
        .unwrap()
        .changed
    );
    assert!(db.org_replica_state("legacy-item").unwrap().is_some());
}

#[test]
fn org_link_identity_isolated_by_org_and_follows_current_revision() {
    let db = mem_db();
    let org_a = "11111111-1111-4111-8111-111111111111";
    let org_b = "22222222-2222-4222-8222-222222222222";
    let doc = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let endpoint_a = format!("{org_a}:{doc}");
    let endpoint_b = format!("{org_b}:{doc}");
    seed_org_state(&db, org_a);
    seed_org_state(&db, org_b);
    for (item, org, title, seq) in [
        ("a-old", org_a, "A old", 1),
        ("a-new", org_a, "A current", 2),
        ("b-item", org_b, "B current", 1),
    ] {
        db.upsert_org_item(
            item,
            org,
            seq,
            "owner",
            title,
            "body",
            "2026-08-12T00:00:00Z",
            seq as u32,
            1,
            &sha32(seq as u8),
            None,
            Some("owner"),
            None,
        )
        .unwrap();
        db.set_org_item_document_metadata(item, Some(doc), "view", Some("owner"))
            .unwrap();
    }
    db.repair_org_reconcile_metadata("a-new", org_a, 1, Some(doc), "view", Some("owner"), true)
        .unwrap();
    db.repair_org_reconcile_metadata("b-item", org_b, 1, Some(doc), "view", Some("owner"), true)
        .unwrap();

    assert_eq!(
        db.org_link_target_visible(&endpoint_a).unwrap(),
        Some(("a-new".into(), "A current".into()))
    );
    assert_eq!(
        db.org_link_target_visible(&endpoint_b).unwrap(),
        Some(("b-item".into(), "B current".into()))
    );

    db.upsert_manual_link("org", &endpoint_a, "org", &endpoint_b)
        .expect("view-only current org documents are valid private link endpoints");
    let before_revision = db
        .links_for_visible(crate::links::LinkKind::Org, &endpoint_b, &HashSet::new())
        .unwrap();
    assert_eq!(before_revision.len(), 1);
    assert_eq!(before_revision[0].other_id, endpoint_a);
    assert_eq!(before_revision[0].navigation_id.as_deref(), Some("a-new"));
    assert_eq!(before_revision[0].other_title, "A current");

    db.upsert_org_item(
        "a-next",
        org_a,
        3,
        "owner",
        "A next",
        "body",
        "2026-08-12T00:00:01Z",
        3,
        1,
        &sha32(3),
        None,
        Some("owner"),
        None,
    )
    .unwrap();
    db.set_org_item_document_metadata("a-next", Some(doc), "view", Some("owner"))
        .unwrap();
    db.repair_org_reconcile_metadata("a-next", org_a, 1, Some(doc), "view", Some("owner"), true)
        .unwrap();
    let after_revision = db
        .links_for_visible(crate::links::LinkKind::Org, &endpoint_b, &HashSet::new())
        .unwrap();
    assert_eq!(after_revision.len(), 1);
    assert_eq!(
        after_revision[0].other_id, endpoint_a,
        "the stored edge identity must remain the stable org+document composite"
    );
    assert_eq!(
        after_revision[0].navigation_id.as_deref(),
        Some("a-next"),
        "navigation must follow the authoritative current revision"
    );
    assert_eq!(after_revision[0].other_title, "A next");
    assert_eq!(
        manual_link_tuple_count(&db, "org", &endpoint_a, "org", &endpoint_b),
        1
    );

    db.set_org_context_enabled(org_a, false).unwrap();
    assert_eq!(
        db.org_link_target_visible(&endpoint_a).unwrap(),
        None,
        "context disable must hide the endpoint"
    );
    assert_eq!(
        link_count(&db, "org", &endpoint_a, "manual"),
        1,
        "context disable must preserve the private edge for reversible re-enable"
    );
    assert!(
        db.links_for_visible(crate::links::LinkKind::Org, &endpoint_b, &HashSet::new(),)
            .unwrap()
            .is_empty(),
        "a visible endpoint must not reveal a context-disabled neighbour"
    );
    db.set_org_context_enabled(org_a, true).unwrap();
    assert_eq!(
        db.org_link_target_visible(&endpoint_a).unwrap(),
        Some(("a-next".into(), "A next".into()))
    );
    db.evict_org_item("a-old").unwrap();
    db.evict_org_item("a-new").unwrap();
    assert_eq!(link_count(&db, "org", &endpoint_a, "manual"), 1);
    db.evict_org_item("a-next").unwrap();
    assert_eq!(
        link_count(&db, "org", &endpoint_a, "manual"),
        1,
        "final withdrawal withholds rather than destroys a private link"
    );
    assert!(db.org_link_target_visible(&endpoint_a).unwrap().is_none());
}

#[test]
fn manual_org_links_accept_view_only_same_and_cross_org_but_refuse_noncurrent_head() {
    let db = mem_db();
    let org_a = "11111111-1111-4111-8111-111111111111";
    let org_b = "22222222-2222-4222-8222-222222222222";
    let doc_a = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let doc_same_org = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    let doc_cross_org = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
    let doc_noncurrent = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";
    seed_org_state(&db, org_a);
    seed_org_state(&db, org_b);

    for (seq, item_id, org_id, doc_id, is_current) in [
        (1, "a-view", org_a, doc_a, true),
        (2, "a-other-view", org_a, doc_same_org, true),
        (3, "b-view", org_b, doc_cross_org, true),
        (4, "a-obsolete", org_a, doc_noncurrent, false),
    ] {
        db.upsert_org_item(
            item_id,
            org_id,
            seq,
            "owner",
            item_id,
            "body",
            "2026-08-13T00:00:00Z",
            1,
            1,
            &sha32(seq as u8),
            None,
            Some("owner"),
            None,
        )
        .unwrap();
        db.set_org_item_document_metadata(item_id, Some(doc_id), "view", Some("owner"))
            .unwrap();
        db.repair_org_reconcile_metadata(
            item_id,
            org_id,
            1,
            Some(doc_id),
            "view",
            Some("owner"),
            is_current,
        )
        .unwrap();
    }

    let a = format!("{org_a}:{doc_a}");
    let same_org = format!("{org_a}:{doc_same_org}");
    let cross_org = format!("{org_b}:{doc_cross_org}");
    let noncurrent = format!("{org_a}:{doc_noncurrent}");
    db.upsert_manual_link("org", &a, "org", &same_org)
        .expect("view permission is sufficient for a same-org private link");
    db.upsert_manual_link("org", &same_org, "org", &cross_org)
        .expect("view permission is sufficient for a cross-org private link");
    assert_eq!(manual_link_tuple_count(&db, "org", &a, "org", &same_org), 1);
    assert_eq!(
        manual_link_tuple_count(&db, "org", &same_org, "org", &cross_org),
        1
    );

    assert!(matches!(
        db.upsert_manual_link("org", &noncurrent, "org", &a),
        Err(crate::error::AppError::Locked(_))
    ));
    assert_eq!(
        manual_link_tuple_count(&db, "org", &noncurrent, "org", &a),
        0,
        "a live but non-current revision must fail the in-transaction endpoint gate"
    );
}

#[test]
fn bidirectional_org_manual_links_preserve_and_delete_exact_directed_tuples() {
    let db = mem_db();
    let org_id = "11111111-1111-4111-8111-111111111111";
    let left_doc = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let right_doc = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    seed_org_state(&db, org_id);
    for (seq, item_id, doc_id) in [
        (1, "left-current", left_doc),
        (2, "right-current", right_doc),
    ] {
        db.upsert_org_item(
            item_id,
            org_id,
            seq,
            "owner",
            item_id,
            "body",
            "2026-08-13T00:00:00Z",
            1,
            1,
            &sha32(seq as u8),
            None,
            Some("owner"),
            None,
        )
        .unwrap();
        db.set_org_item_document_metadata(item_id, Some(doc_id), "view", Some("owner"))
            .unwrap();
        db.repair_org_reconcile_metadata(
            item_id,
            org_id,
            1,
            Some(doc_id),
            "view",
            Some("owner"),
            true,
        )
        .unwrap();
    }
    let left = format!("{org_id}:{left_doc}");
    let right = format!("{org_id}:{right_doc}");
    let forward = crate::storage::models::ManualLinkEdge {
        src_kind: "org".into(),
        src_id: left.clone(),
        dst_kind: "org".into(),
        dst_id: right.clone(),
    };
    let reverse = crate::storage::models::ManualLinkEdge {
        src_kind: "org".into(),
        src_id: right.clone(),
        dst_kind: "org".into(),
        dst_id: left.clone(),
    };

    db.upsert_manual_link("org", &left, "org", &right).unwrap();
    db.upsert_manual_link("org", &right, "org", &left).unwrap();
    let collapsed = db
        .links_for_visible(crate::links::LinkKind::Org, &left, &HashSet::new())
        .unwrap();
    assert_eq!(collapsed.len(), 1, "two directions collapse to one chip");
    assert!(collapsed[0].manual_edges.contains(&forward));
    assert!(collapsed[0].manual_edges.contains(&reverse));

    db.delete_manual_link("org", &left, "org", &right).unwrap();
    assert_eq!(manual_link_tuple_count(&db, "org", &left, "org", &right), 0);
    assert_eq!(
        manual_link_tuple_count(&db, "org", &right, "org", &left),
        1,
        "deleting one direction must preserve the reverse exact tuple"
    );

    db.upsert_manual_link("org", &left, "org", &right).unwrap();
    assert!(!db
        .delete_manual_links(&[forward.clone(), reverse.clone()])
        .unwrap());
    assert_eq!(manual_link_tuple_count(&db, "org", &left, "org", &right), 0);
    assert_eq!(manual_link_tuple_count(&db, "org", &right, "org", &left), 0);
}

#[test]
fn leaving_org_preserves_private_composite_link_rows_but_withholds_endpoint() {
    let db = mem_db();
    let org_a = "11111111-1111-4111-8111-111111111111";
    let org_b = "22222222-2222-4222-8222-222222222222";
    let doc_a = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let doc_b = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    seed_org_state(&db, org_a);
    seed_org_state(&db, org_b);
    for (item, org, doc) in [("a", org_a, doc_a), ("b", org_b, doc_b)] {
        db.upsert_org_item(
            item,
            org,
            1,
            "owner",
            item,
            "body",
            "2026-08-12T00:00:00Z",
            1,
            1,
            &sha32(1),
            None,
            Some("owner"),
            None,
        )
        .unwrap();
        db.set_org_item_document_metadata(item, Some(doc), "view", Some("owner"))
            .unwrap();
        db.repair_org_reconcile_metadata(item, org, 1, Some(doc), "view", Some("owner"), true)
            .unwrap();
    }
    let endpoint_a = format!("{org_a}:{doc_a}");
    let endpoint_b = format!("{org_b}:{doc_b}");
    db.upsert_manual_link("org", &endpoint_a, "org", &endpoint_b)
        .unwrap();

    assert!(db.delete_org_state(org_a).unwrap());
    assert_eq!(
        link_count(&db, "org", &endpoint_a, "manual"),
        1,
        "leaving must preserve the user's opaque private graph row"
    );
    assert!(
        db.links_for_visible(crate::links::LinkKind::Org, &endpoint_a, &HashSet::new())
            .unwrap()
            .is_empty(),
        "the preserved row must remain completely withheld without membership"
    );
    assert!(
        db.links_for_visible(crate::links::LinkKind::Org, &endpoint_b, &HashSet::new())
            .unwrap()
            .is_empty(),
        "a still-joined org must not reveal a neighbour from an org this device left"
    );
    assert_eq!(
        db.org_link_target_visible(&endpoint_b).unwrap(),
        Some(("b".into(), "b".into())),
        "leaving one org must not evict another org reusing the link domain"
    );
}

/// An item's OCK generation is a historical encryption-key witness, not a requirement that the
/// locally cached membership generation still equal it. A rotated member may receive an older live
/// feed cell after learning generation N. Disabling Shared Brain context hides reads, but must not
/// stall the append-only cursor or discard ciphertext that becomes readable again on re-enable.
#[test]
fn historical_generation_and_disabled_context_feed_commit_converge_without_visibility() {
    let db = mem_db();
    seed_org_state(&db, STABLE_ORG_ID);
    db.set_org_generation(STABLE_ORG_ID, 2).unwrap();
    db.set_org_context_enabled(STABLE_ORG_ID, false).unwrap();
    let prepared = Db::prepare_org_item_index("Historical", "t", "private history", None).unwrap();

    let outcome = db
        .commit_org_feed_item_with_metadata(
            "historical-item",
            STABLE_ORG_ID,
            7,
            "owner",
            "Historical",
            "private history",
            "2026-08-13T00:00:00Z",
            1,
            1,
            &sha32(7),
            None,
            Some("owner"),
            &prepared,
            Some(STABLE_DOC_ID),
            "view",
            Some("owner"),
            true,
        )
        .unwrap();

    assert!(
        outcome.changed,
        "generation N-1 must ingest under membership N"
    );
    assert_eq!(
        db.org_last_seq_for(STABLE_ORG_ID).unwrap(),
        7,
        "context disable must not stall the durable feed cursor"
    );
    assert!(
        db.get_org_item("historical-item").unwrap().is_none(),
        "disabled context must withhold the locally converged plaintext"
    );
    assert!(
        db.search_org_chunks_fts("private history", 10)
            .unwrap()
            .is_empty(),
        "disabled context must also withhold retrieval"
    );

    db.set_org_context_enabled(STABLE_ORG_ID, true).unwrap();
    assert_eq!(
        db.get_org_item("historical-item")
            .unwrap()
            .map(|item| item.title),
        Some("Historical".into()),
        "re-enable restores the already-converged replica without a replay"
    );

    let future = Db::prepare_org_item_index("Future", "t", "future body", None).unwrap();
    let future_outcome = db
        .commit_org_feed_item_with_metadata(
            "future-item",
            STABLE_ORG_ID,
            8,
            "owner",
            "Future",
            "future body",
            "2026-08-13T00:00:01Z",
            1,
            3,
            &sha32(8),
            None,
            Some("owner"),
            &future,
            None,
            "view",
            None,
            false,
        )
        .unwrap();
    assert!(
        !future_outcome.changed,
        "a future generation is not yet admissible"
    );
    assert_eq!(db.org_last_seq_for(STABLE_ORG_ID).unwrap(), 7);
}

/// A predecessor tombstone can arrive before this client can open/ingest the successor. The stable
/// endpoint must be withheld during that gap, not destructively erased from the user's private graph;
/// once the successor lands, the exact row becomes visible again without reconstruction.
#[test]
fn predecessor_tombstone_withholds_stable_link_until_successor_ingest() {
    let db = mem_db();
    let org_id = "11111111-1111-4111-8111-111111111111";
    let doc_id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let endpoint = format!("{org_id}:{doc_id}");
    seed_org_state(&db, org_id);
    let old = Db::prepare_org_item_index("Old", "t", "old body", None).unwrap();
    db.commit_org_feed_item_with_metadata(
        "old-item",
        org_id,
        1,
        "owner",
        "Old",
        "old body",
        "2026-08-13T00:00:00Z",
        1,
        1,
        &sha32(1),
        None,
        Some("owner"),
        &old,
        Some(doc_id),
        "edit",
        Some("owner"),
        true,
    )
    .unwrap();
    db.upsert_manual_link("org", &endpoint, "meeting", "local-anchor")
        .unwrap();

    assert!(db.commit_org_feed_tombstone(org_id, "old-item", 2).unwrap());
    assert_eq!(
        link_count(&db, "org", &endpoint, "manual"),
        1,
        "the opaque user edge survives the unavailable-successor gap"
    );
    assert!(db.org_link_target_visible(&endpoint).unwrap().is_none());

    let next = Db::prepare_org_item_index("Current", "t", "current body", None).unwrap();
    db.commit_org_feed_item_with_metadata(
        "current-item",
        org_id,
        3,
        "owner",
        "Current",
        "current body",
        "2026-08-13T00:00:02Z",
        2,
        1,
        &sha32(2),
        None,
        Some("editor"),
        &next,
        Some(doc_id),
        "edit",
        Some("owner"),
        true,
    )
    .unwrap();
    assert_eq!(
        db.org_link_target_visible(&endpoint).unwrap(),
        Some(("current-item".into(), "Current".into()))
    );
    assert_eq!(link_count(&db, "org", &endpoint, "manual"), 1);
}

/// The narrow legacy single-edge API is a restore/retry primitive and therefore idempotent. The
/// collapsed-chip batch API remains strict and atomic: one stale tuple rolls the complete set back.
#[test]
fn single_manual_delete_is_idempotent_while_collapsed_batch_stays_strict() {
    let db = mem_db();
    seed_folder(&db, "f1", "Notes");
    seed_note_doc(&db, "left", "f1", "Left", "");
    seed_note_doc(&db, "right", "f1", "Right", "");

    db.upsert_manual_link("note", "left", "note", "right")
        .unwrap();
    db.delete_manual_link("note", "left", "note", "right")
        .unwrap();
    db.delete_manual_link("note", "left", "note", "right")
        .expect("a restore/retry delete of the same single tuple is success");

    let legacy_marker = crate::enrich::apply_link_markers(
        "left prose",
        &[crate::enrich::ContextHit {
            source: "note".into(),
            detail: "[[Right]]".into(),
            url: None,
        }],
    );
    db.set_document_text("left", &legacy_marker).unwrap();
    db.set_note_doc_exported_path("left", Some("/vault/left.md"))
        .unwrap();
    db.delete_manual_link("note", "left", "note", "right")
        .expect("missing-row retry still commits legacy marker cleanup");
    let left_text: String = db
        .lock()
        .query_row("SELECT text FROM documents WHERE id = 'left'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(!left_text.contains("[[Right]]"));
    let cleanup = db.pending_lock_marker_export_cleanup().unwrap();
    assert!(cleanup.iter().any(|row| {
        row.source_kind == "note"
            && row.source_id == "left"
            && row.exported_path == "/vault/left.md"
    }));

    db.upsert_manual_link("note", "left", "note", "right")
        .unwrap();
    db.upsert_manual_link("note", "right", "note", "left")
        .unwrap();
    db.delete_manual_link("note", "left", "note", "right")
        .unwrap();
    let requested = [
        crate::storage::models::ManualLinkEdge {
            src_kind: "note".into(),
            src_id: "left".into(),
            dst_kind: "note".into(),
            dst_id: "right".into(),
        },
        crate::storage::models::ManualLinkEdge {
            src_kind: "note".into(),
            src_id: "right".into(),
            dst_kind: "note".into(),
            dst_id: "left".into(),
        },
    ];
    assert!(db.delete_manual_links(&requested).is_err());
    assert_eq!(
        link_count(&db, "note", "right", "manual"),
        1,
        "strict batch failure rolls back the still-existing reverse tuple"
    );
}

#[test]
fn locked_manual_marker_cleanup_verifies_seal_before_atomic_replacement() {
    let db = mem_db();
    seed_folder(&db, "f-marker-seal", "Locked notes");
    seed_note_doc(&db, "marker-left", "f-marker-seal", "Left", "");
    seed_note_doc(&db, "marker-right", "f-marker-seal", "Right", "");
    let marker = crate::enrich::apply_link_markers(
        "left prose",
        &[crate::enrich::ContextHit {
            source: "note".into(),
            detail: "[[Right]]".into(),
            url: None,
        }],
    );
    let stripped = crate::enrich::strip_managed_links_block(&marker);
    db.set_document_text("marker-left", &marker).unwrap();
    db.set_note_doc_exported_path("marker-left", Some("/vault/marker-left.md"))
        .unwrap();
    db.lock()
        .execute_batch(
            "UPDATE folders SET locked=1 WHERE id='f-marker-seal';
         UPDATE documents SET text_blob=X'01' WHERE id='marker-left';",
        )
        .unwrap();
    db.upsert_manual_link("note", "marker-left", "note", "marker-right")
        .unwrap();

    let edge = crate::storage::models::ManualLinkEdge {
        src_kind: "note".into(),
        src_id: "marker-left".into(),
        dst_kind: "note".into(),
        dst_id: "marker-right".into(),
    };
    let content_key = zeroize::Zeroizing::new([7_u8; 32]);
    let aad = b"murmur:document:v1|folder=f-marker-seal|document=marker-left|type=document";
    let blob = crate::crypto::encrypt(&content_key, stripped.as_bytes(), aad).unwrap();
    let invalid = crate::storage::links::PreparedManualMarkerSeal {
        note_id: "marker-left".into(),
        folder_id: "f-marker-seal".into(),
        stripped_text: stripped.clone(),
        text_blob: blob.clone(),
        content_key: zeroize::Zeroizing::new([8_u8; 32]),
    };
    assert!(db
        .delete_manual_links_with_marker_seals(std::slice::from_ref(&edge), &[invalid])
        .is_err());
    assert_eq!(
        db.get_note_row("marker-left").unwrap().unwrap().text,
        marker
    );
    assert_eq!(link_count(&db, "note", "marker-left", "manual"), 1);
    assert!(db.pending_lock_marker_export_cleanup().unwrap().is_empty());

    let valid = crate::storage::links::PreparedManualMarkerSeal {
        note_id: "marker-left".into(),
        folder_id: "f-marker-seal".into(),
        stripped_text: stripped.clone(),
        text_blob: blob,
        content_key: content_key.clone(),
    };
    assert!(db
        .delete_manual_links_with_marker_seals(&[edge], &[valid])
        .unwrap());
    let stored = db.get_note_row("marker-left").unwrap().unwrap();
    let stored_blob: Vec<u8> = db
        .lock()
        .query_row(
            "SELECT text_blob FROM documents WHERE id='marker-left'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        stored.text.is_empty(),
        "a locked note must not retain plaintext beside its verified ciphertext"
    );
    assert_eq!(
        crate::crypto::decrypt(&content_key, &stored_blob, aad).unwrap(),
        stripped.as_bytes(),
        "the committed locked representation must decrypt byte-identical"
    );
    assert_eq!(link_count(&db, "note", "marker-left", "manual"), 0);
    assert_eq!(db.pending_lock_marker_export_cleanup().unwrap().len(), 1);
}

#[test]
fn current_document_management_does_not_mutate_origin_cas_baseline() {
    let db = mem_db();
    seed_org_state(&db, STABLE_ORG_ID);
    db.insert_org_share(
        "share-1",
        STABLE_ORG_ID,
        None,
        Some("local-note"),
        "note",
        Some("Local"),
        1,
        1,
        &sha32(1),
        "2026-08-12T00:00:00Z",
    )
    .unwrap();
    db.set_org_share_uploaded("share-1", "old-item", "2026-08-12T00:00:01Z")
        .unwrap();
    db.set_org_share_document_metadata("share-1", STABLE_DOC_ID, "view")
        .unwrap();
    db.set_org_share_document_metadata("share-1", STABLE_DOC_ID, "edit")
        .unwrap();
    db.clear_org_share_document_metadata("share-1").unwrap();
    let cleared = db.org_share_by_item("old-item").unwrap().unwrap();
    assert_eq!(cleared.doc_id, None);
    assert_eq!(cleared.access, "view");
    db.set_org_share_document_metadata("share-1", STABLE_DOC_ID, "view")
        .unwrap();

    for (item, seq, rev, access) in [("old-item", 1, 1, "view"), ("remote-current", 2, 2, "edit")] {
        db.upsert_org_item(
            item,
            STABLE_ORG_ID,
            seq,
            "owner",
            item,
            "body",
            "2026-08-12T00:00:00Z",
            rev,
            1,
            &sha32(rev as u8),
            None,
            Some("editor"),
            None,
        )
        .unwrap();
        db.set_org_item_document_metadata(item, Some(STABLE_DOC_ID), access, Some("owner"))
            .unwrap();
    }
    db.repair_org_reconcile_metadata(
        "remote-current",
        STABLE_ORG_ID,
        1,
        Some(STABLE_DOC_ID),
        "edit",
        Some("owner"),
        true,
    )
    .unwrap();
    db.evict_org_item("old-item").unwrap();

    // Local journal ids and relay item ids are separate namespaces. A colliding local id must not
    // hijack a revoke request for the current relay item.
    db.insert_org_share(
        "remote-current",
        STABLE_ORG_ID,
        None,
        Some("collision-source"),
        "note",
        Some("Unrelated"),
        1,
        1,
        &sha32(9),
        "2026-08-12T00:00:02Z",
    )
    .unwrap();
    db.set_org_share_uploaded(
        "remote-current",
        "unrelated-relay-item",
        "2026-08-12T00:00:03Z",
    )
    .unwrap();
    db.set_org_share_document_metadata(
        "remote-current",
        "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
        "view",
    )
    .unwrap();

    assert_eq!(
        db.current_org_document_status(STABLE_ORG_ID, STABLE_DOC_ID)
            .unwrap(),
        Some(("remote-current".into(), 2, "edit".into()))
    );
    assert!(db.org_share_by_item("remote-current").unwrap().is_none());
    let management = db
        .org_share_for_revoke_target("remote-current")
        .unwrap()
        .unwrap();
    assert_eq!(management.id, "share-1");
    assert_eq!(management.item_id.as_deref(), Some("old-item"));
    assert_eq!(
        management.rev, 1,
        "remote feed must not rewrite expectedRev"
    );

    assert!(db.evict_org_document(STABLE_ORG_ID, STABLE_DOC_ID).unwrap());
    assert!(db
        .current_org_document_status(STABLE_ORG_ID, STABLE_DOC_ID)
        .unwrap()
        .is_none());
}

#[test]
fn org_access_attempts_are_exact_transactional_cas_across_failures_and_accounts() {
    let db = mem_db();
    seed_org_state(&db, STABLE_ORG_ID);
    let actor = "c534b6d2-02c1-4c2c-a256-3af8592b1567";
    let other_actor = "d645c7e3-13d2-4d3d-b367-4bf9603c2678";
    db.upsert_org_item(
        "access-head",
        STABLE_ORG_ID,
        1,
        actor,
        "Shared",
        "body",
        "2026-08-12T00:00:00Z",
        1,
        1,
        &sha32(1),
        None,
        Some(actor),
        None,
    )
    .unwrap();
    db.set_org_item_document_metadata("access-head", Some(STABLE_DOC_ID), "edit", Some(actor))
        .unwrap();
    db.repair_org_reconcile_metadata(
        "access-head",
        STABLE_ORG_ID,
        1,
        Some(STABLE_DOC_ID),
        "edit",
        Some(actor),
        true,
    )
    .unwrap();
    db.insert_org_share(
        "access-share",
        STABLE_ORG_ID,
        None,
        Some("access-source"),
        "note",
        Some("Shared"),
        1,
        1,
        &sha32(1),
        "2026-08-12T00:00:00Z",
    )
    .unwrap();
    db.set_org_share_uploaded("access-share", "access-head", "2026-08-12T00:00:01Z")
        .unwrap();
    db.set_org_share_document_metadata("access-share", STABLE_DOC_ID, "edit")
        .unwrap();

    let dispatch_1 = "11111111-2222-4333-8444-555555555555";
    assert!(db
        .persist_org_access_attempt_if_current(
            1,
            "relay.example",
            dispatch_1,
            STABLE_ORG_ID,
            STABLE_DOC_ID,
            "edit",
            "view",
            actor,
            actor,
            "2026-08-12T00:00:02Z",
        )
        .unwrap());
    assert_eq!(db.pending_org_access_attempts().unwrap().len(), 1);
    assert_eq!(
        db.count_share_egress_by_kind("org_share_access").unwrap(),
        1
    );
    assert!(!db
        .persist_org_access_attempt_if_current(
            2,
            "relay.example",
            dispatch_1,
            STABLE_ORG_ID,
            STABLE_DOC_ID,
            "edit",
            "view",
            actor,
            actor,
            "2026-08-12T00:00:03Z",
        )
        .unwrap());
    assert_eq!(
        db.count_share_egress_by_kind("org_share_access").unwrap(),
        1
    );
    assert!(!db
        .apply_org_access_attempt_if_current(
            dispatch_1,
            STABLE_ORG_ID,
            STABLE_DOC_ID,
            "edit",
            "view",
            other_actor,
            actor,
        )
        .unwrap());
    assert!(!db
        .apply_org_access_attempt_if_current(
            dispatch_1,
            STABLE_ORG_ID,
            "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
            "edit",
            "view",
            actor,
            actor,
        )
        .unwrap());
    db.lock()
        .execute_batch(
            "CREATE TRIGGER abort_access_projection BEFORE UPDATE OF access ON org_items
         BEGIN SELECT RAISE(ABORT, 'access projection fault'); END;",
        )
        .unwrap();
    assert!(db
        .apply_org_access_attempt_if_current(
            dispatch_1,
            STABLE_ORG_ID,
            STABLE_DOC_ID,
            "edit",
            "view",
            actor,
            actor,
        )
        .is_err());
    assert_eq!(db.pending_org_access_attempts().unwrap().len(), 1);
    assert_eq!(
        db.get_org_item("access-head").unwrap().unwrap().access,
        "edit"
    );
    assert_eq!(
        db.get_org_share("access-share").unwrap().unwrap().access,
        "edit"
    );
    db.lock()
        .execute_batch("DROP TRIGGER abort_access_projection;")
        .unwrap();
    assert!(db
        .apply_org_access_attempt_if_current(
            dispatch_1,
            STABLE_ORG_ID,
            STABLE_DOC_ID,
            "edit",
            "view",
            actor,
            actor,
        )
        .unwrap());
    assert_eq!(
        db.get_org_item("access-head").unwrap().unwrap().access,
        "view"
    );
    assert_eq!(
        db.get_org_share("access-share").unwrap().unwrap().access,
        "view"
    );
    assert!(db.pending_org_access_attempts().unwrap().is_empty());
    assert!(!db
        .apply_org_access_attempt_if_current(
            dispatch_1,
            STABLE_ORG_ID,
            STABLE_DOC_ID,
            "edit",
            "view",
            actor,
            actor,
        )
        .unwrap());

    let dispatch_2 = "22222222-3333-4444-8555-666666666666";
    assert!(db
        .persist_org_access_attempt_if_current(
            3,
            "relay.example",
            dispatch_2,
            STABLE_ORG_ID,
            STABLE_DOC_ID,
            "view",
            "edit",
            actor,
            actor,
            "2026-08-12T00:00:04Z",
        )
        .unwrap());
    assert!(!db
        .fail_org_access_attempt_if_current(
            dispatch_2,
            STABLE_ORG_ID,
            STABLE_DOC_ID,
            "view",
            "edit",
            other_actor,
            actor,
        )
        .unwrap());
    assert!(db
        .fail_org_access_attempt_if_current(
            dispatch_2,
            STABLE_ORG_ID,
            STABLE_DOC_ID,
            "view",
            "edit",
            actor,
            actor,
        )
        .unwrap());
    assert!(db.pending_org_access_attempts().unwrap().is_empty());

    let dispatch_3 = "33333333-4444-4555-8666-777777777777";
    assert!(db
        .persist_org_access_attempt_if_current(
            4,
            "relay.example",
            dispatch_3,
            STABLE_ORG_ID,
            STABLE_DOC_ID,
            "view",
            "edit",
            actor,
            actor,
            "2026-08-12T00:00:05Z",
        )
        .unwrap());
    assert!(!db
        .apply_org_access_attempt_if_current(
            dispatch_2,
            STABLE_ORG_ID,
            STABLE_DOC_ID,
            "view",
            "edit",
            actor,
            actor,
        )
        .unwrap());
    db.lock().execute(
        "UPDATE org_shares SET state='failed',last_error='direct_put_pending' WHERE id='access-share'",
        [],
    ).unwrap();
    assert!(!db
        .apply_org_access_attempt_if_current(
            dispatch_3,
            STABLE_ORG_ID,
            STABLE_DOC_ID,
            "view",
            "edit",
            actor,
            actor,
        )
        .unwrap());
    assert_eq!(
        db.get_org_item("access-head").unwrap().unwrap().access,
        "view"
    );
    assert_eq!(db.pending_org_access_attempts().unwrap().len(), 1);
}

#[test]
fn feed_repairs_stable_metadata_even_when_content_action_is_already_applied() {
    let db = mem_db();
    seed_org_state(&db, STABLE_ORG_ID);
    db.upsert_org_item(
        "local-first",
        STABLE_ORG_ID,
        5,
        "owner",
        "Title",
        "body",
        "2026-08-12T00:00:00Z",
        1,
        1,
        &sha32(4),
        None,
        Some("owner"),
        None,
    )
    .unwrap();
    db.set_org_last_seq(STABLE_ORG_ID, 5).unwrap();
    let prepared = Db::prepare_org_item_index("Title", "t", "body", None).unwrap();
    let applied = db
        .commit_org_feed_item_with_metadata(
            "local-first",
            STABLE_ORG_ID,
            5,
            "owner",
            "Title",
            "body",
            "2026-08-12T00:00:00Z",
            1,
            1,
            &sha32(4),
            None,
            Some("owner"),
            &prepared,
            Some(STABLE_DOC_ID),
            "edit",
            Some("owner"),
            true,
        )
        .unwrap();
    assert!(
        applied.changed,
        "a legacy row without the atomic projection witness must repair its full projection"
    );
    assert_eq!(
        db.org_replica_state("local-first")
            .unwrap()
            .unwrap()
            .projection_sha256
            .as_deref(),
        Some(sha32(4).as_slice())
    );
    let ctx = db.org_item_edit_ctx("local-first").unwrap().unwrap();
    assert_eq!(ctx.doc_id.as_deref(), Some(STABLE_DOC_ID));
    assert_eq!(ctx.access, "edit");
    assert_eq!(ctx.document_owner_user_id.as_deref(), Some("owner"));
}

#[test]
fn anti_entropy_metadata_preserves_org_link_across_supersede_and_repairs_hash_equal_row() {
    let db = mem_db();
    let org_id = "11111111-1111-4111-8111-111111111111";
    let doc_id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let endpoint = format!("{org_id}:{doc_id}");
    seed_org_state(&db, org_id);
    db.upsert_org_item(
        "old-rev",
        org_id,
        1,
        "owner",
        "Old",
        "old body",
        "2026-08-12T00:00:00Z",
        1,
        1,
        &sha32(1),
        None,
        Some("owner"),
        None,
    )
    .unwrap();
    db.set_org_item_document_metadata("old-rev", Some(doc_id), "view", Some("owner"))
        .unwrap();
    db.repair_org_reconcile_metadata(
        "old-rev",
        org_id,
        1,
        Some(doc_id),
        "view",
        Some("owner"),
        true,
    )
    .unwrap();
    db.upsert_org_item(
        "attacker-max-rev",
        org_id,
        99,
        "attacker",
        "Forged max rev",
        "must never navigate",
        "2026-08-12T00:00:00Z",
        u32::MAX,
        1,
        &sha32(9),
        None,
        Some("attacker"),
        None,
    )
    .unwrap();
    db.set_org_item_document_metadata("attacker-max-rev", Some(doc_id), "edit", Some("attacker"))
        .unwrap();
    db.upsert_manual_link("org", &endpoint, "meeting", "local-anchor")
        .unwrap();

    let prepared = Db::prepare_org_item_index("Current", "t", "current body", None).unwrap();
    assert!(
        db.commit_org_reconcile_item_with_metadata(
            "current-rev",
            org_id,
            2,
            "owner",
            "Current",
            "current body",
            "2026-08-12T00:00:00Z",
            2,
            1,
            &sha32(2),
            None,
            Some("editor"),
            &prepared,
            Some(doc_id),
            "edit",
            Some("owner"),
            true,
        )
        .unwrap()
        .changed
    );
    db.evict_org_item("old-rev").unwrap();
    assert_eq!(link_count(&db, "org", &endpoint, "manual"), 1);
    assert_eq!(
        db.org_link_target_visible(&endpoint).unwrap(),
        Some(("current-rev".into(), "Current".into()))
    );

    // A later hash-converged anti-entropy pass repairs permission metadata without re-fetching or
    // rewriting the already-correct plaintext/index.
    db.lock()
        .execute(
            "UPDATE org_items SET doc_id = NULL, access = 'view', document_owner_user_id = NULL
              WHERE item_id = 'current-rev'",
            [],
        )
        .unwrap();
    assert!(
        db.repair_org_reconcile_metadata(
            "current-rev",
            org_id,
            1,
            Some(doc_id),
            "edit",
            Some("owner"),
            true,
        )
        .unwrap()
        .changed
    );
    let ctx = db.org_item_edit_ctx("current-rev").unwrap().unwrap();
    assert_eq!(ctx.doc_id.as_deref(), Some(doc_id));
    assert_eq!(ctx.access, "edit");
    assert_eq!(ctx.document_owner_user_id.as_deref(), Some("owner"));
}

/// Every stable-head assignment can demote readable plaintext. The demotion and global-derived Ask
/// invalidation must be one SQLCipher transaction across local publish, live feed, reconcile ingest,
/// and metadata-only reconcile (including an explicit current→non-current repair).
#[test]
fn stable_current_mutations_atomically_invalidate_global_ask_history() {
    for mode in [
        "local",
        "feed",
        "reconcile",
        "repair_assign",
        "repair_demote",
    ] {
        let db = mem_db();
        seed_org_state(&db, STABLE_ORG_ID);
        seed_folder(&db, "f-dep", "Dependency");
        for (item_id, seq, rev) in [("old-current", 1_u64, 1_u32), ("new-head", 2, 2)] {
            db.upsert_org_item(
                item_id,
                STABLE_ORG_ID,
                seq,
                "owner",
                item_id,
                "shared body",
                "2026-08-13T00:00:00Z",
                rev,
                1,
                &sha32(rev as u8),
                None,
                Some("owner"),
                None,
            )
            .unwrap();
            db.set_org_item_document_metadata(item_id, Some(STABLE_DOC_ID), "view", Some("owner"))
                .unwrap();
        }
        db.repair_org_reconcile_metadata(
            "old-current",
            STABLE_ORG_ID,
            1,
            Some(STABLE_DOC_ID),
            "view",
            Some("owner"),
            true,
        )
        .unwrap();
        db.persist_ask_exchange(
            &crate::storage::models::AskConversationScope::Vault,
            None,
            "question",
            "derived from old current org content",
            &[],
            &[],
            &[],
            &["f-dep".to_string()],
            "2026-08-13T00:01:00Z",
        )
        .unwrap();
        let generation_before: i64 = db
            .lock()
            .query_row(
                "SELECT visibility_generation FROM ask_history_state WHERE singleton=1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let prepared =
            Db::prepare_org_item_index("new-head", "2026-08-13T00:00:00Z", "shared body", None)
                .unwrap();

        match mode {
            "local" => {
                db.commit_local_org_replica_with_metadata(
                    "new-head",
                    STABLE_ORG_ID,
                    2,
                    "owner",
                    "new-head",
                    "shared body",
                    "2026-08-13T00:00:00Z",
                    2,
                    1,
                    &sha32(2),
                    None,
                    Some("owner"),
                    &prepared,
                    None,
                    Some(STABLE_DOC_ID),
                    "view",
                    Some("owner"),
                    true,
                )
                .unwrap();
            }
            "feed" => {
                db.commit_org_feed_item_with_metadata(
                    "new-head",
                    STABLE_ORG_ID,
                    3,
                    "owner",
                    "new-head",
                    "shared body",
                    "2026-08-13T00:00:00Z",
                    3,
                    1,
                    &sha32(3),
                    None,
                    Some("owner"),
                    &prepared,
                    Some(STABLE_DOC_ID),
                    "view",
                    Some("owner"),
                    true,
                )
                .unwrap();
            }
            "reconcile" => {
                db.commit_org_reconcile_item_with_metadata(
                    "new-head",
                    STABLE_ORG_ID,
                    3,
                    "owner",
                    "new-head",
                    "shared body",
                    "2026-08-13T00:00:00Z",
                    3,
                    1,
                    &sha32(3),
                    None,
                    Some("owner"),
                    &prepared,
                    Some(STABLE_DOC_ID),
                    "view",
                    Some("owner"),
                    true,
                )
                .unwrap();
            }
            "repair_assign" => {
                db.repair_org_reconcile_metadata(
                    "new-head",
                    STABLE_ORG_ID,
                    1,
                    Some(STABLE_DOC_ID),
                    "view",
                    Some("owner"),
                    true,
                )
                .unwrap();
            }
            "repair_demote" => {
                db.repair_org_reconcile_metadata(
                    "old-current",
                    STABLE_ORG_ID,
                    1,
                    Some(STABLE_DOC_ID),
                    "view",
                    Some("owner"),
                    false,
                )
                .unwrap();
            }
            _ => unreachable!(),
        }

        let conn = db.lock();
        let generation_after: i64 = conn
            .query_row(
                "SELECT visibility_generation FROM ask_history_state WHERE singleton=1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(generation_after, generation_before + 1, "mode={mode}");
        for table in [
            "ask_conversations",
            "ask_conversation_messages",
            "ask_conversation_dependencies",
        ] {
            let count: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, 0, "mode={mode} table={table}");
        }
    }
}

/// Ingest round-trip: upsert an org item WITH a real (stub) embedder → both the int8 KNN leg AND
/// the FTS leg retrieve it back with the right author/title/snippet + content hash.
#[test]
fn org_ingest_round_trips_through_int8_knn_and_fts() {
    let db = mem_db();
    seed_org_state(&db, "org-1");
    let emb = crate::embed::StubEmbedder;
    let sha = sha32(1);
    db.upsert_org_item(
        "it-1",
        "org-1",
        5,
        "anna",
        "Roadmap sync",
        "decided the budget for the apollo project this quarter",
        "2026-07-10T09:00:00Z",
        1,
        1,
        &sha,
        None,
        None,
        Some(&emb),
    )
    .unwrap();

    // FTS leg finds it.
    let fts = db.search_org_chunks_fts("apollo budget", 10).unwrap();
    assert_eq!(fts.len(), 1, "FTS must retrieve the ingested item");
    assert_eq!(fts[0].item_id, "it-1");
    assert_eq!(fts[0].author_hint, "anna");
    assert_eq!(fts[0].title, "Roadmap sync");
    assert_eq!(fts[0].content_sha256, sha);
    assert!(fts[0].snippet.contains("apollo"));

    // int8 KNN leg finds it (query embedded with the SAME stub embedder → identical space).
    let qv = emb
        .embed_query(&["apollo budget".to_string()])
        .unwrap()
        .remove(0);
    let knn = db.search_org_chunks_knn(&qv, 10, 0.0).unwrap();
    assert!(
        knn.iter().any(|h| h.item_id == "it-1"),
        "int8 KNN must retrieve the ingested item"
    );

    // The full decrypted item is readable for the viewer.
    let detail = db.get_org_item("it-1").unwrap().unwrap();
    assert_eq!(detail.title, "Roadmap sync");
    assert_eq!(detail.author_hint, "anna");
    assert!(detail.markdown.contains("apollo"));
}

/// FTS-only fallback: ingesting with NO embedder writes chunks (FTS-reachable) but ZERO int8
/// vectors — so the KNN leg finds nothing while FTS still does. Proves the StubEmbedder-absent
/// (ftsOnly) path writes no vectors at rest.
#[test]
fn org_ingest_fts_only_when_no_embedder() {
    let db = mem_db();
    seed_org_state(&db, "org-1");
    db.upsert_org_item(
        "it-x",
        "org-1",
        1,
        "bob",
        "Notes",
        "quarterly hiring plan for the platform team",
        "2026-07-10T09:00:00Z",
        1,
        1,
        &sha32(2),
        None, // source_kind: unclassified in this test
        None, // author_user_id: unknown in this test
        None, // no embedder → FTS-only
    )
    .unwrap();

    let fts = db.search_org_chunks_fts("hiring platform", 10).unwrap();
    assert_eq!(fts.len(), 1, "FTS reaches an FTS-only ingested item");

    // No vectors were written → a KNN over any vector finds nothing.
    let qv = crate::embed::StubEmbedder
        .embed_query(&["hiring platform".to_string()])
        .unwrap()
        .remove(0);
    let knn = db.search_org_chunks_knn(&qv, 10, 0.0).unwrap();
    assert!(knn.is_empty(), "no int8 vectors when ingested FTS-only");

    // The re-embed backlog lists exactly this item.
    let backlog = db.org_items_needing_embed("org-1", 10).unwrap();
    assert_eq!(backlog, vec!["it-x".to_string()]);
}

/// Stable-document retrieval must follow the relay-authoritative `is_current` witness, never the
/// largest locally observed revision. A live attacker-controlled high-rev row for the same doc is
/// kept in the replica for reconciliation, but must stay absent from every content reader and the
/// re-embed backlog.
#[test]
fn org_content_readers_expose_only_authoritative_current_revision() {
    let db = mem_db();
    let org_id = "11111111-1111-4111-8111-111111111111";
    let doc_id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    seed_org_state(&db, org_id);

    let conn = db.lock();
    for (item_id, title, rev, is_current) in [
        ("attacker-high-rev", "Attacker", 999_i64, 0_i64),
        ("authoritative-current", "Current", 2_i64, 1_i64),
    ] {
        conn.execute(
            "INSERT INTO org_items
               (item_id, org_id, seq, author_hint, title, markdown, created_at, rev, generation,
                content_sha256, tombstoned, doc_id, is_current)
             VALUES (?1, ?2, ?3, 'member', ?4, 'sapphire parcel body',
                     '2026-08-13T00:00:00Z', ?3, 1, ?5, 0, ?6, ?7)",
            rusqlite::params![
                item_id,
                org_id,
                rev,
                title,
                sha32(if is_current == 1 { 2 } else { 9 }),
                doc_id,
                is_current,
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO org_chunks (item_id, chunk_idx, text)
             VALUES (?1, 0, 'sapphire parcel body')",
            rusqlite::params![item_id],
        )
        .unwrap();
        let chunk_id = conn.last_insert_rowid();
        let blob = crate::embed::vec_to_int8_blob(&one_hot(0));
        conn.execute(
            "INSERT INTO org_vec_chunks(chunk_id, embedding) VALUES (?1, vec_int8(?2))",
            rusqlite::params![chunk_id, blob],
        )
        .unwrap();
    }
    drop(conn);

    let knn_ids = db
        .search_org_chunks_knn(&one_hot(0), 10, 0.0)
        .unwrap()
        .into_iter()
        .map(|hit| hit.item_id)
        .collect::<Vec<_>>();
    assert_eq!(knn_ids, vec!["authoritative-current"]);

    let strict_ids = db
        .search_org_chunks_fts("sapphire parcel", 10)
        .unwrap()
        .into_iter()
        .map(|hit| hit.item_id)
        .collect::<Vec<_>>();
    assert_eq!(strict_ids, vec!["authoritative-current"]);

    let fallback_ids = db
        .search_org_chunks_fts("missing sapphire", 10)
        .unwrap()
        .into_iter()
        .map(|hit| hit.item_id)
        .collect::<Vec<_>>();
    assert_eq!(fallback_ids, vec!["authoritative-current"]);

    db.lock()
        .execute(
            "DELETE FROM org_vec_chunks WHERE chunk_id IN
                 (SELECT id FROM org_chunks WHERE item_id IN
                     ('attacker-high-rev', 'authoritative-current'))",
            [],
        )
        .unwrap();
    let backlog = db.org_items_needing_embed(org_id, 10).unwrap();
    assert_eq!(backlog, vec!["authoritative-current"]);
}

/// Crash boundary: fetching a page never makes its advertised tail durable. Each successful
/// action advances the cursor in the SAME transaction as that exact action, so stopping after
/// item 10 leaves item 11 replayable on restart instead of silently skipping it.
#[test]
fn org_feed_cursor_stops_at_last_committed_action_not_fetched_page_tail() {
    let db = mem_db();
    seed_org_state(&db, "org-1");
    let first_sha = sha32(31);
    let first = Db::prepare_org_item_index("First", "t", "first body", None).unwrap();

    // Imagine the server page contained seq 10 and 11 (and advertised next_seq=11). The process
    // commits the first action, then crashes before applying the second.
    db.commit_org_feed_item(
        "it-10",
        "org-1",
        10,
        "anna",
        "First",
        "first body",
        "t",
        1,
        1,
        &first_sha,
        None,
        Some("author-1"),
        &first,
    )
    .unwrap();

    assert_eq!(db.org_last_seq_for("org-1").unwrap(), 10);
    assert!(db.get_org_item("it-10").unwrap().is_some());
    assert!(db.get_org_item("it-11").unwrap().is_none());

    // Restart/replay can still apply the uncommitted action and advances only to that action.
    let second_sha = sha32(32);
    let second = Db::prepare_org_item_index("Second", "t", "second body", None).unwrap();
    db.commit_org_feed_item(
        "it-11",
        "org-1",
        11,
        "bob",
        "Second",
        "second body",
        "t",
        1,
        1,
        &second_sha,
        None,
        Some("author-2"),
        &second,
    )
    .unwrap();
    assert_eq!(db.org_last_seq_for("org-1").unwrap(), 11);
}

/// A feed page may finish downloading after the user left the org. Membership and cursor claim
/// are checked in the SAME transaction before the first plaintext write, so the stale action is a
/// no-op rather than an orphaned decrypted replica that could reappear on a later rejoin.
#[test]
fn org_feed_commit_without_membership_cannot_insert_plaintext() {
    let db = mem_db();
    let sha = sha32(34);
    let prepared =
        Db::prepare_org_item_index("Withdrawn", "t", "must never persist", None).unwrap();

    let applied = db
        .commit_org_feed_item(
            "it-withdrawn",
            "org-left",
            7,
            "anna",
            "Withdrawn",
            "must never persist",
            "t",
            1,
            1,
            &sha,
            None,
            Some("author-1"),
            &prepared,
        )
        .unwrap();

    assert!(!applied, "withdrawn membership must reject the feed action");
    let (items, chunks): (i64, i64) = {
        let conn = db.lock();
        (
            conn.query_row("SELECT COUNT(*) FROM org_items", [], |r| r.get(0))
                .unwrap(),
            conn.query_row("SELECT COUNT(*) FROM org_chunks", [], |r| r.get(0))
                .unwrap(),
        )
    };
    assert_eq!((items, chunks), (0, 0));
    assert_eq!(db.org_last_seq_for("org-left").unwrap(), 0);
}

/// Two syncs can buffer the same live page. Once a newer tombstone claimed the cursor, an older
/// or equal live action must neither advance the cursor nor clear the tombstone/reinsert chunks.
#[test]
fn stale_live_feed_action_cannot_resurrect_tombstoned_item() {
    let db = mem_db();
    seed_org_state(&db, "org-1");
    let sha = sha32(35);
    let live = Db::prepare_org_item_index("Live", "t", "private body", None).unwrap();
    assert!(db
        .commit_org_feed_item(
            "it-race",
            "org-1",
            10,
            "anna",
            "Live",
            "private body",
            "t",
            1,
            1,
            &sha,
            None,
            Some("author-1"),
            &live,
        )
        .unwrap());
    assert!(db
        .commit_org_feed_tombstone("org-1", "it-race", 11)
        .unwrap());

    for stale_seq in [10, 11] {
        assert!(!db
            .commit_org_feed_item(
                "it-race",
                "org-1",
                stale_seq,
                "anna",
                "Stale",
                "resurrection payload",
                "t",
                2,
                1,
                &sha32(36),
                None,
                Some("author-1"),
                &Db::prepare_org_item_index("Stale", "t", "resurrection payload", None,).unwrap(),
            )
            .unwrap());
    }

    let (tombstoned, markdown, chunks): (i64, String, i64) = {
        let conn = db.lock();
        (
            conn.query_row(
                "SELECT tombstoned FROM org_items WHERE item_id = 'it-race'",
                [],
                |r| r.get(0),
            )
            .unwrap(),
            conn.query_row(
                "SELECT markdown FROM org_items WHERE item_id = 'it-race'",
                [],
                |r| r.get(0),
            )
            .unwrap(),
            conn.query_row(
                "SELECT COUNT(*) FROM org_chunks WHERE item_id = 'it-race'",
                [],
                |r| r.get(0),
            )
            .unwrap(),
        )
    };
    assert_eq!(db.org_last_seq_for("org-1").unwrap(), 11);
    assert_eq!(tombstoned, 1);
    assert!(markdown.is_empty());
    assert_eq!(chunks, 0);
}

/// Consent withdrawal is one SQLite transaction: membership, plaintext, FTS source chunks and
/// vector rows disappear together, so no crash boundary can leave a membership-less replica.
#[test]
fn delete_org_state_atomically_purges_the_complete_replica() {
    let db = mem_db();
    seed_org_state(&db, "org-1");
    db.upsert_org_item(
        "it-delete",
        "org-1",
        3,
        "anna",
        "Delete",
        "withdrawn body",
        "t",
        1,
        1,
        &sha32(37),
        None,
        Some("author-1"),
        Some(&crate::embed::StubEmbedder),
    )
    .unwrap();
    db.persist_ask_exchange(
        &crate::storage::models::AskConversationScope::Vault,
        None,
        "question",
        "legacy opaque derived answer (defense-only fixture)",
        &[],
        &[],
        &[],
        &[],
        "2026-08-06T12:00:00Z",
    )
    .unwrap();

    db.delete_org_state("org-1").unwrap();

    let counts: (i64, i64, i64, i64, i64) = {
        let conn = db.lock();
        (
            conn.query_row("SELECT COUNT(*) FROM org_state", [], |r| r.get(0))
                .unwrap(),
            conn.query_row("SELECT COUNT(*) FROM org_items", [], |r| r.get(0))
                .unwrap(),
            conn.query_row("SELECT COUNT(*) FROM org_chunks", [], |r| r.get(0))
                .unwrap(),
            conn.query_row("SELECT COUNT(*) FROM org_vec_chunks", [], |r| r.get(0))
                .unwrap(),
            conn.query_row("SELECT COUNT(*) FROM ask_conversations", [], |r| r.get(0))
                .unwrap(),
        )
    };
    assert_eq!(counts, (0, 0, 0, 0, 0));
}

#[test]
fn membership_withdrawal_purges_stale_history_even_with_empty_replica() {
    let db = mem_db();
    seed_org_state(&db, "org-1");
    db.persist_ask_exchange(
        &crate::storage::models::AskConversationScope::Vault,
        None,
        "question",
        "legacy opaque derived copy (defense-only fixture)",
        &[],
        &[],
        &[],
        &[],
        "2026-08-06T12:00:00Z",
    )
    .unwrap();

    assert!(db.delete_org_state("org-1").unwrap());
    let count: i64 = db
        .lock()
        .query_row("SELECT COUNT(*) FROM ask_conversations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 0);
}

/// The publish HTTP response can arrive after leave/removal committed. Its best-effort owner
/// refresh must re-check membership in the same SQLite transaction as the prospective plaintext
/// insert, otherwise it could recreate a membership-less replica after the consent purge.
#[test]
fn local_org_replica_commit_after_leave_cannot_restore_plaintext() {
    let db = mem_db();
    seed_org_state(&db, "org-1");
    let prepared =
        Db::prepare_org_item_index("Late publish", "t", "withdrawn plaintext", None).unwrap();

    db.delete_org_state("org-1").unwrap();
    let superseded_evicted = db
        .commit_local_org_replica(
            "it-late",
            "org-1",
            9,
            "anna",
            "Late publish",
            "withdrawn plaintext",
            "t",
            1,
            1,
            &sha32(38),
            None,
            Some("author-1"),
            &prepared,
            None,
        )
        .unwrap();
    assert!(!superseded_evicted);

    let counts: (i64, i64) = {
        let conn = db.lock();
        (
            conn.query_row("SELECT COUNT(*) FROM org_items", [], |r| r.get(0))
                .unwrap(),
            conn.query_row("SELECT COUNT(*) FROM org_chunks", [], |r| r.get(0))
                .unwrap(),
        )
    };
    assert_eq!(counts, (0, 0));
}

/// A share/republish local FTS refresh must not erase real vectors if feed sync already ingested
/// the same immutable item. The preserve decision and old-item tombstone live in one transaction.
#[test]
fn local_org_replica_refresh_preserves_feed_vectors_for_same_item() {
    let db = mem_db();
    seed_org_state(&db, "org-1");
    let emb = crate::embed::StubEmbedder;
    let sha = sha32(33);
    db.upsert_org_item(
        "it-live",
        "org-1",
        12,
        "anna",
        "Vectorized",
        "semantic body",
        "t",
        1,
        1,
        &sha,
        None,
        None,
        Some(&emb),
    )
    .unwrap();
    let vectors_before: i64 = db
        .lock()
        .query_row("SELECT COUNT(*) FROM org_vec_chunks", [], |r| r.get(0))
        .unwrap();
    assert!(vectors_before > 0);

    let fts_only = Db::prepare_org_item_index("Vectorized", "t", "semantic body", None).unwrap();
    let superseded_evicted = db
        .commit_local_org_replica(
            "it-live",
            "org-1",
            12,
            "anna",
            "Vectorized",
            "semantic body",
            "t",
            1,
            1,
            &sha,
            None,
            Some("author-1"),
            &fts_only,
            None,
        )
        .unwrap();
    assert!(!superseded_evicted);

    let vectors_after: i64 = db
        .lock()
        .query_row("SELECT COUNT(*) FROM org_vec_chunks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(vectors_after, vectors_before);
    assert_eq!(
        db.org_item_edit_ctx("it-live")
            .unwrap()
            .unwrap()
            .author_user_id
            .as_deref(),
        Some("author-1")
    );
}

/// The supersede result must come from the SAME transaction that tombstones the predecessor. A
/// pre-read can race a feed ingest and miss the visibility reduction, leaving open Ask plaintext
/// without an epoch bump/event even though the transaction already purged durable history.
#[test]
fn local_org_replica_commit_reports_transactional_supersede_once() {
    let db = mem_db();
    seed_org_state(&db, "org-1");
    let old_sha = sha32(39);
    db.upsert_org_item(
        "it-old",
        "org-1",
        10,
        "anna",
        "Old",
        "old body",
        "t",
        1,
        1,
        &old_sha,
        None,
        Some("author-1"),
        None,
    )
    .unwrap();
    db.persist_ask_exchange(
        &crate::storage::models::AskConversationScope::Vault,
        None,
        "question",
        "legacy opaque derived answer (defense-only fixture)",
        &[],
        &[],
        &[],
        &[],
        "2026-08-06T12:00:00Z",
    )
    .unwrap();

    let new_sha = sha32(40);
    let prepared = Db::prepare_org_item_index("New", "t2", "new body", None).unwrap();
    let first = db
        .commit_local_org_replica(
            "it-new",
            "org-1",
            11,
            "anna",
            "New",
            "new body",
            "t2",
            2,
            1,
            &new_sha,
            None,
            Some("author-1"),
            &prepared,
            Some("it-old"),
        )
        .unwrap();
    assert!(first, "the transaction evicted a live predecessor");
    assert!(db.org_replica_state("it-old").unwrap().unwrap().tombstoned);
    let ask_rows: i64 = db
        .lock()
        .query_row("SELECT COUNT(*) FROM ask_conversations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        ask_rows, 0,
        "supersede atomically purges derived Ask history"
    );

    let second = db
        .commit_local_org_replica(
            "it-new",
            "org-1",
            11,
            "anna",
            "New",
            "new body",
            "t2",
            2,
            1,
            &new_sha,
            None,
            Some("author-1"),
            &prepared,
            Some("it-old"),
        )
        .unwrap();
    assert!(!second, "an already tombstoned predecessor is a no-op");
}

/// Model-switch org rebuild is vector-only: purge removes old-space vectors but leaves the
/// canonical item, chunk/FTS rows and feed cursor intact; one-item CAS repair restores vectors.
#[test]
fn org_vector_reindex_primitives_preserve_replica_chunks_fts_and_cursor() {
    let db = mem_db();
    seed_org_state(&db, "org-1");
    let emb = crate::embed::StubEmbedder;
    db.upsert_org_item(
        "it-reindex",
        "org-1",
        15,
        "anna",
        "Reindex",
        "durable semantic rocket plan",
        "t",
        1,
        1,
        &sha32(34),
        None,
        Some("author-1"),
        Some(&emb),
    )
    .unwrap();
    db.set_org_last_seq("org-1", 15).unwrap();
    let chunks_before: i64 = db
        .lock()
        .query_row(
            "SELECT COUNT(*) FROM org_chunks WHERE item_id = 'it-reindex'",
            [],
            |r| r.get(0),
        )
        .unwrap();

    assert!(db.purge_all_org_vectors().unwrap() > 0);
    assert_eq!(db.org_last_seq_for("org-1").unwrap(), 15);
    assert_eq!(
        db.search_org_chunks_fts("rocket plan", 10).unwrap().len(),
        1
    );
    let chunks_after_purge: i64 = db
        .lock()
        .query_row(
            "SELECT COUNT(*) FROM org_chunks WHERE item_id = 'it-reindex'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(chunks_after_purge, chunks_before);

    let batch = db
        .next_missing_org_item_vector_batch(None)
        .unwrap()
        .expect("one live item batch");
    let vectors = Db::prepare_org_vector_blobs(&batch.texts, &emb).unwrap();
    assert!(db
        .commit_org_item_vectors_if_unchanged(&batch, &vectors)
        .unwrap());
    let vectors_after: i64 = db
        .lock()
        .query_row("SELECT COUNT(*) FROM org_vec_chunks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(vectors_after, chunks_before);
    assert_eq!(db.org_last_seq_for("org-1").unwrap(), 15);
}

/// Global rebuild/startup-repair readers must never materialize plaintext from an obsolete stable
/// document revision. The attacker row sorts first and has the larger revision, while the relay
/// witness deliberately marks only the lower revision current.
#[test]
fn org_vector_rebuild_readers_skip_live_noncurrent_revision() {
    let db = mem_db();
    seed_org_state(&db, "org-1");
    let conn = db.lock();
    for (item_id, rev, is_current) in [
        ("a-attacker-high-rev", 999_i64, 0_i64),
        ("z-authoritative-current", 2_i64, 1_i64),
    ] {
        conn.execute(
            "INSERT INTO org_items
               (item_id, org_id, seq, author_hint, title, markdown, created_at, rev, generation,
                content_sha256, tombstoned, doc_id, is_current)
             VALUES (?1, 'org-1', ?2, 'member', ?1, 'vector repair secret', 't', ?2, 1,
                     ?3, 0, 'doc-1', ?4)",
            rusqlite::params![
                item_id,
                rev,
                sha32(if is_current == 1 { 2 } else { 9 }),
                is_current
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO org_chunks (item_id, chunk_idx, text)
             VALUES (?1, 0, 'vector repair secret')",
            rusqlite::params![item_id],
        )
        .unwrap();
    }
    drop(conn);

    assert_eq!(
        db.next_org_item_vector_batch(None)
            .unwrap()
            .expect("current rebuild batch")
            .item_id,
        "z-authoritative-current"
    );
    assert_eq!(
        db.next_missing_org_item_vector_batch(None)
            .unwrap()
            .expect("current repair batch")
            .item_id,
        "z-authoritative-current"
    );
    assert!(db
        .org_item_vector_batch("a-attacker-high-rev")
        .unwrap()
        .is_none());
}

/// Embedding runs outside the DB transaction. If a revision is current at read time but is demoted
/// before the vector batch commits, the transaction must reject that stale batch and persist no
/// searchable vector rows.
#[test]
fn org_vector_batch_commit_rejects_revision_demoted_after_read() {
    let db = mem_db();
    seed_org_state(&db, STABLE_ORG_ID);
    let emb = crate::embed::StubEmbedder;
    db.upsert_org_item(
        "current-before-embed",
        STABLE_ORG_ID,
        2,
        "member",
        "Current",
        "demotion race content",
        "t",
        2,
        1,
        &sha32(2),
        None,
        Some("author-1"),
        None,
    )
    .unwrap();
    db.set_org_item_document_metadata(
        "current-before-embed",
        Some(STABLE_DOC_RACE_ID),
        "view",
        Some("author-1"),
    )
    .unwrap();
    db.repair_org_reconcile_metadata(
        "current-before-embed",
        STABLE_ORG_ID,
        1,
        Some(STABLE_DOC_RACE_ID),
        "view",
        Some("author-1"),
        true,
    )
    .unwrap();

    let batch = db
        .org_item_vector_batch("current-before-embed")
        .unwrap()
        .expect("current batch before embedding");
    let vectors = Db::prepare_org_vector_blobs(&batch.texts, &emb).unwrap();

    db.lock()
        .execute(
            "UPDATE org_items SET is_current = 0 WHERE item_id = 'current-before-embed'",
            [],
        )
        .unwrap();
    assert!(!db
        .commit_org_item_vectors_if_unchanged(&batch, &vectors)
        .unwrap());
    let persisted: i64 = db
        .lock()
        .query_row("SELECT COUNT(*) FROM org_vec_chunks", [], |row| row.get(0))
        .unwrap();
    assert_eq!(persisted, 0, "demoted revision must receive no vectors");
}

/// CAS must compare content as well as rowids: SQLite can reuse deleted max INTEGER PRIMARY KEY
/// values after a clean replace, so an id-only check could attach old-text vectors to new text.
#[test]
fn org_vector_cas_rejects_reused_chunk_rowids_with_changed_text() {
    let db = mem_db();
    seed_org_state(&db, "org-1");
    let emb = crate::embed::StubEmbedder;
    db.upsert_org_item(
        "it-cas",
        "org-1",
        20,
        "anna",
        "CAS",
        "old rocket wording",
        "t",
        1,
        1,
        &sha32(35),
        None,
        Some("author-1"),
        Some(&emb),
    )
    .unwrap();
    db.purge_all_org_vectors().unwrap();
    let stale_batch = db
        .next_missing_org_item_vector_batch(None)
        .unwrap()
        .expect("stale snapshot");
    let stale_vectors = Db::prepare_org_vector_blobs(&stale_batch.texts, &emb).unwrap();

    // Clean replace with the same item/chunk count. With this lone max-row item SQLite normally
    // reuses the same rowids; the regression asserts that exact precondition explicitly.
    db.upsert_org_item(
        "it-cas",
        "org-1",
        20,
        "anna",
        "CAS",
        "new submarine wording",
        "t",
        1,
        1,
        &sha32(36),
        None,
        Some("author-1"),
        None,
    )
    .unwrap();
    let current_batch = db
        .org_item_vector_batch("it-cas")
        .unwrap()
        .expect("current batch");
    assert_eq!(current_batch.chunk_ids, stale_batch.chunk_ids);
    assert_ne!(current_batch.texts, stale_batch.texts);

    assert!(!db
        .commit_org_item_vectors_if_unchanged(&stale_batch, &stale_vectors)
        .unwrap());
    let vectors_after: i64 = db
        .lock()
        .query_row("SELECT COUNT(*) FROM org_vec_chunks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(vectors_after, 0, "stale old-text vectors must not commit");
    assert_eq!(
        db.search_org_chunks_fts("submarine", 10).unwrap().len(),
        1,
        "new FTS text remains canonical"
    );

    // Legacy NULL hashes cannot use `None == None` as a content token. Snapshot the current
    // NULL-hash row, clean-replace it again with reused ids + changed text + NULL hash, and prove
    // the fallback ordered `(id,text)` comparison also rejects the stale vectors.
    db.lock()
        .execute(
            "UPDATE org_items SET content_sha256 = NULL WHERE item_id = 'it-cas'",
            [],
        )
        .unwrap();
    let legacy_stale_batch = db
        .next_missing_org_item_vector_batch(None)
        .unwrap()
        .expect("legacy stale snapshot");
    let legacy_stale_vectors =
        Db::prepare_org_vector_blobs(&legacy_stale_batch.texts, &emb).unwrap();
    db.upsert_org_item(
        "it-cas",
        "org-1",
        20,
        "anna",
        "CAS",
        "third asteroid wording",
        "t",
        1,
        1,
        &sha32(37),
        None,
        Some("author-1"),
        None,
    )
    .unwrap();
    db.lock()
        .execute(
            "UPDATE org_items SET content_sha256 = NULL WHERE item_id = 'it-cas'",
            [],
        )
        .unwrap();
    let legacy_current_batch = db
        .org_item_vector_batch("it-cas")
        .unwrap()
        .expect("legacy current batch");
    assert_eq!(legacy_current_batch.chunk_ids, legacy_stale_batch.chunk_ids);
    assert_ne!(legacy_current_batch.texts, legacy_stale_batch.texts);
    assert!(!db
        .commit_org_item_vectors_if_unchanged(&legacy_stale_batch, &legacy_stale_vectors,)
        .unwrap());
    let legacy_vectors_after: i64 = db
        .lock()
        .query_row("SELECT COUNT(*) FROM org_vec_chunks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(legacy_vectors_after, 0);
}

/// Tombstone eviction: a tombstoned item disappears from BOTH retrieval legs + the viewer, and
/// its chunks/vectors/FTS rows are purged. Re-tombstoning is idempotent.
#[test]
fn org_tombstone_evicts_from_retrieval_and_viewer() {
    let db = mem_db();
    seed_org_state(&db, "org-1");
    let emb = crate::embed::StubEmbedder;
    db.upsert_org_item(
        "it-t",
        "org-1",
        1,
        "carol",
        "Secret plan",
        "the classified atlas acquisition timeline",
        "2026-07-10T09:00:00Z",
        1,
        1,
        &sha32(3),
        None,
        None,
        Some(&emb),
    )
    .unwrap();
    db.persist_ask_exchange(
        &crate::storage::models::AskConversationScope::Vault,
        None,
        "question",
        "legacy opaque derived answer (defense-only fixture)",
        &[],
        &[],
        &[],
        &[],
        "2026-08-06T12:00:00Z",
    )
    .unwrap();
    assert_eq!(
        db.search_org_chunks_fts("atlas acquisition", 10)
            .unwrap()
            .len(),
        1
    );

    db.tombstone_org_item("it-t").unwrap();

    assert!(
        db.search_org_chunks_fts("atlas acquisition", 10)
            .unwrap()
            .is_empty(),
        "tombstoned item must vanish from FTS"
    );
    let qv = emb
        .embed_query(&["atlas acquisition".to_string()])
        .unwrap()
        .remove(0);
    assert!(
        db.search_org_chunks_knn(&qv, 10, 0.0).unwrap().is_empty(),
        "tombstoned item must vanish from KNN"
    );
    assert!(
        db.get_org_item("it-t").unwrap().is_none(),
        "the viewer must not return a tombstoned item"
    );
    // Its chunks/vectors are gone.
    let n: i64 = db
        .lock()
        .query_row(
            "SELECT COUNT(*) FROM org_chunks WHERE item_id='it-t'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 0, "org_chunks purged on tombstone");
    let ask_count: i64 = db
        .lock()
        .query_row("SELECT COUNT(*) FROM ask_conversations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(ask_count, 0, "org tombstone must purge global-derived Ask");
    // Idempotent re-tombstone.
    db.tombstone_org_item("it-t").unwrap();
}

/// ITEM 3 (RED-before-GREEN, leak): an org eviction MUST take the item's `note_attachments` image
/// BLOBs with it, on EVERY path — including the feed tombstone.
///
/// Pre-fix these survived forever: `note_attachments.org_item_id` carries
/// `REFERENCES org_items(item_id) ON DELETE CASCADE`, but an eviction is an UPDATE
/// (`tombstoned = 1`) — the header row deliberately survives as a tombstone so an append-only
/// re-pull stays idempotent — so the CASCADE never fired and a withdrawn colleague's pictures stayed
/// as plaintext BLOBs in SQLite. Also asserts the primitive's return value: `true` only when a LIVE
/// row was actually evicted, so callers can count real convergence work.
#[test]
fn org_feed_attachment_collision_rolls_back_item_cursor_and_completeness_witness() {
    let db = mem_db();
    let org_id = "33333333-3333-4333-8333-333333333333";
    seed_org_state(&db, org_id);
    db.upsert_org_item(
        "other",
        org_id,
        1,
        "owner",
        "Other",
        "body",
        "t",
        1,
        1,
        &sha32(1),
        None,
        Some("owner"),
        None,
    )
    .unwrap();
    db.replace_org_item_attachment_bundle(
        "other",
        &[crate::storage::IncomingAttachment {
            id: "collision".into(),
            mime_type: "image/png".into(),
            extension: "png".into(),
            width: 1,
            height: 1,
            sha256: [2; 32],
            data: vec![2],
        }],
    )
    .unwrap();
    let prepared = Db::prepare_org_item_index("Target", "t", "private target", None).unwrap();
    let attachment = crate::storage::IncomingAttachment {
        id: "collision".into(),
        mime_type: "image/png".into(),
        extension: "png".into(),
        width: 1,
        height: 1,
        sha256: [3; 32],
        data: vec![3],
    };
    let failed = db.commit_org_feed_item_with_metadata_and_attachments(
        "target",
        org_id,
        2,
        "owner",
        "Target",
        "private target",
        "t",
        1,
        1,
        &sha32(4),
        None,
        Some("owner"),
        &prepared,
        Some("11111111-1111-4111-8111-111111111111"),
        "view",
        Some("owner"),
        true,
        &[attachment],
    );
    assert!(failed.is_err());
    let conn = db.lock();
    assert_eq!(
        conn.query_row(
            "SELECT last_seq FROM org_state WHERE org_id=?1",
            [org_id],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM org_items WHERE item_id='target'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    drop(conn);
    db.lock()
        .execute("DELETE FROM note_attachments WHERE id='collision'", [])
        .unwrap();
    let repaired = crate::storage::IncomingAttachment {
        id: "target-image".into(),
        mime_type: "image/png".into(),
        extension: "png".into(),
        width: 1,
        height: 1,
        sha256: [3; 32],
        data: vec![3],
    };
    assert!(
        db.commit_org_feed_item_with_metadata_and_attachments(
            "target",
            org_id,
            2,
            "owner",
            "Target",
            "private target",
            "t",
            1,
            1,
            &sha32(4),
            None,
            Some("owner"),
            &prepared,
            Some("11111111-1111-4111-8111-111111111111"),
            "view",
            Some("owner"),
            true,
            &[repaired],
        )
        .unwrap()
        .changed
    );
    let conn = db.lock();
    assert!(conn
        .query_row(
            "SELECT projection_sha256=?2 FROM org_items WHERE item_id=?1",
            rusqlite::params!["target", sha32(4)],
            |r| r.get::<_, bool>(0)
        )
        .unwrap());
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM note_attachments WHERE org_item_id='target'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
}

#[test]
fn org_feed_atomically_closes_projection_pending_and_preserves_newer_source_dirty() {
    let db = mem_db();
    let org_id = "11111111-1111-4111-8111-111111111111";
    let doc_id = "22222222-2222-4222-8222-222222222222";
    seed_org_state(&db, org_id);
    db.insert_org_share(
        "share-a",
        org_id,
        None,
        Some("source-b"),
        "note",
        Some("B"),
        2,
        1,
        &sha32(4),
        "2026-08-13T00:00:00Z",
    )
    .unwrap();
    db.set_org_share_document_metadata("share-a", doc_id, "view")
        .unwrap();
    {
        let conn = db.lock();
        conn.execute(
            "UPDATE org_shares SET state='failed',last_error='projection_pending',
              item_id='item-a',dispatch_id='dispatch-a',expected_actor_user_id='actor',
              expected_owner_user_id='owner',republish_dirty=1 WHERE id='share-a'",
            [],
        )
        .unwrap();
    }
    let prepared = Db::prepare_org_item_index("A", "t", "remote A", None).unwrap();
    let outcome = db
        .commit_org_feed_item_with_metadata_and_attachments(
            "item-a",
            org_id,
            2,
            "actor",
            "A",
            "remote A",
            "t",
            2,
            1,
            &sha32(4),
            None,
            Some("actor"),
            &prepared,
            Some(doc_id),
            "view",
            Some("owner"),
            true,
            &[],
        )
        .unwrap();
    assert!(outcome.changed);
    let row = db.get_org_share("share-a").unwrap().unwrap();
    assert_eq!(row.state, "uploaded");
    assert_eq!(row.last_error, None);
    assert_eq!(
        row.republish_dirty, 1,
        "newer source B remains queued after A closes"
    );
    assert_eq!(
        db.org_replica_state("item-a")
            .unwrap()
            .unwrap()
            .projection_sha256,
        Some(sha32(4))
    );
}

#[test]
fn org_feed_tombstone_closes_projection_pending_journals_without_resurrection() {
    let db = mem_db();
    let org_id = "11111111-1111-4111-8111-111111111111";
    seed_org_state(&db, org_id);
    for (id, source) in [("anchored", Some("source")), ("journal", None)] {
        db.insert_org_share(
            id,
            org_id,
            None,
            source,
            "note",
            Some("T"),
            2,
            1,
            &sha32(5),
            "2026-08-13T00:00:00Z",
        )
        .unwrap();
        db.lock().execute(
            "UPDATE org_shares SET state='failed',last_error='projection_pending',item_id='item-a'
              WHERE id=?1", [id],
        ).unwrap();
    }
    assert!(db.commit_org_feed_tombstone(org_id, "item-a", 1).unwrap());
    assert_eq!(
        db.get_org_share("anchored")
            .unwrap()
            .unwrap()
            .last_error
            .as_deref(),
        Some("org_edit_conflict")
    );
    assert!(db.get_org_share("journal").unwrap().is_none());
}

#[test]
fn confirmed_document_delete_terminalizes_sibling_anchors_and_plaintext_in_one_tx() {
    let db = mem_db();
    let org_id = "11111111-1111-4111-8111-111111111111";
    let doc_id = "22222222-2222-4222-8222-222222222222";
    seed_org_state(&db, org_id);
    for (index, item) in ["item-1", "item-2"].into_iter().enumerate() {
        db.insert_org_share(
            item,
            org_id,
            None,
            Some(item),
            "note",
            Some("T"),
            (index + 1) as u32,
            1,
            &sha32(index as u8),
            "2026-08-13T00:00:00Z",
        )
        .unwrap();
        db.set_org_share_uploaded(item, item, "2026-08-13T00:00:01Z")
            .unwrap();
        db.set_org_share_document_metadata(item, doc_id, "view")
            .unwrap();
        db.upsert_org_item(
            item,
            org_id,
            (index + 1) as u64,
            "owner",
            "T",
            "secret",
            "t",
            (index + 1) as u32,
            1,
            &sha32(index as u8),
            None,
            Some("owner"),
            None,
        )
        .unwrap();
        db.set_org_item_document_metadata(item, Some(doc_id), "view", Some("owner"))
            .unwrap();
    }
    let endpoint = format!("{org_id}:{doc_id}");
    db.lock()
        .execute(
            "INSERT INTO links
           (src_kind,src_id,dst_kind,dst_id,edge_type,created_at)
         VALUES('org',?1,'note','local-note','manual',1)",
            [&endpoint],
        )
        .unwrap();
    assert!(db
        .terminalize_and_evict_org_document(org_id, doc_id, "2026-08-13T00:00:02Z")
        .unwrap());
    for id in ["item-1", "item-2"] {
        assert_eq!(db.get_org_share(id).unwrap().unwrap().state, "revoked");
        let item = db.get_org_item(id).unwrap();
        assert!(item.is_none(), "tombstoned plaintext is not readable");
    }
    assert_eq!(
        db.lock()
            .query_row(
                "SELECT COUNT(*) FROM links WHERE src_kind='org' AND src_id=?1",
                [&endpoint],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
}

#[test]
fn evicting_an_org_item_purges_its_attachment_blobs_on_every_path() {
    let attachment_count = |db: &Db, item_id: &str| -> i64 {
        db.lock()
            .query_row(
                "SELECT COUNT(*) FROM note_attachments WHERE org_item_id = ?1",
                rusqlite::params![item_id],
                |r| r.get(0),
            )
            .unwrap()
    };
    let seed_item_with_image = |db: &Db, item_id: &str, seq: u64| {
        db.upsert_org_item(
            item_id,
            "org-1",
            seq,
            "carol",
            "Illustrated plan",
            "the classified atlas acquisition timeline",
            "2026-07-10T09:00:00Z",
            1,
            1,
            &sha32(4),
            None,
            None,
            Some(&crate::embed::StubEmbedder),
        )
        .unwrap();
        db.replace_org_item_attachment_bundle(
            item_id,
            &[crate::storage::IncomingAttachment {
                id: uuid::Uuid::new_v4().to_string(),
                mime_type: "image/png".into(),
                extension: "png".into(),
                width: 4,
                height: 4,
                sha256: [5u8; 32],
                data: vec![9u8; 16],
            }],
        )
        .unwrap();
        assert_eq!(attachment_count(db, item_id), 1);
    };

    // Path A — the direct eviction primitive.
    let db = mem_db();
    seed_org_state(&db, "org-1");
    seed_item_with_image(&db, "it-img", 1);
    assert!(
        db.evict_org_item("it-img").unwrap(),
        "evicting a LIVE item reports that it did real work"
    );
    assert_eq!(
        attachment_count(&db, "it-img"),
        0,
        "withdrawn colleague images must not survive the eviction"
    );
    assert!(
        !db.evict_org_item("it-img").unwrap(),
        "a second eviction is an idempotent no-op and reports no work"
    );
    assert!(
        !db.evict_org_item("it-never-existed").unwrap(),
        "evicting an unknown id is a no-op, not an error"
    );

    // Path B — the FEED tombstone (the arm a member's background sync actually applies).
    let db = mem_db();
    seed_org_state(&db, "org-1");
    seed_item_with_image(&db, "it-feed", 1);
    assert!(db.commit_org_feed_tombstone("org-1", "it-feed", 2).unwrap());
    assert_eq!(
        attachment_count(&db, "it-feed"),
        0,
        "the feed tombstone path routes through the SAME eviction primitive"
    );
}

/// The anti-entropy reconcile cursor is a SECOND, wholly independent cursor: advancing/completing it
/// must never touch the live `last_seq` pull cursor (rewinding that would silently re-ingest, or —
/// worse — skip, live feed records). A completed pass rewinds only the SLOW cursor.
#[test]
fn reconcile_cursor_is_independent_of_the_live_feed_cursor() {
    let db = mem_db();
    seed_org_state(&db, "org-1");
    db.set_org_last_seq("org-1", 42).unwrap();

    assert_eq!(db.org_reconcile_seq_for("org-1").unwrap(), 0);
    assert!(db.org_reconcile_pass_at("org-1").unwrap().is_none());

    db.set_org_reconcile_seq("org-1", 7).unwrap();
    assert_eq!(db.org_reconcile_seq_for("org-1").unwrap(), 7);
    assert_eq!(
        db.org_last_seq_for("org-1").unwrap(),
        42,
        "advancing the slow cursor never writes last_seq"
    );

    db.complete_org_reconcile_pass("org-1", "2026-07-26T00:00:00Z")
        .unwrap();
    assert_eq!(
        db.org_reconcile_seq_for("org-1").unwrap(),
        0,
        "a completed pass rewinds the SLOW cursor to re-observe the whole feed"
    );
    assert_eq!(
        db.org_reconcile_pass_at("org-1").unwrap().as_deref(),
        Some("2026-07-26T00:00:00Z")
    );
    assert_eq!(
        db.org_last_seq_for("org-1").unwrap(),
        42,
        "completing a pass never rewinds the LIVE cursor"
    );

    // An unknown org is a no-op on both writers, and reads as a fresh pass.
    db.set_org_reconcile_seq("org-none", 9).unwrap();
    db.complete_org_reconcile_pass("org-none", "2026-07-26T00:00:00Z")
        .unwrap();
    assert_eq!(db.org_reconcile_seq_for("org-none").unwrap(), 0);
    assert!(db.org_reconcile_pass_at("org-none").unwrap().is_none());
}

/// A reconcile-found live record is committed WITHOUT claiming a feed sequence — the sweep walks
/// seqs that are usually already behind the live cursor, so `commit_org_feed_item`'s
/// `seq > last_seq` claim would silently drop the write. The reconcile commit keeps the two
/// invariants that matter: live membership is still required, and a tombstone is still permanent.
#[test]
fn reconcile_commit_writes_below_the_live_cursor_but_never_resurrects_a_tombstone() {
    let db = mem_db();
    seed_org_state(&db, "org-1");
    db.set_org_last_seq("org-1", 42).unwrap();
    let prepared = Db::prepare_org_item_index("Recovered", "t", "recovered body", None).unwrap();

    // Below the live cursor, reconcile still installs the complete projection and leaves the live
    // cursor exactly where it was. Feed's legacy-incomplete repair behavior has its own oracle.
    assert!(db
        .commit_org_reconcile_item(
            "it-below",
            "org-1",
            5,
            "anna",
            "Recovered",
            "recovered body",
            "t",
            1,
            1,
            &sha32(6),
            None,
            None,
            &prepared,
        )
        .unwrap());
    assert!(db.get_org_item("it-below").unwrap().is_some());
    assert_eq!(db.org_last_seq_for("org-1").unwrap(), 42);

    // A tombstone is permanent: the sweep must never resurrect withdrawn plaintext.
    assert!(db.evict_org_item("it-below").unwrap());
    assert!(!db
        .commit_org_reconcile_item(
            "it-below",
            "org-1",
            5,
            "anna",
            "Recovered",
            "recovered body",
            "t",
            2,
            1,
            &sha32(6),
            None,
            None,
            &prepared,
        )
        .unwrap());
    assert!(db.get_org_item("it-below").unwrap().is_none());

    // Withdrawn membership can never be followed by a plaintext replica resurrection.
    assert!(!db
        .commit_org_reconcile_item(
            "it-gone-org",
            "org-left",
            5,
            "anna",
            "Recovered",
            "recovered body",
            "t",
            1,
            1,
            &sha32(6),
            None,
            None,
            &prepared,
        )
        .unwrap());
    assert!(db.get_org_item("it-gone-org").unwrap().is_none());
}

/// The reconcile sweep decides "already converged" from the feed's `content_sha256` alone — no blob
/// fetch. This is the read that backs it: what the device currently holds for one item id.
#[test]
fn org_replica_state_reports_what_this_device_holds() {
    let db = mem_db();
    seed_org_state(&db, "org-1");
    assert!(db.org_replica_state("it-unknown").unwrap().is_none());

    let sha = sha32(8);
    db.upsert_org_item(
        "it-held",
        "org-1",
        3,
        "carol",
        "Held",
        "held body",
        "2026-07-10T09:00:00Z",
        1,
        1,
        &sha,
        None,
        None,
        None,
    )
    .unwrap();
    let held = db.org_replica_state("it-held").unwrap().unwrap();
    assert!(!held.tombstoned);
    assert_eq!(held.content_sha256.as_deref(), Some(sha.as_slice()));

    db.evict_org_item("it-held").unwrap();
    let gone = db.org_replica_state("it-held").unwrap().unwrap();
    assert!(
        gone.tombstoned,
        "an evicted item is still reported — as a permanent tombstone"
    );
}

/// LEAVE PURGE (RED-before-GREEN, leak/consent): `purge_org_replica` drops the WHOLE decrypted
/// replica of an org — every `org_items` header, its `org_chunks`/`org_vec_chunks`, and the
/// `fts_org_chunks` tokens — so `org_leave` leaves NO searchable copy of colleagues' content.
/// Pre-fix `org_leave` deleted only `org_state`, so the replica lingered forever and `org_search`
/// could still return it. Idempotent; scoped to the named org (a second org survives).
#[test]
fn purge_org_replica_drops_the_whole_decrypted_replica() {
    let db = mem_db();
    seed_org_state(&db, "org-1");
    seed_org_state(&db, "org-2");
    let emb = crate::embed::StubEmbedder;
    db.upsert_org_item(
        "it-a",
        "org-1",
        1,
        "anna",
        "Roadmap",
        "the falcon rollout plan alpha",
        "t",
        1,
        1,
        &sha32(6),
        None,
        None,
        Some(&emb),
    )
    .unwrap();
    db.upsert_org_item(
        "it-b",
        "org-1",
        2,
        "bob",
        "Budget",
        "the falcon rollout budget beta",
        "t",
        1,
        1,
        &sha32(7),
        None,
        None,
        Some(&emb),
    )
    .unwrap();
    // A DIFFERENT org's item must SURVIVE the scoped purge.
    db.upsert_org_item(
        "it-other",
        "org-2",
        1,
        "carol",
        "Other",
        "unrelated other-org content",
        "t",
        1,
        1,
        &sha32(8),
        None,
        None,
        Some(&emb),
    )
    .unwrap();

    // Precondition: org-1 items are retrievable.
    assert_eq!(
        db.search_org_chunks_fts("falcon rollout", 10)
            .unwrap()
            .len(),
        2
    );

    db.purge_org_replica("org-1").unwrap();

    // FTS + KNN + the viewer + the raw rows all show org-1 gone.
    assert!(
        db.search_org_chunks_fts("falcon rollout", 10)
            .unwrap()
            .is_empty(),
        "org-1 replica must vanish from FTS after leave-purge"
    );
    // KNN: no org-1 item survives (the surviving org-2 item may still appear — StubEmbedder
    // vectors are text-independent — so assert org-1's ids are GONE, not that KNN is empty).
    let qv = emb
        .embed_query(&["falcon rollout".to_string()])
        .unwrap()
        .remove(0);
    let knn = db.search_org_chunks_knn(&qv, 10, 0.0).unwrap();
    assert!(
        knn.iter()
            .all(|h| h.item_id != "it-a" && h.item_id != "it-b"),
        "no org-1 item may survive in KNN after leave-purge: {:?}",
        knn.iter().map(|h| &h.item_id).collect::<Vec<_>>()
    );
    assert!(db.get_org_item("it-a").unwrap().is_none());
    assert!(db.get_org_item("it-b").unwrap().is_none());
    let n_items: i64 = db
        .lock()
        .query_row(
            "SELECT COUNT(*) FROM org_items WHERE org_id='org-1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n_items, 0, "org_items header rows for org-1 are gone");
    let n_chunks: i64 = db
        .lock()
        .query_row(
            "SELECT COUNT(*) FROM org_chunks WHERE item_id IN ('it-a','it-b')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n_chunks, 0, "org_chunks for org-1 items are gone");

    // The OTHER org is untouched, and the purge is idempotent.
    assert!(
        db.get_org_item("it-other").unwrap().is_some(),
        "org-2 survives the scoped purge"
    );
    db.purge_org_replica("org-1").unwrap(); // idempotent no-op
}

/// Re-pull idempotency: upserting the SAME item id twice REPLACES (never duplicates) its chunks —
/// a clean re-index, and a bumped rev overwrites the body.
#[test]
fn org_upsert_is_idempotent_and_replaces_on_rev_bump() {
    let db = mem_db();
    seed_org_state(&db, "org-1");
    let emb = crate::embed::StubEmbedder;
    db.upsert_org_item(
        "it-r",
        "org-1",
        1,
        "dan",
        "V1",
        "first version body alpha",
        "t",
        1,
        1,
        &sha32(4),
        None,
        None,
        Some(&emb),
    )
    .unwrap();
    db.upsert_org_item(
        "it-r",
        "org-1",
        2,
        "dan",
        "V2",
        "second version body bravo",
        "t",
        2,
        1,
        &sha32(5),
        None,
        None,
        Some(&emb),
    )
    .unwrap();
    // Only the v2 body is chunked (no duplicate/stale chunks).
    assert!(
        db.search_org_chunks_fts("bravo", 10).unwrap().len() == 1,
        "the re-index surfaces the new body"
    );
    assert!(
        db.search_org_chunks_fts("alpha", 10).unwrap().is_empty(),
        "the stale v1 body is fully replaced (no orphan chunks)"
    );
    let detail = db.get_org_item("it-r").unwrap().unwrap();
    assert_eq!(detail.rev, 2);
    assert_eq!(detail.title, "V2");
}

/// ROOT-CAUSE FIX REGRESSION (2026-07-15, Bug A): `upsert_org_item`'s `author_user_id` param is
/// now written directly in BOTH the INSERT and the `ON CONFLICT` clause, via
/// `COALESCE(excluded.author_user_id, org_items.author_user_id)`. RED-before-GREEN against the
/// OLD mechanism (a separate `set_org_item_author` follow-up call, never threaded through
/// `upsert_org_item` itself): a caller who upserts WITH a known author (e.g. feed-ingest, or a
/// share-time local-replica upsert where the caller IS the author) must have that author survive
/// a LATER upsert of the SAME item that does NOT know the author (`None` — e.g. a light re-upsert
/// from an older call site, or a partial feed page). Pre-fix there was no such param at all, so
/// EVERY upsert left `author_user_id` NULL until a separate, easy-to-forget follow-up call — this
/// test pins the COALESCE behavior that makes the row correct from birth and un-clobberable.
#[test]
fn upsert_org_item_author_survives_a_later_upsert_that_passes_none() {
    let db = mem_db();
    seed_org_state(&db, "org-1");
    let emb = crate::embed::StubEmbedder;

    // First upsert KNOWS the author (mirrors feed-ingest / share-time local-replica upsert).
    db.upsert_org_item(
        "it-auth",
        "org-1",
        1,
        "anna",
        "T1",
        "first body",
        "2026-07-10T09:00:00Z",
        1,
        1,
        &sha32(41),
        None,
        Some("author-uuid-1"),
        Some(&emb),
    )
    .unwrap();
    let ctx0 = db.org_item_edit_ctx("it-auth").unwrap().unwrap();
    assert_eq!(ctx0.author_user_id.as_deref(), Some("author-uuid-1"));

    // A LATER re-upsert of the SAME item (e.g. a partial feed page, or a legacy call site) that
    // does NOT know the author (`None`) must NEVER clobber the already-known author back to NULL.
    db.upsert_org_item(
        "it-auth",
        "org-1",
        2,
        "anna",
        "T2",
        "second body",
        "2026-07-10T09:00:00Z",
        2,
        1,
        &sha32(42),
        None,
        None,
        Some(&emb),
    )
    .unwrap();
    let ctx1 = db.org_item_edit_ctx("it-auth").unwrap().unwrap();
    assert_eq!(
        ctx1.author_user_id.as_deref(),
        Some("author-uuid-1"),
        "a None re-upsert must NOT clobber an already-known author back to NULL"
    );
    assert_eq!(
        ctx1.rev, 2,
        "the re-upsert DID update rev — only the author is COALESCE-protected"
    );

    // A THIRD upsert that DOES supply a (possibly fresher) author overwrites it normally — the
    // COALESCE only protects against clobbering with NULL, it never freezes a stale value.
    db.upsert_org_item(
        "it-auth",
        "org-1",
        3,
        "anna",
        "T3",
        "third body",
        "2026-07-10T09:00:00Z",
        3,
        1,
        &sha32(43),
        None,
        Some("author-uuid-2"),
        Some(&emb),
    )
    .unwrap();
    let ctx2 = db.org_item_edit_ctx("it-auth").unwrap().unwrap();
    assert_eq!(
        ctx2.author_user_id.as_deref(),
        Some("author-uuid-2"),
        "a Some(...) re-upsert DOES overwrite the author with the fresher value"
    );
}

/// PER-INSTANCE ORG TOGGLE (RED-before-GREEN, the user's hard mandate: a disabled org's context
/// must NEVER leak through). Two orgs, BOTH with matching content; org-1 disabled, org-2 stays
/// enabled. The disabled org's item must be COMPLETELY absent from both retrieval legs — not
/// merely ranked lower — while the enabled org's item still surfaces normally. Proves the SQL
/// filter is a real exclusion, not a soft demotion.
#[test]
fn disabled_org_is_excluded_from_both_retrieval_legs_while_enabled_org_still_surfaces() {
    let db = mem_db();
    seed_org_state(&db, "org-1");
    seed_org_state(&db, "org-2");
    let emb = crate::embed::StubEmbedder;
    db.upsert_org_item(
        "it-disabled",
        "org-1",
        1,
        "anna",
        "Disabled org note",
        "the quantum ledger migration plan",
        "t",
        1,
        1,
        &sha32(11),
        None,
        None,
        Some(&emb),
    )
    .unwrap();
    db.upsert_org_item(
        "it-enabled",
        "org-2",
        1,
        "bob",
        "Enabled org note",
        "the quantum ledger migration rollout",
        "t",
        1,
        1,
        &sha32(12),
        None,
        None,
        Some(&emb),
    )
    .unwrap();

    // Precondition: BOTH are reachable while both orgs are enabled.
    let fts_before = db
        .search_org_chunks_fts("quantum ledger migration", 10)
        .unwrap();
    assert_eq!(
        fts_before.len(),
        2,
        "both items reachable before any org is disabled"
    );

    db.set_org_context_enabled("org-1", false).unwrap();

    let fts_after = db
        .search_org_chunks_fts("quantum ledger migration", 10)
        .unwrap();
    assert!(
        fts_after.iter().all(|h| h.item_id != "it-disabled"),
        "the disabled org's item must be ABSENT from FTS, not just re-ranked: {:?}",
        fts_after.iter().map(|h| &h.item_id).collect::<Vec<_>>()
    );
    assert!(
        fts_after.iter().any(|h| h.item_id == "it-enabled"),
        "the STILL-enabled org's item must keep surfacing normally"
    );

    let qv = emb
        .embed_query(&["quantum ledger migration".to_string()])
        .unwrap()
        .remove(0);
    let knn_after = db.search_org_chunks_knn(&qv, 10, 0.0).unwrap();
    assert!(
        knn_after.iter().all(|h| h.item_id != "it-disabled"),
        "the disabled org's item must be ABSENT from KNN too: {:?}",
        knn_after.iter().map(|h| &h.item_id).collect::<Vec<_>>()
    );
    assert!(knn_after.iter().any(|h| h.item_id == "it-enabled"));

    // Re-enabling is instant — no re-sync/re-ingest needed, the replica was never touched.
    db.set_org_context_enabled("org-1", true).unwrap();
    let fts_reenabled = db
        .search_org_chunks_fts("quantum ledger migration", 10)
        .unwrap();
    assert_eq!(
        fts_reenabled.len(),
        2,
        "re-enabling instantly restores visibility, no re-sync"
    );
}

/// PER-INSTANCE ORG TOGGLE (RED-before-GREEN): `count_org_items` must agree with the actual
/// gated content — `search_org_chunks_knn`/`_fts`/`get_org_item`/`list_org_items_inner` all
/// exclude a disabled org's items, so the count (used as `OrgStatus.received_count`) must too.
/// Before the fix this counted raw `org_items` rows with no `context_enabled` join, so a
/// disabled org's count stayed stale/inflated instead of dropping to zero like every sibling read.
#[test]
fn count_org_items_excludes_a_disabled_org_and_restores_on_reenable() {
    let db = mem_db();
    seed_org_state(&db, "org-1");
    let emb = crate::embed::StubEmbedder;
    db.upsert_org_item(
        "it-1",
        "org-1",
        1,
        "anna",
        "Kickoff",
        "roadmap body",
        "t",
        1,
        1,
        &sha32(21),
        None,
        None,
        Some(&emb),
    )
    .unwrap();
    db.upsert_org_item(
        "it-2",
        "org-1",
        1,
        "bob",
        "Follow-up",
        "roadmap follow-up",
        "t",
        2,
        1,
        &sha32(22),
        None,
        None,
        Some(&emb),
    )
    .unwrap();

    assert_eq!(
        db.count_org_items("org-1").unwrap(),
        2,
        "both items counted while enabled"
    );

    db.set_org_context_enabled("org-1", false).unwrap();
    assert_eq!(
            db.count_org_items("org-1").unwrap(),
            0,
            "a disabled org's received_count must drop to zero, matching search/list, not stay inflated"
        );

    db.set_org_context_enabled("org-1", true).unwrap();
    assert_eq!(
        db.count_org_items("org-1").unwrap(),
        2,
        "re-enabling instantly restores the count — the replica was never touched"
    );
}

/// The self-share dedup source: `all_org_shared_content_hashes` returns every non-null
/// `content_sha256` from local `org_shares` — the set a retrieval hit is checked against.
#[test]
fn org_shared_content_hashes_are_collected() {
    let db = mem_db();
    let now = "2026-07-10T00:00:00Z";
    db.insert_org_share(
        "s1",
        "org-1",
        Some("m1"),
        None,
        "note",
        Some("T1"),
        1,
        1,
        &sha32(7),
        now,
    )
    .unwrap();
    db.insert_org_share(
        "s2",
        "org-1",
        None,
        Some("d1"),
        "note",
        Some("T2"),
        1,
        1,
        &sha32(8),
        now,
    )
    .unwrap();
    let hashes = db.all_org_shared_content_hashes().unwrap();
    assert!(hashes.contains(&sha32(7)));
    assert!(hashes.contains(&sha32(8)));
}

#[test]
fn org_share_scrub_is_explicit_and_legacy_default_is_fail_safe() {
    let db = mem_db();
    let now = "2026-08-12T00:00:00Z";
    db.lock()
        .execute(
            "INSERT INTO org_shares
               (id, org_id, meeting_id, kind, title, rev, generation, content_sha256,
                state, created_at, updated_at)
             VALUES (?1, ?2, ?3, 'note', 'Legacy', 1, 1, ?4, 'queued', ?5, ?5)",
            rusqlite::params!["legacy-default", "org-1", "m1", sha32(1), now],
        )
        .unwrap();
    db.insert_org_share_with_scrub(
        "explicit-off",
        "org-1",
        Some("m2"),
        None,
        "note",
        Some("Explicit"),
        1,
        1,
        &sha32(2),
        false,
        now,
    )
    .unwrap();

    assert!(db.get_org_share("legacy-default").unwrap().unwrap().scrub);
    assert!(!db.get_org_share("explicit-off").unwrap().unwrap().scrub);
}

/// `org_shares_for_source` (the re-publish-on-edit enumerator) returns EVERY uploaded row for a
/// source ACROSS ALL orgs (a note may be shared to several), and ONLY `uploaded` rows — a
/// `queued`/`failed`/`revoked` row is excluded (no live server item to supersede). A `None`/`None`
/// call returns empty.
#[test]
fn org_shares_for_source_returns_uploaded_rows_across_all_orgs() {
    let db = mem_db();
    let now = "2026-07-11T00:00:00Z";
    // Document d1 shared to TWO orgs, both uploaded.
    db.insert_org_share(
        "a",
        "org-1",
        None,
        Some("d1"),
        "note",
        Some("T"),
        1,
        1,
        &sha32(1),
        now,
    )
    .unwrap();
    db.set_org_share_uploaded("a", "item-a", now).unwrap();
    db.insert_org_share(
        "b",
        "org-2",
        None,
        Some("d1"),
        "note",
        Some("T"),
        1,
        1,
        &sha32(2),
        now,
    )
    .unwrap();
    db.set_org_share_uploaded("b", "item-b", now).unwrap();
    // A THIRD row for d1 still `queued` (not uploaded) — must be EXCLUDED.
    db.insert_org_share(
        "c",
        "org-3",
        None,
        Some("d1"),
        "note",
        Some("T"),
        1,
        1,
        &sha32(3),
        now,
    )
    .unwrap();
    // An unrelated document's uploaded row — must NOT be returned for d1.
    db.insert_org_share(
        "d",
        "org-1",
        None,
        Some("d2"),
        "note",
        Some("T"),
        1,
        1,
        &sha32(4),
        now,
    )
    .unwrap();
    db.set_org_share_uploaded("d", "item-d", now).unwrap();

    let rows = db.org_shares_for_source(None, Some("d1")).unwrap();
    assert_eq!(
        rows.len(),
        2,
        "both uploaded org rows for d1 (across orgs) returned"
    );
    let orgs: std::collections::HashSet<_> = rows.iter().map(|r| r.org_id.as_str()).collect();
    assert!(orgs.contains("org-1") && orgs.contains("org-2"));
    assert!(
        rows.iter().all(|r| r.state == "uploaded"),
        "only uploaded rows (the queued org-3 row is excluded)"
    );

    // Meeting arm + the both-None guard.
    db.insert_org_share(
        "e",
        "org-1",
        Some("m1"),
        None,
        "note",
        Some("T"),
        1,
        1,
        &sha32(5),
        now,
    )
    .unwrap();
    db.set_org_share_uploaded("e", "item-e", now).unwrap();
    let mrows = db.org_shares_for_source(Some("m1"), None).unwrap();
    assert_eq!(mrows.len(), 1);
    assert_eq!(mrows[0].meeting_id.as_deref(), Some("m1"));
    assert!(db.org_shares_for_source(None, None).unwrap().is_empty());
}

/// STUCK-REPUBLISH FIX: `org_shares_for_source` also surfaces a `failed` row that still carries a
/// non-null `item_id` — the exact shape `set_org_share_failed` produces when a REPUBLISH (not the
/// initial publish) fails transiently, since that function never clears `item_id` (only the success
/// path's `reset_org_share_for_retry` does). Such a row's OLD item is still genuinely live on the
/// server, so it must stay visible (not silently excluded forever). A `failed` row that never
/// published at all (`item_id` still NULL, e.g. the initial publish itself failed) must stay
/// EXCLUDED — there is no live item to supersede yet; the launch sweep's fresh-share retry handles
/// that case instead.
#[test]
fn org_shares_for_source_includes_failed_rows_with_a_live_item_id() {
    let db = mem_db();
    let now = "2026-07-12T00:00:00Z";

    // Row f1: published once (item-f1), THEN a republish attempt failed — mirrors the real sequence
    // (insert → uploaded → failed-on-republish). `item_id` stays set (set_org_share_failed doesn't
    // touch it).
    db.insert_org_share(
        "f1",
        "org-1",
        None,
        Some("d-stuck"),
        "note",
        Some("T"),
        1,
        1,
        &sha32(1),
        now,
    )
    .unwrap();
    db.set_org_share_uploaded("f1", "item-f1", now).unwrap();
    db.set_org_share_failed("f1", "republish_upload_failed", now)
        .unwrap();

    // Row f2: NEVER successfully published — failed before ever getting an item_id. Must stay
    // excluded (no live server item to supersede).
    db.insert_org_share(
        "f2",
        "org-2",
        None,
        Some("d-never-live"),
        "note",
        Some("T"),
        1,
        1,
        &sha32(2),
        now,
    )
    .unwrap();
    db.set_org_share_failed("f2", "publish_failed", now)
        .unwrap();

    let stuck_rows = db.org_shares_for_source(None, Some("d-stuck")).unwrap();
    assert_eq!(
        stuck_rows.len(),
        1,
        "a failed-with-item_id row (stuck republish) IS returned"
    );
    assert_eq!(stuck_rows[0].state, "failed");
    assert_eq!(stuck_rows[0].item_id.as_deref(), Some("item-f1"));

    let never_live_rows = db
        .org_shares_for_source(None, Some("d-never-live"))
        .unwrap();
    assert!(
        never_live_rows.is_empty(),
        "a failed row that never published (item_id NULL) stays excluded"
    );
}

/// `uploaded_org_shares_for_source_in_org` is (org, source)-SCOPED and OLDEST-FIRST: only the
/// Known-live rows plus fail-closed admission blockers for the EXACT (org, source), with uploaded
/// keepers first and blockers last. Another org/source and revoked rows remain excluded.
#[test]
fn uploaded_org_shares_for_source_in_org_scopes_and_orders_oldest_first() {
    let db = mem_db();
    // (org-1, d1): three uploaded rows at increasing timestamps → all returned, oldest-first.
    db.insert_org_share(
        "u-b",
        "org-1",
        None,
        Some("d1"),
        "note",
        Some("T"),
        1,
        1,
        &sha32(2),
        "2026-07-11T00:02:00Z",
    )
    .unwrap();
    db.set_org_share_uploaded("u-b", "item-b", "2026-07-11T00:02:00Z")
        .unwrap();
    db.insert_org_share(
        "u-a",
        "org-1",
        None,
        Some("d1"),
        "note",
        Some("T"),
        1,
        1,
        &sha32(1),
        "2026-07-11T00:01:00Z",
    )
    .unwrap();
    db.set_org_share_uploaded("u-a", "item-a", "2026-07-11T00:01:00Z")
        .unwrap();
    db.insert_org_share(
        "u-c",
        "org-1",
        None,
        Some("d1"),
        "note",
        Some("T"),
        1,
        1,
        &sha32(3),
        "2026-07-11T00:03:00Z",
    )
    .unwrap();
    db.set_org_share_uploaded("u-c", "item-c", "2026-07-11T00:03:00Z")
        .unwrap();
    // A still-`queued` row is retained as an admission blocker: NULL item identity is not proof that
    // an older client never dispatched it.
    db.insert_org_share(
        "u-q",
        "org-1",
        None,
        Some("d1"),
        "note",
        Some("T"),
        1,
        1,
        &sha32(4),
        "2026-07-11T00:00:40Z",
    )
    .unwrap();
    // A `revoked` row for the same (org, source) — EXCLUDED (intentionally torn down).
    db.insert_org_share(
        "u-r",
        "org-1",
        None,
        Some("d1"),
        "note",
        Some("T"),
        1,
        1,
        &sha32(5),
        "2026-07-11T00:00:30Z",
    )
    .unwrap();
    db.set_org_share_uploaded("u-r", "item-r", "2026-07-11T00:00:30Z")
        .unwrap();
    db.set_org_share_state("u-r", "revoked", "2026-07-11T00:05:00Z")
        .unwrap();
    // Same source d1 in ANOTHER org — EXCLUDED (org-scoped).
    db.insert_org_share(
        "u-o2",
        "org-2",
        None,
        Some("d1"),
        "note",
        Some("T"),
        1,
        1,
        &sha32(6),
        "2026-07-11T00:00:10Z",
    )
    .unwrap();
    db.set_org_share_uploaded("u-o2", "item-o2", "2026-07-11T00:00:10Z")
        .unwrap();
    // ANOTHER source in org-1 — EXCLUDED.
    db.insert_org_share(
        "u-d2",
        "org-1",
        None,
        Some("d2"),
        "note",
        Some("T"),
        1,
        1,
        &sha32(7),
        "2026-07-11T00:00:20Z",
    )
    .unwrap();
    db.set_org_share_uploaded("u-d2", "item-d2", "2026-07-11T00:00:20Z")
        .unwrap();

    let rows = db
        .uploaded_org_shares_for_source_in_org("org-1", None, Some("d1"))
        .unwrap();
    let ids: Vec<_> = rows.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["u-a", "u-b", "u-c", "u-q"],
        "known-live rows are first, followed by the exact-source admission blocker"
    );
    // The both-None guard returns empty (no source ⇒ nothing).
    assert!(db
        .uploaded_org_shares_for_source_in_org("org-1", None, None)
        .unwrap()
        .is_empty());
}

/// `duplicate_uploaded_org_shares` returns EXACTLY the extras (every `uploaded` row with an EARLIER
/// `uploaded` sibling in its (org, source) group) — never a keeper, never a single (non-dup) share,
/// never a revoked row. This is the on-launch dedup worklist.
#[test]
fn duplicate_uploaded_org_shares_returns_only_the_extras() {
    let db = mem_db();
    // Group A: (org-1, d1) has THREE uploaded rows → keeper = earliest (a1); extras = a2, a3.
    db.insert_org_share(
        "a1",
        "org-1",
        None,
        Some("d1"),
        "note",
        Some("T"),
        1,
        1,
        &sha32(1),
        "2026-07-11T00:01:00Z",
    )
    .unwrap();
    db.set_org_share_uploaded("a1", "item-a1", "2026-07-11T00:01:00Z")
        .unwrap();
    db.insert_org_share(
        "a2",
        "org-1",
        None,
        Some("d1"),
        "note",
        Some("T"),
        1,
        1,
        &sha32(2),
        "2026-07-11T00:02:00Z",
    )
    .unwrap();
    db.set_org_share_uploaded("a2", "item-a2", "2026-07-11T00:02:00Z")
        .unwrap();
    db.insert_org_share(
        "a3",
        "org-1",
        None,
        Some("d1"),
        "note",
        Some("T"),
        1,
        1,
        &sha32(3),
        "2026-07-11T00:03:00Z",
    )
    .unwrap();
    db.set_org_share_uploaded("a3", "item-a3", "2026-07-11T00:03:00Z")
        .unwrap();
    // Group B: (org-1, m1) meeting has a SINGLE uploaded row → NOT a duplicate.
    db.insert_org_share(
        "b1",
        "org-1",
        Some("m1"),
        None,
        "note",
        Some("T"),
        1,
        1,
        &sha32(4),
        "2026-07-11T00:01:00Z",
    )
    .unwrap();
    db.set_org_share_uploaded("b1", "item-b1", "2026-07-11T00:01:00Z")
        .unwrap();
    // Group C: (org-2, d1) SAME doc but DIFFERENT org → its own single-member group → NOT a duplicate.
    db.insert_org_share(
        "c1",
        "org-2",
        None,
        Some("d1"),
        "note",
        Some("T"),
        1,
        1,
        &sha32(5),
        "2026-07-11T00:01:00Z",
    )
    .unwrap();
    db.set_org_share_uploaded("c1", "item-c1", "2026-07-11T00:01:00Z")
        .unwrap();
    // A REVOKED extra in group A must NOT count (only live uploaded rows dedup).
    db.insert_org_share(
        "a-rev",
        "org-1",
        None,
        Some("d1"),
        "note",
        Some("T"),
        1,
        1,
        &sha32(6),
        "2026-07-11T00:04:00Z",
    )
    .unwrap();
    db.set_org_share_uploaded("a-rev", "item-arev", "2026-07-11T00:04:00Z")
        .unwrap();
    db.set_org_share_state("a-rev", "revoked", "2026-07-11T00:05:00Z")
        .unwrap();

    let extras = db.duplicate_uploaded_org_shares().unwrap();
    let ids: std::collections::HashSet<_> = extras.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(
        ids,
        ["a2", "a3"].into_iter().collect(),
        "only the later extras of group A (keep the earliest per group)"
    );
    assert!(
        !ids.contains("a1"),
        "the earliest (keeper) is never returned"
    );
    assert!(
        !ids.contains("b1") && !ids.contains("c1"),
        "single-share groups are not duplicates"
    );
    assert!(
        !ids.contains("a-rev"),
        "a revoked row is not a live duplicate"
    );
}

#[test]
fn known_uploaded_share_is_keeper_ahead_of_older_ambiguous_null_item() {
    let db = mem_db();
    db.insert_org_share(
        "ambiguous",
        "org-1",
        None,
        Some("d1"),
        "note",
        Some("T"),
        1,
        1,
        &sha32(1),
        "2026-07-11T00:00:00Z",
    )
    .unwrap();
    db.set_org_share_failed("ambiguous", "initial_post_pending", "2026-07-11T00:00:01Z")
        .unwrap();
    db.insert_org_share(
        "live",
        "org-1",
        None,
        Some("d1"),
        "note",
        Some("T"),
        1,
        1,
        &sha32(2),
        "2026-07-11T00:01:00Z",
    )
    .unwrap();
    db.set_org_share_uploaded("live", "item-live", "2026-07-11T00:01:01Z")
        .unwrap();
    let rows = db
        .uploaded_org_shares_for_source_in_org("org-1", None, Some("d1"))
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].id, "live");
    assert_eq!(rows[1].id, "ambiguous");
    assert!(!db
        .acquire_new_org_share_for_source(
            "third",
            "org-1",
            None,
            Some("d1"),
            "note",
            Some("T"),
            1,
            1,
            &sha32(3),
            true,
            "2026-07-11T00:02:00Z",
        )
        .unwrap());
}

#[test]
fn folder_and_source_closures_block_share_insert_and_rearm_until_reopened() {
    let db = mem_db();
    db.insert_folder(&Folder {
        id: "closing-folder".into(),
        name: "Closing".into(),
        path: "Closing".into(),
        parent_id: None,
        locked: false,
        created_at: "2026-08-14T00:00:00Z".into(),
    })
    .unwrap();
    db.insert_document(
        "closing-doc",
        "closing-folder",
        "doc.md",
        "body",
        "document",
        1,
    )
    .unwrap();
    let mut pre_note = sample_meeting("closing-meeting", "2026-08-14T00:00:00Z");
    pre_note.folder_id = Some("closing-folder".into());
    db.insert_meeting(&pre_note).unwrap();
    db.begin_org_folder_closure("closing-folder").unwrap();
    assert!(
        db.insert_outbound_note_share(
            "blocked-link",
            "closing-doc",
            "link",
            1,
            "2026-08-14T00:00:01Z",
        )
        .is_err(),
        "folder closure must reject link/user share admission too"
    );
    assert!(
        db.insert_outbound_share(
            "blocked-pre-note-link",
            "closing-meeting",
            "link",
            1,
            "2026-08-14T00:00:01Z",
        )
        .is_err(),
        "canonical pre-note placement must participate in the folder closure"
    );
    assert!(db
        .insert_org_share(
            "blocked-folder",
            "org-1",
            None,
            Some("closing-doc"),
            "note",
            Some("T"),
            1,
            1,
            &sha32(1),
            "t",
        )
        .is_err());
    assert!(db
        .insert_org_share(
            "blocked-pre-note-org",
            "org-1",
            Some("closing-meeting"),
            None,
            "meeting",
            Some("T"),
            1,
            1,
            &sha32(1),
            "t",
        )
        .is_err());
    db.clear_org_folder_closure("closing-folder").unwrap();
    db.insert_org_share(
        "share",
        "org-1",
        None,
        Some("closing-doc"),
        "note",
        Some("T"),
        1,
        1,
        &sha32(1),
        "t",
    )
    .unwrap();
    db.begin_org_source_closure("document", "closing-doc")
        .unwrap();
    assert!(
        db.insert_outbound_note_share(
            "blocked-source-link",
            "closing-doc",
            "link",
            1,
            "2026-08-14T00:00:02Z",
        )
        .is_err(),
        "source closure must reject link/user share admission too"
    );
    assert!(
        db.lock()
            .execute(
                "UPDATE documents SET text='edited while closing' WHERE id='closing-doc'",
                [],
            )
            .is_err(),
        "a destructive closure must freeze the source during remote revoke"
    );
    for sql in [
        "UPDATE documents SET name='renamed.md' WHERE id='closing-doc'",
        "UPDATE documents SET kind='note' WHERE id='closing-doc'",
    ] {
        assert!(
            db.lock().execute(sql, []).is_err(),
            "every envelope identity field is frozen"
        );
    }
    assert!(db
        .reset_org_share_for_retry("share", Some("T2"), 1, 1, &sha32(2), true, "t2")
        .is_err());
    db.clear_org_source_closure("document", "closing-doc")
        .unwrap();
    db.reset_org_share_for_retry("share", Some("T2"), 1, 1, &sha32(2), true, "t2")
        .unwrap();
}

#[test]
fn meeting_source_closure_blocks_provider_insert_delete_and_title_changes() {
    let db = mem_db();
    db.insert_folder(&Folder {
        id: "meeting-close-folder".into(),
        name: "Closing".into(),
        path: "Closing".into(),
        parent_id: None,
        locked: false,
        created_at: "2026-08-14T00:00:00Z".into(),
    })
    .unwrap();
    db.insert_meeting(&Meeting {
        id: "meeting-close".into(),
        started_at: "t".into(),
        ended_at: None,
        title: Some("Before".into()),
        duration_s: 0,
        audio_path: None,
        status: MeetingStatus::Summarized,
        folder_id: Some("meeting-close-folder".into()),
    })
    .unwrap();
    db.upsert_note(&NoteRecord {
        meeting_id: "meeting-close".into(),
        provider_id: "provider-a".into(),
        markdown: "body".into(),
        created_at: "t".into(),
        exported_path: None,
        model_requested: None,
        model_served: None,
        gateway_host: None,
    })
    .unwrap();
    db.begin_org_source_closure("meeting", "meeting-close")
        .unwrap();

    assert!(db
        .upsert_note(&NoteRecord {
            meeting_id: "meeting-close".into(),
            provider_id: "provider-b".into(),
            markdown: "other".into(),
            created_at: "t2".into(),
            exported_path: None,
            model_requested: None,
            model_served: None,
            gateway_host: None,
        })
        .is_err());
    assert!(db
        .lock()
        .execute(
            "DELETE FROM notes WHERE meeting_id='meeting-close' AND provider_id='provider-a'",
            [],
        )
        .is_err());
    assert!(db
        .lock()
        .execute(
            "UPDATE meetings SET title='After' WHERE id='meeting-close'",
            [],
        )
        .is_err());

    db.delete_meeting("meeting-close").unwrap();
    let exists: i64 = db
        .lock()
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM meetings WHERE id='meeting-close')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(exists, 0);
    let phase: String = db
        .lock()
        .query_row(
            "SELECT phase FROM org_share_closures
          WHERE scope_kind='meeting' AND scope_id='meeting-close'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        phase, "closed",
        "meeting cascade and barrier completion commit together"
    );
}

#[test]
fn source_closure_blocks_manual_attachment_mutation_but_allows_authorized_parent_cascade() {
    let db = mem_db();
    db.insert_folder(&Folder {
        id: "attachment-folder".into(),
        name: "Attachments".into(),
        path: "Attachments".into(),
        parent_id: None,
        locked: false,
        created_at: "2026-08-14T00:00:00Z".into(),
    })
    .unwrap();
    db.insert_document(
        "attachment-doc",
        "attachment-folder",
        "note.md",
        "body",
        "note",
        1,
    )
    .unwrap();
    let owner = crate::storage::AttachmentOwner::Document {
        document_id: "attachment-doc".into(),
    };
    let hash = [3u8; 32];
    db.insert_attachment(&crate::storage::NewAttachment {
        id: "attachment-one",
        owner: &owner,
        mime_type: "image/png",
        extension: "png",
        width: 1,
        height: 1,
        sha256: &hash,
        byte_len: 1,
        data: &[1],
        data_blob: None,
        created_at: 1,
    })
    .unwrap();
    db.begin_org_source_closure("document", "attachment-doc")
        .unwrap();
    assert!(db.delete_attachment(&owner, "attachment-one").is_err());
    db.delete_document("attachment-doc").unwrap();
    assert!(db.list_attachments(&owner).unwrap().is_empty());
    let phase: String = db
        .lock()
        .query_row(
            "SELECT phase FROM org_share_closures
          WHERE scope_kind='document' AND scope_id='attachment-doc'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        phase, "closed",
        "parent cascade and barrier completion commit together"
    );
}

#[test]
fn source_delete_with_attachment_rolls_back_until_every_share_is_terminal() {
    let db = mem_db();
    db.insert_folder(&Folder {
        id: "atomic-delete-folder".into(),
        name: "Atomic".into(),
        path: "Atomic".into(),
        parent_id: None,
        locked: false,
        created_at: "2026-08-14T00:00:00Z".into(),
    })
    .unwrap();
    db.insert_document(
        "atomic-delete-doc",
        "atomic-delete-folder",
        "note.md",
        "body",
        "note",
        1,
    )
    .unwrap();
    let owner = crate::storage::AttachmentOwner::Document {
        document_id: "atomic-delete-doc".into(),
    };
    db.insert_attachment(&crate::storage::NewAttachment {
        id: "atomic-image",
        owner: &owner,
        mime_type: "image/png",
        extension: "png",
        width: 1,
        height: 1,
        sha256: &[7; 32],
        byte_len: 1,
        data: &[7],
        data_blob: None,
        created_at: 1,
    })
    .unwrap();
    db.insert_outbound_share_attempt(
        "atomic-share",
        None,
        Some("atomic-delete-doc"),
        "link",
        1,
        "c534b6d2-02c1-4c2c-a256-3af8592b1567",
        "t",
    )
    .unwrap();
    db.begin_org_source_closure("document", "atomic-delete-doc")
        .unwrap();

    assert!(db.delete_document("atomic-delete-doc").is_err());
    let exists: i64 = db
        .lock()
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM documents WHERE id='atomic-delete-doc')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(exists, 1);
    assert_eq!(db.list_attachments(&owner).unwrap().len(), 1);
    let phase: String = db
        .lock()
        .query_row(
            "SELECT phase FROM org_share_closures
          WHERE scope_kind='document' AND scope_id='atomic-delete-doc'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(phase, "closing");

    db.set_outbound_share_state("atomic-share", "revoked")
        .unwrap();
    db.delete_document("atomic-delete-doc").unwrap();
    let exists: i64 = db
        .lock()
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM documents WHERE id='atomic-delete-doc')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(exists, 0);
    assert!(db.list_attachments(&owner).unwrap().is_empty());
    let phase: String = db
        .lock()
        .query_row(
            "SELECT phase FROM org_share_closures
          WHERE scope_kind='document' AND scope_id='atomic-delete-doc'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(phase, "closed");
}

/// Duplicate collapse cancels only rows with durable proof that no remote publish exists. Legacy
/// queued/generic-failed NULL identities are preserved because old clients dispatched while queued.
#[test]
fn cancel_pending_org_shares_scopes_to_org_and_source() {
    let db = mem_db();
    // Legacy queued + generic failed are NOT safe to cancel from local state alone.
    db.insert_org_share(
        "q",
        "org-1",
        None,
        Some("d1"),
        "note",
        Some("T"),
        1,
        1,
        &sha32(1),
        "2026-07-11T00:01:00Z",
    )
    .unwrap();
    db.insert_org_share(
        "f",
        "org-1",
        None,
        Some("d1"),
        "note",
        Some("T"),
        1,
        1,
        &sha32(2),
        "2026-07-11T00:02:00Z",
    )
    .unwrap();
    db.set_org_share_failed("f", "boom", "2026-07-11T00:02:30Z")
        .unwrap();
    db.insert_org_share(
        "too-large",
        "org-1",
        None,
        Some("d1"),
        "note",
        Some("T"),
        1,
        1,
        &sha32(8),
        "2026-07-11T00:02:35Z",
    )
    .unwrap();
    db.set_org_share_failed("too-large", "too_large", "2026-07-11T00:02:36Z")
        .unwrap();
    // Ambiguous stable-document attempts carry durable recovery witnesses and must never be
    // collapsed as ordinary pending duplicates, even when they share this exact local source.
    for (id, reason, byte) in [
        ("direct", "direct_put_pending", 21u8),
        ("republish-put", "republish_put_pending", 24u8),
        ("edit-conflict", "org_edit_conflict", 27u8),
        ("initial", "initial_post_pending", 22u8),
        ("replayable", "initial_post_replayable", 23u8),
    ] {
        db.insert_org_share(
            id,
            "org-1",
            None,
            Some("d1"),
            "note",
            Some("T"),
            1,
            1,
            &sha32(byte),
            "2026-07-11T00:02:45Z",
        )
        .unwrap();
        db.set_org_share_failed(id, reason, "2026-07-11T00:02:50Z")
            .unwrap();
    }
    // An UPLOADED row for the same (org, source) — NOT cancelled.
    db.insert_org_share(
        "u",
        "org-1",
        None,
        Some("d1"),
        "note",
        Some("T"),
        1,
        1,
        &sha32(3),
        "2026-07-11T00:00:30Z",
    )
    .unwrap();
    db.set_org_share_uploaded("u", "item-u", "2026-07-11T00:00:30Z")
        .unwrap();
    // A queued row for ANOTHER source and ANOTHER org — NOT touched.
    db.insert_org_share(
        "q-d2",
        "org-1",
        None,
        Some("d2"),
        "note",
        Some("T"),
        1,
        1,
        &sha32(4),
        "2026-07-11T00:01:00Z",
    )
    .unwrap();
    db.insert_org_share(
        "q-o2",
        "org-2",
        None,
        Some("d1"),
        "note",
        Some("T"),
        1,
        1,
        &sha32(5),
        "2026-07-11T00:01:00Z",
    )
    .unwrap();

    let n = db
        .cancel_pending_org_shares_for_source_in_org(
            "org-1",
            None,
            Some("d1"),
            "2026-07-11T01:00:00Z",
        )
        .unwrap();
    assert_eq!(
        n, 2,
        "only authenticated-absence and exact pre-dispatch failures cancel"
    );
    assert_eq!(db.get_org_share("q").unwrap().unwrap().state, "queued");
    assert_eq!(db.get_org_share("f").unwrap().unwrap().state, "failed");
    assert_eq!(
        db.get_org_share("too-large").unwrap().unwrap().state,
        "revoked"
    );
    for id in ["direct", "republish-put", "edit-conflict", "initial"] {
        assert_eq!(
            db.get_org_share(id).unwrap().unwrap().state,
            "failed",
            "an ambiguous recovery witness is preserved"
        );
    }
    assert_eq!(
        db.get_org_share("replayable").unwrap().unwrap().state,
        "revoked"
    );
    assert_eq!(
        db.get_org_share("u").unwrap().unwrap().state,
        "uploaded",
        "the uploaded keeper is untouched"
    );
    assert_eq!(
        db.get_org_share("q-d2").unwrap().unwrap().state,
        "queued",
        "another source is untouched"
    );
    assert_eq!(
        db.get_org_share("q-o2").unwrap().unwrap().state,
        "queued",
        "another org is untouched"
    );
    // both-None guard → no-op.
    assert_eq!(
        db.cancel_pending_org_shares_for_source_in_org("org-1", None, None, "x")
            .unwrap(),
        0
    );
}

#[test]
fn reusable_org_share_null_and_ambiguous_error_guards_are_fail_closed() {
    let db = mem_db();
    let insert = |id: &str, source: &str, byte: u8| {
        db.insert_org_share(
            id,
            "org-1",
            None,
            Some(source),
            "note",
            Some("T"),
            1,
            1,
            &sha32(byte),
            "2026-07-11T00:00:00Z",
        )
        .unwrap();
    };

    insert("plain", "plain-source", 1);
    assert_eq!(
        db.find_reusable_org_share("org-1", None, Some("plain-source"))
            .unwrap()
            .unwrap()
            .id,
        "plain",
        "a normal queued row with NULL last_error remains reusable"
    );
    db.reset_org_share_for_retry(
        "plain",
        Some("Changed"),
        1,
        1,
        &sha32(2),
        true,
        "2026-07-11T00:01:00Z",
    )
    .unwrap();

    insert("direct", "direct-source", 3);
    db.set_org_share_failed("direct", "direct_put_pending", "2026-07-11T00:01:00Z")
        .unwrap();
    assert!(db
        .find_reusable_org_share("org-1", None, Some("direct-source"))
        .unwrap()
        .is_none());
    assert!(matches!(
        db.reset_org_share_for_retry(
            "direct",
            Some("T"),
            1,
            1,
            &sha32(3),
            true,
            "2026-07-11T00:02:00Z",
        ),
        Err(crate::error::AppError::Unavailable(_))
    ));
    assert_eq!(
        db.get_org_share("direct")
            .unwrap()
            .unwrap()
            .last_error
            .as_deref(),
        Some("direct_put_pending")
    );

    insert("republish-put", "republish-put-source", 6);
    db.set_org_share_failed(
        "republish-put",
        "republish_put_pending",
        "2026-07-11T00:01:00Z",
    )
    .unwrap();
    assert!(db
        .find_reusable_org_share("org-1", None, Some("republish-put-source"))
        .unwrap()
        .is_none());
    assert!(matches!(
        db.reset_org_share_for_retry(
            "republish-put",
            Some("T"),
            1,
            1,
            &sha32(6),
            true,
            "2026-07-11T00:02:00Z",
        ),
        Err(crate::error::AppError::Unavailable(_))
    ));

    let (id, reason, byte) = ("edit-conflict", "org_edit_conflict", 9u8);
    insert(id, id, byte);
    db.set_org_share_failed(id, reason, "2026-07-11T00:01:00Z")
        .unwrap();
    assert!(db
        .find_reusable_org_share("org-1", None, Some(id))
        .unwrap()
        .is_none());
    assert!(matches!(
        db.reset_org_share_for_retry(
            id,
            Some("T"),
            1,
            1,
            &sha32(byte),
            true,
            "2026-07-11T00:02:00Z",
        ),
        Err(crate::error::AppError::Unavailable(_))
    ));
    assert_eq!(
        db.get_org_share(id).unwrap().unwrap().last_error.as_deref(),
        Some(reason)
    );

    insert("replay", "replay-source", 4);
    db.set_org_share_failed("replay", "initial_post_replayable", "2026-07-11T00:01:00Z")
        .unwrap();
    assert_eq!(
        db.find_reusable_org_share("org-1", None, Some("replay-source"))
            .unwrap()
            .unwrap()
            .id,
        "replay"
    );
    assert!(matches!(
        db.reset_org_share_for_retry(
            "replay",
            Some("Changed"),
            1,
            1,
            &sha32(5),
            true,
            "2026-07-11T00:02:00Z",
        ),
        Err(crate::error::AppError::Unavailable(_))
    ));
    assert!(matches!(
        db.reset_org_share_for_retry(
            "replay",
            Some("T"),
            1,
            1,
            &sha32(4),
            true,
            "2026-07-11T00:03:00Z",
        ),
        Err(crate::error::AppError::Unavailable(_))
    ));
    assert_eq!(
        db.get_org_share("replay")
            .unwrap()
            .unwrap()
            .last_error
            .as_deref(),
        Some("initial_post_replayable"),
        "only the actor/owner/doc/access-bound replay CAS may re-arm an ambiguous initial POST"
    );
}

/// NOTES feature — the additive columns exist after migrate() and re-migrating is a no-op
/// (idempotent guard). `documents.title/updated_at/exported_path` + `folders.kind` are present,
/// and existing folders default to `kind='meeting'`.
#[test]
fn notes_migration_adds_columns_and_is_idempotent() {
    let db = mem_db();
    let has_col = |table: &str, col: &str| -> bool {
        let conn = db.lock();
        let mut stmt = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap();
        let cols: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        cols.iter().any(|c| c == col)
    };
    for col in ["title", "updated_at", "exported_path"] {
        assert!(has_col("documents", col), "documents.{col} added");
    }
    assert!(has_col("folders", "kind"), "folders.kind added");

    // A folder inserted via the legacy 6-column path reads back kind='meeting' (the DEFAULT).
    db.insert_folder(&Folder {
        id: "leg".into(),
        name: "Legacy".into(),
        path: "Legacy".into(),
        parent_id: None,
        locked: false,
        created_at: "2026-07-10T00:00:00Z".into(),
    })
    .unwrap();
    assert_eq!(
        db.folder_kind("leg").unwrap().as_deref(),
        Some("meeting"),
        "existing folders default to kind='meeting' (Meetings tree unchanged)"
    );
    assert!(
        db.list_note_folders().unwrap().is_empty(),
        "a meeting folder is NOT a note-folder"
    );

    // Re-migrate: still fine (idempotent), columns still present.
    db.migrate().unwrap();
    assert!(has_col("documents", "exported_path"));
}

/// NOTES — front-matter parsing: `tags` (flow list, block list, single scalar), scalar
/// `properties` (excl. tags), and the body snippet (front-matter stripped). No new deps.
#[test]
fn front_matter_parse_and_snippet() {
    // Flow-list tags + scalar props.
    let md =
        "---\ntags: [alpha, \"beta gamma\"]\nstatus: draft\ndate: 2026-07-10\n---\nBody text here.";
    let (tags, props) = parse_front_matter(md);
    assert_eq!(tags, vec!["alpha".to_string(), "beta gamma".to_string()]);
    assert_eq!(props.get("status"), Some(&"draft".to_string()));
    assert_eq!(props.get("date"), Some(&"2026-07-10".to_string()));
    assert!(!props.contains_key("tags"), "tags excluded from properties");
    assert_eq!(note_snippet(md), "Body text here.");
    let (_yaml, body) = split_front_matter(md);
    assert_eq!(body, "Body text here.", "front-matter stripped from body");

    // Block-list tags.
    let md2 = "---\ntags:\n  - one\n  - two\n---\n# Heading\nMore.";
    let (tags2, _p2) = parse_front_matter(md2);
    assert_eq!(tags2, vec!["one".to_string(), "two".to_string()]);

    // No front-matter: whole string is body, no tags/props.
    let md3 = "Just a plain note, no YAML.";
    let (tags3, props3) = parse_front_matter(md3);
    assert!(tags3.is_empty() && props3.is_empty());
    assert_eq!(split_front_matter(md3).1, md3);

    // Snippet truncation at ~180 chars.
    let long = format!("---\ntags: []\n---\n{}", "word ".repeat(100));
    assert!(
        note_snippet(&long).chars().count() <= 181,
        "snippet capped ~180 chars (+ellipsis)"
    );
}

/// TIER 1 installed-base flip: `migrate()` sets `semantic_search_enabled='true'` + the
/// `semantic_default_v1` sentinel exactly ONCE; a later opt-out (`false`) survives a re-migrate
/// (the sentinel guards the block so it never re-fires). Config-only, idempotent, reversible.
#[test]
fn tier1_semantic_default_migration_runs_once_and_opt_out_persists() {
    let db = mem_db();
    db.migrate().unwrap();
    assert_eq!(
        db.get_setting("semantic_default_v1").unwrap().as_deref(),
        Some("1"),
        "sentinel is set after the migration"
    );
    assert_eq!(
        db.get_setting("semantic_search_enabled")
            .unwrap()
            .as_deref(),
        Some("true"),
        "installed base is flipped ON once"
    );
    // A user turns semantic OFF after the migration; re-running migrate() must NOT flip it back.
    db.set_setting("semantic_search_enabled", "false").unwrap();
    db.migrate().unwrap();
    assert_eq!(
        db.get_setting("semantic_search_enabled")
            .unwrap()
            .as_deref(),
        Some("false"),
        "sentinel-guarded: a post-migration opt-out persists across re-migrate"
    );
}

/// WS8 installed-base flip (mirrors the `semantic_default_v1` precedent): `migrate()` sets
/// `capture_system_audio='true'` + the `capture_default_v1` sentinel exactly ONCE; a later opt-out
/// (`false`) survives a re-migrate (the sentinel guards the block so it never re-fires). Config-only,
/// idempotent, reversible.
#[test]
fn capture_default_migration_runs_once_and_opt_out_persists() {
    let db = mem_db();
    db.migrate().unwrap();
    assert_eq!(
        db.get_setting("capture_default_v1").unwrap().as_deref(),
        Some("1"),
        "sentinel is set after the migration"
    );
    assert_eq!(
        db.get_setting("capture_system_audio").unwrap().as_deref(),
        Some("true"),
        "installed base is flipped ON once"
    );
    // A user turns system-audio capture OFF after the migration; re-running migrate() must NOT
    // flip it back.
    db.set_setting("capture_system_audio", "false").unwrap();
    db.migrate().unwrap();
    assert_eq!(
        db.get_setting("capture_system_audio").unwrap().as_deref(),
        Some("false"),
        "sentinel-guarded: a post-migration opt-out persists across re-migrate"
    );
}

// ── Phase 2b: egress_log ─────────────────────────────────────────────────

fn sample_egress_entry() -> crate::summarize::egress_log::EgressEntry {
    use crate::summarize::egress_log::EgressEntry;
    use crate::summarize::meta::{CallMeta, RedactionCounts};
    EgressEntry {
        provider_id: "anthropic".to_string(),
        destination: "api.anthropic.com".to_string(),
        model_requested: "claude-opus-4-8".to_string(),
        call_kind: "complete",
        meta: CallMeta {
            model_served: Some("claude-opus-4-8-20251001".to_string()),
            prompt_tokens: Some(100),
            completion_tokens: Some(50),
            total_tokens: Some(150),
            cached_tokens: None,
            redactions: None,
        },
        redactions: RedactionCounts {
            email: 1,
            card: 0,
            phone: 1,
            name: 2,
        },
        system_bytes: 512,
        user_bytes: 1024,
        meeting_id: Some("m1".to_string()),
    }
}

/// After migrate(), `egress_log` exists; insert_egress then SELECT COUNT(*) == 1.
#[test]
fn egress_log_table_exists_after_migrate_and_insert_works() {
    let db = mem_db();
    let entry = sample_egress_entry();
    db.insert_egress(1_700_000_000, &entry).unwrap();

    let conn = db.lock();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM egress_log", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1, "one row must have been inserted into egress_log");
}

/// migrate() is idempotent even with the new egress_log table (re-running migrate() on an
/// already-migrated DB must not error).
#[test]
fn egress_log_migrate_idempotent() {
    let db = mem_db(); // migrate() already ran once
    db.migrate().unwrap(); // second run must succeed
}

/// insert_egress stores token counts and redaction counts correctly.
#[test]
fn egress_log_insert_round_trips_counts() {
    let db = mem_db();
    let entry = sample_egress_entry();
    db.insert_egress(9999, &entry).unwrap();

    let conn = db.lock();
    let (ts, prompt_tokens, redactions_email, redactions_name, system_bytes): (
        i64,
        Option<i64>,
        i64,
        i64,
        i64,
    ) = conn
        .query_row(
            "SELECT ts, prompt_tokens, redactions_email, redactions_name, system_bytes
                   FROM egress_log LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    assert_eq!(ts, 9999);
    assert_eq!(prompt_tokens, Some(100));
    assert_eq!(redactions_email, 1);
    assert_eq!(redactions_name, 2);
    assert_eq!(system_bytes, 512);
}

// ── Phase 6: egress_summary ───────────────────────────────────────────────

/// Helper: build an `EgressEntry` with the given model_served, total_tokens, and redaction
/// email count (other fields are stable across tests).
fn egress_entry_for_summary(
    model_served: &str,
    total_tokens: u32,
    redaction_email: u32,
    redaction_name: u32,
) -> crate::summarize::egress_log::EgressEntry {
    use crate::summarize::egress_log::EgressEntry;
    use crate::summarize::meta::{CallMeta, RedactionCounts};
    EgressEntry {
        provider_id: "anthropic".to_string(),
        destination: "api.anthropic.com".to_string(),
        model_requested: model_served.to_string(),
        call_kind: "complete",
        meta: CallMeta {
            model_served: Some(model_served.to_string()),
            prompt_tokens: None,
            completion_tokens: None,
            total_tokens: Some(total_tokens),
            cached_tokens: None,
            redactions: None,
        },
        redactions: RedactionCounts {
            email: redaction_email,
            card: 0,
            phone: 0,
            name: redaction_name,
        },
        system_bytes: 0,
        user_bytes: 0,
        meeting_id: None,
    }
}

/// `egress_summary(30)` on an EMPTY table returns all-zero totals and empty vecs — not an error.
/// RED → verify the function exists and handles the zero case before we insert any rows.
#[test]
fn egress_summary_empty_table_returns_zeros() {
    let db = mem_db();
    let ledger = db
        .egress_summary(30)
        .expect("egress_summary must not error on empty table");
    assert_eq!(
        ledger.total_calls, 0,
        "total_calls should be 0 on empty table"
    );
    assert_eq!(
        ledger.total_tokens, 0,
        "total_tokens should be 0 on empty table"
    );
    assert!(
        ledger.by_model.is_empty(),
        "by_model should be empty on empty table"
    );
    assert!(
        ledger.by_day.is_empty(),
        "by_day should be empty on empty table"
    );
    assert_eq!(ledger.total_redactions.email, 0);
    assert_eq!(ledger.total_redactions.name, 0);
    assert!(
        ledger.recent.is_empty(),
        "recent should be empty on empty table"
    );
}

/// Insert 3 rows — 2 models ("claude-opus", "gpt-4o"), 2 distinct UTC dates, known redaction
/// counts — and assert `egress_summary(30)` aggregates them correctly.
///
/// Row layout:
///   ts=1_700_000_000 (2023-11-14 UTC) — claude-opus, 100 tokens, email=1, name=0
///   ts=1_700_086_400 (2023-11-15 UTC) — gpt-4o,     200 tokens, email=0, name=1
///   ts=1_700_086_401 (2023-11-15 UTC) — claude-opus,  50 tokens, email=2, name=1
#[test]
fn egress_summary_aggregates_correctly() {
    let db = mem_db();

    // Insert outside the 30-day window: ts=0 should be excluded once `since` is computed.
    // (We use absolute timestamps well in the past — window uses `now - 30*86400` which will
    // exclude ts=0. But we can't control "now" in tests so use days <= 0 for all-rows, or
    // override the window. Here we use days=0 which means "all rows" per our implementation.)
    let entry1 = egress_entry_for_summary("claude-opus", 100, 1, 0);
    let entry2 = egress_entry_for_summary("gpt-4o", 200, 0, 1);
    let entry3 = egress_entry_for_summary("claude-opus", 50, 2, 1);

    // Use realistic timestamps on the same "relative past" — for the rolling window test we
    // use days=0 (all-rows mode: since=0) to avoid clock skew in CI.
    db.insert_egress(1_700_000_000, &entry1).unwrap();
    db.insert_egress(1_700_086_400, &entry2).unwrap();
    db.insert_egress(1_700_086_401, &entry3).unwrap();

    let ledger = db.egress_summary(0).expect("egress_summary must not error");

    // ── totals ────────────────────────────────────────────────────────
    assert_eq!(ledger.total_calls, 3, "total_calls must be 3");
    assert_eq!(
        ledger.total_tokens, 350,
        "total_tokens must be 100+200+50=350"
    );

    // ── by_model (ordered tokens DESC: gpt-4o=200, claude-opus=150) ──
    assert_eq!(ledger.by_model.len(), 2, "by_model must have 2 entries");
    let gpt = ledger
        .by_model
        .iter()
        .find(|m| m.model == "gpt-4o")
        .expect("gpt-4o entry must exist");
    assert_eq!(gpt.calls, 1, "gpt-4o: 1 call");
    assert_eq!(gpt.tokens, 200, "gpt-4o: 200 tokens");

    let opus = ledger
        .by_model
        .iter()
        .find(|m| m.model == "claude-opus")
        .expect("claude-opus entry must exist");
    assert_eq!(opus.calls, 2, "claude-opus: 2 calls");
    assert_eq!(opus.tokens, 150, "claude-opus: 100+50=150 tokens");

    // First entry is the one with more tokens (gpt-4o=200 > claude-opus=150)
    assert_eq!(
        ledger.by_model[0].model, "gpt-4o",
        "by_model[0] must be gpt-4o (most tokens)"
    );

    // ── by_day (2 distinct UTC days, ascending) ───────────────────────
    assert_eq!(ledger.by_day.len(), 2, "by_day must have 2 entries");
    // 1_700_000_000 = 2023-11-14 UTC; 1_700_086_400/1_700_086_401 = 2023-11-15 UTC
    assert_eq!(
        ledger.by_day[0].day, "2023-11-14",
        "by_day[0] must be 2023-11-14"
    );
    assert_eq!(ledger.by_day[0].tokens, 100, "2023-11-14: 100 tokens");
    assert_eq!(
        ledger.by_day[1].day, "2023-11-15",
        "by_day[1] must be 2023-11-15"
    );
    assert_eq!(
        ledger.by_day[1].tokens, 250,
        "2023-11-15: 200+50=250 tokens"
    );

    // ── total_redactions: email=1+0+2=3, name=0+1+1=2 ────────────────
    assert_eq!(
        ledger.total_redactions.email, 3,
        "total email redactions must be 3"
    );
    assert_eq!(ledger.total_redactions.card, 0);
    assert_eq!(ledger.total_redactions.phone, 0);
    assert_eq!(
        ledger.total_redactions.name, 2,
        "total name redactions must be 2"
    );

    // ── recent: 3 rows, newest first ─────────────────────────────────
    assert_eq!(ledger.recent.len(), 3, "recent must have 3 rows");
    assert_eq!(
        ledger.recent[0].ts, 1_700_086_401,
        "recent[0] must be the newest row"
    );
    assert_eq!(
        ledger.recent[2].ts, 1_700_000_000,
        "recent[2] must be the oldest row"
    );
}

/// `egress_summary(30)` excludes rows outside the time window. Two rows in the past (ts≈0 =
/// 1970, well before any 30-day window); one row at `now_unix - 1` (inside the window).
/// We verify that only the in-window row is counted when days=30.
#[test]
fn egress_summary_respects_time_window() {
    let db = mem_db();

    // A row far in the past — should be excluded from any 30-day window.
    let old = egress_entry_for_summary("old-model", 9999, 5, 5);
    db.insert_egress(1_000, &old).unwrap(); // 1970, definitley outside 30d window

    // A row "now" — should be included.
    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let recent = egress_entry_for_summary("new-model", 42, 0, 0);
    db.insert_egress(now_unix, &recent).unwrap();

    let ledger = db
        .egress_summary(30)
        .expect("egress_summary must not error");
    assert_eq!(
        ledger.total_calls, 1,
        "only 1 in-window row should be counted"
    );
    assert_eq!(
        ledger.total_tokens, 42,
        "only in-window tokens should be counted"
    );
    assert_eq!(
        ledger.by_model.len(),
        1,
        "by_model should only contain new-model"
    );
    assert_eq!(ledger.by_model[0].model, "new-model");
    // Redaction totals from the old row must NOT appear.
    assert_eq!(
        ledger.total_redactions.email, 0,
        "old row redactions must be excluded"
    );
}

/// FIX 2: a row where BOTH `model_served` and `model_requested` are empty strings (the common
/// case for claude_code / anthropic default-model calls before the gateway provider was added)
/// must appear in `by_model` under the label `'(unknown)'`, not as a blank `""` string.
#[test]
fn egress_summary_blank_model_fields_bucket_under_unknown() {
    use crate::summarize::egress_log::EgressEntry;
    use crate::summarize::meta::{CallMeta, RedactionCounts};

    let db = mem_db();

    // Row with both model fields empty (default-model call; model_requested="" stored verbatim).
    let entry_no_model = EgressEntry {
        provider_id: "claude_code".to_string(),
        destination: "claude_code (Anthropic CLI)".to_string(),
        model_requested: String::new(), // "" — the real default
        call_kind: "complete",
        meta: CallMeta {
            model_served: None, // stored as NULL
            prompt_tokens: Some(10),
            completion_tokens: Some(5),
            total_tokens: Some(15),
            cached_tokens: None,
            redactions: None,
        },
        redactions: RedactionCounts::default(),
        system_bytes: 0,
        user_bytes: 0,
        meeting_id: None,
    };

    db.insert_egress(0, &entry_no_model).unwrap();

    let ledger = db.egress_summary(0).expect("egress_summary must not error");
    assert_eq!(ledger.by_model.len(), 1, "must have one by_model entry");
    assert_eq!(
        ledger.by_model[0].model, "(unknown)",
        "empty model_served + empty model_requested must bucket under '(unknown)', not ''"
    );
    assert_eq!(ledger.by_model[0].tokens, 15);
}

/// brain2 realtime notes: the `manual_notes` buffer round-trips (set → get), defaults to "" for a
/// meeting that never set it (NULL legacy column), and `set("")` clears it. The buffer is DURABLE
/// (canonical store) — it is not destroyed by any plain getter/setter.
#[test]
fn manual_notes_round_trip_and_default_empty() {
    let db = mem_db();
    db.insert_meeting(&sample_meeting("m1", "2026-06-30T09:00:00Z"))
        .unwrap();

    // Default: never set ⇒ "" (NULL column reads back empty, no behavior change for legacy rows).
    assert_eq!(db.get_manual_notes("m1").unwrap(), "");
    // Unknown meeting ⇒ "" (no row), never an error.
    assert_eq!(db.get_manual_notes("nope").unwrap(), "");

    // Set → get round-trips verbatim.
    db.set_manual_notes("m1", "ship the deck by Friday; Anna owns QA")
        .unwrap();
    assert_eq!(
        db.get_manual_notes("m1").unwrap(),
        "ship the deck by Friday; Anna owns QA"
    );

    // Overwrite replaces the whole buffer (FE owns the full text).
    db.set_manual_notes("m1", "rewritten").unwrap();
    assert_eq!(db.get_manual_notes("m1").unwrap(), "rewritten");
}

/// SEAL ROUND-TRIP (mirrors `seal_transcript_timeline_round_trips_byte_identical`): the typed
/// notes seal to a `manual_notes_blob` (plaintext blanked, ciphertext does NOT leak), and unlock
/// restores the plaintext BYTE-IDENTICAL. Uses the DB seal/restore primitives + crypto directly.
#[test]
fn seal_manual_notes_round_trips_byte_identical() {
    let db = mem_db();
    db.insert_meeting(&sample_meeting("m1", "2026-06-30T09:00:00Z"))
        .unwrap();
    let typed = "zażółć gęślą jaźń 🔒 — DECISION: ship Friday; Anna owns QA";
    db.set_manual_notes("m1", typed).unwrap();

    let ck = crate::crypto::random_key().unwrap();
    // SEAL: encrypt → verify-before-destroy → blank plaintext (the seal_meeting_extras pattern).
    let rn = db.raw_manual_notes("m1").unwrap().unwrap();
    let blob = crate::crypto::encrypt(&ck, rn.text.as_bytes(), b"aad").unwrap();
    assert_eq!(
        crate::crypto::decrypt(&ck, &blob, b"aad").unwrap(),
        rn.text.as_bytes()
    );
    db.seal_manual_notes("m1", &blob).unwrap();

    // At rest while sealed: plaintext blanked, blob present, ciphertext doesn't leak the plaintext.
    let sealed = db.raw_manual_notes("m1").unwrap().unwrap();
    assert_eq!(sealed.text, "", "plaintext blanked while sealed");
    assert!(
        sealed.blob.is_some(),
        "manual_notes_blob present while sealed"
    );
    assert_eq!(
        db.get_manual_notes("m1").unwrap(),
        "",
        "the gated reader sees blank while sealed"
    );
    let cipher = sealed.blob.as_ref().unwrap();
    let leaks = cipher.windows(typed.len()).any(|w| w == typed.as_bytes());
    assert!(!leaks, "manual-notes ciphertext must not leak plaintext");

    // UNLOCK: decrypt the blob → restore plaintext byte-identical.
    let blob = sealed.blob.unwrap();
    let pt = String::from_utf8(crate::crypto::decrypt(&ck, &blob, b"aad").unwrap()).unwrap();
    db.set_manual_notes("m1", &pt).unwrap();
    assert_eq!(
        db.get_manual_notes("m1").unwrap(),
        typed,
        "typed notes round-trip byte-identical"
    );

    // PERMANENT remove-lock: clear the blob after the plaintext is back.
    db.clear_manual_notes_blob("m1").unwrap();
    assert!(
        db.raw_manual_notes("m1").unwrap().unwrap().blob.is_none(),
        "blob cleared on remove-lock"
    );
    assert_eq!(
        db.get_manual_notes("m1").unwrap(),
        typed,
        "plaintext survives the blob clear"
    );
}

/// LOCK-SAFETY (verify-before-destroy): startup reconciliation re-blanks the typed-notes
/// plaintext of a locked meeting ONLY when the sealed `manual_notes_blob` exists. A buffer that
/// was NEVER sealed (no blob) is LEFT INTACT — reconciliation must never destroy the only copy.
#[test]
fn reconcile_reblanks_manual_notes_only_when_blob_present() {
    let db = mem_db();
    seed_folder(&db, "f-lock", "Secret");
    // Meeting A: sealed blob present + plaintext stranded by a crash-while-unlocked → MUST re-blank.
    db.insert_meeting(&sample_meeting("m-sealed", "2026-06-30T09:00:00Z"))
        .unwrap();
    note_for(&db, "m-sealed", "claude_code", "");
    db.set_note_folder("m-sealed", Some("f-lock")).unwrap();
    db.seal_note("m-sealed", "claude_code", b"ciphertext")
        .unwrap();
    db.seal_manual_notes("m-sealed", b"ck-ciphertext-blob")
        .unwrap(); // blob present
    db.set_manual_notes("m-sealed", "restored plaintext stranded by the crash")
        .unwrap();

    // Meeting B: NO blob (buffer typed but never sealed) → MUST be left intact (no encrypted copy).
    db.insert_meeting(&sample_meeting("m-unsealed", "2026-06-30T09:30:00Z"))
        .unwrap();
    note_for(&db, "m-unsealed", "claude_code", "note");
    db.set_note_folder("m-unsealed", Some("f-lock")).unwrap();
    db.set_manual_notes(
        "m-unsealed",
        "typed but never sealed — must not be destroyed",
    )
    .unwrap();

    db.set_folder_locked("f-lock", true, Some(b"wrapped"))
        .unwrap();
    db.reblank_locked_folders_at_rest().unwrap();

    assert_eq!(
        db.get_manual_notes("m-sealed").unwrap(),
        "",
        "sealed meeting's stranded plaintext re-blanked"
    );
    assert_eq!(
        db.get_manual_notes("m-unsealed").unwrap(),
        "typed but never sealed — must not be destroyed",
        "an unsealed buffer (no blob) must NEVER be blanked — that would destroy the only copy"
    );
}

/// Test-only builder for a correction example tied to a meeting.
fn corr_rec(
    kind: &str,
    input: &str,
    model_output: &str,
    fin: Option<&str>,
    accepted: bool,
    at: &str,
    meeting_id: Option<&str>,
) -> crate::storage::models::CorrectionRecord {
    crate::storage::models::CorrectionRecord {
        id: 0,
        kind: kind.to_string(),
        input: input.to_string(),
        model_output: model_output.to_string(),
        final_output: fin.map(str::to_string),
        accepted,
        owner_id: "local".to_string(),
        created_at: at.to_string(),
        meeting_id: meeting_id.map(str::to_string),
    }
}

#[test]
fn correction_log_round_trips_and_filters_by_kind() {
    let db = mem_db();
    // Two visible meetings in an OPEN folder so their corrections are returned by the gate.
    seed_folder(&db, "f-open", "Open");
    for id in ["m1", "m2", "m3"] {
        db.insert_meeting(&sample_meeting(id, "2026-06-24T10:00:00Z"))
            .unwrap();
        note_for(&db, id, "claude_code", "note");
        db.set_note_folder(id, Some("f-open")).unwrap();
    }
    let nothing = std::collections::HashSet::new();

    // Insert two NER examples (one accepted-as-is, one edited) and one of another kind.
    let id1 = db
        .log_correction(&corr_rec(
            "ner",
            "in-1",
            "out-1",
            None,
            true,
            "2026-06-28T10:00:00Z",
            Some("m1"),
        ))
        .unwrap();
    let id2 = db
        .log_correction(&corr_rec(
            "ner",
            "in-2",
            "out-2",
            Some("fixed-2"),
            false,
            "2026-06-28T10:01:00Z",
            Some("m2"),
        ))
        .unwrap();
    db.log_correction(&corr_rec(
        "timeline",
        "in-3",
        "out-3",
        None,
        true,
        "2026-06-28T10:02:00Z",
        Some("m3"),
    ))
    .unwrap();
    assert!(id2 > id1);

    // Only the matching kind comes back, newest first.
    let ner = db.list_corrections("ner", 10, &nothing).unwrap();
    assert_eq!(ner.len(), 2);
    assert_eq!(ner[0].id, id2);
    assert_eq!(ner[0].input, "in-2");
    assert_eq!(ner[0].final_output.as_deref(), Some("fixed-2"));
    assert!(!ner[0].accepted);
    assert_eq!(ner[0].meeting_id.as_deref(), Some("m2"));
    assert_eq!(ner[1].id, id1);
    assert!(ner[1].accepted);
    assert_eq!(ner[1].final_output, None);
    assert_eq!(ner[1].owner_id, "local");

    // Limit is honoured.
    assert_eq!(db.list_corrections("ner", 1, &nothing).unwrap().len(), 1);
    // A kind with no rows yields empty (not an error).
    assert!(db
        .list_corrections("does-not-exist", 10, &nothing)
        .unwrap()
        .is_empty());
}

/// GATE: a correction row for a sealed-and-not-unlocked meeting is EXCLUDED; the same kind's row
/// for a visible meeting is INCLUDED. Session-unlocking the sealed folder makes it reappear.
/// (RED before the gate: the un-gated reader returned both rows regardless of seal state.)
#[test]
fn list_corrections_excludes_sealed_meeting() {
    let db = mem_db();
    seed_folder(&db, "f-open", "Open");
    seed_folder(&db, "f-locked", "Secret");
    db.set_folder_locked("f-locked", true, Some(b"wrapped"))
        .unwrap();
    // Visible meeting in the open folder.
    db.insert_meeting(&sample_meeting("m-open", "2026-06-24T10:00:00Z"))
        .unwrap();
    note_for(&db, "m-open", "claude_code", "note");
    db.set_note_folder("m-open", Some("f-open")).unwrap();
    // Sealed meeting in the locked folder.
    db.insert_meeting(&sample_meeting("m-sealed", "2026-06-24T11:00:00Z"))
        .unwrap();
    note_for(&db, "m-sealed", "claude_code", "note");
    db.set_note_folder("m-sealed", Some("f-locked")).unwrap();

    db.log_correction(&corr_rec(
        "ner",
        "in-o",
        "out-o",
        None,
        true,
        "2026-06-28T10:00:00Z",
        Some("m-open"),
    ))
    .unwrap();
    db.log_correction(&corr_rec(
        "ner",
        "in-s",
        "out-s",
        None,
        true,
        "2026-06-28T10:01:00Z",
        Some("m-sealed"),
    ))
    .unwrap();

    let nothing = std::collections::HashSet::new();
    let visible = db.list_corrections("ner", 10, &nothing).unwrap();
    assert_eq!(
        visible.len(),
        1,
        "sealed meeting's correction leaked through the gate"
    );
    assert_eq!(visible[0].meeting_id.as_deref(), Some("m-open"));

    // Session-unlock the locked folder → its correction reappears.
    let mut unlocked = std::collections::HashSet::new();
    unlocked.insert("f-locked".to_string());
    let both = db.list_corrections("ner", 10, &unlocked).unwrap();
    assert_eq!(
        both.len(),
        2,
        "session-unlocked meeting's correction must reappear"
    );
}

/// Canonical-placement gate matrix for correction and voice-biometric derived data.
///
/// These rows model data re-derived while a locked folder was session-unlocked and left behind
/// after a crash/relock boundary. The readers must still treat `meetings.folder_id` as the durable
/// visibility authority: opening one sealed folder restores only that folder's rows, while another
/// sealed folder stays invisible and an unrelated open meeting stays independently scoped.
#[test]
fn canonical_meeting_folder_gates_corrections_voiceprints_and_speaker_labels() {
    let db = mem_db();
    seed_folder(&db, "f-secret", "Secret");
    seed_folder(&db, "f-other-secret", "Other secret");
    seed_folder(&db, "f-open", "Open");

    for (meeting_id, folder_id, started_at) in [
        ("m-secret", "f-secret", "2026-06-24T10:00:00Z"),
        ("m-other-secret", "f-other-secret", "2026-06-24T11:00:00Z"),
        ("m-open", "f-open", "2026-06-24T12:00:00Z"),
    ] {
        db.insert_meeting(&sample_meeting(meeting_id, started_at))
            .unwrap();
        // Use the production filing API. Deliberately create no provider note: this proves the
        // canonical meetings.folder_id gate also covers pre-note recordings.
        db.set_meeting_folder(meeting_id, Some(folder_id)).unwrap();
        assert_eq!(
            db.get_meeting(meeting_id)
                .unwrap()
                .unwrap()
                .folder_id
                .as_deref(),
            Some(folder_id)
        );
    }

    db.set_folder_locked("f-secret", true, Some(b"wrapped-secret"))
        .unwrap();
    db.set_folder_locked("f-other-secret", true, Some(b"wrapped-other"))
        .unwrap();

    for (meeting_id, marker, created_at) in [
        ("m-secret", "secret", "2026-06-28T10:00:00Z"),
        ("m-other-secret", "other-secret", "2026-06-28T10:01:00Z"),
        ("m-open", "open", "2026-06-28T10:02:00Z"),
    ] {
        db.log_correction(&corr_rec(
            "canonical-gate",
            &format!("input-{marker}"),
            &format!("output-{marker}"),
            None,
            true,
            created_at,
            Some(meeting_id),
        ))
        .unwrap();
    }

    db.insert_voiceprint(
        "vp-secret",
        "m-secret",
        1,
        Some("Secret speaker"),
        &[1.0, 0.0],
        "2026-06-28T10:00:00Z",
    )
    .unwrap();
    db.insert_voiceprint(
        "vp-other-secret",
        "m-other-secret",
        2,
        Some("Other secret speaker"),
        &[0.0, 1.0],
        "2026-06-28T10:01:00Z",
    )
    .unwrap();
    db.insert_voiceprint(
        "vp-open",
        "m-open",
        3,
        Some("Open speaker"),
        &[0.5, 0.5],
        "2026-06-28T10:02:00Z",
    )
    .unwrap();

    let none = std::collections::HashSet::new();
    let corrections = db.list_corrections("canonical-gate", 10, &none).unwrap();
    assert_eq!(corrections.len(), 1);
    assert_eq!(corrections[0].meeting_id.as_deref(), Some("m-open"));

    let voiceprints = db.list_voiceprints_visible(&none).unwrap();
    assert_eq!(voiceprints.len(), 1);
    assert_eq!(voiceprints[0].meeting_id, "m-open");
    assert!(db
        .list_visible_speaker_labels_for_meeting("m-secret", &none)
        .unwrap()
        .is_empty());
    assert!(db
        .list_visible_speaker_labels_for_meeting("m-other-secret", &none)
        .unwrap()
        .is_empty());
    let open_labels = db
        .list_visible_speaker_labels_for_meeting("m-open", &none)
        .unwrap();
    assert_eq!(open_labels.len(), 1);
    assert_eq!(open_labels[0].label, "Open speaker");

    let unlocked = std::collections::HashSet::from(["f-secret".to_string()]);
    let corrections = db
        .list_corrections("canonical-gate", 10, &unlocked)
        .unwrap();
    assert_eq!(corrections.len(), 2);
    assert!(corrections
        .iter()
        .any(|row| row.meeting_id.as_deref() == Some("m-secret")));
    assert!(corrections
        .iter()
        .any(|row| row.meeting_id.as_deref() == Some("m-open")));
    assert!(!corrections
        .iter()
        .any(|row| row.meeting_id.as_deref() == Some("m-other-secret")));

    let voiceprints = db.list_voiceprints_visible(&unlocked).unwrap();
    assert_eq!(voiceprints.len(), 2);
    assert!(voiceprints.iter().any(|row| row.meeting_id == "m-secret"));
    assert!(voiceprints.iter().any(|row| row.meeting_id == "m-open"));
    assert!(!voiceprints
        .iter()
        .any(|row| row.meeting_id == "m-other-secret"));

    let secret_labels = db
        .list_visible_speaker_labels_for_meeting("m-secret", &unlocked)
        .unwrap();
    assert_eq!(secret_labels.len(), 1);
    assert_eq!(secret_labels[0].cluster_index, 1);
    assert_eq!(secret_labels[0].label, "Secret speaker");
    assert!(db
        .list_visible_speaker_labels_for_meeting("m-other-secret", &unlocked)
        .unwrap()
        .is_empty());
}

// ── Feature A — note↔note backlinks reader ───────────────────────────────────────────────────

/// PURE parse: bare `[[T]]`, alias `[[T|a]]`→`T`, heading `[[T#h]]`→`T`, dedup first-seen, empty
/// on no match. RED before `wikilink_re`/`extract_wikilink_titles` existed.
#[test]
fn extract_wikilink_titles_covers_forms() {
    assert_eq!(extract_wikilink_titles("see [[Alpha]] here"), vec!["Alpha"]);
    assert_eq!(
        extract_wikilink_titles("[[Alpha|the alias]]"),
        vec!["Alpha"]
    );
    assert_eq!(extract_wikilink_titles("[[Alpha#Heading]]"), vec!["Alpha"]);
    // Duplicates (across forms) dedup, first-seen order preserved.
    assert_eq!(
        extract_wikilink_titles("[[Beta]] then [[Alpha]] then [[Beta|x]]"),
        vec!["Beta", "Alpha"]
    );
    // Surrounding whitespace inside the brackets is trimmed.
    assert_eq!(extract_wikilink_titles("[[  Gamma  ]]"), vec!["Gamma"]);
    // No wikilink → empty.
    assert!(extract_wikilink_titles("plain text, no links").is_empty());
    assert!(extract_wikilink_titles("").is_empty());
}

/// GATE 1 (target). A VISIBLE meeting note genuinely links `[[TargetTitle]]`, but the TARGET note
/// lives in a sealed-and-not-unlocked folder → the reader must return `[]` (never reveal the
/// locked target HAS backlinks). RED against a scan-then-filter impl that resolves the target's
/// title / lists backlinks BEFORE the visibility early-return.
#[test]
fn backlinks_sealed_target_hides_all() {
    let db = mem_db();
    seed_folder(&db, "f-open", "Open");
    seed_folder(&db, "f-locked", "Secret");
    db.set_folder_locked("f-locked", true, Some(b"wrapped"))
        .unwrap();

    // SOURCE: a visible meeting whose note body links to the target's title.
    db.insert_meeting(&sample_meeting("m-src", "2026-06-24T10:00:00Z"))
        .unwrap();
    note_for(&db, "m-src", "claude_code", "recap — see [[TargetTitle]]");
    db.set_note_folder("m-src", Some("f-open")).unwrap();

    // TARGET: a standalone note titled exactly "TargetTitle", in the SEALED folder.
    db.insert_note(
        "n-target",
        "f-locked",
        "target",
        "TargetTitle",
        "the target body",
        1_000,
    )
    .unwrap();

    let nothing = std::collections::HashSet::new();
    let got = db
        .backlinks_for_visible(SourceKind::Note, "n-target", &nothing)
        .unwrap();
    assert!(
        got.is_empty(),
        "sealed target must not reveal it HAS backlinks; got {got:?}"
    );
}

/// GATE 2 (source). A SEALED-and-not-unlocked SOURCE note links `[[VisibleTarget]]`. Querying the
/// visible target's backlinks must NOT include that sealed source. RED against an impl that scans
/// all note bodies ignoring `visibility_clause` on the source side.
#[test]
fn backlinks_sealed_source_never_contributes() {
    let db = mem_db();
    seed_folder(&db, "f-open", "Open");
    seed_folder(&db, "f-locked", "Secret");
    db.set_folder_locked("f-locked", true, Some(b"wrapped"))
        .unwrap();

    // TARGET: a VISIBLE meeting titled "VisibleTarget".
    let mut target = sample_meeting("m-target", "2026-06-24T09:00:00Z");
    target.title = Some("VisibleTarget".to_string());
    db.insert_meeting(&target).unwrap();
    note_for(&db, "m-target", "claude_code", "target note body");
    db.set_note_folder("m-target", Some("f-open")).unwrap();

    // A VISIBLE source that DOES link the target (the true positive that must appear).
    db.insert_note(
        "n-open-src",
        "f-open",
        "open-src",
        "OpenSource",
        "links [[VisibleTarget]] openly",
        2_000,
    )
    .unwrap();

    // A SEALED source that ALSO links the target — must NOT contribute while locked.
    db.insert_note(
        "n-sealed-src",
        "f-locked",
        "sealed-src",
        "SealedSource",
        "secretly links [[VisibleTarget]]",
        3_000,
    )
    .unwrap();

    let nothing = std::collections::HashSet::new();
    let got = db
        .backlinks_for_visible(SourceKind::Meeting, "m-target", &nothing)
        .unwrap();
    let ids: Vec<&str> = got.iter().map(|b| b.id.as_str()).collect();
    assert!(
        ids.contains(&"n-open-src"),
        "the visible source must be present; got {ids:?}"
    );
    assert!(
        !ids.contains(&"n-sealed-src"),
        "sealed source leaked into backlinks; got {ids:?}"
    );
}

/// Session-unlock reverses BOTH gates: the sealed TARGET now yields its backlink, and the sealed
/// SOURCE now contributes. Mirrors the entity/correction unlock-reappearance pattern.
#[test]
fn backlinks_unlock_reverses() {
    let db = mem_db();
    seed_folder(&db, "f-open", "Open");
    seed_folder(&db, "f-locked", "Secret");
    db.set_folder_locked("f-locked", true, Some(b"wrapped"))
        .unwrap();

    // A visible source linking a SEALED target (the gate-1 half).
    db.insert_meeting(&sample_meeting("m-src", "2026-06-24T10:00:00Z"))
        .unwrap();
    note_for(&db, "m-src", "claude_code", "see [[TargetTitle]]");
    db.set_note_folder("m-src", Some("f-open")).unwrap();
    db.insert_note(
        "n-target",
        "f-locked",
        "target",
        "TargetTitle",
        "target body",
        1_000,
    )
    .unwrap();

    // A SEALED source linking a VISIBLE target (the gate-2 half).
    let mut vis_target = sample_meeting("m-vis-target", "2026-06-24T08:00:00Z");
    vis_target.title = Some("VisibleTarget".to_string());
    db.insert_meeting(&vis_target).unwrap();
    note_for(&db, "m-vis-target", "claude_code", "vis target body");
    db.set_note_folder("m-vis-target", Some("f-open")).unwrap();
    db.insert_note(
        "n-sealed-src",
        "f-locked",
        "sealed-src",
        "SealedSource",
        "links [[VisibleTarget]]",
        3_000,
    )
    .unwrap();

    // Locked: sealed target hides all; sealed source absent.
    let nothing = std::collections::HashSet::new();
    assert!(
        db.backlinks_for_visible(SourceKind::Note, "n-target", &nothing)
            .unwrap()
            .is_empty(),
        "sealed target still hidden while locked"
    );
    let vis_locked = db
        .backlinks_for_visible(SourceKind::Meeting, "m-vis-target", &nothing)
        .unwrap();
    assert!(
        !vis_locked.iter().any(|b| b.id == "n-sealed-src"),
        "sealed source must be absent while locked"
    );

    // Session-unlock the sealed folder → BOTH halves flip.
    let mut unlocked = std::collections::HashSet::new();
    unlocked.insert("f-locked".to_string());

    let target_now = db
        .backlinks_for_visible(SourceKind::Note, "n-target", &unlocked)
        .unwrap();
    assert_eq!(
        target_now.iter().map(|b| b.id.as_str()).collect::<Vec<_>>(),
        vec!["m-src"],
        "unlocked target must now surface its backlink"
    );
    assert_eq!(target_now[0].kind, SourceKind::Meeting);

    let vis_now = db
        .backlinks_for_visible(SourceKind::Meeting, "m-vis-target", &unlocked)
        .unwrap();
    assert!(
        vis_now.iter().any(|b| b.id == "n-sealed-src"),
        "unlocked source must now contribute"
    );
}

/// GATE 2 (meeting leg) COMPLETENESS: a meeting with SEVERAL provider notes must surface as a
/// backlink when ANY of its notes links the target — even if the first (newest-ordered) note does
/// NOT. RED against a dedup-before-check impl that keeps only the first provider note per meeting.
#[test]
fn backlinks_meeting_leg_scans_all_provider_notes() {
    let db = mem_db();
    seed_folder(&db, "f-open", "Open");

    // TARGET: a visible standalone note titled exactly "MultiTarget".
    db.insert_note(
        "n-multi-target",
        "f-open",
        "mt",
        "MultiTarget",
        "the target body",
        1_000,
    )
    .unwrap();

    // SOURCE: one meeting, TWO provider notes — only the SECOND provider links the target.
    db.insert_meeting(&sample_meeting("m-multi", "2026-06-24T10:00:00Z"))
        .unwrap();
    note_for(&db, "m-multi", "claude_code", "recap with no link at all");
    note_for(
        &db,
        "m-multi",
        "anthropic",
        "deep dive — see [[MultiTarget]]",
    );
    db.set_note_folder("m-multi", Some("f-open")).unwrap();

    let nothing = std::collections::HashSet::new();
    let got = db
        .backlinks_for_visible(SourceKind::Note, "n-multi-target", &nothing)
        .unwrap();
    let ids: Vec<&str> = got.iter().map(|b| b.id.as_str()).collect();
    assert!(
        ids.contains(&"m-multi"),
        "a meeting whose link lives in a non-first provider note must still surface; got {ids:?}"
    );
}

/// `resolve_wikilink` prefers a standalone NOTE, falls back to a MEETING, and is `None` for a
/// title that matches nothing (or is blank).
#[test]
fn resolve_wikilink_prefers_note_then_meeting_else_none() {
    let db = mem_db();
    seed_folder(&db, "f-open", "Open");

    // A visible standalone note titled "Alpha".
    db.insert_note("n-alpha", "f-open", "alpha", "Alpha", "body", 1_000)
        .unwrap();
    // A visible meeting titled "Beta" (its AI note lives in `notes`, not `documents`).
    let mut beta = sample_meeting("m-beta", "2026-06-24T10:00:00Z");
    beta.title = Some("Beta".to_string());
    db.insert_meeting(&beta).unwrap();
    note_for(&db, "m-beta", "claude_code", "beta note");
    db.set_note_folder("m-beta", Some("f-open")).unwrap();

    let nothing = std::collections::HashSet::new();
    let a = db
        .resolve_wikilink("Alpha", &nothing)
        .unwrap()
        .expect("Alpha resolves");
    assert_eq!(a.kind, "note");
    assert_eq!(a.id, "n-alpha");

    let b = db
        .resolve_wikilink("Beta", &nothing)
        .unwrap()
        .expect("Beta resolves");
    assert_eq!(b.kind, "meeting");
    assert_eq!(b.id, "m-beta");

    assert!(db.resolve_wikilink("Nope", &nothing).unwrap().is_none());
    assert!(db.resolve_wikilink("   ", &nothing).unwrap().is_none());
}

/// The `+ Link` / `[[` picker must not offer never-named notes: a screen of identical "Untitled"
/// rows is useless and a pick can't be a meaningful link target (2026-07-20). RED before the
/// `list_link_candidates_visible` sentinel filter (which listed every untitled note). Complements
/// #417 (which fixed the backlink fan-out) by covering the candidate-picker surface.
#[test]
fn link_candidates_exclude_untitled_notes() {
    let db = mem_db();
    seed_folder(&db, "f-open", "Open");
    db.insert_note("n-real", "f-open", "real", "Design Doc", "body", 2_000)
        .unwrap();
    db.insert_note("n-unnamed", "f-open", "Untitled", "Untitled", "", 3_000)
        .unwrap();
    let nothing = std::collections::HashSet::new();
    let (rows, _total) = db
        .list_link_candidates_visible("", 40, 0, &nothing)
        .unwrap();
    let ids: Vec<&str> = rows.iter().map(|c| c.id.as_str()).collect();
    assert!(
        ids.contains(&"n-real"),
        "a real-titled note is still a candidate; got {ids:?}"
    );
    assert!(
        !ids.contains(&"n-unnamed"),
        "a never-named 'Untitled' note must NOT be offered as a link candidate; got {ids:?}"
    );
}

/// SELF-LINK AVOIDANCE (2026-07-16 companion note): a companion note (`documents` row with a
/// non-null `meeting_id`) whose managed title EQUALS its meeting's title must be EXCLUDED from the
/// note-leg, so `[[Meeting Title]]` resolves to the MEETING (kind='meeting'), never to its own
/// companion note. RED against the pre-fix note-leg-first resolver (which returned the companion
/// note, kind='note', because the note leg matched first). A companion note is STILL a valid
/// target for an UNRELATED title that names it (the exclusion is only the self-title collision).
#[test]
fn resolve_wikilink_companion_note_self_title_resolves_to_meeting() {
    let db = mem_db();
    seed_folder(&db, "f-open", "Open");

    // A meeting titled "Q3 Planning" (its AI note lives in `notes` so it is visible).
    let mut m = sample_meeting("m-q3", "2026-07-16T10:00:00Z");
    m.title = Some("Q3 Planning".to_string());
    db.insert_meeting(&m).unwrap();
    note_for(&db, "m-q3", "claude_code", "ai note body");
    db.set_note_folder("m-q3", Some("f-open")).unwrap();

    // A COMPANION note whose managed title == the meeting title, structurally linked by meeting_id.
    db.insert_note(
        "n-comp",
        "f-open",
        "q3-planning",
        "Q3 Planning",
        "jotted body",
        2_000,
    )
    .unwrap();
    db.set_document_meeting_id("n-comp", "m-q3").unwrap();

    let nothing = std::collections::HashSet::new();
    let t = db
        .resolve_wikilink("Q3 Planning", &nothing)
        .unwrap()
        .expect("[[Q3 Planning]] resolves");
    assert_eq!(
        t.kind, "meeting",
        "[[Meeting Title]] must resolve to the MEETING, never its own companion note"
    );
    assert_eq!(t.id, "m-q3");

    // A companion note is STILL a valid target for a DIFFERENT title that names it.
    db.insert_note("n-other", "f-open", "sidebar", "Sidebar", "x", 3_000)
        .unwrap();
    db.set_document_meeting_id("n-other", "m-q3").unwrap(); // linked, but title != meeting title.
    let o = db
        .resolve_wikilink("Sidebar", &nothing)
        .unwrap()
        .expect("[[Sidebar]] resolves");
    assert_eq!(o.kind, "note");
    assert_eq!(o.id, "n-other");
}

/// STRUCTURED backlink leg (2026-07-16 companion note): a companion note linked by
/// `documents.meeting_id` surfaces under its meeting's "Linked mentions" EVEN WHEN its body/
/// front-matter carries no matching `[[Title]]` string (the string-scan legs would miss it). RED
/// against the pre-fix backlinks (title-scan only). Lock-gated: sealed companion note → absent.
#[test]
fn backlinks_include_meeting_id_linked_companion_note() {
    let db = mem_db();
    seed_folder(&db, "f-open", "Open");

    let mut m = sample_meeting("m-bl", "2026-07-16T09:00:00Z");
    m.title = Some("Weekly Sync".to_string());
    db.insert_meeting(&m).unwrap();
    note_for(&db, "m-bl", "claude_code", "ai note"); // makes the meeting visible.
    db.set_note_folder("m-bl", Some("f-open")).unwrap();

    // A companion note with NO `[[Weekly Sync]]` string anywhere — only the structured meeting_id.
    db.insert_note(
        "n-bl",
        "f-open",
        "weekly-sync",
        "Weekly Sync",
        "just a jot, no wikilink",
        5_000,
    )
    .unwrap();
    db.set_document_meeting_id("n-bl", "m-bl").unwrap();

    let nothing = std::collections::HashSet::new();
    let got = db
        .backlinks_for_visible(SourceKind::Meeting, "m-bl", &nothing)
        .unwrap();
    let ids: Vec<&str> = got.iter().map(|b| b.id.as_str()).collect();
    assert!(
            ids.contains(&"n-bl"),
            "the meeting_id-linked companion note must surface in backlinks even without a [[]] string; got {ids:?}"
        );
    // No duplicate: exactly one entry for the companion note.
    assert_eq!(
        ids.iter().filter(|id| **id == "n-bl").count(),
        1,
        "companion note must appear exactly once (structured leg deduped against the string leg)"
    );
}

/// The structured companion-backlink leg is LOCK-GATED: a companion note in a sealed-not-unlocked
/// folder never surfaces under its meeting's backlinks; a session-unlock reverses it.
#[test]
fn backlinks_companion_note_leg_is_lock_gated() {
    let db = mem_db();
    seed_folder(&db, "f-open", "Open");
    seed_folder(&db, "f-locked", "Secret");
    db.set_folder_locked("f-locked", true, Some(b"wrapped"))
        .unwrap();

    let mut m = sample_meeting("m-g", "2026-07-16T08:00:00Z");
    m.title = Some("Board Prep".to_string());
    db.insert_meeting(&m).unwrap();
    note_for(&db, "m-g", "claude_code", "ai note");
    db.set_note_folder("m-g", Some("f-open")).unwrap();

    // Companion note filed into the LOCKED folder, linked by meeting_id.
    db.insert_note(
        "n-g",
        "f-locked",
        "board-prep",
        "Board Prep",
        "sensitive jot",
        6_000,
    )
    .unwrap();
    db.set_document_meeting_id("n-g", "m-g").unwrap();

    let nothing = std::collections::HashSet::new();
    let sealed = db
        .backlinks_for_visible(SourceKind::Meeting, "m-g", &nothing)
        .unwrap();
    assert!(
        !sealed.iter().any(|b| b.id == "n-g"),
        "a sealed-not-unlocked companion note must NOT surface in backlinks"
    );

    let mut unlocked = std::collections::HashSet::new();
    unlocked.insert("f-locked".to_string());
    let visible = db
        .backlinks_for_visible(SourceKind::Meeting, "m-g", &unlocked)
        .unwrap();
    assert!(
        visible.iter().any(|b| b.id == "n-g"),
        "a session-unlock surfaces the companion note in backlinks"
    );
}

/// The additive `documents.meeting_id` migration + helpers: set/read round-trips, `None` when
/// unset, and one-note-per-meeting lookup by the structured column.
#[test]
fn companion_note_meeting_id_set_and_lookup() {
    let db = mem_db();
    seed_folder(&db, "f-open", "Open");
    db.insert_note("n1", "f-open", "n1", "N1", "body", 1_000)
        .unwrap();
    assert!(db.companion_note_for_meeting("m1").unwrap().is_none());
    db.set_document_meeting_id("n1", "m1").unwrap();
    assert_eq!(
        db.companion_note_for_meeting("m1").unwrap().as_deref(),
        Some("n1")
    );
    // An unrelated meeting id still resolves to None.
    assert!(db.companion_note_for_meeting("m2").unwrap().is_none());
}

/// GATED: a `[[Title]]` pointing at a SEALED-and-not-session-unlocked note resolves to `None`
/// (a wikilink click can never reveal/open locked content); session-unlock reverses it. RED
/// against an ungated title lookup.
#[test]
fn resolve_wikilink_hides_sealed_target() {
    let db = mem_db();
    seed_folder(&db, "f-open", "Open");
    seed_folder(&db, "f-locked", "Secret");
    db.set_folder_locked("f-locked", true, Some(b"wrapped"))
        .unwrap();

    // A sealed note titled "Secret" — plaintext still present, so the gate is the sole suppressor.
    db.insert_note(
        "n-secret",
        "f-locked",
        "secret",
        "Secret",
        "classified",
        1_000,
    )
    .unwrap();

    let nothing = std::collections::HashSet::new();
    assert!(
        db.resolve_wikilink("Secret", &nothing).unwrap().is_none(),
        "a sealed note must not be resolvable/navigable from a wikilink"
    );

    let mut unlocked = std::collections::HashSet::new();
    unlocked.insert("f-locked".to_string());
    let t = db
        .resolve_wikilink("Secret", &unlocked)
        .unwrap()
        .expect("unlocked target resolves");
    assert_eq!(t.kind, "note");
    assert_eq!(t.id, "n-secret");
}

/// SIBLING-GAP FIX (2026-07-15): `resolve_wikilink` gained a THIRD leg over `org_items` — an
/// exact title match on a joined-and-enabled org's Shared Brain content must now resolve,
/// mirroring what the prefix-search picker (`list_link_candidates` folding in
/// `search_org_brain_hits`) already offered as an autocomplete candidate. RED against the
/// pre-fix code (which had only the note+meeting legs and returned `None` here).
#[test]
fn resolve_wikilink_falls_back_to_org_item_exact_title_match() {
    let db = mem_db();
    let org_id = "11111111-1111-4111-8111-111111111111";
    let doc_id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    seed_org_state(&db, org_id);
    let emb = crate::embed::StubEmbedder;
    for (item_id, seq, rev) in [
        ("attacker-max-rev", 99, 99),
        ("authoritative-current", 2, 2),
    ] {
        db.upsert_org_item(
            item_id,
            org_id,
            seq,
            "anna",
            "Nebula Rollout",
            "the nebula rollout plan for q3",
            "2026-07-10T09:00:00Z",
            rev,
            1,
            &sha32(rev as u8),
            None,
            None,
            Some(&emb),
        )
        .unwrap();
        db.set_org_item_document_metadata(item_id, Some(doc_id), "view", Some("owner"))
            .unwrap();
    }
    db.repair_org_reconcile_metadata(
        "authoritative-current",
        org_id,
        1,
        Some(doc_id),
        "view",
        Some("owner"),
        true,
    )
    .unwrap();

    let nothing = std::collections::HashSet::new();
    let t = db
        .resolve_wikilink("Nebula Rollout", &nothing)
        .unwrap()
        .expect("an exact-title org item must resolve");
    assert_eq!(t.kind, "org");
    assert_eq!(
        t.id, "authoritative-current",
        "exact-title resolution must follow the relay-authoritative current head, not max rev"
    );
    let expected_link_id = format!("{org_id}:{doc_id}");
    assert_eq!(t.stable_id.as_deref(), Some(expected_link_id.as_str()));
}

/// Rows ingested before stable document identities existed remain navigable by their immutable feed
/// item identity. They are never promoted into a durable stable-link target: autocomplete/manual
/// graph writes still require an authenticated `(org_id, doc_id)` composite.
#[test]
fn resolve_wikilink_keeps_legacy_org_item_navigable_without_stable_link_identity() {
    let db = mem_db();
    seed_org_state(&db, "org-1");
    db.upsert_org_item(
        "legacy-item",
        "org-1",
        1,
        "anna",
        "Legacy Shared Note",
        "legacy shared body",
        "2026-07-10T09:00:00Z",
        1,
        1,
        &sha32(12),
        None,
        Some("anna"),
        None,
    )
    .unwrap();

    let target = db
        .resolve_wikilink("Legacy Shared Note", &HashSet::new())
        .unwrap()
        .expect("legacy shared title remains readable and navigable");
    assert_eq!(target.kind, "org");
    assert_eq!(target.id, "legacy-item");
    assert_eq!(
        target.stable_id, None,
        "pre-docId rows must never fabricate a durable stable identity"
    );
    assert!(db
        .org_link_doc_id_for_item_visible("legacy-item")
        .unwrap()
        .is_none());
    assert!(
        db.upsert_manual_link("org", "legacy-item", "meeting", "local-anchor")
            .is_err(),
        "a raw legacy item id cannot become an opaque persistent org endpoint"
    );
}

/// NEGATIVE (mirrors `org_tombstone_evicts_from_retrieval_and_viewer` /
/// `org_brain_available_requires_at_least_one_enabled_org`): a TOMBSTONED org item, or one
/// whose org is joined but per-instance DISABLED, must NOT resolve — the new leg must not be any
/// laxer than the existing `get_org_item`/`search_org_brain_hits` gate.
#[test]
fn resolve_wikilink_excludes_tombstoned_and_disabled_org_items() {
    let db = mem_db();
    let org_1 = "11111111-1111-4111-8111-111111111111";
    let org_2 = "22222222-2222-4222-8222-222222222222";
    let doc_1 = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let doc_2 = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    seed_org_state(&db, org_1);
    seed_org_state(&db, org_2);
    let emb = crate::embed::StubEmbedder;
    db.upsert_org_item(
        "it-gone",
        org_1,
        1,
        "anna",
        "Ghost Doc",
        "this item will be tombstoned",
        "2026-07-10T09:00:00Z",
        1,
        1,
        &sha32(10),
        None,
        None,
        Some(&emb),
    )
    .unwrap();
    db.upsert_org_item(
        "it-disabled",
        org_2,
        1,
        "bob",
        "Disabled Org Doc",
        "this org is disabled on this install",
        "2026-07-10T09:00:00Z",
        1,
        1,
        &sha32(11),
        None,
        None,
        Some(&emb),
    )
    .unwrap();
    for (item_id, org_id, doc_id) in [("it-gone", org_1, doc_1), ("it-disabled", org_2, doc_2)] {
        db.set_org_item_document_metadata(item_id, Some(doc_id), "view", Some("owner"))
            .unwrap();
        db.repair_org_reconcile_metadata(
            item_id,
            org_id,
            1,
            Some(doc_id),
            "view",
            Some("owner"),
            true,
        )
        .unwrap();
    }

    let nothing = std::collections::HashSet::new();
    // Sanity: both resolve before we exclude them.
    assert!(db
        .resolve_wikilink("Ghost Doc", &nothing)
        .unwrap()
        .is_some());
    assert!(db
        .resolve_wikilink("Disabled Org Doc", &nothing)
        .unwrap()
        .is_some());

    db.tombstone_org_item("it-gone").unwrap();
    assert!(
        db.resolve_wikilink("Ghost Doc", &nothing)
            .unwrap()
            .is_none(),
        "a tombstoned org item must not resolve as a wikilink target"
    );

    db.set_org_context_enabled(org_2, false).unwrap();
    assert!(
        db.resolve_wikilink("Disabled Org Doc", &nothing)
            .unwrap()
            .is_none(),
        "a per-instance-disabled org's item must not resolve as a wikilink target"
    );
}

/// `list_link_candidates_visible` prefix-filters (case-insensitively) over notes + meetings,
/// notes-first, and an empty prefix returns everything up to the cap (recency order).
#[test]
fn list_link_candidates_prefix_filters_notes_and_meetings() {
    let db = mem_db();
    seed_folder(&db, "f-open", "Open");

    db.insert_note("n-alpha", "f-open", "alpha", "Alpha Project", "body", 1_000)
        .unwrap();
    db.insert_note("n-alt", "f-open", "alt", "Alternate Plan", "body", 2_000)
        .unwrap();
    let mut beta = sample_meeting("m-beta", "2026-06-24T10:00:00Z");
    beta.title = Some("Alpine Standup".to_string());
    db.insert_meeting(&beta).unwrap();
    note_for(&db, "m-beta", "claude_code", "beta note");
    db.set_note_folder("m-beta", Some("f-open")).unwrap();

    let nothing = std::collections::HashSet::new();

    // "Alp" matches "Alpha Project" (note) and "Alpine Standup" (meeting), not "Alternate Plan".
    let (hits, total) = db
        .list_link_candidates_visible("Alp", 10, 0, &nothing)
        .unwrap();
    let titles: Vec<&str> = hits.iter().map(|c| c.title.as_str()).collect();
    assert!(titles.contains(&"Alpha Project"));
    assert!(titles.contains(&"Alpine Standup"));
    assert!(!titles.contains(&"Alternate Plan"));
    // Notes leg is queried first (mirrors `resolve_wikilink`'s note-first preference).
    assert_eq!(hits[0].kind, "note");
    assert_eq!(total, 2, "local total counts exactly the matching rows");

    // Case-insensitive (SQLite LIKE is ASCII-case-insensitive by default, same as the
    // existing `search_snippet`/path LIKE readers in this file).
    let (ci, _) = db
        .list_link_candidates_visible("alp", 10, 0, &nothing)
        .unwrap();
    assert_eq!(ci.len(), hits.len());

    // Empty prefix returns everything (capped), not nothing.
    let (all, _) = db
        .list_link_candidates_visible("", 10, 0, &nothing)
        .unwrap();
    assert!(
        all.len() >= 3,
        "empty prefix should list existing candidates"
    );

    // A cap of 1 returns exactly 1 row (notes leg fills it before the meetings leg runs) —
    // while the reported total still spans BOTH legs (it feeds the org-leg offset math).
    let (capped, capped_total) = db
        .list_link_candidates_visible("Alp", 1, 0, &nothing)
        .unwrap();
    assert_eq!(capped.len(), 1);
    assert_eq!(capped_total, 2);
}

/// PAGINATION: `offset` walks the ONE combined [notes ++ meetings] ordering without
/// duplicating or skipping rows across page boundaries — including the page that straddles
/// the notes→meetings seam — and runs dry with an empty page past the end.
#[test]
fn list_link_candidates_paginates_across_the_notes_meetings_seam() {
    let db = mem_db();
    seed_folder(&db, "f-open", "Open");

    // 3 notes (newest-updated first: n3, n2, n1) + 2 meetings (newest-started first: m2, m1).
    for (id, name, title, at) in [
        ("n1", "one", "Note One", 1_000i64),
        ("n2", "two", "Note Two", 2_000),
        ("n3", "three", "Note Three", 3_000),
    ] {
        db.insert_note(id, "f-open", name, title, "body", at)
            .unwrap();
    }
    for (id, started, title) in [
        ("m1", "2026-06-20T10:00:00Z", "Meeting One"),
        ("m2", "2026-06-24T10:00:00Z", "Meeting Two"),
    ] {
        let mut m = sample_meeting(id, started);
        m.title = Some(title.to_string());
        db.insert_meeting(&m).unwrap();
        note_for(&db, id, "claude_code", "note");
        db.set_note_folder(id, Some("f-open")).unwrap();
    }

    let nothing = std::collections::HashSet::new();
    let page = |offset: i64| {
        db.list_link_candidates_visible("", 2, offset, &nothing)
            .unwrap()
    };

    // Page 0: the two newest notes. Total spans both legs on every page.
    let (p0, total) = page(0);
    assert_eq!(
        p0.iter().map(|c| c.title.as_str()).collect::<Vec<_>>(),
        vec!["Note Three", "Note Two"]
    );
    assert_eq!(total, 5);

    // Page 1 straddles the seam: the last note, then the newest meeting.
    let (p1, _) = page(2);
    assert_eq!(
        p1.iter()
            .map(|c| (c.kind.as_str(), c.title.as_str()))
            .collect::<Vec<_>>(),
        vec![("note", "Note One"), ("meeting", "Meeting Two")]
    );

    // Page 2: entirely inside the meetings leg.
    let (p2, _) = page(4);
    assert_eq!(
        p2.iter().map(|c| c.title.as_str()).collect::<Vec<_>>(),
        vec!["Meeting One"]
    );

    // Past the end: an empty page, never an error (the FE's "has more" probe).
    let (p3, _) = page(5);
    assert!(p3.is_empty());
}

/// GATED: a sealed-and-not-session-unlocked note/meeting never appears as a link candidate,
/// even when its title matches the prefix exactly — session-unlock reverses it. RED against an
/// ungated prefix scan (this is the same discipline `resolve_wikilink_hides_sealed_target`
/// enforces for the exact-title resolver).
#[test]
fn list_link_candidates_hides_sealed_sources() {
    let db = mem_db();
    seed_folder(&db, "f-open", "Open");
    seed_folder(&db, "f-locked", "Secret");
    db.set_folder_locked("f-locked", true, Some(b"wrapped"))
        .unwrap();

    db.insert_note(
        "n-secret",
        "f-locked",
        "secret",
        "Secret Roadmap",
        "classified",
        1_000,
    )
    .unwrap();
    let mut sealed_meeting = sample_meeting("m-secret", "2026-06-24T10:00:00Z");
    sealed_meeting.title = Some("Secret Standup".to_string());
    db.insert_meeting(&sealed_meeting).unwrap();
    note_for(&db, "m-secret", "claude_code", "secret meeting note");
    db.set_note_folder("m-secret", Some("f-locked")).unwrap();

    let nothing = std::collections::HashSet::new();
    let (hidden, hidden_total) = db
        .list_link_candidates_visible("Secret", 10, 0, &nothing)
        .unwrap();
    assert!(
        hidden.is_empty(),
        "sealed-and-not-unlocked note/meeting must not be a link candidate"
    );
    assert_eq!(
        hidden_total, 0,
        "the pagination total must not leak that sealed rows exist"
    );

    let mut unlocked = std::collections::HashSet::new();
    unlocked.insert("f-locked".to_string());
    let (visible, visible_total) = db
        .list_link_candidates_visible("Secret", 10, 0, &unlocked)
        .unwrap();
    assert_eq!(
        visible.len(),
        2,
        "unlocking reveals both the note and the meeting"
    );
    assert_eq!(visible_total, 2);
}

/// DOCUMENTS LEG (2026-07-20 link-documents): `list_link_candidates_visible` now surfaces
/// uploaded `kind='document'` rows too — matched by TITLE when present AND by the `name`
/// (filename) fallback when a document has no `title` — prefix-filtered case-insensitively.
/// RED against the pre-fix two-leg reader (which only ever returned notes + meetings).
#[test]
fn list_link_candidates_returns_visible_documents_by_title_and_name() {
    let db = mem_db();
    seed_folder(&db, "f-open", "Open");

    // A document WITH a title (matched on the title). Documents carry a title only after a
    // rename/ingest, so set it directly here (the test-only raw-UPDATE pattern this file uses).
    db.insert_document(
        "d-titled",
        "f-open",
        "report-final.pdf",
        "body",
        "document",
        1_000,
    )
    .unwrap();
    db.lock()
        .execute(
            "UPDATE documents SET title = 'Quarterly Report' WHERE id='d-titled'",
            [],
        )
        .unwrap();
    // A document with NO title — only a filename (matched on the `name` fallback).
    db.insert_document(
        "d-cv",
        "f-open",
        "Oskar_Orlowski_CV.pdf",
        "body",
        "document",
        2_000,
    )
    .unwrap();

    let nothing = std::collections::HashSet::new();

    // Prefix "Quart" hits the titled doc; not the CV.
    let (by_title, _) = db
        .list_link_candidates_visible("Quart", 10, 0, &nothing)
        .unwrap();
    assert_eq!(by_title.len(), 1);
    assert_eq!(by_title[0].kind, "document");
    assert_eq!(by_title[0].id, "d-titled");
    assert_eq!(by_title[0].title, "Quarterly Report");

    // Prefix "Oskar" hits the CV by its filename (COALESCE(title, name) fallback).
    let (by_name, _) = db
        .list_link_candidates_visible("Oskar", 10, 0, &nothing)
        .unwrap();
    assert_eq!(by_name.len(), 1);
    assert_eq!(by_name[0].kind, "document");
    assert_eq!(by_name[0].id, "d-cv");
    assert_eq!(by_name[0].title, "Oskar_Orlowski_CV.pdf");

    // Case-insensitive (SQLite LIKE is ASCII-case-insensitive), same as the notes/meetings legs.
    let (ci, _) = db
        .list_link_candidates_visible("oskar", 10, 0, &nothing)
        .unwrap();
    assert_eq!(ci.len(), 1);
    assert_eq!(ci[0].id, "d-cv");
}

/// GATED (documents): a sealed-and-not-session-unlocked document is ABSENT from candidates AND
/// must not inflate `local_total` — session-unlock reverses it. Same discipline as
/// `list_link_candidates_hides_sealed_sources` for notes/meetings, now for the documents leg.
/// RED against an ungated documents scan.
#[test]
fn list_link_candidates_hides_sealed_documents() {
    let db = mem_db();
    seed_folder(&db, "f-locked", "Secret");
    db.set_folder_locked("f-locked", true, Some(b"wrapped"))
        .unwrap();

    // A sealed document titled "Secret Dossier" — plaintext still present, so the gate is the
    // sole suppressor.
    db.insert_document(
        "d-secret",
        "f-locked",
        "dossier.pdf",
        "classified",
        "document",
        1_000,
    )
    .unwrap();
    db.lock()
        .execute(
            "UPDATE documents SET title = 'Secret Dossier' WHERE id='d-secret'",
            [],
        )
        .unwrap();

    let nothing = std::collections::HashSet::new();
    let (hidden, hidden_total) = db
        .list_link_candidates_visible("Secret", 10, 0, &nothing)
        .unwrap();
    assert!(
        hidden.is_empty(),
        "a sealed-and-not-unlocked document must not be a link candidate"
    );
    assert_eq!(
        hidden_total, 0,
        "the pagination total must not leak that a sealed document exists"
    );

    let mut unlocked = std::collections::HashSet::new();
    unlocked.insert("f-locked".to_string());
    let (visible, visible_total) = db
        .list_link_candidates_visible("Secret", 10, 0, &unlocked)
        .unwrap();
    assert_eq!(visible.len(), 1, "unlocking reveals the document");
    assert_eq!(visible[0].id, "d-secret");
    assert_eq!(visible_total, 1);
}

/// PAGINATION across the notes → meetings → documents seam: `offset` walks the ONE combined
/// ordering without duplicating or skipping rows, and the documents leg sits AFTER both notes
/// and meetings (so pre-existing notes/meetings page positions are unchanged). RED against the
/// pre-fix reader (documents never appeared, and `local_total` omitted the document count).
#[test]
fn list_link_candidates_paginates_across_the_documents_seam() {
    let db = mem_db();
    seed_folder(&db, "f-open", "Open");

    // 2 notes (newest-updated first: n2, n1) + 1 meeting + 1 document.
    db.insert_note("n1", "f-open", "one", "Note One", "body", 1_000)
        .unwrap();
    db.insert_note("n2", "f-open", "two", "Note Two", "body", 2_000)
        .unwrap();
    let mut m = sample_meeting("m1", "2026-06-20T10:00:00Z");
    m.title = Some("Meeting One".to_string());
    db.insert_meeting(&m).unwrap();
    note_for(&db, "m1", "claude_code", "note");
    db.set_note_folder("m1", Some("f-open")).unwrap();
    db.insert_document("d1", "f-open", "Handbook.pdf", "body", "document", 3_000)
        .unwrap();

    let nothing = std::collections::HashSet::new();
    let page = |offset: i64| {
        db.list_link_candidates_visible("", 2, offset, &nothing)
            .unwrap()
    };

    // The combined stable ordering is [Note Two, Note One] ++ [Meeting One] ++ [Handbook.pdf].
    // Total spans all three legs on every page (it feeds the org-leg offset math downstream).
    let (p0, total) = page(0);
    assert_eq!(
        p0.iter().map(|c| c.title.as_str()).collect::<Vec<_>>(),
        vec!["Note Two", "Note One"]
    );
    assert_eq!(total, 4, "local total = notes + meetings + documents");

    // Page 1 straddles the meetings→documents seam: the meeting, then the document last.
    let (p1, _) = page(2);
    assert_eq!(
        p1.iter()
            .map(|c| (c.kind.as_str(), c.title.as_str()))
            .collect::<Vec<_>>(),
        vec![("meeting", "Meeting One"), ("document", "Handbook.pdf")]
    );

    // Past the end: an empty page, never an error, and no duplicate of the document.
    let (p2, _) = page(4);
    assert!(p2.is_empty());

    // Cross-page dedup + no-gap sanity: the union of all pages is exactly the 4 distinct ids.
    let mut seen: Vec<String> = Vec::new();
    for off in [0, 2, 4] {
        for c in page(off).0 {
            assert!(!seen.contains(&c.id), "no row appears on two pages");
            seen.push(c.id);
        }
    }
    let mut ids = seen.clone();
    ids.sort();
    assert_eq!(ids, vec!["d1", "m1", "n1", "n2"]);
}

/// DOCUMENT LEG (`resolve_wikilink`): a visible `kind='document'` resolves to
/// `WikiTarget{kind:"document"}`; a SEALED document does NOT resolve (falls through to None);
/// and a title shared by a note AND a document prefers the NOTE (note-first ordering). RED
/// against the pre-fix resolver, which had no document leg (a doc-only title returned None).
#[test]
fn resolve_wikilink_resolves_visible_document_and_prefers_note_and_hides_sealed() {
    let db = mem_db();
    seed_folder(&db, "f-open", "Open");
    seed_folder(&db, "f-locked", "Secret");
    db.set_folder_locked("f-locked", true, Some(b"wrapped"))
        .unwrap();

    // A visible document titled "Roadmap" (a doc-only title, no note/meeting names it).
    db.insert_document("d-road", "f-open", "roadmap.pdf", "body", "document", 1_000)
        .unwrap();
    // A sealed document titled "Dossier" — plaintext present, so the gate is the sole suppressor.
    db.insert_document(
        "d-seal",
        "f-locked",
        "dossier.pdf",
        "body",
        "document",
        1_000,
    )
    .unwrap();
    // A note AND a document sharing the title "Shared" — note-first must win.
    db.insert_note("n-shared", "f-open", "shared-note", "Shared", "body", 2_000)
        .unwrap();
    db.insert_document(
        "d-shared",
        "f-open",
        "shared.pdf",
        "body",
        "document",
        2_000,
    )
    .unwrap();
    // Set the document titles directly (the test-only raw-UPDATE pattern this file uses).
    {
        let conn = db.lock();
        conn.execute("UPDATE documents SET title='Roadmap' WHERE id='d-road'", [])
            .unwrap();
        conn.execute("UPDATE documents SET title='Dossier' WHERE id='d-seal'", [])
            .unwrap();
        conn.execute(
            "UPDATE documents SET title='Shared' WHERE id='d-shared'",
            [],
        )
        .unwrap();
    }

    let nothing = std::collections::HashSet::new();

    // Doc-only title resolves to the document.
    let road = db
        .resolve_wikilink("Roadmap", &nothing)
        .unwrap()
        .expect("[[Roadmap]] resolves to the document");
    assert_eq!(road.kind, "document");
    assert_eq!(road.id, "d-road");

    // Case-insensitive fallback also reaches the document.
    let road_ci = db
        .resolve_wikilink("roadmap", &nothing)
        .unwrap()
        .expect("[[roadmap]] resolves case-insensitively");
    assert_eq!(road_ci.kind, "document");
    assert_eq!(road_ci.id, "d-road");

    // Sealed document does NOT resolve (nothing unlocked).
    assert!(
        db.resolve_wikilink("Dossier", &nothing).unwrap().is_none(),
        "a sealed document must not be resolvable from a wikilink"
    );
    // Session-unlock reverses it.
    let mut unlocked = std::collections::HashSet::new();
    unlocked.insert("f-locked".to_string());
    let sealed = db
        .resolve_wikilink("Dossier", &unlocked)
        .unwrap()
        .expect("unlocked document resolves");
    assert_eq!(sealed.kind, "document");
    assert_eq!(sealed.id, "d-seal");

    // A title shared by a note AND a document prefers the NOTE (note leg runs first).
    let shared = db
        .resolve_wikilink("Shared", &nothing)
        .unwrap()
        .expect("[[Shared]] resolves");
    assert_eq!(
        shared.kind, "note",
        "a note+doc title collision prefers the note"
    );
    assert_eq!(shared.id, "n-shared");
}

/// FAIL-CLOSED: a correction row with a NULL `meeting_id` (legacy/unattributed) is never returned
/// by the gated reader, even with nothing locked.
#[test]
fn list_corrections_excludes_null_meeting_id() {
    let db = mem_db();
    db.log_correction(&corr_rec(
        "ner",
        "in-x",
        "out-x",
        None,
        true,
        "2026-06-28T10:00:00Z",
        None,
    ))
    .unwrap();
    let nothing = std::collections::HashSet::new();
    assert!(
        db.list_corrections("ner", 10, &nothing).unwrap().is_empty(),
        "NULL-meeting_id correction must be excluded (fail-closed)"
    );
}

/// PURGE-ON-SEAL: a correction row tied to a meeting in a folder is GONE after the folder is
/// sealed (relock blanker). (RED before the purge: the row survived at rest.)
#[test]
fn correction_log_purged_on_lock() {
    let db = mem_db();
    seed_folder(&db, "f-locked", "Secret");
    db.insert_meeting(&sample_meeting("m1", "2026-06-24T10:00:00Z"))
        .unwrap();
    note_for(&db, "m1", "claude_code", "note");
    db.set_note_folder("m1", Some("f-locked")).unwrap();
    db.log_correction(&corr_rec(
        "ner",
        "in-1",
        "out-1",
        None,
        true,
        "2026-06-28T10:00:00Z",
        Some("m1"),
    ))
    .unwrap();
    assert_eq!(
        correction_count(&db, "m1"),
        1,
        "expected a correction row before seal"
    );

    // Seal: blank the note (content_blob present so blank_sealed_notes_in_folders acts), then run
    // the relock blanker for the folder.
    db.seal_note("m1", "claude_code", b"ciphertext").unwrap();
    let mut folders = std::collections::HashSet::new();
    folders.insert("f-locked".to_string());
    db.blank_sealed_notes_in_folders(&folders).unwrap();

    assert_eq!(
        correction_count(&db, "m1"),
        0,
        "correction_log row must be purged on seal (no flywheel data for sealed meetings)"
    );
}

/// PURGE-ON-DELETE: deleting a meeting also removes its correction-log rows.
#[test]
fn correction_log_purged_on_delete_meeting() {
    let db = mem_db();
    db.insert_meeting(&sample_meeting("m1", "2026-06-24T10:00:00Z"))
        .unwrap();
    db.log_correction(&corr_rec(
        "ner",
        "in-1",
        "out-1",
        None,
        true,
        "2026-06-28T10:00:00Z",
        Some("m1"),
    ))
    .unwrap();
    assert_eq!(correction_count(&db, "m1"), 1);
    db.delete_meeting("m1").unwrap();
    assert_eq!(
        correction_count(&db, "m1"),
        0,
        "delete_meeting must purge the meeting's correction_log rows"
    );
}

fn correction_count(db: &Db, meeting_id: &str) -> i64 {
    db.lock()
        .query_row(
            "SELECT COUNT(*) FROM correction_log WHERE meeting_id = ?1",
            rusqlite::params![meeting_id],
            |r| r.get(0),
        )
        .unwrap()
}

fn interaction_count(db: &Db, meeting_id: &str) -> i64 {
    db.lock()
        .query_row(
            "SELECT COUNT(*) FROM assistant_interactions WHERE meeting_id = ?1",
            rusqlite::params![meeting_id],
            |r| r.get(0),
        )
        .unwrap()
}

/// PERSIST → READ round-trips a voice-assistant interaction (command + answer + citations +
/// status + source_label) for a VISIBLE (unfoldered) meeting.
#[test]
fn assistant_interaction_round_trips_for_visible_meeting() {
    let db = mem_db();
    db.insert_meeting(&sample_meeting("m1", "2026-06-24T10:00:00Z"))
        .unwrap();
    db.insert_assistant_interaction(
        "m1",
        "Klaudku, sprawdź jaka była pogoda",
        "Wczoraj było słonecznie. Zobacz [[Notatka o pogodzie]].",
        &[
            "[[Notatka o pogodzie]]".to_string(),
            "(web) Weather — http://x".to_string(),
        ],
        "ok",
        Some("research"),
        None,
        None,
        "2026-06-24T10:05:00Z",
    )
    .unwrap();

    let nothing = std::collections::HashSet::new();
    let got = db
        .list_assistant_interactions_visible("m1", &nothing)
        .unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].command, "Klaudku, sprawdź jaka była pogoda");
    assert!(got[0].answer.contains("słonecznie"));
    assert_eq!(
        got[0].citations,
        vec![
            "[[Notatka o pogodzie]]".to_string(),
            "(web) Weather — http://x".to_string()
        ]
    );
    assert_eq!(got[0].status, "ok");
    assert_eq!(got[0].source_label.as_deref(), Some("research"));
    assert_eq!(got[0].created_at, "2026-06-24T10:05:00Z");
}

/// GATE: a sealed-and-NOT-session-unlocked meeting's interactions are NEVER returned by the gated
/// read (empty), even if a row exists at rest. RED-able: drop the `meeting_is_visible` guard in
/// `list_assistant_interactions_visible` and this fails (the row leaks through the read).
#[test]
fn assistant_interactions_gated_for_sealed_meeting() {
    let db = mem_db();
    seed_folder(&db, "f-locked", "Secret");
    db.insert_meeting(&sample_meeting("m1", "2026-06-24T10:00:00Z"))
        .unwrap();
    note_for(&db, "m1", "claude_code", "note"); // gives the meeting a note in the folder
    db.set_note_folder("m1", Some("f-locked")).unwrap();
    db.insert_assistant_interaction(
        "m1",
        "secret command",
        "secret answer",
        &[],
        "ok",
        Some("research"),
        None,
        None,
        "2026-06-24T10:05:00Z",
    )
    .unwrap();
    // Seal the folder (visibility_clause keys off folders.locked); session-unlock set is empty.
    db.set_folder_locked("f-locked", true, Some(b"wrapped"))
        .unwrap();

    let empty = std::collections::HashSet::new();
    assert!(
        db.list_assistant_interactions_visible("m1", &empty)
            .unwrap()
            .is_empty(),
        "a sealed-not-unlocked meeting must surface NO interactions through the gated read"
    );
    // …and once the folder is session-unlocked, the row is visible again.
    let mut unlocked = std::collections::HashSet::new();
    unlocked.insert("f-locked".to_string());
    assert_eq!(
        db.list_assistant_interactions_visible("m1", &unlocked)
            .unwrap()
            .len(),
        1,
        "a session-unlocked folder's interactions ARE visible"
    );
}

/// PURGE-ON-SEAL: the interactions of a meeting in a folder are GONE after the seal purge tx
/// (`purge_chunks_for_meetings`, which lock_folder runs). RED-able: drop the
/// `purge_assistant_interactions_tx` call and the row survives at rest in a locked folder.
#[test]
fn assistant_interactions_purged_on_seal() {
    let db = mem_db();
    seed_folder(&db, "f-locked", "Secret");
    db.insert_meeting(&sample_meeting("m1", "2026-06-24T10:00:00Z"))
        .unwrap();
    note_for(&db, "m1", "claude_code", "note");
    db.set_note_folder("m1", Some("f-locked")).unwrap();
    db.insert_assistant_interaction(
        "m1",
        "cmd",
        "answer",
        &[],
        "ok",
        Some("research"),
        None,
        None,
        "2026-06-24T10:05:00Z",
    )
    .unwrap();
    assert_eq!(interaction_count(&db, "m1"), 1, "row present before seal");

    // The seal purge runs in the SAME tx that drops chunks + corrections for the sealed meetings.
    db.purge_chunks_for_meetings(&["m1".to_string()]).unwrap();
    assert_eq!(
        interaction_count(&db, "m1"),
        0,
        "assistant_interactions must be purged on seal (Q&A log dropped on seal by design)"
    );
}

/// PURGE-ON-DELETE: deleting a meeting also removes its interactions (FK ON DELETE CASCADE).
#[test]
fn assistant_interactions_purged_on_delete_meeting() {
    let db = mem_db();
    db.insert_meeting(&sample_meeting("m1", "2026-06-24T10:00:00Z"))
        .unwrap();
    db.insert_assistant_interaction(
        "m1",
        "cmd",
        "answer",
        &[],
        "ok",
        Some("research"),
        None,
        None,
        "2026-06-24T10:05:00Z",
    )
    .unwrap();
    assert_eq!(interaction_count(&db, "m1"), 1);
    db.delete_meeting("m1").unwrap();
    assert_eq!(
        interaction_count(&db, "m1"),
        0,
        "delete_meeting must purge the meeting's assistant_interactions rows"
    );
}

// ── Brain v2 L4 — live_bullets: round-trip + purge on EVERY seal path (the L2 lesson) ──────

fn bullets_row(db: &Db, meeting_id: &str) -> Option<String> {
    db.get_live_bullets(meeting_id).unwrap()
}

#[test]
fn live_bullets_round_trip_upsert_get_clear() {
    let db = mem_db();
    db.insert_meeting(&sample_meeting("m1", "2026-07-10T10:00:00Z"))
        .unwrap();
    assert_eq!(
        bullets_row(&db, "m1"),
        None,
        "no row before the first upsert"
    );
    db.upsert_live_bullets("m1", "- [a]: one", "2026-07-10T10:01:00Z")
        .unwrap();
    assert_eq!(bullets_row(&db, "m1").as_deref(), Some("- [a]: one"));
    // Upsert REPLACES (one row per meeting — the RAM buffer is the accumulator).
    db.upsert_live_bullets("m1", "- [a]: one\n- [b]: two", "2026-07-10T10:02:00Z")
        .unwrap();
    assert_eq!(
        bullets_row(&db, "m1").as_deref(),
        Some("- [a]: one\n- [b]: two")
    );
    db.clear_live_bullets("m1").unwrap();
    assert_eq!(bullets_row(&db, "m1"), None, "cleared (Stop-time consume)");
    db.clear_live_bullets("m1").unwrap(); // idempotent
}

/// PURGE-ON-SEAL (RED-shaped: content seeded, seal, assert blank). Dropping the
/// `purge_live_bullets_tx` call from `purge_chunks_for_meetings` leaves the row at rest in a
/// locked folder and fails this.
#[test]
fn live_bullets_purged_on_seal() {
    let db = mem_db();
    seed_folder(&db, "f-locked", "Secret");
    db.insert_meeting(&sample_meeting("m1", "2026-07-10T10:00:00Z"))
        .unwrap();
    note_for(&db, "m1", "claude_code", "note");
    db.set_note_folder("m1", Some("f-locked")).unwrap();
    db.upsert_live_bullets("m1", "- [deal]: pricing agreed", "2026-07-10T10:05:00Z")
        .unwrap();
    assert!(bullets_row(&db, "m1").is_some(), "row present before seal");

    // The seal purge runs in the SAME tx that drops chunks + interactions for sealed meetings.
    db.purge_chunks_for_meetings(&["m1".to_string()]).unwrap();
    assert_eq!(
        bullets_row(&db, "m1"),
        None,
        "live_bullets must be purged on seal (running notes are derived plaintext)"
    );
}

/// PURGE-ON-RELOCK: the relock re-blank tx (`blank_sealed_notes_in_folders`) drops the row too
/// — a session-unlocked folder that recorded new bullets must not keep them at rest after
/// relock.
#[test]
fn live_bullets_purged_on_relock_reblank() {
    let db = mem_db();
    seed_folder(&db, "f-locked", "Secret");
    db.insert_meeting(&sample_meeting("m1", "2026-07-10T10:00:00Z"))
        .unwrap();
    note_for(&db, "m1", "claude_code", "note");
    db.set_note_folder("m1", Some("f-locked")).unwrap();
    db.upsert_live_bullets("m1", "- [deal]: pricing agreed", "2026-07-10T10:05:00Z")
        .unwrap();

    let mut folders = HashSet::new();
    folders.insert("f-locked".to_string());
    db.blank_sealed_notes_in_folders(&folders).unwrap();
    assert_eq!(
        bullets_row(&db, "m1"),
        None,
        "relock must purge the live-bullets row"
    );
}

/// PURGE-AT-REST (startup reconcile): a crash mid-recording into a since-locked folder leaves
/// the row behind — `reblank_locked_folders_at_rest` must drop it like the Q&A log.
#[test]
fn live_bullets_purged_by_at_rest_reconcile() {
    let db = mem_db();
    seed_folder(&db, "f-locked", "Secret");
    db.insert_meeting(&sample_meeting("m1", "2026-07-10T10:00:00Z"))
        .unwrap();
    note_for(&db, "m1", "claude_code", "note");
    db.set_note_folder("m1", Some("f-locked")).unwrap();
    db.set_folder_locked("f-locked", true, None).unwrap();
    db.upsert_live_bullets("m1", "- [deal]: pricing agreed", "2026-07-10T10:05:00Z")
        .unwrap();

    db.reblank_locked_folders_at_rest().unwrap();
    assert_eq!(
        bullets_row(&db, "m1"),
        None,
        "startup reconciliation must purge a locked folder's live-bullets row"
    );
}

/// TOCTOU (lock-security W2, 2026-07-10 — mirrors
/// `topic_index_refuses_write_when_sealed_at_rest_mid_flight`): a `lock_folder` committing
/// BETWEEN the worker's `current_meeting` check and the row upsert purged the row and sealed
/// the note at rest (markdown blanked, `content_blob` kept) — the upsert's own in-tx
/// sealed-at-rest re-check must then REFUSE to re-write plaintext bullets for the sealed
/// meeting. RED before the fix: the plain upsert wrote the row unconditionally.
#[test]
fn live_bullets_upsert_refuses_when_sealed_at_rest_mid_flight() {
    let db = mem_db();
    seed_folder(&db, "f-locked", "Secret");
    db.insert_meeting(&sample_meeting("m1", "2026-07-10T10:00:00Z"))
        .unwrap();
    note_for(&db, "m1", "claude_code", "note");
    db.set_note_folder("m1", Some("f-locked")).unwrap();
    db.set_folder_locked("f-locked", true, None).unwrap();
    // "lock_folder committed mid-model-call": note sealed at rest — exactly what
    // `blank_sealed_notes_in_folders` leaves behind.
    {
        let conn = db.lock();
        conn.execute(
            "UPDATE notes SET markdown = '', content_blob = X'00' WHERE meeting_id = 'm1'",
            [],
        )
        .unwrap();
    }
    db.upsert_live_bullets("m1", "- [deal]: pricing agreed", "2026-07-10T10:05:00Z")
        .unwrap();
    assert_eq!(
        bullets_row(&db, "m1"),
        None,
        "the in-tx sealed-at-rest re-check must refuse re-upserting plaintext bullets"
    );
}

/// PURGE-ON-DISCARD (lock-security W3, 2026-07-10): `discard_folder_seal` purges every other
/// derived table — the live-bullets row must go with them (contract consistency; RED before
/// `live_bullets` joined the discard purge list).
#[test]
fn live_bullets_purged_on_discard_folder_seal() {
    let db = mem_db();
    seed_folder(&db, "f-locked", "Secret");
    db.insert_meeting(&sample_meeting("m1", "2026-07-10T10:00:00Z"))
        .unwrap();
    note_for(&db, "m1", "claude_code", "note");
    db.set_note_folder("m1", Some("f-locked")).unwrap();
    db.upsert_live_bullets("m1", "- [deal]: pricing agreed", "2026-07-10T10:05:00Z")
        .unwrap();
    db.set_folder_locked("f-locked", true, None).unwrap();

    db.discard_folder_seal("f-locked").unwrap();
    assert_eq!(
        bullets_row(&db, "m1"),
        None,
        "discard_folder_seal must purge the live-bullets row like every other derived table"
    );
}

// ── Brain v2 L5 — PENDING `brief_runs` purge-on-seal (lock-security LEAK fix, 2026-07-10) ────

/// Seed one `brief_runs` row directly. `note_md` carries a RECOGNIZABLE marker so the leak
/// asserts can prove the synthesized content is gone, not just a row id.
fn seed_brief_run(db: &Db, id: &str, status: &str, note_md: &str, meeting_ids: &[&str]) {
    db.insert_brief_run(&crate::storage::models::BriefRun {
        id: id.to_string(),
        schedule_id: "sched1".to_string(),
        status: status.to_string(),
        note_md: note_md.to_string(),
        meeting_ids: meeting_ids.iter().map(|s| s.to_string()).collect(),
        proposed_at: "2026-07-10T09:00:00Z".to_string(),
        accepted_at: None,
    })
    .unwrap();
}

/// Every `brief_runs` (id, note_md) AT REST — the raw table, not the pending-only listing, so
/// a test asserts what actually survives a seal.
fn brief_rows_at_rest(db: &Db) -> Vec<(String, String)> {
    let conn = db.lock();
    let mut stmt = conn
        .prepare("SELECT id, note_md FROM brief_runs ORDER BY id")
        .unwrap();
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .unwrap();
    rows.map(|r| r.unwrap()).collect()
}

/// Seed the leak shape: meeting `m1` (in `f-locked`) + open meeting `m2`; a PENDING run whose
/// synthesis references the soon-sealed `m1`, a PENDING run over only `m2`, and an ACCEPTED
/// (consumed — `note_md` blanked on accept) run referencing `m1`.
fn seed_brief_leak_shape(db: &Db) {
    seed_folder(db, "f-locked", "Secret");
    db.insert_meeting(&sample_meeting("m1", "2026-07-10T10:00:00Z"))
        .unwrap();
    note_for(db, "m1", "claude_code", "note");
    db.set_note_folder("m1", Some("f-locked")).unwrap();
    db.insert_meeting(&sample_meeting("m2", "2026-07-10T11:00:00Z"))
        .unwrap();
    seed_brief_run(
        db,
        "r-leak",
        "pending",
        "SYNTH-LEAK pricing agreed",
        &["m1", "m2"],
    );
    seed_brief_run(db, "r-other", "pending", "other-scope synthesis", &["m2"]);
    seed_brief_run(db, "r-done", "accepted", "", &["m1"]);
}

/// Assert the post-seal shape: the intersecting PENDING run (and its synthesized content) is
/// GONE; the unrelated pending run and the accepted (consumed) run SURVIVE.
fn assert_brief_leak_purged(db: &Db) {
    let rows = brief_rows_at_rest(db);
    assert!(
        !rows.iter().any(|(id, _)| id == "r-leak"),
        "the pending run referencing the sealed meeting must be purged: {rows:?}"
    );
    assert!(
        !rows.iter().any(|(_, md)| md.contains("SYNTH-LEAK")),
        "no synthesized content survives the seal: {rows:?}"
    );
    assert!(
        rows.iter().any(|(id, _)| id == "r-other"),
        "a pending run over other meetings survives: {rows:?}"
    );
    assert!(
        rows.iter().any(|(id, _)| id == "r-done"),
        "an accepted (consumed) run survives: {rows:?}"
    );
}

/// PURGE-ON-SEAL (RED-before-GREEN, the reproduced L5 leak): a PENDING brief run whose
/// `meeting_ids` references a just-sealed meeting paraphrases that meeting's note — the seal
/// tx (`purge_chunks_for_meetings`, which `lock_folder` runs) must delete it, exactly like the
/// memory rollups it mirrors.
#[test]
fn pending_brief_runs_purged_on_seal_accepted_and_unrelated_survive() {
    let db = mem_db();
    seed_brief_leak_shape(&db);
    db.purge_chunks_for_meetings(&["m1".to_string()]).unwrap();
    assert_brief_leak_purged(&db);
}

/// PURGE-ON-RELOCK: the relock re-blank tx (`blank_sealed_notes_in_folders`) drops the
/// intersecting pending run too — a brief proposed during a session-unlock must not keep its
/// synthesis at rest after relock.
#[test]
fn pending_brief_runs_purged_on_relock_reblank() {
    let db = mem_db();
    seed_brief_leak_shape(&db);
    let mut folders = HashSet::new();
    folders.insert("f-locked".to_string());
    db.blank_sealed_notes_in_folders(&folders).unwrap();
    assert_brief_leak_purged(&db);
}

/// PURGE-AT-REST (startup reconcile): a crash after the 09:00 schedule staged a brief over a
/// since-locked folder leaves the row behind — `reblank_locked_folders_at_rest` must drop it
/// like every other derived-plaintext table.
#[test]
fn pending_brief_runs_purged_by_at_rest_reconcile() {
    let db = mem_db();
    seed_brief_leak_shape(&db);
    db.set_folder_locked("f-locked", true, None).unwrap();
    db.reblank_locked_folders_at_rest().unwrap();
    assert_brief_leak_purged(&db);
}

/// PURGE-ON-DELETE: deleting a meeting drops any PENDING brief run referencing it (its
/// `note_md` paraphrases the deleted note); the accepted/unrelated rows survive.
#[test]
fn pending_brief_runs_purged_on_delete_meeting() {
    let db = mem_db();
    seed_brief_leak_shape(&db);
    db.delete_meeting("m1").unwrap();
    assert_brief_leak_purged(&db);
}

// ── 2026-07-10 lock-audit F3/F5: folder-delete purge + the relock-tx derived families ─────────

/// Raw at-rest scalar (COUNT) — what actually survives, independent of any gated read.
fn count_raw(db: &Db, sql: &str) -> i64 {
    let conn = db.lock();
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

/// F3 (RED-before-GREEN): deleting a folder must purge its documents' `doc_chunks` +
/// `doc_vec_chunks` + `fts_doc_chunks` tokens IN the delete tx. Before the fix `delete_folder`
/// was a bare `DELETE FROM folders`: the `documents`→`doc_chunks` FK CASCADE does NOT fire the
/// `fts_doc_chunks_ad` trigger (cascade actions skip triggers without `recursive_triggers`), and
/// `doc_vec_chunks` is a FK-less vec0 table — leaving searchable tokens + invertible vectors of
/// the deleted content at rest.
#[test]
fn delete_folder_purges_doc_vec_chunks_and_fts() {
    let db = mem_db();
    seed_folder(&db, "f-docs", "Research");
    db.insert_document(
        "d1",
        "f-docs",
        "spec.md",
        "unicornbudget approved",
        "document",
        1,
    )
    .unwrap();
    db.index_document_chunks("d1", None).unwrap(); // chunks + FTS (model-less: no vectors)
                                                   // Attach a vector to the chunk directly (no embedder on CI) — the orphan the fix must purge.
    {
        let conn = db.lock();
        let chunk_id: i64 = conn
            .query_row(
                "SELECT id FROM doc_chunks WHERE document_id = 'd1' LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let blob = crate::embed::vec_to_blob(&one_hot(0));
        conn.execute(
            "INSERT INTO doc_vec_chunks(chunk_id, embedding) VALUES (?1, ?2)",
            rusqlite::params![chunk_id, blob],
        )
        .unwrap();
    }

    db.delete_folder("f-docs").unwrap();

    assert_eq!(
        count_raw(
            &db,
            "SELECT COUNT(*) FROM documents WHERE folder_id = 'f-docs'"
        ),
        0,
        "documents gone with the folder"
    );
    assert_eq!(
        count_raw(
            &db,
            "SELECT COUNT(*) FROM doc_chunks WHERE document_id = 'd1'"
        ),
        0,
        "doc_chunks purged with the folder"
    );
    assert_eq!(
        count_raw(&db, "SELECT COUNT(*) FROM doc_vec_chunks"),
        0,
        "no orphan doc vector survives the folder delete (FK-less vec0)"
    );
    assert_eq!(
        count_raw(
            &db,
            "SELECT COUNT(*) FROM fts_doc_chunks WHERE fts_doc_chunks MATCH 'unicornbudget'"
        ),
        0,
        "no searchable FTS token of the deleted content survives (cascade skips the _ad trigger)"
    );
}

/// NIT-3 (RED-before-GREEN — link lifecycle): deleting a folder purges every `links` edge
/// incident on a to-be-deleted DERIVED document, so no link row is left dangling to a row that
/// no longer exists. RED on the pre-fix `delete_folder` (docs + chunks purged, but `links` were
/// not) — the `d1`-incident edges survived the delete.
#[test]
fn delete_folder_purges_links_referencing_deleted_documents() {
    let db = mem_db();
    seed_folder(&db, "f-docs", "Research");
    seed_folder(&db, "f-keep", "Keep");
    db.insert_document("d1", "f-docs", "spec.md", "the spec body", "document", 1)
        .unwrap();
    // A surviving note in ANOTHER folder that links AT the doc, plus a semantic edge between two
    // docs in the deleted folder — both are incident on `d1` and must be purged with it.
    db.insert_document("d2", "f-docs", "notes.md", "adjacent doc", "document", 1)
        .unwrap();
    seed_note_doc(&db, "keeper", "f-keep", "Keeper", "");
    let now = 1_700_000_000_000i64;
    {
        let mut conn = db.lock();
        let tx = conn.transaction().unwrap();
        // keeper (survives) --wikilink--> d1 (deleted): src stays, dst vanishes.
        Db::upsert_link_tx(
            &tx, "note", "keeper", "document", "d1", "wikilink", 1.0, "user", "active", now,
        )
        .unwrap();
        // d1 <--semantic--> d2 (both deleted).
        Db::upsert_link_tx(
            &tx,
            "document",
            "d1",
            "document",
            "d2",
            "semantic",
            0.8,
            "auto",
            "suggested",
            now,
        )
        .unwrap();
        tx.commit().unwrap();
    }
    assert_eq!(
        link_count(&db, "document", "d1", "wikilink"),
        1,
        "edge to d1 exists before delete"
    );
    assert_eq!(
        link_count(&db, "document", "d1", "semantic"),
        1,
        "semantic edge on d1 exists before delete"
    );

    db.delete_folder("f-docs").unwrap();

    assert_eq!(
        count_raw(
            &db,
            "SELECT COUNT(*) FROM links WHERE src_id IN ('d1','d2') OR dst_id IN ('d1','d2')"
        ),
        0,
        "no links row references a deleted document id after delete_folder"
    );
    // The surviving note in the OTHER folder keeps its own row-space clean (its dangling edge gone).
    assert_eq!(
        link_count(&db, "note", "keeper", "wikilink"),
        0,
        "the surviving note's edge to the deleted doc is purged"
    );
}

/// F5 (RED-before-GREEN): the RELOCK tx (`blank_sealed_notes_in_folders`, both the
/// `relock_folder` and `relock_all_inner` legs) must purge the four derived-content families the
/// LOCK tx (`purge_chunks_for_meetings`) and the STARTUP reconcile
/// (`reblank_locked_folders_at_rest`) already purge: `facts`, `user_facts`,
/// `assistant_interactions`, `speaker_voiceprints`. Before the fix, rows derived DURING a
/// session unlock survived the relock at rest.
#[test]
fn relock_purges_facts_user_facts_interactions_and_voiceprints() {
    let db = mem_db();
    seed_folder(&db, "f-locked", "Secret");
    db.insert_meeting(&sample_meeting("m1", "2026-07-10T10:00:00Z"))
        .unwrap();
    note_for(&db, "m1", "claude_code", "note");
    db.set_note_folder("m1", Some("f-locked")).unwrap();

    // Rows re-derived while the folder was session-unlocked (the relock must drop them).
    let anna = db.upsert_entity("Anna", EntityKind::Person).unwrap();
    db.apply_fact_ops(&[crate::facts::FactOp::Add(crate::facts::NewFact {
        entity_id: anna.clone(),
        subject: "Anna".into(),
        predicate: "role".into(),
        object: "QA lead".into(),
        valid_from: "2026-07-10T10:00:00Z".into(),
        recorded_at: "2026-07-10T10:00:00Z".into(),
        confidence: 1.0,
        meeting_id: Some("m1".into()),
    })])
    .unwrap();
    db.apply_user_fact_ops(&[crate::facts::FactOp::Add(crate::facts::NewFact {
        entity_id: "user".into(),
        subject: "user".into(),
        predicate: "prefers".into(),
        object: "Polish replies".into(),
        valid_from: "2026-07-10T10:00:00Z".into(),
        recorded_at: "2026-07-10T10:00:00Z".into(),
        confidence: 1.0,
        meeting_id: Some("m1".into()),
    })])
    .unwrap();
    db.insert_assistant_interaction(
        "m1",
        "what did Anna say",
        "she owns QA sign-off",
        &[],
        "answered",
        None,
        None,
        None,
        "2026-07-10T10:05:00Z",
    )
    .unwrap();
    db.insert_voiceprint(
        "vp1",
        "m1",
        0,
        Some("Anna"),
        &[1.0, 0.0, 0.0],
        "2026-07-10T10:06:00Z",
    )
    .unwrap();

    // RELOCK tx.
    let mut folders = HashSet::new();
    folders.insert("f-locked".to_string());
    db.blank_sealed_notes_in_folders(&folders).unwrap();

    assert_eq!(
        count_raw(&db, "SELECT COUNT(*) FROM facts WHERE meeting_id = 'm1'"),
        0,
        "relock must purge the sealed meeting's facts"
    );
    assert_eq!(
        count_raw(
            &db,
            "SELECT COUNT(*) FROM user_facts WHERE meeting_id = 'm1'"
        ),
        0,
        "relock must purge the sealed meeting's user facts"
    );
    assert_eq!(
        count_raw(
            &db,
            "SELECT COUNT(*) FROM assistant_interactions WHERE meeting_id = 'm1'"
        ),
        0,
        "relock must purge the sealed meeting's Q&A log"
    );
    assert_eq!(
        count_raw(
            &db,
            "SELECT COUNT(*) FROM speaker_voiceprints WHERE meeting_id = 'm1'"
        ),
        0,
        "relock must purge the sealed meeting's voiceprints"
    );
}

/// PURGE-ON-DELETE: deleting a meeting removes its live-bullets row (explicit purge + FK).
#[test]
fn live_bullets_purged_on_delete_meeting() {
    let db = mem_db();
    db.insert_meeting(&sample_meeting("m1", "2026-07-10T10:00:00Z"))
        .unwrap();
    db.upsert_live_bullets("m1", "- [deal]: pricing agreed", "2026-07-10T10:05:00Z")
        .unwrap();
    db.delete_meeting("m1").unwrap();
    assert_eq!(
        bullets_row(&db, "m1"),
        None,
        "delete_meeting must purge the live-bullets row"
    );
}

/// THREAD round-trip: exchanges persisted WITH a thread identity read back through the gated
/// thread reader — `thread_id` + `anchor_text` intact, `command` = the user's LATEST message
/// of each exchange, ordered by id ASC — while legacy rows (NULL `thread_id`) are EXCLUDED.
/// RED before PR D: `insert_assistant_interaction` had no thread params and
/// `list_assistant_threads_visible` did not exist (thread identity was FE-RAM-only).
#[test]
fn assistant_threads_round_trip_ordered_and_exclude_legacy() {
    let db = mem_db();
    db.insert_meeting(&sample_meeting("m1", "2026-07-02T10:00:00Z"))
        .unwrap();
    // A legacy-shaped voice row (no thread) — must NOT surface in the thread reader.
    db.insert_assistant_interaction(
        "m1",
        "legacy voice cmd",
        "a0",
        &[],
        "ok",
        Some("research"),
        None,
        None,
        "2026-07-02T10:01:00Z",
    )
    .unwrap();
    // An @brain thread: two exchanges, the first anchored to a note.
    db.insert_assistant_interaction(
        "m1",
        "what did we decide on pricing?",
        "Tiered pricing.",
        &["[[Pricing sync]]".to_string()],
        "ok",
        Some("research"),
        Some("t-1"),
        Some("• pricing: tiered, ship Friday"),
        "2026-07-02T10:02:00Z",
    )
    .unwrap();
    db.insert_assistant_interaction(
        "m1",
        "and the timeline?",
        "Ships Friday.",
        &[],
        "ok",
        Some("research"),
        Some("t-1"),
        None,
        "2026-07-02T10:03:00Z",
    )
    .unwrap();

    let rows = db
        .list_assistant_threads_visible("m1", &std::collections::HashSet::new())
        .unwrap();
    assert_eq!(
        rows.len(),
        2,
        "only thread-carrying rows; the legacy NULL row is excluded"
    );
    assert_eq!(rows[0].thread_id, "t-1");
    assert_eq!(
        rows[0].anchor_text.as_deref(),
        Some("• pricing: tiered, ship Friday")
    );
    assert_eq!(
        rows[0].command, "what did we decide on pricing?",
        "command is the LATEST user message of the exchange, never the rendered history"
    );
    assert_eq!(rows[0].answer, "Tiered pricing.");
    assert_eq!(rows[0].citations, vec!["[[Pricing sync]]".to_string()]);
    assert_eq!(rows[0].status, "ok");
    assert_eq!(rows[0].created_at, "2026-07-02T10:02:00Z");
    // Ordered by id ASC — the follow-up comes second.
    assert_eq!(rows[1].command, "and the timeline?");
    assert_eq!(rows[1].thread_id, "t-1");
    assert!(rows[1].anchor_text.is_none());
}

/// GATE: a sealed-and-NOT-session-unlocked meeting's threads are NEVER returned by the gated
/// thread reader (EMPTY, never an error) — even with rows at rest; a session-unlocked folder's
/// threads ARE. RED-able: drop the `meeting_is_visible` guard in
/// `list_assistant_threads_visible` and the first assertion fails (the row leaks).
#[test]
fn assistant_threads_gated_for_sealed_meeting() {
    let db = mem_db();
    seed_folder(&db, "f-locked", "Secret");
    db.insert_meeting(&sample_meeting("m1", "2026-07-02T10:00:00Z"))
        .unwrap();
    note_for(&db, "m1", "claude_code", "note");
    db.set_note_folder("m1", Some("f-locked")).unwrap();
    db.insert_assistant_interaction(
        "m1",
        "secret thread question",
        "secret answer",
        &[],
        "ok",
        Some("research"),
        Some("t-secret"),
        Some("secret anchor"),
        "2026-07-02T10:05:00Z",
    )
    .unwrap();
    db.set_folder_locked("f-locked", true, Some(b"wrapped"))
        .unwrap();

    let empty = std::collections::HashSet::new();
    assert!(
        db.list_assistant_threads_visible("m1", &empty)
            .unwrap()
            .is_empty(),
        "a sealed-not-unlocked meeting must surface NO threads through the gated read"
    );
    let mut unlocked = std::collections::HashSet::new();
    unlocked.insert("f-locked".to_string());
    assert_eq!(
        db.list_assistant_threads_visible("m1", &unlocked)
            .unwrap()
            .len(),
        1,
        "a session-unlocked folder's threads ARE visible"
    );
}

/// PURGE-ON-SEAL: thread rows ride `purge_assistant_interactions_tx` (no new purge code) — after
/// the seal purge the raw rows are GONE and the thread reader returns EMPTY even for a session
/// that can see the meeting. RED-able: exclude thread-carrying rows from the purge DELETE and
/// both assertions fail (thread content would survive a seal at rest).
#[test]
fn assistant_threads_purged_on_seal() {
    let db = mem_db();
    seed_folder(&db, "f-locked", "Secret");
    db.insert_meeting(&sample_meeting("m1", "2026-07-02T10:00:00Z"))
        .unwrap();
    note_for(&db, "m1", "claude_code", "note");
    db.set_note_folder("m1", Some("f-locked")).unwrap();
    db.insert_assistant_interaction(
        "m1",
        "thread cmd",
        "thread answer",
        &[],
        "ok",
        Some("research"),
        Some("t-1"),
        Some("anchor"),
        "2026-07-02T10:05:00Z",
    )
    .unwrap();
    assert_eq!(interaction_count(&db, "m1"), 1, "row present before seal");

    // The seal purge runs in the SAME tx that drops chunks + corrections for sealed meetings.
    db.purge_chunks_for_meetings(&["m1".to_string()]).unwrap();
    assert_eq!(
        interaction_count(&db, "m1"),
        0,
        "raw thread rows gone after the seal purge"
    );
    let mut unlocked = std::collections::HashSet::new();
    unlocked.insert("f-locked".to_string());
    assert!(
        db.list_assistant_threads_visible("m1", &unlocked)
            .unwrap()
            .is_empty(),
        "the thread reader has nothing to return after the purge"
    );
}

/// Phase 4 durable thread→meeting binding: a thread row persisted against a PAST meeting
/// (the resolved FE-bound scope, `fe_id.or(current_meeting)` in `run_assistant_query`) is
/// retrievable under THAT meeting and is INVISIBLE under a DIFFERENT (e.g. currently-recording)
/// meeting — proving a bound thread durably answers about its own meeting, not whatever is
/// recording. The binding needs NO schema change: it rides the existing `meeting_id` column
/// (the resolved scope is what `persist_interaction` stores). RED if `run_assistant_query` had
/// kept binding to `current_meeting` while the FE viewed a past meeting.
#[test]
fn thread_binds_to_its_own_meeting_not_a_different_recording() {
    let db = mem_db();
    // The PAST meeting the FE thread is bound to, and a DIFFERENT meeting that is "recording".
    db.insert_meeting(&sample_meeting("m-past", "2026-07-01T09:00:00Z"))
        .unwrap();
    db.insert_meeting(&sample_meeting("m-recording", "2026-07-06T09:00:00Z"))
        .unwrap();
    // Persist the exchange against the RESOLVED scope (the FE-bound past meeting) — this is what
    // `run_assistant_query` now does with
    // `resolve_scope_meeting(Some("m-past"), None, Some("m-recording"))` (Phase 6: fe > focus > recording).
    db.insert_assistant_interaction(
        "m-past",
        "o czym to spotkanie",
        "It was the Q3 budget review.",
        &["[[Budget review]]".to_string()],
        "ok",
        Some("research"),
        Some("t-past"),
        Some("budget review"),
        "2026-07-06T09:05:00Z",
    )
    .unwrap();

    let nothing = std::collections::HashSet::new();
    // The bound past meeting owns the thread.
    let past = db
        .list_assistant_threads_visible("m-past", &nothing)
        .unwrap();
    assert_eq!(
        past.len(),
        1,
        "the bound past meeting must own its thread row"
    );
    assert_eq!(past[0].thread_id, "t-past");
    // The recording meeting must NOT see it — the binding is durable to the past meeting.
    assert!(
        db.list_assistant_threads_visible("m-recording", &nothing)
            .unwrap()
            .is_empty(),
        "a different (recording) meeting must NOT surface the bound thread"
    );
}

/// PR D migration: `assistant_interactions` carries the THREAD columns (`thread_id` +
/// `anchor_text`). RED before the guarded ALTERs land ("no such column"), GREEN after;
/// `migrate_is_idempotent` covers the re-run. Legacy rows read back NULL/NULL.
#[test]
fn assistant_interactions_have_thread_columns() {
    let db = mem_db();
    let n: i64 = db
        .lock()
        .query_row(
            "SELECT COUNT(*) FROM assistant_interactions
                  WHERE thread_id IS NOT NULL OR anchor_text IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        n, 0,
        "fresh table: no thread rows yet, but the columns must exist"
    );
}

#[test]
fn master_paths_round_trip() {
    let db = mem_db();
    db.insert_meeting(&sample_meeting("m1", "2026-06-24T10:00:00Z"))
        .unwrap();
    // Default: both NULL (legacy / non-opted meetings → nothing for the seal lifecycle to do).
    assert_eq!(db.get_meeting_master_paths("m1").unwrap(), (None, None));
    db.set_meeting_mic_master_path("m1", Some("/a/m1.mic.wav"))
        .unwrap();
    db.set_meeting_sys_master_path("m1", Some("/a/m1.sys.wav"))
        .unwrap();
    assert_eq!(
        db.get_meeting_master_paths("m1").unwrap(),
        (
            Some("/a/m1.mic.wav".to_string()),
            Some("/a/m1.sys.wav".to_string())
        )
    );
    // Independent columns: clearing one leaves the other.
    db.set_meeting_mic_master_path("m1", None).unwrap();
    assert_eq!(
        db.get_meeting_master_paths("m1").unwrap(),
        (None, Some("/a/m1.sys.wav".to_string()))
    );
}

#[test]
fn meeting_round_trip_and_lifecycle() {
    let db = mem_db();
    db.insert_meeting(&sample_meeting("m1", "2026-06-24T10:00:00Z"))
        .unwrap();

    let got = db.get_meeting("m1").unwrap().unwrap();
    assert_eq!(got.id, "m1");
    assert_eq!(got.status, MeetingStatus::Draft);
    assert_eq!(got.duration_s, 0);

    db.update_meeting_status("m1", MeetingStatus::Recording)
        .unwrap();
    assert_eq!(
        db.get_meeting("m1").unwrap().unwrap().status,
        MeetingStatus::Recording
    );

    db.set_meeting_title("m1", "Standup").unwrap();
    db.finalize_meeting("m1", "2026-06-24T10:30:00Z", 1800, "/tmp/m1.wav")
        .unwrap();

    let fin = db.get_meeting("m1").unwrap().unwrap();
    assert_eq!(fin.title.as_deref(), Some("Standup"));
    assert_eq!(fin.ended_at.as_deref(), Some("2026-06-24T10:30:00Z"));
    assert_eq!(fin.duration_s, 1800);
    assert_eq!(fin.audio_path.as_deref(), Some("/tmp/m1.wav"));

    assert!(db.get_meeting("nope").unwrap().is_none());
}

#[test]
fn reconcile_stuck_recordings_flips_recording_to_error_and_is_idempotent() {
    let db = mem_db();

    // A ghost: inserted RECORDING and never stopped (crash mid-capture).
    db.insert_meeting(&sample_meeting("ghost", "2026-07-04T09:00:00Z"))
        .unwrap();
    db.update_meeting_status("ghost", MeetingStatus::Recording)
        .unwrap();

    // A healthy, finished meeting that must be left alone.
    db.insert_meeting(&sample_meeting("done", "2026-07-04T10:00:00Z"))
        .unwrap();
    db.update_meeting_status("done", MeetingStatus::Summarized)
        .unwrap();

    // An already-terminal ERROR row must NOT be re-counted (guards against a broad WHERE).
    db.insert_meeting(&sample_meeting("failed", "2026-07-04T11:00:00Z"))
        .unwrap();
    db.update_meeting_status("failed", MeetingStatus::Error)
        .unwrap();

    // First reconcile flips exactly the one stuck RECORDING row to ERROR.
    assert_eq!(db.reconcile_stuck_recordings().unwrap(), 1);
    assert_eq!(
        db.get_meeting("ghost").unwrap().unwrap().status,
        MeetingStatus::Error
    );
    // Non-recording rows are untouched.
    assert_eq!(
        db.get_meeting("done").unwrap().unwrap().status,
        MeetingStatus::Summarized
    );
    assert_eq!(
        db.get_meeting("failed").unwrap().unwrap().status,
        MeetingStatus::Error
    );

    // Idempotent: with no live recording a second call reconciles nothing.
    assert_eq!(db.reconcile_stuck_recordings().unwrap(), 0);
    assert_eq!(
        db.get_meeting("ghost").unwrap().unwrap().status,
        MeetingStatus::Error
    );
}

/// The load-bearing skip invariant the STAGE-2 crash-salvage ordering depends on:
/// `reconcile_stuck_recordings_except(&[claimed])` must LEAVE the claimed ghost in RECORDING
/// (salvage owns its final status), while every OTHER stuck RECORDING ghost still flips to ERROR.
/// Without this skip, reconcile would clobber a claimed row to ERROR in the window before the async
/// salvage worker transitions it — corrupting the salvage.
#[test]
fn reconcile_stuck_recordings_except_leaves_the_claimed_row_recording() {
    let db = mem_db();

    // Two crash ghosts, both stuck in RECORDING.
    db.insert_meeting(&sample_meeting("claimed", "2026-07-04T09:00:00Z"))
        .unwrap();
    db.update_meeting_status("claimed", MeetingStatus::Recording)
        .unwrap();
    db.insert_meeting(&sample_meeting("unclaimed", "2026-07-04T09:05:00Z"))
        .unwrap();
    db.update_meeting_status("unclaimed", MeetingStatus::Recording)
        .unwrap();

    // Reconcile, EXCEPTing the salvage-claimed id: exactly the one un-claimed ghost flips.
    let claimed = vec!["claimed".to_string()];
    assert_eq!(db.reconcile_stuck_recordings_except(&claimed).unwrap(), 1);

    // The claimed row is SKIPPED — still RECORDING, so the async salvage worker owns its fate.
    assert_eq!(
        db.get_meeting("claimed").unwrap().unwrap().status,
        MeetingStatus::Recording,
        "the claimed ghost must stay RECORDING for salvage to own its final status"
    );
    // The un-claimed ghost is reconciled to the terminal ERROR exactly as before.
    assert_eq!(
        db.get_meeting("unclaimed").unwrap().unwrap().status,
        MeetingStatus::Error
    );

    // Idempotent: re-running with the same exclusion reconciles nothing (the claimed row is
    // still RECORDING but skipped, the unclaimed one is already ERROR).
    assert_eq!(db.reconcile_stuck_recordings_except(&claimed).unwrap(), 0);
    assert_eq!(
        db.get_meeting("claimed").unwrap().unwrap().status,
        MeetingStatus::Recording
    );
}

#[test]
fn latest_meeting_orders_by_started_at() {
    let db = mem_db();
    assert!(db.latest_meeting().unwrap().is_none());
    db.insert_meeting(&sample_meeting("old", "2026-06-23T09:00:00Z"))
        .unwrap();
    db.insert_meeting(&sample_meeting("new", "2026-06-24T09:00:00Z"))
        .unwrap();
    assert_eq!(db.latest_meeting().unwrap().unwrap().id, "new");
}

#[test]
fn segments_replace_and_cascade() {
    let db = mem_db();
    db.insert_meeting(&sample_meeting("m1", "2026-06-24T10:00:00Z"))
        .unwrap();
    let segs = vec![
        Segment {
            idx: 0,
            start_s: 0.0,
            end_s: 1.5,
            text: "hello".into(),
            speaker: Some("me".into()),
            confidence: None,
        },
        Segment {
            idx: 1,
            start_s: 1.5,
            end_s: 3.0,
            text: "world".into(),
            speaker: None,
            confidence: None,
        },
    ];
    db.insert_segments("m1", &segs).unwrap();
    // re-insert (same PKs) must not error thanks to INSERT OR REPLACE
    db.insert_segments("m1", &segs).unwrap();

    let conn = db.lock();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM segments WHERE meeting_id = 'm1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 2);
    drop(conn);

    // Speaker attribution round-trips: "me" persists, None reads back as None.
    let read = db.get_segments("m1").unwrap();
    assert_eq!(read[0].speaker.as_deref(), Some("me"));
    assert_eq!(read[1].speaker, None);

    // deleting the meeting cascades to its segments
    db.lock()
        .execute("DELETE FROM meetings WHERE id = 'm1'", [])
        .unwrap();
    let conn = db.lock();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM segments", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
}

/// Salvage-from-disk: a pipeline RE-RUN over the same meeting must not interleave a stale tail
/// from a prior (longer) transcript — `replace_segments` swaps the whole set atomically in one
/// transaction (delete + insert), unlike the keyed `INSERT OR REPLACE` which would have left
/// old idx 2 alive.
#[test]
fn replace_segments_swaps_out_a_stale_tail_atomically() {
    let db = mem_db();
    db.insert_meeting(&sample_meeting("m-replace", "2026-07-16T10:00:00Z"))
        .unwrap();
    let seg = |idx: i64, text: &str| Segment {
        idx,
        start_s: idx as f64,
        end_s: idx as f64 + 1.0,
        text: text.into(),
        speaker: None,
        confidence: None,
    };
    db.insert_segments(
        "m-replace",
        &[seg(0, "old a"), seg(1, "old b"), seg(2, "stale tail")],
    )
    .unwrap();

    // The fresh re-run yields FEWER segments — the stale idx-2 tail must be gone.
    db.replace_segments("m-replace", &[seg(0, "new a"), seg(1, "new b")])
        .unwrap();

    let read = db.get_segments("m-replace").unwrap();
    assert_eq!(read.len(), 2, "the stale higher-idx tail is swapped out");
    assert_eq!(read[0].text, "new a");
    assert_eq!(read[1].text, "new b");
}

/// `transition_meeting_status` is a compare-and-swap: only the caller that observes the
/// expected `from` status wins — the single-flight claim `retry_transcription` relies on.
#[test]
fn transition_meeting_status_is_a_single_flight_claim() {
    let db = mem_db();
    db.insert_meeting(&sample_meeting("m-cas", "2026-07-16T10:00:00Z"))
        .unwrap();
    db.update_meeting_status("m-cas", MeetingStatus::Error)
        .unwrap();

    assert!(
        db.transition_meeting_status("m-cas", MeetingStatus::Error, MeetingStatus::Recording)
            .unwrap(),
        "the first claim (Error → Recording) wins"
    );
    assert!(
        !db.transition_meeting_status("m-cas", MeetingStatus::Error, MeetingStatus::Recording)
            .unwrap(),
        "a second claim loses — the row is no longer in Error"
    );
    assert_eq!(
        db.get_meeting("m-cas").unwrap().unwrap().status,
        MeetingStatus::Recording
    );
    assert!(
        !db.transition_meeting_status("m-none", MeetingStatus::Error, MeetingStatus::Recording)
            .unwrap(),
        "a missing row never transitions"
    );
}

/// Mid-run-relock fail-closed purge: `delete_unsealed_segments` removes ONLY the blob-less
/// plaintext rows a relock's re-blank (guarded on `text_blob IS NOT NULL`) can never cover —
/// a sealed row (blob present) survives byte-untouched.
#[test]
fn delete_unsealed_segments_removes_only_blobless_rows() {
    let db = mem_db();
    db.insert_meeting(&sample_meeting("m-purge", "2026-07-16T10:00:00Z"))
        .unwrap();
    let seg = |idx: i64, text: &str| Segment {
        idx,
        start_s: idx as f64,
        end_s: idx as f64 + 1.0,
        text: text.into(),
        speaker: None,
        confidence: None,
    };
    db.insert_segments(
        "m-purge",
        &[seg(0, "sealed row"), seg(1, "fresh plaintext")],
    )
    .unwrap();
    // Seal row 0 (blob set, text blanked) — the durable copy a relock governs.
    db.seal_segment("m-purge", 0, b"fake-ciphertext").unwrap();

    assert_eq!(db.delete_unsealed_segments("m-purge").unwrap(), 1);

    let raw = db.raw_segments("m-purge").unwrap();
    assert_eq!(raw.len(), 1, "only the unsealed plaintext row was purged");
    assert_eq!(raw[0].idx, 0, "the sealed row survives");
    assert!(raw[0].text_blob.is_some(), "its sealed blob is untouched");
}

/// Tier 3b/A: `segments.confidence` persists through `insert_segments` → `get_segments`. A stored
/// `Some(0.42)` round-trips (within f32 epsilon), a `None` writes NULL and reads back `None`, and
/// the additive column never disturbs the other fields.
#[test]
fn segments_confidence_round_trips() {
    let db = mem_db();
    db.insert_meeting(&sample_meeting("mc", "2026-07-03T10:00:00Z"))
        .unwrap();
    let segs = vec![
        Segment {
            idx: 0,
            start_s: 0.0,
            end_s: 1.0,
            text: "clear speech".into(),
            speaker: Some("me".into()),
            confidence: Some(0.42),
        },
        Segment {
            idx: 1,
            start_s: 1.0,
            end_s: 2.0,
            text: "unknown".into(),
            speaker: Some("others".into()),
            confidence: None,
        },
    ];
    db.insert_segments("mc", &segs).unwrap();
    let read = db.get_segments("mc").unwrap();
    assert_eq!(read.len(), 2);
    let c0 = read[0].confidence.expect("Some(0.42) must round-trip");
    assert!((c0 - 0.42).abs() < 1e-6, "confidence drifted: {c0}");
    assert_eq!(
        read[1].confidence, None,
        "NULL confidence must read back as None"
    );
    // The additive column did not perturb the other fields.
    assert_eq!(read[0].text, "clear speech");
    assert_eq!(read[1].speaker.as_deref(), Some("others"));
}

#[test]
fn note_upsert_get_and_export_path() {
    let db = mem_db();
    db.insert_meeting(&sample_meeting("m1", "2026-06-24T10:00:00Z"))
        .unwrap();

    let mut note = NoteRecord {
        meeting_id: "m1".into(),
        provider_id: "claude_code".into(),
        markdown: "# v1".into(),
        created_at: "2026-06-24T10:31:00Z".into(),
        exported_path: None,
        model_requested: None,
        model_served: None,
        gateway_host: None,
    };
    db.upsert_note(&note).unwrap();

    // upsert overwrites the same (meeting_id, provider_id)
    note.markdown = "# v2".into();
    db.upsert_note(&note).unwrap();
    let got = db.get_note("m1", "claude_code").unwrap().unwrap();
    assert_eq!(got.markdown, "# v2");
    assert!(got.exported_path.is_none());

    db.set_note_exported_path("m1", "claude_code", "/vault/m1.md")
        .unwrap();
    let got = db.get_note("m1", "claude_code").unwrap().unwrap();
    assert_eq!(got.exported_path.as_deref(), Some("/vault/m1.md"));

    assert!(db.get_note("m1", "ollama").unwrap().is_none());
    assert_eq!(db.latest_note().unwrap().unwrap().markdown, "# v2");
}

/// Phase 5 — provenance round-trip: model_requested / model_served / gateway_host are
/// persisted via `upsert_note` and read back correctly by every note-fetch path.
#[test]
fn note_provenance_round_trips() {
    let db = mem_db();
    db.insert_meeting(&sample_meeting("prov1", "2026-06-30T10:00:00Z"))
        .unwrap();

    let note = NoteRecord {
        meeting_id: "prov1".into(),
        provider_id: "gateway".into(),
        markdown: "---\ntitle: T\n---\nBody.".into(),
        created_at: "2026-06-30T10:05:00Z".into(),
        exported_path: None,
        model_requested: Some("gpt-4o".into()),
        model_served: Some("gpt-4o-2024-11-20".into()),
        gateway_host: Some("gw.example.com".into()),
    };
    db.upsert_note(&note).unwrap();

    let got = db.get_note("prov1", "gateway").unwrap().unwrap();
    assert_eq!(got.model_requested.as_deref(), Some("gpt-4o"));
    assert_eq!(got.model_served.as_deref(), Some("gpt-4o-2024-11-20"));
    assert_eq!(got.gateway_host.as_deref(), Some("gw.example.com"));

    let got2 = db.get_latest_note_for_meeting("prov1").unwrap().unwrap();
    assert_eq!(got2.model_served.as_deref(), Some("gpt-4o-2024-11-20"));
}

/// Phase 5 — legacy notes (columns NULL) read back as `None` provenance fields.
#[test]
fn note_provenance_legacy_rows_read_as_none() {
    let db = mem_db();
    db.insert_meeting(&sample_meeting("leg1", "2026-06-30T10:00:00Z"))
        .unwrap();
    // A note with no provenance (as all pre-Phase-5 notes would be after migration).
    db.upsert_note(&NoteRecord {
        meeting_id: "leg1".into(),
        provider_id: "claude_code".into(),
        markdown: "# Legacy note".into(),
        created_at: "2026-06-30T10:01:00Z".into(),
        exported_path: None,
        model_requested: None,
        model_served: None,
        gateway_host: None,
    })
    .unwrap();
    let got = db.get_note("leg1", "claude_code").unwrap().unwrap();
    assert!(
        got.model_requested.is_none(),
        "legacy: model_requested is None"
    );
    assert!(got.model_served.is_none(), "legacy: model_served is None");
    assert!(got.gateway_host.is_none(), "legacy: gateway_host is None");
}

/// Phase 5 — `migrate()` twice is still a no-op (idempotent migration covering the new columns).
/// The existing `migrate_is_idempotent` test covers the full table list; this one checks
/// specifically that the three provenance columns are present after `migrate()` runs once (they
/// would be absent in a real pre-Phase-5 DB and must be added by the first migrate).
#[test]
fn migrate_adds_provenance_columns_to_notes() {
    let db = mem_db(); // mem_db() calls migrate() once during open_with_key
    let conn = db.lock();
    // PRAGMA table_info returns one row per column; verify all three are present.
    let cols: Vec<String> = conn
        .prepare("PRAGMA table_info(notes)")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();
    for col in &["model_requested", "model_served", "gateway_host"] {
        assert!(
            cols.iter().any(|c| c == col),
            "column {col} must be present after migrate(); columns: {cols:?}"
        );
    }
}

#[test]
fn settings_kv_round_trip() {
    let db = mem_db();
    assert!(db.get_setting("vault_path").unwrap().is_none());
    db.set_setting("vault_path", "/vault").unwrap();
    db.set_setting("provider_id", "claude_code").unwrap();
    // overwrite
    db.set_setting("vault_path", "/vault2").unwrap();
    assert_eq!(
        db.get_setting("vault_path").unwrap().as_deref(),
        Some("/vault2")
    );

    // Only the keys THIS test set — migrate() also seeds the Tier 1 semantic-default keys
    // (semantic_search_enabled / semantic_default_v1), which are not this KV test's concern.
    let all: Vec<(String, String)> = db
        .all_settings()
        .unwrap()
        .into_iter()
        .filter(|(k, _)| k == "provider_id" || k == "vault_path")
        .collect();
    assert_eq!(
        all,
        vec![
            ("provider_id".to_string(), "claude_code".to_string()),
            ("vault_path".to_string(), "/vault2".to_string()),
        ]
    );
}

use crate::storage::models::{Folder, NoteFolder, PropertySchemaField};

// ── Feature C — typed note front-matter properties ──────────────────────────────────────────

/// Seed a NOTE folder (`kind='note'`) at `id`/`name`, so `note_folder_by_id` / the schema fns
/// resolve it. Mirrors the command layer's `insert_note_folder`.
fn seed_note_folder(db: &Db, id: &str, name: &str) {
    db.insert_note_folder(
        &NoteFolder {
            id: id.to_string(),
            name: name.to_string(),
            path: format!("Notes/{name}"),
            parent_id: None,
            locked: false,
            unlocked: false,
            is_root: false,
            kind: "note".into(),
        },
        "2026-07-14T00:00:00Z",
    )
    .unwrap();
}

#[test]
fn note_folder_schema_round_trip() {
    let db = mem_db();
    seed_note_folder(&db, "nf1", "Tasks");

    // No schema row yet → empty vec.
    assert!(db.get_note_folder_schema("nf1").unwrap().is_empty());

    let fields = vec![
        PropertySchemaField {
            key: "status".into(),
            kind: PropertyKind::Select,
            options: vec!["Open".into(), "Done".into()],
        },
        PropertySchemaField {
            key: "due".into(),
            kind: PropertyKind::Date,
            options: vec![],
        },
        PropertySchemaField {
            key: "priority".into(),
            kind: PropertyKind::Number,
            options: vec![],
        },
    ];
    db.set_note_folder_schema("nf1", &fields).unwrap();
    let got = db.get_note_folder_schema("nf1").unwrap();
    assert_eq!(got, fields, "set→get must round-trip the schema");

    // ON CONFLICT upsert: a second set REPLACES (never appends/duplicates).
    let replaced = vec![PropertySchemaField {
        key: "owner".into(),
        kind: PropertyKind::Text,
        options: vec![],
    }];
    db.set_note_folder_schema("nf1", &replaced).unwrap();
    let got2 = db.get_note_folder_schema("nf1").unwrap();
    assert_eq!(got2, replaced, "upsert must replace the whole schema");
}

#[test]
fn coerce_property_value_checkbox() {
    // true-ish and false-ish forms coerce; anything else preserved as Text (never dropped).
    for t in ["true", "1", "yes", "TRUE", "Yes"] {
        assert_eq!(
            coerce_property_value(t, PropertyKind::Checkbox, &[]),
            PropertyValue::Checkbox(true),
            "{t} → true"
        );
    }
    for f in ["false", "0", "no", "NO"] {
        assert_eq!(
            coerce_property_value(f, PropertyKind::Checkbox, &[]),
            PropertyValue::Checkbox(false),
            "{f} → false"
        );
    }
    // Malformed bool → preserved as Text, not dropped.
    assert_eq!(
        coerce_property_value("maybe", PropertyKind::Checkbox, &[]),
        PropertyValue::Text("maybe".into())
    );
}

#[test]
fn coerce_property_value_number() {
    assert_eq!(
        coerce_property_value("42", PropertyKind::Number, &[]),
        PropertyValue::Number(42.0)
    );
    assert_eq!(
        coerce_property_value("3.5", PropertyKind::Number, &[]),
        PropertyValue::Number(3.5)
    );
    // Non-numeric → preserved as Text.
    assert_eq!(
        coerce_property_value("high", PropertyKind::Number, &[]),
        PropertyValue::Text("high".into())
    );
    // NaN/inf strings must not become a Number.
    assert_eq!(
        coerce_property_value("NaN", PropertyKind::Number, &[]),
        PropertyValue::Text("NaN".into())
    );
}

#[test]
fn coerce_property_value_date() {
    assert_eq!(
        coerce_property_value("2026-07-14", PropertyKind::Date, &[]),
        PropertyValue::Date("2026-07-14".into())
    );
    assert_eq!(
        coerce_property_value("2026-07-14T09:30:00Z", PropertyKind::Date, &[]),
        PropertyValue::Date("2026-07-14T09:30:00Z".into())
    );
    // Not a date → preserved as Text.
    assert_eq!(
        coerce_property_value("someday", PropertyKind::Date, &[]),
        PropertyValue::Text("someday".into())
    );
    assert_eq!(
        coerce_property_value("2026-13-40", PropertyKind::Date, &[]),
        PropertyValue::Text("2026-13-40".into()),
        "implausible month/day is not a date"
    );
}

#[test]
fn coerce_property_value_select_out_of_options_preserved_as_text() {
    let opts = vec!["Open".to_string(), "Done".to_string()];
    // In-options (case-insensitive) → Select with the CANONICAL option casing.
    assert_eq!(
        coerce_property_value("done", PropertyKind::Select, &opts),
        PropertyValue::Select("Done".into())
    );
    // Out-of-options value is PRESERVED as Text, never dropped.
    assert_eq!(
        coerce_property_value("Blocked", PropertyKind::Select, &opts),
        PropertyValue::Text("Blocked".into())
    );
}

/// GATE (RED against reading text_blob-derived plaintext without the gate): a typed note in a
/// LOCKED note-folder must yield NO rows via `list_notes_visible_typed` before session-unlock,
/// and the REAL typed row once the folder id is in the unlock set.
#[test]
fn list_notes_typed_empty_for_sealed_folder() {
    let db = mem_db();
    seed_note_folder(&db, "nf-lock", "Secret Tasks");
    db.set_note_folder_schema(
        "nf-lock",
        &[PropertySchemaField {
            key: "status".into(),
            kind: PropertyKind::Select,
            options: vec!["Open".into(), "Done".into()],
        }],
    )
    .unwrap();
    // A note whose front-matter carries the typed `status` property.
    db.insert_note(
        "n1",
        "nf-lock",
        "secret-task",
        "Secret Task",
        "---\nstatus: Done\n---\nbody",
        1_000,
    )
    .unwrap();
    // Seal the folder (locked=1). The plaintext `text` column is NOT blanked by this bare
    // set_folder_locked, so the ONLY thing keeping it invisible is the visibility gate.
    db.set_folder_locked("nf-lock", true, Some(b"wrapped"))
        .unwrap();

    let nothing = std::collections::HashSet::new();
    assert!(
        db.list_notes_visible_typed("nf-lock", &nothing)
            .unwrap()
            .is_empty(),
        "sealed-not-unlocked note-folder must yield NO typed rows (gate violation)"
    );

    // Session-unlock → the real typed row (status coerced to Select Done) appears.
    let mut unlocked = std::collections::HashSet::new();
    unlocked.insert("nf-lock".to_string());
    let rows = db.list_notes_visible_typed("nf-lock", &unlocked).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].title, "Secret Task");
    assert_eq!(
        rows[0].values.get("status"),
        Some(&PropertyValue::Select("Done".into())),
        "unlocked row must carry the coerced typed value"
    );
}

fn seed_folder(db: &Db, id: &str, name: &str) {
    db.insert_folder(&Folder {
        id: id.to_string(),
        name: name.to_string(),
        path: name.to_string(),
        parent_id: None,
        locked: false,
        created_at: "2026-06-26T00:00:00Z".to_string(),
    })
    .unwrap();
}

fn note_for(db: &Db, meeting_id: &str, provider_id: &str, markdown: &str) {
    db.upsert_note(&NoteRecord {
        meeting_id: meeting_id.to_string(),
        provider_id: provider_id.to_string(),
        markdown: markdown.to_string(),
        created_at: "2026-06-26T09:05:00Z".to_string(),
        exported_path: Some(format!("/vault/{meeting_id}-{provider_id}.md")),
        model_requested: None,
        model_served: None,
        gateway_host: None,
    })
    .unwrap();
}

fn folder_id_of<'a>(meetings: &'a [Meeting], id: &str) -> &'a Option<String> {
    &meetings
        .iter()
        .find(|m| m.id == id)
        .expect("meeting present")
        .folder_id
}

#[test]
fn meeting_folder_id_surfaces_in_list_and_search() {
    let db = mem_db();
    seed_folder(&db, "f1", "Secret");
    // Meeting WITH a note moved into a folder.
    db.insert_meeting(&sample_meeting("m1", "2026-06-24T10:00:00Z"))
        .unwrap();
    note_for(&db, "m1", "claude_code", "# planning the budget review");
    db.set_meeting_folder("m1", Some("f1")).unwrap();

    // Meeting with a note but NO folder (root) → folder_id None.
    db.insert_meeting(&sample_meeting("m2", "2026-06-24T11:00:00Z"))
        .unwrap();
    note_for(&db, "m2", "claude_code", "# budget at the root level");

    // Meeting with NO note at all → folder_id None (no note rows to derive from).
    db.insert_meeting(&sample_meeting("m3", "2026-06-24T12:00:00Z"))
        .unwrap();

    // list_meetings carries the derived folder_id.
    let listed = db.list_meetings(50).unwrap();
    assert_eq!(folder_id_of(&listed, "m1").as_deref(), Some("f1"));
    assert_eq!(*folder_id_of(&listed, "m2"), None);
    assert_eq!(*folder_id_of(&listed, "m3"), None);

    // get_meeting carries it too.
    assert_eq!(
        db.get_meeting("m1").unwrap().unwrap().folder_id.as_deref(),
        Some("f1")
    );
    assert_eq!(db.get_meeting("m2").unwrap().unwrap().folder_id, None);
    assert_eq!(db.get_meeting("m3").unwrap().unwrap().folder_id, None);

    // search hits carry the derived folder_id on the embedded meeting.
    let hits = db.search("budget", 50).unwrap();
    let h1 = hits.iter().find(|h| h.meeting.id == "m1").expect("m1 hit");
    let h2 = hits.iter().find(|h| h.meeting.id == "m2").expect("m2 hit");
    assert_eq!(h1.meeting.folder_id.as_deref(), Some("f1"));
    assert_eq!(h2.meeting.folder_id, None);
}

#[test]
fn multi_provider_meeting_reports_one_consistent_folder_id() {
    // A meeting re-summarized with TWO providers has two note rows. set_meeting_folder
    // updates BOTH (WHERE meeting_id), so the correlated subselect returns a single,
    // consistent folder regardless of which row it picks.
    let db = mem_db();
    seed_folder(&db, "f1", "Secret");
    db.insert_meeting(&sample_meeting("m1", "2026-06-24T10:00:00Z"))
        .unwrap();
    note_for(&db, "m1", "claude_code", "# claude budget note");
    note_for(&db, "m1", "ollama", "# ollama budget note");
    db.set_meeting_folder("m1", Some("f1")).unwrap();

    // Both provider rows carry the same folder.
    let folders: Vec<Option<String>> = {
        let conn = db.lock();
        let mut stmt = conn
            .prepare("SELECT folder_id FROM notes WHERE meeting_id = 'm1' ORDER BY provider_id")
            .unwrap();
        let rows = stmt
            .query_map([], |r| r.get::<_, Option<String>>(0))
            .unwrap();
        rows.map(|r| r.unwrap()).collect()
    };
    assert_eq!(
        folders,
        vec![Some("f1".to_string()), Some("f1".to_string())]
    );

    // list_meetings reports a single consistent folder_id (LIMIT 1 subselect, no dup rows).
    let listed = db.list_meetings(50).unwrap();
    let m1_rows: Vec<&Meeting> = listed.iter().filter(|m| m.id == "m1").collect();
    assert_eq!(
        m1_rows.len(),
        1,
        "one meeting row despite two provider notes"
    );
    assert_eq!(m1_rows[0].folder_id.as_deref(), Some("f1"));

    // Clearing the folder (move to root) clears it for every provider row.
    db.set_meeting_folder("m1", None).unwrap();
    assert_eq!(db.get_meeting("m1").unwrap().unwrap().folder_id, None);
}

// ── Phase 1: FTS5/BM25 retrieval ──────────────────────────────────────────

/// A normal provider regeneration must not erase one side of an unresolved legacy folder split.
/// Otherwise the next startup migration sees the one surviving provider folder as authoritative
/// and silently files the whole meeting there. Only the explicit filing API may resolve the split.
#[test]
fn provider_upsert_preserves_ambiguous_legacy_folders_across_reopen_until_explicit_filing() {
    const TEST_DEK: &str =
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let path = unique_temp_path("murmur-legacy-provider-folder-split", "sqlite");
    let _ = std::fs::remove_file(&path);

    {
        let db = Db::open_with_key(&path, TEST_DEK).unwrap();
        seed_folder(&db, "folder-a", "Folder A");
        seed_folder(&db, "folder-b", "Folder B");
        db.insert_meeting(&sample_meeting("m1", "2026-06-24T10:00:00Z"))
            .unwrap();
        note_for(&db, "m1", "provider-a", "# original A");
        note_for(&db, "m1", "provider-b", "# original B");
        db.lock()
            .execute(
                "UPDATE notes
                    SET folder_id = CASE provider_id
                        WHEN 'provider-a' THEN 'folder-a'
                        WHEN 'provider-b' THEN 'folder-b'
                    END
                  WHERE meeting_id = 'm1'",
                [],
            )
            .unwrap();

        // Production re-summarization/upsert for one provider. Canonical placement remains NULL
        // because the legacy A/B disagreement has not been explicitly resolved by the user.
        note_for(&db, "m1", "provider-a", "# regenerated A");
        let folders: Vec<Option<String>> = {
            let conn = db.lock();
            let mut stmt = conn
                .prepare(
                    "SELECT folder_id FROM notes
                      WHERE meeting_id = 'm1' ORDER BY provider_id",
                )
                .unwrap();
            stmt.query_map([], |row| row.get(0))
                .unwrap()
                .map(|row| row.unwrap())
                .collect()
        };
        assert_eq!(
            folders,
            vec![Some("folder-a".into()), Some("folder-b".into())],
            "ordinary provider upsert must preserve both legacy ownership witnesses"
        );
    }

    {
        let db = Db::open_with_key(&path, TEST_DEK).unwrap();
        assert_eq!(
            db.get_meeting("m1").unwrap().unwrap().folder_id,
            None,
            "startup migration must not guess a canonical folder after provider regeneration"
        );
        assert_eq!(
            db.folders_for_meeting("m1").unwrap(),
            vec!["folder-a".to_string(), "folder-b".to_string()]
        );
        let unlocked = std::collections::HashSet::from([
            "folder-a".to_string(),
            "folder-b".to_string(),
        ]);
        assert!(
            db.get_note_if_visible("m1", &unlocked).unwrap().is_none(),
            "ambiguous legacy ownership must remain fail-closed even when both folders are open"
        );

        db.set_meeting_folder("m1", Some("folder-a")).unwrap();
        assert_eq!(
            db.get_meeting("m1")
                .unwrap()
                .unwrap()
                .folder_id
                .as_deref(),
            Some("folder-a")
        );
        let synchronized: Vec<Option<String>> = {
            let conn = db.lock();
            let mut stmt = conn
                .prepare(
                    "SELECT folder_id FROM notes
                      WHERE meeting_id = 'm1' ORDER BY provider_id",
                )
                .unwrap();
            stmt.query_map([], |row| row.get(0))
                .unwrap()
                .map(|row| row.unwrap())
                .collect()
        };
        assert_eq!(
            synchronized,
            vec![Some("folder-a".into()), Some("folder-a".into())],
            "explicit filing is the authority that synchronizes provider rows"
        );
        assert!(db
            .get_note_if_visible("m1", &std::collections::HashSet::new())
            .unwrap()
            .is_some());
    }

    let _ = std::fs::remove_file(path);
}

/// RED-on-LIKE / GREEN-on-FTS: a doc containing BOTH terms is returned for either word order.
/// The old `LIKE '%alpha beta%'` only matched the contiguous substring, so "beta alpha" missed
/// it. FTS5 indexes per-token, so order is irrelevant — both queries return the meeting.
#[test]
fn fts_word_order_symmetry() {
    let db = mem_db();
    db.insert_meeting(&sample_meeting("m1", "2026-06-24T10:00:00Z"))
        .unwrap();
    note_for(
        &db,
        "m1",
        "claude_code",
        "the alpha and the beta of the plan",
    );

    let fwd = db.search("alpha beta", 50).unwrap();
    assert!(
        fwd.iter().any(|h| h.meeting.id == "m1"),
        "'alpha beta' must match the note containing both terms"
    );
    let rev = db.search("beta alpha", 50).unwrap();
    assert!(
        rev.iter().any(|h| h.meeting.id == "m1"),
        "'beta alpha' must ALSO match (word order is irrelevant under FTS5) — this is the bug \
             the old LIKE substring search had"
    );
}

/// Punctuation-only / empty queries must not crash the FTS MATCH parser — they yield no hits.
#[test]
fn fts_punctuation_and_empty_queries_are_safe() {
    let db = mem_db();
    db.insert_meeting(&sample_meeting("m1", "2026-06-24T10:00:00Z"))
        .unwrap();
    note_for(&db, "m1", "claude_code", "quarterly planning notes");
    // Operators / punctuation that would be FTS5 syntax if passed raw.
    for q in [
        "",
        "   ",
        "\"",
        "*",
        "AND OR NOT",
        "(",
        ":",
        "^foo",
        "a* b\"c(",
    ] {
        let hits = db.search(q, 50);
        assert!(hits.is_ok(), "query {q:?} must not error the FTS parser");
    }
    // A real term inside punctuation still matches.
    let hits = db.search("(planning)", 50).unwrap();
    assert!(hits.iter().any(|h| h.meeting.id == "m1"));
}

/// Sealed exclusion under FTS: a sealed-and-NOT-session-unlocked meeting does NOT appear in
/// `search_visible` MATCH results, and once its note plaintext is blanked, its tokens are gone
/// from the FTS index (the `_au` trigger purged them on the blanking UPDATE).
#[test]
fn fts_sealed_meeting_excluded_and_tokens_purged() {
    let db = mem_db();
    seed_folder(&db, "f-locked", "Secret");
    db.insert_meeting(&sample_meeting("sealed", "2026-06-24T10:00:00Z"))
        .unwrap();
    note_for(
        &db,
        "sealed",
        "claude_code",
        "ACQUISITION zarządzanie tajemnica",
    );
    db.set_note_folder("sealed", Some("f-locked")).unwrap();

    // Sanity: before sealing, with the folder session-unlocked it IS findable.
    let mut unlocked = std::collections::HashSet::new();
    unlocked.insert("f-locked".to_string());
    let pre = db.search_visible("ACQUISITION", 50, &unlocked).unwrap();
    assert!(pre.iter().any(|h| h.meeting.id == "sealed"));

    // Seal: blank the plaintext markdown (what seal_note does). The `_au` trigger re-syncs FTS.
    db.seal_note("sealed", "claude_code", b"ciphertext-not-real")
        .unwrap();

    // (a) NOT in search_visible when NOT session-unlocked (empty set).
    let nothing = std::collections::HashSet::new();
    let hidden = db.search_visible("ACQUISITION", 50, &nothing).unwrap();
    assert!(
        !hidden.iter().any(|h| h.meeting.id == "sealed"),
        "sealed-not-unlocked meeting leaked into search_visible MATCH results"
    );

    // (b) The token is GONE from the raw FTS note index after blanking (no stale plaintext).
    let raw_match: i64 = {
        let conn = db.lock();
        conn.query_row(
            "SELECT COUNT(*) FROM fts_notes WHERE fts_notes MATCH 'ACQUISITION'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(
        raw_match, 0,
        "sealed note's tokens must be purged from the FTS index after blanking"
    );

    // (c) Even with the folder session-unlocked, the now-blanked plaintext is no longer in the
    // index, so the term doesn't match — consistent with the at-rest sealed state (the real
    // content only returns after unlock decrypts content_blob back into markdown, which
    // re-fires the FTS trigger).
    let post = db.search_visible("ACQUISITION", 50, &unlocked).unwrap();
    assert!(!post.iter().any(|h| h.meeting.id == "sealed"));
}

/// Lock-GATE (not token-purge) closes the FTS title path. A meeting's TITLE is NOT blanked on
/// seal — the real title stays plaintext-at-rest in `meetings.title` — so its title token
/// survives in `fts_meetings` and still produces an FTS candidate after sealing. The
/// `search_visible` visibility predicate, not the `_au` trigger, is what must exclude a
/// sealed-and-not-session-unlocked meeting matched by its title. (Closes the coverage gap the
/// lock-security review flagged: every prior sealed-exclusion test matched via the note branch
/// and/or relied on token-purge with a never-actually-locked folder.)
#[test]
fn fts_sealed_meeting_title_match_excluded_by_gate() {
    let db = mem_db();
    let kek = [7u8; 32];
    seed_folder(&db, "f-locked", "Secret");
    db.insert_meeting(&Meeting {
        id: "titled".to_string(),
        started_at: "2026-06-24T10:00:00Z".to_string(),
        ended_at: None,
        title: Some("Zebra Quarterly Sync".to_string()),
        duration_s: 60,
        audio_path: None,
        status: MeetingStatus::Summarized,
        folder_id: None,
    })
    .unwrap();
    note_for(&db, "titled", "claude_code", "innocuous body text");
    db.set_note_folder("titled", Some("f-locked")).unwrap();

    // Sanity (folder not yet locked): the title token is findable with an empty unlock set.
    let nothing = std::collections::HashSet::new();
    assert!(
        db.search_visible("Zebra", 50, &nothing)
            .unwrap()
            .iter()
            .any(|h| h.meeting.id == "titled"),
        "title token should be findable before the folder is locked"
    );

    // Lock the folder (locked=1) + seal the note (blank markdown). The TITLE stays plaintext.
    let ck = crate::crypto::random_key().unwrap();
    let wrapped = crate::crypto::encrypt(&kek, &ck, b"").unwrap();
    db.set_folder_locked("f-locked", true, Some(&wrapped))
        .unwrap();
    let blob = crate::crypto::encrypt(&ck, b"innocuous body text", b"").unwrap();
    db.seal_note("titled", "claude_code", &blob).unwrap();

    // The title token IS still present in the raw FTS meetings index (titles are not blanked) —
    // so exclusion CANNOT come from token-purge here; it must come from the visibility gate.
    let title_in_index: i64 = {
        let conn = db.lock();
        conn.query_row(
            "SELECT COUNT(*) FROM fts_meetings WHERE fts_meetings MATCH 'Zebra'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(
            title_in_index, 1,
            "sealed meeting's plaintext title token must remain in the FTS index (titles aren't blanked)"
        );

    // GATE must exclude it with an empty unlock set, despite the surviving title token.
    let hidden = db.search_visible("Zebra", 50, &nothing).unwrap();
    assert!(
        !hidden.iter().any(|h| h.meeting.id == "titled"),
        "sealed-not-unlocked meeting leaked via its plaintext TITLE token through the FTS gate"
    );

    // Session-unlocking the folder admits it again (title still matches).
    let mut unlocked = std::collections::HashSet::new();
    unlocked.insert("f-locked".to_string());
    let shown = db.search_visible("Zebra", 50, &unlocked).unwrap();
    assert!(
        shown.iter().any(|h| h.meeting.id == "titled"),
        "session-unlocked sealed meeting should be findable by its title again"
    );
}

// ── Phase 2a: vector retrieval (note_chunks + vec0) ───────────────────────

/// Insert a controlled `note_chunks` + `vec_chunks` pair directly (bypassing the embedder) so
/// ordering tests can use known vectors. Returns the new chunk_id.
fn insert_known_chunk(db: &Db, meeting_id: &str, text: &str, vector: &[f32]) -> i64 {
    let conn = db.lock();
    conn.execute(
        "INSERT INTO note_chunks (meeting_id, provider_id, chunk_idx, source_type, text)
             VALUES (?1, 'claude_code', 0, 'voice', ?2)",
        rusqlite::params![meeting_id, text],
    )
    .unwrap();
    let chunk_id = conn.last_insert_rowid();
    let blob = crate::embed::vec_to_blob(vector);
    conn.execute(
        "INSERT INTO vec_chunks(chunk_id, embedding) VALUES (?1, ?2)",
        rusqlite::params![chunk_id, blob],
    )
    .unwrap();
    chunk_id
}

/// A one-hot EMBED_DIM vector with `1.0` at `dim` (controlled distinct directions for KNN).
fn one_hot(dim: usize) -> Vec<f32> {
    let mut v = vec![0f32; crate::embed::EMBED_DIM];
    v[dim] = 1.0;
    v
}

/// KNN ordering: the chunk whose vector equals the query is nearest; a near-aligned one is
/// next; an orthogonal one is farthest.
#[test]
fn vec_knn_orders_nearest_first() {
    let db = mem_db();
    db.insert_meeting(&sample_meeting("m-near", "2026-06-24T10:00:00Z"))
        .unwrap();
    db.insert_meeting(&sample_meeting("m-mid", "2026-06-24T11:00:00Z"))
        .unwrap();
    db.insert_meeting(&sample_meeting("m-far", "2026-06-24T12:00:00Z"))
        .unwrap();
    // Every meeting needs a (visible, open-folder) note so the gate admits it.
    note_for(&db, "m-near", "claude_code", "near");
    note_for(&db, "m-mid", "claude_code", "mid");
    note_for(&db, "m-far", "claude_code", "far");

    // query == one_hot(0). near = one_hot(0) (identical), mid = mix of dim 0+1, far = one_hot(2).
    let query = one_hot(0);
    insert_known_chunk(&db, "m-near", "near", &one_hot(0));
    let mut mid = vec![0f32; crate::embed::EMBED_DIM];
    mid[0] = 0.9;
    mid[1] = 0.1;
    insert_known_chunk(&db, "m-mid", "mid", &mid);
    insert_known_chunk(&db, "m-far", "far", &one_hot(2));

    let nothing = std::collections::HashSet::new();
    let hits = db
        .search_semantic_visible(&query, 3, 0.0, &nothing)
        .unwrap();
    let order: Vec<&str> = hits.iter().map(|h| h.meeting.id.as_str()).collect();
    assert_eq!(
        order,
        vec!["m-near", "m-mid", "m-far"],
        "KNN must return nearest-first"
    );
    assert!(hits.iter().all(|h| h.matched_in == "semantic"));
}

/// GATE: a sealed-and-not-session-unlocked meeting is ABSENT from semantic results with an
/// empty unlock set, and PRESENT when its folder id is in the set. (Mirrors the FTS gate test;
/// here the chunk row deliberately still EXISTS so exclusion comes from the gate, not purge.)
#[test]
fn vec_semantic_search_is_gated_by_visibility() {
    let db = mem_db();
    seed_folder(&db, "f-locked", "Secret");
    db.insert_meeting(&sample_meeting("sealed", "2026-06-24T10:00:00Z"))
        .unwrap();
    note_for(&db, "sealed", "claude_code", "secret body");
    db.set_note_folder("sealed", Some("f-locked")).unwrap();
    insert_known_chunk(&db, "sealed", "secret body", &one_hot(0));
    // Flip the folder to locked=1 (visibility_clause keys off folders.locked). The chunk row
    // still exists — so exclusion can ONLY come from the gate here.
    db.set_folder_locked("f-locked", true, None).unwrap();

    let query = one_hot(0);
    // Empty unlock set → excluded.
    let nothing = std::collections::HashSet::new();
    let hidden = db
        .search_semantic_visible(&query, 10, 0.0, &nothing)
        .unwrap();
    assert!(
        !hidden.iter().any(|h| h.meeting.id == "sealed"),
        "sealed-not-unlocked meeting leaked through the semantic gate"
    );
    // Folder session-unlocked → present.
    let mut unlocked = std::collections::HashSet::new();
    unlocked.insert("f-locked".to_string());
    let shown = db
        .search_semantic_visible(&query, 10, 0.0, &unlocked)
        .unwrap();
    assert!(
        shown.iter().any(|h| h.meeting.id == "sealed"),
        "session-unlocked meeting must reappear in semantic results"
    );
}

/// GATE (hybrid): the fused FTS+semantic+graph reader also excludes a sealed-not-session-unlocked
/// meeting — with BOTH the FTS query term AND a query vector that match its (deliberately-surviving)
/// chunk, so exclusion can ONLY come from the shared `visibility_clause`, not purge. Companion to
/// `vec_semantic_search_is_gated_by_visibility`; asserts the unlock re-index change did not weaken
/// the hybrid gate. RED if the gate inside `search_semantic_visible`/`search_visible` were removed.
#[test]
fn vec_hybrid_search_is_gated_by_visibility() {
    let db = mem_db();
    seed_folder(&db, "f-locked", "Secret");
    db.insert_meeting(&sample_meeting("sealed", "2026-06-24T10:00:00Z"))
        .unwrap();
    note_for(&db, "sealed", "claude_code", "quarterly budget secret body");
    db.set_note_folder("sealed", Some("f-locked")).unwrap();
    insert_known_chunk(&db, "sealed", "quarterly budget secret body", &one_hot(0));
    db.set_folder_locked("f-locked", true, None).unwrap();

    let query_vec = one_hot(0);
    // Empty unlock set → the sealed meeting is absent through the whole fused reader.
    let nothing = std::collections::HashSet::new();
    let hidden = db
        .search_hybrid_visible("budget", &query_vec, 10, 0.0, &nothing, None)
        .unwrap();
    assert!(
        !hidden.iter().any(|h| h.meeting.id == "sealed"),
        "sealed-not-unlocked meeting leaked through the hybrid gate"
    );

    // Session-unlock → it reappears in the fused results.
    let mut unlocked = std::collections::HashSet::new();
    unlocked.insert("f-locked".to_string());
    let shown = db
        .search_hybrid_visible("budget", &query_vec, 10, 0.0, &unlocked, None)
        .unwrap();
    assert!(
        shown.iter().any(|h| h.meeting.id == "sealed"),
        "session-unlocked meeting must reappear in hybrid results"
    );
}

// ── document ingestion (documents + doc_chunks + doc_vec0) ────────────────

/// IMPORT/INDEX round-trip at the DB layer: an inserted document chunks + embeds (via the stub so
/// vectors are deterministic), the metadata read returns it (no text), and the full-text read
/// returns the plaintext. Purging the doc removes its chunks AND vectors (1:1).
#[test]
fn document_index_and_purge_round_trip() {
    let db = mem_db();
    seed_folder(&db, "f-open", "Project");
    db.insert_document(
        "d1",
        "f-open",
        "spec.md",
        "budget planning for the quarter",
        "document",
        100,
    )
    .unwrap();
    db.index_document_chunks("d1", Some(&crate::embed::StubEmbedder))
        .unwrap();

    // Metadata (no text) + full text read back.
    let listed = db.documents_in_folder("f-open").unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, "d1");
    assert_eq!(listed[0].name, "spec.md");
    let (folder, name, text) = db.get_document("d1").unwrap().unwrap();
    assert_eq!(folder, "f-open");
    assert_eq!(name, "spec.md");
    assert_eq!(text, "budget planning for the quarter");

    // Chunks + vectors exist. Brain v3 PR-2 HIERARCHY: L1 section-parents are FTS-only (NOT
    // embedded), so vectors are 1:1 with the EMBED-WORTHY rows (L0 leaves + L2 summary), NOT with
    // ALL doc_chunks. Assert exactly that: every L0/L2 row has a vec0 row, no L1 row does.
    let count = |sql: &str| -> i64 { db.lock().query_row(sql, [], |r| r.get(0)).unwrap() };
    let chunks = count("SELECT COUNT(*) FROM doc_chunks WHERE document_id = 'd1'");
    let vecs = count(
        "SELECT COUNT(*) FROM doc_vec_chunks WHERE chunk_id IN \
               (SELECT id FROM doc_chunks WHERE document_id = 'd1')",
    );
    let embed_worthy =
        count("SELECT COUNT(*) FROM doc_chunks WHERE document_id = 'd1' AND level IN (0, 2)");
    let l1 = count("SELECT COUNT(*) FROM doc_chunks WHERE document_id = 'd1' AND level = 1");
    assert!(chunks >= 1, "document must be chunked");
    assert!(l1 >= 1, "a section-parent (L1) must exist");
    assert_eq!(
        vecs, embed_worthy,
        "vectors are 1:1 with the EMBED-worthy L0+L2 rows"
    );
    assert_eq!(
        count(
            "SELECT COUNT(*) FROM doc_vec_chunks WHERE chunk_id IN \
                   (SELECT id FROM doc_chunks WHERE document_id = 'd1' AND level = 1)"
        ),
        0,
        "L1 section-parents are NEVER embedded (no vec0 row)"
    );

    // Purge drops BOTH chunks and vectors.
    db.purge_doc_chunks_for_documents(&["d1".to_string()])
        .unwrap();
    assert_eq!(
        count("SELECT COUNT(*) FROM doc_chunks WHERE document_id = 'd1'"),
        0
    );
    assert_eq!(
        count("SELECT COUNT(*) FROM doc_vec_chunks"),
        0,
        "vectors purged with chunks"
    );
    // The document row + its plaintext survive the chunk purge (re-embeddable).
    assert_eq!(
        db.get_document("d1").unwrap().unwrap().2,
        "budget planning for the quarter"
    );
}

/// Brain v3 PR-2 — HIERARCHY persists: an imported document (stored via `blocks_to_stored_text`
/// so its page/heading structure survives) indexes into L0 leaves (embedded), L1 section-parents
/// (FTS-only, page_no + section_path set), and an L2 summary (embedded). Purge drops ALL levels +
/// their vec0 rows.
#[test]
fn hierarchical_index_persists_levels_and_purges_all() {
    let db = mem_db();
    seed_folder(&db, "f-open", "Project");
    let blocks = vec![
        crate::extract::ExtractedBlock {
            text: "The budget is 100k for the quarter.".into(),
            page: Some(1),
            heading_path: Some("Design".into()),
        },
        crate::extract::ExtractedBlock {
            text: "Anna owns delivery of the API layer.".into(),
            page: Some(2),
            heading_path: Some("Design › Storage".into()),
        },
    ];
    let stored = crate::extract::blocks_to_stored_text(&blocks);
    db.insert_document("d1", "f-open", "spec.pdf", &stored, "document", 100)
        .unwrap();
    db.index_document_chunks("d1", Some(&crate::embed::StubEmbedder))
        .unwrap();

    // (level, parent_id, section_path, page_no) per doc_chunks row.
    type DocChunkMetaRow = (i64, Option<i64>, Option<String>, Option<i64>);
    let rows: Vec<DocChunkMetaRow> = {
        let conn = db.lock();
        let mut stmt = conn
            .prepare(
                "SELECT level, parent_id, section_path, page_no FROM doc_chunks \
                       WHERE document_id = 'd1' ORDER BY chunk_index",
            )
            .unwrap();
        let mapped = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        mapped
    };
    let l0 = rows.iter().filter(|r| r.0 == 0).count();
    let l1 = rows.iter().filter(|r| r.0 == 1).count();
    let l2 = rows.iter().filter(|r| r.0 == 2).count();
    assert_eq!(l1, 2, "two heading sections → two L1 parents");
    assert!(l0 >= 2, "leaves for each section");
    assert!(l2 >= 1, "an L2 summary");
    // Every L0 leaf points at an L1 parent; L1/L2 have no parent.
    assert!(
        rows.iter().filter(|r| r.0 == 0).all(|r| r.1.is_some()),
        "every leaf has a parent_id"
    );
    assert!(
        rows.iter().filter(|r| r.0 != 0).all(|r| r.1.is_none()),
        "L1/L2 rows have no parent"
    );
    // Section path + page carried on the leaves.
    assert!(rows
        .iter()
        .any(|r| r.2.as_deref() == Some("Design › Storage")));
    assert!(rows.iter().any(|r| r.3 == Some(2)));

    // Vectors: exactly the L0+L2 rows (never L1).
    let vec_count = |sql: &str| -> i64 { db.lock().query_row(sql, [], |r| r.get(0)).unwrap() };
    assert_eq!(
        vec_count(
            "SELECT COUNT(*) FROM doc_vec_chunks WHERE chunk_id IN \
                   (SELECT id FROM doc_chunks WHERE document_id='d1' AND level IN (0,2))"
        ),
        (l0 + l2) as i64
    );
    assert_eq!(
        vec_count(
            "SELECT COUNT(*) FROM doc_vec_chunks WHERE chunk_id IN \
                   (SELECT id FROM doc_chunks WHERE document_id='d1' AND level=1)"
        ),
        0,
        "L1 parents are never embedded"
    );

    // Purge removes ALL levels + all vec rows.
    db.purge_doc_chunks_for_documents(&["d1".to_string()])
        .unwrap();
    assert_eq!(db.doc_chunk_count("d1").unwrap(), 0);
    assert_eq!(vec_count("SELECT COUNT(*) FROM doc_vec_chunks"), 0);
}

/// Audit Fix 1 test fixture — one document with a DOMINANT "Alpha" section (3 leaves of plain
/// filler) and a SMALLER "Beta" section (2 leaves; "pistachio" appears mid-paragraph in both —
/// past the first sentence, so the L2 outline never matches it; "zloty" appears in the second
/// Beta leaf ONLY). The pre-fix dominant-by-leaf-count expansion always served Alpha.
fn seed_two_section_doc(db: &Db, doc_id: &str, folder_id: &str) {
    let alpha_para = "Alpha filler sentence with plain ordinary words in it. ".repeat(13);
    let beta_one = format!(
        "This beta paragraph opens with a plain first sentence. \
             The pistachio pistachio pistachio budget appears in the middle here. {}",
        "Beta filler words about the plan. ".repeat(16)
    );
    let beta_two = format!(
        "Another beta paragraph opens plainly as well. \
             A second pistachio mention and one zloty token live right here. {}",
        "More beta filler words in this spot. ".repeat(16)
    );
    let mk = |text: &str, page: u32, heading: &str| crate::extract::ExtractedBlock {
        text: text.trim().to_string(),
        page: Some(page),
        heading_path: Some(heading.to_string()),
    };
    let blocks = vec![
        mk(&alpha_para, 1, "Alpha"),
        mk(&alpha_para, 1, "Alpha"),
        mk(&alpha_para, 2, "Alpha"),
        mk(&beta_one, 3, "Beta"),
        mk(&beta_two, 3, "Beta"),
    ];
    let stored = crate::extract::blocks_to_stored_text(&blocks);
    db.insert_document(doc_id, folder_id, "plan.pdf", &stored, "document", 100)
        .unwrap();
    db.index_document_chunks(doc_id, Some(&crate::embed::StubEmbedder))
        .unwrap();
}

/// Audit Fix 1 (RED observed pre-fix: expansion served the dominant Alpha section) — a hit in
/// the SMALLER Beta section, corroborated by its sibling leaf, expands to the HIT's OWN L1
/// parent: the full Beta section, never Alpha.
#[test]
fn expand_serves_the_hit_sections_own_parent_when_siblings_corroborate() {
    let db = mem_db();
    seed_folder(&db, "f1", "Docs");
    seed_two_section_doc(&db, "d1", "f1");
    let nothing = std::collections::HashSet::new();
    let hits = db
        .search_doc_chunks_fts_visible("pistachio", 20, &nothing)
        .unwrap();
    assert_eq!(hits.len(), 1, "one document → one deduped hit");
    let h = &hits[0];
    assert_eq!(h.level, 0, "a Beta LEAF must win the per-document dedup");
    assert_eq!(h.section_path.as_deref(), Some("Beta"));
    assert!(
        h.parent_id.is_some(),
        "a section leaf carries its L1 parent id"
    );
    assert!(h.chunk_id > 0);
    assert_eq!(
        h.sibling_hits, 2,
        "both Beta leaves are in the pre-dedup candidate set (winner + 1 sibling)"
    );
    let parents = db.expand_doc_parents_visible(&hits, &nothing).unwrap();
    assert_eq!(parents.len(), 1, "a corroborated section hit expands");
    let p = &parents[0];
    assert_eq!(p.level, 1);
    assert!(
        p.snippet.contains("This beta paragraph opens")
            && p.snippet.contains("Another beta paragraph opens"),
        "expansion serves the FULL Beta section (both leaves), got: {:?}…",
        p.snippet.chars().take(80).collect::<String>()
    );
    assert!(
        !p.snippet.contains("Alpha filler"),
        "expansion must NEVER serve the dominant Alpha section for a Beta hit"
    );
}

/// Audit Fix 1 — a SINGLE uncorroborated leaf hit (`sibling_hits == 1`) keeps its leaf snippet:
/// no expansion row. (RED pre-fix: expansion was unconditional and query-independent.)
#[test]
fn expand_requires_two_corroborating_sibling_leaves() {
    let db = mem_db();
    seed_folder(&db, "f1", "Docs");
    seed_two_section_doc(&db, "d1", "f1");
    let nothing = std::collections::HashSet::new();
    let hits = db
        .search_doc_chunks_fts_visible("zloty", 20, &nothing)
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].level, 0, "the single matching Beta leaf wins");
    assert_eq!(
        hits[0].sibling_hits, 1,
        "only the winner itself matched — no corroborating sibling"
    );
    assert!(
        db.expand_doc_parents_visible(&hits, &nothing)
            .unwrap()
            .is_empty(),
        "an uncorroborated single-leaf hit must keep its leaf snippet"
    );
}

/// Audit Fix 1 — a FLAT document (no headings; `section_path` NULL) NEVER expands, even when
/// two of its leaves corroborate: its L1 is the doc head, not a real section. (RED pre-fix:
/// the old code expanded flat docs to the head, contrary to its own comments.)
#[test]
fn flat_doc_hit_keeps_its_leaf_no_expansion() {
    let db = mem_db();
    seed_folder(&db, "f1", "Docs");
    let mut text = String::new();
    for i in 0..6 {
        let marker = if i == 2 || i == 5 {
            "The zanzibar clause appears in this very paragraph. "
        } else {
            ""
        };
        text.push_str(&format!(
            "Flat paragraph number {i} with plain filler words. {marker}{}\n\n",
            "More flat filler text in this paragraph. ".repeat(16)
        ));
    }
    db.insert_document("d2", "f1", "notes.md", &text, "document", 100)
        .unwrap();
    db.index_document_chunks("d2", Some(&crate::embed::StubEmbedder))
        .unwrap();
    let nothing = std::collections::HashSet::new();
    let hits = db
        .search_doc_chunks_fts_visible("zanzibar", 20, &nothing)
        .unwrap();
    assert_eq!(hits.len(), 1, "a mid-file leaf must be keyword-reachable");
    assert!(
        hits[0].section_path.is_none(),
        "a flat doc's leaf has no section path"
    );
    assert_eq!(
        hits[0].sibling_hits, 2,
        "both matching flat leaves share the flat L1 — corroboration alone must NOT expand"
    );
    assert!(
        db.expand_doc_parents_visible(&hits, &nothing)
            .unwrap()
            .is_empty(),
        "a flat doc must keep its leaf snippet — never expand to the doc head"
    );
}

/// Brain v3 — `expand_doc_parents_visible` is GATED: a sealed-not-unlocked folder's document
/// yields NOTHING even for a STALE pre-seal hit still carrying a valid `parent_id` (RED if the
/// `visibility_clause` were removed), and unlocking restores the expansion.
#[test]
fn expand_doc_parents_is_gated_and_returns_section_text() {
    let db = mem_db();
    seed_folder(&db, "f-lock", "Secret");
    seed_two_section_doc(&db, "d1", "f-lock");
    let nothing = std::collections::HashSet::new();
    // Open folder → the corroborated Beta hit expands to its section text.
    let hits = db
        .search_doc_chunks_fts_visible("pistachio", 20, &nothing)
        .unwrap();
    assert_eq!(hits.len(), 1);
    let open = db.expand_doc_parents_visible(&hits, &nothing).unwrap();
    assert_eq!(open.len(), 1, "one corroborated hit → one parent");
    assert!(
        open[0].snippet.contains("beta paragraph"),
        "parent text is the SECTION body, got: {:?}",
        open[0].snippet.chars().take(80).collect::<String>()
    );

    // Seal → the SAME (now stale) hits are gated OUT with an empty unlock set: the parent
    // lookup re-applies the visibility gate per hit, so a pre-seal hit cannot fetch anything.
    db.set_folder_locked("f-lock", true, None).unwrap();
    assert!(
        db.expand_doc_parents_visible(&hits, &nothing)
            .unwrap()
            .is_empty(),
        "sealed-not-unlocked parent leaked through expand gate"
    );
    // Unlock → restored.
    let mut unlocked = std::collections::HashSet::new();
    unlocked.insert("f-lock".to_string());
    assert_eq!(
        db.expand_doc_parents_visible(&hits, &unlocked)
            .unwrap()
            .len(),
        1,
        "unlock restores the parent"
    );
}

/// GATE: a document in a sealed-and-not-session-unlocked folder is ABSENT from
/// `search_doc_chunks_visible` with an empty unlock set, and PRESENT when its folder is in the
/// set. The doc-chunk row deliberately STILL EXISTS, so exclusion can ONLY come from the gate —
/// RED if the `visibility_clause` inside `search_doc_chunks_visible` were removed.
#[test]
fn doc_chunk_search_is_gated_by_visibility() {
    let db = mem_db();
    seed_folder(&db, "f-locked", "Secret");
    db.insert_document(
        "d1",
        "f-locked",
        "secret.md",
        "launch date is the 14th",
        "document",
        100,
    )
    .unwrap();
    db.index_document_chunks("d1", Some(&crate::embed::StubEmbedder))
        .unwrap();
    // Query vector = the stub passage embedding of the doc's own chunk text → guaranteed nearest.
    let chunk_text: String = db
        .lock()
        .query_row(
            "SELECT text FROM doc_chunks WHERE document_id = 'd1' ORDER BY chunk_index LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let query = crate::embed::StubEmbedder
        .embed_passage(std::slice::from_ref(&chunk_text))
        .unwrap()
        .into_iter()
        .next()
        .unwrap();

    // Open folder → visible.
    let nothing = std::collections::HashSet::new();
    assert!(
        db.search_doc_chunks_visible(&query, 10, 0.0, &nothing)
            .unwrap()
            .iter()
            .any(|h| h.document_id == "d1"),
        "open-folder document chunk must be visible to search"
    );

    // Seal the folder (chunk row deliberately survives) → INVISIBLE with empty unlock set.
    db.set_folder_locked("f-locked", true, None).unwrap();
    let hidden = db
        .search_doc_chunks_visible(&query, 10, 0.0, &nothing)
        .unwrap();
    assert!(
        !hidden.iter().any(|h| h.document_id == "d1"),
        "sealed-not-unlocked document chunk leaked through the gate"
    );

    // Session-unlock → present again.
    let mut unlocked = std::collections::HashSet::new();
    unlocked.insert("f-locked".to_string());
    let shown = db
        .search_doc_chunks_visible(&query, 10, 0.0, &unlocked)
        .unwrap();
    assert!(
        shown.iter().any(|h| h.document_id == "d1"),
        "session-unlocked document chunk must reappear in search"
    );
}

/// ALWAYS-CHUNK contract (PR B): indexing with NO embedder (`None` — the model-less default
/// install) stores `doc_chunks` rows and ZERO vectors, and the chunk text is immediately
/// keyword-findable via the gated FTS leg. Deterministic regardless of whether the dev machine
/// has the real e5 model.
#[test]
fn index_document_chunks_none_embedder_chunks_no_vectors_fts_finds() {
    let db = mem_db();
    seed_folder(&db, "f-open", "Project");
    db.insert_document(
        "d1",
        "f-open",
        "spec.md",
        "the pistachio launch is in March",
        "document",
        100,
    )
    .unwrap();
    db.index_document_chunks("d1", None).unwrap();

    assert!(
        db.doc_chunk_count("d1").unwrap() >= 1,
        "chunk rows stored without a model"
    );
    assert_eq!(
        db.doc_vec_count("d1").unwrap(),
        0,
        "no vectors written without a model"
    );

    let nothing = std::collections::HashSet::new();
    let hits = db
        .search_doc_chunks_fts_visible("pistachio", 10, &nothing)
        .unwrap();
    assert!(
        hits.iter()
            .any(|h| h.document_id == "d1" && h.snippet.contains("pistachio")),
        "chunk-only document must be keyword-findable: {hits:?}"
    );
    // Punctuation-only query defuses to no hits (never an FTS syntax error).
    assert!(db
        .search_doc_chunks_fts_visible("?!*(", 10, &nothing)
        .unwrap()
        .is_empty());
}

/// READ-TIME REFLOW (doc-preview fix), chunk-input NO-OP: an md upload's stored text carries NO
/// block sentinel → it reconstructs as one plain block, the reflow gate no-ops on clean prose, and
/// the chunk text is byte-identical to the stored text (retrieval unchanged for normal docs).
#[test]
fn index_document_chunks_reflow_is_noop_on_clean_md() {
    let db = mem_db();
    seed_folder(&db, "f-open", "Project");
    // A clean md doc (no sentinel, normal prose) — the gate must NOT fire.
    db.insert_document(
        "d1",
        "f-open",
        "spec.md",
        "the pistachio launch is in March",
        "document",
        100,
    )
    .unwrap();
    db.index_document_chunks("d1", None).unwrap();
    let nothing = std::collections::HashSet::new();
    let hits = db
        .search_doc_chunks_fts_visible("pistachio", 10, &nothing)
        .unwrap();
    assert!(
        hits.iter()
            .any(|h| h.document_id == "d1" && h.snippet.contains("pistachio")),
        "clean md chunk text is unchanged by the (no-op) reflow gate: {hits:?}"
    );
}

/// Brain v3 audit Fix 3(b) — `get_document_outline_if_visible` returns the L1 section tree
/// (section_path + page) in document order, is GATED (a sealed-not-unlocked doc → EMPTY; unlock →
/// the tree), and is BOUNDED by `cap`. RED-before-GREEN: the gate assertion fails if the reader
/// forgets `visibility_clause` (the sealed doc's headings leak).
#[test]
fn get_document_outline_is_visibility_gated_and_ordered() {
    let db = mem_db();
    seed_folder(&db, "f-lock", "Specs");
    // A multi-section doc: two headings across two pages → two L1 rows.
    let blocks = vec![
        crate::extract::ExtractedBlock {
            text: "The system stores everything in SQLite.".to_string(),
            page: Some(1),
            heading_path: Some("Design".to_string()),
        },
        crate::extract::ExtractedBlock {
            text: "Chunks form a three-level tree.".to_string(),
            page: Some(2),
            heading_path: Some("Design › Storage".to_string()),
        },
    ];
    let stored = crate::extract::blocks_to_stored_text(&blocks);
    db.insert_document("d1", "f-lock", "spec.pdf", &stored, "document", 100)
        .unwrap();
    db.index_document_chunks("d1", None).unwrap();

    // OPEN folder → the outline lists the section-parents in document order with their pages.
    let nothing = std::collections::HashSet::new();
    let outline = db
        .get_document_outline_if_visible("d1", &nothing, 64)
        .unwrap();
    let l1: Vec<_> = outline.iter().filter(|e| e.level == 1).collect();
    assert!(
        l1.len() >= 2,
        "two headings → at least two L1 rows: {outline:?}"
    );
    assert_eq!(l1[0].section_path.as_deref(), Some("Design"));
    assert_eq!(l1[0].page_no, Some(1));
    assert_eq!(l1[1].section_path.as_deref(), Some("Design › Storage"));
    assert_eq!(l1[1].page_no, Some(2), "document order preserved");

    // SEAL the folder → the outline is EMPTY (the gate excludes it; the same masking as every
    // other doc reader — headings never leak from a sealed-not-unlocked folder).
    db.set_folder_locked("f-lock", true, None).unwrap();
    let sealed = db
        .get_document_outline_if_visible("d1", &nothing, 64)
        .unwrap();
    assert!(
        sealed.is_empty(),
        "sealed-not-unlocked doc must yield an EMPTY outline: {sealed:?}"
    );

    // Session-unlock → the tree reappears.
    let mut unlocked = std::collections::HashSet::new();
    unlocked.insert("f-lock".to_string());
    let reopened = db
        .get_document_outline_if_visible("d1", &unlocked, 64)
        .unwrap();
    assert!(
        reopened
            .iter()
            .any(|e| e.section_path.as_deref() == Some("Design")),
        "unlock restores the outline: {reopened:?}"
    );

    // cap = 0 → empty; cap = 1 → at most one entry (bounded).
    assert!(db
        .get_document_outline_if_visible("d1", &unlocked, 0)
        .unwrap()
        .is_empty());
    assert!(
        db.get_document_outline_if_visible("d1", &unlocked, 1)
            .unwrap()
            .len()
            <= 1
    );
}

/// READ-TIME REFLOW (doc-preview fix), chunk-input DE-FRAGMENTS: a document whose stored text is a
/// LOCATED (page) block of pathologically letter-spaced PDF glyphs (`"Fron\nt\nend"`) must chunk on
/// the RECOVERED word ("Frontend") so a query for the real word retrieves it — proving reflow runs
/// on the chunk input, not just the display. RED-before-GREEN: the raw fragment does NOT contain
/// "Frontend", so without reflow the FTS query for "Frontend" finds nothing.
#[test]
fn index_document_chunks_reflow_defragments_letter_spaced_pdf_text() {
    let db = mem_db();
    seed_folder(&db, "f-open", "CVs");
    // Build a stored text that carries the block sentinel (a located PDF page block), so
    // `blocks_from_stored_text` reconstructs a located block whose fragmented text reflow repairs.
    // The fragment is heavily letter-spaced (many ≤3-char lines) so it trips the conservative
    // fragmentation gate — a lightly-broken block would (correctly) NOT reflow.
    let block = crate::extract::ExtractedBlock {
            text: "S\ntaff Fron\nt\nend Engineer bu\ni\nl\nd\ning realt\nime web plat\nforms in\nA\nng\nular and TypeScript".to_string(),
            page: Some(1),
            heading_path: None,
        };
    let stored = crate::extract::blocks_to_stored_text(std::slice::from_ref(&block));
    assert!(
        !stored.contains("Frontend"),
        "precondition: the raw stored fragment does NOT contain the recovered word (RED state)"
    );
    db.insert_document("d1", "f-open", "cv.pdf", &stored, "document", 100)
        .unwrap();
    db.index_document_chunks("d1", None).unwrap();

    let nothing = std::collections::HashSet::new();
    let hits = db
        .search_doc_chunks_fts_visible("Frontend", 10, &nothing)
        .unwrap();
    assert!(
        hits.iter().any(|h| h.document_id == "d1"),
        "reflow must de-fragment the chunk input so a query for the recovered word retrieves the \
             document — without reflow the shattered fragment never matches: {hits:?}"
    );
}

/// CONTACT DIGEST (Brain retrieval fix, 2026-07-19): a phone/email buried in a document's prose is
/// NOT retrievable by a natural-language "what's the phone number" query — the query WORDS never
/// co-occur with the digits. `chunk_document_hierarchical` emits a synthetic contact-digest chunk
/// pairing the fact VALUES with bilingual (PL/EN) bridge words, so those queries now match via FTS.
/// RED-before-GREEN: the raw prose carries "+48 786 327 907" but NOT the words "numer telefonu" /
/// "phone number", so WITHOUT the digest an FTS query for those words returns nothing. A control
/// doc with NO contact fact must never match a contact query (recall-safety).
#[test]
fn index_document_chunks_contact_digest_makes_phone_retrievable_by_nl_query() {
    let db = mem_db();
    seed_folder(&db, "f-open", "CVs");
    let block = crate::extract::ExtractedBlock {
            text: "Oskar Orlowski — Staff Frontend Engineer. Warsaw, Poland. +48 786 327 907 \
                   orlow@wp.pl. Ten years building realtime web platforms in Angular and TypeScript."
                .to_string(),
            page: Some(1),
            heading_path: None,
        };
    let stored = crate::extract::blocks_to_stored_text(std::slice::from_ref(&block));
    assert!(
        !stored.to_lowercase().contains("numer telefonu")
            && !stored.to_lowercase().contains("phone number"),
        "precondition: the raw prose does NOT contain the query words (RED state)"
    );
    db.insert_document(
        "d-cv",
        "f-open",
        "Oskar_Orlowski_CV.pdf",
        &stored,
        "document",
        100,
    )
    .unwrap();
    // Control: a document with NO contact fact — a contact query must never return it.
    let ctrl = crate::extract::ExtractedBlock {
        text: "Notes about the weekly roadmap and platform priorities for the team.".to_string(),
        page: None,
        heading_path: None,
    };
    let ctrl_stored = crate::extract::blocks_to_stored_text(std::slice::from_ref(&ctrl));
    db.insert_document(
        "d-other",
        "f-open",
        "roadmap.md",
        &ctrl_stored,
        "document",
        100,
    )
    .unwrap();
    db.index_document_chunks("d-cv", None).unwrap();
    db.index_document_chunks("d-other", None).unwrap();

    let nothing = std::collections::HashSet::new();
    for q in ["numer telefonu", "phone number", "telefon"] {
        let hits = db.search_doc_chunks_fts_visible(q, 10, &nothing).unwrap();
        assert!(
            hits.iter().any(|h| h.document_id == "d-cv"),
            "query {q:?} must retrieve the CV via the contact digest: {hits:?}"
        );
        assert!(
            !hits.iter().any(|h| h.document_id == "d-other"),
            "contact query {q:?} must NOT match the contactless control doc: {hits:?}"
        );
    }
    // The digest also carries the number itself (spaced + bare), so a digit query lands too.
    let by_number = db
        .search_doc_chunks_fts_visible("786 327 907", 10, &nothing)
        .unwrap();
    assert!(
        by_number.iter().any(|h| h.document_id == "d-cv"),
        "the phone number itself must retrieve the CV: {by_number:?}"
    );
}

/// TOCTOU (lock-security finding, PR-1): `index_document_chunks` must REFUSE the entire write —
/// including its clean-replace PURGE — when the row is sealed at rest RIGHT NOW (a `lock_folder`
/// committing mid-embed blanks `text` into `text_blob`). The re-check runs BEFORE the purge, so a
/// refused racing index leaves the DB untouched rather than re-deriving from the (now-blank) read.
/// RED-before-GREEN distinguisher: index once (chunks present) → seal at rest WITHOUT purging
/// (`seal_document` blanks `text`/sets `text_blob`, mirroring the read-gate-independent-of-purge
/// pattern) → index again. WITH the guard the indexer early-returns before the purge, so the prior
/// chunks SURVIVE the refused write; WITHOUT it the purge runs and the blank re-read leaves ZERO —
/// the two outcomes differ, so removing `doc_sealed_at_rest_tx` fails this test.
#[test]
fn document_index_refuses_write_when_sealed_at_rest_mid_flight() {
    let db = mem_db();
    seed_folder(&db, "f-lock", "Secret");
    db.insert_document(
        "d1",
        "f-lock",
        "spec.md",
        "the pistachio launch is in March",
        "document",
        100,
    )
    .unwrap();
    db.index_document_chunks("d1", None).unwrap();
    assert!(
        db.doc_chunk_count("d1").unwrap() >= 1,
        "precondition: the open document is chunked"
    );

    // Seal at rest exactly like `seal_document` (blank text, blob kept) WITHOUT purging chunks —
    // isolates the guard from the seal-time purge (the `list_facts_visible_excludes_sealed_meeting`
    // read-gate pattern). A racing `index_document_chunks` must now refuse the whole write.
    db.seal_document("d1", &[0u8]).unwrap();
    db.index_document_chunks("d1", None).unwrap();
    assert!(
        db.doc_chunk_count("d1").unwrap() >= 1,
        "the in-tx sealed-at-rest re-check must REFUSE the write (guard runs before the purge) — \
             without it the purge+blank-read would zero the chunks, so this survival IS the guard"
    );

    // Restore the plaintext (session unlock un-blanks `text`) → indexing proceeds again (re-derives).
    db.set_document_text(
        "d1",
        "the pistachio launch is in March; new pistachio milestone",
    )
    .unwrap();
    db.index_document_chunks("d1", None).unwrap();
    let texts: String = {
        let conn = db.lock();
        let mut stmt = conn
            .prepare("SELECT text FROM doc_chunks WHERE document_id = 'd1' ORDER BY chunk_index")
            .unwrap();
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect::<Vec<_>>();
        rows.join("\n")
    };
    assert!(
        texts.contains("milestone"),
        "an unsealed (un-blanked) document re-indexes the fresh plaintext"
    );
}

/// TOCTOU (lock-security finding, PR-1): the authored-NOTE twin — `index_note_chunks` (the
/// front-matter-stripped note path) must ALSO refuse when the note's `documents` row is sealed at
/// rest mid-flight. RED-before-GREEN: remove the `doc_sealed_at_rest_tx` guard in
/// `index_note_chunks` → the note's body chunks persist behind the seal.
#[test]
fn note_index_refuses_write_when_sealed_at_rest_mid_flight() {
    let db = mem_db();
    seed_folder(&db, "f-lock", "Secret");
    db.insert_document(
        "n1",
        "f-lock",
        "meeting-notes.md",
        "budget owner is Dana; pistachio ship date confirmed",
        "note",
        100,
    )
    .unwrap();
    db.seal_document("n1", &[0u8]).unwrap();

    db.index_note_chunks("n1", "Meeting notes", "budget owner is Dana", None)
        .unwrap();
    assert_eq!(
        db.doc_chunk_count("n1").unwrap(),
        0,
        "the in-tx sealed-at-rest re-check must refuse writing note body chunks"
    );

    db.set_document_text("n1", "budget owner is Dana; pistachio ship date confirmed")
        .unwrap();
    db.index_note_chunks("n1", "Meeting notes", "budget owner is Dana", None)
        .unwrap();
    assert!(
        db.doc_chunk_count("n1").unwrap() >= 1,
        "an unsealed (un-blanked) note indexes again"
    );
}

/// GATE twin of `doc_chunk_search_is_gated_by_visibility` for the KEYWORD leg: a doc chunk row
/// that deliberately SURVIVES in a sealed-not-unlocked folder is EXCLUDED by
/// `search_doc_chunks_fts_visible` (defense-in-depth `visibility_clause`) and reappears only
/// with the session unlock set. RED if the clause were dropped from the FTS join.
#[test]
fn doc_chunk_fts_search_is_gated_by_visibility() {
    let db = mem_db();
    seed_folder(&db, "f-locked", "Secret");
    db.insert_document(
        "d1",
        "f-locked",
        "secret.md",
        "launch date is the 14th",
        "document",
        100,
    )
    .unwrap();
    db.index_document_chunks("d1", None).unwrap();

    let nothing = std::collections::HashSet::new();
    assert!(
        db.search_doc_chunks_fts_visible("launch", 10, &nothing)
            .unwrap()
            .iter()
            .any(|h| h.document_id == "d1"),
        "open-folder document chunk must be keyword-visible"
    );

    // Seal the folder (chunk row deliberately survives) → INVISIBLE with empty unlock set.
    db.set_folder_locked("f-locked", true, None).unwrap();
    assert!(
        !db.search_doc_chunks_fts_visible("launch", 10, &nothing)
            .unwrap()
            .iter()
            .any(|h| h.document_id == "d1"),
        "sealed-not-unlocked document chunk leaked through the FTS gate"
    );

    // Session-unlock → present again.
    let mut unlocked = std::collections::HashSet::new();
    unlocked.insert("f-locked".to_string());
    assert!(
        db.search_doc_chunks_fts_visible("launch", 10, &unlocked)
            .unwrap()
            .iter()
            .any(|h| h.document_id == "d1"),
        "session-unlocked document chunk must reappear in FTS search"
    );
}

/// SEAL-DARKNESS at the INDEX level: purging a document's chunks (what the lock path does)
/// removes its tokens from `fts_doc_chunks` in the same statement (the `_ad` trigger), so no
/// sealed token survives in the inverted index at rest; re-indexing restores them. Mirrors
/// `sealed_tokens_purged_from_fts_after_blank` for the meeting FTS trio.
#[test]
fn doc_fts_tokens_purged_with_chunks_and_restored_on_reindex() {
    let db = mem_db();
    seed_folder(&db, "f", "F");
    db.insert_document(
        "d1",
        "f",
        "n.md",
        "unicornfeather budget detail",
        "note",
        100,
    )
    .unwrap();
    db.index_document_chunks("d1", None).unwrap();

    let fts_count = |db: &Db| -> i64 {
        db.lock()
                .query_row(
                    "SELECT COUNT(*) FROM fts_doc_chunks WHERE fts_doc_chunks MATCH '\"unicornfeather\"'",
                    [],
                    |r| r.get(0),
                )
                .unwrap()
    };
    assert!(fts_count(&db) >= 1, "token indexed after chunking");

    db.purge_doc_chunks_for_documents(&["d1".to_string()])
        .unwrap();
    assert_eq!(
        fts_count(&db),
        0,
        "sealed/purged token must not survive in the FTS index"
    );
    let nothing = std::collections::HashSet::new();
    assert!(db
        .search_doc_chunks_fts_visible("unicornfeather", 10, &nothing)
        .unwrap()
        .is_empty());

    db.index_document_chunks("d1", None).unwrap();
    assert!(fts_count(&db) >= 1, "re-index restores the keyword index");
}

/// POLISH DIACRITICS: the doc FTS uses the SAME `unicode61 remove_diacritics 2` tokenizer as
/// the meeting tables, so an ASCII-folded query matches the diacritic original and vice versa
/// for NFD-decomposable marks (ż/ź/ę/ś/ą/ó/ć/ń — note ł, a stroked letter with NO Unicode
/// decomposition, is genuinely NOT foldable by any FTS5 remove_diacritics mode).
#[test]
fn doc_fts_matches_polish_diacritics_folded() {
    let db = mem_db();
    seed_folder(&db, "f", "F");
    db.insert_document(
        "d1",
        "f",
        "pl.md",
        "gęślą jaźń — budżet kwartalny",
        "note",
        100,
    )
    .unwrap();
    db.index_document_chunks("d1", None).unwrap();

    let nothing = std::collections::HashSet::new();
    for q in ["budzet", "jazn", "gesla"] {
        assert!(
            db.search_doc_chunks_fts_visible(q, 10, &nothing)
                .unwrap()
                .iter()
                .any(|h| h.document_id == "d1"),
            "ASCII query {q:?} must match the diacritic original (remove_diacritics 2)"
        );
    }
    assert!(
        db.search_doc_chunks_fts_visible("gęślą", 10, &nothing)
            .unwrap()
            .iter()
            .any(|h| h.document_id == "d1"),
        "diacritic query must match too"
    );
}

/// SEAL ROUND-TRIP (verify-before-destroy, byte-identical): a document's text encrypts under a CK
/// (AAD-less here for the unit, mirroring the `seal_*_round_trips_byte_identical` pattern), blanks,
/// and decrypts back byte-identical via `seal_document` / `set_document_text` / `clear_document_blob`.
#[test]
fn seal_document_round_trips_byte_identical() {
    let db = mem_db();
    seed_folder(&db, "f", "F");
    let original = "zażółć gęślą jaźń — DECISION: ship Friday";
    db.insert_document("d1", "f", "n.md", original, "document", 100)
        .unwrap();

    let ck = crate::crypto::random_key().unwrap();
    // Encrypt + VERIFY decryptable BEFORE sealing (the command's verify-before-destroy rule).
    let blob = crate::crypto::encrypt(&ck, original.as_bytes(), b"").unwrap();
    assert_eq!(
        crate::crypto::decrypt(&ck, &blob, b"").unwrap(),
        original.as_bytes()
    );
    db.seal_document("d1", &blob).unwrap();
    // Plaintext blanked, blob present.
    let raw = db.raw_documents_in_folder("f").unwrap();
    assert_eq!(raw[0].text, "");
    let stored_blob = raw[0].blob.clone().unwrap();
    // Decrypt the STORED blob → byte-identical to the original.
    let restored = crate::crypto::decrypt(&ck, &stored_blob, b"").unwrap();
    assert_eq!(
        restored,
        original.as_bytes(),
        "sealed document round-trips byte-identical"
    );
    // Restore + clear the blob (remove-lock shape).
    db.set_document_text("d1", &String::from_utf8(restored).unwrap())
        .unwrap();
    db.clear_document_blob("d1").unwrap();
    let raw2 = db.raw_documents_in_folder("f").unwrap();
    assert_eq!(raw2[0].text, original);
    assert!(raw2[0].blob.is_none());
}

/// delete_document drops the row AND cascades its chunks/vectors (mirrors delete_meeting).
#[test]
fn delete_document_drops_row_and_chunks() {
    let db = mem_db();
    seed_folder(&db, "f", "F");
    db.insert_document("d1", "f", "n.md", "alpha bravo charlie", "document", 100)
        .unwrap();
    db.index_document_chunks("d1", Some(&crate::embed::StubEmbedder))
        .unwrap();
    db.delete_document("d1").unwrap();
    assert!(
        db.get_document("d1").unwrap().is_none(),
        "document row deleted"
    );
    let count: i64 = db
        .lock()
        .query_row(
            "SELECT COUNT(*) FROM doc_chunks WHERE document_id = 'd1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0, "doc chunks cascade-deleted");
}

/// RELATED-BY-MEANING GATE: `related_meetings_visible` re-embeds the SOURCE meeting's own chunk
/// text into a centroid, runs the gated KNN, and must NEVER surface a sealed-not-session-unlocked
/// neighbour — even when that neighbour's chunk row deliberately survives (so exclusion can ONLY
/// come from the visibility gate, not from purge). Session-unlocking its folder admits it again.
/// RED if the gate inside `search_semantic_visible` were removed.
#[test]
fn related_meetings_visible_is_gated_by_visibility() {
    let db = mem_db();
    seed_folder(&db, "f-open", "Open");
    seed_folder(&db, "f-locked", "Secret");

    // Source (open) + an open target with the SAME note text (near in stub space) + a sealed
    // meeting with the SAME text (would be near). All indexed via the stub so vectors are real.
    let body = "shared budget planning topic for the quarter";
    for id in ["source", "target", "sealed"] {
        db.insert_meeting(&sample_meeting(id, "2026-06-24T10:00:00Z"))
            .unwrap();
        note_for(&db, id, "claude_code", body);
    }
    db.set_note_folder("source", Some("f-open")).unwrap();
    db.set_note_folder("target", Some("f-open")).unwrap();
    db.set_note_folder("sealed", Some("f-locked")).unwrap();
    for id in ["source", "target", "sealed"] {
        db.index_meeting_chunks(id, &[], &crate::embed::StubEmbedder)
            .unwrap();
    }
    // Lock the sealed folder WITHOUT purging — its chunk row survives, so any exclusion must be
    // the gate doing its job.
    db.set_folder_locked("f-locked", true, None).unwrap();
    assert!(
        chunk_count(&db, "sealed") > 0,
        "sealed chunk must survive for a true gate test"
    );

    let stub = crate::embed::StubEmbedder;

    // Empty unlock set → the sealed neighbour is ABSENT (no hit, no snippet); the open target IS.
    let nothing = std::collections::HashSet::new();
    let hidden = db
        .related_meetings_visible("source", &stub, 5, &nothing)
        .unwrap();
    assert!(
        hidden.iter().any(|h| h.meeting.id == "target"),
        "open semantically-near target must be returned"
    );
    assert!(
        !hidden.iter().any(|h| h.meeting.id == "sealed"),
        "sealed-not-unlocked neighbour leaked through related_meetings_visible"
    );

    // Session-unlock the sealed folder → it now appears.
    let mut unlocked = std::collections::HashSet::new();
    unlocked.insert("f-locked".to_string());
    let shown = db
        .related_meetings_visible("source", &stub, 5, &unlocked)
        .unwrap();
    assert!(
        shown.iter().any(|h| h.meeting.id == "sealed"),
        "session-unlocked neighbour must reappear in related results"
    );
}

/// SELF-EXCLUSION: the source meeting is never present in its own related results, even though it
/// is (trivially) its own nearest neighbour in the KNN.
#[test]
fn related_meetings_visible_excludes_self() {
    let db = mem_db();
    let body = "atlas roadmap and hiring plan";
    for id in ["self", "other"] {
        db.insert_meeting(&sample_meeting(id, "2026-06-24T10:00:00Z"))
            .unwrap();
        note_for(&db, id, "claude_code", body);
        db.index_meeting_chunks(id, &[], &crate::embed::StubEmbedder)
            .unwrap();
    }
    let stub = crate::embed::StubEmbedder;
    let nothing = std::collections::HashSet::new();
    let hits = db
        .related_meetings_visible("self", &stub, 5, &nothing)
        .unwrap();
    assert!(
        !hits.iter().any(|h| h.meeting.id == "self"),
        "a meeting must never be in its own related results"
    );
    assert!(
        hits.iter().any(|h| h.meeting.id == "other"),
        "the other meeting (same text) should still be returned"
    );
}

/// EMPTY: a meeting with no `note_chunks` (never indexed, or chunks purged on lock) yields an
/// empty result — never an error, never a panic.
#[test]
fn related_meetings_visible_empty_without_chunks() {
    let db = mem_db();
    db.insert_meeting(&sample_meeting("bare", "2026-06-24T10:00:00Z"))
        .unwrap();
    note_for(
        &db,
        "bare",
        "claude_code",
        "has a note but was never indexed",
    );
    // No index_meeting_chunks call → zero chunk rows.
    assert_eq!(chunk_count(&db, "bare"), 0);
    let stub = crate::embed::StubEmbedder;
    let nothing = std::collections::HashSet::new();
    let hits = db
        .related_meetings_visible("bare", &stub, 5, &nothing)
        .unwrap();
    assert!(hits.is_empty(), "no chunks ⇒ empty related result");
}

// ── Brain v3 PR-3 — LINK ENGINE (persisted `links` rows) ──────────────────────────────────

/// Count `links` rows incident on `(kind, id)` with the given `edge_type` (any status).
fn link_count(db: &Db, kind: &str, id: &str, edge_type: &str) -> i64 {
    db.lock()
        .query_row(
            "SELECT COUNT(*) FROM links
                   WHERE edge_type = ?3
                     AND ((src_kind = ?1 AND src_id = ?2) OR (dst_kind = ?1 AND dst_id = ?2))",
            rusqlite::params![kind, id, edge_type],
            |r| r.get(0),
        )
        .unwrap()
}

fn manual_link_tuple_count(
    db: &Db,
    src_kind: &str,
    src_id: &str,
    dst_kind: &str,
    dst_id: &str,
) -> i64 {
    db.lock()
        .query_row(
            "SELECT COUNT(*) FROM links
              WHERE src_kind = ?1 AND src_id = ?2 AND dst_kind = ?3 AND dst_id = ?4
                AND edge_type = 'manual'",
            rusqlite::params![src_kind, src_id, dst_kind, dst_id],
            |row| row.get(0),
        )
        .unwrap()
}

/// A note-doc (`kind='note'`) with a title, in a folder, indexed via the stub so it has vectors.
fn seed_note_doc(db: &Db, id: &str, folder_id: &str, title: &str, body: &str) {
    db.insert_note(id, folder_id, title, title, body, 1_700_000_000_000)
        .unwrap();
    db.index_note_chunks(id, title, body, Some(&crate::embed::StubEmbedder))
        .unwrap();
}

/// The `links` table is created by migrate and survives a re-migrate (idempotency).
#[test]
fn migrate_creates_links_table_idempotently() {
    let db = mem_db();
    let has_links = |db: &Db| -> bool {
        db.lock()
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'links'",
                [],
                |_| Ok(()),
            )
            .optional()
            .unwrap()
            .is_some()
    };
    assert!(has_links(&db), "links table missing after migrate");
    let has_cleanup_outbox = |db: &Db| -> bool {
        db.lock()
            .query_row(
                "SELECT 1 FROM sqlite_master
                      WHERE type = 'table' AND name = 'lock_marker_export_cleanup'",
                [],
                |_| Ok(()),
            )
            .optional()
            .unwrap()
            .is_some()
    };
    assert!(
        has_cleanup_outbox(&db),
        "lock-marker export cleanup outbox missing after migrate"
    );
    db.migrate().unwrap();
    db.migrate().unwrap();
    assert!(
        has_links(&db),
        "links table missing after re-migrate (idempotency broken)"
    );
    assert!(
        has_cleanup_outbox(&db),
        "lock-marker export cleanup outbox missing after re-migrate"
    );
    // The companion-backfill sentinel is set exactly once and re-migrate does not error.
    let sentinel: Option<String> = db
        .lock()
        .query_row(
            "SELECT value FROM settings WHERE key = 'links_companion_backfill_v1'",
            [],
            |r| r.get(0),
        )
        .optional()
        .unwrap();
    assert_eq!(
        sentinel.as_deref(),
        Some("1"),
        "companion backfill sentinel missing"
    );
}

/// WIKILINK edges are stored by RESOLVED TARGET ID and SURVIVE a target rename — the root-cause
/// fix. Index `[[Target]]` in a source note, rename the target, and the edge still resolves via
/// the stored id (a title-string edge would have gone stale).
#[test]
fn wikilink_edges_stored_by_id_survive_rename() {
    let db = mem_db();
    seed_folder(&db, "f1", "Notes");
    // Target note titled "Target" + a source note whose body links [[Target]].
    seed_note_doc(&db, "target", "f1", "Target", "the target body");
    db.insert_note(
        "source",
        "f1",
        "Source",
        "Source",
        "see [[Target]] for context",
        1,
    )
    .unwrap();

    let unlocked = HashSet::new();
    db.index_wikilinks_for_source(
        crate::links::LinkKind::Note,
        "source",
        "see [[Target]] for context",
        &unlocked,
    )
    .unwrap();

    // One wikilink edge source → target, storing the TARGET's id (not the title).
    let (dk, di): (String, String) = db
        .lock()
        .query_row(
            "SELECT dst_kind, dst_id FROM links
                   WHERE src_kind='note' AND src_id='source' AND edge_type='wikilink'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(dk, "note");
    assert_eq!(
        di, "target",
        "edge must store the resolved target ID, not the title string"
    );

    // RENAME the target: the edge id is unchanged, so the link still resolves through the reader.
    db.lock()
        .execute(
            "UPDATE documents SET title = 'Renamed' WHERE id='target'",
            [],
        )
        .unwrap();
    let edges = db
        .links_for_visible(crate::links::LinkKind::Note, "source", &unlocked)
        .unwrap();
    let hit = edges
        .iter()
        .find(|e| e.other_id == "target")
        .expect("edge survives rename");
    assert_eq!(
        hit.other_title, "Renamed",
        "reader resolves the CURRENT title from the stored id"
    );
    assert_eq!(hit.edge_type, "wikilink");

    // Re-index a body that no longer links [[Target]] → the stale edge is dropped (self-healing).
    db.index_wikilinks_for_source(
        crate::links::LinkKind::Note,
        "source",
        "no links now",
        &unlocked,
    )
    .unwrap();
    assert_eq!(
        link_count(&db, "note", "source", "wikilink"),
        0,
        "removed wikilink is dropped"
    );
}

/// SEMANTIC auto-linker: two meetings with IDENTICAL stub-embedded text are mutual nearest
/// neighbours (cos 1.0 ≥ STRONG), so each suggests the other as a `semantic` `suggested` edge;
/// self is never suggested; the CAP bounds the fan-out.
#[test]
fn semantic_auto_linker_mutual_knn_suggests_and_caps() {
    let db = mem_db();
    let body = "quarterly revenue growth and the hiring plan for the platform team";
    // 8 identical-text meetings — every pair is a mutual nearest neighbour in stub space.
    for i in 0..8 {
        let id = format!("m{i}");
        db.insert_meeting(&sample_meeting(&id, "2026-06-24T10:00:00Z"))
            .unwrap();
        note_for(&db, &id, "claude_code", body);
        db.index_meeting_chunks(&id, &[], &crate::embed::StubEmbedder)
            .unwrap();
    }
    let unlocked = HashSet::new();
    let written = db
        .auto_link_semantic(crate::links::LinkKind::Meeting, "m0", &unlocked)
        .unwrap();
    assert_eq!(
        written,
        crate::links::SEMANTIC_LINK_CAP,
        "cap bounds the fan-out to SEMANTIC_LINK_CAP"
    );

    // Every suggested edge is semantic/suggested/auto, undirected (canonicalized), score≈1.0, and
    // never points at self. Assert per-row inside the query_map (no complex-tuple intermediate).
    let conn = db.lock();
    let mut stmt = conn
            .prepare("SELECT src_kind, src_id, dst_kind, dst_id, edge_type, status, created_by, score FROM links")
            .unwrap();
    let mut seen = 0usize;
    let mut rows = stmt.query([]).unwrap();
    while let Some(r) = rows.next().unwrap() {
        let sk: String = r.get(0).unwrap();
        let si: String = r.get(1).unwrap();
        let dk: String = r.get(2).unwrap();
        let di: String = r.get(3).unwrap();
        let et: String = r.get(4).unwrap();
        let st: String = r.get(5).unwrap();
        let cb: String = r.get(6).unwrap();
        let score: f64 = r.get(7).unwrap();
        assert_eq!(et, "semantic");
        assert_eq!(st, "suggested");
        assert_eq!(cb, "auto");
        assert!(score >= 0.80, "score is the cosine, above floor");
        assert!(
            !(sk == "meeting" && si == "m0" && dk == "meeting" && di == "m0"),
            "never self-links"
        );
        // Canonicalized: src <= dst by (kind, id).
        assert!(
            (sk.as_str(), si.as_str()) <= (dk.as_str(), di.as_str()),
            "undirected edge canonicalized"
        );
        // m0 is one of the two endpoints.
        assert!(si == "m0" || di == "m0", "edge incident on the source");
        seen += 1;
    }
    assert_eq!(seen, crate::links::SEMANTIC_LINK_CAP);
}

/// Build a note markdown of `n` ~800-char paragraphs (blank-line separated) so `chunk_note` emits
/// `n` separate chunks — the shape that reproduces the ≥11-chunk STARVATION. Every paragraph of ONE
/// item is IDENTICAL (so the item's own chunks all sit exactly at its own centroid — distance 0 —
/// and are STRICTLY closer to it than any OTHER item's chunk). The `marker` word is per-item (it
/// separates the two items' clusters); the shared `common` phrase makes the cross-item cosine still
/// clear the FLOOR. On the OLD fixed chunk-`k` probe the item's own `n`>k distance-0 chunks fill the
/// entire top-K, so after GROUP BY + self-drop ZERO neighbours survive — the starvation this fixes.
fn many_chunk_body(marker: &str, common: &str, n: usize) -> String {
    // One ~800-char paragraph: the shared phrase + a per-item marker, REPEATED across n paragraphs.
    let para = format!("{common} {marker} ").repeat(13);
    (0..n)
        .map(|_| para.clone())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Fix 1 (RED before the item-granular kNN fan-out): an item with ≥11 chunks (a long note — the
/// exact case linking is FOR) must still surface its legitimate same-table neighbour. On the OLD
/// `k = SEMANTIC_LINK_K + 1` CHUNK probe, the source's own 12 chunks fill the top-11, so
/// `auto_link_semantic` returned ZERO candidates; the fan-out now over-fetches distinct NON-SELF
/// items (and excludes self chunks), so the neighbour is found and suggested.
#[test]
fn semantic_auto_linker_item_with_many_chunks_gets_neighbour() {
    let db = mem_db();
    let common = "quarterly revenue growth and the platform hiring plan roadmap budget";
    // Two long meetings sharing `common` but with DISTINCT per-item markers, each 12 chunks. The
    // source's own 12 distance-0 chunks would fill the OLD top-11 probe and starve out the
    // neighbour; the fix over-fetches distinct non-self items so the neighbour survives.
    let body_a = many_chunk_body("projectalpha", common, 12);
    let body_b = many_chunk_body("projectbeta", common, 12);
    for (id, body) in [("m-long-a", &body_a), ("m-long-b", &body_b)] {
        db.insert_meeting(&sample_meeting(id, "2026-06-24T10:00:00Z"))
            .unwrap();
        note_for(&db, id, "claude_code", body);
        db.index_meeting_chunks(id, &[], &crate::embed::StubEmbedder)
            .unwrap();
    }
    // Sanity: the source really has ≥11 chunks (the condition the bug needs to bite).
    assert!(
        db.note_vec_count("m-long-a").unwrap() >= 11,
        "test precondition: the source must have >=11 chunks to reproduce the starvation"
    );

    let unlocked = HashSet::new();
    let written = db
        .auto_link_semantic(crate::links::LinkKind::Meeting, "m-long-a", &unlocked)
        .unwrap();
    assert!(
        written >= 1,
        "a >=11-chunk item must still surface its same-table neighbour (starvation regression)"
    );
    // The suggested edge is incident on the actual neighbour m-long-b.
    let edges = db
        .links_for_visible(crate::links::LinkKind::Meeting, "m-long-a", &unlocked)
        .unwrap();
    assert!(
        edges
            .iter()
            .any(|e| e.other_id == "m-long-b" && e.edge_type == "semantic"),
        "the legitimate neighbour must be a suggested semantic edge, not starved out"
    );
}

/// Fix 2 (RED before the both-endpoint cap): a HUB node that receives suggestions from MANY other
/// items' passes must keep only the top-`SEMANTIC_LINK_CAP` by score — the source-side cap alone
/// left a hub with unbounded INBOUND suggestions. We seed `CAP + 3` suggested-semantic edges all
/// pointing at one hub with descending scores, then run the trim (as each upsert now does) and
/// assert only the top CAP survive, weakest dropped.
#[test]
fn semantic_suggestions_capped_per_node_on_both_endpoints() {
    let db = mem_db();
    let cap = crate::links::SEMANTIC_LINK_CAP;
    let now = 1_700_000_000_000i64;
    // Insert CAP+3 suggested-semantic edges other_i → hub, descending score.
    {
        let mut conn = db.lock();
        let tx = conn.transaction().unwrap();
        for i in 0..(cap + 3) {
            let src = format!("m{i:02}");
            // canonicalize so the pair order is stable (hub id "zhub" sorts last → src < dst).
            let score = 0.99 - (i as f64) * 0.01;
            Db::upsert_link_tx(
                &tx,
                "meeting",
                &src,
                "meeting",
                "zhub",
                "semantic",
                score,
                "auto",
                "suggested",
                now,
            )
            .unwrap();
        }
        // Now enforce the per-node cap on the hub (what auto_link_semantic does after each upsert).
        Db::trim_node_semantic_suggestions_tx(&tx, "meeting", "zhub").unwrap();
        tx.commit().unwrap();
    }
    // Only CAP suggested-semantic edges remain incident on the hub.
    assert_eq!(
        db.link_edge_count("meeting", "zhub", "semantic") as usize,
        cap,
        "a hub must keep only the top-CAP suggested-semantic edges (both-endpoint cap)"
    );
    // The survivors are the HIGHEST scores (weakest dropped): the last-inserted (lowest-score)
    // src m{cap+2} must be gone; the first (highest) m00 must remain.
    let survivors: Vec<String> = {
        let conn = db.lock();
        let mut stmt = conn
            .prepare("SELECT src_id FROM links WHERE dst_id='zhub' AND edge_type='semantic'")
            .unwrap();
        let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
        rows.map(|r| r.unwrap()).collect()
    };
    assert!(
        survivors.contains(&"m00".to_string()),
        "highest-score edge kept"
    );
    assert!(
        !survivors.contains(&format!("m{:02}", cap + 2)),
        "lowest-score edge trimmed"
    );
}

/// Fix 2 guard: the per-node trim NEVER touches an active/accepted/manual/wikilink edge — only
/// `status='suggested' AND edge_type='semantic'` rows are eligible. A hub with many ACTIVE edges
/// plus a few suggestions keeps every active edge and only its suggestions are capped.
#[test]
fn per_node_trim_never_touches_active_or_non_semantic() {
    let db = mem_db();
    let now = 1_700_000_000_000i64;
    {
        let mut conn = db.lock();
        let tx = conn.transaction().unwrap();
        // 8 ACTIVE manual edges into the hub (never suggestions).
        for i in 0..8 {
            Db::upsert_link_tx(
                &tx,
                "note",
                &format!("a{i:02}"),
                "meeting",
                "zhub",
                "manual",
                1.0,
                "user",
                "active",
                now,
            )
            .unwrap();
        }
        // Plus CAP+2 suggested-semantic edges.
        for i in 0..(crate::links::SEMANTIC_LINK_CAP + 2) {
            Db::upsert_link_tx(
                &tx,
                "meeting",
                &format!("s{i:02}"),
                "meeting",
                "zhub",
                "semantic",
                0.9 - (i as f64) * 0.01,
                "auto",
                "suggested",
                now,
            )
            .unwrap();
        }
        Db::trim_node_semantic_suggestions_tx(&tx, "meeting", "zhub").unwrap();
        tx.commit().unwrap();
    }
    assert_eq!(
        db.link_edge_count("meeting", "zhub", "manual"),
        8,
        "active manual edges must NEVER be trimmed by the semantic-suggestion cap"
    );
    assert_eq!(
        db.link_edge_count("meeting", "zhub", "semantic") as usize,
        crate::links::SEMANTIC_LINK_CAP,
        "only the suggested-semantic edges are capped"
    );
}

/// Fix 3 (RED before the indexed backlink fast path): a wikilink backlink present as a `links`
/// ROW is returned even when the SOURCE BODY no longer literally contains `[[Title]]` (the target
/// was renamed, so the body's stale old title would fail the regex scan). The old body-scan-only
/// path missed it; the fast path serves it from the id-keyed `links` row.
#[test]
fn backlinks_served_from_links_index_without_body_scan() {
    let db = mem_db();
    seed_folder(&db, "f1", "Notes");
    seed_note_doc(&db, "target", "f1", "Target", "the target body");
    // Source body links [[Target]] → index it (stores the RESOLVED target id in `links`).
    db.insert_note(
        "source",
        "f1",
        "Source",
        "Source",
        "see [[Target]] for context",
        1,
    )
    .unwrap();
    let unlocked = HashSet::new();
    db.index_wikilinks_for_source(
        crate::links::LinkKind::Note,
        "source",
        "see [[Target]] for context",
        &unlocked,
    )
    .unwrap();
    // Now RENAME the target AND rewrite the source body so it no longer contains any [[Target]]
    // OR [[Renamed]] string — only the id-keyed `links` row still connects them. The body-scan
    // legs (which match the CURRENT target title against the body) can NOT find this backlink.
    db.lock()
        .execute("UPDATE documents SET title='Renamed' WHERE id='target'", [])
        .unwrap();
    db.lock()
        .execute(
            "UPDATE documents SET text='no literal wikilink here anymore' WHERE id='source'",
            [],
        )
        .unwrap();

    let back = db
        .backlinks_for_visible(SourceKind::Note, "target", &unlocked)
        .unwrap();
    assert!(
            back.iter().any(|b| b.id == "source"),
            "a backlink present as a links row must be served from the index even when the body no longer matches"
        );
}

/// Fix 3 gating: the indexed fast path STILL fails closed on a sealed endpoint — a backlink whose
/// SOURCE is sealed-not-unlocked is hidden, and a sealed TARGET yields an empty list (no existence
/// leak) exactly as the body-scan path did.
#[test]
fn backlinks_index_fast_path_stays_visibility_gated() {
    let db = mem_db();
    seed_folder(&db, "f-open", "Open");
    seed_folder(&db, "f-secret", "Secret");
    seed_note_doc(&db, "target", "f-open", "Target", "the target body");
    db.insert_note(
        "secret-src",
        "f-secret",
        "Secret Src",
        "Secret Src",
        "see [[Target]]",
        1,
    )
    .unwrap();
    let unlocked = HashSet::new();
    db.index_wikilinks_for_source(
        crate::links::LinkKind::Note,
        "secret-src",
        "see [[Target]]",
        &unlocked,
    )
    .unwrap();
    // With everything open, the backlink is served (from the index).
    assert!(
        db.backlinks_for_visible(SourceKind::Note, "target", &unlocked)
            .unwrap()
            .iter()
            .any(|b| b.id == "secret-src"),
        "open backlink is served"
    );
    // Seal the SOURCE folder → the index fast path drops it (source gate).
    db.set_folder_locked("f-secret", true, None).unwrap();
    assert!(
        db.backlinks_for_visible(SourceKind::Note, "target", &unlocked)
            .unwrap()
            .iter()
            .all(|b| b.id != "secret-src"),
        "a sealed-source backlink must never leak through the indexed fast path"
    );
    // Seal the TARGET folder → an empty list (no existence leak), still gated at Gate 1.
    db.set_folder_locked("f-open", true, None).unwrap();
    assert!(
        db.backlinks_for_visible(SourceKind::Note, "target", &unlocked)
            .unwrap()
            .is_empty(),
        "a sealed target reveals nothing (Gate 1) even with the fast path"
    );
}

/// backlink-id fix (RED before the fan-out guard): the title-string body-scan legs must NOT
/// attribute a source's `[[Untitled]]` to EVERY item titled "Untitled" — only to the ONE that
/// `resolve_wikilink("Untitled")` actually navigates to. Two VISIBLE notes A and B are BOTH titled
/// "Untitled"; B is `updated_at`-newer so the resolver (`ORDER BY updated_at DESC`) picks B. Source
/// S carries `[[Untitled]]` in its body but is NOT in the `links` index (never indexed), forcing
/// the body-scan leg. Pre-fix (title-string match): BOTH A and B get S — a bogus fan-out. Post-fix:
/// only B (the resolved target) gets S; A gets none.
#[test]
fn backlinks_body_scan_no_fanout_across_duplicate_titles() {
    let db = mem_db();
    seed_folder(&db, "f1", "Notes");

    // A and B BOTH titled "Untitled". B is updated more recently (higher created_at ⇒ higher
    // updated_at, since insert_note stores created_at as updated_at too) so resolve_wikilink picks
    // B. Do NOT index either — their bodies are irrelevant here; they are the duplicate TARGETS.
    db.insert_note(
        "n-a",
        "f1",
        "a",
        "Untitled",
        "the A body, empty note",
        1_000,
    )
    .unwrap();
    db.insert_note(
        "n-b",
        "f1",
        "b",
        "Untitled",
        "the B body, empty note",
        2_000,
    )
    .unwrap();

    // Sanity: the resolver picks B (the newest same-titled note) — this is where a click navigates.
    let nothing = std::collections::HashSet::new();
    let resolved = db.resolve_wikilink("Untitled", &nothing).unwrap().unwrap();
    assert_eq!(
        (resolved.kind.as_str(), resolved.id.as_str()),
        ("note", "n-b"),
        "resolve_wikilink must pick the newest same-titled note (B)"
    );

    // SOURCE S carries [[Untitled]] but is deliberately NOT indexed → served only by the body scan.
    db.insert_note(
        "n-s",
        "f1",
        "s",
        "Source",
        "please see [[Untitled]] for context",
        3_000,
    )
    .unwrap();

    let a_back = db
        .backlinks_for_visible(SourceKind::Note, "n-a", &nothing)
        .unwrap();
    let b_back = db
        .backlinks_for_visible(SourceKind::Note, "n-b", &nothing)
        .unwrap();
    let a_ids: Vec<&str> = a_back.iter().map(|s| s.id.as_str()).collect();
    let b_ids: Vec<&str> = b_back.iter().map(|s| s.id.as_str()).collect();

    assert!(
        b_ids.contains(&"n-s"),
        "the resolved target (B) must get the [[Untitled]] body-scan backlink; got {b_ids:?}"
    );
    assert!(
            !a_ids.contains(&"n-s"),
            "the NON-resolved same-titled note (A) must NOT fan-out and steal the backlink; got {a_ids:?}"
        );
}

/// backlink-id fix: a UNIQUELY-titled target still gets its body-scan backlink (no regression) —
/// its title resolves to itself, so `title_targets_us` is true and the leg runs. Un-indexed source.
#[test]
fn backlinks_body_scan_unique_title_still_works() {
    let db = mem_db();
    seed_folder(&db, "f1", "Notes");

    // A uniquely-titled target T.
    db.insert_note("n-t", "f1", "t", "UniqueTitle", "the target body", 1_000)
        .unwrap();
    // A source S with [[UniqueTitle]] in its body — NOT indexed, so served only by the body scan.
    db.insert_note(
        "n-s",
        "f1",
        "s",
        "Source",
        "refer to [[UniqueTitle]] here",
        2_000,
    )
    .unwrap();

    let nothing = std::collections::HashSet::new();
    let back = db
        .backlinks_for_visible(SourceKind::Note, "n-t", &nothing)
        .unwrap();
    let ids: Vec<&str> = back.iter().map(|s| s.id.as_str()).collect();
    assert!(
        ids.contains(&"n-s"),
        "a uniquely-titled target must still surface its body-scan backlink; got {ids:?}"
    );
}

/// backlink-id fix: the id-based INDEX fast path is UNCONDITIONAL — a real indexed wikilink edge to
/// A is returned even when `title_targets_us` is false for A (a duplicate-title scenario where the
/// title resolves to a DIFFERENT same-titled item, B). The guard only ever removes BOGUS title-
/// string fan-out; it never suppresses a legitimate id-keyed edge.
#[test]
fn backlinks_index_path_unaffected_by_title_guard() {
    let db = mem_db();
    seed_folder(&db, "f1", "Notes");

    // A and B BOTH titled "Untitled"; B newer ⇒ resolve_wikilink("Untitled") picks B, so for TARGET
    // A the guard is FALSE.
    db.insert_note("n-a", "f1", "a", "Untitled", "A body", 1_000)
        .unwrap();
    db.insert_note("n-b", "f1", "b", "Untitled", "B body", 2_000)
        .unwrap();

    // SOURCE S is INDEXED against A's id directly (a resolved, id-keyed `links` edge), NOT via the
    // current title. Build the edge by indexing S while A is the resolvable target, then flip so the
    // title now resolves to B — the id-keyed edge to A must remain.
    db.insert_note("n-s", "f1", "s", "Source", "see [[A Unique Handle]]", 3_000)
        .unwrap();
    // Give A a unique title momentarily so the wikilink resolves to A's id and gets indexed.
    db.lock()
        .execute(
            "UPDATE documents SET title='A Unique Handle' WHERE id='n-a'",
            [],
        )
        .unwrap();
    let nothing = std::collections::HashSet::new();
    db.index_wikilinks_for_source(
        crate::links::LinkKind::Note,
        "n-s",
        "see [[A Unique Handle]]",
        &nothing,
    )
    .unwrap();
    // Now rename A back to "Untitled" (duplicate with B, B newer) so title resolution points at B.
    db.lock()
        .execute("UPDATE documents SET title='Untitled' WHERE id='n-a'", [])
        .unwrap();

    // For target A: title_targets_us is FALSE (resolve_wikilink("Untitled") → B), yet the id-keyed
    // index edge S → A must still surface.
    let a_back = db
        .backlinks_for_visible(SourceKind::Note, "n-a", &nothing)
        .unwrap();
    assert!(
        a_back.iter().any(|s| s.id == "n-s"),
        "the id-keyed INDEX edge to A must survive the title guard; got {:?}",
        a_back.iter().map(|s| s.id.as_str()).collect::<Vec<_>>()
    );
}

/// Fix 6 (RED before case-insensitive wikilink resolution): Obsidian resolves `[[links]]`
/// case-insensitively — `[[project x]]` must resolve to a note titled "Project X". The old
/// byte-exact match returned `None`.
#[test]
fn resolve_wikilink_is_case_insensitive() {
    let db = mem_db();
    seed_folder(&db, "f1", "Notes");
    seed_note_doc(&db, "px", "f1", "Project X", "the project x body");
    let unlocked = HashSet::new();
    // Lowercased link text resolves to the mixed-case title.
    let target = db.resolve_wikilink("project x", &unlocked).unwrap();
    assert!(
        target.is_some(),
        "[[project x]] must resolve to the 'Project X' note"
    );
    let t = target.unwrap();
    assert_eq!(t.kind, "note");
    assert_eq!(t.id, "px");
    // An EXACT match still wins/works unchanged.
    assert_eq!(
        db.resolve_wikilink("Project X", &unlocked)
            .unwrap()
            .map(|t| t.id),
        Some("px".to_string())
    );
}

/// Fix 6 meeting leg: a lowercased `[[meeting title]]` resolves the meeting when no note matches.
#[test]
fn resolve_wikilink_case_insensitive_meeting_leg() {
    let db = mem_db();
    let mut m = sample_meeting("mm1", "2026-06-24T10:00:00Z");
    m.title = Some("Weekly Standup".to_string());
    db.insert_meeting(&m).unwrap();
    note_for(&db, "mm1", "claude_code", "notes");
    let unlocked = HashSet::new();
    let target = db.resolve_wikilink("weekly standup", &unlocked).unwrap();
    assert_eq!(
        target.map(|t| (t.kind, t.id)),
        Some(("meeting".to_string(), "mm1".to_string())),
        "[[weekly standup]] must resolve the 'Weekly Standup' meeting case-insensitively"
    );
}

// ── note↔meeting-links PR-1 — MANUAL edge (DB layer) ─────────────────────────────────────────

/// A user-initiated `manual` edge is created as a DIRECTED `active`/`user`/score=1.0 row, and its
/// gated reader hides it when EITHER endpoint is sealed-not-unlocked (both directions) — restored
/// when the sealed side is session-unlocked. Same both-endpoint gate the wikilink edge rides.
#[test]
fn manual_link_row_created_and_gated_both_endpoints() {
    let db = mem_db();
    seed_folder(&db, "f-open", "Open");
    seed_folder(&db, "f-secret", "Secret");
    seed_note_doc(&db, "open", "f-open", "Open Note", "");
    seed_note_doc(&db, "secret", "f-secret", "Secret Note", "");

    // Create the manual edge open → secret.
    db.upsert_manual_link("note", "open", "note", "secret")
        .unwrap();
    assert_eq!(
        link_count(&db, "note", "open", "manual"),
        1,
        "one directed manual row exists"
    );
    // It is `active`/`user`/1.0.
    let (created_by, status, score): (String, String, f64) = db
        .lock()
        .query_row(
            "SELECT created_by, status, score FROM links WHERE edge_type='manual'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(created_by, "user");
    assert_eq!(status, "active");
    assert!((score - 1.0).abs() < 1e-9);

    // Both open → visible from either side.
    let nothing = HashSet::new();
    assert_eq!(
        db.links_for_visible(crate::links::LinkKind::Note, "open", &nothing)
            .unwrap()
            .len(),
        1,
        "manual edge visible when both endpoints open (src side)"
    );
    assert_eq!(
        db.links_for_visible(crate::links::LinkKind::Note, "secret", &nothing)
            .unwrap()
            .len(),
        1,
        "manual edge visible from the dst side too"
    );

    // Seal the SECRET folder → the manual edge vanishes from BOTH directions (neighbour gate on
    // the open side; queried-item existence gate on the secret side).
    db.set_folder_locked("f-secret", true, None).unwrap();
    assert!(
        db.links_for_visible(crate::links::LinkKind::Note, "open", &nothing)
            .unwrap()
            .is_empty(),
        "manual edge hidden from the open side when its neighbour is sealed"
    );
    assert!(
        db.links_for_visible(crate::links::LinkKind::Note, "secret", &nothing)
            .unwrap()
            .is_empty(),
        "a sealed queried item never reveals it HAS a manual link"
    );

    // Session-unlock the secret folder → the manual edge is restored on both sides.
    let mut unlocked = HashSet::new();
    unlocked.insert("f-secret".to_string());
    let edges = db
        .links_for_visible(crate::links::LinkKind::Note, "open", &unlocked)
        .unwrap();
    assert_eq!(
        edges.len(),
        1,
        "manual edge restored once secret is unlocked"
    );
    assert_eq!(edges[0].edge_type, "manual");
    assert!(
        edges[0].manual,
        "the surviving chip is flagged user-removable"
    );
}

/// DISPLAY DEDUPE: a `manual` edge AND a `wikilink` edge for the SAME `(other_kind, other_id)`
/// pair collapse to ONE `links_for_visible` chip — preferring the deterministic `wikilink`
/// `edge_type` (its stable id) but flagging `manual=true` so the FE renders the removable `×`.
#[test]
fn links_for_visible_dedupes_manual_and_wikilink() {
    let db = mem_db();
    seed_folder(&db, "f1", "Notes");
    seed_note_doc(&db, "target", "f1", "Target", "the target body");
    seed_note_doc(&db, "source", "f1", "Source", "");

    let unlocked = HashSet::new();
    // A manual edge source → target.
    db.upsert_manual_link("note", "source", "note", "target")
        .unwrap();
    // AND a wikilink edge for the SAME pair (as if the [[Target]] materialized into the body).
    db.index_wikilinks_for_source(
        crate::links::LinkKind::Note,
        "source",
        "see [[Target]] for context",
        &unlocked,
    )
    .unwrap();
    // Two raw rows for the pair (manual + wikilink)...
    assert_eq!(link_count(&db, "note", "source", "manual"), 1);
    assert_eq!(link_count(&db, "note", "source", "wikilink"), 1);

    // ...but ONE collapsed chip in the reader.
    let edges = db
        .links_for_visible(crate::links::LinkKind::Note, "source", &unlocked)
        .unwrap();
    let to_target: Vec<_> = edges.iter().filter(|e| e.other_id == "target").collect();
    assert_eq!(to_target.len(), 1, "manual + wikilink collapse to ONE chip");
    assert_eq!(
        to_target[0].edge_type, "wikilink",
        "the deterministic edge type wins the collapse"
    );
    assert!(
        to_target[0].manual,
        "the collapsed chip is flagged as a removable manual link"
    );
}

/// The display representative may point in the opposite direction from the exact manual row. The
/// chip must carry that stored tuple rather than asking the FE to reconstruct it from `direction`.
#[test]
fn links_for_visible_preserves_opposite_manual_tuple_under_wikilink() {
    let db = mem_db();
    seed_folder(&db, "f1", "Notes");
    seed_note_doc(&db, "source", "f1", "Source", "");
    seed_note_doc(&db, "target", "f1", "Target", "");
    let unlocked = HashSet::new();

    db.index_wikilinks_for_source(
        crate::links::LinkKind::Note,
        "source",
        "see [[Target]]",
        &unlocked,
    )
    .unwrap();
    db.upsert_manual_link("note", "target", "note", "source")
        .unwrap();

    let edges = db
        .links_for_visible(crate::links::LinkKind::Note, "source", &unlocked)
        .unwrap();
    let chip = edges.iter().find(|edge| edge.other_id == "target").unwrap();
    assert_eq!(chip.edge_type, "wikilink");
    assert_eq!(chip.direction, "out");
    assert_eq!(
        chip.manual_edges,
        vec![crate::storage::models::ManualLinkEdge {
            src_kind: "note".into(),
            src_id: "target".into(),
            dst_kind: "note".into(),
            dst_id: "source".into(),
        }],
        "the removable chip must retain the opposite directed manual tuple"
    );
    let wire = serde_json::to_value(chip).unwrap();
    let tuple = &wire["manualEdges"][0];
    assert_eq!(tuple["srcKind"], "note");
    assert_eq!(tuple["srcId"], "target");
    assert_eq!(tuple["dstKind"], "note");
    assert_eq!(tuple["dstId"], "source");
    assert!(
        tuple.get("src_kind").is_none() && tuple.get("dst_kind").is_none(),
        "the IPC tuple must serialize with camelCase keys"
    );
}

/// Both directed manual rows collapse to one neighbour chip, but neither exact unlink handle may be
/// lost: one click must be able to remove the complete hidden set atomically.
#[test]
fn links_for_visible_preserves_bidirectional_manual_tuples() {
    let db = mem_db();
    seed_folder(&db, "f1", "Notes");
    seed_note_doc(&db, "left", "f1", "Left", "");
    seed_note_doc(&db, "right", "f1", "Right", "");
    db.upsert_manual_link("note", "left", "note", "right")
        .unwrap();
    db.upsert_manual_link("note", "right", "note", "left")
        .unwrap();

    let edges = db
        .links_for_visible(crate::links::LinkKind::Note, "left", &HashSet::new())
        .unwrap();
    let chip = edges.iter().find(|edge| edge.other_id == "right").unwrap();
    assert_eq!(edges.len(), 1, "the pair remains one displayed chip");
    assert_eq!(
        chip.manual_edges,
        vec![
            crate::storage::models::ManualLinkEdge {
                src_kind: "note".into(),
                src_id: "left".into(),
                dst_kind: "note".into(),
                dst_id: "right".into(),
            },
            crate::storage::models::ManualLinkEdge {
                src_kind: "note".into(),
                src_id: "right".into(),
                dst_kind: "note".into(),
                dst_id: "left".into(),
            },
        ],
        "both exact directed manual tuples must survive display collapse"
    );
}

/// A pair carrying BOTH a user `manual` (active) edge AND an auto `semantic` (suggested) edge —
/// realistic, since a manually-linked pair is often also content-similar — must collapse to the
/// ACTIVE, removable MANUAL chip, NOT a semantic Accept/Dismiss suggestion. RED before the
/// `edge_rank` swap (a `manual` link must outrank a semantic SUGGESTION): the surviving chip was
/// `edge_type="semantic" status="suggested"`, so the FE rendered the user's active link as an
/// un-removable suggestion. (The manual+wikilink dedupe test above never exercised this collision,
/// because both of its edges are non-semantic/active.)
#[test]
fn links_for_visible_manual_beats_suggested_semantic() {
    let db = mem_db();
    seed_folder(&db, "f1", "Notes");
    seed_note_doc(&db, "target", "f1", "Target", "the target body");
    seed_note_doc(&db, "source", "f1", "Source", "");
    let unlocked = HashSet::new();
    // A user MANUAL link source → target...
    db.upsert_manual_link("note", "source", "note", "target")
        .unwrap();
    // ...AND an auto SEMANTIC SUGGESTION for the SAME pair.
    {
        let now = 1_700_000_000_000i64;
        let mut conn = db.lock();
        let tx = conn.transaction().unwrap();
        Db::upsert_link_tx(
            &tx,
            "note",
            "source",
            "note",
            "target",
            "semantic",
            0.9,
            "auto",
            "suggested",
            now,
        )
        .unwrap();
        tx.commit().unwrap();
    }
    assert_eq!(link_count(&db, "note", "source", "manual"), 1);
    assert_eq!(link_count(&db, "note", "source", "semantic"), 1);

    let edges = db
        .links_for_visible(crate::links::LinkKind::Note, "source", &unlocked)
        .unwrap();
    let to_target: Vec<_> = edges.iter().filter(|e| e.other_id == "target").collect();
    assert_eq!(to_target.len(), 1, "manual + semantic collapse to ONE chip");
    let chip = to_target[0];
    assert!(chip.manual, "the collapsed chip is a removable manual link");
    // Load-bearing: the chip must NOT read as a suggested-semantic row — the FE partitions
    // `edge_type=='semantic' && status=='suggested'` as an Accept/Dismiss suggestion, EXCLUDED
    // from the removable deterministic chips (so a user's active link would lose its `×`).
    assert!(
        !(chip.edge_type == "semantic" && chip.status == "suggested"),
        "a manually-linked pair must never render as an unconfirmed suggestion; got {} / {}",
        chip.edge_type,
        chip.status
    );
    assert_eq!(chip.status, "active", "a user manual link is active");
}

/// A pair with ONLY a `wikilink` edge (no manual) is NOT flagged `manual` — the FE renders it as
/// an auto (non-removable) chip. Guards the collapse against over-flagging.
#[test]
fn links_for_visible_non_manual_pair_not_flagged() {
    let db = mem_db();
    seed_folder(&db, "f1", "Notes");
    seed_note_doc(&db, "target", "f1", "Target", "the target body");
    seed_note_doc(&db, "source", "f1", "Source", "");
    let unlocked = HashSet::new();
    db.index_wikilinks_for_source(
        crate::links::LinkKind::Note,
        "source",
        "see [[Target]] for context",
        &unlocked,
    )
    .unwrap();
    let edges = db
        .links_for_visible(crate::links::LinkKind::Note, "source", &unlocked)
        .unwrap();
    let chip = edges.iter().find(|e| e.other_id == "target").unwrap();
    assert_eq!(chip.edge_type, "wikilink");
    assert!(
        !chip.manual,
        "a pure wikilink chip is not a removable manual link"
    );
}

/// PURGE-ON-SEAL is NOT edge-type-agnostic, and this test used to say it was.
///
/// It pinned the behaviour that sealing a folder destroys every `manual` edge touching it, and by
/// pinning it made the loss look deliberate. It is not survivable: a manual edge is the ONLY record
/// of a link the user made by hand — `link_items` writes the row and no body marker — so nothing
/// re-derives it on unlock. Locking a folder for an afternoon silently deleted the connections the
/// user built, and returned the automatic ones.
///
/// What must hold instead, and what this test now checks: the row SURVIVES the seal at rest, stays
/// INVISIBLE through the gated reader from the still-open side while the neighbour is sealed, and is
/// visible again once the folder is unlocked. Only the decision survives, never its visibility.
#[test]
fn a_manual_link_survives_a_seal_at_rest_but_stays_invisible_until_unlock() {
    let db = mem_db();
    seed_folder(&db, "f-open", "Open");
    seed_folder(&db, "f-secret", "Secret");
    seed_note_doc(&db, "open", "f-open", "Open Note", "");
    seed_note_doc(&db, "secret", "f-secret", "Secret Note", "");

    db.upsert_manual_link("note", "open", "note", "secret")
        .unwrap();
    assert_eq!(link_count(&db, "note", "secret", "manual"), 1);

    // Seal the SECRET folder and run the relock reblank (carries the `purge_links_tx` leg).
    db.set_folder_locked("f-secret", true, None).unwrap();
    let mut folders = HashSet::new();
    folders.insert("f-secret".to_string());
    db.blank_sealed_notes_in_folders(&folders).unwrap();

    // AT REST: the user's decision is still on the books. It carries ids only — no title, no body.
    assert_eq!(
        link_count(&db, "note", "secret", "manual"),
        1,
        "a hand-made link is not derivable from anything else, so the seal must not destroy it"
    );

    // THROUGH THE GATE: from the still-open side the sealed neighbour is not disclosed at all —
    // neither its title nor the fact that an edge reaches it.
    let sealed = HashSet::new();
    let edges = db
        .links_for_visible(crate::links::LinkKind::Note, "open", &sealed)
        .unwrap();
    assert!(
        !edges.iter().any(|e| e.other_id == "secret"),
        "the preserved row must stay invisible while its neighbour is sealed — got {edges:?}"
    );

    // UNLOCKED: the same edge the user made is back, unchanged.
    db.set_folder_locked("f-secret", false, None).unwrap();
    let mut unlocked = HashSet::new();
    unlocked.insert("f-secret".to_string());
    let edges = db
        .links_for_visible(crate::links::LinkKind::Note, "open", &unlocked)
        .unwrap();
    let restored = edges
        .iter()
        .find(|e| e.other_id == "secret")
        .expect("the manual link must be visible again after unlock");
    assert_eq!(restored.edge_type, "manual");
    assert!(restored.manual, "and it must still read as a removable link");
}

/// The invisibility above is checked from the OPEN side; this checks the other three surfaces a
/// preserved row could escape through, because "the row survives" and "the row is hidden" are
/// separate properties and only the first is obvious from the constant.
#[test]
fn a_preserved_manual_link_is_hidden_from_the_sealed_side_and_from_the_graph() {
    let db = mem_db();
    seed_folder(&db, "f-open", "Open");
    seed_folder(&db, "f-secret", "Secret");
    seed_note_doc(&db, "open", "f-open", "Open Note", "");
    seed_note_doc(&db, "secret", "f-secret", "Secret Note", "");
    db.upsert_manual_link("note", "open", "note", "secret")
        .unwrap();

    db.set_folder_locked("f-secret", true, None).unwrap();
    let mut folders = HashSet::new();
    folders.insert("f-secret".to_string());
    db.blank_sealed_notes_in_folders(&folders).unwrap();
    let sealed = HashSet::new();

    // The SEALED side answers nothing at all — GATE 1 refuses before any edge is considered, so the
    // open neighbour's id never appears either.
    assert!(
        db.links_for_visible(crate::links::LinkKind::Note, "secret", &sealed)
            .unwrap()
            .is_empty(),
        "querying the sealed item itself must disclose no edges"
    );

    // The full-brain graph reads `links` raw and filters in the caller, so it is the surface where a
    // preserved row is most likely to escape. No node for the sealed note, and no edge touching it.
    let graph = db
        .build_full_graph(&sealed, crate::storage::models::FullGraphOpts::default())
        .unwrap();
    assert!(
        !graph.nodes.iter().any(|n| n.id == "secret"),
        "the sealed note must not be a graph node"
    );
    assert!(
        !graph
            .edges
            .iter()
            .any(|e| e.src == "secret" || e.dst == "secret"),
        "no edge may touch the sealed note — got {:?}",
        graph.edges
    );
}

/// The other three edge kinds are genuinely derived, so the seal must STILL purge them — otherwise a
/// wikilink the user deleted from a note body while the folder was closed would come back on unlock.
/// This is the control that keeps the fix above from widening into "seals stop purging links".
#[test]
fn a_seal_still_purges_the_derived_edge_kinds() {
    let db = mem_db();
    seed_folder(&db, "f-open", "Open");
    seed_folder(&db, "f-secret", "Secret");
    seed_note_doc(&db, "open", "f-open", "Open Note", "links [[Secret Note]]");
    seed_note_doc(&db, "secret", "f-secret", "Secret Note", "body");

    let nothing = HashSet::new();
    db.index_wikilinks_for_source(
        crate::links::LinkKind::Note,
        "open",
        "links [[Secret Note]]",
        &nothing,
    )
    .unwrap();
    assert_eq!(link_count(&db, "note", "open", "wikilink"), 1);

    db.set_folder_locked("f-secret", true, None).unwrap();
    let mut folders = HashSet::new();
    folders.insert("f-secret".to_string());
    db.blank_sealed_notes_in_folders(&folders).unwrap();

    assert_eq!(
        link_count(&db, "note", "open", "wikilink"),
        0,
        "a wikilink is re-derived from the body on unlock, so it must not be preserved"
    );

    // Companion is the other `created_by='user'` derived kind, and the one that would silently ride
    // along if the preserved class were ever keyed on `created_by` instead of `edge_type`.
    let db2 = mem_db();
    seed_folder(&db2, "f-locked", "Secret");
    db2.insert_meeting(&sample_meeting("m1", "2026-06-24T10:00:00Z"))
        .unwrap();
    note_for(&db2, "m1", "claude_code", "meeting note body");
    db2.set_note_folder("m1", Some("f-locked")).unwrap();
    seed_note_doc(&db2, "companion", "f-locked", "Companion", "typed notes");
    db2.set_document_meeting_id("companion", "m1").unwrap();
    db2.set_companion_link("companion", "m1").unwrap();
    assert_eq!(link_count(&db2, "meeting", "m1", "companion"), 1);

    db2.set_folder_locked("f-locked", true, None).unwrap();
    let mut folders2 = HashSet::new();
    folders2.insert("f-locked".to_string());
    db2.blank_sealed_notes_in_folders(&folders2).unwrap();
    assert_eq!(
        link_count(&db2, "meeting", "m1", "companion"),
        0,
        "a companion edge is structural and re-derived on unlock — it must not be preserved"
    );
}

/// PURGE-ON-SEAL, BOTH DIRECTIONS (existence-leak RED→GREEN): a wikilink edge between two notes in
/// DIFFERENT folders is purged when EITHER folder is sealed — from the sealed side AND from the
/// still-open side (the open note's reader must not surface the sealed neighbour).
#[test]
fn purge_links_tx_drops_edges_on_seal_both_directions() {
    let db = mem_db();
    seed_folder(&db, "f-open", "Open");
    seed_folder(&db, "f-secret", "Secret");
    seed_note_doc(&db, "open", "f-open", "Open Note", "links [[Secret Note]]");
    seed_note_doc(&db, "secret", "f-secret", "Secret Note", "the secret body");

    let nothing = HashSet::new();
    db.index_wikilinks_for_source(
        crate::links::LinkKind::Note,
        "open",
        "links [[Secret Note]]",
        &nothing,
    )
    .unwrap();
    assert_eq!(
        link_count(&db, "note", "open", "wikilink"),
        1,
        "edge exists before seal"
    );

    // Seal the SECRET folder (its document is a to-be-sealed endpoint). Set locked + run the
    // relock reblank, which carries the `purge_links_tx` leg.
    db.seal_note("secret", "claude_code", b"x").ok(); // best-effort (documents have no notes row)
    db.set_folder_locked("f-secret", true, None).unwrap();
    let mut folders = HashSet::new();
    folders.insert("f-secret".to_string());
    db.blank_sealed_notes_in_folders(&folders).unwrap();

    // Edge is gone at rest (no `links` row survives for the sealed endpoint) — BOTH directions.
    assert_eq!(
        link_count(&db, "note", "secret", "wikilink"),
        0,
        "sealed endpoint's edge purged (dst side)"
    );
    assert_eq!(
        link_count(&db, "note", "open", "wikilink"),
        0,
        "open source's edge to the sealed note purged (src side)"
    );

    // And the gated reader on the OPEN note surfaces nothing about the sealed neighbour.
    let edges = db
        .links_for_visible(crate::links::LinkKind::Note, "open", &nothing)
        .unwrap();
    assert!(
        edges.is_empty(),
        "open note's link list must not name the sealed neighbour"
    );
}

/// BOTH-ENDPOINT READ GATE: even if a stray `links` row survived (defense-in-depth), the reader
/// hides an edge whose OTHER endpoint is sealed-not-unlocked, AND returns empty when the QUERIED
/// item is itself sealed (existence-leak guard) — the two-gate model.
#[test]
fn links_for_visible_gates_both_endpoints() {
    let db = mem_db();
    seed_folder(&db, "f-open", "Open");
    seed_folder(&db, "f-secret", "Secret");
    seed_note_doc(&db, "open", "f-open", "Open Note", "");
    seed_note_doc(&db, "secret", "f-secret", "Secret Note", "");
    // Insert a raw edge open → secret WITHOUT purging (simulate a stray survivor).
    let now = 1_700_000_000_000i64;
    {
        let mut conn = db.lock();
        let tx = conn.transaction().unwrap();
        Db::upsert_link_tx(
            &tx, "note", "open", "note", "secret", "wikilink", 1.0, "user", "active", now,
        )
        .unwrap();
        tx.commit().unwrap();
    }
    db.set_folder_locked("f-secret", true, None).unwrap();

    // GATE 2 (neighbour sealed): querying the OPEN note with an empty unlock set drops the edge.
    let nothing = HashSet::new();
    let edges = db
        .links_for_visible(crate::links::LinkKind::Note, "open", &nothing)
        .unwrap();
    assert!(
        edges.is_empty(),
        "edge to a sealed neighbour must not surface (neighbour gate)"
    );

    // GATE 1 (queried item sealed): querying the SECRET note itself returns empty (existence leak
    // guard) — even though the stray edge is incident on it.
    let edges = db
        .links_for_visible(crate::links::LinkKind::Note, "secret", &nothing)
        .unwrap();
    assert!(
        edges.is_empty(),
        "a sealed queried item must not reveal it HAS links (queried-item gate)"
    );

    // Session-unlock the secret folder → both endpoints visible → the edge appears from either side.
    let mut unlocked = HashSet::new();
    unlocked.insert("f-secret".to_string());
    let edges = db
        .links_for_visible(crate::links::LinkKind::Note, "open", &unlocked)
        .unwrap();
    assert_eq!(edges.len(), 1, "both-visible edge surfaces once");
    assert_eq!(edges[0].other_id, "secret");
    assert_eq!(edges[0].other_title, "Secret Note");
}

/// Seed a meeting M (titled, visible) + its companion note N (`documents.meeting_id = M`, same
/// title by construction) both linked to an anchor note A, and return the ids so the four
/// companion-collapse tests share one setup. `note_edge_type`/`meeting_edge_type` pick the
/// edge_type of the A→N and A→M edges so a test can make either `manual`.
fn seed_companion_collapse_case(db: &Db, note_edge_type: &str, meeting_edge_type: &str) {
    seed_folder(db, "f-open", "Open");
    // Anchor note A we query from.
    seed_note_doc(db, "anchor", "f-open", "Anchor Note", "");
    // Meeting M (titled, no notes → visible).
    let mut m = sample_meeting("m", "2026-06-24T10:00:00Z");
    m.title = Some("Weekly Sync".to_string());
    db.insert_meeting(&m).unwrap();
    // Companion note N: same title as M, structurally tied via documents.meeting_id.
    seed_note_doc(db, "compnote", "f-open", "Weekly Sync", "");
    db.set_document_meeting_id("compnote", "m").unwrap();
    let now = 1_700_000_000_000i64;
    let mut conn = db.lock();
    let tx = conn.transaction().unwrap();
    // A → M (meeting edge) and A → N (companion-note edge).
    Db::upsert_link_tx(
        &tx,
        "note",
        "anchor",
        "meeting",
        "m",
        meeting_edge_type,
        1.0,
        "user",
        "active",
        now,
    )
    .unwrap();
    Db::upsert_link_tx(
        &tx,
        "note",
        "anchor",
        "note",
        "compnote",
        note_edge_type,
        1.0,
        "user",
        "active",
        now,
    )
    .unwrap();
    tx.commit().unwrap();
}

/// COMPANION COLLAPSE (2026-07-19) — the core case: an anchor linked to a meeting M (auto edge) AND
/// to M's companion note N (auto edge, same title by construction) collapses to ONE chip — the
/// MEETING survives, the companion-note edge is dropped. RED before the transform: the result
/// carried BOTH `(meeting, m)` and `(note, compnote)` (two chips, same title).
#[test]
fn links_for_visible_collapses_meeting_and_its_companion_note() {
    let db = mem_db();
    // Both edges AUTO: companion note edge is `companion`, meeting edge is `wikilink`.
    seed_companion_collapse_case(&db, "companion", "wikilink");
    let nothing = HashSet::new();
    let edges = db
        .links_for_visible(crate::links::LinkKind::Note, "anchor", &nothing)
        .unwrap();
    // Exactly one chip: the MEETING (canonical entity); the companion-note edge is folded away.
    assert_eq!(
        edges.len(),
        1,
        "meeting + its companion note collapse to one chip"
    );
    assert_eq!(
        edges[0].other_kind, "meeting",
        "the surviving chip is the meeting"
    );
    assert_eq!(edges[0].other_id, "m");
    assert!(
        !edges
            .iter()
            .any(|e| e.other_kind == "note" && e.other_id == "compnote"),
        "the companion-note edge must be dropped, not shown as a second chip"
    );
}

/// COMPANION COLLAPSE — removability guard: if the anchor→companion-note edge is `manual` (the user
/// explicitly linked that note), it must NOT collapse — both M and N survive so the user can still
/// see and remove the manual link. Guards invariant 3 (no lost removability / no silent reappear).
#[test]
fn links_for_visible_keeps_manual_companion_note() {
    let db = mem_db();
    // Note edge is MANUAL; meeting edge is auto.
    seed_companion_collapse_case(&db, "manual", "wikilink");
    let nothing = HashSet::new();
    let edges = db
        .links_for_visible(crate::links::LinkKind::Note, "anchor", &nothing)
        .unwrap();
    assert_eq!(
        edges.len(),
        2,
        "a manual companion-note link is preserved (removable)"
    );
    assert!(
        edges
            .iter()
            .any(|e| e.other_kind == "meeting" && e.other_id == "m"),
        "meeting chip survives"
    );
    let note_chip = edges
        .iter()
        .find(|e| e.other_kind == "note" && e.other_id == "compnote")
        .expect("manual companion-note chip must survive");
    assert!(
        note_chip.manual,
        "the surviving note chip is flagged manual (removable ×)"
    );
}

/// COMPANION COLLAPSE — degrade gracefully: an anchor linked to the companion note N but NOT to its
/// meeting M → N survives (there is no meeting neighbour to fold into). Invariant 4.
#[test]
fn links_for_visible_keeps_companion_note_when_meeting_absent() {
    let db = mem_db();
    seed_folder(&db, "f-open", "Open");
    seed_note_doc(&db, "anchor", "f-open", "Anchor Note", "");
    let mut m = sample_meeting("m", "2026-06-24T10:00:00Z");
    m.title = Some("Weekly Sync".to_string());
    db.insert_meeting(&m).unwrap();
    seed_note_doc(&db, "compnote", "f-open", "Weekly Sync", "");
    db.set_document_meeting_id("compnote", "m").unwrap();
    let now = 1_700_000_000_000i64;
    {
        let mut conn = db.lock();
        let tx = conn.transaction().unwrap();
        // ONLY the companion-note edge exists on the anchor; the meeting is NOT a neighbour.
        Db::upsert_link_tx(
            &tx,
            "note",
            "anchor",
            "note",
            "compnote",
            "companion",
            1.0,
            "user",
            "active",
            now,
        )
        .unwrap();
        tx.commit().unwrap();
    }
    let nothing = HashSet::new();
    let edges = db
        .links_for_visible(crate::links::LinkKind::Note, "anchor", &nothing)
        .unwrap();
    assert_eq!(
        edges.len(),
        1,
        "companion note with no meeting neighbour is kept"
    );
    assert_eq!(edges[0].other_kind, "note");
    assert_eq!(edges[0].other_id, "compnote");
}

/// COMPANION COLLAPSE — STRUCTURAL only: two notes that merely SHARE a title (no
/// `documents.meeting_id` relationship) must BOTH survive — the collapse keys on the structural
/// companion link, never a title-string match. Invariant 4 (never a title collision collapse).
#[test]
fn links_for_visible_does_not_collapse_unrelated_same_title() {
    let db = mem_db();
    seed_folder(&db, "f-open", "Open");
    seed_note_doc(&db, "anchor", "f-open", "Anchor Note", "");
    // A meeting AND a standalone note that happen to share a title, but the note is NOT the
    // meeting's companion (no meeting_id set).
    let mut m = sample_meeting("m", "2026-06-24T10:00:00Z");
    m.title = Some("Weekly Sync".to_string());
    db.insert_meeting(&m).unwrap();
    seed_note_doc(&db, "othernote", "f-open", "Weekly Sync", "");
    // deliberately NO set_document_meeting_id — the title match is coincidental.
    let now = 1_700_000_000_000i64;
    {
        let mut conn = db.lock();
        let tx = conn.transaction().unwrap();
        Db::upsert_link_tx(
            &tx, "note", "anchor", "meeting", "m", "wikilink", 1.0, "user", "active", now,
        )
        .unwrap();
        Db::upsert_link_tx(
            &tx,
            "note",
            "anchor",
            "note",
            "othernote",
            "wikilink",
            1.0,
            "user",
            "active",
            now,
        )
        .unwrap();
        tx.commit().unwrap();
    }
    let nothing = HashSet::new();
    let edges = db
        .links_for_visible(crate::links::LinkKind::Note, "anchor", &nothing)
        .unwrap();
    assert_eq!(
        edges.len(),
        2,
        "same-title-but-unrelated note and meeting both survive"
    );
    assert!(edges
        .iter()
        .any(|e| e.other_kind == "meeting" && e.other_id == "m"));
    assert!(edges
        .iter()
        .any(|e| e.other_kind == "note" && e.other_id == "othernote"));
}

/// Fix 0 (brain-v3 audit) — LINK-WRITER TOCTOU: a `index_wikilinks_for_source` pass that RESOLVED
/// its target while a neighbour was visible must NOT insert an edge if that neighbour SEALED AT
/// REST before the write commits. Simulate the race by sealing the target's document
/// (`text_blob` set + `text` blanked) BEFORE calling the writer with a stale (empty) unlock set:
/// the resolve leg would still find it (targets are resolved via `resolve_wikilink` against the
/// snapshot), but the in-tx re-check must drop it. RED before Fix 0: the writer inserted a
/// wikilink row naming the sealed neighbour.
#[test]
fn index_wikilinks_refuses_sealed_at_rest_target() {
    let db = mem_db();
    seed_folder(&db, "f-open", "Open");
    seed_folder(&db, "f-secret", "Secret");
    seed_note_doc(&db, "open", "f-open", "Open Note", "");
    seed_note_doc(&db, "secret", "f-secret", "Secret Note", "the secret body");
    // Resolve the title FIRST while visible (mirrors the outside-tx snapshot), then seal-at-rest.
    let unlocked = HashSet::new();
    assert!(
        db.resolve_wikilink("Secret Note", &unlocked)
            .unwrap()
            .is_some(),
        "target resolves while visible (the stale snapshot the writer captured)"
    );
    // Now the target seals at rest between the resolve and the writer's own commit.
    db.seal_document("secret", b"ciphertext").unwrap();
    // The writer runs with the STALE snapshot; the in-tx re-check must drop the sealed target.
    db.index_wikilinks_for_source(
        crate::links::LinkKind::Note,
        "open",
        "links to [[Secret Note]]",
        &unlocked,
    )
    .unwrap();
    assert_eq!(
        link_count(&db, "note", "secret", "wikilink"),
        0,
        "a wikilink to a now-sealed-at-rest target must not be inserted (TOCTOU)"
    );
}

/// Fix 0 (brain-v3 audit) — LINK-WRITER TOCTOU, SOURCE side: a re-derive whose OWN source sealed at
/// rest mid-flight must write NOTHING (not even delete-then-insert its own wikilink rows). RED
/// before Fix 0: the DELETE-THEN-INSERT ran and re-inserted the source's edges behind the lock.
#[test]
fn index_wikilinks_refuses_when_source_sealed_at_rest() {
    let db = mem_db();
    seed_folder(&db, "f-src", "Src");
    seed_folder(&db, "f-tgt", "Tgt");
    seed_note_doc(&db, "src", "f-src", "Src Note", "");
    seed_note_doc(&db, "tgt", "f-tgt", "Tgt Note", "the target body");
    // Source is sealed at rest (a lock committed between the caller's snapshot and this write).
    db.seal_document("src", b"ciphertext").unwrap();
    let unlocked = HashSet::new();
    db.index_wikilinks_for_source(
        crate::links::LinkKind::Note,
        "src",
        "links to [[Tgt Note]]",
        &unlocked,
    )
    .unwrap();
    assert_eq!(
        link_count(&db, "note", "src", "wikilink"),
        0,
        "a sealed-at-rest source writes no wikilink edges"
    );
}

/// Fix 0 (brain-v3 audit) — LINK-WRITER TOCTOU, SEMANTIC pass: an `auto_link_semantic` whose
/// candidate sealed at rest between the (outside-tx) kNN and the upsert must not suggest it. Seal
/// one of two content-identical neighbours AFTER indexing but BEFORE the pass, with a stale
/// (empty) snapshot. RED before Fix 0: a semantic suggestion naming the sealed neighbour landed.
#[test]
fn auto_link_semantic_refuses_sealed_at_rest_neighbour() {
    let db = mem_db();
    let body = "identical clustering text for the semantic neighbour test";
    for id in ["a", "b"] {
        db.insert_meeting(&sample_meeting(id, "2026-06-24T10:00:00Z"))
            .unwrap();
        note_for(&db, id, "claude_code", body);
        db.index_meeting_chunks(id, &[], &crate::embed::StubEmbedder)
            .unwrap();
    }
    // Neighbour 'b' seals at rest (its note row: content_blob set + markdown blanked) after the
    // vectors were indexed but before the pass over 'a' runs.
    db.seal_note("b", "claude_code", b"ciphertext").unwrap();
    let unlocked = HashSet::new();
    db.auto_link_semantic(crate::links::LinkKind::Meeting, "a", &unlocked)
        .unwrap();
    assert_eq!(
        link_count(&db, "meeting", "b", "semantic"),
        0,
        "no semantic suggestion may name a now-sealed-at-rest neighbour (TOCTOU)"
    );
}

/// Fix 1 (brain-v3 audit) — DECISION rows SURVIVE the seal purge: a `dismissed` tombstone and an
/// `accepted` edge must persist across `blank_sealed_notes_in_folders` (the relock purge that runs
/// `purge_links_tx`), so a lock→unlock never resurrects a dismissed suggestion or forgets an
/// accepted edge. RED before Fix 1: the edge-type-agnostic purge deleted BOTH decision rows, so
/// their `status` count dropped to 0 at rest.
#[test]
fn seal_preserves_dismissed_and_accepted_link_decisions() {
    let db = mem_db();
    seed_folder(&db, "f-locked", "Secret");
    for id in ["a", "b", "c"] {
        db.insert_meeting(&sample_meeting(id, "2026-06-24T10:00:00Z"))
            .unwrap();
        note_for(&db, id, "claude_code", "some body text");
        db.set_note_folder(id, Some("f-locked")).unwrap();
    }
    let now = 1_700_000_000_000i64;
    {
        let mut conn = db.lock();
        let tx = conn.transaction().unwrap();
        // A DISMISSED tombstone a↔b and an ACCEPTED edge a↔c (canonicalized order not required for
        // the count assertions — we count incident rows on 'a').
        Db::upsert_link_tx(
            &tx,
            "meeting",
            "a",
            "meeting",
            "b",
            "semantic",
            0.9,
            "auto",
            "dismissed",
            now,
        )
        .unwrap();
        Db::upsert_link_tx(
            &tx, "meeting", "a", "meeting", "c", "semantic", 0.8, "accepted", "active", now,
        )
        .unwrap();
        // A DERIVED edge (auto-suggested) a↔b that MUST be purged (control).
        Db::upsert_link_tx(
            &tx,
            "meeting",
            "b",
            "meeting",
            "c",
            "semantic",
            0.7,
            "auto",
            "suggested",
            now,
        )
        .unwrap();
        tx.commit().unwrap();
    }
    // Seal the folder: blank the notes (content_blob present) + run the relock purge.
    for id in ["a", "b", "c"] {
        db.seal_note(id, "claude_code", b"ciphertext").unwrap();
    }
    let mut folders = HashSet::new();
    folders.insert("f-locked".to_string());
    db.blank_sealed_notes_in_folders(&folders).unwrap();

    // The DISMISSED tombstone survives (still incident on 'a', still dismissed).
    let dismissed: i64 = db
        .lock()
        .query_row(
            "SELECT COUNT(*) FROM links WHERE edge_type='semantic' AND status='dismissed'
                   AND src_id='a' AND dst_id='b'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        dismissed, 1,
        "a dismissed tombstone must survive the seal purge"
    );
    // The ACCEPTED edge survives.
    let accepted: i64 = db
        .lock()
        .query_row(
            "SELECT COUNT(*) FROM links WHERE status='active' AND created_by='accepted'
                   AND src_id='a' AND dst_id='c'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(accepted, 1, "an accepted edge must survive the seal purge");
    // The DERIVED suggested edge b↔c is GONE (purged as normal).
    let derived: i64 = db
        .lock()
        .query_row(
            "SELECT COUNT(*) FROM links WHERE status='suggested' AND src_id='b' AND dst_id='c'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        derived, 0,
        "a derived (auto-suggested) edge is still purged on seal"
    );
}

/// Fix 1 (brain-v3 audit) — the preserved dismissed/accepted rows STAY INVISIBLE via the
/// both-endpoint read gate while an endpoint is sealed (only the DECISION state survives, never its
/// visibility). A sealed meeting 'a' returns no links; the still-open queried side sees no edge to
/// the sealed neighbour. This is the load-bearing "preserve rows but don't leak" invariant.
#[test]
fn preserved_link_decisions_stay_invisible_while_sealed() {
    let db = mem_db();
    seed_folder(&db, "f-open", "Open");
    seed_folder(&db, "f-secret", "Secret");
    for (id, f) in [("open", "f-open"), ("secret", "f-secret")] {
        db.insert_meeting(&sample_meeting(id, "2026-06-24T10:00:00Z"))
            .unwrap();
        note_for(&db, id, "claude_code", "some body text");
        db.set_note_folder(id, Some(f)).unwrap();
    }
    let now = 1_700_000_000_000i64;
    {
        let mut conn = db.lock();
        let tx = conn.transaction().unwrap();
        // An ACCEPTED edge open↔secret — a preserved decision row.
        Db::upsert_link_tx(
            &tx, "meeting", "open", "meeting", "secret", "semantic", 0.9, "accepted", "active", now,
        )
        .unwrap();
        tx.commit().unwrap();
    }
    // Seal the SECRET folder.
    db.seal_note("secret", "claude_code", b"ciphertext")
        .unwrap();
    db.set_folder_locked("f-secret", true, None).unwrap();

    let nothing = HashSet::new();
    // The preserved accepted row is STILL invisible from the open side (neighbour sealed).
    let edges = db
        .links_for_visible(crate::links::LinkKind::Meeting, "open", &nothing)
        .unwrap();
    assert!(
        edges.iter().all(|e| e.other_id != "secret"),
        "a preserved accepted row must not surface the sealed neighbour"
    );
    // And the sealed queried item reveals no links at all (existence gate).
    let edges = db
        .links_for_visible(crate::links::LinkKind::Meeting, "secret", &nothing)
        .unwrap();
    assert!(
        edges.is_empty(),
        "a sealed queried item must not reveal it HAS links"
    );
}

/// Fix 2 (brain-v3 audit) — COMPANION edge RE-DERIVED on unlock. A companion note (in the folder)
/// links to its meeting via the `companion` edge; the seal purge drops it (not a preserved decision
/// row); `rederive_companion_links_for_folder` restores it. RED before Fix 2: rederive re-ran only
/// the wikilink/semantic passes, so the companion edge stayed purged after one lock cycle.
#[test]
fn rederive_restores_companion_edge_same_folder() {
    let db = mem_db();
    seed_folder(&db, "f-locked", "Secret");
    db.insert_meeting(&sample_meeting("m1", "2026-06-24T10:00:00Z"))
        .unwrap();
    note_for(&db, "m1", "claude_code", "meeting note body");
    db.set_note_folder("m1", Some("f-locked")).unwrap();
    // A companion note (a `documents(kind='note')`) filed in the SAME folder, linked to m1.
    seed_note_doc(&db, "companion", "f-locked", "Companion", "typed notes");
    db.set_document_meeting_id("companion", "m1").unwrap();
    db.set_companion_link("companion", "m1").unwrap();
    assert_eq!(
        link_count(&db, "meeting", "m1", "companion"),
        1,
        "companion edge exists pre-seal"
    );

    // Seal the folder: the purge drops the companion edge (it's not dismissed/accepted).
    db.seal_note("m1", "claude_code", b"ciphertext").unwrap();
    db.seal_document("companion", b"ciphertext").unwrap();
    let mut folders = HashSet::new();
    folders.insert("f-locked".to_string());
    db.blank_sealed_notes_in_folders(&folders).unwrap();
    assert_eq!(
        link_count(&db, "meeting", "m1", "companion"),
        0,
        "companion edge purged on seal"
    );

    // UNLOCK: restore the notes' plaintext (so the endpoints are NOT sealed-at-rest), then rederive.
    db.restore_note_markdown("m1", "claude_code", "meeting note body")
        .unwrap();
    db.set_document_text("companion", "typed notes").unwrap();
    let restored = db
        .rederive_companion_links_for_folder("f-locked", &["m1".to_string()])
        .unwrap();
    assert_eq!(restored, 1, "one companion edge re-asserted");
    assert_eq!(
        link_count(&db, "meeting", "m1", "companion"),
        1,
        "the companion edge is restored on unlock (present in links)"
    );
}

/// Fix 2 (brain-v3 audit) — COMPANION edge re-derived when the companion note lives in a DIFFERENT
/// (still-open) folder than its meeting: unlocking the MEETING's folder must re-assert the inbound
/// leg (note-in-other-folder → meeting-in-this-folder). RED before Fix 2: the inbound leg was never
/// scanned, so the edge stayed gone.
#[test]
fn rederive_restores_companion_edge_cross_folder_inbound() {
    let db = mem_db();
    seed_folder(&db, "f-mtg", "Meetings");
    seed_folder(&db, "f-notes", "Notes");
    db.insert_meeting(&sample_meeting("m1", "2026-06-24T10:00:00Z"))
        .unwrap();
    note_for(&db, "m1", "claude_code", "meeting note body");
    db.set_note_folder("m1", Some("f-mtg")).unwrap();
    // Companion note in the OTHER (open) folder, linked to m1.
    seed_note_doc(&db, "companion", "f-notes", "Companion", "typed notes");
    db.set_document_meeting_id("companion", "m1").unwrap();
    db.set_companion_link("companion", "m1").unwrap();

    // Seal the MEETING's folder only → its endpoint purge drops the companion edge.
    db.seal_note("m1", "claude_code", b"ciphertext").unwrap();
    let mut folders = HashSet::new();
    folders.insert("f-mtg".to_string());
    db.blank_sealed_notes_in_folders(&folders).unwrap();
    assert_eq!(
        link_count(&db, "meeting", "m1", "companion"),
        0,
        "companion edge purged"
    );

    // Unlock the meeting's folder: restore m1's plaintext, then rederive (companion note in f-notes
    // was never sealed, so its text is intact).
    db.restore_note_markdown("m1", "claude_code", "meeting note body")
        .unwrap();
    let restored = db
        .rederive_companion_links_for_folder("f-mtg", &["m1".to_string()])
        .unwrap();
    assert_eq!(
        restored, 1,
        "the inbound companion edge (note in another folder) is re-asserted"
    );
    assert_eq!(
        link_count(&db, "meeting", "m1", "companion"),
        1,
        "cross-folder companion edge restored"
    );
}

/// Fix 2 (brain-v3 audit) — the companion re-derive OBEYS Fix 0: it does NOT re-assert an edge to a
/// companion note that is ITSELF still sealed-at-rest (in another sealed folder). Guards against
/// naming a sealed neighbour during the unlock of a DIFFERENT folder.
#[test]
fn rederive_companion_skips_sealed_note_endpoint() {
    let db = mem_db();
    seed_folder(&db, "f-mtg", "Meetings");
    seed_folder(&db, "f-secret", "Secret");
    db.insert_meeting(&sample_meeting("m1", "2026-06-24T10:00:00Z"))
        .unwrap();
    note_for(&db, "m1", "claude_code", "meeting note body");
    db.set_note_folder("m1", Some("f-mtg")).unwrap();
    // Companion note in a STILL-SEALED folder, linked to m1.
    seed_note_doc(&db, "companion", "f-secret", "Companion", "typed notes");
    db.set_document_meeting_id("companion", "m1").unwrap();
    // Seal the companion note at rest (its folder stays locked).
    db.seal_document("companion", b"ciphertext").unwrap();
    db.set_folder_locked("f-secret", true, None).unwrap();
    // Unlock m1's folder: m1 plaintext restored; the companion note is still sealed.
    let restored = db
        .rederive_companion_links_for_folder("f-mtg", &["m1".to_string()])
        .unwrap();
    assert_eq!(
        restored, 0,
        "no companion edge to a still-sealed companion note (Fix-0 discipline)"
    );
    assert_eq!(
        link_count(&db, "meeting", "m1", "companion"),
        0,
        "no edge naming the sealed note"
    );
}

/// Fix 3 (brain-v3 audit) — INBOUND wikilink RESTORED on unlock. Note A (open folder) links
/// `[[Project X]]` whose target note X lives in folder F. Seal F → the A→X edge is purged (X is a
/// sealed endpoint). `rederive_inbound_wikilinks_for_folder` re-indexes A (the outside source) so
/// the edge is restored. RED before Fix 3: rederive re-ran only F's OWN sources, so A→X stayed gone
/// (A may never be edited again).
#[test]
fn rederive_restores_inbound_wikilink_from_outside_source() {
    let db = mem_db();
    seed_folder(&db, "f-open", "Open");
    seed_folder(&db, "f-secret", "Secret");
    // Target note X in the (to-be-sealed) folder.
    seed_note_doc(&db, "x", "f-secret", "Project X", "the project body");
    // Source note A in the OPEN folder, body links [[Project X]].
    seed_note_doc(
        &db,
        "a",
        "f-open",
        "Note A",
        "see [[Project X]] for context",
    );

    // Index A while X is visible → the A→X wikilink edge exists.
    let mut open_all = HashSet::new();
    open_all.insert("f-open".to_string());
    open_all.insert("f-secret".to_string());
    db.index_wikilinks_for_source(
        crate::links::LinkKind::Note,
        "a",
        "see [[Project X]] for context",
        &open_all,
    )
    .unwrap();
    assert_eq!(
        link_count(&db, "note", "x", "wikilink"),
        1,
        "A→X edge exists pre-seal"
    );

    // Seal F: blank X + run the relock purge (drops the A→X edge, X being a sealed endpoint).
    db.seal_document("x", b"ciphertext").unwrap();
    db.set_folder_locked("f-secret", true, None).unwrap();
    let mut folders = HashSet::new();
    folders.insert("f-secret".to_string());
    db.blank_sealed_notes_in_folders(&folders).unwrap();
    assert_eq!(
        link_count(&db, "note", "x", "wikilink"),
        0,
        "A→X edge purged on seal"
    );

    // UNLOCK F: restore X's plaintext + add F to the unlock set, then re-derive the INBOUND leg.
    db.set_document_text("x", "the project body").unwrap();
    let mut unlocked = HashSet::new();
    unlocked.insert("f-open".to_string());
    unlocked.insert("f-secret".to_string());
    let reindexed = db
        .rederive_inbound_wikilinks_for_folder("f-secret", &[], &unlocked)
        .unwrap();
    assert!(reindexed >= 1, "the outside source A was re-indexed");
    assert_eq!(
        link_count(&db, "note", "x", "wikilink"),
        1,
        "the inbound A→X wikilink is restored on unlock"
    );
    // And the gated reader on X surfaces A once F is unlocked.
    let edges = db
        .links_for_visible(crate::links::LinkKind::Note, "x", &unlocked)
        .unwrap();
    assert!(
        edges.iter().any(|e| e.other_id == "a"),
        "X's link list surfaces the restored inbound source A"
    );
}

/// Fix 4 (brain-v3 audit) — a later-sealed neighbour's materialized `[[Title]]` is STRIPPED from a
/// VISIBLE source note's plaintext on seal, and only from the MACHINE block (a user-typed wikilink
/// outside it survives). RED before Fix 4: N's body still named the sealed neighbour after the seal.
#[test]
fn seal_strips_sealed_neighbour_marker_from_visible_note() {
    let db = mem_db();
    seed_folder(&db, "f-open", "Open");
    seed_folder(&db, "f-secret", "Secret");
    // Neighbour M in the (to-be-sealed) folder.
    seed_note_doc(&db, "m", "f-secret", "Neighbour M", "the neighbour body");
    // Source note N (open): a USER-typed [[Neighbour M]] outside the block + the MACHINE block hit.
    let machine_block = crate::enrich::apply_link_markers(
        "user body mentions [[Neighbour M]] inline",
        &[crate::enrich::ContextHit {
            source: "note".into(),
            detail: "[[Neighbour M]]".into(),
            url: None,
        }],
    );
    seed_note_doc(&db, "n", "f-open", "Note N", &machine_block);
    db.set_note_doc_exported_path("n", Some("/vault/n.md"))
        .unwrap();
    // An ACCEPTED edge N↔M so the marker survives (Fix 1) + re-materializes on unlock.
    let now = 1_700_000_000_000i64;
    {
        let mut conn = db.lock();
        let tx = conn.transaction().unwrap();
        Db::upsert_link_tx(
            &tx, "note", "n", "note", "m", "semantic", 0.9, "accepted", "active", now,
        )
        .unwrap();
        tx.commit().unwrap();
    }

    // Seal M at rest, then strip the sealed neighbour's marker from visible sources.
    db.seal_document("m", b"ciphertext").unwrap();
    let changed = db
        .strip_sealed_neighbour_markers(&[], &["m".to_string()])
        .unwrap();
    assert!(
        changed.iter().any(|(is_m, id)| !is_m && id == "n"),
        "N was reported as a changed source"
    );
    let n_text: String = db
        .lock()
        .query_row("SELECT text FROM documents WHERE id = 'n'", [], |r| {
            r.get(0)
        })
        .unwrap();
    // The MACHINE block hit is gone…
    assert!(
        !n_text.contains("> - [[Neighbour M]]"),
        "the managed-block marker naming the sealed neighbour is stripped"
    );
    // …but the USER-typed inline wikilink survives (never touch the user's own content).
    assert!(
        n_text.contains("user body mentions [[Neighbour M]] inline"),
        "a user-typed wikilink outside the managed block is NOT stripped"
    );
    let pending = db.pending_lock_marker_export_cleanup().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].source_kind, "note");
    assert_eq!(pending[0].source_id, "n");
    assert_eq!(pending[0].exported_path, "/vault/n.md");
    assert_eq!(pending[0].sealed_title, "Neighbour M");
}

/// Crash replay net: even if the SQLCipher body was already stripped by a prior attempt, a
/// surviving edge still re-enqueues the exact exported path/title. This closes the crash window
/// where the first transaction committed but the process died before the vault write.
#[test]
fn seal_journals_export_cleanup_when_db_body_is_already_scrubbed() {
    let db = mem_db();
    seed_folder(&db, "f-open", "Open");
    seed_folder(&db, "f-secret", "Secret");
    seed_note_doc(&db, "m", "f-secret", "Neighbour M", "secret body");
    seed_note_doc(
        &db,
        "n",
        "f-open",
        "Note N",
        "body already has no managed marker",
    );
    db.set_note_doc_exported_path("n", Some("/vault/replay.md"))
        .unwrap();
    {
        let mut conn = db.lock();
        let tx = conn.transaction().unwrap();
        Db::upsert_link_tx(
            &tx,
            "note",
            "n",
            "note",
            "m",
            "semantic",
            0.9,
            "accepted",
            "active",
            1_700_000_000_000,
        )
        .unwrap();
        tx.commit().unwrap();
    }
    db.seal_document("m", b"ciphertext").unwrap();

    let changed = db
        .strip_sealed_neighbour_markers(&[], &["m".to_string()])
        .unwrap();

    assert!(changed.is_empty(), "the DB body was already scrubbed");
    let pending = db.pending_lock_marker_export_cleanup().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].exported_path, "/vault/replay.md");
    assert_eq!(pending[0].sealed_title, "Neighbour M");
}

/// Every provider row is a distinct canonical note/export, including equal-timestamp rows.
/// Stripping one arbitrary newest row (then UPDATEing every MAX timestamp row) both leaked the
/// older export and collapsed provider-specific markdown.
#[test]
fn seal_strips_and_journals_every_meeting_provider_without_collapsing_content() {
    let db = mem_db();
    seed_folder(&db, "f-secret", "Secret");
    seed_note_doc(&db, "target", "f-secret", "Secret Target", "secret body");
    db.insert_meeting(&sample_meeting("source", "2026-06-26T09:00:00Z"))
        .unwrap();
    let claude_body = crate::enrich::apply_link_markers(
        "claude-specific body",
        &[crate::enrich::ContextHit {
            source: "note".into(),
            detail: "[[Secret Target]]".into(),
            url: None,
        }],
    );
    let ollama_body = crate::enrich::apply_link_markers(
        "ollama-specific body",
        &[crate::enrich::ContextHit {
            source: "note".into(),
            detail: "[[Secret Target]]".into(),
            url: None,
        }],
    );
    for (provider, body, path) in [
        ("claude_code", claude_body, "/vault/source-claude.md"),
        ("ollama", ollama_body, "/vault/source-ollama.md"),
    ] {
        db.upsert_note(&NoteRecord {
            meeting_id: "source".into(),
            provider_id: provider.into(),
            markdown: body,
            // Deliberately identical: provider_id, not MAX(timestamp), is row identity.
            created_at: "2026-06-26T09:05:00Z".into(),
            exported_path: Some(path.into()),
            model_requested: None,
            model_served: None,
            gateway_host: None,
        })
        .unwrap();
    }
    {
        let mut conn = db.lock();
        let tx = conn.transaction().unwrap();
        Db::upsert_link_tx(
            &tx,
            "meeting",
            "source",
            "note",
            "target",
            "semantic",
            0.9,
            "accepted",
            "active",
            1_700_000_000_000,
        )
        .unwrap();
        tx.commit().unwrap();
    }
    db.seal_document("target", b"ciphertext").unwrap();

    db.strip_sealed_neighbour_markers(&[], &["target".to_string()])
        .unwrap();

    let rows: Vec<(String, String)> = {
        let conn = db.lock();
        let mut stmt = conn
            .prepare(
                "SELECT provider_id, markdown FROM notes
                      WHERE meeting_id = 'source' ORDER BY provider_id",
            )
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|row| row.unwrap())
            .collect()
    };
    assert_eq!(rows.len(), 2);
    assert!(rows[0].1.contains("claude-specific body"));
    assert!(rows[1].1.contains("ollama-specific body"));
    assert!(rows
        .iter()
        .all(|(_, body)| !body.contains("> - [[Secret Target]]")));
    let pending = db.pending_lock_marker_export_cleanup().unwrap();
    assert_eq!(pending.len(), 2);
    assert!(pending.iter().any(|row| {
        row.provider_id == "claude_code" && row.exported_path == "/vault/source-claude.md"
    }));
    assert!(pending.iter().any(|row| {
        row.provider_id == "ollama" && row.exported_path == "/vault/source-ollama.md"
    }));
}

/// Fix 4 regression (adversarial-verifier): the ALWAYS-ON auto related-notes pass renders the
/// neighbour `[[Title]]` in the hit's `url` field (`detail` = a task-free gist), NOT `detail`. The
/// original detail-only strip KEPT it → the sealed title leaked in the visible note's plaintext +
/// `.md`. The strip must match `[[…]]` in EITHER field.
#[test]
fn seal_strips_auto_related_url_shape_marker() {
    let db = mem_db();
    seed_folder(&db, "f-open", "Open");
    seed_folder(&db, "f-secret", "Secret");
    seed_note_doc(&db, "m", "f-secret", "Neighbour M", "the neighbour body");
    // Auto related-notes shape: `[[Title]]` lives in `url`, `detail` is a gist.
    let machine_block = crate::enrich::apply_link_markers(
        "user body",
        &[crate::enrich::ContextHit {
            source: "Murmur".into(),
            detail: "discussed the roadmap".into(),
            url: Some("[[Neighbour M]]".into()),
        }],
    );
    seed_note_doc(&db, "n", "f-open", "Note N", &machine_block);
    let now = 1_700_000_000_000i64;
    {
        let mut conn = db.lock();
        let tx = conn.transaction().unwrap();
        Db::upsert_link_tx(
            &tx, "note", "n", "note", "m", "semantic", 0.9, "accepted", "active", now,
        )
        .unwrap();
        tx.commit().unwrap();
    }
    db.seal_document("m", b"ciphertext").unwrap();
    let changed = db
        .strip_sealed_neighbour_markers(&[], &["m".to_string()])
        .unwrap();
    assert!(
        changed.iter().any(|(is_m, id)| !is_m && id == "n"),
        "N reported changed"
    );
    let n_text: String = db
        .lock()
        .query_row("SELECT text FROM documents WHERE id = 'n'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(
            !n_text.contains("[[Neighbour M]]"),
            "the auto-related url-shape marker naming the sealed neighbour must be stripped; got {n_text:?}"
        );
}

/// Fix 4 regression (lock-security): semantic edges are CANONICALIZED (smaller `(kind,id)` = `src`),
/// so a marker can be written into the DST endpoint naming a sealed SRC. The original dst-only
/// collect scan missed it. Here a `document`-src (`"document" < "note"`) is sealed, its title
/// materialized into note N (dst) — the collect's src-leg must find N and strip it.
#[test]
fn seal_strips_marker_naming_a_sealed_src_endpoint() {
    let db = mem_db();
    seed_folder(&db, "f-open", "Open");
    seed_folder(&db, "f-secret", "Secret");
    seed_note_doc(&db, "d", "f-secret", "Doc D", "doc body");
    let machine_block = crate::enrich::apply_link_markers(
        "user body",
        &[crate::enrich::ContextHit {
            source: "note".into(),
            detail: "[[Doc D]]".into(),
            url: None,
        }],
    );
    seed_note_doc(&db, "n", "f-open", "Note N", &machine_block);
    let now = 1_700_000_000_000i64;
    {
        let mut conn = db.lock();
        let tx = conn.transaction().unwrap();
        // Store the edge with the sealed DOCUMENT d as the SRC and the marker-owning note n as the
        // DST (the canonicalized shape auto_link_semantic produces, since "document" < "note").
        // upsert_link_tx stores endpoints as-passed, so this exercises the NEW src-leg of the collect
        // scan: sealing d, the dst-leg (WHERE dst_id='d') misses (d is the src), and ONLY the src-leg
        // (WHERE src_id='d' … dst_kind IN('note','meeting')) finds n — RED on the old dst-only code.
        Db::upsert_link_tx(
            &tx, "document", "d", "note", "n", "semantic", 0.9, "accepted", "active", now,
        )
        .unwrap();
        tx.commit().unwrap();
    }
    db.seal_document("d", b"ciphertext").unwrap();
    let changed = db
        .strip_sealed_neighbour_markers(&[], &["d".to_string()])
        .unwrap();
    assert!(
            changed.iter().any(|(is_m, id)| !is_m && id == "n"),
            "N (dst, owning the marker) must be found via the src-leg and reported changed; got {changed:?}"
        );
    let n_text: String = db
        .lock()
        .query_row("SELECT text FROM documents WHERE id = 'n'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(
        !n_text.contains("[[Doc D]]"),
        "a marker naming the sealed SRC endpoint must be stripped; got {n_text:?}"
    );
}

/// FIX 2 (block-drop) — the machine `> [!related]- Related notes` (`murmur:links`) block is
/// RETIRED, so on unlock the accepted marker is NO LONGER RE-MATERIALIZED into the source note's
/// body/vault `.md`: `rematerialize_accepted_markers_for_folder` is now a documented NO-OP. The
/// accepted link ROW is preserved (surfaces in the live Related panel); only the body-block echo
/// is gone. RED before FIX 2 (the OLD assertions, now inverted): the old code reported N changed
/// and rewrote `[[Neighbour M]]` into N's body — the exact behavior this change removes.
#[test]
fn unlock_does_not_rematerialize_links_block() {
    let db = mem_db();
    seed_folder(&db, "f-open", "Open");
    seed_folder(&db, "f-secret", "Secret");
    seed_note_doc(&db, "m", "f-secret", "Neighbour M", "the neighbour body");
    seed_note_doc(&db, "n", "f-open", "Note N", "just the note body");
    let now = 1_700_000_000_000i64;
    {
        let mut conn = db.lock();
        let tx = conn.transaction().unwrap();
        Db::upsert_link_tx(
            &tx, "note", "n", "note", "m", "semantic", 0.9, "accepted", "active", now,
        )
        .unwrap();
        tx.commit().unwrap();
    }
    // Seal M, lock its folder, strip (N had no marker yet — the strip is a no-op).
    db.seal_document("m", b"ciphertext").unwrap();
    db.set_folder_locked("f-secret", true, None).unwrap();
    let _ = db
        .strip_sealed_neighbour_markers(&[], &["m".to_string()])
        .unwrap();

    // UNLOCK M's folder: restore M's plaintext, then invoke the (now no-op) rematerialize leg.
    db.set_document_text("m", "the neighbour body").unwrap();
    let mut unlocked = HashSet::new();
    unlocked.insert("f-open".to_string());
    unlocked.insert("f-secret".to_string());
    let changed = db
        .rematerialize_accepted_markers_for_folder("f-secret", &[], &unlocked)
        .unwrap();
    assert!(
        changed.is_empty(),
        "no source is reported changed — the block re-materialization is retired; got {changed:?}"
    );
    let n_text: String = db
        .lock()
        .query_row("SELECT text FROM documents WHERE id = 'n'", [], |r| {
            r.get(0)
        })
        .unwrap();
    // The load-bearing FIX-2 guarantee: N's body has NO reborn machine links block afterward.
    assert!(
        !n_text.contains("murmur:links")
            && !n_text.contains("[!related]")
            && !n_text.contains("[[Neighbour M]]"),
        "unlock must NOT rematerialize a murmur:links block into the source body; got {n_text:?}"
    );
    assert_eq!(
        n_text, "just the note body",
        "the source body is byte-identical to before — the retired block is never reborn"
    );
    // The accepted link ROW is untouched (still surfaces in the Related panel).
    let edges = db
        .links_for_visible(crate::links::LinkKind::Note, "n", &unlocked)
        .unwrap();
    assert!(
        edges.iter().any(|e| e.other_id == "m"),
        "the accepted N↔M edge is preserved — only the body-block echo is retired"
    );
}

/// Fix 4 (brain-v3 audit) — the strip NEVER touches a source that is itself sealed-at-rest (its
/// body is already blanked — resurrecting plaintext behind its own lock would be a bug).
#[test]
fn seal_strip_skips_sealed_at_rest_source() {
    let db = mem_db();
    seed_folder(&db, "f-a", "A");
    seed_folder(&db, "f-b", "B");
    seed_note_doc(&db, "m", "f-a", "Neighbour M", "m body");
    // Source N is itself sealed at rest (blank text + blob).
    seed_note_doc(&db, "n", "f-b", "Note N", "n body");
    db.seal_document("n", b"ciphertext").unwrap();
    let now = 1_700_000_000_000i64;
    {
        let mut conn = db.lock();
        let tx = conn.transaction().unwrap();
        Db::upsert_link_tx(
            &tx, "note", "n", "note", "m", "wikilink", 1.0, "user", "active", now,
        )
        .unwrap();
        tx.commit().unwrap();
    }
    db.seal_document("m", b"ciphertext").unwrap();
    let changed = db
        .strip_sealed_neighbour_markers(&[], &["m".to_string()])
        .unwrap();
    assert!(
        !changed.iter().any(|(_, id)| id == "n"),
        "a sealed-at-rest source is never re-touched (its body is already blank)"
    );
}

/// DISMISS TOMBSTONES: a dismissed semantic suggestion is never re-suggested by a later auto pass.
#[test]
fn dismiss_tombstones_a_semantic_suggestion() {
    let db = mem_db();
    let body = "identical clustering text for the semantic neighbour test";
    for id in ["a", "b"] {
        db.insert_meeting(&sample_meeting(id, "2026-06-24T10:00:00Z"))
            .unwrap();
        note_for(&db, id, "claude_code", body);
        db.index_meeting_chunks(id, &[], &crate::embed::StubEmbedder)
            .unwrap();
    }
    let unlocked = HashSet::new();
    db.auto_link_semantic(crate::links::LinkKind::Meeting, "a", &unlocked)
        .unwrap();
    let (link_id, _): (i64, String) = db
        .lock()
        .query_row(
            "SELECT id, status FROM links WHERE edge_type='semantic' LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get::<_, String>(1)?)),
        )
        .unwrap();
    db.dismiss_link(link_id).unwrap();
    // Re-run the auto pass on 'a' → the dismissed edge stays dismissed (never resurrected).
    db.auto_link_semantic(crate::links::LinkKind::Meeting, "a", &unlocked)
        .unwrap();
    let status: String = db
        .lock()
        .query_row(
            "SELECT status FROM links WHERE id = ?1",
            rusqlite::params![link_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        status, "dismissed",
        "a dismissed suggestion is a permanent tombstone"
    );
    // The reader never returns a dismissed edge.
    let edges = db
        .links_for_visible(crate::links::LinkKind::Meeting, "a", &unlocked)
        .unwrap();
    assert!(
        edges.iter().all(|e| e.id != link_id),
        "dismissed edge is never surfaced"
    );
}

/// NIT (RED-before-GREEN — no downgrade of a user's decision): once a user ACCEPTS a semantic
/// edge (`status='active'`, `created_by='accepted'`), a LATER auto semantic pass that re-suggests
/// the SAME edge must NOT downgrade it back to `suggested`. The `upsert_link_tx` DO-UPDATE guard
/// preserves an existing `active` row's status/created_by against an incoming `suggested` write
/// (refreshing only the score). RED on the pre-fix upsert (`status = excluded.status` clobbered
/// the accepted edge back to `suggested` on every re-run).
#[test]
fn accepted_semantic_edge_survives_auto_resuggest() {
    let db = mem_db();
    let body = "identical clustering text for the semantic neighbour test";
    for id in ["a", "b"] {
        db.insert_meeting(&sample_meeting(id, "2026-06-24T10:00:00Z"))
            .unwrap();
        note_for(&db, id, "claude_code", body);
        db.index_meeting_chunks(id, &[], &crate::embed::StubEmbedder)
            .unwrap();
    }
    let unlocked = HashSet::new();
    db.auto_link_semantic(crate::links::LinkKind::Meeting, "a", &unlocked)
        .unwrap();
    let link_id: i64 = db
        .lock()
        .query_row(
            "SELECT id FROM links WHERE edge_type='semantic' LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    // User ACCEPTS it: active + accepted.
    db.accept_link(link_id).unwrap();

    // Re-run the auto pass on 'a' → the accepted edge is re-suggested by the linker but MUST NOT
    // be downgraded.
    db.auto_link_semantic(crate::links::LinkKind::Meeting, "a", &unlocked)
        .unwrap();
    let (status, cb): (String, String) = db
        .lock()
        .query_row(
            "SELECT status, created_by FROM links WHERE id = ?1",
            rusqlite::params![link_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        status, "active",
        "an accepted semantic edge stays active across a re-suggest"
    );
    assert_eq!(
        cb, "accepted",
        "created_by is preserved as 'accepted', never reset to 'auto'"
    );
}

/// ACCEPT flips status + created_by (the .md materialize is command-layer; this pins the DB flip).
#[test]
fn accept_flips_status_and_created_by() {
    let db = mem_db();
    let now = 1_700_000_000_000i64;
    {
        let mut conn = db.lock();
        let tx = conn.transaction().unwrap();
        Db::upsert_link_tx(
            &tx,
            "meeting",
            "m1",
            "meeting",
            "m2",
            "semantic",
            0.9,
            "auto",
            "suggested",
            now,
        )
        .unwrap();
        tx.commit().unwrap();
    }
    let link_id: i64 = db
        .lock()
        .query_row(
            "SELECT id FROM links WHERE edge_type='semantic' LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    db.accept_link(link_id).unwrap();
    let (status, cb): (String, String) = db
        .lock()
        .query_row(
            "SELECT status, created_by FROM links WHERE id = ?1",
            rusqlite::params![link_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(status, "active");
    assert_eq!(cb, "accepted");
}

/// PURGE-ON-LOCK: index a meeting's chunks while visible, then re-blank its folder (the relock
/// path) → no `note_chunks`/`vec_chunks` row for that meeting survives at rest.
#[test]
fn vec_chunks_purged_on_lock() {
    let db = mem_db();
    seed_folder(&db, "f-locked", "Secret");
    db.insert_meeting(&sample_meeting("m1", "2026-06-24T10:00:00Z"))
        .unwrap();
    note_for(
        &db,
        "m1",
        "claude_code",
        "First budget paragraph.\n\nSecond hiring paragraph.",
    );
    db.set_note_folder("m1", Some("f-locked")).unwrap();

    // Index while visible (open folder) with the deterministic stub embedder.
    db.index_meeting_chunks("m1", &[], &crate::embed::StubEmbedder)
        .unwrap();
    assert!(chunk_count(&db, "m1") > 0, "expected chunks after indexing");
    assert!(vec_count(&db, "m1") > 0, "expected vectors after indexing");

    // Seal: blank the note (content_blob present so blank_sealed_notes_in_folders acts), then
    // run the relock blanker for the folder.
    db.seal_note("m1", "claude_code", b"ciphertext").unwrap();
    let mut folders = std::collections::HashSet::new();
    folders.insert("f-locked".to_string());
    db.blank_sealed_notes_in_folders(&folders).unwrap();

    assert_eq!(
        chunk_count(&db, "m1"),
        0,
        "note_chunks must be purged on lock (no plaintext chunk at rest)"
    );
    assert_eq!(
        vec_count(&db, "m1"),
        0,
        "vec_chunks must be purged on lock (no invertible vector at rest)"
    );
}

// ── transcript chunks (source_type='transcript') ──────────────────────────

/// Count `note_chunks` of a given `source_type` for a meeting.
fn chunk_count_of(db: &Db, meeting_id: &str, source_type: &str) -> i64 {
    db.lock()
        .query_row(
            "SELECT COUNT(*) FROM note_chunks WHERE meeting_id = ?1 AND source_type = ?2",
            rusqlite::params![meeting_id, source_type],
            |r| r.get(0),
        )
        .unwrap()
}

fn tseg(idx: i64, start: f64, end: f64, speaker: &str, text: &str) -> Segment {
    Segment {
        idx,
        start_s: start,
        end_s: end,
        text: text.to_string(),
        speaker: Some(speaker.to_string()),
        confidence: None,
    }
}

/// `index_meeting_chunks` writes BOTH classes: note-summary (`voice`) AND transcript
/// (`transcript`), 1:1-paired with vec rows, in one clean-replace tx.
#[test]
fn index_meeting_chunks_writes_transcript_class() {
    let db = mem_db();
    db.insert_meeting(&sample_meeting("m1", "2026-06-24T10:00:00Z"))
        .unwrap();
    note_for(
        &db,
        "m1",
        "claude_code",
        "Summary paragraph about the budget.",
    );
    let segs = [
        tseg(0, 0.0, 3.0, "me", "so what did we decide on the migration"),
        tseg(
            1,
            3.0,
            8.0,
            "others",
            "we agreed to defer it to next quarter",
        ),
    ];
    db.index_meeting_chunks("m1", &segs, &crate::embed::StubEmbedder)
        .unwrap();

    assert!(
        chunk_count_of(&db, "m1", "voice") > 0,
        "note-summary chunks must be written"
    );
    assert!(
        chunk_count_of(&db, "m1", "transcript") > 0,
        "transcript chunks must be written from segments"
    );
    // vec rows are 1:1 with note_chunks rows (both classes).
    assert_eq!(
        chunk_count(&db, "m1"),
        vec_count(&db, "m1"),
        "every chunk (both classes) has a vector"
    );
}

/// RED-before-GREEN: a background embed result produced under an invalidated epoch must not
/// enter the clean-replace transaction. The old check-then-write shape wrote these chunks.
#[test]
fn background_index_rejects_stale_epoch_before_transaction() {
    let db = mem_db();
    db.insert_meeting(&sample_meeting("m-epoch", "2026-06-24T10:00:00Z"))
        .unwrap();
    note_for(&db, "m-epoch", "local", "stale embedding candidate");
    let epoch = crate::perf::background_epoch();
    crate::perf::invalidate_background_epoch_for_test();

    let committed = db
        .index_meeting_chunks_background("m-epoch", &[], &crate::embed::StubEmbedder, epoch)
        .unwrap();

    assert!(!committed);
    assert_eq!(chunk_count(&db, "m-epoch"), 0);
}

/// RE-INDEX is a CLEAN REPLACE of BOTH classes: re-running with different segments/note leaves no
/// stale rows of either class.
#[test]
fn index_meeting_chunks_reindex_replaces_both_classes() {
    let db = mem_db();
    db.insert_meeting(&sample_meeting("m1", "2026-06-24T10:00:00Z"))
        .unwrap();
    note_for(&db, "m1", "claude_code", "First note.");
    let segs1 = [tseg(0, 0.0, 3.0, "me", "first pass transcript content")];
    db.index_meeting_chunks("m1", &segs1, &crate::embed::StubEmbedder)
        .unwrap();
    let v1 = chunk_count_of(&db, "m1", "transcript");
    assert!(v1 > 0);

    // Re-index with EMPTY segments → transcript class must go to zero (clean replace, no orphans).
    db.index_meeting_chunks("m1", &[], &crate::embed::StubEmbedder)
        .unwrap();
    assert_eq!(
        chunk_count_of(&db, "m1", "transcript"),
        0,
        "re-index with no segments must leave zero stale transcript chunks"
    );
    assert!(
        chunk_count_of(&db, "m1", "voice") > 0,
        "note class still present after re-index"
    );
    assert_eq!(
        chunk_count(&db, "m1"),
        vec_count(&db, "m1"),
        "1:1 vec pairing preserved after re-index"
    );
}

/// EMPTY segments → no transcript chunks (the "sealed meeting is never chunked" property in the
/// db layer: the gated callers pass the RESTORED plaintext; a sealed meeting whose segments are
/// blanked yields the empty slice ⇒ zero transcript chunks).
#[test]
fn index_meeting_chunks_no_segments_writes_no_transcript_class() {
    let db = mem_db();
    db.insert_meeting(&sample_meeting("m1", "2026-06-24T10:00:00Z"))
        .unwrap();
    note_for(&db, "m1", "claude_code", "Only a note, no transcript.");
    db.index_meeting_chunks("m1", &[], &crate::embed::StubEmbedder)
        .unwrap();
    assert_eq!(
        chunk_count_of(&db, "m1", "transcript"),
        0,
        "blank/sealed transcript ⇒ no transcript chunks"
    );
    assert!(
        chunk_count_of(&db, "m1", "voice") > 0,
        "note class still indexes independently"
    );
}

/// GATE (semantic): a sealed-and-not-session-unlocked meeting's TRANSCRIPT chunks surface ZERO
/// through `search_semantic_visible`. Mirrors `vec_semantic_search_is_gated_by_visibility` but
/// with a real transcript-source chunk that is deliberately left in place (folder flipped to
/// locked WITHOUT purge) — so exclusion can ONLY come from `visibility_clause`. RED if the gate
/// were removed OR if transcript chunks bypassed the shared reader.
#[test]
fn transcript_chunks_are_gated_by_visibility_semantic() {
    let db = mem_db();
    seed_folder(&db, "f-locked", "Secret");
    db.insert_meeting(&sample_meeting("sealed", "2026-06-24T10:00:00Z"))
        .unwrap();
    note_for(&db, "sealed", "claude_code", "note body");
    db.set_note_folder("sealed", Some("f-locked")).unwrap();
    // Insert a REAL transcript-source chunk (source_type='transcript') + its vector, then lock the
    // folder WITHOUT purging — the row survives so any exclusion is the gate.
    {
        let conn = db.lock();
        conn.execute(
                "INSERT INTO note_chunks (meeting_id, provider_id, chunk_idx, source_type, text)
                 VALUES ('sealed', 'transcript', 0, 'transcript', ?1)",
                rusqlite::params!["Secret · 2026-06-24\n[00:00-00:05] (others)\nthe secret merger price is confidential"],
            )
            .unwrap();
        let chunk_id = conn.last_insert_rowid();
        let blob = crate::embed::vec_to_blob(&one_hot(0));
        conn.execute(
            "INSERT INTO vec_chunks(chunk_id, embedding) VALUES (?1, ?2)",
            rusqlite::params![chunk_id, blob],
        )
        .unwrap();
    }
    db.set_folder_locked("f-locked", true, None).unwrap();

    let query = one_hot(0);
    let nothing = std::collections::HashSet::new();
    let hidden = db
        .search_semantic_visible(&query, 10, 0.0, &nothing)
        .unwrap();
    assert!(
        !hidden.iter().any(|h| h.meeting.id == "sealed"),
        "sealed meeting's TRANSCRIPT chunk leaked through the semantic gate"
    );
    // Session-unlock → it reappears (proves the row + gate, not purge).
    let mut unlocked = std::collections::HashSet::new();
    unlocked.insert("f-locked".to_string());
    let shown = db
        .search_semantic_visible(&query, 10, 0.0, &unlocked)
        .unwrap();
    assert!(
        shown.iter().any(|h| h.meeting.id == "sealed"),
        "session-unlocked meeting's transcript chunk must reappear in semantic results"
    );
}

/// GATE (hybrid): the same sealed meeting's transcript chunk is ALSO absent through the fused
/// FTS+semantic+graph reader — with BOTH an FTS term AND a matching query vector — so exclusion
/// is the shared `visibility_clause`, not purge. Companion to `vec_hybrid_search_is_gated_by_visibility`.
#[test]
fn transcript_chunks_are_gated_by_visibility_hybrid() {
    let db = mem_db();
    seed_folder(&db, "f-locked", "Secret");
    db.insert_meeting(&sample_meeting("sealed", "2026-06-24T10:00:00Z"))
        .unwrap();
    note_for(&db, "sealed", "claude_code", "quarterly merger note body");
    db.set_note_folder("sealed", Some("f-locked")).unwrap();
    {
        let conn = db.lock();
        conn.execute(
            "INSERT INTO note_chunks (meeting_id, provider_id, chunk_idx, source_type, text)
                 VALUES ('sealed', 'transcript', 0, 'transcript', ?1)",
            rusqlite::params![
                "Secret · 2026-06-24\n[00:00-00:05] (others)\nthe merger budget is confidential"
            ],
        )
        .unwrap();
        let chunk_id = conn.last_insert_rowid();
        let blob = crate::embed::vec_to_blob(&one_hot(0));
        conn.execute(
            "INSERT INTO vec_chunks(chunk_id, embedding) VALUES (?1, ?2)",
            rusqlite::params![chunk_id, blob],
        )
        .unwrap();
    }
    db.set_folder_locked("f-locked", true, None).unwrap();

    let query_vec = one_hot(0);
    let nothing = std::collections::HashSet::new();
    let hidden = db
        .search_hybrid_visible("merger", &query_vec, 10, 0.0, &nothing, None)
        .unwrap();
    assert!(
        !hidden.iter().any(|h| h.meeting.id == "sealed"),
        "sealed meeting's TRANSCRIPT chunk leaked through the hybrid gate"
    );
    let mut unlocked = std::collections::HashSet::new();
    unlocked.insert("f-locked".to_string());
    let shown = db
        .search_hybrid_visible("merger", &query_vec, 10, 0.0, &unlocked, None)
        .unwrap();
    assert!(
        shown.iter().any(|h| h.meeting.id == "sealed"),
        "session-unlocked meeting's transcript chunk must reappear in hybrid results"
    );
}

/// PURGE-ON-SEAL covers transcript chunks: index BOTH classes while visible, then seal the folder
/// → ZERO transcript-source chunks remain at rest AND the sealed meeting surfaces ZERO through
/// both `search_semantic_visible` and `search_hybrid_visible`. RED if the seal purge missed the
/// transcript class.
#[test]
fn transcript_chunks_purged_on_seal() {
    let db = mem_db();
    seed_folder(&db, "f-locked", "Secret");
    db.insert_meeting(&sample_meeting("m1", "2026-06-24T10:00:00Z"))
        .unwrap();
    note_for(&db, "m1", "claude_code", "budget summary note");
    db.set_note_folder("m1", Some("f-locked")).unwrap();
    let segs = [
        tseg(
            0,
            0.0,
            4.0,
            "me",
            "budget discussion said aloud but not summarized",
        ),
        tseg(
            1,
            4.0,
            9.0,
            "others",
            "we will cut the marketing line item next quarter",
        ),
    ];
    db.index_meeting_chunks("m1", &segs, &crate::embed::StubEmbedder)
        .unwrap();
    assert!(
        chunk_count_of(&db, "m1", "transcript") > 0,
        "transcript chunks present before seal"
    );

    // Seal the folder (blank note + relock blanker → purge_chunks_tx).
    db.seal_note("m1", "claude_code", b"ciphertext").unwrap();
    let mut folders = std::collections::HashSet::new();
    folders.insert("f-locked".to_string());
    db.blank_sealed_notes_in_folders(&folders).unwrap();

    assert_eq!(
        chunk_count_of(&db, "m1", "transcript"),
        0,
        "transcript chunks must be purged on seal (no said-content at rest)"
    );
    assert_eq!(
        chunk_count(&db, "m1"),
        0,
        "ALL chunk classes purged on seal"
    );
    assert_eq!(vec_count(&db, "m1"), 0, "all vectors purged on seal");

    // And they surface nowhere through either gated reader.
    let query = one_hot(0);
    let nothing = std::collections::HashSet::new();
    assert!(
        !db.search_semantic_visible(&query, 10, 0.0, &nothing)
            .unwrap()
            .iter()
            .any(|h| h.meeting.id == "m1"),
        "sealed meeting must not surface via semantic search after purge"
    );
    assert!(
        !db.search_hybrid_visible("marketing", &query, 10, 0.0, &nothing, None)
            .unwrap()
            .iter()
            .any(|h| h.meeting.id == "m1"),
        "sealed meeting must not surface via hybrid search after purge"
    );
}

/// `delete_meeting` must also purge the vec0 layer. `vec_chunks` is an FK-less vec0 vtab, so the
/// `meetings` ON DELETE CASCADE reaches `note_chunks` but NOT `vec_chunks` — without an explicit
/// purge the deleted meeting's invertible embeddings ORPHAN at rest (and a reused rowid could
/// later PK-conflict). Counts the RAW `vec_chunks` table on purpose: a JOIN-through-note_chunks
/// count would false-green, since the cascade already removed the note_chunks rows. (Closes
/// lock-security-review finding 2.)
#[test]
fn vec_chunks_purged_on_delete_meeting() {
    let db = mem_db();
    db.insert_meeting(&sample_meeting("m1", "2026-06-24T10:00:00Z"))
        .unwrap();
    note_for(
        &db,
        "m1",
        "claude_code",
        "First budget paragraph.\n\nSecond hiring paragraph.",
    );
    db.index_meeting_chunks("m1", &[], &crate::embed::StubEmbedder)
        .unwrap();
    let raw_vecs = |db: &Db| -> i64 {
        db.lock()
            .query_row("SELECT COUNT(*) FROM vec_chunks", [], |r| r.get(0))
            .unwrap()
    };
    assert!(raw_vecs(&db) > 0, "expected vectors after indexing");

    db.delete_meeting("m1").unwrap();

    assert_eq!(
        chunk_count(&db, "m1"),
        0,
        "note_chunks gone after delete_meeting (FK cascade)"
    );
    assert_eq!(
        raw_vecs(&db),
        0,
        "vec_chunks must be purged on delete_meeting — no orphaned invertible vector at rest"
    );
}

/// HYBRID RRF fusion over real FTS + vector inputs: a meeting strong in BOTH lists ranks above
/// one strong in only one; dedup is one hit per meeting.
#[test]
fn vec_hybrid_fuses_fts_and_vector() {
    let db = mem_db();
    for (id, ts) in [
        ("both", "2026-06-24T10:00:00Z"),
        ("fts_only", "2026-06-24T11:00:00Z"),
        ("vec_only", "2026-06-24T12:00:00Z"),
    ] {
        db.insert_meeting(&sample_meeting(id, ts)).unwrap();
    }
    // FTS term "alpha": present in `both` and `fts_only` notes.
    note_for(&db, "both", "claude_code", "alpha shared topic");
    note_for(&db, "fts_only", "claude_code", "alpha only here");
    note_for(&db, "vec_only", "claude_code", "unrelated lexical");

    // Vector dim 0: `both` and `vec_only` chunks align with the query; `fts_only` is orthogonal.
    let query = one_hot(0);
    insert_known_chunk(&db, "both", "alpha shared topic", &one_hot(0));
    insert_known_chunk(&db, "vec_only", "unrelated lexical", &one_hot(0));
    insert_known_chunk(&db, "fts_only", "alpha only here", &one_hot(5));

    let nothing = std::collections::HashSet::new();
    let hits = db
        .search_hybrid_visible("alpha", &query, 10, 0.0, &nothing, None)
        .unwrap();
    let ids: Vec<&str> = hits.iter().map(|h| h.meeting.id.as_str()).collect();
    // One hit per meeting (dedup).
    let mut uniq = ids.clone();
    uniq.sort();
    uniq.dedup();
    assert_eq!(uniq.len(), ids.len(), "hybrid must dedup by meeting");
    // `both` is in BOTH ranked lists → must be the top fused result.
    assert_eq!(
        ids.first(),
        Some(&"both"),
        "meeting strong in both lists must rank first"
    );
    assert!(ids.contains(&"fts_only") && ids.contains(&"vec_only"));
}

/// Insert a LOCKED folder directly (the `mod tests` `seed_folder` makes an OPEN one).
fn seed_locked_folder(db: &Db, id: &str, name: &str) {
    db.insert_folder(&Folder {
        id: id.to_string(),
        name: name.to_string(),
        path: name.to_string(),
        parent_id: None,
        locked: true,
        created_at: "2026-06-26T00:00:00Z".to_string(),
    })
    .unwrap();
}

/// GRAPHRAG-LITE: the entity-graph leg surfaces a co-mentioned meeting that BOTH the FTS and
/// vector legs miss. Meeting `B` mentions the same entity as `A` but its note shares no query
/// term and it has no vector chunk — so only the entity-neighbour expansion can reach it.
#[test]
fn graph_leg_surfaces_co_mentioned_meeting_fts_and_vector_miss() {
    let db = mem_db();
    db.insert_meeting(&sample_meeting("A", "2026-06-24T10:00:00Z"))
        .unwrap();
    db.insert_meeting(&sample_meeting("B", "2026-06-24T11:00:00Z"))
        .unwrap();
    note_for(&db, "A", "claude_code", "atlas project kickoff notes");
    // B shares NO token with the query "atlas status" and gets no vector chunk.
    note_for(&db, "B", "claude_code", "quarterly logistics review");
    let atlas = db.upsert_entity("Atlas", EntityKind::Project).unwrap();
    db.add_mention(&atlas, "A").unwrap();
    db.add_mention(&atlas, "B").unwrap();

    let nothing = std::collections::HashSet::new();
    let empty_vec: Vec<f32> = Vec::new();
    // B is absent from FTS (note shares no query token) ...
    assert!(
        !db.search_visible("atlas status", 10, &nothing)
            .unwrap()
            .iter()
            .any(|h| h.meeting.id == "B"),
        "FTS must miss B"
    );
    // ... and absent from vector (no chunk at all → empty KNN).
    assert!(
        db.search_semantic_visible(&empty_vec, 10, 0.0, &nothing)
            .unwrap()
            .is_empty(),
        "vector must miss B"
    );
    // The query names entity Atlas → graph leg pulls in its neighbour B.
    let hits = db
        .search_hybrid_visible("atlas status", &empty_vec, 10, 0.0, &nothing, None)
        .unwrap();
    assert!(
        hits.iter().any(|h| h.meeting.id == "B"),
        "co-mentioned B must surface via the entity-graph leg"
    );
}

/// GATE: an entity-neighbour meeting in a SEALED-and-not-unlocked folder is EXCLUDED from the
/// expansion with an empty unlock set, and reappears once its folder id is unlocked. Also
/// covers the resolver gate: an entity mentioned ONLY in the sealed folder does not resolve.
/// (RED on an ungated neighbour query — both the resolver and the neighbour reader must gate.)
#[test]
fn graph_expansion_respects_lock_gate() {
    let db = mem_db();
    seed_locked_folder(&db, "f-secret", "Secret");
    db.insert_meeting(&sample_meeting("A", "2026-06-24T10:00:00Z"))
        .unwrap();
    db.insert_meeting(&sample_meeting("B", "2026-06-24T11:00:00Z"))
        .unwrap();
    note_for(&db, "A", "claude_code", "atlas open meeting");
    note_for(&db, "B", "claude_code", "atlas sealed meeting");
    db.set_note_folder("B", Some("f-secret")).unwrap();

    // Atlas is visible (mentioned in open A); Phantom is mentioned ONLY in sealed B.
    let atlas = db.upsert_entity("Atlas", EntityKind::Project).unwrap();
    db.add_mention(&atlas, "A").unwrap();
    db.add_mention(&atlas, "B").unwrap();
    let phantom = db.upsert_entity("Phantom", EntityKind::Project).unwrap();
    db.add_mention(&phantom, "B").unwrap();

    let empty = std::collections::HashSet::new();
    // Neighbour reader: B (sealed) excluded, A (open) kept.
    let nbrs = db
        .meetings_mentioning_entities_visible(std::slice::from_ref(&atlas), &empty)
        .unwrap();
    assert!(nbrs.iter().any(|m| m.id == "A"), "open neighbour A present");
    assert!(
        !nbrs.iter().any(|m| m.id == "B"),
        "sealed-folder neighbour B must be excluded with empty unlock set"
    );
    // Resolver: Phantom (sealed-only) does NOT resolve while locked.
    assert!(
        db.entities_matching_query("phantom report", &empty)
            .unwrap()
            .is_empty(),
        "an entity mentioned only in a sealed folder must not resolve"
    );

    // Unlock f-secret → both reappear.
    let mut unlocked = std::collections::HashSet::new();
    unlocked.insert("f-secret".to_string());
    let nbrs_u = db
        .meetings_mentioning_entities_visible(&[atlas], &unlocked)
        .unwrap();
    assert!(
        nbrs_u.iter().any(|m| m.id == "B"),
        "sealed neighbour B reappears when its folder is unlocked"
    );
    assert_eq!(
        db.entities_matching_query("phantom report", &unlocked)
            .unwrap(),
        vec![phantom],
        "Phantom resolves once its folder is unlocked"
    );
}

/// NOISE GUARD: a query that names no known entity leaves the hybrid result byte-identical to
/// the FTS∪vector RRF fusion (the graph leg is empty, no spurious expansion).
#[test]
fn no_entity_match_leaves_hybrid_identical_to_two_leg_fusion() {
    let db = mem_db();
    for (id, ts) in [
        ("m1", "2026-06-24T10:00:00Z"),
        ("m2", "2026-06-24T11:00:00Z"),
    ] {
        db.insert_meeting(&sample_meeting(id, ts)).unwrap();
    }
    note_for(&db, "m1", "claude_code", "budget planning notes");
    note_for(&db, "m2", "claude_code", "budget review summary");
    // An entity exists, but the query below does NOT name it.
    let atlas = db.upsert_entity("Atlas", EntityKind::Project).unwrap();
    db.add_mention(&atlas, "m1").unwrap();

    let query = one_hot(0);
    insert_known_chunk(&db, "m1", "budget planning notes", &one_hot(0));
    insert_known_chunk(&db, "m2", "budget review summary", &one_hot(3));

    let nothing = std::collections::HashSet::new();
    // The query names no entity → resolver empty.
    assert!(
        db.entities_matching_query("budget planning", &nothing)
            .unwrap()
            .is_empty(),
        "query must not resolve any entity"
    );

    // Expected = RRF over EXACTLY the two legs.
    let fts = db.search_visible("budget planning", 10, &nothing).unwrap();
    let sem = db
        .search_semantic_visible(&query, 10, 0.0, &nothing)
        .unwrap();
    let fts_ids: Vec<String> = fts.iter().map(|h| h.meeting.id.clone()).collect();
    let sem_ids: Vec<String> = sem.iter().map(|h| h.meeting.id.clone()).collect();
    let expected: Vec<String> = crate::embed::rrf_fuse(&[fts_ids, sem_ids], crate::embed::RRF_K)
        .into_iter()
        .map(|(id, _)| id)
        .collect();

    let got: Vec<String> = db
        .search_hybrid_visible("budget planning", &query, 10, 0.0, &nothing, None)
        .unwrap()
        .into_iter()
        .map(|h| h.meeting.id)
        .collect();
    assert_eq!(
        got, expected,
        "no-entity hybrid must equal the 2-leg fusion"
    );
}

/// 3-WAY RRF: a meeting present in ALL THREE legs (FTS + vector + entity-graph) ranks first,
/// and every meeting appears exactly once (dedup by meeting id across the legs).
#[test]
fn three_leg_rrf_ranks_multi_leg_first_and_dedups() {
    let db = mem_db();
    for (id, ts) in [
        ("all3", "2026-06-24T10:00:00Z"),
        ("fts_only", "2026-06-24T11:00:00Z"),
        ("graph_only", "2026-06-24T12:00:00Z"),
    ] {
        db.insert_meeting(&sample_meeting(id, ts)).unwrap();
    }
    // all3 + fts_only match the FTS query "atlas budget"; graph_only does not.
    note_for(&db, "all3", "claude_code", "atlas budget plan");
    note_for(&db, "fts_only", "claude_code", "atlas budget review");
    note_for(&db, "graph_only", "claude_code", "logistics offsite recap");
    // Entity Atlas mentioned in all3 + graph_only → both in the graph leg.
    let atlas = db.upsert_entity("Atlas", EntityKind::Project).unwrap();
    db.add_mention(&atlas, "all3").unwrap();
    db.add_mention(&atlas, "graph_only").unwrap();
    // Only all3 has a vector chunk aligned with the query → vector leg = {all3}.
    let query = one_hot(0);
    insert_known_chunk(&db, "all3", "atlas budget plan", &one_hot(0));

    let nothing = std::collections::HashSet::new();
    let hits = db
        .search_hybrid_visible("atlas budget", &query, 10, 0.0, &nothing, None)
        .unwrap();
    let ids: Vec<&str> = hits.iter().map(|h| h.meeting.id.as_str()).collect();
    // Dedup: each meeting once.
    let mut uniq = ids.clone();
    uniq.sort_unstable();
    uniq.dedup();
    assert_eq!(uniq.len(), ids.len(), "3-leg fusion must dedup by meeting");
    // all3 (in all three legs) outranks single-leg meetings.
    assert_eq!(
        ids.first(),
        Some(&"all3"),
        "meeting in all 3 legs ranks first"
    );
    assert!(ids.contains(&"fts_only") && ids.contains(&"graph_only"));
}

fn chunk_count(db: &Db, meeting_id: &str) -> i64 {
    let conn = db.lock();
    conn.query_row(
        "SELECT COUNT(*) FROM note_chunks WHERE meeting_id = ?1",
        rusqlite::params![meeting_id],
        |r| r.get(0),
    )
    .unwrap()
}

fn vec_count(db: &Db, meeting_id: &str) -> i64 {
    let conn = db.lock();
    conn.query_row(
        "SELECT COUNT(*) FROM vec_chunks v
               JOIN note_chunks nc ON nc.id = v.chunk_id
              WHERE nc.meeting_id = ?1",
        rusqlite::params![meeting_id],
        |r| r.get(0),
    )
    .unwrap()
}

// ── Brain v2 L1: topic chunks + temporal filter ───────────────────────────

fn topic_chunk_count(db: &Db, meeting_id: &str) -> i64 {
    let conn = db.lock();
    conn.query_row(
        "SELECT COUNT(*) FROM topic_chunks WHERE meeting_id = ?1",
        rusqlite::params![meeting_id],
        |r| r.get(0),
    )
    .unwrap()
}

fn topic_vec_count(db: &Db, meeting_id: &str) -> i64 {
    let conn = db.lock();
    conn.query_row(
        "SELECT COUNT(*) FROM topic_vec_chunks v
               JOIN topic_chunks tc ON tc.id = v.chunk_id
              WHERE tc.meeting_id = ?1",
        rusqlite::params![meeting_id],
        |r| r.get(0),
    )
    .unwrap()
}

fn long_seg(idx: i64, start: f64, end: f64, text: &str) -> Segment {
    Segment {
        idx,
        start_s: start,
        end_s: end,
        text: text.into(),
        speaker: Some("me".into()),
        confidence: None,
    }
}

/// L1.1 ROUND-TRIP + IDEMPOTENCY: indexing writes topic rows 1:1 with vec0 rows, the aug_text
/// carries the `<title> | <date>` header, a same-content re-index is a NO-OP (no row rewrite —
/// the content-hash probe), and changed segments produce a clean replace.
#[test]
fn topic_index_round_trips_and_is_idempotent() {
    let db = mem_db();
    db.insert_meeting(&sample_meeting("m1", "2026-06-24T10:00:00Z"))
        .unwrap();
    note_for(&db, "m1", "claude_code", "budget notes");
    let segs = vec![long_seg(
        0,
        0.0,
        120.0,
        "we planned the quarterly budget in detail",
    )];
    db.insert_segments("m1", &segs).unwrap();
    let nothing = std::collections::HashSet::new();

    db.index_meeting_topic_chunks("m1", &segs, &crate::embed::StubEmbedder, &nothing)
        .unwrap();
    assert!(
        topic_chunk_count(&db, "m1") > 0,
        "topic rows must be written"
    );
    assert_eq!(
        topic_chunk_count(&db, "m1"),
        topic_vec_count(&db, "m1"),
        "topic chunks and vectors must be 1:1"
    );
    let aug: String = {
        let conn = db.lock();
        conn.query_row(
            "SELECT aug_text FROM topic_chunks WHERE meeting_id = 'm1' LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert!(
        aug.starts_with("(untitled) | 2026-06-24\n"),
        "aug_text must carry the `<title> | <date>` header, got: {aug:?}"
    );

    // Same content ⇒ NO rewrite (the content-hash probe short-circuits before any delete).
    // Sentinel: mutate a column OUTSIDE the hash — a purge-then-reinsert would reset it, a
    // true no-op preserves it.
    {
        let conn = db.lock();
        conn.execute(
            "UPDATE topic_chunks SET start_s = 999.0 WHERE meeting_id = 'm1'",
            [],
        )
        .unwrap();
    }
    db.index_meeting_topic_chunks("m1", &segs, &crate::embed::StubEmbedder, &nothing)
        .unwrap();
    let sentinel: f64 = {
        let conn = db.lock();
        conn.query_row(
            "SELECT MIN(start_s) FROM topic_chunks WHERE meeting_id = 'm1'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(
        sentinel, 999.0,
        "same-content re-index must be a no-op (content_hash idempotency)"
    );

    // Changed content ⇒ clean replace (old text gone, new text present, still 1:1 with vec0).
    let segs2 = vec![long_seg(
        0,
        0.0,
        120.0,
        "completely different hiring topic now",
    )];
    db.insert_segments("m1", &segs2).unwrap();
    db.index_meeting_topic_chunks("m1", &segs2, &crate::embed::StubEmbedder, &nothing)
        .unwrap();
    let texts: Vec<String> = {
        let conn = db.lock();
        let mut stmt = conn
            .prepare("SELECT text FROM topic_chunks WHERE meeting_id = 'm1' ORDER BY seg_index")
            .unwrap();
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        rows
    };
    assert!(
        texts.iter().all(|t| t.contains("hiring")) && !texts.is_empty(),
        "changed content must clean-replace the topic rows, got: {texts:?}"
    );
    assert!(
        texts.iter().all(|t| !t.contains("budget")),
        "old topic text must be gone after the clean replace"
    );
    assert_eq!(topic_chunk_count(&db, "m1"), topic_vec_count(&db, "m1"));
}

/// 2026-07-13 launch-freeze fix: `backfill_topic_chunks_idempotent` bounds REAL (re)embeds to
/// a per-run cap — a vault where EVERY meeting genuinely needs (re)embedding (e.g. Brain
/// freshly enabled on this device) must not try to embed the whole vault in one unthrottled
/// pass on a single app launch. The cap is a no-op cost-wise: the idempotency probe means the
/// NEXT call just resumes past the meetings already done.
#[test]
fn topic_backfill_caps_real_reembeds_per_run_and_resumes_next_call() {
    let db = mem_db();
    const TOTAL: usize = 55;
    const CAP: usize = 50; // mirrors TOPIC_BACKFILL_MAX_REEMBED_PER_RUN
    for i in 0..TOTAL {
        let id = format!("m{i}");
        db.insert_meeting(&sample_meeting(&id, "2026-06-24T10:00:00Z"))
            .unwrap();
        db.insert_segments(&id, &[long_seg(0, 0.0, 60.0, "quarterly planning topic")])
            .unwrap();
    }

    let first_pass = db
        .backfill_topic_chunks_idempotent(&crate::embed::StubEmbedder)
        .unwrap();
    assert_eq!(
        first_pass, CAP,
        "the first run must stop at the per-run cap, not index the whole vault in one pass"
    );
    let indexed_after_first = (0..TOTAL)
        .filter(|i| topic_chunk_count(&db, &format!("m{i}")) > 0)
        .count();
    assert_eq!(
        indexed_after_first, CAP,
        "exactly the capped count of meetings should have topic rows after the first run"
    );

    // Idempotent resume: the SECOND run skips the already-indexed meetings for free (their
    // hash matches — no cursor needed, the hash probe IS the cursor) and reaches the rest.
    let second_pass = db
        .backfill_topic_chunks_idempotent(&crate::embed::StubEmbedder)
        .unwrap();
    assert_eq!(
        second_pass, TOTAL,
        "the second run must finish touching every remaining meeting"
    );
    let indexed_after_second = (0..TOTAL)
        .filter(|i| topic_chunk_count(&db, &format!("m{i}")) > 0)
        .count();
    assert_eq!(
        indexed_after_second, TOTAL,
        "every meeting must be indexed after the second run"
    );
}

/// Test-only `Embedder` that records the SIZE of every `embed()` call it receives (via
/// `embed_passage`'s default impl) instead of the actual text content — used to prove
/// `index_meeting_topic_chunks_reporting` sub-batches a long meeting's topic chunks rather
/// than embedding them all in one call.
struct RecordingEmbedder {
    call_sizes: std::sync::Mutex<Vec<usize>>,
}

impl crate::embed::Embedder for RecordingEmbedder {
    fn dim(&self) -> usize {
        crate::embed::EMBED_DIM
    }
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.call_sizes.lock().unwrap().push(texts.len());
        Ok(texts
            .iter()
            .map(|_| vec![0.0f32; crate::embed::EMBED_DIM])
            .collect())
    }
}

/// 2026-07-13 launch-freeze fix (round 2): a single LONG meeting (topic chunks merge to >= 60s
/// each, so a 1h+ recording can produce 40-60 of them) must NOT be embedded in one Candle/Metal
/// call sized to the whole meeting — that burst scales with ONE meeting's length and is
/// untouched by the per-run meeting-count cap (a freshly-Brain-enabled vault containing a long
/// recording kept freezing on launch even after that cap shipped). Proves the embed calls are
/// sub-batched (via the shared `embed_in_sub_batches` helper) 8 at a time.
#[test]
fn long_meeting_topic_chunks_are_embedded_in_small_sub_batches() {
    let db = mem_db();
    db.insert_meeting(&sample_meeting("m-long", "2026-06-24T10:00:00Z"))
        .unwrap();
    // 10 topic blocks: each a single 65s segment (>= the 60s merge floor, so it survives as
    // its own topic) separated by a 35s lull (>= the 30s lull-boundary threshold) from the next.
    const BLOCKS: i64 = 10;
    let mut segs = Vec::new();
    let mut cursor = 0.0f64;
    for i in 0..BLOCKS {
        let start = cursor;
        let end = start + 65.0;
        segs.push(long_seg(i, start, end, &format!("topic block number {i}")));
        cursor = end + 35.0;
    }
    db.insert_segments("m-long", &segs).unwrap();
    let nothing = std::collections::HashSet::new();

    let embedder = RecordingEmbedder {
        call_sizes: std::sync::Mutex::new(Vec::new()),
    };
    db.index_meeting_topic_chunks("m-long", &segs, &embedder, &nothing)
        .unwrap();

    assert_eq!(
        topic_chunk_count(&db, "m-long"),
        BLOCKS,
        "sanity: the fixture must actually produce one topic chunk per block"
    );
    let calls = embedder.call_sizes.into_inner().unwrap();
    assert!(
        calls.iter().all(|&n| n <= 8),
        "no single embed call may exceed the sub-batch size, got call sizes: {calls:?}"
    );
    assert_eq!(
        calls.iter().sum::<usize>(),
        BLOCKS as usize,
        "every topic chunk must still get embedded exactly once across the sub-batches"
    );
    assert!(
        calls.len() > 1,
        "10 chunks over an 8-per-batch cap must take more than one embed call, got: {calls:?}"
    );
}

/// Brain v3 PR-4 Fix 3 (RED→GREEN): `embed_in_sub_batches_progress` reports `(done, total)`
/// sub-batch counts as each embed sub-batch completes — the real counts the import path streams to
/// the FE ("Embedding k/M") instead of the old always-`0/0`. 20 texts over an 8-per-batch cap →
/// 3 sub-batches, so progress must land 1/3, 2/3, 3/3 in order and end at total==done.
#[test]
fn embed_sub_batch_progress_reports_real_running_counts() {
    let embedder = RecordingEmbedder {
        call_sizes: std::sync::Mutex::new(Vec::new()),
    };
    let texts: Vec<String> = (0..20).map(|i| format!("chunk {i}")).collect();
    let seen: std::sync::Mutex<Vec<(usize, usize)>> = std::sync::Mutex::new(Vec::new());
    let progress = |done: usize, total: usize| seen.lock().unwrap().push((done, total));

    let vecs = embed_in_sub_batches_progress(&embedder, &texts, &progress).unwrap();
    assert_eq!(vecs.len(), 20, "every text is embedded exactly once");

    let seen = seen.into_inner().unwrap();
    assert_eq!(
        seen,
        vec![(1, 3), (2, 3), (3, 3)],
        "progress reports each sub-batch as done/total, in order, ending at total==done"
    );
}

/// Fix 3: the small-input path (<= one sub-batch) still reports a single 1/1 completion, so a tiny
/// document's progress bar reaches 100% rather than never firing.
#[test]
fn embed_sub_batch_progress_small_input_reports_one_of_one() {
    let embedder = RecordingEmbedder {
        call_sizes: std::sync::Mutex::new(Vec::new()),
    };
    let texts: Vec<String> = (0..3).map(|i| format!("chunk {i}")).collect();
    let seen: std::sync::Mutex<Vec<(usize, usize)>> = std::sync::Mutex::new(Vec::new());
    let progress = |done: usize, total: usize| seen.lock().unwrap().push((done, total));

    let vecs = embed_in_sub_batches_progress(&embedder, &texts, &progress).unwrap();
    assert_eq!(vecs.len(), 3);
    assert_eq!(
        seen.into_inner().unwrap(),
        vec![(1, 1)],
        "small input → single 1/1 completion"
    );
}

/// 2026-07-13 launch-freeze fix (round 3): `index_meeting_chunks` (the REGULAR transcript/note
/// indexer, run on every meeting Stop — not just the startup catch-up) had the identical
/// unbounded-single-call bug as the topic-chunk indexer above. A long transcript (~1000
/// chars/chunk target) from a 1h+ recording can produce dozens of chunks; this proves BOTH the
/// note and transcript embed calls now share `embed_in_sub_batches` too.
#[test]
fn long_transcript_chunks_are_embedded_in_small_sub_batches() {
    let db = mem_db();
    db.insert_meeting(&sample_meeting("m-long-transcript", "2026-06-24T10:00:00Z"))
        .unwrap();
    // 20 segments alternating speaker (chunk_transcript's turn-grouping only breaks a turn on
    // a SPEAKER CHANGE, not a time gap — same-speaker segments merge into one turn and "a
    // single oversized turn becomes its own chunk, never split mid-turn", so a same-speaker
    // fixture would collapse to ONE chunk regardless of length). Alternating speakers forces
    // 20 distinct turns totaling well over TRANSCRIPT_CHUNK_CHAR_TARGET(1000) x 8 chars, so
    // the sliding window packs them into well more than 8 chunks.
    let mut segs = Vec::new();
    for i in 0..20i64 {
        let body =
            format!("segment number {i} discusses a distinct agenda topic in detail. ").repeat(10);
        let speaker = if i % 2 == 0 { "me" } else { "others" };
        segs.push(Segment {
            idx: i,
            start_s: i as f64 * 20.0,
            end_s: i as f64 * 20.0 + 18.0,
            text: body,
            speaker: Some(speaker.into()),
            confidence: None,
        });
    }
    db.insert_segments("m-long-transcript", &segs).unwrap();

    let embedder = RecordingEmbedder {
        call_sizes: std::sync::Mutex::new(Vec::new()),
    };
    db.index_meeting_chunks("m-long-transcript", &segs, &embedder)
        .unwrap();

    let calls = embedder.call_sizes.into_inner().unwrap();
    assert!(
        calls.iter().all(|&n| n <= 8),
        "no single embed call may exceed the sub-batch size, got call sizes: {calls:?}"
    );
    assert!(
        calls.len() > 1,
        "a long transcript must take more than one embed call, got: {calls:?}"
    );
    assert_eq!(
            chunk_count(&db, "m-long-transcript"),
            calls.iter().sum::<usize>() as i64,
            "every produced chunk (note + transcript classes) must be embedded exactly once across the sub-batches"
        );
}

/// L1.5 RED-contrast: WITHOUT a date filter an out-of-window meeting that matches lexically IS
/// returned by hybrid search (the pre-L1.5 behavior — the RED half); WITH the window it is
/// EXCLUDED while the in-window match stays (the GREEN half). FTS-only hybrid (empty query
/// vector) so the assertion isolates the lexical leg.
#[test]
fn hybrid_date_filter_excludes_out_of_window_lexical_match() {
    let db = mem_db();
    db.insert_meeting(&sample_meeting("m-in", "2026-06-24T10:00:00Z"))
        .unwrap();
    db.insert_meeting(&sample_meeting("m-out", "2026-05-01T10:00:00Z"))
        .unwrap();
    note_for(&db, "m-in", "claude_code", "budżet kwartalny omówiony");
    note_for(&db, "m-out", "claude_code", "budżet roczny omówiony");
    let nothing = std::collections::HashSet::new();

    // RED baseline (no filter): BOTH lexical matches surface.
    let unfiltered = db
        .search_hybrid_visible("budżet", &[], 10, 0.0, &nothing, None)
        .unwrap();
    assert!(unfiltered.iter().any(|h| h.meeting.id == "m-in"));
    assert!(
        unfiltered.iter().any(|h| h.meeting.id == "m-out"),
        "without a window the out-of-window lexical match must still be returned"
    );

    // GREEN: the "last week of the 2026-06-29 anchor" window excludes m-out on EVERY leg.
    let window = Some(("2026-06-22".to_string(), "2026-06-29".to_string()));
    let filtered = db
        .search_hybrid_visible("budżet", &[], 10, 0.0, &nothing, window)
        .unwrap();
    assert!(
        filtered.iter().any(|h| h.meeting.id == "m-in"),
        "the in-window lexical match must survive the filter"
    );
    assert!(
        !filtered.iter().any(|h| h.meeting.id == "m-out"),
        "date-filtered hybrid search must exclude an out-of-window meeting that matches lexically"
    );
}

/// L1.5 temporal FALLBACK: a query whose tokens match NOTHING lexically but that carries a
/// window returns the visible meetings IN the window (`matched_in: "temporal"`), newest-first —
/// and never an out-of-window one.
#[test]
fn search_visible_in_range_falls_back_to_temporal_window() {
    let db = mem_db();
    db.insert_meeting(&sample_meeting("m-in", "2026-06-24T10:00:00Z"))
        .unwrap();
    db.insert_meeting(&sample_meeting("m-out", "2026-05-01T10:00:00Z"))
        .unwrap();
    note_for(&db, "m-in", "claude_code", "retro zespołu wnioski");
    note_for(&db, "m-out", "claude_code", "kickoff projektu");
    let nothing = std::collections::HashSet::new();

    let window = Some(("2026-06-22".to_string(), "2026-06-29".to_string()));
    // No token of this query appears in any note ⇒ pure window fallback.
    let hits = db
        .search_visible_in_range("what did we discuss", 10, &nothing, window)
        .unwrap();
    assert_eq!(
        hits.len(),
        1,
        "only the in-window meeting may be returned: {hits:?}"
    );
    assert_eq!(hits[0].meeting.id, "m-in");
    assert_eq!(hits[0].matched_in, "temporal");

    // Without a window the same no-match query returns nothing (unchanged FTS behavior).
    let none = db
        .search_visible_in_range("what did we discuss", 10, &nothing, None)
        .unwrap();
    assert!(
        none.is_empty(),
        "no window + no lexical match ⇒ empty, as before"
    );
}

#[test]
fn prunable_candidates_are_oldest_first_and_exclude_locked_folders() {
    const GOOD_KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let p = unique_temp_path("murmur-prunable", "sqlite");
    let _ = std::fs::remove_file(&p);
    let db = Db::open_with_key(&p, GOOD_KEY).unwrap();

    // Two meetings in an OPEN vault-root (folder_id NULL), one in a LOCKED folder.
    for (id, at) in [
        ("old", "2026-01-01T00:00:00Z"),
        ("new", "2026-06-01T00:00:00Z"),
        ("secret", "2026-03-01T00:00:00Z"),
    ] {
        db.insert_meeting(&crate::storage::Meeting {
            id: id.into(),
            started_at: at.into(),
            ended_at: None,
            title: Some("t".into()),
            duration_s: 1,
            audio_path: Some(format!("/a/{id}.wav")),
            status: crate::storage::MeetingStatus::Summarized,
            folder_id: None,
        })
        .unwrap();
        db.upsert_note(&crate::storage::NoteRecord {
            meeting_id: id.into(),
            provider_id: "claude_code".into(),
            markdown: "m".into(),
            created_at: at.into(),
            exported_path: None,
            model_requested: None,
            model_served: None,
            gateway_host: None,
        })
        .unwrap();
    }
    db.insert_folder(&crate::storage::Folder {
        id: "f".into(),
        name: "Secret".into(),
        path: "Secret".into(),
        parent_id: None,
        locked: true,
        created_at: "2026-01-01T00:00:00Z".into(),
    })
    .unwrap();
    db.set_note_folder("secret", Some("f")).unwrap();

    let cands = db.prunable_audio_candidates().unwrap();
    let ids: Vec<&str> = cands.iter().map(|c| c.meeting_id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["old", "new"],
        "locked 'secret' excluded; oldest-first order"
    );
    let _ = std::fs::remove_file(&p);
}

#[test]
fn prunable_candidates_fail_closed_for_dangling_and_ambiguous_owners() {
    let db = mem_db();
    for id in ["open-a", "open-b"] {
        db.insert_folder(&crate::storage::Folder {
            id: id.into(),
            name: id.into(),
            path: id.into(),
            parent_id: None,
            locked: false,
            created_at: "2026-01-01T00:00:00Z".into(),
        })
        .unwrap();
    }
    for (idx, id) in [
        "ownerless",
        "canonical-open",
        "canonical-dangling",
        "legacy-open",
        "legacy-dangling",
        "legacy-ambiguous",
    ]
    .into_iter()
    .enumerate()
    {
        db.insert_meeting(&crate::storage::Meeting {
            id: id.into(),
            started_at: format!("2026-01-{:02}T00:00:00Z", idx + 1),
            ended_at: None,
            title: Some(id.into()),
            duration_s: 1,
            audio_path: Some(format!("/audio/{id}.wav")),
            status: crate::storage::MeetingStatus::Summarized,
            folder_id: match id {
                "canonical-open" => Some("open-a".into()),
                "canonical-dangling" => Some("missing-folder".into()),
                _ => None,
            },
        })
        .unwrap();
    }
    let seed_provider = |meeting_id: &str, provider_id: &str, folder_id: Option<&str>| {
        db.upsert_note(&crate::storage::NoteRecord {
            meeting_id: meeting_id.into(),
            provider_id: provider_id.into(),
            markdown: "body".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            exported_path: None,
            model_requested: None,
            model_served: None,
            gateway_host: None,
        })
        .unwrap();
        db.lock()
            .execute(
                "UPDATE notes SET folder_id=?3 WHERE meeting_id=?1 AND provider_id=?2",
                rusqlite::params![meeting_id, provider_id, folder_id],
            )
            .unwrap();
    };
    seed_provider("legacy-open", "p1", Some("open-a"));
    seed_provider("legacy-dangling", "p1", Some("missing-folder"));
    seed_provider("legacy-ambiguous", "p1", Some("open-a"));
    seed_provider("legacy-ambiguous", "p2", Some("open-b"));

    let ids: Vec<String> = db
        .prunable_audio_candidates()
        .unwrap()
        .into_iter()
        .map(|row| row.meeting_id)
        .collect();
    assert_eq!(
        ids,
        vec!["ownerless", "canonical-open", "legacy-open"],
        "only truly ownerless or unambiguously open-owned audio may be pruned"
    );
}

#[test]
fn delete_folder_refuses_a_remaining_pre_note_canonical_meeting_owner() {
    let db = mem_db();
    db.insert_folder(&crate::storage::Folder {
        id: "folder".into(),
        name: "Folder".into(),
        path: "Folder".into(),
        parent_id: None,
        locked: false,
        created_at: "2026-01-01T00:00:00Z".into(),
    })
    .unwrap();
    let mut meeting = sample_meeting("pre-note", "2026-01-01T00:00:00Z");
    meeting.folder_id = Some("folder".into());
    db.insert_meeting(&meeting).unwrap();

    let error = db.delete_folder("folder").unwrap_err();
    assert!(matches!(error, AppError::Storage(_)));
    assert!(db.folder_by_id("folder").unwrap().is_some());
    assert_eq!(
        db.get_meeting("pre-note").unwrap().unwrap().folder_id.as_deref(),
        Some("folder"),
        "the refused storage delete must leave canonical ownership intact"
    );
}

/// Regression for the note-save contention bug (2026-07-15): `open_with_key` MUST install a
/// busy handler (non-zero `PRAGMA busy_timeout`) on every connection it opens, so a writer
/// that collides with another connection's write lock on the SAME on-disk file gets a brief
/// internal wait instead of an IMMEDIATE `SQLITE_BUSY` surfacing as `AppError::Storage`. This
/// asserts the pragma value directly (cheap, deterministic) rather than trying to race two
/// threads against a timing window.
#[test]
fn open_with_key_sets_a_nonzero_busy_timeout() {
    const GOOD_KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let p = unique_temp_path("murmur-busy-timeout-pragma", "sqlite");
    let _ = std::fs::remove_file(&p);
    let db = Db::open_with_key(&p, GOOD_KEY).unwrap();
    let ms: i64 = db
        .lock()
        .query_row("PRAGMA busy_timeout", [], |r| r.get(0))
        .unwrap();
    assert!(
        ms >= 1000,
        "busy_timeout must be a generous non-zero wait (got {ms}ms) — a 0ms timeout means the \
             very first writer-lock collision fails immediately with SQLITE_BUSY instead of \
             queuing briefly"
    );
    let _ = std::fs::remove_file(&p);
}

/// Concurrent-writer regression: two SEPARATE connections to the same on-disk DB file (mirrors
/// two real `Db` handles — e.g. the main app handle and a background job's own connection)
/// must NOT immediately fail with `SQLITE_BUSY` when one holds a write transaction while the
/// other tries to write. Before the `busy_timeout` fix (`PRAGMA busy_timeout` defaulted to 0),
/// this reproduced the exact failure a note autosave hit when it raced a background writer:
/// the second connection's write failed on the spot instead of waiting briefly for the first
/// to commit. RED against the pre-fix connection setup (default 0ms timeout): this test would
/// intermittently/deterministically fail (immediate "database is locked") if the busy handler
/// were removed — confirmed by temporarily reverting the `busy_timeout` call locally.
#[test]
fn concurrent_writers_do_not_immediately_hit_sqlite_busy() {
    use std::sync::{Arc, Barrier};
    use std::thread;
    use std::time::Duration;

    const GOOD_KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let p = unique_temp_path("murmur-concurrent-writers", "sqlite");
    let _ = std::fs::remove_file(&p);
    // Seed the schema through the normal path first (also proves both handles below share it).
    {
        let seed = Db::open_with_key(&p, GOOD_KEY).unwrap();
        seed.insert_meeting(&crate::storage::Meeting {
            id: "seed".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
            ended_at: None,
            title: Some("t".into()),
            duration_s: 1,
            audio_path: None,
            status: crate::storage::MeetingStatus::Draft,
            folder_id: None,
        })
        .unwrap();
    }

    let db_a = Arc::new(Db::open_with_key(&p, GOOD_KEY).unwrap());
    let db_b = Db::open_with_key(&p, GOOD_KEY).unwrap();
    let barrier = Arc::new(Barrier::new(2));

    // Thread A: open an IMMEDIATE write transaction, hold it briefly (simulating a background
    // writer's in-flight statement), then commit.
    let db_a_thread = Arc::clone(&db_a);
    let barrier_a = Arc::clone(&barrier);
    let handle = thread::spawn(move || -> Result<()> {
        let conn = db_a_thread.lock();
        conn.execute_batch("BEGIN IMMEDIATE;").map_err(map_err)?;
        barrier_a.wait();
        thread::sleep(Duration::from_millis(200));
        conn.execute_batch("COMMIT;").map_err(map_err)?;
        Ok(())
    });

    // Thread B (this thread): wait until A holds its write lock, then attempt a write of its
    // own on a DIFFERENT connection to the same file. With the busy_timeout in place, this
    // must succeed (waiting out A's 200ms hold) instead of failing on the spot.
    barrier.wait();
    let write_result = db_b.insert_meeting(&crate::storage::Meeting {
        id: "concurrent".into(),
        started_at: "2026-01-02T00:00:00Z".into(),
        ended_at: None,
        title: Some("t".into()),
        duration_s: 1,
        audio_path: None,
        status: crate::storage::MeetingStatus::Draft,
        folder_id: None,
    });

    handle.join().unwrap().unwrap();
    assert!(
        write_result.is_ok(),
        "a concurrent writer must wait out a brief lock hold instead of failing immediately \
             with SQLITE_BUSY: {write_result:?}"
    );
    let _ = std::fs::remove_file(&p);
}

/// The Analytics tab must never reveal a sealed-and-not-unlocked folder's meetings — same
/// leak class `visibility_clause` closes for search/graph/brain reads. Regression for the
/// audit finding: `Db::analytics()` used to run bare `SELECT COUNT(*)`/`SUM(duration_s)`
/// over ALL meetings with no folder join at all (confirmed RED against the pre-fix
/// no-arg `analytics()`: it counted the sealed meeting too).
#[test]
fn analytics_excludes_sealed_not_unlocked_folder() {
    let db = mem_db();
    // Two OPEN meetings (folder_id NULL) + one meeting in a LOCKED, not-session-unlocked folder.
    // Dates are RELATIVE to now (well inside the analytics 30-day per_day window) so this test is
    // never calendar-flaky: a hardcoded absolute date ages OUT of the window as real time passes
    // (the old `open-1` at 2026-06-20 dropped off the chart exactly 30 days later — this is that fix).
    let day = |n: i64| (chrono::Utc::now() - chrono::Duration::days(n)).to_rfc3339();
    let (open1_at, open2_at, secret_at) = (day(5), day(4), day(3));
    for (id, at, dur) in [
        ("open-1", open1_at.as_str(), 600i64),
        ("open-2", open2_at.as_str(), 900i64),
        ("secret", secret_at.as_str(), 6000i64),
    ] {
        db.insert_meeting(&crate::storage::Meeting {
            id: id.into(),
            started_at: at.into(),
            ended_at: None,
            title: Some("t".into()),
            duration_s: dur,
            audio_path: None,
            status: crate::storage::MeetingStatus::Summarized,
            folder_id: None,
        })
        .unwrap();
        db.upsert_note(&crate::storage::NoteRecord {
            meeting_id: id.into(),
            provider_id: "claude_code".into(),
            markdown: "m".into(),
            created_at: at.into(),
            exported_path: None,
            model_requested: None,
            model_served: None,
            gateway_host: None,
        })
        .unwrap();
    }
    db.insert_folder(&crate::storage::Folder {
        id: "f-secret".into(),
        name: "Secret".into(),
        path: "Secret".into(),
        parent_id: None,
        locked: true,
        created_at: "2026-06-01T00:00:00Z".into(),
    })
    .unwrap();
    db.set_note_folder("secret", Some("f-secret")).unwrap();

    // Sealed and NOT session-unlocked: the "secret" meeting (6000s, its own day, status
    // Summarized) must be invisible everywhere in the aggregate.
    let nothing = std::collections::HashSet::new();
    let a = db.analytics(&nothing).unwrap();
    assert_eq!(a.total_meetings, 2, "sealed meeting must not be counted");
    assert_eq!(
        a.total_duration_s, 1500,
        "sealed meeting's duration must not contribute to the total"
    );
    assert_eq!(
        a.longest_duration_s, 900,
        "the sealed meeting's 6000s must not surface as the longest"
    );
    assert_eq!(a.avg_duration_s, 750);
    let by_status_total: i64 = a.by_status.iter().map(|s| s.count).sum();
    assert_eq!(
        by_status_total, 2,
        "status breakdown must not include the sealed meeting"
    );
    let per_day_total: i64 = a.per_day.iter().map(|d| d.count).sum();
    assert_eq!(
        per_day_total, 2,
        "per-day activity chart must not include the sealed meeting's day"
    );
    assert!(
        !a.per_day.iter().any(|d| d.date == secret_at[..10]),
        "the sealed meeting's day must not appear in the 30-day activity chart at all"
    );

    // Once the folder is SESSION-unlocked, the same meeting becomes visible again (reversible).
    let mut unlocked = std::collections::HashSet::new();
    unlocked.insert("f-secret".to_string());
    let a2 = db.analytics(&unlocked).unwrap();
    assert_eq!(a2.total_meetings, 3);
    assert_eq!(a2.total_duration_s, 7500);
    assert_eq!(a2.longest_duration_s, 6000);
}

/// The folder tree's `note_count` badge must never reveal a sealed-and-not-unlocked folder's
/// TRUE note count — same leak class `analytics_excludes_sealed_not_unlocked_folder` closes
/// for the Analytics tab. Regression for the audit finding: `Db::count_notes_per_folder()`
/// used to run a bare `SELECT folder_id, COUNT(*) FROM notes GROUP BY folder_id` with no join
/// to `folders` and no unlock-set gate at all — `seal_note` blanks a note's markdown on lock
/// but never deletes/reparents the `notes` row, so the sealed folder's exact note count leaked
/// into the sidebar tree (`list_folders` → `FolderNode.note_count`) even though the lock
/// model's invariant is that a sealed-and-not-unlocked folder must leak NOTHING.
#[test]
fn count_notes_per_folder_excludes_sealed_not_unlocked_folder() {
    let db = mem_db();
    db.insert_folder(&crate::storage::Folder {
        id: "f-open".into(),
        name: "Open".into(),
        path: "Open".into(),
        parent_id: None,
        locked: false,
        created_at: "2026-06-01T00:00:00Z".into(),
    })
    .unwrap();
    db.insert_folder(&crate::storage::Folder {
        id: "f-secret".into(),
        name: "Secret".into(),
        path: "Secret".into(),
        parent_id: None,
        locked: true,
        created_at: "2026-06-01T00:00:00Z".into(),
    })
    .unwrap();

    // 1 note in the OPEN folder, 3 notes in the LOCKED folder.
    for (id, folder) in [
        ("open-1", "f-open"),
        ("secret-1", "f-secret"),
        ("secret-2", "f-secret"),
        ("secret-3", "f-secret"),
    ] {
        db.insert_meeting(&crate::storage::Meeting {
            id: id.into(),
            started_at: "2026-06-20T10:00:00Z".into(),
            ended_at: None,
            title: Some("t".into()),
            duration_s: 60,
            audio_path: None,
            status: crate::storage::MeetingStatus::Summarized,
            folder_id: None,
        })
        .unwrap();
        db.upsert_note(&crate::storage::NoteRecord {
            meeting_id: id.into(),
            provider_id: "claude_code".into(),
            markdown: "m".into(),
            created_at: "2026-06-20T10:00:00Z".into(),
            exported_path: None,
            model_requested: None,
            model_served: None,
            gateway_host: None,
        })
        .unwrap();
        db.set_note_folder(id, Some(folder)).unwrap();
    }

    // Sealed and NOT session-unlocked: the locked folder must be ABSENT from the map (or read
    // back as 0), never its true count of 3.
    let nothing = std::collections::HashSet::new();
    let counts = db.count_notes_per_folder(&nothing).unwrap();
    assert_eq!(counts.get("f-open").copied().unwrap_or(0), 1);
    assert_eq!(
        counts.get("f-secret").copied().unwrap_or(0),
        0,
        "a sealed-and-not-unlocked folder must not leak its true note count"
    );

    // Once the folder is SESSION-unlocked, the true count becomes visible again (reversible).
    let mut unlocked = std::collections::HashSet::new();
    unlocked.insert("f-secret".to_string());
    let counts2 = db.count_notes_per_folder(&unlocked).unwrap();
    assert_eq!(counts2.get("f-secret").copied().unwrap_or(0), 3);
}

#[test]
fn note_aggregates_follow_canonical_and_unambiguous_meeting_visibility() {
    let db = mem_db();
    for (id, locked) in [("open-a", false), ("open-b", false), ("secret", true)] {
        db.insert_folder(&crate::storage::Folder {
            id: id.into(),
            name: id.into(),
            path: id.into(),
            parent_id: None,
            locked,
            created_at: "2026-01-01T00:00:00Z".into(),
        })
        .unwrap();
    }
    for (id, canonical) in [
        ("visible", Some("open-a")),
        ("canonical-secret", Some("secret")),
        ("ambiguous", None),
    ] {
        let mut meeting = sample_meeting(id, "2026-01-01T00:00:00Z");
        meeting.folder_id = canonical.map(str::to_string);
        db.insert_meeting(&meeting).unwrap();
    }
    let seed_provider = |meeting_id: &str, provider_id: &str, folder_id: &str| {
        db.upsert_note(&crate::storage::NoteRecord {
            meeting_id: meeting_id.into(),
            provider_id: provider_id.into(),
            markdown: "body".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            exported_path: None,
            model_requested: None,
            model_served: None,
            gateway_host: None,
        })
        .unwrap();
        // Deliberately preserve legacy/provider drift instead of using set_meeting_folder, whose
        // production contract synchronizes every row and therefore cannot construct the bug.
        db.lock()
            .execute(
                "UPDATE notes SET folder_id=?3 WHERE meeting_id=?1 AND provider_id=?2",
                rusqlite::params![meeting_id, provider_id, folder_id],
            )
            .unwrap();
    };
    seed_provider("visible", "p1", "open-a");
    // Stale OPEN provider metadata must not override the canonical LOCKED meeting owner.
    seed_provider("canonical-secret", "p1", "open-a");
    // Two individually open legacy owners are still ambiguous and therefore invisible.
    seed_provider("ambiguous", "p1", "open-a");
    seed_provider("ambiguous", "p2", "open-b");

    let nothing = std::collections::HashSet::new();
    let analytics = db.analytics(&nothing).unwrap();
    assert_eq!(analytics.total_meetings, 1);
    assert_eq!(analytics.notes_count, 1);
    assert_eq!(db.brain_counts(&nothing).unwrap().0, 1);
    let counts = db.count_notes_per_folder(&nothing).unwrap();
    assert_eq!(counts.get("open-a").copied().unwrap_or(0), 1);
    assert_eq!(counts.get("open-b").copied().unwrap_or(0), 0);
    assert_eq!(counts.get("secret").copied().unwrap_or(0), 0);

    let unlocked = std::collections::HashSet::from(["secret".to_string()]);
    let analytics = db.analytics(&unlocked).unwrap();
    assert_eq!(analytics.total_meetings, 2);
    assert_eq!(analytics.notes_count, 2);
    assert_eq!(db.brain_counts(&unlocked).unwrap().0, 2);
    let counts = db.count_notes_per_folder(&unlocked).unwrap();
    assert_eq!(counts.get("open-a").copied().unwrap_or(0), 1);
    assert_eq!(
        counts.get("secret").copied().unwrap_or(0),
        1,
        "canonical ownership determines the folder badge despite stale provider metadata"
    );
    assert_eq!(counts.get("open-b").copied().unwrap_or(0), 0);
}

/// Race-safety regression (TOCTOU: prune snapshots a plaintext path, a concurrent seal
/// re-points the column to `.enc`, prune must NOT null the sealed pointer). The conditional
/// clear only nulls when the column still holds the snapshotted plaintext path.
#[test]
fn clear_meeting_audio_path_if_only_nulls_a_matching_path() {
    let db = mem_db();
    db.insert_meeting(&crate::storage::Meeting {
        id: "M".into(),
        started_at: "2026-01-01T00:00:00Z".into(),
        ended_at: None,
        title: Some("t".into()),
        duration_s: 1,
        audio_path: Some("/d/M.wav".into()),
        status: crate::storage::MeetingStatus::Summarized,
        folder_id: None,
    })
    .unwrap();

    // Sealed-in-between case: the column was re-pointed to `/d/M.wav.enc` by a concurrent
    // seal; the prune's stale snapshot `/d/M.wav.enc` != current would wrongly match the OLD
    // unconditional NULL. The conditional clear (expected != current plaintext) must NO-OP.
    db.clear_meeting_audio_path_if("M", "/d/M.wav.enc").unwrap();
    assert_eq!(
        db.get_meeting("M").unwrap().unwrap().audio_path,
        Some("/d/M.wav".to_string()),
        "mismatched expected path must NOT null the column (sealed-in-between case)"
    );

    // Matching snapshot (no concurrent seal) → clears.
    db.clear_meeting_audio_path_if("M", "/d/M.wav").unwrap();
    assert_eq!(db.get_meeting("M").unwrap().unwrap().audio_path, None);
}

/// note_templates round-trip: insert → list returns the row with its `sections` (ordered) and
/// `extra_frontmatter_keys` intact through the JSON TEXT columns; REPLACE by id overwrites; delete
/// removes. CONTENT-FREE metadata (mirrors saved_recipes) — no visibility gate.
#[test]
fn note_templates_round_trip() {
    use crate::storage::models::{NoteTemplate, NoteTemplateSection};
    let db = mem_db();
    assert!(db.list_note_templates().unwrap().is_empty());

    let t = NoteTemplate {
        id: "tpl-1".to_string(),
        name: "Client call".to_string(),
        tone: "Warm, outcome-first".to_string(),
        sections: vec![
            NoteTemplateSection {
                heading: "Outcome".to_string(),
                instruction: "What we agreed.".to_string(),
            },
            NoteTemplateSection {
                heading: "Next steps".to_string(),
                instruction: "Owner — action.".to_string(),
            },
        ],
        extra_frontmatter_keys: vec!["client".to_string(), "project".to_string()],
        created_at: "2026-07-25T00:00:00Z".to_string(),
    };
    db.insert_note_template(&t).unwrap();

    let got = db.list_note_templates().unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].name, "Client call");
    assert_eq!(got[0].sections.len(), 2);
    assert_eq!(got[0].sections[0].heading, "Outcome");
    assert_eq!(got[0].sections[1].heading, "Next steps");
    assert_eq!(
        got[0].extra_frontmatter_keys,
        vec!["client".to_string(), "project".to_string()]
    );

    // REPLACE by id (INSERT OR REPLACE): same id, new content.
    let mut t2 = t.clone();
    t2.name = "Renamed".to_string();
    t2.sections.truncate(1);
    db.insert_note_template(&t2).unwrap();
    let got = db.list_note_templates().unwrap();
    assert_eq!(got.len(), 1, "replace, not append");
    assert_eq!(got[0].name, "Renamed");
    assert_eq!(got[0].sections.len(), 1);

    db.delete_note_template("tpl-1").unwrap();
    assert!(db.list_note_templates().unwrap().is_empty());
}

// ── S1 cosine-similarity floor + S2 AND→OR FTS fallback (retrieval-floor-fts-fallback) ──────────

/// Insert a doc-chunk row + its `doc_vec_chunks` embedding under a MEANING-controlled vector (the
/// document twin of [`insert_known_chunk`]). Used by the S1 floor tests.
fn insert_known_doc_chunk(db: &Db, document_id: &str, text: &str, vector: &[f32]) -> i64 {
    let conn = db.lock();
    conn.execute(
        "INSERT INTO doc_chunks (document_id, chunk_index, text) VALUES (?1, 0, ?2)",
        rusqlite::params![document_id, text],
    )
    .unwrap();
    let chunk_id = conn.last_insert_rowid();
    let blob = crate::embed::vec_to_blob(vector);
    conn.execute(
        "INSERT INTO doc_vec_chunks(chunk_id, embedding) VALUES (?1, ?2)",
        rusqlite::params![chunk_id, blob],
    )
    .unwrap();
    chunk_id
}

/// Insert an org item + one chunk + its int8 `org_vec_chunks` embedding under a controlled vector
/// (the KNN leg quantizes to int8, so the stored vector is `round(unit·127)`). `seed_org_state`
/// must have run first (the reader INNER JOINs `org_state` on `context_enabled = 1`).
fn insert_known_org_chunk(
    db: &Db,
    org_id: &str,
    item_id: &str,
    title: &str,
    text: &str,
    vector: &[f32],
) {
    let conn = db.lock();
    conn.execute(
        "INSERT INTO org_items (item_id, org_id, seq, author_hint, title, markdown, created_at, rev, generation, content_sha256, tombstoned)
             VALUES (?1, ?2, 1, 'anna', ?3, ?4, '2026-07-10T09:00:00Z', 1, 1, NULL, 0)",
        rusqlite::params![item_id, org_id, title, text],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO org_chunks (item_id, chunk_idx, text) VALUES (?1, 0, ?2)",
        rusqlite::params![item_id, text],
    )
    .unwrap();
    let chunk_id = conn.last_insert_rowid();
    let blob = crate::embed::vec_to_int8_blob(vector);
    conn.execute(
        "INSERT INTO org_vec_chunks(chunk_id, embedding) VALUES (?1, vec_int8(?2))",
        rusqlite::params![chunk_id, blob],
    )
    .unwrap();
}

/// S1 Test A (meetings): the cosine floor drops an ORTHOGONAL (cos 0.0) k-nearest neighbour while
/// keeping the near (cos 1.0) one. Proving BOTH survive with `min_cosine = 0.0` shows the exclusion
/// is the FLOOR, not the visibility gate.
#[test]
fn s1_floor_drops_orthogonal_semantic_neighbour() {
    let db = mem_db();
    db.insert_meeting(&sample_meeting("m-near", "2026-06-24T10:00:00Z"))
        .unwrap();
    db.insert_meeting(&sample_meeting("m-far", "2026-06-24T11:00:00Z"))
        .unwrap();
    note_for(&db, "m-near", "claude_code", "near");
    note_for(&db, "m-far", "claude_code", "far");
    insert_known_chunk(&db, "m-near", "near", &one_hot(0)); // cos 1.0 vs query
    insert_known_chunk(&db, "m-far", "far", &one_hot(2)); // cos 0.0 vs query

    let nothing = std::collections::HashSet::new();
    let query = one_hot(0);

    // No floor (0.0) → BOTH returned.
    let all = db
        .search_semantic_visible(&query, 10, 0.0, &nothing)
        .unwrap();
    let ids: Vec<&str> = all.iter().map(|h| h.meeting.id.as_str()).collect();
    assert!(
        ids.contains(&"m-near") && ids.contains(&"m-far"),
        "no-floor must return both, got {ids:?}"
    );

    // Floor 0.75 → only the near (cos 1.0) survives.
    let floored = db
        .search_semantic_visible(&query, 10, 0.75, &nothing)
        .unwrap();
    let fids: Vec<&str> = floored.iter().map(|h| h.meeting.id.as_str()).collect();
    assert_eq!(
        fids,
        vec!["m-near"],
        "floor must drop the orthogonal filler, got {fids:?}"
    );
}

/// S1 Test A (documents): same floor behaviour for the doc-chunk vector leg.
#[test]
fn s1_floor_drops_orthogonal_doc_chunk() {
    let db = mem_db();
    seed_folder(&db, "f-open", "Project");
    db.insert_document("d-near", "f-open", "near.md", "near body", "document", 100)
        .unwrap();
    db.insert_document("d-far", "f-open", "far.md", "far body", "document", 100)
        .unwrap();
    insert_known_doc_chunk(&db, "d-near", "near body", &one_hot(0));
    insert_known_doc_chunk(&db, "d-far", "far body", &one_hot(2));

    let nothing = std::collections::HashSet::new();
    let query = one_hot(0);

    let all = db
        .search_doc_chunks_visible(&query, 10, 0.0, &nothing)
        .unwrap();
    let ids: Vec<&str> = all.iter().map(|h| h.document_id.as_str()).collect();
    assert!(
        ids.contains(&"d-near") && ids.contains(&"d-far"),
        "no-floor must return both docs, got {ids:?}"
    );

    let floored = db
        .search_doc_chunks_visible(&query, 10, 0.75, &nothing)
        .unwrap();
    let fids: Vec<&str> = floored.iter().map(|h| h.document_id.as_str()).collect();
    assert_eq!(
        fids,
        vec!["d-near"],
        "floor must drop the orthogonal doc, got {fids:?}"
    );
}

/// S1 Test A (org int8): the floor drops an orthogonal item on the int8 leg — proving the `/127`
/// rescale (the int8 vectors are `round(unit·127)`, a different distance distribution).
#[test]
fn s1_floor_drops_orthogonal_org_chunk_int8() {
    let db = mem_db();
    seed_org_state(&db, "org-1");
    insert_known_org_chunk(&db, "org-1", "it-near", "Near", "near body", &one_hot(0));
    insert_known_org_chunk(&db, "org-1", "it-far", "Far", "far body", &one_hot(2));

    let query = one_hot(0);

    let all = db.search_org_chunks_knn(&query, 10, 0.0).unwrap();
    let ids: Vec<&str> = all.iter().map(|h| h.item_id.as_str()).collect();
    assert!(
        ids.contains(&"it-near") && ids.contains(&"it-far"),
        "no-floor must return both items, got {ids:?}"
    );

    let floored = db.search_org_chunks_knn(&query, 10, 0.75).unwrap();
    let fids: Vec<&str> = floored.iter().map(|h| h.item_id.as_str()).collect();
    assert_eq!(
        fids,
        vec!["it-near"],
        "int8 /127-rescaled floor must drop the orthogonal item, got {fids:?}"
    );
}

/// S1 Test B (recall safety): with the vector leg FLOORED to empty on an irrelevant corpus, an
/// EXACT-WORD FTS hit still surfaces through `search_hybrid_visible` — the floor never touches the
/// FTS/graph legs, and `score_fuse`'s empty-leg redistribution rescales the survivors.
#[test]
fn s1_floor_keeps_fts_hit_when_vector_leg_floored_empty() {
    let db = mem_db();
    db.insert_meeting(&sample_meeting("m-budget", "2026-06-24T10:00:00Z"))
        .unwrap();
    // FTS text carries "budget"; the ONLY chunk is orthogonal (one_hot(2)) to the query one_hot(0).
    note_for(
        &db,
        "m-budget",
        "claude_code",
        "the quarterly budget review",
    );
    insert_known_chunk(&db, "m-budget", "the quarterly budget review", &one_hot(2));

    let nothing = std::collections::HashSet::new();
    let hits = db
        .search_hybrid_visible("budget", &one_hot(0), 10, 0.75, &nothing, None)
        .unwrap();
    assert!(
        hits.iter().any(|h| h.meeting.id == "m-budget"),
        "an exact-word FTS hit must survive even when the vector leg is floored to empty"
    );
}

/// S2 Test C (meetings): the AND→OR fallback recovers a multi-word miss — "etykieta parcel" (AND
/// misses, no "etykieta") matches a note containing only "parcel" via the OR twin.
#[test]
fn s2_and_to_or_fallback_recovers_multiword_miss_meetings() {
    let db = mem_db();
    db.insert_meeting(&sample_meeting("m1", "2026-06-24T10:00:00Z"))
        .unwrap();
    note_for(&db, "m1", "claude_code", "parcel size delivery schedule");
    let nothing = std::collections::HashSet::new();
    let hits = db.search_visible("etykieta parcel", 10, &nothing).unwrap();
    assert!(
        hits.iter().any(|h| h.meeting.id == "m1"),
        "AND→OR fallback must recover a note sharing only 'parcel'"
    );
}

/// S2 Test C (documents): the same AND→OR fallback in the doc-chunk FTS reader.
#[test]
fn s2_and_to_or_fallback_recovers_multiword_miss_docs() {
    let db = mem_db();
    seed_folder(&db, "f-open", "Project");
    db.insert_document(
        "d1",
        "f-open",
        "spec.md",
        "parcel size delivery schedule",
        "document",
        100,
    )
    .unwrap();
    db.index_document_chunks("d1", None).unwrap();
    let nothing = std::collections::HashSet::new();
    let hits = db
        .search_doc_chunks_fts_visible("etykieta parcel", 10, &nothing)
        .unwrap();
    assert!(
        hits.iter().any(|h| h.document_id == "d1"),
        "AND→OR fallback must recover a document sharing only 'parcel'"
    );
}

/// S2 Test C (org): the same AND→OR fallback in the org-chunk FTS reader.
#[test]
fn s2_and_to_or_fallback_recovers_multiword_miss_org() {
    let db = mem_db();
    seed_org_state(&db, "org-1");
    db.upsert_org_item(
        "it-1",
        "org-1",
        1,
        "anna",
        "Parcels",
        "parcel size delivery schedule",
        "2026-07-10T09:00:00Z",
        1,
        1,
        &sha32(1),
        None,
        None,
        None,
    )
    .unwrap();
    let hits = db.search_org_chunks_fts("etykieta parcel", 10).unwrap();
    assert!(
        hits.iter().any(|h| h.item_id == "it-1"),
        "AND→OR fallback must recover an org item sharing only 'parcel'"
    );
}

/// ORG SEARCH RELEVANCE FLOOR: the OR fallback is allowed only when at least
/// `ceil(unique_content_terms / 2)` exact FTS tokens match ONE chunk. This uses a real, file-backed
/// SQLCipher handle (the production `Db::open_with_key` path), not a Rust substring approximation.
/// It pins the two reported coverage boundaries, duplicate-query-term resistance, exact-token
/// matching (`Kong` != `Kongo`), the SQL-construction term bound, and the existing two-term
/// cross-language fallback.
#[test]
fn org_fts_or_fallback_requires_ceil_half_exact_unique_content_terms() {
    const TEST_DEK: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let path = unique_temp_path("murmur-org-fts-coverage", "sqlite");
    let _ = std::fs::remove_file(&path);
    let db = Db::open_with_key(&path, TEST_DEK).unwrap();
    seed_org_state(&db, "org-1");

    let ingest = |item_id: &str, title: &str, body: &str, tag: u8| {
        db.upsert_org_item(
            item_id,
            "org-1",
            u64::from(tag),
            "anna",
            title,
            body,
            "2026-07-10T09:00:00Z",
            1,
            1,
            &sha32(tag),
            None,
            None,
            None,
        )
        .unwrap();
    };

    // Six unique content terms => threshold 3. One exact token is noise; three is signal.
    ingest("six-one", "One of six", "Kong travel diary", 1);
    ingest(
        "six-three",
        "Three of six",
        "hybrid source operator decision",
        2,
    );
    // A second matching chunk for the same item must not duplicate the returned item.
    db.lock()
        .execute(
            "INSERT INTO org_chunks (item_id, chunk_idx, text) VALUES (?1, ?2, ?3)",
            rusqlite::params!["six-three", 99_i64, "hybrid source operator follow-up"],
        )
        .unwrap();
    // A prefix/substring is not an exact unicode61 token match for `kong`.
    ingest("six-kongo", "Kongo is different", "Kongo travel diary", 3);
    let six = db
        .search_org_chunks_fts("hybrid mode source truth kong operator", 10)
        .unwrap();
    let six_ids: Vec<&str> = six.iter().map(|hit| hit.item_id.as_str()).collect();
    assert_eq!(
        six_ids,
        vec!["six-three"],
        "1/6 and Kongo-prefix noise must be rejected while 3/6 survives: {six_ids:?}"
    );

    // Result bounds stay hard after SQL coverage qualification and per-item dedup.
    ingest(
        "six-three-b",
        "Another three of six",
        "mode truth operator decision",
        7,
    );
    let bounded = db
        .search_org_chunks_fts("hybrid mode source truth kong operator", 1)
        .unwrap();
    assert_eq!(
        bounded.len(),
        1,
        "the requested result bound must stay hard"
    );

    // Three unique content terms => threshold 2.
    ingest("three-one", "One of three", "violet memo", 4);
    ingest("three-two", "Two of three", "violet ember memo", 5);
    let three = db.search_org_chunks_fts("violet quartz ember", 10).unwrap();
    let three_ids: Vec<&str> = three.iter().map(|hit| hit.item_id.as_str()).collect();
    assert_eq!(
        three_ids,
        vec!["three-two"],
        "1/3 must be rejected while 2/3 survives: {three_ids:?}"
    );

    // Repeating `violet` cannot turn one matched token into multiple coverage votes.
    let duplicate = db
        .search_org_chunks_fts("violet violet quartz ember", 10)
        .unwrap();
    let duplicate_ids: Vec<&str> = duplicate.iter().map(|hit| hit.item_id.as_str()).collect();
    assert_eq!(
        duplicate_ids,
        vec!["three-two"],
        "duplicate query terms must neither inflate coverage nor the threshold: {duplicate_ids:?}"
    );

    // One-term exact token sanity: `Kong` must not match `Kongo`.
    let kong = db.search_org_chunks_fts("kong", 10).unwrap();
    let kong_ids: Vec<&str> = kong.iter().map(|hit| hit.item_id.as_str()).collect();
    assert_eq!(kong_ids, vec!["six-one"], "Kong != Kongo: {kong_ids:?}");

    // Existing cross-language AND→OR recovery remains: 2 terms => ceil(2/2) = 1.
    ingest(
        "parcel-fallback",
        "Parcel fallback",
        "parcel size delivery schedule",
        6,
    );
    let fallback = db.search_org_chunks_fts("etykieta parcel", 10).unwrap();
    assert!(
        fallback.iter().any(|hit| hit.item_id == "parcel-fallback"),
        "the existing two-term fallback must still recover a one-token domain match"
    );

    // Fallback SQL construction is bounded independently of the full strict expression. A query
    // with 65 unique content terms uses its first bounded 32 fallback terms; matching 16 of those
    // reaches ceil(32 / 2) without SQLite prepare-limit errors.
    let first_half = (0..16)
        .map(|index| format!("term{index:03}"))
        .collect::<Vec<_>>()
        .join(" ");
    ingest("bounded-query", "Bounded query", &first_half, 8);
    let overlong_query = (0..65)
        .map(|index| format!("term{index:03}"))
        .collect::<Vec<_>>()
        .join(" ");
    let overlong = db.search_org_chunks_fts(&overlong_query, 10).unwrap();
    assert!(
        overlong.iter().any(|hit| hit.item_id == "bounded-query"),
        "overlong query must use a bounded term list for fallback SQL: {overlong:?}"
    );

    // The strict phase remains the full original query: once a row matching all 65 terms exists,
    // strict AND returns it and the fallback must not also admit a row matching only the first 16.
    ingest(
        "strict-full-query",
        "Strict full query",
        &overlong_query,
        11,
    );
    let strict_overlong = db.search_org_chunks_fts(&overlong_query, 10).unwrap();
    let strict_ids = strict_overlong
        .iter()
        .map(|hit| hit.item_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        strict_ids,
        vec!["strict-full-query"],
        "full strict AND must not truncate to the fallback term cap: {strict_ids:?}"
    );

    // Unicode lowercase expansion and unicode61 diacritic folding must agree with the coverage
    // list. `İİİ`/`iii` and precomposed/decomposed `résumé`/`resume` are each one logical FTS token,
    // never multiple votes.
    let unicode_terms = {
        let conn = db.lock();
        fts_unicode61_content_terms(
            &conn,
            "İİİ iii résumé resume re\u{301}sume\u{301} quartz ember",
            32,
        )
        .unwrap()
    };
    assert_eq!(
        unicode_terms,
        vec!["iii", "resume", "quartz", "ember"],
        "canonical terms must use SQLite unicode61 single-token identity"
    );
    ingest("unicode-i", "Unicode I", "iii memo", 9);
    let unicode_i = db
        .search_org_chunks_fts("İİİ iii quartz ember", 10)
        .unwrap();
    assert!(
        unicode_i.iter().all(|hit| hit.item_id != "unicode-i"),
        "equivalent lowercase-expanding terms must count once, not satisfy 2/3: {unicode_i:?}"
    );
    ingest("unicode-accent", "Unicode accent", "resume memo", 10);
    let unicode_accent = db
        .search_org_chunks_fts("résumé resume quartz ember", 10)
        .unwrap();
    assert!(
        unicode_accent
            .iter()
            .all(|hit| hit.item_id != "unicode-accent"),
        "unicode61-equivalent diacritic terms must count once, not satisfy 2/3: {unicode_accent:?}"
    );
    let unicode_decomposed = db
        .search_org_chunks_fts("résumé re\u{301}sume\u{301} quartz ember", 10)
        .unwrap();
    assert!(
        unicode_decomposed
            .iter()
            .all(|hit| hit.item_id != "unicode-accent"),
        "decomposed and precomposed unicode61-equivalent terms must count once, not satisfy 2/3: \
         {unicode_decomposed:?}"
    );

    drop(db);
    let _ = std::fs::remove_file(path);
}

/// S2 Test D (precision preserved — the QA "No meetings match" guard): a query whose content words
/// appear in NONE of the corpus notes stays EMPTY even after the OR fallback (no shared content
/// word ⇒ the OR built from the query's own content words matches nothing).
#[test]
fn s2_precision_preserved_no_shared_content_word() {
    let db = mem_db();
    db.insert_meeting(&sample_meeting("m1", "2026-06-24T10:00:00Z"))
        .unwrap();
    db.insert_meeting(&sample_meeting("m2", "2026-06-24T11:00:00Z"))
        .unwrap();
    note_for(&db, "m1", "claude_code", "parcel size delivery schedule");
    note_for(&db, "m2", "claude_code", "roadmap planning session notes");
    let nothing = std::collections::HashSet::new();
    let hits = db
        .search_visible("shipment label generation", 10, &nothing)
        .unwrap();
    assert!(
        hits.is_empty(),
        "OR fallback must NOT match when no content word is shared, got {:?}",
        hits.iter()
            .map(|h| h.meeting.id.clone())
            .collect::<Vec<_>>()
    );
}

/// S2 Test E (stopword-only overlap must NOT hit): a query sharing ONLY stopwords ("the", "is")
/// with a note stays empty — `fts_match_query_any` drops stopwords/<3-char before building the OR,
/// so the shared function words can never produce a hit through the fallback.
#[test]
fn s2_stopword_only_overlap_does_not_hit() {
    let db = mem_db();
    db.insert_meeting(&sample_meeting("m1", "2026-06-24T10:00:00Z"))
        .unwrap();
    note_for(&db, "m1", "claude_code", "the plan is done");
    let nothing = std::collections::HashSet::new();
    let hits = db
        .search_visible("what is the status", 10, &nothing)
        .unwrap();
    assert!(
        hits.is_empty(),
        "stopword-only overlap must not produce a hit through the OR fallback, got {:?}",
        hits.iter()
            .map(|h| h.meeting.id.clone())
            .collect::<Vec<_>>()
    );
}

/// RED before the sweep existed: `unique_temp_path` never removed anything, so every fixture any
/// test ever minted stayed in `TMPDIR` forever (1,599 `.sqlite` + 216 `-wal` + 216 `-shm` measured
/// live, and 67.8 GB accumulated in the harness evidence store).
#[test]
fn sweep_removes_stale_fixtures_of_both_prefix_families() {
    let root = unique_temp_path("murmur-sweep-scope", "dir");
    std::fs::create_dir_all(&root).unwrap();
    for name in [
        "murmur-old.sqlite",
        "murmur-old.sqlite-wal",
        "meetnotes-old.sqlite",
    ] {
        std::fs::write(root.join(name), b"x").unwrap();
    }
    // Callers pass ext = "dir"; directories must be reclaimed too, not just files.
    std::fs::create_dir_all(root.join("murmur-old.dir")).unwrap();
    std::fs::write(root.join("murmur-old.dir/inner"), b"x").unwrap();
    // A foreign temp file must survive: the sweep is namespaced, not a blanket TMPDIR wipe.
    std::fs::write(root.join("unrelated.sqlite"), b"x").unwrap();

    sweep_stale_temp_fixtures(&root, std::time::Duration::ZERO);

    for name in [
        "murmur-old.sqlite",
        "murmur-old.sqlite-wal",
        "meetnotes-old.sqlite",
        "murmur-old.dir",
    ] {
        assert!(
            !root.join(name).exists(),
            "{name} is a stale murmur/meetnotes fixture and must be swept"
        );
    }
    assert!(
        root.join("unrelated.sqlite").exists(),
        "the sweep must never touch files outside the murmur-/meetnotes- namespace"
    );
    std::fs::remove_dir_all(&root).unwrap();
}

/// The age guard is what makes the sweep safe next to a CONCURRENT test process: an entry younger
/// than `min_age` is always left alone, so live fixtures can never be deleted out from under a run.
#[test]
fn sweep_leaves_fixtures_younger_than_min_age() {
    let root = unique_temp_path("murmur-sweep-age", "dir");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("murmur-live.sqlite"), b"x").unwrap();

    sweep_stale_temp_fixtures(&root, STALE_FIXTURE_AGE);

    assert!(
        root.join("murmur-live.sqlite").exists(),
        "a fixture younger than STALE_FIXTURE_AGE belongs to a possibly-live process"
    );
    std::fs::remove_dir_all(&root).unwrap();
}

/// Deleting a meeting must revoke the conversations that could have drawn on ITS folder, and leave
/// every other conversation alone.
///
/// Before this, `delete_meeting` called the global sweep, whose predicate
/// (`provenance_mode = 'globalDerived'`) matches EVERY durable row — so tidying up one recording
/// destroyed the entire Ask history of the vault, including conversations about folders that had
/// not changed at all. A feature whose headline is that it persists did not survive a delete.
///
/// The three assertions are deliberately separate: "the unrelated one survives" is the fix, "the
/// related one dies" is the property the fix must NOT trade away, and "it is still readable" is
/// what makes survival meaningful — a row that outlives the purge but is stamped at a stale
/// visibility generation is invisible, which is the same loss wearing a different hat.
#[test]
fn deleting_a_meeting_revokes_only_conversations_that_could_have_seen_its_folder() {
    use crate::storage::models::AskConversationScope;

    let db = mem_db();
    seed_folder(&db, "f-a", "Alpha");
    seed_folder(&db, "f-b", "Beta");

    let mut meeting = sample_meeting("m-in-a", "2026-09-03T09:00:00Z");
    meeting.folder_id = Some("f-a".to_string());
    db.insert_meeting(&meeting).unwrap();

    // One conversation per folder, each declaring only that folder as its dependency.
    for (folder, question) in [("f-a", "about alpha"), ("f-b", "about beta")] {
        db.persist_ask_exchange(
            &AskConversationScope::Vault,
            None,
            question,
            "an answer",
            &[],
            &[],
            &[],
            &[folder.to_string()],
            "2026-09-03T09:01:00Z",
        )
        .unwrap();
    }
    let unlocked = HashSet::new();
    assert_eq!(
        db.list_ask_conversation_ids(&AskConversationScope::Vault, &unlocked)
            .unwrap()
            .len(),
        2,
        "both conversations start visible"
    );

    db.delete_meeting("m-in-a").unwrap();

    let surviving = db
        .list_ask_conversation_ids(&AskConversationScope::Vault, &unlocked)
        .unwrap();
    assert_eq!(
        surviving.len(),
        1,
        "exactly one conversation should survive — the one that never depended on the deleted \
         meeting's folder. Got {surviving:?}"
    );

    let rows: Vec<(String, String)> = {
        let conn = db.lock();
        let mut stmt = conn
            .prepare(
                "SELECT c.id, d.dependency_ref
                   FROM ask_conversations c
                   JOIN ask_conversation_dependencies d ON d.conversation_id = c.id
                  WHERE d.dependency_kind = 'folder'",
            )
            .unwrap();
        let out = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        out
    };
    assert_eq!(
        rows.iter().map(|(_, f)| f.as_str()).collect::<Vec<_>>(),
        vec!["f-b"],
        "the survivor must be the folder-B conversation, not whichever one happened to be left"
    );
}

/// The same delete, for an UNFILED meeting, still takes the whole history.
///
/// This is not an oversight to fix later. An unfiled meeting has no folder row, so
/// `visible_folder_ids` never records a dependency naming it, so no conversation can be matched by
/// folder — and a conversation may well paraphrase content the user just deleted. The global sweep
/// is the only fail-closed answer, and pinning it here stops a later "optimisation" from scoping a
/// case that cannot be scoped.
#[test]
fn deleting_an_unfiled_meeting_still_revokes_everything() {
    use crate::storage::models::AskConversationScope;

    let db = mem_db();
    seed_folder(&db, "f-b", "Beta");
    db.insert_meeting(&sample_meeting("m-unfiled", "2026-09-03T09:00:00Z"))
        .unwrap();
    db.persist_ask_exchange(
        &AskConversationScope::Vault,
        None,
        "about beta",
        "an answer",
        &[],
        &[],
        &[],
        &["f-b".to_string()],
        "2026-09-03T09:01:00Z",
    )
    .unwrap();

    db.delete_meeting("m-unfiled").unwrap();

    assert!(
        db.list_ask_conversation_ids(&AskConversationScope::Vault, &HashSet::new())
            .unwrap()
            .is_empty(),
        "an unfiled meeting names no folder, so the fail-closed global sweep must still run"
    );
}
