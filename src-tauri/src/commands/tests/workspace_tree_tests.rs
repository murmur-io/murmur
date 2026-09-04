//! Oracles for the workspace hierarchy's READ surface (Projects › Folders › per-kind item groups).
//!
//! File-backed SQLCipher via `open_with_key` + a FIXED literal test key — these never touch the real
//! Keychain, and they never mint a content key: the hierarchy's read path adds no seal, so a lock is
//! modelled here by the `folders.locked` COLUMN, which is exactly what every gate reads.
//!
//! The leak oracles below (`a_sealed_container_*`) are the ones that matter. A green
//! `cargo test --lib`, `ng lint` and `ng build` all pass for an ungated read; only a test that
//! seals a container and then asserts nothing comes back can see it.

use super::*;
use crate::storage::db::Db;
use crate::storage::models::{
    ContainerDto, ContainerNode, ContainerRow, ItemKind, ItemPage, ItemRow, Meeting, MeetingStatus,
    NoteRecord, TypeGroup,
};
use std::collections::HashSet;

const TEST_DEK: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn fresh_db(label: &str) -> (Db, std::path::PathBuf) {
    let path = crate::storage::db::unique_temp_path(&format!("murmur-workspace-{label}"), "sqlite");
    let _ = std::fs::remove_file(&path);
    let db = Db::open_with_key(&path, TEST_DEK).unwrap();
    // Opening a database ADOPTS it: the hierarchy migration creates a default project at the vault
    // root. Undo that here so each test states the exact container shape it is about — otherwise a
    // test that builds its own project at the vault root collides with the automatic one on the
    // UNIQUE path, and every fixture would have to work around a row it never asked for. The
    // migration itself is exercised on purpose by the tests at the end of this file.
    db.lock()
        .execute(
            "DELETE FROM folders WHERE COALESCE(level, 'folder') = 'project'",
            rusqlite::params![],
        )
        .unwrap();
    (db, path)
}

