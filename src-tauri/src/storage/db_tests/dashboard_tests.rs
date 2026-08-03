//! File-backed tests for the DASHBOARD store (2026-08-03) — board/tile CRUD, ordering, the
//! kind allowlist, span clamping, and the two lock-model-relevant properties:
//!
//! 1. `entity_mention_pulse_visible` is GATED — a mention that exists only inside a sealed,
//!    not-session-unlocked meeting contributes NOTHING to the pulse.
//! 2. `migrate()` stays idempotent with the new tables (it runs on every open).
//!
//! These use `open_with_key` + a fixed literal DEK, so they never touch the Keychain.

use super::*;
use crate::storage::models::{EntityKind, Folder, Meeting, MeetingStatus, NoteRecord};

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

fn board(db: &Db, title: &str) -> String {
    let id = format!("board-{title}");
    db.insert_dashboard(&id, title, Some("🚀"), Some("indigo"), "2026-08-03T10:00:00Z")
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
        .update_dashboard(&id, Some("Atlas GA"), None, None, Some(true), "2026-08-03T11:00:00Z")
        .unwrap());
    let d = db.get_dashboard(&id).unwrap().unwrap();
    assert_eq!(d.title, "Atlas GA");
    assert_eq!(d.emoji.as_deref(), Some("🚀"), "untouched field survives a patch");
    assert!(d.pinned);
    assert_eq!(d.updated_at, "2026-08-03T11:00:00Z");

    assert!(db.delete_dashboard(&id).unwrap());
    assert!(db.get_dashboard(&id).unwrap().is_none());
    assert!(!db.delete_dashboard(&id).unwrap(), "second delete is a no-op");
}

#[test]
fn pinned_boards_sort_first() {
    let db = file_db("pinned-sort");
    let a = board(&db, "a");
    let b = board(&db, "b");
    db.update_dashboard(&b, None, None, None, Some(true), "2026-08-03T12:00:00Z")
        .unwrap();
    let ids: Vec<String> = db.list_dashboards().unwrap().into_iter().map(|d| d.id).collect();
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
    assert_eq!(tiles[0].span, 12, "over-wide span clamps to the 12-col grid");
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

#[test]
fn unknown_tile_kind_is_refused() {
    let db = file_db("kind-allowlist");
    let b = board(&db, "k");
    let err = db
        .insert_dashboard_tile("x", &b, "rm -rf", None, None, 4, None, "2026-08-03T10:00:00Z")
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
    db.insert_dashboard_tile("t1", &b, "note", Some("n1"), None, 4, None, "2026-08-03T10:00:00Z")
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
    let atlas = db.upsert_entity("Project Atlas", EntityKind::Project).unwrap();

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
    let unlocked: std::collections::HashSet<String> = ["f-sealed".to_string()].into_iter().collect();
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

/// `migrate()` runs on EVERY open; the new tables must not break that (and must survive it).
#[test]
fn migrate_is_idempotent_with_dashboards() {
    let path = super::unique_temp_path("meetnotes-dash-idempotent", "sqlite");
    let db = Db::open_with_key(&path, TEST_DEK).unwrap();
    let b = board(&db, "keep");
    db.insert_dashboard_tile("t1", &b, "note", Some("n1"), None, 4, None, "2026-08-03T10:00:00Z")
        .unwrap();
    drop(db);

    // Re-open (migrate runs again) — the board and its tile survive untouched.
    let db = Db::open_with_key(&path, TEST_DEK).unwrap();
    db.migrate().unwrap();
    assert_eq!(db.list_dashboards().unwrap().len(), 1);
    assert_eq!(db.list_dashboard_tiles(&b).unwrap().len(), 1);
}
