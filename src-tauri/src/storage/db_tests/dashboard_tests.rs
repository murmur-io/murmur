//! File-backed tests for the DASHBOARD store (2026-08-03) — board/tile CRUD, ordering, the
//! kind allowlist, span clamping, and the two lock-model-relevant properties:
//!
//! 1. `entity_mention_pulse_visible` is GATED — a mention that exists only inside a sealed,
//!    not-session-unlocked meeting contributes NOTHING to the pulse.
//! 2. `migrate()` stays idempotent with the new tables (it runs on every open).
//!
//! These use `open_with_key` + a fixed literal DEK, so they never touch the Keychain.

use super::*;
use crate::storage::models::{
    AskConversationScope, EntityKind, Folder, Meeting, MeetingStatus, NoteRecord,
};

/// The same fixed placeholder the sibling file-backed suites use — the documented
/// MURMUR_DEV_DEK-shaped literal, never a real Keychain DEK.
const TEST_DEK: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"; // MURMUR_DEV_DEK placeholder

fn file_db(label: &str) -> Db {
    Db::open_with_key(
        &super::unique_temp_path(&format!("meetnotes-dash-test-{label}"), "sqlite"),
        TEST_DEK,
    )
    .unwrap()
}

#[test]
fn missing_ask_dispatch_singleton_is_never_reseeded_on_reopen() {
    let path = super::unique_temp_path("meetnotes-dash-test-missing-ask-generation", "sqlite");
    let db = Db::open_with_key(&path, TEST_DEK).unwrap();
    assert_eq!(db.ask_dispatch_generation().unwrap(), 0);
    let dashboard_id = board(&db, "missing-generation");
    db.insert_dashboard_living_answer_tile(
        "t-missing-generation",
        &dashboard_id,
        4,
        "Must remain private",
        "[]",
        "2026-08-03T10:00:00Z",
    )
    .unwrap();
    let context_generation = db
        .dashboard_structural_context_state(&dashboard_id)
        .unwrap()
        .0;
    db.store_dashboard_living_answer_cas(
        "t-missing-generation",
        &dashboard_id,
        "Must remain private",
        "OLD LIVING ANSWER MUST NOT REVIVE",
        "2026-08-03T10:01:00Z",
        "[]",
        context_generation,
        "missing-generation-digest",
        200_000,
    )
    .unwrap();
    let conversation_id = db
        .persist_ask_exchange(
            &AskConversationScope::Vault,
            None,
            "OLD HISTORY QUESTION MUST NOT REVIVE",
            "OLD HISTORY ANSWER MUST NOT REVIVE",
            &[],
            &[],
            &[],
            &[],
            "2026-08-03T10:02:00Z",
        )
        .unwrap();
    db.lock()
        .execute("DELETE FROM ask_dispatch_state WHERE singleton=1", [])
        .unwrap();
    drop(db);
    assert!(matches!(
        Db::open_with_key(&path, TEST_DEK),
        Err(AppError::Storage(message)) if message == "Ask dispatch generation is unavailable"
    ));

    // Inspect the encrypted fixture without running `Db::migrate` again. The failed reopen must
    // neither repair generation 0 (which would revive the stamped payloads) nor clear their bytes.
    let conn = Connection::open(&path).unwrap();
    conn.pragma_update(None, "key", format!("x'{TEST_DEK}'"))
        .unwrap();
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM ask_dispatch_state", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        conn.query_row(
            "SELECT living_answer FROM dashboard_tiles WHERE id='t-missing-generation'",
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap(),
        "OLD LIVING ANSWER MUST NOT REVIVE"
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM ask_conversation_messages WHERE conversation_id=?1",
            [&conversation_id],
            |row| row.get::<_, i64>(0),
        )
        .unwrap(),
        2
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn missing_ask_dispatch_table_with_generation_stamps_is_never_reseeded() {
    let path = super::unique_temp_path("meetnotes-dash-test-missing-ask-table", "sqlite");
    let db = Db::open_with_key(&path, TEST_DEK).unwrap();
    let dashboard_id = board(&db, "missing-generation-table");
    db.insert_dashboard_living_answer_tile(
        "t-missing-generation-table",
        &dashboard_id,
        4,
        "Must remain private",
        "[]",
        "2026-08-03T10:00:00Z",
    )
    .unwrap();
    let context_generation = db
        .dashboard_structural_context_state(&dashboard_id)
        .unwrap()
        .0;
    db.store_dashboard_living_answer_cas(
        "t-missing-generation-table",
        &dashboard_id,
        "Must remain private",
        "DROPPED TABLE ANSWER MUST NOT REVIVE",
        "2026-08-03T10:01:00Z",
        "[]",
        context_generation,
        "missing-generation-table-digest",
        200_000,
    )
    .unwrap();
    let conversation_id = db
        .persist_ask_exchange(
            &AskConversationScope::Vault,
            None,
            "DROPPED TABLE QUESTION MUST NOT REVIVE",
            "DROPPED TABLE HISTORY MUST NOT REVIVE",
            &[],
            &[],
            &[],
            &[],
            "2026-08-03T10:02:00Z",
        )
        .unwrap();
    db.lock().execute_batch("DROP TABLE ask_dispatch_state;").unwrap();
    drop(db);

    assert!(matches!(
        Db::open_with_key(&path, TEST_DEK),
        Err(AppError::Storage(message)) if message == "Ask dispatch generation is unavailable"
    ));
    let conn = Connection::open(&path).unwrap();
    conn.pragma_update(None, "key", format!("x'{TEST_DEK}'"))
        .unwrap();
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='ask_dispatch_state'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap(),
        0,
        "failed startup must not recreate a lost feature-era generation table"
    );
    assert_eq!(
        conn.query_row(
            "SELECT living_answer FROM dashboard_tiles WHERE id='t-missing-generation-table'",
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap(),
        "DROPPED TABLE ANSWER MUST NOT REVIVE"
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM ask_conversation_messages WHERE conversation_id=?1",
            [&conversation_id],
            |row| row.get::<_, i64>(0),
        )
        .unwrap(),
        2
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn malformed_ask_dispatch_singleton_is_not_coerced_on_reopen() {
    let path = super::unique_temp_path("meetnotes-dash-test-malformed-ask-generation", "sqlite");
    let db = Db::open_with_key(&path, TEST_DEK).unwrap();
    db.lock()
        .execute_batch(
            "PRAGMA ignore_check_constraints=ON;
             UPDATE ask_dispatch_state SET generation='corrupt' WHERE singleton=1;
             PRAGMA ignore_check_constraints=OFF;",
        )
        .unwrap();
    drop(db);

    assert!(matches!(
        Db::open_with_key(&path, TEST_DEK),
        Err(AppError::Storage(message)) if message == "Ask dispatch generation is unavailable"
    ));
    let conn = Connection::open(&path).unwrap();
    conn.pragma_update(None, "key", format!("x'{TEST_DEK}'"))
        .unwrap();
    assert_eq!(
        conn.query_row(
            "SELECT typeof(generation), CAST(generation AS TEXT) FROM ask_dispatch_state WHERE singleton=1",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .unwrap(),
        ("text".to_string(), "corrupt".to_string()),
        "failed startup must not coerce malformed state into a reusable generation"
    );
    let _ = std::fs::remove_file(path);
}

fn board(db: &Db, title: &str) -> String {
    let id = format!("board-{title}");
    db.insert_dashboard(
        &id,
        title,
        Some("🚀"),
        Some("indigo"),
        "2026-08-03T10:00:00Z",
    )
    .unwrap();
    id
}

#[test]
fn dashboard_crud_round_trips() {
    let db = file_db("crud");
    assert!(db.list_dashboards().unwrap().is_empty());

    let id = board(&db, "atlas");
    let all = db.list_dashboards().unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].title, "atlas");
    assert_eq!(all[0].emoji.as_deref(), Some("🚀"));
    assert!(!all[0].pinned);

    // Patch: only the supplied fields move; `None` leaves a column untouched.
    assert!(db
        .update_dashboard(
            &id,
            Some("Atlas GA"),
            None,
            None,
            Some(true),
            "2026-08-03T11:00:00Z"
        )
        .unwrap());
    let d = db.get_dashboard(&id).unwrap().unwrap();
    assert_eq!(d.title, "Atlas GA");
    assert_eq!(
        d.emoji.as_deref(),
        Some("🚀"),
        "untouched field survives a patch"
    );
    assert!(d.pinned);
    assert_eq!(d.updated_at, "2026-08-03T11:00:00Z");

    assert!(db.delete_dashboard(&id).unwrap());
    assert!(db.get_dashboard(&id).unwrap().is_none());
    assert!(
        !db.delete_dashboard(&id).unwrap(),
        "second delete is a no-op"
    );
}

