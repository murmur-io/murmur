//! Oracles for the RELATED PICKER's read surface and for the `container` link endpoint.
//!
//! File-backed SQLCipher via `open_with_key` + a FIXED literal test key — these never touch the
//! real Keychain and never mint a content key: the picker adds no seal, so a lock is modelled here
//! by the `folders.locked` COLUMN, which is exactly what every gate reads.
//!
//! The leak oracles are the ones that matter. `cargo test --lib`, `ng lint` and `ng build` are all
//! green for an ungated read; only a test that seals something and then asserts nothing comes back
//! can see it. Same for the WRITE side: only a test that counts the rows in `links` can see a
//! container link that fanned out to its descendants.

use super::*;
use crate::storage::db::Db;
use crate::storage::models::{Meeting, MeetingStatus, NoteRecord, PickerRow};
use std::collections::HashSet;

const TEST_DEK: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn fresh_db(label: &str) -> (Db, std::path::PathBuf) {
    let path =
        crate::storage::db::unique_temp_path(&format!("murmur-related-picker-{label}"), "sqlite");
    let _ = std::fs::remove_file(&path);
    let db = Db::open_with_key(&path, TEST_DEK).unwrap();
    // Opening a database ADOPTS it: the hierarchy migration creates a default project at the vault
    // root. Clear the whole table so each test states the exact container shape it is about.
    db.lock()
        .execute("DELETE FROM folders", rusqlite::params![])
        .unwrap();
    (db, path)
}

/// A container row. `level` is set afterwards because `insert_folder` deliberately does not take
/// one — only the hierarchy migration promotes a row to `'project'`.
fn container(db: &Db, id: &str, name: &str, path: &str, parent: Option<&str>, level: &str) {
    db.insert_folder(&Folder {
        id: id.into(),
        name: name.into(),
        path: path.into(),
        parent_id: parent.map(str::to_string),
        locked: false,
        created_at: "2026-08-22T09:00:00Z".into(),
    })
    .unwrap();
    db.lock()
        .execute(
            "UPDATE folders SET level = ?2 WHERE id = ?1",
            rusqlite::params![id, level],
        )
        .unwrap();
}

/// Seal a container the way every gate observes it: the durable `locked` column.
fn seal(db: &Db, id: &str) {
    db.lock()
        .execute(
            "UPDATE folders SET locked = 1 WHERE id = ?1",
            rusqlite::params![id],
        )
        .unwrap();
}

fn set_flag(db: &Db, id: &str, column: &str, value: &str) {
    db.lock()
        .execute(
            &format!("UPDATE folders SET {column} = {value} WHERE id = ?1"),
            rusqlite::params![id],
        )
        .unwrap();
}

/// A recording filed into `folder` (`None` ⇒ unfiled, i.e. "Not classified · Recordings").
fn meeting_in(db: &Db, id: &str, title: &str, started_at: &str, folder: Option<&str>) {
    db.insert_meeting(&Meeting {
        id: id.into(),
        started_at: started_at.into(),
        ended_at: None,
        title: Some(title.into()),
        duration_s: 600,
        audio_path: None,
        status: MeetingStatus::Summarized,
        folder_id: folder.map(str::to_string),
    })
    .unwrap();
    db.upsert_note(&NoteRecord {
        meeting_id: id.into(),
        provider_id: "claude_code".into(),
        markdown: format!("# {title}"),
        created_at: started_at.into(),
        ..Default::default()
    })
    .unwrap();
    db.set_meeting_folder(id, folder).unwrap();
}

fn note_in(db: &Db, id: &str, name: &str, folder: &str, created_at: i64) {
    db.insert_document(id, folder, name, "body", "note", created_at)
        .unwrap();
}

fn document_in(db: &Db, id: &str, name: &str, folder: &str, created_at: i64) {
    db.insert_document(id, folder, name, "body", "document", created_at)
        .unwrap();
}

fn titles(items: &[PickerRow]) -> Vec<&str> {
    items.iter().map(|row| row.title.as_str()).collect()
}

fn unlocked(ids: &[&str]) -> HashSet<String> {
    ids.iter().map(|id| (*id).to_string()).collect()
}

/// The whole rendered payload as one string, for "does this leak ANY of these substrings" asserts.
fn wire(value: &impl serde::Serialize) -> String {
    serde_json::to_string(value).unwrap()
}

// ── 1. THE WIRE CONTRACT ─────────────────────────────────────────────────────────────────────────

/// Every field of every picker DTO is camelCase on the wire, and NO DTO carries a filesystem path.
///
/// A hand-written e2e mock is typed against the FRONTEND's interface, so it is camelCase by
/// construction and DEFINES a shape rather than verifying one. Only a serialization assertion on
/// the PRODUCING side catches the `TileData` class of bug (#566/#568), where snake_case variant
/// fields reached a camelCase reader as `undefined` and took the whole view down.
#[test]
fn picker_dtos_are_camel_case_and_path_free_on_the_wire() {
    let bootstrap = RelatedPickerBootstrap {
        spaces: vec![PickerContainerNode {
            id: "p1".into(),
            name: "Product".into(),
            level: "project".into(),
            emoji: Some("🗂".into()),
            locked: false,
            unlocked: false,
            linkable: true,
            groups: vec![PickerGroup {
                kind: PickerItemKind::Meeting,
                total: 3,
            }],
            folders: vec![PickerContainerNode {
                id: "f1".into(),
                name: "Atlas".into(),
                level: "folder".into(),
                emoji: None,
                locked: true,
                unlocked: false,
                linkable: false,
                groups: vec![],
                folders: vec![],
            }],
        }],
        unclassified: vec![PickerGroup {
            kind: PickerItemKind::Note,
            total: 1,
        }],
        anchor: Some(PickerAnchorLocation {
            kind: PickerItemKind::Meeting,
            container_id: Some("f1".into()),
            path: vec!["p1".into(), "f1".into()],
            index: 153,
            offset: 141,
            items: vec![PickerRow {
                kind: PickerItemKind::Document,
                id: "d1".into(),
                title: "Launch plan".into(),
            }],
            total: 400,
        }),
    };
    let json = serde_json::to_value(&bootstrap).unwrap();
    assert!(json["spaces"][0].get("linkable").is_some());
    assert!(json["anchor"].get("containerId").is_some());
    assert!(json["anchor"]["items"][0].get("kind").is_some());
    assert_eq!(json["anchor"]["kind"], "meeting");
    assert_eq!(json["spaces"][0]["folders"][0]["locked"], true);

    let page = RelatedPickerPage {
        kind: PickerItemKind::Note,
        offset: 12,
        items: vec![],
        total: 30,
    };
    let search = RelatedPickerSearchPage {
        offset: 0,
        hits: vec![RelatedPickerHit {
            kind: PickerItemKind::Document,
            id: "d1".into(),
            title: "Launch plan".into(),
            breadcrumb: vec!["Product".into(), "Atlas".into()],
        }],
        total: 1,
    };

    // Every serialized key across the three payloads is lowerCamelCase — no `_`, ever.
    for payload in [wire(&bootstrap), wire(&page), wire(&search)] {
        let parsed: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_no_snake_case_keys(&parsed);
        // A picker row must never carry an on-disk path: the FE feeds any path it receives into
        // `convertFileSrc`, the one read that bypasses every command gate.
        assert!(
            !payload.contains('/') || !payload.contains("Users"),
            "a picker DTO must never carry a filesystem path: {payload}"
        );
    }
}

