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
            "emoji", "folders", "groups", "id", "isRoot", "level", "locked", "name", "tint",
            "unlocked",
        ],
        "ContainerNode gained a field: decide explicitly whether a SEALED container may disclose it"
    );
    assert!(node["groups"].as_array().unwrap().is_empty());

    let _ = std::fs::remove_file(&path);
}

/// `Meeting.folder_id` is DERIVED on read, never stored — pinned because the whole hierarchy reader
/// depends on it.
///
/// `meetings` has no folder column and `insert_meeting` does not persist one, so the field on a
/// fixture is inert. Every container decision in this module therefore has exactly one source,
/// `notes.folder_id`; if a real column is ever added, this fails and forces the duplicate source of
/// truth to be reconciled rather than silently introduced.
#[test]
fn a_meetings_folder_id_is_derived_not_stored() {
    let (db, path) = fresh_db("derived-folder-id");
    container(&db, "f1", "Legal", "Legal", None, "folder");
    db.insert_meeting(&Meeting {
        id: "m1".into(),
        started_at: "2026-08-21T09:00:00Z".into(),
        ended_at: None,
        title: Some("Raw".into()),
        duration_s: 1,
        audio_path: None,
        // Deliberately names a container: it must be ignored, because there is nowhere to put it.
        folder_id: Some("f1".into()),
        status: MeetingStatus::Recording,
    })
    .unwrap();

    let has_column = db
        .lock()
        .prepare("SELECT folder_id FROM meetings LIMIT 1")
        .is_ok();
    assert!(
        !has_column,
        "meetings gained a folder_id column — the hierarchy derives a container from notes.folder_id \
         and now has two sources of truth"
    );
    assert!(db.folder_for_meeting("m1").unwrap().is_none());

    let inbox = container_items_inner(&db, &HashSet::new(), None, ItemKind::Meeting, 0, 50).unwrap();
    assert_eq!(inbox.total, 1, "it is unfiled regardless of the DTO field");

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