#[test]
fn context_generation_is_monotonic_and_survives_delete_recreate() {
    let db = file_db("context-generation");
    assert_eq!(db.dashboard_context_state("stable-id").unwrap(), (0, false));

    db.insert_dashboard("stable-id", "X", None, None, "2026-08-03T10:00:00Z")
        .unwrap();
    db.insert_dashboard_tile(
        "t1",
        "stable-id",
        "note",
        Some("n1"),
        None,
        4,
        None,
        "2026-08-03T10:00:01Z",
    )
    .unwrap();
    db.insert_dashboard_tile(
        "t2",
        "stable-id",
        "note",
        Some("n2"),
        None,
        4,
        None,
        "2026-08-03T10:00:02Z",
    )
    .unwrap();
    let created = db.dashboard_context_state("stable-id").unwrap();
    let structural_created = db.dashboard_structural_context_state("stable-id").unwrap();
    assert!(created.1);

    db.reorder_dashboard_tiles("stable-id", &["t2".into(), "t1".into()])
        .unwrap();
    db.reorder_dashboard_tiles("stable-id", &["t1".into(), "t2".into()])
        .unwrap();
    let round_trip = db.dashboard_context_state("stable-id").unwrap();
    let structural_round_trip = db.dashboard_structural_context_state("stable-id").unwrap();
    assert!(
        round_trip.0 > created.0,
        "tile order X→Y→X must change the witness"
    );
    assert!(structural_round_trip.0 > structural_created.0);

    db.delete_dashboard("stable-id").unwrap();
    let deleted = db.dashboard_context_state("stable-id").unwrap();
    let structural_deleted = db.dashboard_structural_context_state("stable-id").unwrap();
    assert!(!deleted.1);
    assert!(deleted.0 > round_trip.0);
    assert!(!structural_deleted.1);
    assert!(structural_deleted.0 > structural_round_trip.0);

    db.insert_dashboard("stable-id", "X", None, None, "2026-08-03T10:03:00Z")
        .unwrap();
    let recreated = db.dashboard_context_state("stable-id").unwrap();
    let structural_recreated = db.dashboard_structural_context_state("stable-id").unwrap();
    assert!(recreated.1);
    assert!(
        recreated.0 > deleted.0,
        "delete→recreate with the same id/payload must not restore a stale witness"
    );
    assert!(structural_recreated.1);
    assert!(structural_recreated.0 > structural_deleted.0);
}