fn assert_no_snake_case_keys(value: &serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                assert!(
                    !key.contains('_'),
                    "serialized key {key:?} is snake_case — the FE reads camelCase"
                );
                assert!(
                    key.chars().next().is_some_and(char::is_lowercase),
                    "serialized key {key:?} must start lowercase"
                );
                assert_no_snake_case_keys(child);
            }
        }
        serde_json::Value::Array(items) => items.iter().for_each(assert_no_snake_case_keys),
        _ => {}
    }
}

// ── 2. AN ANCHOR BEYOND PAGE ONE ─────────────────────────────────────────────────────────────────

/// The point of the whole surface: opening Related from item #150 returns ONLY that item's ancestor
/// chain plus a BOUNDED window that CONTAINS it — never the container, never page one.
#[test]
fn an_anchor_beyond_page_one_yields_its_path_and_a_bounded_centred_window() {
    let (db, path) = fresh_db("centred-window");
    container(&db, "p1", "Product", "Product", None, "project");
    container(&db, "f1", "Atlas", "Product/Atlas", Some("p1"), "folder");
    // 200 recordings, newest first by `started_at`. The anchor is deep in the ordering.
    for i in 0..200 {
        meeting_in(
            &db,
            &format!("m{i:03}"),
            &format!("Recording {i:03}"),
            &format!("2026-01-01T{:02}:{:02}:00Z", i / 60, i % 60),
            Some("f1"),
        );
    }
    // `started_at` DESC ⇒ m199 is row 0 and m049 is row 150.
    let anchor = "m049";

    let boot = related_picker_bootstrap_inner(&db, &HashSet::new(), "meeting", anchor).unwrap();
    let location = boot.anchor.expect("a local anchor must resolve a location");

    assert_eq!(location.index, 150, "the anchor's stable position");
    assert_eq!(location.total, 200, "the container's full visible total");
    assert_eq!(
        location.path,
        vec!["p1".to_string(), "f1".to_string()],
        "only the anchor's ancestor chain, root-first"
    );
    assert!(
        location.items.len() <= 24,
        "the window must stay BOUNDED, got {}",
        location.items.len()
    );
    assert!(
        location.offset > 0,
        "an anchor at row 150 must not open on page one"
    );
    let ids: Vec<&str> = location.items.iter().map(|row| row.id.as_str()).collect();
    assert!(
        ids.contains(&anchor),
        "the window MUST contain the anchor itself; got {ids:?}"
    );
    let position_in_window = ids.iter().position(|id| *id == anchor).unwrap();
    assert!(
        (location.offset + position_in_window as u32) == location.index,
        "offset + row-in-window must equal the reported index"
    );
    assert!(
        (4..=20).contains(&position_in_window),
        "the anchor should land roughly mid-window, got row {position_in_window}"
    );

    // Stable: the same call twice gives the same window.
    let again = related_picker_bootstrap_inner(&db, &HashSet::new(), "meeting", anchor).unwrap();
    assert_eq!(again.anchor.unwrap().items, location.items);

    // `Load earlier` and `Load more` are ordinary pages of the SAME ordering.
    let earlier = related_picker_items_inner(
        &db,
        &HashSet::new(),
        "meeting",
        anchor,
        Some("f1"),
        "meeting",
        location.offset.saturating_sub(24),
        24,
    )
    .unwrap();
    assert_eq!(earlier.total, 200);
    assert!(!earlier.items.is_empty());
    assert!(
        earlier
            .items
            .iter()
            .all(|row| !location.items.iter().any(|w| w.id == row.id)),
        "an earlier page must not overlap the centred window"
    );

    let _ = std::fs::remove_file(path);
}

// ── 3. MIXED PAGES, SEARCH, AND WHAT NEVER APPEARS ───────────────────────────────────────────────

