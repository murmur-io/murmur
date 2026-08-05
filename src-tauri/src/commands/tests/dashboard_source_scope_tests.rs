//! THE SCOPE ORACLE — `dashboard_sources_inner`, the function that DEFINES what a
//! board-scoped Ask is allowed to read.
//!
//! Before 2026-08-04 this function had **no Rust test at all**. Its only coverage
//! was Playwright specs feeding a mocked `get_dashboard_sources`, which assert the
//! frontend's assumption about the answer rather than the answer itself — so every
//! property below was, in practice, unverified on the surface that enforces it.
//!
//! Four properties, each with a CONTROL so it cannot go vacuous: a passing test
//! that would also pass with the gate deleted proves nothing, and that exact
//! failure (an unfalsifiable answer-cache gate) is what the adversarial review of
//! PR #562 caught in this very feature.

use super::*;
use crate::storage::models::{Folder, Meeting, MeetingStatus, NoteRecord};
use std::collections::HashSet;

const TEST_DEK: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"; // MURMUR_DEV_DEK placeholder

fn file_db(label: &str) -> crate::storage::Db {
    crate::storage::Db::open_with_key(
        &crate::storage::db::unique_temp_path(&format!("meetnotes-dash-scope-{label}"), "sqlite"),
        TEST_DEK,
    )
    .unwrap()
}

fn seed_folder(db: &crate::storage::Db, id: &str, locked: bool) {
    db.insert_folder(&Folder {
        id: id.to_string(),
        name: id.to_string(),
        path: id.to_string(),
        parent_id: None,
        locked,
        created_at: "2026-08-01T00:00:00Z".to_string(),
    })
    .unwrap();
}

/// A meeting plus its note, filed into `folder_id` so `visibility_clause` has
/// something to gate on.
fn seed_meeting(db: &crate::storage::Db, meeting_id: &str, folder_id: &str) {
    db.insert_meeting(&Meeting {
        id: meeting_id.to_string(),
        started_at: "2026-08-01T09:00:00Z".to_string(),
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
        created_at: "2026-08-01T09:00:00Z".to_string(),
        exported_path: None,
        model_requested: None,
        model_served: None,
        gateway_host: None,
    })
    .unwrap();
    db.set_note_folder(meeting_id, Some(folder_id)).unwrap();
}

/// A standalone NOTE document — `documents` with `kind='note'`, which is what a
/// `LinkKind::Note` source actually points at (NOT the per-meeting `notes` row).
fn seed_note(db: &crate::storage::Db, note_id: &str, folder_id: &str) {
    db.insert_note(note_id, folder_id, note_id, note_id, "body", 1_780_000_000)
        .unwrap();
}

fn tile(id: &str, kind: &str, ref_id: Option<&str>, position: i64) -> DashboardTile {
    DashboardTile {
        id: id.to_string(),
        dashboard_id: "b1".to_string(),
        kind: kind.to_string(),
        ref_id: ref_id.map(|s| s.to_string()),
        title: None,
        span: 4,
        position,
        config: None,
        created_at: "2026-08-01T09:00:00Z".to_string(),
    }
}

fn nothing_unlocked() -> HashSet<String> {
    HashSet::new()
}

/// A source inside a SEALED, not-session-unlocked folder must not enter the scope.
///
/// This is the load-bearing one: `explicit_sources` is handed straight to
/// `ask_vault`, which packs each source's text into the prompt. A sealed source
/// surviving this filter would put locked content into a model call.
#[test]
fn a_sealed_source_never_enters_the_ask_scope() {
    let db = file_db("sealed");
    seed_folder(&db, "f-open", false);
    seed_folder(&db, "f-sealed", true);
    seed_meeting(&db, "m-open", "f-open");
    seed_meeting(&db, "m-sealed", "f-sealed");

    let tiles = vec![
        tile("t1", "meeting", Some("m-open"), 0),
        tile("t2", "meeting", Some("m-sealed"), 1),
    ];

    let out = dashboard_sources_inner(&db, tiles.clone(), &nothing_unlocked()).unwrap();
    assert_eq!(
        out.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
        vec!["m-open"],
        "a sealed meeting must not contribute a source"
    );

    // CONTROL — the same call with the folder SESSION-UNLOCKED returns it, so the
    // assertion above is testing the gate and not merely a seeding mistake.
    let unlocked: HashSet<String> = ["f-sealed".to_string()].into_iter().collect();
    let out = dashboard_sources_inner(&db, tiles, &unlocked).unwrap();
    assert_eq!(out.len(), 2, "session-unlocking the folder restores the source");
}