#[test]
fn dashboard_chrome_update_does_not_invalidate_composite_identity() {
    let db = file_db("context-chrome");
    db.insert_dashboard("board", "Before", None, None, "2026-08-03T10:00:00Z")
        .unwrap();
    let before = db.dashboard_context_state("board").unwrap();
    db.update_dashboard(
        "board",
        Some("After"),
        Some("✨"),
        Some("blue"),
        Some(true),
        "2026-08-03T10:01:00Z",
    )
    .unwrap();
    assert_eq!(
        db.dashboard_context_state("board").unwrap(),
        before,
        "live-resolved chrome is not provider grounding"
    );
}

#[test]
fn pinned_boards_sort_first() {
    let db = file_db("pinned-sort");
    let a = board(&db, "a");
    let b = board(&db, "b");
    db.update_dashboard(&b, None, None, None, Some(true), "2026-08-03T12:00:00Z")
        .unwrap();
    let ids: Vec<String> = db
        .list_dashboards()
        .unwrap()
        .into_iter()
        .map(|d| d.id)
        .collect();
    assert_eq!(ids, vec![b, a], "pinned first, then position");
}

#[test]
fn tiles_append_clamp_and_reorder() {
    let db = file_db("tiles");
    let b = board(&db, "tiles");

    for (i, kind) in ["note", "meeting", "pulse"].iter().enumerate() {
        db.insert_dashboard_tile(
            &format!("t{i}"),
            &b,
            kind,
            Some("ref"),
            None,
            // Spans outside 3..=12 must clamp, not error — the FE can't be trusted to bound this.
            if i == 0 { 99 } else { 1 },
            None,
            "2026-08-03T10:00:00Z",
        )
        .unwrap();
    }
    let tiles = db.list_dashboard_tiles(&b).unwrap();
    assert_eq!(tiles.len(), 3);
    assert_eq!(
        tiles[0].span, 12,
        "over-wide span clamps to the 12-col grid"
    );
    assert_eq!(tiles[1].span, 3, "under-wide span clamps to the minimum");
    assert_eq!(
        tiles.iter().map(|t| t.position).collect::<Vec<_>>(),
        vec![0, 1, 2],
        "insertion appends densely"
    );

    // Reorder is a whole-list rewrite in one transaction.
    db.reorder_dashboard_tiles(&b, &["t2".into(), "t0".into(), "t1".into()])
        .unwrap();
    let order: Vec<String> = db
        .list_dashboard_tiles(&b)
        .unwrap()
        .into_iter()
        .map(|t| t.id)
        .collect();
    assert_eq!(order, vec!["t2", "t0", "t1"]);

    assert!(db.delete_dashboard_tile("t0").unwrap());
    assert_eq!(db.list_dashboard_tiles(&b).unwrap().len(), 2);
}