/// A container row. `level` is set afterwards because `insert_folder` deliberately does not take
/// one — every folder a user creates today is a `'folder'`, and only the hierarchy migration (a
/// later step) promotes a row to `'project'`.
fn container(db: &Db, id: &str, name: &str, path: &str, parent: Option<&str>, level: &str) {
    db.insert_folder(&Folder {
        id: id.to_string(),
        name: name.to_string(),
        path: path.to_string(),
        parent_id: parent.map(str::to_string),
        locked: false,
        created_at: "2026-08-22T09:00:00Z".to_string(),
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

/// A meeting filed into `folder`. A meeting has NO folder column — its container lives on its note
/// rows — so the note must exist before `set_meeting_folder` has anything to update.
fn meeting_in(db: &Db, id: &str, title: &str, started_at: &str, folder: Option<&str>) {
    db.insert_meeting(&Meeting {
        id: id.to_string(),
        started_at: started_at.to_string(),
        ended_at: None,
        title: Some(title.to_string()),
        duration_s: 600,
        audio_path: None,
        status: MeetingStatus::Summarized,
        folder_id: folder.map(str::to_string),
    })
    .unwrap();
    db.upsert_note(&NoteRecord {
        meeting_id: id.to_string(),
        provider_id: "claude_code".to_string(),
        markdown: format!("# {title}"),
        created_at: started_at.to_string(),
        ..Default::default()
    })
    .unwrap();
    db.set_meeting_folder(id, folder).unwrap();
}

/// An authored note in `folder`.
fn note_in(db: &Db, id: &str, name: &str, folder: &str, created_at: i64) {
    db.insert_document(id, folder, name, "body", "note", created_at)
        .unwrap();
}

fn tree(db: &Db, unlocked: &HashSet<String>) -> Vec<ContainerNode> {
    workspace_tree_inner(db, unlocked).unwrap()
}

fn group(node: &ContainerNode, kind: ItemKind) -> Option<&TypeGroup> {
    node.groups.iter().find(|g| g.kind == kind)
}

// ── the wire contract ────────────────────────────────────────────────────────────────────────────

/// Every field of every hierarchy DTO is camelCase on the wire.
///
/// The dashboard meeting tile shipped `started_at`/`duration_s`/`has_audio` against a camelCase
/// frontend; every field read `undefined`, the tile threw while rendering and took the whole board
/// down, and six fixes went to the wrong component first. The hand-written e2e mocks could not see
/// it — they are typed against the frontend's own interface and are camelCase by construction, so
/// they DEFINE a shape rather than verify one. Only a serialization assertion catches this class.
#[test]
fn workspace_dto_fields_are_camel_case_on_the_wire() {
    let item = ItemRow {
        kind: ItemKind::Meeting,
        id: "m1".into(),
        title: Some("Standup".into()),
        duration_s: Some(600),
        sort_at: 1,
    };
    let node = ContainerNode {
        id: "p1".into(),
        name: "Acme".into(),
        kind: "meeting".into(),
        level: "project".into(),
        emoji: Some("🗂".into()),
        tint: Some("#fff".into()),
        locked: false,
        unlocked: false,
        is_root: false,
        folders: vec![],
        groups: vec![TypeGroup {
            kind: ItemKind::Meeting,
            total: 1,
            items: vec![item.clone()],
        }],
    };
    let page = ItemPage {
        kind: ItemKind::Note,
        items: vec![],
        total: 0,
    };
    let dto = ContainerDto {
        id: "f1".into(),
        name: "Legal".into(),
        level: "folder".into(),
        emoji: None,
        tint: None,
        locked: false,
        unlocked: false,
        is_root: false,
        parent_id: Some("p1".into()),
        parent_name: Some("Acme".into()),
    };

    // Every key at every depth, not just the top level — the tile bug was in NESTED fields.
    fn assert_camel(value: &serde_json::Value, path: &str) {
        match value {
            serde_json::Value::Object(map) => {
                for (key, child) in map {
                    assert!(
                        !key.contains('_'),
                        "`{path}.{key}` is snake_case on the wire while the frontend reads camelCase"
                    );
                    assert!(
                        key.chars().next().is_some_and(|c| c.is_ascii_lowercase()),
                        "`{path}.{key}` must start lowercase"
                    );
                    assert_camel(child, &format!("{path}.{key}"));
                }
            }
            serde_json::Value::Array(items) => {
                for (i, child) in items.iter().enumerate() {
                    assert_camel(child, &format!("{path}[{i}]"));
                }
            }
            _ => {}
        }
    }

    for (label, value) in [
        ("ContainerNode", serde_json::to_value(&node).unwrap()),
        ("ItemPage", serde_json::to_value(&page).unwrap()),
        ("ContainerDto", serde_json::to_value(&dto).unwrap()),
        ("ItemRow", serde_json::to_value(&item).unwrap()),
    ] {
        assert_camel(&value, label);
    }

    // The exact keys the frontend reads, pinned BY NAME so the failure message is unmistakable.
    let json = serde_json::to_value(&item).unwrap();
    assert!(json.get("durationS").is_some(), "the FE reads `durationS`: {json}");
    assert!(json.get("sortAt").is_some(), "the FE reads `sortAt`: {json}");
    assert_eq!(json.get("kind").and_then(|k| k.as_str()), Some("meeting"));
    let json = serde_json::to_value(&node).unwrap();
    assert!(json.get("isRoot").is_some(), "the FE reads `isRoot`: {json}");
}

// ── the leak oracles ─────────────────────────────────────────────────────────────────────────────

/// A sealed-and-not-session-unlocked container contributes NO item rows and NO group totals.
///
/// The count matters as much as the rows: `count_notes_per_folder` already gates folder counts to
/// zero for exactly this reason, and a total is a disclosure about content the user cannot read.
#[test]
fn a_sealed_container_discloses_no_items_and_no_totals() {
    let (db, path) = fresh_db("sealed-tree");
    container(&db, "p1", "Acme", "", None, "project");
    container(&db, "f1", "Legal", "Legal", Some("p1"), "folder");
    meeting_in(&db, "m1", "Board strategy", "2026-08-20T09:00:00Z", Some("f1"));
    note_in(&db, "d1", "Merger terms", "f1", 1_700_000_000_000);

    // Open: the folder reports both kinds.
    let open = tree(&db, &HashSet::new());
    let folder = &open[0].folders[0];
    assert_eq!(group(folder, ItemKind::Meeting).map(|g| g.total), Some(1));
    assert_eq!(group(folder, ItemKind::Note).map(|g| g.total), Some(1));

    seal(&db, "f1");

    let sealed = tree(&db, &HashSet::new());
    let folder = &sealed[0].folders[0];
    assert!(folder.locked, "the folder reports itself sealed");
    assert!(!folder.unlocked, "and not session-unlocked");
    assert!(
        folder.groups.is_empty(),
        "a sealed container leaked groups: {:?}",
        folder.groups
    );
    // Nothing about its contents may appear anywhere in the payload, at any depth.
    let json = serde_json::to_string(&sealed).unwrap();
    assert!(!json.contains("Board strategy"), "leaked a meeting title: {json}");
    assert!(!json.contains("Merger terms"), "leaked a note title: {json}");
    assert!(!json.contains("\"m1\""), "leaked a meeting id: {json}");
    assert!(!json.contains("\"d1\""), "leaked a note id: {json}");

    // Session-unlocking it brings the content back — the seal is a gate, not a deletion.
    let unlocked: HashSet<String> = ["f1".to_string()].into_iter().collect();
    let back = tree(&db, &unlocked);
    let folder = &back[0].folders[0];
    assert!(folder.unlocked);
    assert_eq!(group(folder, ItemKind::Meeting).map(|g| g.total), Some(1));

    let _ = std::fs::remove_file(&path);
}

/// A sealed container REFUSES its item page rather than answering with an empty one.
///
/// An empty page and a refusal are distinguishable by a prober; only the refusal is honest about
/// why there is nothing to show.
#[test]
fn a_sealed_container_refuses_its_item_page() {
    let (db, path) = fresh_db("sealed-page");
    container(&db, "p1", "Acme", "", None, "project");
    container(&db, "f1", "Legal", "Legal", Some("p1"), "folder");
    meeting_in(&db, "m1", "Board strategy", "2026-08-20T09:00:00Z", Some("f1"));
    seal(&db, "f1");

    let err = container_items_inner(&db, &HashSet::new(), Some("f1"), ItemKind::Meeting, 0, 10)
        .expect_err("a sealed container must refuse, not return an empty page");
    assert!(
        matches!(err, AppError::Locked(_)),
        "a locked-content refusal must be AppError::Locked, got {err:?}"
    );

    // An UNKNOWN container is refused the same way: a caller must not learn "no such container"
    // by getting an empty page back.
    let err = container_items_inner(&db, &HashSet::new(), Some("nope"), ItemKind::Meeting, 0, 10)
        .expect_err("an unknown container fails closed");
    assert!(matches!(err, AppError::Locked(_)));

    let unlocked: HashSet<String> = ["f1".to_string()].into_iter().collect();
    let page = container_items_inner(&db, &unlocked, Some("f1"), ItemKind::Meeting, 0, 10).unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items.len(), 1);

    let _ = std::fs::remove_file(&path);
}

/// No item row carries an on-disk path.
///
/// `get_meeting_detail` nulls `audio_path` for a locked meeting because the frontend feeds any path
/// it receives into `convertFileSrc` — the one audio read that bypasses `export_audio` and
/// `meeting_is_unlocked`. A tree row that carried a path would reopen that hole regardless of the
/// gate above it.
#[test]
fn no_item_row_carries_an_on_disk_path() {
    let (db, path) = fresh_db("no-paths");
    container(&db, "p1", "Acme", "", None, "project");
    db.insert_meeting(&Meeting {
        id: "m1".into(),
        started_at: "2026-08-20T09:00:00Z".into(),
        ended_at: None,
        title: Some("Board".into()),
        duration_s: 60,
        audio_path: Some("/private/var/audio/m1.wav".into()),
        status: MeetingStatus::Summarized,
        folder_id: None,
    })
    .unwrap();
    db.upsert_note(&NoteRecord {
        meeting_id: "m1".into(),
        provider_id: "claude_code".into(),
        markdown: "# Board".into(),
        created_at: "2026-08-20T09:00:00Z".into(),
        exported_path: Some("/vault/Board.md".into()),
        ..Default::default()
    })
    .unwrap();

    let page = container_items_inner(&db, &HashSet::new(), None, ItemKind::Meeting, 0, 10).unwrap();
    let json = serde_json::to_string(&page).unwrap();
    assert!(!json.contains("/private/var"), "leaked an audio path: {json}");
    assert!(!json.contains(".wav"), "leaked an audio path: {json}");
    assert!(!json.contains("/vault/"), "leaked a vault path: {json}");

    let _ = std::fs::remove_file(&path);
}

// ── the tree's own rules ─────────────────────────────────────────────────────────────────────────

/// An empty type group is ABSENT, never a zero — so the UI's "hide an empty type" rule needs no
/// client-side filtering, and a container with only meetings shows exactly one group.
#[test]
fn an_empty_type_group_is_absent_rather_than_zero() {
    let (db, path) = fresh_db("empty-groups");
    container(&db, "p1", "Acme", "", None, "project");
    meeting_in(&db, "m1", "Standup", "2026-08-20T09:00:00Z", Some("p1"));

    let project = &tree(&db, &HashSet::new())[0];
    assert_eq!(
        project.groups.len(),
        1,
        "only the non-empty kind may appear: {:?}",
        project.groups
    );
    assert_eq!(project.groups[0].kind, ItemKind::Meeting);
    assert!(group(project, ItemKind::Note).is_none());
    // Tasks and dashboards are seams that return nothing yet; they must not appear as zeros either.
    assert!(group(project, ItemKind::Task).is_none());
    assert!(group(project, ItemKind::Dashboard).is_none());

    let _ = std::fs::remove_file(&path);
}

/// The tree carries only the newest few items per group, but reports the TRUE total, and the paged
/// reader reaches the rest.
#[test]
fn a_group_is_capped_while_its_total_is_the_real_count() {
    let (db, path) = fresh_db("capped");
    container(&db, "p1", "Acme", "", None, "project");
    for i in 0..12 {
        meeting_in(
            &db,
            &format!("m{i:02}"),
            &format!("Standup {i:02}"),
            &format!("2026-08-{:02}T09:00:00Z", i + 1),
            Some("p1"),
        );
    }

    let project = &tree(&db, &HashSet::new())[0];
    let meetings = group(project, ItemKind::Meeting).unwrap();
    assert_eq!(meetings.total, 12, "the total is the real count");
    assert_eq!(meetings.items.len(), 8, "the tree carries only a page");
    // Newest first: the last-dated meeting leads.
    assert_eq!(meetings.items[0].id, "m11");
    assert!(
        meetings.items[0].sort_at > meetings.items[1].sort_at,
        "rows are newest-first, and RFC3339 with fractional seconds still resolves to a real \
         epoch (SQLite's own strftime returns NULL for it, which would collapse every row to 0)"
    );

    let page = container_items_inner(&db, &HashSet::new(), Some("p1"), ItemKind::Meeting, 8, 10)
        .unwrap();
    assert_eq!(page.total, 12);
    assert_eq!(page.items.len(), 4, "the rest is reachable by paging");

    let _ = std::fs::remove_file(&path);
}

/// A recording's COMPANION note belongs to its meeting's row, not to the Notes group.
///
/// Without this every recording would be listed twice in the same container — once as its meeting
/// and once as the note the recording UI writes into.
#[test]
fn a_companion_note_is_not_listed_beside_its_meeting() {
    let (db, path) = fresh_db("companion");
    container(&db, "p1", "Acme", "", None, "project");
    meeting_in(&db, "m1", "Standup", "2026-08-20T09:00:00Z", Some("p1"));
    note_in(&db, "d-companion", "Standup", "p1", 1_700_000_000_000);
    db.set_document_meeting_id("d-companion", "m1").unwrap();
    note_in(&db, "d-standalone", "Research", "p1", 1_700_000_001_000);

    let project = &tree(&db, &HashSet::new())[0];
    let notes = group(project, ItemKind::Note).unwrap();
    assert_eq!(notes.total, 1, "only the standalone note counts: {notes:?}");
    assert_eq!(notes.items[0].id, "d-standalone");
    assert_eq!(group(project, ItemKind::Meeting).unwrap().total, 1);

    let _ = std::fs::remove_file(&path);
}

/// A system container never reaches the tree.
///
/// An in-flight feature adds `.murmur/tasks` (`kind='task'`, `parent_id IS NULL`) protected by
/// `RAISE(ABORT)` triggers. Surfacing it would put an internal folder in the user's sidebar.
#[test]
fn a_system_container_never_reaches_the_tree() {
    let (db, path) = fresh_db("system-folder");
    container(&db, "p1", "Acme", "", None, "project");
    container(&db, "sys", "tasks", ".murmur/tasks", None, "project");
    db.lock()
        .execute(
            "UPDATE folders SET kind = 'task' WHERE id = 'sys'",
            rusqlite::params![],
        )
        .unwrap();

    let forest = tree(&db, &HashSet::new());
    assert_eq!(forest.len(), 1, "only the user project: {forest:?}");
    assert_eq!(forest[0].id, "p1");
    assert!(db.list_containers().unwrap().iter().all(|c| c.id != "sys"));

    let _ = std::fs::remove_file(&path);
}

/// A meeting with no container lands in the Inbox; an authored note never can, because
/// `documents.folder_id` is NOT NULL.
#[test]
fn a_meeting_with_no_container_lands_in_the_inbox() {
    let (db, path) = fresh_db("inbox");
    container(&db, "p1", "Acme", "", None, "project");
    meeting_in(&db, "m-filed", "Filed", "2026-08-20T09:00:00Z", Some("p1"));
    meeting_in(&db, "m-loose", "Loose", "2026-08-21T09:00:00Z", None);
    note_in(&db, "d1", "Research", "p1", 1_700_000_000_000);

    let inbox = container_items_inner(&db, &HashSet::new(), None, ItemKind::Meeting, 0, 50).unwrap();
    assert_eq!(inbox.total, 1);
    assert_eq!(inbox.items[0].id, "m-loose");

    let inbox_notes =
        container_items_inner(&db, &HashSet::new(), None, ItemKind::Note, 0, 50).unwrap();
    assert_eq!(
        inbox_notes.total, 0,
        "an authored note always has a container, so the Inbox note leg is empty by construction"
    );

    // And the loose meeting is NOT attributed to the project.
    let project = &tree(&db, &HashSet::new())[0];
    assert_eq!(group(project, ItemKind::Meeting).unwrap().total, 1);
    assert_eq!(group(project, ItemKind::Meeting).unwrap().items[0].id, "m-filed");

    let _ = std::fs::remove_file(&path);
}

/// The tree renders whatever depth already exists rather than silently dropping rows, and a
/// corrupted parent cycle terminates instead of hanging.
#[test]
fn existing_depth_is_rendered_and_a_cycle_terminates() {
    let (db, path) = fresh_db("depth");
    container(&db, "p1", "Acme", "", None, "project");
    container(&db, "f1", "Notes", "Notes", Some("p1"), "folder");
    container(&db, "f2", "Research", "Notes/Research", Some("f1"), "folder");

    let project = &tree(&db, &HashSet::new())[0];
    assert_eq!(project.folders.len(), 1);
    assert_eq!(project.folders[0].id, "f1");
    assert_eq!(project.folders[0].folders[0].id, "f2");

    // A cycle must not hang the reader.
    db.lock()
        .execute(
            "UPDATE folders SET parent_id = 'f2' WHERE id = 'f1'",
            rusqlite::params![],
        )
        .unwrap();
    let _ = tree(&db, &HashSet::new());

    let _ = std::fs::remove_file(&path);
}

/// A container resolves on its own, with its owning project's name for a breadcrumb.
#[test]
fn a_container_resolves_with_its_parent_name() {
    let (db, path) = fresh_db("breadcrumb");
    container(&db, "p1", "Acme", "", None, "project");
    container(&db, "f1", "Legal", "Legal", Some("p1"), "folder");

    let dto = get_container_inner(&db, &HashSet::new(), "f1").unwrap().unwrap();
    assert_eq!(dto.level, "folder");
    assert_eq!(dto.parent_name.as_deref(), Some("Acme"));
    let dto = get_container_inner(&db, &HashSet::new(), "p1").unwrap().unwrap();
    assert_eq!(dto.level, "project");
    assert!(dto.parent_name.is_none());
    assert!(get_container_inner(&db, &HashSet::new(), "nope")
        .unwrap()
        .is_none());

    let _ = std::fs::remove_file(&path);
}

// ── the legacy sidebar keeps working ─────────────────────────────────────────────────────────────

/// The legacy folder tree hides project rows and re-roots their children.
///
/// Without this the shipped sidebar would render ONE root — the project — with every folder beneath
/// it, and `MeetingsSidebarTreeComponent` filters note-kind folders at the TOP LEVEL ONLY, so every
/// note folder would leak into the Meetings tree. That is verbatim the folder leak fixed on
/// 2026-07-14.
#[test]
fn the_legacy_folder_tree_hides_projects_and_reroots_their_children() {
    let (db, path) = fresh_db("legacy-shim");
    container(&db, "p1", "Acme", "", None, "project");
    container(&db, "f1", "Legal", "Legal", Some("p1"), "folder");
    container(&db, "f2", "Notes", "Notes", Some("p1"), "folder");
    db.lock()
        .execute(
            "UPDATE folders SET kind = 'note' WHERE id = 'f2'",
            rusqlite::params![],
        )
        .unwrap();

    let folders = db.list_folders().unwrap();
    let levels = db.folder_levels().unwrap();
    let flattened = flatten_projects_for_legacy_tree(folders, &levels);
    let counts = std::collections::HashMap::new();
    let unlocked = HashSet::new();
    let kinds = db.folder_kinds().unwrap();
    let legacy = build_folder_tree(&flattened, &counts, &unlocked, &kinds);

    let roots: Vec<&str> = legacy.iter().map(|n| n.id.as_str()).collect();
    assert_eq!(
        roots,
        vec!["f1", "f2"],
        "both former roots stay roots, and the project is not one of them"
    );
    assert!(
        legacy.iter().all(|n| n.children.is_empty()),
        "no folder gained a child"
    );
    // The shipped Meetings tree filters note-kind folders at the top level; that filter still sees
    // the note folder AS a root, which is the only place it looks.
    assert_eq!(
        legacy.iter().filter(|n| n.kind != "note").count(),
        1,
        "the note folder is still filterable at the top level"
    );

    let _ = std::fs::remove_file(&path);
}

// ── the schema itself ────────────────────────────────────────────────────────────────────────────

/// The new folder columns are additive, defaulted, and survive a second `migrate()` unchanged.
///
/// Real user databases exist with sealed folders and wrapped keys; a migration that is not
/// idempotent, or that touches `locked`/`wrapped_key`/`path`, is unrecoverable.
#[test]
fn the_new_folder_columns_are_additive_and_idempotent() {
    let (db, path) = fresh_db("idempotent");
    container(&db, "f1", "Legal", "Legal", None, "folder");
    seal(&db, "f1");
    db.lock()
        .execute(
            "UPDATE folders SET wrapped_key = X'0102', emoji = '🗂', tint = '#abc', position = 7
              WHERE id = 'f1'",
            rusqlite::params![],
        )
        .unwrap();

    let before: (String, i64, Option<Vec<u8>>, Option<String>, i64) = db
        .lock()
        .query_row(
            "SELECT path, locked, wrapped_key, emoji, position FROM folders WHERE id = 'f1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();

    // Re-opening runs migrate() again on an already-migrated database.
    drop(db);
    let db = Db::open_with_key(&path, TEST_DEK).unwrap();

    let after: (String, i64, Option<Vec<u8>>, Option<String>, i64) = db
        .lock()
        .query_row(
            "SELECT path, locked, wrapped_key, emoji, position FROM folders WHERE id = 'f1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    assert_eq!(before, after, "a second migrate() changed a folder row");

    // A row that predates the columns reads back with the documented defaults.
    container(&db, "f2", "New", "New", None, "folder");
    let rows: Vec<ContainerRow> = db.list_containers().unwrap();
    let fresh = rows.iter().find(|c| c.id == "f2").unwrap();
    assert_eq!(fresh.level, "folder", "the default level is 'folder'");
    assert_eq!(fresh.position, 0);
    assert!(fresh.emoji.is_none());

    let _ = std::fs::remove_file(&path);
}

/// The container level is constrained by the DATABASE, not by convention.
///
/// Without the CHECK, any string persists: `list_containers` would accept it and the tree would
/// render rows at a level nothing defines, leaving every later consumer to defend against it.
#[test]
fn an_undefined_container_level_cannot_be_persisted() {
    let (db, path) = fresh_db("level-check");
    container(&db, "f1", "Legal", "Legal", None, "folder");

    let err = db.lock().execute(
        "UPDATE folders SET level = 'workspace' WHERE id = 'f1'",
        rusqlite::params![],
    );
    assert!(
        err.is_err(),
        "an undefined level must be refused by the schema, not tolerated by readers"
    );
    // Both defined values remain writable.
    for level in ["project", "folder"] {
        db.lock()
            .execute(
                "UPDATE folders SET level = ?2 WHERE id = ?1",
                rusqlite::params!["f1", level],
            )
            .unwrap();
    }
    // And a freshly inserted row still takes the default without naming it.
    container(&db, "f2", "Ops", "Ops", None, "folder");
    let rows = db.list_containers().unwrap();
    assert_eq!(rows.iter().find(|c| c.id == "f2").unwrap().level, "folder");

    let _ = std::fs::remove_file(&path);
}

/// The paged reader applies the SAME system-container exclusion as the tree.
///
/// Two sinks over the same rows that disagree are how a deliberately hidden container becomes
/// reachable through the back door: the tree refuses to render `.murmur/…`, so the page must refuse
/// to serve it rather than resolving it by id.
#[test]
fn the_paged_reader_refuses_a_system_container() {
    let (db, path) = fresh_db("system-page");
    container(&db, "sys", "tasks", ".murmur/tasks", None, "folder");
    db.lock()
        .execute(
            "UPDATE folders SET kind = 'task' WHERE id = 'sys'",
            rusqlite::params![],
        )
        .unwrap();

    let err = container_items_inner(&db, &HashSet::new(), Some("sys"), ItemKind::Note, 0, 10)
        .expect_err("a system container is not addressable through the paged reader");
    assert!(
        matches!(err, AppError::Locked(_)),
        "fail closed, and indistinguishable from a sealed container: {err:?}"
    );

    let _ = std::fs::remove_file(&path);
}

/// The tree's per-group ordering is stated by the query, not inherited from the engine's incidental
/// row emission order.
#[test]
fn group_items_are_ordered_newest_first_by_the_query() {
    let (db, path) = fresh_db("group-order");
    container(&db, "p1", "Acme", "", None, "project");
    // Inserted OUT of chronological order so a pass cannot come from insertion order.
    for (id, day) in [("m-c", 3), ("m-a", 1), ("m-e", 5), ("m-b", 2), ("m-d", 4)] {
        meeting_in(
            &db,
            id,
            id,
            &format!("2026-08-0{day}T09:00:00Z"),
            Some("p1"),
        );
    }

    let project = &tree(&db, &HashSet::new())[0];
    let ids: Vec<&str> = group(project, ItemKind::Meeting)
        .unwrap()
        .items
        .iter()
        .map(|i| i.id.as_str())
        .collect();
    assert_eq!(ids, vec!["m-e", "m-d", "m-c", "m-b", "m-a"]);

    let _ = std::fs::remove_file(&path);
}

/// A kind whose leg is still a seam returns a truthful EMPTY page, not a database error.
///
/// The seam ignores the scope and emits no container placeholder, so binding one anyway hands
/// SQLite a parameter count its statement never declared. The page statement survived that by
/// accident (its LIMIT/OFFSET made the counts balance); the COUNT twin did not.
#[test]
fn a_seam_kind_returns_an_empty_page_rather_than_an_error() {
    let (db, path) = fresh_db("seam-page");
    container(&db, "p1", "Acme", "", None, "project");

    for kind in [ItemKind::Task, ItemKind::Dashboard] {
        // Addressed at a real container...
        let page = container_items_inner(&db, &HashSet::new(), Some("p1"), kind, 0, 10)
            .unwrap_or_else(|e| panic!("{kind:?} page at a container must not error: {e:?}"));
        assert_eq!(page.total, 0);
        assert!(page.items.is_empty());
        assert_eq!(page.kind, kind);
        // ...and at the Inbox.
        let page = container_items_inner(&db, &HashSet::new(), None, kind, 0, 10)
            .unwrap_or_else(|e| panic!("{kind:?} page at the Inbox must not error: {e:?}"));
        assert_eq!(page.total, 0);
    }

    let _ = std::fs::remove_file(&path);
}

/// A meeting whose provider note rows span two containers is attributed to one the viewer can SEE.
///
/// The meeting is visible as soon as ONE of its containers is readable. Attributing it to the
/// unreadable one would place it in a container whose groups are suppressed — so it would appear in
/// neither container and be reachable through nothing, while still existing. That hides rather than
/// discloses, which is why it needs its own oracle: no leak test would ever catch it.
#[test]
fn a_meeting_spanning_two_containers_is_attributed_to_a_visible_one() {
    let (db, path) = fresh_db("split-meeting");
    container(&db, "p1", "Acme", "", None, "project");
    // `f-a-sealed` sorts FIRST, so a purely lexicographic pick would choose the sealed container.
    container(&db, "f-a-sealed", "Legal", "Legal", Some("p1"), "folder");
    container(&db, "f-b-open", "Ops", "Ops", Some("p1"), "folder");

    meeting_in(&db, "m1", "Board strategy", "2026-08-20T09:00:00Z", Some("f-b-open"));
    // A second provider row for the SAME meeting, filed in the other container — the shape a
    // re-summarize produces.
    db.upsert_note(&NoteRecord {
        meeting_id: "m1".to_string(),
        provider_id: "ollama".to_string(),
        markdown: "# Board strategy (ollama)".to_string(),
        created_at: "2026-08-20T09:05:00Z".to_string(),
        ..Default::default()
    })
    .unwrap();
    db.lock()
        .execute(
            "UPDATE notes SET folder_id = 'f-a-sealed' WHERE meeting_id = 'm1' AND provider_id = 'ollama'",
            rusqlite::params![],
        )
        .unwrap();
    seal(&db, "f-a-sealed");

    let project = &tree(&db, &HashSet::new())[0];
    let open = project
        .folders
        .iter()
        .find(|f| f.id == "f-b-open")
        .expect("the open folder is in the tree");
    let sealed = project
        .folders
        .iter()
        .find(|f| f.id == "f-a-sealed")
        .expect("the sealed folder is still listed by name");

    assert!(sealed.groups.is_empty(), "the sealed container stays silent");
    let meetings = group(open, ItemKind::Meeting)
        .expect("the meeting must be reachable in the container the viewer can see");
    assert_eq!(meetings.total, 1);
    assert_eq!(meetings.items[0].id, "m1");

    // And the paged reader agrees with the tree.
    let page =
        container_items_inner(&db, &HashSet::new(), Some("f-b-open"), ItemKind::Meeting, 0, 10)
            .unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].id, "m1");

    let _ = std::fs::remove_file(&path);
}

/// The breadcrumb sink answers for a SEALED container, and what it may answer with is pinned.
///
/// This is the one sink whose sealed branch is a deliberate disclosure: the container's NAME is what
/// a user needs in order to unlock it, and the shipped folder tree already returns locked folder
/// names. Because it is deliberate, it needs a negative-path oracle MORE than the refusing sinks do
/// — the exact key set is asserted so that adding any field to `ContainerDto` (a path, an item
/// count, a preview) fails here and forces that decision to be made again rather than inherited.
#[test]
fn the_breadcrumb_sink_discloses_a_name_and_nothing_else_for_a_sealed_container() {
    let (db, path) = fresh_db("sealed-breadcrumb");
    container(&db, "p1", "Acme", "", None, "project");
    container(&db, "f1", "Legal", "Legal", Some("p1"), "folder");
    meeting_in(&db, "m1", "Board strategy", "2026-08-20T09:00:00Z", Some("f1"));
    note_in(&db, "d1", "Merger terms", "f1", 1_700_000_000_000);
    seal(&db, "f1");

    let dto = get_container_inner(&db, &HashSet::new(), "f1")
        .unwrap()
        .expect("a sealed container still resolves — its name is how the user reaches the unlock");
    assert!(dto.locked, "it reports itself sealed");
    assert!(!dto.unlocked, "and not session-unlocked");
    assert_eq!(dto.name, "Legal", "the name is the deliberate disclosure");
    assert_eq!(dto.parent_name.as_deref(), Some("Acme"));

    // Nothing about the CONTENTS may ride along, now or after a later field is added.
    let json = serde_json::to_value(&dto).unwrap();
    let mut keys: Vec<&str> = json.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "emoji", "id", "isRoot", "level", "locked", "name", "parentId", "parentName", "tint",
            "unlocked",
        ],
        "ContainerDto gained a field: decide explicitly whether a SEALED container may disclose it"
    );
    let raw = serde_json::to_string(&json).unwrap();
    assert!(!raw.contains("Board strategy"), "leaked a meeting title: {raw}");
    assert!(!raw.contains("Merger terms"), "leaked a note title: {raw}");

    // Session-unlocking flips only the session flag.
    let unlocked: HashSet<String> = ["f1".to_string()].into_iter().collect();
    let dto = get_container_inner(&db, &unlocked, "f1").unwrap().unwrap();
    assert!(dto.locked && dto.unlocked);

    let _ = std::fs::remove_file(&path);
}