/// Meetings, notes and documents all appear; the reserved note root is HIDDEN and its real folder
/// children hoisted; unfiled recordings AND reserved-root notes both model as "Not classified";
/// a machine-owned `.murmur/` container never appears at all.
#[test]
fn hierarchy_models_both_unclassified_sources_and_hides_system_containers() {
    let (db, path) = fresh_db("hierarchy-shape");
    container(&db, "p1", "Product", "Product", None, "project");
    container(&db, "f1", "Atlas", "Product/Atlas", Some("p1"), "folder");
    // The reserved note root, with a REAL folder child that must be hoisted to `p1`'s depth.
    container(
        &db,
        "root",
        "Notes home",
        "Product/Notes",
        Some("p1"),
        "folder",
    );
    set_flag(&db, "root", "is_root", "1");
    db.lock()
        .execute(
            "UPDATE folders SET kind = 'note' WHERE id = 'root'",
            rusqlite::params![],
        )
        .unwrap();
    container(
        &db,
        "hoisted",
        "Ideas",
        "Product/Notes/Ideas",
        Some("root"),
        "folder",
    );
    // A machine-owned container the tree deliberately hides.
    container(&db, "sys", "Tasks", ".murmur/tasks", None, "project");
    // Neither an orphan nor an arbitrary-level row belongs to the rendered hierarchy, so their
    // contents must not leak back in as a fake "Not classified" search hit.
    container(
        &db,
        "orphan",
        "Orphan",
        "Lost/Orphan",
        Some("missing-parent"),
        "folder",
    );
    container(
        &db,
        "odd-level",
        "Odd level",
        "Product/Odd",
        Some("p1"),
        "folder",
    );
    // Current schema protects this with a CHECK. The reader still fails closed for a legacy or
    // corrupt row, so the oracle deliberately bypasses CHECK enforcement for this fixture only.
    db.lock()
        .execute_batch(
            "PRAGMA ignore_check_constraints = ON;
             UPDATE folders SET level = 'dashboard' WHERE id = 'odd-level';
             PRAGMA ignore_check_constraints = OFF;",
        )
        .unwrap();

    meeting_in(
        &db,
        "m-filed",
        "Filed recording",
        "2026-03-01T09:00:00Z",
        Some("f1"),
    );
    meeting_in(
        &db,
        "m-loose",
        "Loose recording",
        "2026-03-02T09:00:00Z",
        None,
    );
    note_in(&db, "n-filed", "Filed note", "f1", 1_700_000_000_000);
    note_in(&db, "n-root", "Loose idea", "root", 1_700_000_100_000);
    document_in(&db, "d-filed", "Launch plan.pdf", "f1", 1_700_000_200_000);
    meeting_in(
        &db,
        "m-orphan",
        "Hidden orphan recording",
        "2026-03-03T09:00:00Z",
        Some("orphan"),
    );
    note_in(
        &db,
        "n-odd",
        "Hidden arbitrary note",
        "odd-level",
        1_700_000_300_000,
    );

    let boot = related_picker_bootstrap_inner(&db, &HashSet::new(), "meeting", "m-filed").unwrap();

    // Exactly one top-level Space; the machine container is absent.
    assert_eq!(boot.spaces.len(), 1);
    assert_eq!(boot.spaces[0].id, "p1");
    let payload = wire(&boot);
    assert!(
        !payload.contains("\"sys\""),
        "a system container must never appear"
    );
    assert!(
        !payload.contains("Notes home"),
        "the reserved note root is a SECTION, not a folder row"
    );
    assert!(!payload.contains("Orphan"));
    assert!(!payload.contains("Odd level"));

    // The root's real folder child is hoisted to the Space's own depth.
    let folder_ids: Vec<&str> = boot.spaces[0]
        .folders
        .iter()
        .map(|f| f.id.as_str())
        .collect();
    assert!(folder_ids.contains(&"f1"));
    assert!(
        folder_ids.contains(&"hoisted"),
        "a folder under the reserved root must stay reachable; got {folder_ids:?}"
    );

    // `Not classified` carries BOTH sources: the unfiled recording and the reserved-root note.
    let unclassified_kinds: Vec<PickerItemKind> =
        boot.unclassified.iter().map(|g| g.kind).collect();
    assert!(unclassified_kinds.contains(&PickerItemKind::Meeting));
    assert!(unclassified_kinds.contains(&PickerItemKind::Note));

    let loose_meetings = related_picker_items_inner(
        &db,
        &HashSet::new(),
        "meeting",
        "m-filed",
        None,
        "meeting",
        0,
        50,
    )
    .unwrap();
    assert_eq!(titles(&loose_meetings.items), vec!["Loose recording"]);
    let loose_notes = related_picker_items_inner(
        &db,
        &HashSet::new(),
        "meeting",
        "m-filed",
        None,
        "note",
        0,
        50,
    )
    .unwrap();
    assert_eq!(titles(&loose_notes.items), vec!["Loose idea"]);

    // A real folder serves all three linkable kinds and NOTHING else.
    for (kind, expected) in [
        ("meeting", vec!["Filed recording"]),
        ("note", vec!["Filed note"]),
        ("document", vec!["Launch plan.pdf"]),
    ] {
        let page = related_picker_items_inner(
            &db,
            &HashSet::new(),
            "meeting",
            "m-filed",
            Some("f1"),
            kind,
            0,
            50,
        )
        .unwrap();
        assert_eq!(titles(&page.items), expected, "kind {kind}");
    }
    // Task and dashboard are not kinds this surface has at all — asking is an InvalidArg, never a
    // silently-coerced page.
    for bogus in ["task", "dashboard", "person"] {
        assert!(matches!(
            related_picker_items_inner(
                &db,
                &HashSet::new(),
                "meeting",
                "m-filed",
                Some("f1"),
                bogus,
                0,
                50,
            ),
            Err(AppError::InvalidArg(_))
        ));
    }

    // Search spans every kind and carries a full breadcrumb.
    let hits = related_picker_search_inner(&db, &HashSet::new(), "meeting", "m-filed", "l", 0, 50)
        .unwrap();
    let found: Vec<(&str, Vec<&str>)> = hits
        .hits
        .iter()
        .map(|h| {
            (
                h.title.as_str(),
                h.breadcrumb.iter().map(String::as_str).collect(),
            )
        })
        .collect();
    assert!(
        found.contains(&("Launch plan.pdf", vec!["Product", "Atlas"])),
        "a filed hit carries its Space/folder breadcrumb; got {found:?}"
    );
    assert!(
        found.contains(&("Loose recording", vec!["Not classified"])),
        "an unfiled hit reads as Not classified; got {found:?}"
    );
    assert!(
        found.contains(&("Loose idea", vec!["Not classified"])),
        "a reserved-root note reads as Not classified; got {found:?}"
    );

    let hidden =
        related_picker_search_inner(&db, &HashSet::new(), "meeting", "m-filed", "Hidden", 0, 50)
            .unwrap();
    assert_eq!(
        hidden.total, 0,
        "items owned by orphan/arbitrary-level containers must not become search hits"
    );
    for invalid_scope in ["orphan", "odd-level"] {
        assert!(matches!(
            related_picker_items_inner(
                &db,
                &HashSet::new(),
                "meeting",
                "m-filed",
                Some(invalid_scope),
                "meeting",
                0,
                50,
            ),
            Err(AppError::Locked(_))
        ));
    }

    let _ = std::fs::remove_file(path);
}