/// A reorder is a TOTAL rewrite, even when the caller's list is partial, duplicated or bogus.
/// Before this was enforced, an omitted id kept its old position and a duplicate collapsed two
/// tiles onto one — leaving duplicate positions, so the rendered order fell back to the secondary
/// sort key instead of what the user dragged.
#[test]
fn reorder_tolerates_partial_duplicate_and_unknown_ids() {
    let db = file_db("reorder-permutation");
    let b = board(&db, "reorder");
    for i in 0..4 {
        db.insert_dashboard_tile(
            &format!("t{i}"),
            &b,
            "note",
            Some("n"),
            None,
            4,
            None,
            "2026-08-03T10:00:00Z",
        )
        .unwrap();
    }

    // Ask for: a duplicate, an unknown id, and only two of the four real tiles.
    db.reorder_dashboard_tiles(
        &b,
        &[
            "t2".into(),
            "t2".into(),
            "does-not-exist".into(),
            "t0".into(),
        ],
    )
    .unwrap();

    let tiles = db.list_dashboard_tiles(&b).unwrap();
    let order: Vec<String> = tiles.iter().map(|t| t.id.clone()).collect();
    assert_eq!(
        order,
        vec!["t2", "t0", "t1", "t3"],
        "requested ids first (deduped), then the untouched rest in their prior order"
    );
    let positions: Vec<i64> = tiles.iter().map(|t| t.position).collect();
    assert_eq!(
        positions,
        vec![0, 1, 2, 3],
        "positions stay dense and unique"
    );
}

/// `None` = leave alone, `Some("")` = clear, `Some(v)` = set. Plain `COALESCE` could only ever
/// set, so "remove the emoji" was unexpressible.
#[test]
fn partial_updates_can_clear_a_nullable_column() {
    let db = file_db("clear-nullable");
    let id = board(&db, "clear");
    assert_eq!(
        db.get_dashboard(&id).unwrap().unwrap().emoji.as_deref(),
        Some("🚀")
    );

    // None leaves it.
    db.update_dashboard(&id, None, None, None, None, "2026-08-03T11:00:00Z")
        .unwrap();
    assert_eq!(
        db.get_dashboard(&id).unwrap().unwrap().emoji.as_deref(),
        Some("🚀")
    );

    // Empty string CLEARS it.
    db.update_dashboard(&id, None, Some(""), Some(""), None, "2026-08-03T12:00:00Z")
        .unwrap();
    let d = db.get_dashboard(&id).unwrap().unwrap();
    assert_eq!(d.emoji, None);
    assert_eq!(d.tint, None);

    // And a tile's title/config clear the same way.
    db.insert_dashboard_tile(
        "t1",
        &id,
        "note",
        Some("n1"),
        Some("a title"),
        4,
        Some("{}"),
        "2026-08-03T10:00:00Z",
    )
    .unwrap();
    db.update_dashboard_tile("t1", Some(""), None, Some(""))
        .unwrap();
    let t = db.get_dashboard_tile("t1").unwrap().unwrap();
    assert_eq!(t.title, None);
    assert_eq!(t.config, None);
}