/// A DERIVED tile is a view over the vault, not a retrievable document, so it
/// contributes no `SourceRef` of its own.
///
/// This is deliberate, not an oversight, and the test exists so nobody "fixes" it
/// by widening the match: `SourceRef.kind` is a `LinkKind`, and there is no
/// `LinkKind` a drift lane or a promise ledger could honestly claim. What those
/// tiles SHOW still has to reach the model — but as rendered text, never as a
/// retrieval source.
#[test]
fn derived_tiles_contribute_no_source() {
    let db = file_db("derived");
    seed_folder(&db, "f-open", false);
    seed_meeting(&db, "m-open", "f-open");

    let tiles = vec![
        tile("t1", "drift", Some("e-atlas"), 0),
        tile("t2", "numbers", Some("e-atlas"), 1),
        tile("t3", "pulse", Some("e-atlas"), 2),
        tile("t4", "person", Some("e-kuba"), 3),
        tile("t5", "promises", None, 4),
        tile("t6", "reminders", None, 5),
        tile("t7", "living_answer", None, 6),
    ];
    assert!(
        dashboard_sources_inner(&db, tiles, &nothing_unlocked())
            .unwrap()
            .is_empty(),
        "no derived tile may contribute a retrieval source"
    );

    // CONTROL — a MATERIAL tile on the same board does contribute, so the empty
    // result above is the kind filter and not a broken fixture.
    let material = vec![tile("t8", "meeting", Some("m-open"), 7)];
    assert_eq!(
        dashboard_sources_inner(&db, material, &nothing_unlocked())
            .unwrap()
            .len(),
        1
    );
}

/// A tile pointing at a source that no longer exists contributes nothing, and —
/// crucially — does not fail the call.
///
/// `resolve_tile` distinguishes deleted from sealed so the TILE can render
/// "missing"; the scope has no such need and must simply drop it. An `Err` here
/// would take the whole board's Ask down with one stale tile.
#[test]
fn a_deleted_source_is_dropped_rather_than_erroring() {
    let db = file_db("deleted");
    seed_folder(&db, "f-open", false);
    seed_meeting(&db, "m-open", "f-open");

    let tiles = vec![
        tile("t1", "meeting", Some("m-open"), 0),
        tile("t2", "note", Some("n-never-existed"), 1),
        tile("t3", "document", Some("d-never-existed"), 2),
        // A material kind with no anchor at all — the `Unconfigured` shape.
        tile("t4", "note", None, 3),
    ];

    let out = dashboard_sources_inner(&db, tiles, &nothing_unlocked())
        .expect("a dangling ref must not fail the whole scope");
    assert_eq!(
        out.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
        vec!["m-open"],
        "dangling note/document refs are dropped, and none of them errors"
    );
}

/// FINDING, recorded rather than silently encoded as intent (2026-08-04).
///
/// A `meeting` ref that resolves to NOTHING still enters the scope, because
/// `meetings_store::Db::meeting_is_visible` ends in `Ok(!has_notes || has_visible)`
/// — a meeting with no note row is visible BY CONSTRUCTION, and a meeting that
/// does not exist trivially has no note row.
///
/// For a deleted meeting this is only wasteful: `fair_pack_explicit_sections`
/// divides the prompt budget by the number of sources, so a stale tile silently
/// shrinks every other source's share while contributing nothing.
///
/// The case worth a specialist's eye is the OTHER one this predicate admits: a
/// meeting that exists, sits in a SEALED folder, and has no note row yet — the
/// shape `lock-model.md` already calls out for audio ("recorded into an
/// already-sealed folder, or a crash window"). This test does not assert that is
/// safe; it pins the CURRENT behaviour so the change is visible if someone
/// tightens the predicate, and flags it for `lock-security-reviewer`. Do not
/// "fix" it inside a frontend density pass — `storage/**` is a lock-risk path.
#[test]
fn a_dangling_meeting_ref_currently_survives_the_gate() {
    let db = file_db("dangling-meeting");
    let tiles = vec![tile("t1", "meeting", Some("m-never-existed"), 0)];
    let out = dashboard_sources_inner(&db, tiles, &nothing_unlocked()).unwrap();
    assert_eq!(
        out.len(),
        1,
        "documents the fail-OPEN branch of meeting_is_visible; see the doc comment"
    );
}

/// Two tiles pointing at the same source contribute ONE entry.
///
/// Without this, a board with a duplicate tile pays for that source twice inside
/// `fair_pack_explicit_sections`, whose budget is `total / n` — so duplicating a
/// tile would silently shrink every OTHER source's share of the prompt.
#[test]
fn duplicate_tiles_dedupe_by_kind_and_id() {
    let db = file_db("dedupe");
    seed_folder(&db, "f-open", false);
    seed_meeting(&db, "m-open", "f-open");
    seed_meeting(&db, "m-other", "f-open");

    let tiles = vec![
        tile("t1", "meeting", Some("m-open"), 0),
        tile("t2", "meeting", Some("m-open"), 1),
        tile("t3", "meeting", Some("m-other"), 2),
    ];

    let out = dashboard_sources_inner(&db, tiles, &nothing_unlocked()).unwrap();
    assert_eq!(out.len(), 2, "the repeated source appears once");
    assert_eq!(out[0].id, "m-open", "first occurrence keeps its position");
    assert_eq!(out[1].id, "m-other");

    // CONTROL — dedupe is on (kind, id), not on id alone. A note and a meeting
    // that happen to share an id are two different sources and both survive.
    let db2 = file_db("dedupe-kind");
    seed_folder(&db2, "f-open", false);
    seed_meeting(&db2, "same-id", "f-open");
    seed_note(&db2, "same-id", "f-open");
    let mixed = vec![
        tile("t1", "meeting", Some("same-id"), 0),
        tile("t2", "note", Some("same-id"), 1),
    ];
    let out = dashboard_sources_inner(&db2, mixed, &nothing_unlocked()).unwrap();
    assert_eq!(
        out.len(),
        2,
        "a note and a meeting sharing an id are distinct sources"
    );
}