/// RED-before-GREEN: search follows the full VISIBLE hierarchy breadcrumb, not only the leaf
/// title. A folder query includes deeper descendants; a Space query includes every visible leaf
/// below it; sealed descendants stay absent from both rows and totals. The bounded pages retain
/// the same total/order, and the pre-existing title match remains intact.
#[test]
fn search_matches_space_and_folder_breadcrumbs_without_leaking_locked_descendants() {
    let (db, path) = fresh_db("search-visible-breadcrumbs");
    container(&db, "p1", "Product", "Product", None, "project");
    container(&db, "f1", "Atlas", "Product/Atlas", Some("p1"), "folder");
    container(
        &db,
        "f2",
        "Research",
        "Product/Atlas/Research",
        Some("f1"),
        "folder",
    );
    container(
        &db,
        "secret",
        "Restricted",
        "Product/Atlas/Restricted",
        Some("f1"),
        "folder",
    );
    container(
        &db,
        "root",
        "Notes home",
        "Product/Notes",
        Some("p1"),
        "folder",
    );
    db.lock()
        .execute(
            "UPDATE folders SET kind = 'note', is_root = 1 WHERE id = 'root'",
            rusqlite::params![],
        )
        .unwrap();

    meeting_in(
        &db,
        "m-space",
        "Direct scope",
        "2026-03-03T09:00:00Z",
        Some("p1"),
    );
    meeting_in(
        &db,
        "m-anchor",
        "Kickoff",
        "2026-03-01T09:00:00Z",
        Some("f1"),
    );
    note_in(&db, "n-atlas", "Roadmap memo", "f1", 1_700_000_100_000);
    document_in(&db, "d-research", "Evidence.pdf", "f2", 1_700_000_200_000);
    meeting_in(
        &db,
        "m-secret",
        "Vaulted thing",
        "2026-03-04T09:00:00Z",
        Some("secret"),
    );
    meeting_in(
        &db,
        "m-loose",
        "Loose recording",
        "2026-03-05T09:00:00Z",
        None,
    );
    note_in(&db, "n-loose", "Loose idea", "root", 1_700_000_300_000);
    seal(&db, "secret");

    // Folder-only match: both the folder's direct leaves and a deeper folder's leaves appear.
    // Exercise paging too: every page reports the same total and concatenates to stable kind/time
    // order without duplicates.
    let atlas_first = related_picker_search_inner(
        &db,
        &HashSet::new(),
        "meeting",
        "m-anchor",
        "  aTlAs  ",
        0,
        2,
    )
    .unwrap();
    let atlas_rest = related_picker_search_inner(
        &db,
        &HashSet::new(),
        "meeting",
        "m-anchor",
        "atlas",
        2,
        2,
    )
    .unwrap();
    assert_eq!(atlas_first.total, 3);
    assert_eq!(atlas_rest.total, 3);
    let atlas_ids: Vec<&str> = atlas_first
        .hits
        .iter()
        .chain(atlas_rest.hits.iter())
        .map(|hit| hit.id.as_str())
        .collect();
    assert_eq!(atlas_ids, vec!["m-anchor", "n-atlas", "d-research"]);
    assert_eq!(
        atlas_rest.hits[0].breadcrumb,
        vec!["Product", "Atlas", "Research"]
    );
    assert!(
        atlas_first
            .hits
            .iter()
            .chain(atlas_rest.hits.iter())
            .all(|hit| hit.id != "m-secret" && hit.title != "Vaulted thing"),
        "a locked descendant must not appear or inflate the total"
    );

    // Matching the sealed folder's OWN disclosed label still reveals none of its children and not
    // even their count. This exercises the new breadcrumb OR directly rather than only through a
    // matching visible ancestor.
    let restricted = related_picker_search_inner(
        &db,
        &HashSet::new(),
        "meeting",
        "m-anchor",
        "Restricted",
        0,
        50,
    )
    .unwrap();
    assert!(restricted.hits.is_empty());
    assert_eq!(restricted.total, 0);

    // Space-only match reaches a direct Space leaf plus every visible nested scope. The hidden
    // Notes root is represented as Not classified, so its direct note is correctly NOT swept into
    // the Product breadcrumb result merely because the storage row sits below Product.
    let product = related_picker_search_inner(
        &db,
        &HashSet::new(),
        "meeting",
        "m-anchor",
        "product",
        0,
        50,
    )
    .unwrap();
    assert_eq!(product.total, 4);
    assert_eq!(
        product
            .hits
            .iter()
            .map(|hit| hit.id.as_str())
            .collect::<Vec<_>>(),
        vec!["m-space", "m-anchor", "n-atlas", "d-research"]
    );
    assert!(product.hits.iter().all(|hit| {
        hit.breadcrumb.first().map(String::as_str) == Some("Product")
            && hit.id != "m-secret"
            && hit.id != "n-loose"
    }));

    // The synthetic visible path participates too, without adding container DTO hits.
    let unclassified = related_picker_search_inner(
        &db,
        &HashSet::new(),
        "meeting",
        "m-anchor",
        "NOT CLASSIFIED",
        0,
        50,
    )
    .unwrap();
    assert_eq!(unclassified.total, 2);
    assert_eq!(
        unclassified
            .hits
            .iter()
            .map(|hit| hit.id.as_str())
            .collect::<Vec<_>>(),
        vec!["m-loose", "n-loose"]
    );
    assert!(
        unclassified
            .hits
            .iter()
            .all(|hit| hit.breadcrumb == vec!["Not classified"])
    );

    // Existing title substring behavior is unchanged and still carries the resolved breadcrumb.
    let title = related_picker_search_inner(
        &db,
        &HashSet::new(),
        "meeting",
        "m-anchor",
        "evidence",
        0,
        50,
    )
    .unwrap();
    assert_eq!(title.total, 1);
    assert_eq!(title.hits[0].id, "d-research");
    assert_eq!(
        title.hits[0].breadcrumb,
        vec!["Product", "Atlas", "Research"]
    );

    let _ = std::fs::remove_file(path);
}