/// note/document share the `documents` table, so the probe must match the row's KIND too.
#[test]
fn ref_exists_probe_distinguishes_note_from_document() {
    let db = file_db("ref-kind");
    seed_folder(&db, "f-open", "Open", false);
    db.insert_note("n-1", "f-open", "n-1.md", "A note", "body", 1)
        .unwrap();
    assert!(db.dashboard_ref_exists("note", "n-1").unwrap());
    assert!(
        !db.dashboard_ref_exists("document", "n-1").unwrap(),
        "a note row must not answer YES to a document probe"
    );
}

#[test]
fn unknown_tile_kind_is_refused() {
    let db = file_db("kind-allowlist");
    let b = board(&db, "k");
    let err = db
        .insert_dashboard_tile(
            "x",
            &b,
            "rm -rf",
            None,
            None,
            4,
            None,
            "2026-08-03T10:00:00Z",
        )
        .unwrap_err();
    assert!(
        matches!(err, crate::error::AppError::InvalidArg(_)),
        "an unknown kind is an InvalidArg, not a stored row: {err:?}"
    );
    assert!(db.list_dashboard_tiles(&b).unwrap().is_empty());
}

#[test]
fn deleting_a_board_removes_its_tiles() {
    let db = file_db("cascade");
    let b = board(&db, "cascade");
    db.insert_dashboard_tile(
        "t1",
        &b,
        "note",
        Some("n1"),
        None,
        4,
        None,
        "2026-08-03T10:00:00Z",
    )
    .unwrap();
    db.delete_dashboard(&b).unwrap();
    assert!(
        db.list_dashboard_tiles(&b).unwrap().is_empty(),
        "tiles must not outlive their board"
    );
    assert!(db.get_dashboard_tile("t1").unwrap().is_none());
}

fn seed_folder(db: &Db, id: &str, name: &str, locked: bool) {
    db.insert_folder(&Folder {
        id: id.to_string(),
        name: name.to_string(),
        path: name.to_string(),
        parent_id: None,
        locked,
        created_at: "2026-08-01T00:00:00Z".to_string(),
    })
    .unwrap();
}

/// Seed a meeting whose note lives in `folder`, so `visibility_clause` has something to gate on,
/// and mention `entity_id` in it.
fn seed_mentioned_meeting(db: &Db, meeting_id: &str, folder_id: &str, entity_id: &str, at: &str) {
    db.insert_meeting(&Meeting {
        id: meeting_id.to_string(),
        started_at: at.to_string(),
        ended_at: None,
        title: Some("standup".to_string()),
        duration_s: 600,
        audio_path: None,
        status: MeetingStatus::Summarized,
        folder_id: Some(folder_id.to_string()),
    })
    .unwrap();
    db.upsert_note(&NoteRecord {
        meeting_id: meeting_id.to_string(),
        provider_id: "test".to_string(),
        markdown: "# note".to_string(),
        created_at: at.to_string(),
        exported_path: None,
        model_requested: None,
        model_served: None,
        gateway_host: None,
    })
    .unwrap();
    db.set_note_folder(meeting_id, Some(folder_id)).unwrap();
    db.add_mention(entity_id, meeting_id).unwrap();
}