/// A meeting with NO note rows is visible and lands in the Inbox with its real title.
///
/// This is the shipped `list_meetings_visible` predicate verbatim — a meeting is hidden only when
/// EVERY note it has is sealed-and-not-unlocked, so one with none at all is not hidden — and it
/// follows from deriving a meeting's container from its note rows. It is also the only path on which
/// a meeting reaches a reader without passing a folder gate, so it is pinned rather than left
/// implicit: such a meeting has no container, therefore no container can seal it, and it is exactly
/// the pre-summarize or errored recording the Inbox exists to hold.
#[test]
fn a_note_less_meeting_is_visible_in_the_inbox() {
    let (db, path) = fresh_db("note-less");
    container(&db, "p1", "Acme", "", None, "project");
    db.insert_meeting(&Meeting {
        id: "m-raw".into(),
        started_at: "2026-08-21T09:00:00Z".into(),
        ended_at: None,
        title: Some("Recording in progress".into()),
        duration_s: 42,
        audio_path: None,
        status: MeetingStatus::Recording,
        folder_id: None,
    })
    .unwrap();

    let inbox = container_items_inner(&db, &HashSet::new(), None, ItemKind::Meeting, 0, 50).unwrap();
    assert_eq!(inbox.total, 1);
    assert_eq!(inbox.items[0].id, "m-raw");
    assert_eq!(
        inbox.items[0].title.as_deref(),
        Some("Recording in progress")
    );

    // It belongs to no container, so no container seal can reach it — and it is absent from every
    // container group rather than being attributed to one.
    let project = &tree(&db, &HashSet::new())[0];
    assert!(group(project, ItemKind::Meeting).is_none());

    let _ = std::fs::remove_file(&path);
}