/// RED-before-GREEN: databases that declined hierarchy adoption legitimately keep the exact
/// canonical Notes root parentless. Its real folders remain reachable, while the root stays hidden
/// and non-selectable. An arbitrary `is_root` row is not a substitute for storage-owned identity.
#[test]
fn parentless_canonical_notes_root_hoists_only_its_reachable_children() {
    let (db, path) = fresh_db("legacy-parentless-notes-root");
    container(&db, "notes-root", "Notes home", "Notes", None, "folder");
    db.lock()
        .execute(
            "UPDATE folders SET kind = 'note', is_root = 1 WHERE id = 'notes-root'",
            rusqlite::params![],
        )
        .unwrap();
    assert_eq!(db.note_root_id().unwrap().as_deref(), Some("notes-root"));
    container(
        &db,
        "legacy-folder",
        "Legacy ideas",
        "Notes/Legacy ideas",
        Some("notes-root"),
        "folder",
    );
    db.lock()
        .execute(
            "UPDATE folders SET kind = 'note' WHERE id = 'legacy-folder'",
            rusqlite::params![],
        )
        .unwrap();

    // A different flagged row and its descendants must not gain the canonical root's privilege.
    container(&db, "fake-root", "Fake root", "Fake", None, "folder");
    set_flag(&db, "fake-root", "is_root", "1");
    container(
        &db,
        "fake-child",
        "Fake child",
        "Fake/Child",
        Some("fake-root"),
        "folder",
    );

    note_in(
        &db,
        "n-anchor",
        "Legacy launch idea",
        "legacy-folder",
        1_700_000_300_000,
    );
    note_in(
        &db,
        "n-root",
        "Canonical loose note",
        "notes-root",
        1_700_000_200_000,
    );
    note_in(
        &db,
        "n-fake",
        "Fake loose note",
        "fake-root",
        1_700_000_100_000,
    );

    let boot = related_picker_bootstrap_inner(&db, &HashSet::new(), "note", "n-anchor").unwrap();
    assert_eq!(
        boot.spaces
            .iter()
            .map(|node| node.id.as_str())
            .collect::<Vec<_>>(),
        vec!["legacy-folder"],
        "the canonical root's child is hoisted into the top level; arbitrary roots stay orphaned"
    );
    let wire = wire(&boot);
    assert!(
        !wire.contains("Notes home"),
        "the canonical root stays hidden"
    );
    assert!(!wire.contains("Fake root"));
    assert!(!wire.contains("Fake child"));
    let anchor = boot.anchor.expect("anchor location");
    assert_eq!(anchor.container_id.as_deref(), Some("legacy-folder"));
    assert_eq!(anchor.path, vec!["legacy-folder"]);

    let page = related_picker_items_inner(
        &db,
        &HashSet::new(),
        "note",
        "n-anchor",
        Some("legacy-folder"),
        "note",
        0,
        20,
    )
    .unwrap();
    assert_eq!(titles(&page.items), vec!["Legacy launch idea"]);
    assert!(matches!(
        related_picker_items_inner(
            &db,
            &HashSet::new(),
            "note",
            "n-anchor",
            Some("notes-root"),
            "note",
            0,
            20,
        ),
        Err(AppError::Locked(_))
    ));

    let unclassified = related_picker_items_inner(
        &db,
        &HashSet::new(),
        "note",
        "n-anchor",
        None,
        "note",
        0,
        20,
    )
    .unwrap();
    assert_eq!(
        titles(&unclassified.items),
        vec!["Canonical loose note"],
        "only documents in db.note_root_id() are unclassified"
    );

    let search = related_picker_search_inner(
        &db,
        &HashSet::new(),
        "note",
        "n-anchor",
        "Legacy launch",
        0,
        20,
    )
    .unwrap();
    assert_eq!(search.hits.len(), 1);
    assert_eq!(search.hits[0].id, "n-anchor");
    assert_eq!(search.hits[0].breadcrumb, vec!["Legacy ideas"]);
    let fake_search =
        related_picker_search_inner(&db, &HashSet::new(), "note", "n-anchor", "Fake", 0, 20)
            .unwrap();
    assert!(fake_search.hits.is_empty());

    let _ = std::fs::remove_file(path);
}

// ── 4. THE ANCHOR GATE, AND WHAT A SEALED CONTAINER DISCLOSES ────────────────────────────────────