/// THE LEAK ORACLE for the Pulse tile: a mention that exists only inside a SEALED, not-session-
/// unlocked folder must contribute nothing — not even a count. Counting it would betray hidden
/// activity ("this went quiet" vs "this is busy behind a lock") without ever showing the content.
#[test]
fn pulse_excludes_sealed_meetings_until_unlocked() {
    let db = file_db("pulse-gate");
    seed_folder(&db, "f-open", "Open", false);
    seed_folder(&db, "f-sealed", "Sealed", true);
    let atlas = db
        .upsert_entity("Project Atlas", EntityKind::Project)
        .unwrap();

    seed_mentioned_meeting(&db, "m-open", "f-open", &atlas, "2026-08-01T09:00:00Z");
    seed_mentioned_meeting(&db, "m-sealed", "f-sealed", &atlas, "2026-08-02T09:00:00Z");

    // Nothing unlocked: only the open meeting is counted.
    let none = std::collections::HashSet::new();
    let pulse = db.entity_mention_pulse_visible(&atlas, 100, &none).unwrap();
    assert_eq!(
        pulse.len(),
        1,
        "a sealed meeting must not contribute to the pulse: {pulse:?}"
    );
    assert!(pulse[0].starts_with("2026-08-01"));

    // Session-unlock the folder: the same reader now includes it (reversible, not a hole).
    let unlocked: std::collections::HashSet<String> =
        ["f-sealed".to_string()].into_iter().collect();
    let pulse = db
        .entity_mention_pulse_visible(&atlas, 100, &unlocked)
        .unwrap();
    assert_eq!(pulse.len(), 2, "unlocking restores the hidden mention");
}

/// The existence probe answers "is there a row" and nothing else — and says NO for an id that
/// was never stored, so a tile pointing at a deleted source renders "missing", not "locked".
#[test]
fn ref_exists_probe_is_existence_only() {
    let db = file_db("ref-exists");
    let id = db.upsert_entity("Atlas", EntityKind::Project).unwrap();
    assert!(db.dashboard_ref_exists("person", &id).unwrap());
    assert!(!db.dashboard_ref_exists("person", "e-nope").unwrap());
    assert!(!db.dashboard_ref_exists("meeting", &id).unwrap());
    assert!(
        !db.dashboard_ref_exists("living_answer", &id).unwrap(),
        "a kind with no anchor table probes to false rather than erroring"
    );
}