/// A meeting is never attributed to a container the tree refuses to render.
///
/// Attribution and rendering must agree. If a note filed into a machine-owned container could
/// capture the attribution of a meeting whose other note sits in a real one, the meeting would show
/// up in NO container while still existing — unreachable rather than leaked, which is why no leak
/// oracle would ever catch it. Both now resolve through one shared renderable-container predicate.
#[test]
fn a_system_container_cannot_capture_a_meeting_attribution() {
    let (db, path) = fresh_db("system-attribution");
    container(&db, "p1", "Acme", "", None, "project");
    container(&db, "f-user", "Ops", "Ops", Some("p1"), "folder");
    // Sorts BEFORE "f-user", so a pick that ignored the container kind would choose it.
    container(&db, ".sys", "tasks", ".murmur/tasks", None, "folder");
    db.lock()
        .execute(
            "UPDATE folders SET kind = 'task' WHERE id = '.sys'",
            rusqlite::params![],
        )
        .unwrap();

    meeting_in(&db, "m1", "Board strategy", "2026-08-20T09:00:00Z", Some("f-user"));
    db.upsert_note(&NoteRecord {
        meeting_id: "m1".to_string(),
        provider_id: "ollama".to_string(),
        markdown: "# Board strategy (ollama)".to_string(),
        created_at: "2026-08-20T09:05:00Z".to_string(),
        ..Default::default()
    })
    .unwrap();
    db.lock()
        .execute(
            "UPDATE notes SET folder_id = '.sys' WHERE meeting_id = 'm1' AND provider_id = 'ollama'",
            rusqlite::params![],
        )
        .unwrap();

    let project = &tree(&db, &HashSet::new())[0];
    let user = project
        .folders
        .iter()
        .find(|f| f.id == "f-user")
        .expect("the user container is in the tree");
    let meetings = group(user, ItemKind::Meeting)
        .expect("the meeting stays reachable in the container the tree actually renders");
    assert_eq!(meetings.total, 1);
    assert_eq!(meetings.items[0].id, "m1");

    let page = container_items_inner(&db, &HashSet::new(), Some("f-user"), ItemKind::Meeting, 0, 10)
        .unwrap();
    assert_eq!(page.total, 1, "the paged reader agrees with the tree");

    let _ = std::fs::remove_file(&path);
}

/// A meeting filed into a SEALED container does not fall through into the Inbox.
///
/// Worth its own oracle because the mechanism that normally keeps a filed meeting out of the Inbox
/// — a non-NULL attribution — is deliberately disabled for a sealed container: attribution is
/// itself gated, so it resolves to NULL there. Exclusion then rests entirely on the outer
/// visibility conjunct. This pins that the remaining guard is the one doing the work.
#[test]
fn a_meeting_in_a_sealed_container_does_not_fall_into_the_inbox() {
    let (db, path) = fresh_db("sealed-not-inbox");
    container(&db, "p1", "Acme", "", None, "project");
    container(&db, "f1", "Legal", "Legal", Some("p1"), "folder");
    meeting_in(&db, "m1", "Board strategy", "2026-08-20T09:00:00Z", Some("f1"));
    seal(&db, "f1");

    let inbox = container_items_inner(&db, &HashSet::new(), None, ItemKind::Meeting, 0, 50).unwrap();
    assert_eq!(
        inbox.total, 0,
        "a sealed container's meeting must not reappear as unfiled: {:?}",
        inbox.items
    );
    let raw = serde_json::to_string(&inbox).unwrap();
    assert!(!raw.contains("Board strategy"), "leaked into the Inbox: {raw}");

    // Session-unlocking restores it to its own container, still not to the Inbox.
    let unlocked: HashSet<String> = ["f1".to_string()].into_iter().collect();
    let inbox = container_items_inner(&db, &unlocked, None, ItemKind::Meeting, 0, 50).unwrap();
    assert_eq!(inbox.total, 0);
    let page =
        container_items_inner(&db, &unlocked, Some("f1"), ItemKind::Meeting, 0, 50).unwrap();
    assert_eq!(page.total, 1);

    let _ = std::fs::remove_file(&path);
}

/// The tree node's key set is pinned for a SEALED container, like the breadcrumb DTO's.
///
/// `ContainerNode` is returned for a sealed container too (carrying its name, so the user can reach
/// the unlock). Asserting only that seeded content strings are absent would let a LATER field —
/// a path, an item count, a preview — be disclosed with the whole suite still green.
#[test]
fn the_tree_node_key_set_is_pinned_for_a_sealed_container() {
    let (db, path) = fresh_db("node-keys");
    container(&db, "p1", "Acme", "", None, "project");
    container(&db, "f1", "Legal", "Legal", Some("p1"), "folder");
    meeting_in(&db, "m1", "Board strategy", "2026-08-20T09:00:00Z", Some("f1"));
    seal(&db, "f1");

    let forest = tree(&db, &HashSet::new());
    let node = serde_json::to_value(&forest[0].folders[0]).unwrap();
    let mut keys: Vec<&str> = node.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "emoji", "folders", "groups", "id", "isRoot", "kind", "level", "locked", "name",
            "tint", "unlocked",
        ],
        "ContainerNode gained a field: decide explicitly whether a SEALED container may disclose it"
    );
    assert!(node["groups"].as_array().unwrap().is_empty());

    let _ = std::fs::remove_file(&path);
}