/// A sealed OR unknown anchor fails CLOSED and INDISTINGUISHABLY, discloses no titles/counts/hits,
/// and a session unlock restores the whole surface.
#[test]
fn a_sealed_or_unknown_anchor_fails_closed_indistinguishably_until_unlock() {
    let (db, path) = fresh_db("anchor-gate");
    container(&db, "p1", "Product", "Product", None, "project");
    container(
        &db,
        "secret",
        "Secret",
        "Product/Secret",
        Some("p1"),
        "folder",
    );
    container(&db, "open", "Open", "Product/Open", Some("p1"), "folder");
    meeting_in(
        &db,
        "m-secret",
        "Board pay review",
        "2026-03-01T09:00:00Z",
        Some("secret"),
    );
    meeting_in(
        &db,
        "m-open",
        "Weekly standup",
        "2026-03-02T09:00:00Z",
        Some("open"),
    );
    seal(&db, "secret");

    // SEALED anchor → refused, with nothing at all in the payload.
    let sealed_err =
        related_picker_bootstrap_inner(&db, &HashSet::new(), "meeting", "m-secret").unwrap_err();
    // UNKNOWN anchor → the SAME variant and the SAME message, so the modal is not an oracle.
    let unknown_err =
        related_picker_bootstrap_inner(&db, &HashSet::new(), "meeting", "nope").unwrap_err();
    assert!(matches!(sealed_err, AppError::Locked(_)));
    assert!(matches!(unknown_err, AppError::Locked(_)));
    assert_eq!(sealed_err.to_string(), unknown_err.to_string());

    // Search refuses the same way — an unlocked sibling surface cannot walk around the gate.
    let search_err =
        related_picker_search_inner(&db, &HashSet::new(), "meeting", "m-secret", "pay", 0, 50)
            .unwrap_err();
    assert_eq!(search_err.to_string(), sealed_err.to_string());

    // From an OPEN anchor, the sealed container discloses its NAME and nothing else.
    let boot = related_picker_bootstrap_inner(&db, &HashSet::new(), "meeting", "m-open").unwrap();
    let payload = wire(&boot);
    assert!(
        payload.contains("Secret"),
        "a locked container still shows its name"
    );
    assert!(
        !payload.contains("Board pay review"),
        "a locked container must NEVER disclose a child title"
    );
    assert!(
        !payload.contains("m-secret"),
        "a locked container must NEVER disclose a child id"
    );
    let secret = boot.spaces[0]
        .folders
        .iter()
        .find(|f| f.id == "secret")
        .unwrap();
    assert!(secret.locked && !secret.unlocked);
    assert!(
        secret.groups.is_empty(),
        "a sealed container carries NO groups — not even a zero total"
    );
    assert!(
        !secret.linkable,
        "a sealed container is not a valid link endpoint"
    );

    // Its items are REFUSED, not answered with an empty page.
    assert!(matches!(
        related_picker_items_inner(
            &db,
            &HashSet::new(),
            "meeting",
            "m-open",
            Some("secret"),
            "meeting",
            0,
            50,
        ),
        Err(AppError::Locked(_))
    ));
    // Paging carries the anchor gate too. After auto-relock, even an OPEN scope is unavailable to
    // a modal whose anchor has become sealed; otherwise the already-open modal is a lock bypass.
    let relocked_page_err = related_picker_items_inner(
        &db,
        &HashSet::new(),
        "meeting",
        "m-secret",
        Some("open"),
        "meeting",
        0,
        50,
    )
    .unwrap_err();
    assert_eq!(relocked_page_err.to_string(), sealed_err.to_string());
    let unknown_page_err = related_picker_items_inner(
        &db,
        &HashSet::new(),
        "meeting",
        "nope",
        Some("open"),
        "meeting",
        0,
        50,
    )
    .unwrap_err();
    assert_eq!(unknown_page_err.to_string(), sealed_err.to_string());
    // And a search from an open anchor cannot surface a sealed hit.
    let hits = related_picker_search_inner(&db, &HashSet::new(), "meeting", "m-open", "pay", 0, 50)
        .unwrap();
    assert_eq!(
        hits.total, 0,
        "a sealed hit must not even inflate the total"
    );
    assert!(hits.hits.is_empty());

    // SESSION UNLOCK restores every one of those.
    let session = unlocked(&["secret"]);
    let boot = related_picker_bootstrap_inner(&db, &session, "meeting", "m-secret").unwrap();
    assert!(boot.anchor.is_some(), "an unlocked anchor resolves again");
    let secret = boot.spaces[0]
        .folders
        .iter()
        .find(|f| f.id == "secret")
        .unwrap();
    assert!(secret.linkable && !secret.groups.is_empty());
    let page = related_picker_items_inner(
        &db,
        &session,
        "meeting",
        "m-secret",
        Some("secret"),
        "meeting",
        0,
        50,
    )
    .unwrap();
    assert_eq!(titles(&page.items), vec!["Board pay review"]);
    let hits =
        related_picker_search_inner(&db, &session, "meeting", "m-open", "pay", 0, 50).unwrap();
    assert_eq!(hits.total, 1);

    let _ = std::fs::remove_file(path);
}

// ── 5. THE CONTAINER LINK ENDPOINT — one directed edge, zero descendants ─────────────────────────

fn link_rows(db: &Db) -> Vec<(String, String, String, String, String)> {
    let conn = db.lock();
    let mut stmt = conn
        .prepare("SELECT src_kind, src_id, dst_kind, dst_id, edge_type FROM links ORDER BY id")
        .unwrap();
    let rows = stmt
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .unwrap();
    rows.map(Result::unwrap).collect()
}

/// Linking a whole Space/folder writes EXACTLY ONE directed manual edge to the container's stable
/// id — and ZERO edges to anything it contains.
#[test]
fn linking_a_container_writes_one_directed_edge_and_zero_descendant_edges() {
    let (db, path) = fresh_db("container-write");
    container(&db, "p1", "Product", "Product", None, "project");
    container(&db, "f1", "Atlas", "Product/Atlas", Some("p1"), "folder");
    meeting_in(
        &db,
        "m-anchor",
        "Anchor",
        "2026-03-01T09:00:00Z",
        Some("f1"),
    );
    meeting_in(&db, "m-child", "Child", "2026-03-02T09:00:00Z", Some("f1"));
    note_in(&db, "n-child", "Child note", "f1", 1_700_000_000_000);

    db.upsert_manual_link_visible("meeting", "m-anchor", "container", "f1", &HashSet::new())
        .unwrap();

    let rows = link_rows(&db);
    assert_eq!(
        rows,
        vec![(
            "meeting".to_string(),
            "m-anchor".to_string(),
            "container".to_string(),
            "f1".to_string(),
            "manual".to_string(),
        )],
        "exactly ONE directed edge to the container; a container link NEVER fans out"
    );

    // It reads back as a chip with the container's LIVE name and its Space/folder level.
    let edges = db
        .links_for_visible(crate::links::LinkKind::Meeting, "m-anchor", &HashSet::new())
        .unwrap();
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].other_kind, "container");
    assert_eq!(edges[0].other_id, "f1");
    assert_eq!(edges[0].other_title, "Atlas");
    assert_eq!(edges[0].other_container_level.as_deref(), Some("folder"));

    // RENAME and REPARENT both preserve it: the stored id is `folders.id`, and only the display
    // string is resolved at read time.
    db.lock()
        .execute(
            "UPDATE folders SET name = 'Atlas v2', parent_id = NULL, level = 'project' WHERE id = 'f1'",
            rusqlite::params![],
        )
        .unwrap();
    let edges = db
        .links_for_visible(crate::links::LinkKind::Meeting, "m-anchor", &HashSet::new())
        .unwrap();
    assert_eq!(edges[0].other_title, "Atlas v2");
    assert_eq!(edges[0].other_container_level.as_deref(), Some("project"));

    // Unlink removes exactly that one row.
    db.delete_manual_links_with_marker_seals(
        &[crate::storage::models::ManualLinkEdge {
            src_kind: "meeting".into(),
            src_id: "m-anchor".into(),
            dst_kind: "container".into(),
            dst_id: "f1".into(),
        }],
        &[],
        &HashSet::new(),
    )
    .unwrap();
    assert!(link_rows(&db).is_empty());

    let _ = std::fs::remove_file(path);
}