#[test]
fn tile_mutation_metadata_never_hydrates_residual_title_or_config() {
    let db = file_db("tile-mutation-metadata");
    let dashboard_id = board(&db, "metadata");
    db.insert_dashboard_tile(
        "t-secret",
        &dashboard_id,
        "living_answer",
        None,
        Some("RESIDUAL TITLE SENTINEL"),
        4,
        Some(r#"{"question":"RESIDUAL CONFIG SENTINEL"}"#),
        "2026-08-03T10:00:00Z",
    )
    .unwrap();
    db.lock()
        .execute(
            "UPDATE dashboard_tiles SET title=x'00', config=x'01' WHERE id='t-secret'",
            [],
        )
        .unwrap();

    assert_eq!(
        db.dashboard_tile_metadata("t-secret").unwrap(),
        Some((dashboard_id.clone(), "living_answer".to_string()))
    );
    let before = db.dashboard_context_state(&dashboard_id).unwrap();
    db.reorder_dashboard_tiles(&dashboard_id, &["t-secret".to_string()])
        .unwrap();
    assert!(db.dashboard_context_state(&dashboard_id).unwrap().0 > before.0);
}

#[test]
fn living_answer_birth_commits_question_provenance_and_generation_atomically() {
    let db = file_db("living-answer-birth-atomic");
    let dashboard_id = board(&db, "living");
    let before = db.dashboard_context_state(&dashboard_id).unwrap();

    db.insert_dashboard_living_answer_tile(
        "t-living",
        &dashboard_id,
        4,
        "What changed?",
        r#"["f-open"]"#,
        "2026-08-03T10:00:00Z",
    )
    .unwrap();

    assert_eq!(
        db.dashboard_living_question_after_preflight("t-living")
            .unwrap()
            .as_deref(),
        Some("What changed?")
    );
    assert_eq!(
        db.dashboard_living_answer_preflight("t-living")
            .unwrap()
            .unwrap()
            .question_readable_folders_json
            .as_deref(),
        Some(r#"["f-open"]"#)
    );
    assert!(db.dashboard_context_state(&dashboard_id).unwrap().0 > before.0);

    db.lock()
        .execute_batch(
            "CREATE TRIGGER fail_living_birth BEFORE INSERT ON dashboard_tiles
             WHEN NEW.id='t-fail' BEGIN SELECT RAISE(ABORT, 'injected failure'); END;",
        )
        .unwrap();
    let generation_before_failure = db.dashboard_context_state(&dashboard_id).unwrap();
    assert!(db
        .insert_dashboard_living_answer_tile(
            "t-fail",
            &dashboard_id,
            4,
            "Must not persist",
            r#"["f-open"]"#,
            "2026-08-03T10:01:00Z",
        )
        .is_err());
    assert!(db.dashboard_tile_metadata("t-fail").unwrap().is_none());
    assert_eq!(
        db.dashboard_context_state(&dashboard_id).unwrap(),
        generation_before_failure,
        "a failed birth must leave neither an orphan tile nor a witness bump"
    );
}

#[test]
fn living_answer_cache_write_is_atomic_and_generation_bound() {
    let db = file_db("living-answer-cache-cas");
    let dashboard_id = board(&db, "living-cache");
    db.insert_dashboard_living_answer_tile(
        "t-living",
        &dashboard_id,
        4,
        "What changed?",
        r#"["f-open"]"#,
        "2026-08-03T10:00:00Z",
    )
    .unwrap();
    let expected_generation = db
        .dashboard_structural_context_state(&dashboard_id)
        .unwrap()
        .0;
    let general_before = db.dashboard_context_state(&dashboard_id).unwrap().0;

    assert!(db
        .store_dashboard_living_answer_cas(
            "t-living",
            &dashboard_id,
            "What changed?",
            "The launch moved to Friday.",
            "2026-08-03T10:01:00Z",
            r#"["f-open"]"#,
            expected_generation,
            "packed-context-digest",
            200_000,
        )
        .unwrap());
    let committed_generation = db
        .dashboard_structural_context_state(&dashboard_id)
        .unwrap()
        .0;
    assert_eq!(committed_generation, expected_generation);
    assert_eq!(
        db.dashboard_context_state(&dashboard_id).unwrap().0,
        general_before + 1,
        "cache writes invalidate general board/history witnesses only"
    );
    assert_eq!(
        db.dashboard_living_answer_content_after_preflight("t-living")
            .unwrap(),
        Some(crate::storage::dashboards_store::LivingAnswerContent {
            question: "What changed?".to_string(),
            answer: Some("The launch moved to Friday.".to_string()),
            answered_at: Some("2026-08-03T10:01:00Z".to_string()),
        })
    );
    assert_eq!(
        db.dashboard_living_answer_preflight("t-living")
            .unwrap()
            .unwrap()
            .answer,
        crate::storage::dashboards_store::LivingAnswerCacheState::Valid {
            readable_folders_json: r#"["f-open"]"#.to_string(),
            context_generation: committed_generation,
            context_digest: "packed-context-digest".to_string(),
            context_budget: 200_000,
            ask_dispatch_generation: db.ask_dispatch_generation().unwrap(),
        }
    );

    db.insert_dashboard_tile(
        "t-mutation",
        &dashboard_id,
        "promises",
        None,
        None,
        4,
        None,
        "2026-08-03T10:02:00Z",
    )
    .unwrap();
    assert!(!db
        .store_dashboard_living_answer_cas(
            "t-living",
            &dashboard_id,
            "What changed?",
            "STALE OVERWRITE",
            "2026-08-03T10:03:00Z",
            r#"["f-open"]"#,
            committed_generation,
            "stale-digest",
            200_000,
        )
        .unwrap());
    assert!(
        db.dashboard_living_answer_content_after_preflight("t-living")
            .unwrap()
            .is_none(),
        "a dashboard mutation must withhold the old cache before content hydration"
    );
}

/// `migrate()` runs on EVERY open; the new tables must not break that (and must survive it).
#[test]
fn migrate_is_idempotent_with_dashboards() {
    let path = super::unique_temp_path("meetnotes-dash-idempotent", "sqlite");
    let db = Db::open_with_key(&path, TEST_DEK).unwrap();
    let b = board(&db, "keep");
    db.insert_dashboard_tile(
        "t1",
        &b,
        "note",
        Some("n1"),
        None,
        4,
        None,
        "2026-08-03T10:00:00Z",
    )
    .unwrap();
    let state_before_reopen = db.dashboard_context_state(&b).unwrap();
    let structural_before_reopen = db.dashboard_structural_context_state(&b).unwrap();
    assert!(state_before_reopen.1);
    drop(db);

    // Re-open (migrate runs again) — the board and its tile survive untouched.
    let db = Db::open_with_key(&path, TEST_DEK).unwrap();
    db.migrate().unwrap();
    assert_eq!(db.list_dashboards().unwrap().len(), 1);
    assert_eq!(db.list_dashboard_tiles(&b).unwrap().len(), 1);
    let conn = db.lock();
    let columns = conn
        .prepare("PRAGMA table_info(ask_conversations)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    for column in [
        "dashboard_id",
        "dashboard_context_generation",
        "dashboard_context_digest",
        "ask_dispatch_generation",
    ] {
        assert!(
            columns.iter().any(|found| found == column),
            "missing {column}"
        );
    }
    let state: (i64, i64) = conn
        .query_row(
            "SELECT generation, exists_now FROM dashboard_context_state WHERE dashboard_id=?1",
            [&b],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        state,
        (state_before_reopen.0, i64::from(state_before_reopen.1)),
        "reopening and rerunning migration must preserve the durable witness"
    );
    let structural: i64 = conn
        .query_row(
            "SELECT structural_generation FROM dashboard_context_state WHERE dashboard_id=?1",
            [&b],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(structural, structural_before_reopen.0);
}

#[test]
fn pre_composite_schema_upgrades_additively_and_preserves_legacy_history() {
    let db = file_db("pre-composite-upgrade");
    let board_id = board(&db, "legacy-board");
    let scope = AskConversationScope::Vault;
    let conversation_id = db
        .persist_ask_exchange(
            &scope,
            None,
            "legacy question",
            "legacy answer",
            &[],
            &[],
            &[],
            &[],
            "2026-08-03T10:00:00Z",
        )
        .unwrap();

    // Construct the exact pre-feature shape while retaining the rest of the real encrypted
    // schema. Destructive DDL is test-fixture setup only; production migration remains additive.
    db.lock()
        .execute_batch(
            "DROP TABLE ask_dispatch_state;
             DROP TABLE dashboard_context_state;
             ALTER TABLE ask_conversations DROP COLUMN dashboard_context_digest;
             ALTER TABLE ask_conversations DROP COLUMN dashboard_context_generation;
             ALTER TABLE ask_conversations DROP COLUMN dashboard_id;
             ALTER TABLE ask_conversations DROP COLUMN ask_dispatch_generation;
             ALTER TABLE dashboard_tiles DROP COLUMN question_readable_folders_json;
             ALTER TABLE dashboard_tiles DROP COLUMN answer_readable_folders_json;
             ALTER TABLE dashboard_tiles DROP COLUMN living_question;
             ALTER TABLE dashboard_tiles DROP COLUMN living_answer;
             ALTER TABLE dashboard_tiles DROP COLUMN living_answered_at;
             ALTER TABLE dashboard_tiles DROP COLUMN living_answer_context_generation;
             ALTER TABLE dashboard_tiles DROP COLUMN living_answer_context_digest;
             ALTER TABLE dashboard_tiles DROP COLUMN living_answer_context_budget;
             ALTER TABLE dashboard_tiles DROP COLUMN living_answer_ask_dispatch_generation;",
        )
        .unwrap();

    db.migrate().unwrap();
    db.migrate().unwrap();
    let state: (i64, i64) = db
        .lock()
        .query_row(
            "SELECT generation, exists_now FROM dashboard_context_state WHERE dashboard_id=?1",
            [&board_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(state, (0, 1));
    assert_eq!(
        db.dashboard_structural_context_state(&board_id).unwrap(),
        (0, true)
    );
    let tile_columns = db
        .lock()
        .prepare("PRAGMA table_info(dashboard_tiles)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    for column in [
        "question_readable_folders_json",
        "answer_readable_folders_json",
        "living_question",
        "living_answer",
        "living_answered_at",
        "living_answer_context_generation",
        "living_answer_context_digest",
        "living_answer_context_budget",
        "living_answer_ask_dispatch_generation",
    ] {
        assert!(tile_columns.iter().any(|found| found == column));
    }
    let legacy = db
        .load_ask_conversation(&scope, &conversation_id, &std::collections::HashSet::new())
        .unwrap();
    assert!(legacy.is_none(), "legacy unstamped history must fail closed");
}