/// A recording can be filed BEFORE its generated note exists, and the later note inherits exactly
/// that canonical placement.
#[test]
fn a_pre_note_meeting_is_filed_and_its_later_note_inherits_the_folder() {
    let (db, path) = fresh_db("canonical-folder-id");
    container(&db, "f1", "Legal", "Legal", None, "folder");
    db.insert_meeting(&Meeting {
        id: "m1".into(),
        started_at: "2026-08-21T09:00:00Z".into(),
        ended_at: None,
        title: Some("Raw".into()),
        duration_s: 1,
        audio_path: None,
        folder_id: Some("f1".into()),
        status: MeetingStatus::Recording,
    })
    .unwrap();

    assert_eq!(db.folder_for_meeting("m1").unwrap().as_deref(), Some("f1"));

    let inbox = container_items_inner(&db, &HashSet::new(), None, ItemKind::Meeting, 0, 50).unwrap();
    assert_eq!(inbox.total, 0, "the pre-note recording must leave Unfiled");
    let filed = container_items_inner(
        &db,
        &HashSet::new(),
        Some("f1"),
        ItemKind::Meeting,
        0,
        50,
    )
    .unwrap();
    assert_eq!(filed.total, 1);

    db.upsert_note(&NoteRecord {
        meeting_id: "m1".into(),
        provider_id: "claude_code".into(),
        markdown: "# Ready".into(),
        created_at: "2026-08-21T09:01:00Z".into(),
        ..Default::default()
    })
    .unwrap();
    let inherited: Option<String> = db
        .lock()
        .query_row(
            "SELECT folder_id FROM notes WHERE meeting_id='m1' AND provider_id='claude_code'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(inherited.as_deref(), Some("f1"));

    let _ = std::fs::remove_file(&path);
}

/// Canonical placement covers pre-note recordings across the read tree, seal enumeration, and
/// audio retention. A locked recording with no provider note must not leak into Unfiled or be
/// offered to the audio pruner merely because the old note-derived join is empty.
#[test]
fn a_locked_pre_note_meeting_is_hidden_enumerated_for_seal_and_not_audio_prunable() {
    let (db, path) = fresh_db("locked-pre-note");
    container(&db, "f1", "Secret", "Secret", None, "folder");
    seal(&db, "f1");
    db.insert_meeting(&Meeting {
        id: "m1".into(),
        started_at: "2026-08-21T09:00:00Z".into(),
        ended_at: None,
        title: Some("Hidden raw".into()),
        duration_s: 1,
        audio_path: Some("/tmp/hidden.wav".into()),
        status: MeetingStatus::Recording,
        folder_id: Some("f1".into()),
    })
    .unwrap();

    assert!(db.list_meetings_visible(50, &HashSet::new(), None).unwrap().is_empty());
    assert_eq!(
        container_items_inner(&db, &HashSet::new(), None, ItemKind::Meeting, 0, 50)
            .unwrap()
            .total,
        0
    );
    assert_eq!(db.meeting_ids_in_folder("f1").unwrap(), vec!["m1"]);
    assert!(
        db.prunable_audio_candidates()
            .unwrap()
            .iter()
            .all(|row| row.meeting_id != "m1")
    );
    let _ = std::fs::remove_file(&path);
}

/// Nullable canonical ownership is read as `None`, not as a rusqlite type error, for an attachment
/// owned by a genuinely unfiled pre-note recording.
#[test]
fn an_unfiled_meeting_attachment_owner_has_no_folder_without_error() {
    let (db, path) = fresh_db("attachment-none");
    db.insert_meeting(&Meeting {
        id: "m1".into(),
        started_at: "2026-08-21T09:00:00Z".into(),
        ended_at: None,
        title: Some("Unfiled".into()),
        duration_s: 1,
        audio_path: None,
        status: MeetingStatus::Recording,
        folder_id: None,
    })
    .unwrap();
    let owner = crate::storage::AttachmentOwner::Meeting {
        meeting_id: "m1".into(),
        provider_id: "claude_code".into(),
    };
    assert_eq!(db.folder_for_attachment_owner(&owner).unwrap(), None);
    let _ = std::fs::remove_file(&path);
}

/// On a database as it exists TODAY, the legacy folder tree is byte-identical to what it was before
/// this change.
///
/// This is the central claim of this step — purely additive, no shipped behaviour changes — and it
/// was the one claim with no oracle. No row carries `level='project'` until the separate hierarchy
/// migration runs, so the shim must be the IDENTITY on every such database: same rows, same order,
/// same parents, and therefore the same serialized tree the builder produced before the shim
/// existed.
#[test]
fn the_legacy_folder_tree_is_unchanged_on_a_pre_migration_database() {
    let (db, path) = fresh_db("legacy-identity");
    // A shape with everything the shipped sidebar cares about: both kinds, a sealed folder, an
    // is_root note home, and real nesting.
    container(&db, "f-meet", "Work", "Work", None, "folder");
    container(&db, "f-notes", "Notes", "Notes", None, "folder");
    container(&db, "f-child", "Research", "Notes/Research", Some("f-notes"), "folder");
    container(&db, "f-locked", "Legal", "Legal", None, "folder");
    db.lock()
        .execute(
            "UPDATE folders SET kind = 'note', is_root = 1 WHERE id = 'f-notes'",
            rusqlite::params![],
        )
        .unwrap();
    db.lock()
        .execute(
            "UPDATE folders SET kind = 'note' WHERE id = 'f-child'",
            rusqlite::params![],
        )
        .unwrap();
    seal(&db, "f-locked");
    meeting_in(&db, "m1", "Standup", "2026-08-20T09:00:00Z", Some("f-meet"));

    let folders = db.list_folders().unwrap();
    let levels = db.folder_levels().unwrap();
    assert!(
        levels.values().all(|l| l == "folder"),
        "a pre-migration database has no project rows: {levels:?}"
    );

    let counts = db.count_notes_per_folder(&HashSet::new()).unwrap();
    let kinds = db.folder_kinds().unwrap();

    // What the builder produced BEFORE the shim existed: the raw folder list, unfiltered.
    let before = build_folder_tree(&folders, &counts, &HashSet::new(), &kinds);
    // What it produces now: the same list through the shim.
    let flattened = flatten_projects_for_legacy_tree(folders.clone(), &levels);
    let after = build_folder_tree(&flattened, &counts, &HashSet::new(), &kinds);

    assert_eq!(
        flattened.len(),
        folders.len(),
        "the shim dropped a row on a database with no projects"
    );
    assert_eq!(
        serde_json::to_string(&before).unwrap(),
        serde_json::to_string(&after).unwrap(),
        "the legacy folder tree changed on a pre-migration database"
    );

    let _ = std::fs::remove_file(&path);
}

// ── the hierarchy migration ──────────────────────────────────────────────────────────────────────
//
// `migrate()` adopts a database the moment it is opened, so a PRE-migration state is modelled by
// removing the project it created and then driving `migrate_hierarchy_v1` directly. That is the same
// entry point `migrate()` calls, so these exercise the real migration rather than a re-statement.

/// A database in the shape it had before the hierarchy existed: containers, no project. Identical to
/// [`fresh_db`], named separately so the migration tests read as what they are.
fn pre_migration_db(label: &str) -> (Db, std::path::PathBuf) {
    fresh_db(label)
}

fn run_migration(db: &Db) {
    Db::migrate_hierarchy_v1(&db.lock()).unwrap();
}

/// (id, path, parent_id, level, locked, wrapped_key) — the columns a migration must be able to
/// prove it did or did not change.
type FolderRow = (String, String, Option<String>, String, i64, Option<Vec<u8>>);

fn folder_rows(db: &Db) -> Vec<FolderRow> {
    let conn = db.lock();
    let mut stmt = conn
        .prepare(
            "SELECT id, path, parent_id, COALESCE(level, 'folder'), locked, wrapped_key
               FROM folders ORDER BY id",
        )
        .unwrap();
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })
        .unwrap();
    rows.map(|r| r.unwrap()).collect()
}

/// Every existing container is adopted, and note containers land under the note root so that path
/// composition agrees with what is already on disk.
#[test]
fn the_migration_adopts_every_existing_container() {
    let (db, path) = pre_migration_db("adopt");
    container(&db, "f-work", "Work", "Work", None, "folder");
    container(&db, "f-notes", "Notes", "Notes", None, "folder");
    container(&db, "f-research", "Research", "Notes/Research", None, "folder");
    // `execute` runs only the FIRST statement of a batch — use `execute_batch` for both.
    db.lock()
        .execute_batch(
            "UPDATE folders SET kind = 'note' WHERE id IN ('f-notes', 'f-research');
             UPDATE folders SET is_root = 1 WHERE id = 'f-notes';",
        )
        .unwrap();

    run_migration(&db);

    let containers = db.list_containers().unwrap();
    let project = containers
        .iter()
        .find(|c| c.level == "project")
        .expect("a default project exists");
    assert!(project.parent_id.is_none());

    let by_id = |id: &str| containers.iter().find(|c| c.id == id).unwrap().clone();
    assert_eq!(by_id("f-work").parent_id.as_deref(), Some(project.id.as_str()));
    assert_eq!(by_id("f-notes").parent_id.as_deref(), Some(project.id.as_str()));
    assert_eq!(
        by_id("f-research").parent_id.as_deref(),
        Some("f-notes"),
        "a note container is adopted under the note root, so parent.path + name == its own path"
    );
    assert!(by_id("f-notes").is_root, "the note root keeps its flag");
    for id in ["f-work", "f-notes", "f-research"] {
        assert_eq!(by_id(id).level, "folder");
    }

    // And the tree now renders as a single forest rooted at the project.
    let forest = tree(&db, &HashSet::new());
    assert_eq!(forest.len(), 1);
    assert_eq!(forest[0].id, project.id);

    let _ = std::fs::remove_file(&path);
}