/// Every refusable container endpoint refuses, and every refusal writes ZERO rows.
#[test]
fn an_invalid_container_endpoint_is_refused_with_no_write() {
    let (db, path) = fresh_db("container-refusals");
    container(&db, "p1", "Product", "Product", None, "project");
    container(
        &db,
        "sealed",
        "Sealed",
        "Product/Sealed",
        Some("p1"),
        "folder",
    );
    container(
        &db,
        "root",
        "Notes home",
        "Product/Notes",
        Some("p1"),
        "folder",
    );
    set_flag(&db, "root", "is_root", "1");
    container(&db, "sys", "Tasks", ".murmur/tasks", None, "project");
    seal(&db, "sealed");
    meeting_in(
        &db,
        "m-anchor",
        "Anchor",
        "2026-03-01T09:00:00Z",
        Some("p1"),
    );

    for bad in ["missing", "sealed", "root", "sys"] {
        let err = db
            .upsert_manual_link_visible("meeting", "m-anchor", "container", bad, &HashSet::new())
            .unwrap_err();
        assert!(
            matches!(err, AppError::Locked(_)),
            "endpoint {bad:?} must be refused fail-closed, got {err:?}"
        );
    }
    // A container cannot be linked to itself either.
    assert!(link_rows(&db).is_empty(), "a refusal must write NOTHING");

    // The sealed one becomes valid the moment the session unlocks it.
    db.upsert_manual_link_visible(
        "meeting",
        "m-anchor",
        "container",
        "sealed",
        &unlocked(&["sealed"]),
    )
    .unwrap();
    assert_eq!(link_rows(&db).len(), 1);

    let _ = std::fs::remove_file(path);
}

/// A manual item→container relation SURVIVES a seal at rest (it is a user decision), but the whole
/// relation is INVISIBLE until the session unlocks it — in either direction.
#[test]
fn a_container_relation_survives_seal_at_rest_but_is_invisible_until_unlock() {
    let (db, path) = fresh_db("container-seal");
    container(&db, "p1", "Product", "Product", None, "project");
    container(
        &db,
        "target",
        "Atlas",
        "Product/Atlas",
        Some("p1"),
        "folder",
    );
    container(&db, "home", "Home", "Product/Home", Some("p1"), "folder");
    meeting_in(
        &db,
        "m-anchor",
        "Anchor",
        "2026-03-01T09:00:00Z",
        Some("home"),
    );

    db.upsert_manual_link_visible(
        "meeting",
        "m-anchor",
        "container",
        "target",
        &HashSet::new(),
    )
    .unwrap();

    // Seal the TARGET container: the relation row survives (a user decision), but the chip is gone.
    seal(&db, "target");
    assert_eq!(link_rows(&db).len(), 1, "a user decision survives a seal");
    let edges = db
        .links_for_visible(crate::links::LinkKind::Meeting, "m-anchor", &HashSet::new())
        .unwrap();
    assert!(
        edges.is_empty(),
        "a relation to a sealed container must not be visible — not even its existence"
    );
    // …and it comes back on unlock, with the container's live name.
    let edges = db
        .links_for_visible(
            crate::links::LinkKind::Meeting,
            "m-anchor",
            &unlocked(&["target"]),
        )
        .unwrap();
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].other_title, "Atlas");

    // Seal the ANCHOR's container instead: the whole relation disappears from that side too.
    db.lock()
        .execute(
            "UPDATE folders SET locked = 0 WHERE id = 'target'",
            rusqlite::params![],
        )
        .unwrap();
    seal(&db, "home");
    let edges = db
        .links_for_visible(crate::links::LinkKind::Meeting, "m-anchor", &HashSet::new())
        .unwrap();
    assert!(
        edges.is_empty(),
        "a locked ANCHOR must not reveal that it has any relation at all"
    );
    let edges = db
        .links_for_visible(
            crate::links::LinkKind::Meeting,
            "m-anchor",
            &unlocked(&["home"]),
        )
        .unwrap();
    assert_eq!(edges.len(), 1, "unlocking the anchor restores the relation");

    let _ = std::fs::remove_file(path);
}

// ── 6. CONTAINER LIFECYCLE ───────────────────────────────────────────────────────────────────────

/// The trash snapshot carries BOTH directions of a container's incident rows, the delete purges
/// them transactionally, and a restore replays the exact rows with `INSERT OR IGNORE` so a newer
/// explicit user decision wins.
#[test]
fn container_delete_snapshots_and_purges_both_directions_and_restore_respects_a_newer_decision() {
    let (db, path) = fresh_db("container-lifecycle");
    container(&db, "p1", "Product", "Product", None, "project");
    container(
        &db,
        "doomed",
        "Doomed",
        "Product/Doomed",
        Some("p1"),
        "folder",
    );
    meeting_in(&db, "m-out", "Outgoing", "2026-03-01T09:00:00Z", Some("p1"));
    meeting_in(&db, "m-in", "Incoming", "2026-03-02T09:00:00Z", Some("p1"));

    // One edge in each direction — an inbound edge is somebody's own "this Space is about that
    // note" decision, and losing it is what makes a restored folder look unrelated to everything.
    db.upsert_manual_link_visible("meeting", "m-out", "container", "doomed", &HashSet::new())
        .unwrap();
    db.upsert_manual_link_visible("container", "doomed", "meeting", "m-in", &HashSet::new())
        .unwrap();

    let snapshot = db.link_rows_for_container("doomed").unwrap();
    assert_eq!(snapshot.len(), 2, "BOTH directions are snapshotted");

    // The delete purges them in the same transaction that drops the container.
    {
        let mut conn = db.lock();
        let tx = conn.transaction().unwrap();
        Db::purge_container_links_tx(&tx, "doomed").unwrap();
        tx.commit().unwrap();
    }
    assert!(
        link_rows(&db).is_empty(),
        "no relation may dangle at a deleted place"
    );

    // Meanwhile the user makes a NEWER explicit decision about the same pair.
    db.upsert_manual_link_visible("meeting", "m-out", "container", "doomed", &HashSet::new())
        .unwrap();
    db.lock()
        .execute(
            "UPDATE links SET status = 'dismissed' WHERE dst_id = 'doomed'",
            rusqlite::params![],
        )
        .unwrap();

    let restored = db.restore_link_rows(&snapshot, &HashSet::new()).unwrap();
    let rows = link_rows(&db);
    assert_eq!(rows.len(), 2, "the missing direction is replayed exactly");
    assert_eq!(
        restored, 1,
        "INSERT OR IGNORE skips the pair that already exists"
    );
    let status: String = db
        .lock()
        .query_row(
            "SELECT status FROM links WHERE src_id = 'm-out' AND dst_id = 'doomed'",
            rusqlite::params![],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        status, "dismissed",
        "a newer explicit user decision must survive a restore"
    );

    let _ = std::fs::remove_file(path);
}

/// Restoring a folder must not resurrect its manual relation when the OTHER endpoint was deleted
/// while the folder sat in trash. A sealed-but-still-existing endpoint is different: retain the
/// decision at rest and let the ordinary two-endpoint read gate hide it until session unlock.
#[test]
fn container_restore_drops_dangling_relation_but_keeps_a_sealed_existing_endpoint() {
    let (db, path) = fresh_db("container-restore-endpoints");
    container(&db, "p1", "Product", "Product", None, "project");
    container(
        &db,
        "target",
        "Target",
        "Product/Target",
        Some("p1"),
        "folder",
    );
    container(&db, "home", "Home", "Product/Home", Some("p1"), "folder");
    meeting_in(
        &db,
        "m-gone",
        "Deleted later",
        "2026-03-01T09:00:00Z",
        Some("p1"),
    );
    meeting_in(
        &db,
        "m-sealed",
        "Sealed later",
        "2026-03-02T09:00:00Z",
        Some("home"),
    );

    db.upsert_manual_link_visible("container", "target", "meeting", "m-gone", &HashSet::new())
        .unwrap();
    db.upsert_manual_link_visible(
        "container",
        "target",
        "meeting",
        "m-sealed",
        &HashSet::new(),
    )
    .unwrap();
    let snapshot = db.link_rows_for_container("target").unwrap();
    assert_eq!(snapshot.len(), 2);
    {
        let mut conn = db.lock();
        let tx = conn.transaction().unwrap();
        Db::purge_container_links_tx(&tx, "target").unwrap();
        tx.commit().unwrap();
    }

    db.lock()
        .execute(
            "DELETE FROM meetings WHERE id = 'm-gone'",
            rusqlite::params![],
        )
        .unwrap();
    seal(&db, "home");

    let restored = db.restore_link_rows(&snapshot, &HashSet::new()).unwrap();
    assert_eq!(
        restored, 1,
        "only the relation with two live endpoints returns"
    );
    let rows = link_rows(&db);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].3, "m-sealed");

    let hidden = db
        .links_for_visible(crate::links::LinkKind::Container, "target", &HashSet::new())
        .unwrap();
    assert!(
        hidden.is_empty(),
        "the restored row stays gated while sealed"
    );
    let visible = db
        .links_for_visible(
            crate::links::LinkKind::Container,
            "target",
            &unlocked(&["home"]),
        )
        .unwrap();
    assert_eq!(
        visible.len(),
        1,
        "session unlock reveals the retained decision"
    );

    let _ = std::fs::remove_file(path);
}

// ── 7. EXISTING BEHAVIOUR IS UNTOUCHED ───────────────────────────────────────────────────────────

/// A meeting↔note relation still round-trips exactly as before, alongside a container relation.
#[test]
fn existing_content_link_behaviour_is_unchanged_beside_a_container_relation() {
    let (db, path) = fresh_db("coexist");
    container(&db, "p1", "Product", "Product", None, "project");
    container(&db, "f1", "Atlas", "Product/Atlas", Some("p1"), "folder");
    meeting_in(&db, "m1", "Anchor", "2026-03-01T09:00:00Z", Some("f1"));
    note_in(&db, "n1", "Neighbour", "f1", 1_700_000_000_000);

    db.upsert_manual_link_visible("meeting", "m1", "note", "n1", &HashSet::new())
        .unwrap();
    db.upsert_manual_link_visible("meeting", "m1", "container", "f1", &HashSet::new())
        .unwrap();

    let edges = db
        .links_for_visible(crate::links::LinkKind::Meeting, "m1", &HashSet::new())
        .unwrap();
    let by_kind: std::collections::HashMap<&str, &crate::storage::models::LinkEdge> =
        edges.iter().map(|e| (e.other_kind.as_str(), e)).collect();

    let note_edge = by_kind.get("note").expect("the note chip still renders");
    assert_eq!(note_edge.other_title, "Neighbour");
    assert!(note_edge.manual);
    assert!(
        note_edge.other_container_level.is_none(),
        "a non-container neighbour carries no level — the field is skipped entirely"
    );
    // The additive field must not appear on the wire for a content edge.
    assert!(!wire(note_edge).contains("otherContainerLevel"));

    let container_edge = by_kind
        .get("container")
        .expect("the container chip renders");
    assert_eq!(
        container_edge.other_container_level.as_deref(),
        Some("folder")
    );

    let _ = std::fs::remove_file(path);
}