/// The migration moves nothing on disk: no `path` and no `exported_path` changes.
///
/// This is the property the whole design exists to preserve. `folders.path` is a real vault
/// directory and `notes.exported_path` is where `lock_folder` goes to delete a plaintext `.md`; a
/// rewritten path with an unmoved file (or the reverse) is the NOTES-2 sealed-content leak.
#[test]
fn the_migration_moves_no_path_and_no_export() {
    let (db, path) = pre_migration_db("no-move");
    container(&db, "f-work", "Work", "Work", None, "folder");
    container(&db, "f-pl", "Sprzedaż", "Sprzedaż", None, "folder");
    meeting_in(&db, "m1", "Standup", "2026-08-20T09:00:00Z", Some("f-work"));
    db.lock()
        .execute(
            "UPDATE notes SET exported_path = '/vault/Work/Standup.md' WHERE meeting_id = 'm1'",
            rusqlite::params![],
        )
        .unwrap();
    note_in(&db, "d1", "Oferta", "f-pl", 1_700_000_000_000);
    db.lock()
        .execute(
            "UPDATE documents SET exported_path = '/vault/Sprzedaż/Oferta.md' WHERE id = 'd1'",
            rusqlite::params![],
        )
        .unwrap();

    let paths_before: Vec<(String, String)> = folder_rows(&db)
        .into_iter()
        .map(|(id, p, _, _, _, _)| (id, p))
        .collect();
    let exports_before: Vec<String> = {
        let conn = db.lock();
        let mut stmt = conn
            .prepare(
                "SELECT exported_path FROM notes WHERE exported_path IS NOT NULL
                 UNION ALL
                 SELECT exported_path FROM documents WHERE exported_path IS NOT NULL",
            )
            .unwrap();
        let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
        let mut v: Vec<String> = rows.map(|r| r.unwrap()).collect();
        v.sort();
        v
    };

    run_migration(&db);

    let paths_after: Vec<(String, String)> = folder_rows(&db)
        .into_iter()
        .filter(|(_, _, _, level, _, _)| level != "project")
        .map(|(id, p, _, _, _, _)| (id, p))
        .collect();
    assert_eq!(
        paths_before, paths_after,
        "a container path changed — the vault directory it names did not"
    );

    let exports_after: Vec<String> = {
        let conn = db.lock();
        let mut stmt = conn
            .prepare(
                "SELECT exported_path FROM notes WHERE exported_path IS NOT NULL
                 UNION ALL
                 SELECT exported_path FROM documents WHERE exported_path IS NOT NULL",
            )
            .unwrap();
        let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
        let mut v: Vec<String> = rows.map(|r| r.unwrap()).collect();
        v.sort();
        v
    };
    assert_eq!(
        exports_before, exports_after,
        "an exported .md path changed — the seal would delete nothing there"
    );

    // The project itself sits at the vault root, which is what makes all of the above possible.
    let project = db
        .list_containers()
        .unwrap()
        .into_iter()
        .find(|c| c.level == "project")
        .unwrap();
    let project_path: String = db
        .lock()
        .query_row(
            "SELECT path FROM folders WHERE id = ?1",
            rusqlite::params![project.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(project_path, "", "the default project IS the vault root");

    let _ = std::fs::remove_file(&path);
}

/// Running it again changes nothing — including after a full re-open, which is how it runs for real.
#[test]
fn the_migration_is_idempotent() {
    let (db, path) = pre_migration_db("idempotent-migration");
    container(&db, "f-work", "Work", "Work", None, "folder");
    container(&db, "f-notes", "Notes", "Notes", None, "folder");
    db.lock()
        .execute(
            "UPDATE folders SET kind = 'note', is_root = 1 WHERE id = 'f-notes'",
            rusqlite::params![],
        )
        .unwrap();

    run_migration(&db);
    let once = folder_rows(&db);
    run_migration(&db);
    assert_eq!(once, folder_rows(&db), "a second run changed a row");

    // Re-opening runs the whole of migrate() again on an adopted database.
    drop(db);
    let db = Db::open_with_key(&path, TEST_DEK).unwrap();
    assert_eq!(once, folder_rows(&db), "re-opening changed a row");
    assert_eq!(
        db.list_containers()
            .unwrap()
            .iter()
            .filter(|c| c.level == "project")
            .count(),
        1,
        "a second project was created"
    );

    let _ = std::fs::remove_file(&path);
}

/// User-created Spaces are peer project roots. Re-running migration or reopening must not demote
/// one under the other, otherwise only the first Space survives the hierarchy.
#[test]
fn reopening_preserves_multiple_peer_spaces() {
    let (db, path) = pre_migration_db("multiple-spaces");
    container(&db, "p-work", "Work", "Work", None, "project");
    container(&db, "p-home", "Home", "Home", None, "project");
    container(&db, "f-home", "Plans", "Home/Plans", Some("p-home"), "folder");
    drop(db);

    let db = Db::open_with_key(&path, TEST_DEK).unwrap();
    let projects: Vec<_> = db
        .list_containers()
        .unwrap()
        .into_iter()
        .filter(|row| row.level == "project")
        .collect();
    assert_eq!(
        projects.len(),
        3,
        "the vault-root Workspace plus both user Spaces must survive"
    );
    assert!(projects.iter().any(|row| row.id == "p-work"));
    assert!(projects.iter().any(|row| row.id == "p-home"));
    assert!(projects.iter().all(|row| row.parent_id.is_none()));
    assert_eq!(
        db.list_containers()
            .unwrap()
            .into_iter()
            .find(|row| row.id == "f-home")
            .unwrap()
            .parent_id
            .as_deref(),
        Some("p-home")
    );
    let _ = std::fs::remove_file(&path);
}

/// Legacy provider rows with one unambiguous non-null folder become one canonical meeting placement,
/// and every sibling provider row is normalized in the same migration pass. Leaving a NULL sibling
/// beside a canonical locked folder would expose that note through note-level readers.
#[test]
fn migration_backfills_one_folder_and_normalizes_null_provider_siblings() {
    let (db, path) = fresh_db("canonical-backfill");
    container(&db, "f1", "Secret", "Secret", None, "folder");
    db.insert_meeting(&Meeting {
        id: "m1".into(),
        started_at: "2026-08-20T09:00:00Z".into(),
        ended_at: None,
        title: Some("Legacy".into()),
        duration_s: 1,
        audio_path: None,
        status: MeetingStatus::Summarized,
        folder_id: None,
    })
    .unwrap();
    for provider in ["a", "b"] {
        db.upsert_note(&NoteRecord {
            meeting_id: "m1".into(),
            provider_id: provider.into(),
            markdown: provider.into(),
            created_at: "2026-08-20T09:01:00Z".into(),
            ..Default::default()
        })
        .unwrap();
    }
    db.lock()
        .execute(
            "UPDATE notes SET folder_id='f1' WHERE meeting_id='m1' AND provider_id='a'",
            [],
        )
        .unwrap();
    drop(db);

    let db = Db::open_with_key(&path, TEST_DEK).unwrap();
    assert_eq!(db.folder_for_meeting("m1").unwrap().as_deref(), Some("f1"));
    let folders: Vec<Option<String>> = {
        let conn = db.lock();
        let mut stmt = conn
            .prepare("SELECT folder_id FROM notes WHERE meeting_id='m1' ORDER BY provider_id")
            .unwrap();
        stmt.query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    };
    assert_eq!(folders, vec![Some("f1".into()), Some("f1".into())]);
    let _ = std::fs::remove_file(&path);
}

/// A split legacy meeting is never assigned to an arbitrary provider folder. Every governing
/// folder must be readable, so one locked sibling fails closed until the user explicitly files it.
#[test]
fn migration_leaves_conflicting_legacy_folders_ambiguous_and_visibility_fails_closed() {
    let (db, path) = fresh_db("ambiguous-backfill");
    container(&db, "f-open", "Open", "Open", None, "folder");
    container(&db, "f-locked", "Locked", "Locked", None, "folder");
    db.insert_meeting(&Meeting {
        id: "m1".into(),
        started_at: "2026-08-20T09:00:00Z".into(),
        ended_at: None,
        title: Some("Legacy split".into()),
        duration_s: 1,
        audio_path: None,
        status: MeetingStatus::Summarized,
        folder_id: None,
    })
    .unwrap();
    for (provider, folder) in [("a", "f-open"), ("b", "f-locked")] {
        db.upsert_note(&NoteRecord {
            meeting_id: "m1".into(),
            provider_id: provider.into(),
            markdown: provider.into(),
            created_at: "2026-08-20T09:01:00Z".into(),
            ..Default::default()
        })
        .unwrap();
        db.lock()
            .execute(
                "UPDATE notes SET folder_id=?2 WHERE meeting_id='m1' AND provider_id=?1",
                rusqlite::params![provider, folder],
            )
            .unwrap();
    }
    seal(&db, "f-locked");
    drop(db);

    let db = Db::open_with_key(&path, TEST_DEK).unwrap();
    let canonical: Option<String> = db
        .lock()
        .query_row("SELECT folder_id FROM meetings WHERE id='m1'", [], |row| row.get(0))
        .unwrap();
    assert!(canonical.is_none());
    assert!(matches!(db.folder_for_meeting("m1"), Err(AppError::Locked(_))));
    assert!(
        db.list_meetings_visible(50, &HashSet::new(), None)
            .unwrap()
            .iter()
            .all(|meeting| meeting.id != "m1"),
        "one open provider must not authorize a conflicting locked sibling"
    );
    let _ = std::fs::remove_file(&path);
}

/// A system container is not adopted, not re-parented, and not read.
///
/// This is a launch-safety property, not tidiness: an in-flight feature owns a container whose row
/// and children are guarded by RAISE(ABORT) triggers, and this migration runs before any content
/// surface comes up — so a blanket re-parent would be a failure to START the app.
#[test]
fn the_migration_leaves_a_system_container_alone() {
    let (db, path) = pre_migration_db("system-untouched");
    container(&db, "f-work", "Work", "Work", None, "folder");
    container(&db, "sys", "tasks", ".murmur/tasks", None, "folder");
    db.lock()
        .execute(
            "UPDATE folders SET kind = 'task' WHERE id = 'sys'",
            rusqlite::params![],
        )
        .unwrap();

    run_migration(&db);

    let (parent, level): (Option<String>, String) = db
        .lock()
        .query_row(
            "SELECT parent_id, COALESCE(level, 'folder') FROM folders WHERE id = 'sys'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(parent.is_none(), "the system container was re-parented");
    assert_eq!(level, "folder", "the system container was re-levelled");
    assert!(db.list_containers().unwrap().iter().all(|c| c.id != "sys"));

    let _ = std::fs::remove_file(&path);
}

/// The migration never WRITES a container's sealed flag or wrapped key.
///
/// This is the narrow column-level claim; that a real seal still opens after adoption — the property
/// that actually matters, since the content is only recoverable through the key those columns
/// describe — is proven separately by
/// `lifecycle_tests::a_real_seal_survives_adoption_and_still_unseals_byte_identical`, which seals
/// through the ordinary lock path with real key material and unseals afterwards.
#[test]
fn the_migration_touches_no_lock_state() {
    let (db, path) = pre_migration_db("sealed-migration");
    container(&db, "f-locked", "Legal", "Legal", None, "folder");
    seal(&db, "f-locked");
    db.lock()
        .execute(
            "UPDATE folders SET wrapped_key = X'DEADBEEF' WHERE id = 'f-locked'",
            rusqlite::params![],
        )
        .unwrap();

    let before: (i64, Option<Vec<u8>>) = db
        .lock()
        .query_row(
            "SELECT locked, wrapped_key FROM folders WHERE id = 'f-locked'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();

    run_migration(&db);

    let after: (i64, Option<Vec<u8>>) = db
        .lock()
        .query_row(
            "SELECT locked, wrapped_key FROM folders WHERE id = 'f-locked'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(before, after, "the migration touched a sealed container's key");
    assert_eq!(before.0, 1);

    // It is adopted like any other container, and still reports itself sealed.
    let sealed = db
        .list_containers()
        .unwrap()
        .into_iter()
        .find(|c| c.id == "f-locked")
        .unwrap();
    assert!(sealed.parent_id.is_some());
    assert!(sealed.locked);

    let _ = std::fs::remove_file(&path);
}

/// The default project is named after the vault directory when one is configured.
#[test]
fn the_default_project_takes_the_vault_directory_name() {
    let (db, path) = pre_migration_db("vault-name");
    db.lock()
        .execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES ('vault_path', '/Users/x/Obsidian/Second Brain')",
            rusqlite::params![],
        )
        .unwrap();

    run_migration(&db);

    let project = db
        .list_containers()
        .unwrap()
        .into_iter()
        .find(|c| c.level == "project")
        .unwrap();
    assert_eq!(project.name, "Second Brain");

    // With no vault configured, a neutral name is used instead.
    let (db2, path2) = pre_migration_db("neutral-name");
    run_migration(&db2);
    let project = db2
        .list_containers()
        .unwrap()
        .into_iter()
        .find(|c| c.level == "project")
        .unwrap();
    assert_eq!(project.name, "Workspace");

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&path2);
}

/// Anything already occupying the vault root leaves the database un-adopted — nothing is promoted.
///
/// The slot is UNIQUE, so an occupant cannot be moved aside, and promoting it would produce a row
/// that is a project AND carries a user's contents. Two things follow from that, and both are why
/// this declines instead: the legacy shim hides project rows, so a promoted container and everything
/// filed inside it would vanish from the shipped sidebar; and a promoted note-kind row would be a
/// note folder at the vault root, which the note-folder move resolves and would then compose
/// filesystem work from — against the vault root itself.
#[test]
fn an_occupied_vault_root_declines_adoption() {
    // Including the shapes that most resemble the migration's own result: a project-level row at the
    // vault root, of each kind — the meeting-kind one being exactly what a completed adoption looks
    // like, minus the adoption. Only the exact shape this migration produces — a MEETING-kind
    // project there — may be read as "already adopted"; anything else is an occupant and declines,
    // because accepting it would both skip the occupancy check and, for the note kind, leave a note
    // folder whose path is the vault root.
    for (label, prepare) in [
        ("plain", 0u8),
        ("sealed", 1u8),
        ("note-kind", 2u8),
        ("note-kind project", 3u8),
        ("meeting-kind project", 4u8),
    ] {
        let (db, path) = pre_migration_db(&format!("occupied-{label}"));
        container(&db, "f-root", "Everything", "", None, "folder");
        container(&db, "f-work", "Work", "Work", None, "folder");
        meeting_in(&db, "m1", "Root standup", "2026-08-20T09:00:00Z", Some("f-root"));
        if prepare == 1 {
            seal(&db, "f-root");
        } else if prepare == 2 {
            db.lock()
                .execute(
                    "UPDATE folders SET kind = 'note' WHERE id = 'f-root'",
                    rusqlite::params![],
                )
                .unwrap();
        } else if prepare == 3 {
            db.lock()
                .execute(
                    "UPDATE folders SET kind = 'note', level = 'project' WHERE id = 'f-root'",
                    rusqlite::params![],
                )
                .unwrap();
        } else if prepare == 4 {
            // The shape that most resembles a completed adoption. It is NOT one: `f-work` is still
            // outside. Recognising the shape alone would report this database done and leave that
            // container orphaned on this launch and every later one.
            db.lock()
                .execute(
                    "UPDATE folders SET level = 'project' WHERE id = 'f-root'",
                    rusqlite::params![],
                )
                .unwrap();
        }

        // What the shipped sidebar shows, captured BEFORE the migration runs.
        let shipped_forest = |db: &Db| -> String {
            let folders = db.list_folders().unwrap();
            let levels = db.folder_levels().unwrap();
            let counts = db.count_notes_per_folder(&HashSet::new()).unwrap();
            let kinds = db.folder_kinds().unwrap();
            serde_json::to_string(&build_folder_tree(
                &flatten_projects_for_legacy_tree(folders, &levels),
                &counts,
                &HashSet::new(),
                &kinds,
            ))
            .unwrap()
        };
        let before = folder_rows(&db);
        let forest_before = shipped_forest(&db);

        run_migration(&db);

        assert_eq!(
            before,
            folder_rows(&db),
            "{label}: an occupied vault root must leave every row untouched"
        );
        // And the shipped sidebar renders exactly what it rendered before — which is the whole reason
        // for declining rather than adopting the occupant. (Compared before-vs-after, not shimmed-vs-
        // unshimmed: hiding a project-level row is what the shim is FOR, so on a database that already
        // contains one those two differ by design and always have.)
        assert_eq!(
            forest_before,
            shipped_forest(&db),
            "{label}: the shipped sidebar changed"
        );

        let _ = std::fs::remove_file(&path);
    }
}

/// Containers that already had a parent keep it — existing depth is preserved, never flattened.
#[test]
fn the_migration_preserves_existing_nesting() {
    let (db, path) = pre_migration_db("preserve-depth");
    container(&db, "f-parent", "Work", "Work", None, "folder");
    container(&db, "f-child", "Q3", "Work/Q3", Some("f-parent"), "folder");

    run_migration(&db);

    let containers = db.list_containers().unwrap();
    let project = containers.iter().find(|c| c.level == "project").unwrap();
    assert_eq!(
        containers
            .iter()
            .find(|c| c.id == "f-child")
            .unwrap()
            .parent_id
            .as_deref(),
        Some("f-parent"),
        "an already-nested container was re-parented"
    );
    assert_eq!(
        containers
            .iter()
            .find(|c| c.id == "f-parent")
            .unwrap()
            .parent_id
            .as_deref(),
        Some(project.id.as_str())
    );

    let _ = std::fs::remove_file(&path);
}

// ── the review round that produced these ─────────────────────────────────────────────────────────
//
// Every test below exists because an independent review found the corresponding path unproven. Two
// of them cover failure modes that would not have been leaks but LAUNCH FAILURES: this migration
// runs inside `migrate()`, before any content surface, so a write against a trigger-guarded row
// aborts startup. They therefore install a real `RAISE(ABORT)` trigger and prove the absence of the
// write, rather than inspecting the row afterwards and inferring it.

/// Arm a row so that ANY write to it aborts, exactly as the reserved container's own triggers do.
fn forbid_writes_to(db: &Db, id: &str) {
    db.lock()
        .execute_batch(&format!(
            "CREATE TRIGGER guard_update_{id} BEFORE UPDATE ON folders WHEN OLD.id = '{id}'
               BEGIN SELECT RAISE(ABORT, 'protected container written'); END;
             CREATE TRIGGER guard_child_{id} BEFORE UPDATE ON folders WHEN NEW.parent_id = '{id}'
               BEGIN SELECT RAISE(ABORT, 'protected container given a child'); END;",
            id = id.replace('-', "_")
        ))
        .unwrap_or_else(|e| panic!("could not arm the guard: {e}"));
}

/// THE shim oracle, on an ADOPTED database.
///
/// Part 1's shim exists so the shipped sidebar keeps rendering the old forest AFTER adoption, and
/// until now every test in this file un-adopted its database first — so the one shape the shim is
/// for was the one shape nothing covered. Both of its failure modes are live: keying roots off a
/// NULL parent would make the sidebar lose every folder (or surface the project as a root), and
/// note-kind containers reachable through the new parent edges would re-enter the Meetings tree,
/// which filters them at the top level ONLY. That leak shipped once already.
#[test]
fn the_legacy_folder_tree_is_unchanged_on_an_adopted_database() {
    let path = crate::storage::db::unique_temp_path("murmur-workspace-adopted-shim", "sqlite");
    let _ = std::fs::remove_file(&path);
    // NOT `fresh_db`: this one keeps the project the migration creates.
    let db = Db::open_with_key(&path, TEST_DEK).unwrap();
    container(&db, "f-work", "Work", "Work", None, "folder");
    container(&db, "f-notes", "Notes", "Notes", None, "folder");
    container(&db, "f-research", "Research", "Notes/Research", None, "folder");
    container(&db, "f-child", "Q3", "Work/Q3", Some("f-work"), "folder");
    db.lock()
        .execute_batch(
            "UPDATE folders SET kind = 'note' WHERE id IN ('f-notes', 'f-research');
             UPDATE folders SET is_root = 1 WHERE id = 'f-notes';",
        )
        .unwrap();
    // Re-open so the migration adopts the containers seeded above.
    drop(db);
    let db = Db::open_with_key(&path, TEST_DEK).unwrap();
    assert_eq!(
        db.list_containers()
            .unwrap()
            .iter()
            .filter(|c| c.level == "project")
            .count(),
        1,
        "the database is adopted"
    );

    let folders = db.list_folders().unwrap();
    let levels = db.folder_levels().unwrap();
    let flattened = flatten_projects_for_legacy_tree(folders, &levels);
    let counts = db.count_notes_per_folder(&HashSet::new()).unwrap();
    let kinds = db.folder_kinds().unwrap();
    let legacy = build_folder_tree(&flattened, &counts, &HashSet::new(), &kinds);

    let roots: Vec<&str> = legacy.iter().map(|n| n.id.as_str()).collect();
    // (a) the project is not among the roots, and (b) every former root is still a root.
    assert!(
        !roots.iter().any(|id| db
            .list_containers()
            .unwrap()
            .iter()
            .any(|c| c.level == "project" && c.id == *id)),
        "the project surfaced as a legacy root: {roots:?}"
    );
    assert!(roots.contains(&"f-work"), "a former root vanished: {roots:?}");
    assert!(roots.contains(&"f-notes"), "a former root vanished: {roots:?}");
    assert!(
        roots.contains(&"f-research"),
        "a note container adopted under the note root must still be a legacy ROOT, or the shipped \
         Meetings tree — which filters note kinds at the top level only — would swallow it: {roots:?}"
    );
    // (c) real nesting is still nested, and the note kinds are still filterable where the shipped
    //     Meetings tree looks: the top level.
    let work = legacy.iter().find(|n| n.id == "f-work").unwrap();
    assert_eq!(work.children.len(), 1);
    assert_eq!(work.children[0].id, "f-child");
    let meeting_forest: Vec<&str> = legacy
        .iter()
        .filter(|n| n.kind != "note")
        .map(|n| n.id.as_str())
        .collect();
    assert_eq!(
        meeting_forest,
        vec!["f-work"],
        "a note-kind container re-entered the Meetings tree: {meeting_forest:?}"
    );

    let _ = std::fs::remove_file(&path);
}

/// With SEVERAL note roots, each note container is adopted under the one its own path is stamped
/// under.
///
/// `ensure_notes_root` deliberately creates a separate root when the existing one is locked (its
/// "Inbox N" fallback), so a real database can hold more than one. Picking the wrong one breaks
/// `parent.path + name == path`, and the next rename then recomposes from that wrong parent and
/// relocates the real vault directory while `exported_path` stays behind — the sealed-content leak,
/// because the seal deletes the plaintext at the recorded path and so deletes nothing.
#[test]
fn a_note_container_is_adopted_under_the_root_its_path_is_stamped_under() {
    let (db, path) = pre_migration_db("two-note-roots");
    container(&db, "r-notes", "Notes", "Notes", None, "folder");
    container(&db, "r-inbox", "Inbox 2", "Inbox 2", None, "folder");
    container(&db, "c-under-notes", "Research", "Notes/Research", None, "folder");
    container(&db, "c-under-inbox", "Drafts", "Inbox 2/Drafts", None, "folder");
    db.lock()
        .execute_batch(
            "UPDATE folders SET kind = 'note';
             UPDATE folders SET is_root = 1 WHERE id IN ('r-notes', 'r-inbox');",
        )
        .unwrap();

    run_migration(&db);

    let containers = db.list_containers().unwrap();
    let parent_of = |id: &str| {
        containers
            .iter()
            .find(|c| c.id == id)
            .unwrap()
            .parent_id
            .clone()
    };
    assert_eq!(parent_of("c-under-notes").as_deref(), Some("r-notes"));
    assert_eq!(parent_of("c-under-inbox").as_deref(), Some("r-inbox"));

    // And composition holds for both, which is the property the whole rule exists to protect.
    for (child, root, name) in [
        ("c-under-notes", "Notes", "Research"),
        ("c-under-inbox", "Inbox 2", "Drafts"),
    ] {
        let stored: String = db
            .lock()
            .query_row(
                "SELECT path FROM folders WHERE id = ?1",
                rusqlite::params![child],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stored, format!("{root}/{name}"));
    }

    let _ = std::fs::remove_file(&path);
}

/// A note container whose path cannot be composed from any root is left to the project, with its
/// path untouched — never silently re-homed under a root that would misdescribe it.
#[test]
fn a_note_container_with_an_uncomposable_path_is_not_re_homed() {
    let (db, path) = pre_migration_db("uncomposable");
    container(&db, "r-notes", "Notes", "Notes", None, "folder");
    // Two levels deep: `parent.path + '/' + name` would compose to "Notes/Deep", not its real path.
    container(&db, "c-deep", "Deep", "Notes/Mid/Deep", None, "folder");
    db.lock()
        .execute_batch(
            "UPDATE folders SET kind = 'note';
             UPDATE folders SET is_root = 1 WHERE id = 'r-notes';",
        )
        .unwrap();

    run_migration(&db);

    let containers = db.list_containers().unwrap();
    let project = containers.iter().find(|c| c.level == "project").unwrap();
    let deep = containers.iter().find(|c| c.id == "c-deep").unwrap();
    assert_eq!(
        deep.parent_id.as_deref(),
        Some(project.id.as_str()),
        "an uncomposable container belongs to the project, not to a root that misdescribes it"
    );
    let stored: String = db
        .lock()
        .query_row("SELECT path FROM folders WHERE id = 'c-deep'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(stored, "Notes/Mid/Deep", "its path is untouched either way");

    let _ = std::fs::remove_file(&path);
}

/// A protected container flagged as a note root is never chosen as an adoption parent.
///
/// Proven by the trigger, not by inspection: giving it a child would ABORT inside `migrate()`, which
/// is a failure to start the app.
#[test]
fn a_protected_container_is_never_chosen_as_a_note_root() {
    let (db, path) = pre_migration_db("protected-root");
    container(&db, "sys", "tasks", ".murmur/tasks", None, "folder");
    container(&db, "r-notes", "Notes", "Notes", None, "folder");
    container(&db, "c-note", "Research", "Notes/Research", None, "folder");
    db.lock()
        .execute_batch(
            "UPDATE folders SET kind = 'note' WHERE id IN ('r-notes', 'c-note');
             UPDATE folders SET kind = 'task', is_root = 1 WHERE id = 'sys';
             UPDATE folders SET is_root = 1 WHERE id = 'r-notes';",
        )
        .unwrap();
    forbid_writes_to(&db, "sys");

    run_migration(&db);

    let containers = db.list_containers().unwrap();
    assert_eq!(
        containers
            .iter()
            .find(|c| c.id == "c-note")
            .unwrap()
            .parent_id
            .as_deref(),
        Some("r-notes"),
        "the real note root, not the protected row"
    );

    let _ = std::fs::remove_file(&path);
}

/// A protected container occupying the vault root leaves the database un-adopted rather than
/// aborting startup — and is neither read nor written.
#[test]
fn a_protected_container_at_the_vault_root_declines_adoption() {
    let (db, path) = pre_migration_db("protected-vault-root");
    container(&db, "sys", "root", "", None, "folder");
    container(&db, "f-work", "Work", "Work", None, "folder");
    db.lock()
        .execute(
            "UPDATE folders SET kind = 'task' WHERE id = 'sys'",
            rusqlite::params![],
        )
        .unwrap();
    forbid_writes_to(&db, "sys");

    // Must not abort: the whole migration runs before any content surface.
    run_migration(&db);

    assert!(
        db.list_containers()
            .unwrap()
            .iter()
            .all(|c| c.level != "project"),
        "the database is left un-adopted, and will retry on the next launch"
    );
    let work_parent: Option<String> = db
        .lock()
        .query_row("SELECT parent_id FROM folders WHERE id = 'f-work'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(work_parent.is_none(), "nothing was adopted");

    let _ = std::fs::remove_file(&path);
}

/// A protected container is left byte-identical even when its level is not the default.
///
/// The level-normalisation statement was inert for such a row only by accident — its level
/// coalesced out of the predicate. A future import stamping a non-default level would have turned it
/// into a write against a trigger-guarded row during startup.
#[test]
fn a_protected_container_with_an_unexpected_level_is_left_byte_identical() {
    let (db, path) = pre_migration_db("protected-level");
    container(&db, "sys", "tasks", ".murmur/tasks", None, "project");
    container(&db, "f-work", "Work", "Work", None, "folder");
    db.lock()
        .execute(
            "UPDATE folders SET kind = 'task' WHERE id = 'sys'",
            rusqlite::params![],
        )
        .unwrap();
    let before: (Option<String>, String, String) = db
        .lock()
        .query_row(
            "SELECT parent_id, COALESCE(level, 'folder'), path FROM folders WHERE id = 'sys'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    forbid_writes_to(&db, "sys");

    // The system row already carries level='project'. The adoption guard must not mistake that for a
    // completed migration — otherwise the database would never be adopted, on this launch or any
    // later one — so this asserts the real containers were adopted as well as that the row is intact.
    run_migration(&db);

    let containers = db.list_containers().unwrap();
    let project = containers
        .iter()
        .find(|c| c.level == "project")
        .expect("a system row at project level must not masquerade as the adoption result");
    assert_eq!(
        containers.iter().find(|c| c.id == "f-work").unwrap().parent_id.as_deref(),
        Some(project.id.as_str()),
        "the user container was adopted"
    );

    let after: (Option<String>, String, String) = db
        .lock()
        .query_row(
            "SELECT parent_id, COALESCE(level, 'folder'), path FROM folders WHERE id = 'sys'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(before, after, "a protected container was modified");

    let _ = std::fs::remove_file(&path);
}

/// The reserved prefix excludes the reserved DIRECTORY itself, not only its descendants.
#[test]
fn the_reserved_prefix_root_itself_is_excluded() {
    let (db, path) = pre_migration_db("reserved-root");
    // A user KIND at exactly the reserved path: only the path guard can exclude it.
    container(&db, "reserved", "murmur", ".murmur", None, "folder");
    container(&db, "f-work", "Work", "Work", None, "folder");
    forbid_writes_to(&db, "reserved");

    run_migration(&db);

    let parent: Option<String> = db
        .lock()
        .query_row(
            "SELECT parent_id FROM folders WHERE id = 'reserved'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(parent.is_none(), "the reserved directory itself was adopted");

    let _ = std::fs::remove_file(&path);
}

/// The adoption is one atomic unit: a failure part-way leaves the database byte-identical.
///
/// It matters more than usual because the completion guard is RESULT-based. A partial run that left
/// the project row behind would short-circuit that guard on every later launch, with orphaned
/// containers and nothing to repair them — so atomicity here is what makes "retry on the next
/// launch" true rather than aspirational.
#[test]
fn a_failed_adoption_leaves_the_database_untouched() {
    let (db, path) = pre_migration_db("atomic-adoption");
    container(&db, "f-work", "Work", "Work", None, "folder");
    container(&db, "f-legal", "Legal", "Legal", None, "folder");
    let before = folder_rows(&db);

    // Abort the moment anything is given a parent — i.e. after the project row has been inserted.
    db.lock()
        .execute_batch(
            "CREATE TRIGGER abort_adoption BEFORE UPDATE ON folders
               WHEN NEW.parent_id IS NOT NULL AND OLD.parent_id IS NULL
               BEGIN SELECT RAISE(ABORT, 'injected mid-adoption failure'); END;",
        )
        .unwrap();

    let err = Db::migrate_hierarchy_v1(&db.lock()).expect_err("the injected failure propagates");
    assert!(matches!(err, AppError::Storage(_)), "unexpected error: {err:?}");

    db.lock()
        .execute_batch("DROP TRIGGER abort_adoption")
        .unwrap();
    assert_eq!(
        before,
        folder_rows(&db),
        "a partial adoption survived: the project row would then satisfy the completion guard \
         forever while its containers stayed orphaned"
    );
    // And the next attempt, with the failure removed, adopts cleanly.
    run_migration(&db);
    let containers = db.list_containers().unwrap();
    let project = containers.iter().find(|c| c.level == "project").unwrap();
    assert_eq!(
        containers
            .iter()
            .find(|c| c.id == "f-work")
            .unwrap()
            .parent_id
            .as_deref(),
        Some(project.id.as_str())
    );

    let _ = std::fs::remove_file(&path);
}

/// A user project at some OTHER path does not satisfy the completion guard.
///
/// The guard recognises the exact result this migration produces — a user container occupying the
/// vault root. Accepting any project-level row would let a database that was never adopted
/// short-circuit forever, leaving every former root orphaned.
#[test]
fn a_project_away_from_the_vault_root_does_not_count_as_adopted() {
    let (db, path) = pre_migration_db("stray-project");
    container(&db, "p-stray", "Elsewhere", "Elsewhere", None, "project");
    container(&db, "f-work", "Work", "Work", None, "folder");

    run_migration(&db);

    let containers = db.list_containers().unwrap();
    let root_project = containers
        .iter()
        .find(|c| c.level == "project" && c.id != "p-stray")
        .expect("a stray project must not stand in for the adoption result");
    assert_eq!(
        containers
            .iter()
            .find(|c| c.id == "f-work")
            .unwrap()
            .parent_id
            .as_deref(),
        Some(root_project.id.as_str()),
        "the real containers were adopted"
    );

    let _ = std::fs::remove_file(&path);
}

/// A container at the vault root is not resolvable as a NOTE folder, whatever shape it arrived in.
///
/// This is what makes the note-folder move unreachable for such a row unconditionally, rather than
/// only for the container this migration creates: the move composes `src`/`dst` from a container's
/// own path, and an empty path is the vault directory itself, so resolving one there would let the
/// move relocate the user's whole vault.
#[test]
fn a_vault_root_container_is_not_a_note_folder() {
    let (db, path) = pre_migration_db("root-not-note-folder");
    container(&db, "f-root", "Everything", "", None, "folder");
    container(&db, "f-notes", "Notes", "Notes", None, "folder");
    db.lock()
        .execute_batch(
            "UPDATE folders SET kind = 'note' WHERE id IN ('f-root', 'f-notes');
             UPDATE folders SET is_root = 1 WHERE id = 'f-notes';",
        )
        .unwrap();

    assert!(
        db.note_folder_by_id("f-root").unwrap().is_none(),
        "a note-kind container at the vault root must not resolve as a note folder"
    );
    // A project-level one is refused for the same reason, and an ordinary note folder still resolves.
    db.lock()
        .execute(
            "UPDATE folders SET level = 'project' WHERE id = 'f-root'",
            rusqlite::params![],
        )
        .unwrap();
    assert!(db.note_folder_by_id("f-root").unwrap().is_none());
    assert!(db.note_folder_by_id("f-notes").unwrap().is_some());

    let _ = std::fs::remove_file(&path);
}
