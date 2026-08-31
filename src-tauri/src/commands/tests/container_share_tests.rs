//! Oracles for SHARED CONTAINERS — the share PLAN, the received-forest read model, and the wire
//! contract of every DTO the two produce.
//!
//! The plan tests are the leak oracles. A green `cargo test --lib`, `ng lint` and `ng build` all
//! pass for a planner that walks into a sealed folder and publishes its notes; only a test that
//! seals a container and then asserts its items never reach the plan can see it.
//!
//! File-backed SQLCipher via `open_with_key` + a FIXED literal test key — never the real Keychain,
//! and never a content key: a lock is modelled by the `folders.locked` COLUMN, which is exactly
//! what the planner reads.

use super::*;
use crate::commands::org_containers::{
    build_shared_workspace, plan_container_share, ContainerSharePreview, ContainerShareResult,
    ContainerShareStatus, SharedContainerNode, SharedItemRow, SharedWorkspace,
    MAX_CONTAINER_SHARE_ITEMS,
};
use crate::storage::db::Db;
use crate::storage::models::{
    ContainerShareRow, Meeting, MeetingStatus, NoteRecord, OrgContainerRow, OrgState,
};

const TEST_DEK: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn fresh_db(label: &str) -> Db {
    let path =
        crate::storage::db::unique_temp_path(&format!("murmur-container-cmd-{label}"), "sqlite");
    let _ = std::fs::remove_file(&path);
    let db = Db::open_with_key(&path, TEST_DEK).unwrap();
    // Opening ADOPTS the vault: the hierarchy migration creates a default project at the root.
    // Remove it so each test states the exact container shape it is about.
    db.lock()
        .execute(
            "DELETE FROM folders WHERE COALESCE(level, 'folder') = 'project'",
            rusqlite::params![],
        )
        .unwrap();
    db
}

fn container(db: &Db, id: &str, name: &str, path: &str, parent: Option<&str>, level: &str) {
    db.insert_folder(&Folder {
        id: id.to_string(),
        name: name.to_string(),
        path: path.to_string(),
        parent_id: parent.map(str::to_string),
        locked: false,
        created_at: "2026-08-29T09:00:00Z".to_string(),
    })
    .unwrap();
    db.lock()
        .execute(
            "UPDATE folders SET level = ?2 WHERE id = ?1",
            rusqlite::params![id, level],
        )
        .unwrap();
}

fn seal(db: &Db, id: &str) {
    db.lock()
        .execute(
            "UPDATE folders SET locked = 1 WHERE id = ?1",
            rusqlite::params![id],
        )
        .unwrap();
}

fn note_in(db: &Db, id: &str, name: &str, folder: &str, created_at: i64) {
    db.insert_document(id, folder, name, "body", "note", created_at)
        .unwrap();
}

fn meeting_in(db: &Db, id: &str, title: &str, folder: &str) {
    db.insert_meeting(&Meeting {
        id: id.to_string(),
        started_at: "2026-08-29T10:00:00Z".to_string(),
        ended_at: None,
        title: Some(title.to_string()),
        duration_s: 600,
        audio_path: None,
        status: MeetingStatus::Summarized,
        folder_id: Some(folder.to_string()),
    })
    .unwrap();
    db.upsert_note(&NoteRecord {
        meeting_id: id.to_string(),
        provider_id: "claude_code".to_string(),
        markdown: format!("# {title}"),
        created_at: "2026-08-29T10:00:00Z".to_string(),
        ..Default::default()
    })
    .unwrap();
    db.set_meeting_folder(id, Some(folder)).unwrap();
}

fn seed_org(db: &Db, org_id: &str, name: &str) {
    db.upsert_org_state(&OrgState {
        org_id: org_id.into(),
        name: name.into(),
        role: "member".into(),
        joined_at: "2026-08-29T10:00:00Z".into(),
        consented: true,
        last_seq: 0,
        generation: 1,
        context_enabled: true,
    })
    .unwrap();
}

fn received_container(
    db: &Db,
    org: &str,
    container_id: &str,
    name: &str,
    level: &str,
    parent: Option<&str>,
) {
    db.upsert_org_container(&OrgContainerRow {
        org_id: org.into(),
        container_id: container_id.into(),
        item_id: format!("item-{container_id}"),
        level: level.into(),
        name: name.into(),
        emoji: None,
        tint: None,
        parent_container_id: parent.map(str::to_string),
        position: 0,
        access: "view".into(),
        author_hint: "kgm004a".into(),
        author_user_id: Some("u1".into()),
        document_owner_user_id: Some("u1".into()),
        seq: 1,
        rev: 1,
        generation: 1,
        created_at: "2026-08-29T10:00:00Z".into(),
    })
    .unwrap();
}

/// A received ITEM, written straight into the replica the way ingest would.
fn received_item(db: &Db, org: &str, item_id: &str, title: &str, parent: Option<&str>) {
    db.lock()
        .execute(
            "INSERT INTO org_items(item_id, org_id, seq, author_hint, title, markdown, created_at,
                                   rev, generation, is_current, tombstoned, source_kind,
                                   parent_container_id, position)
             VALUES (?1, ?2, 1, 'kgm004a', ?3, 'body', '2026-08-29T10:00:00Z', 1, 1, 1, 0,
                     'document', ?4, 0)",
            rusqlite::params![item_id, org, title, parent],
        )
        .unwrap();
}

fn state_with(db: Db) -> AppState {
    AppState::for_tests(db)
}

// ── the wire contract ────────────────────────────────────────────────────────────────────────

/// Every key of every shared-container DTO is camelCase.
///
/// `TileData` shipped `started_at`/`duration_s`/`has_audio` against a camelCase frontend; every
/// field read `undefined`, the tile threw while rendering, and it took the whole board down. The
/// hand-written e2e mocks could not see it — typed against the frontend's own interface, they are
/// camelCase by construction, so they DEFINE a shape rather than verify one. Only a serialization
/// assertion over the PRODUCING side catches this class.
#[test]
fn every_shared_container_dto_key_is_camel_case() {
    fn assert_camel(value: &serde_json::Value) {
        match value {
            serde_json::Value::Object(map) => {
                for (key, child) in map {
                    assert!(
                        !key.contains('_')
                            && key.starts_with(|c: char| c.is_ascii_lowercase())
                            && key.chars().all(|c| c.is_ascii_alphanumeric()),
                        "{key} must be camelCase on the wire"
                    );
                    assert_camel(child);
                }
            }
            serde_json::Value::Array(items) => items.iter().for_each(assert_camel),
            _ => {}
        }
    }

    let preview = ContainerSharePreview {
        folder_id: "f1".into(),
        name: "Klienci".into(),
        level: "space".into(),
        note_count: 3,
        meeting_count: 2,
        folder_count: 1,
        skipped_sealed: 1,
        skipped_dashboards: 1,
        total_items: 6,
    };
    assert_camel(&serde_json::to_value(&preview).unwrap());

    let result = ContainerShareResult {
        container_id: "c1".into(),
        published: 5,
        failed: 0,
    };
    assert_camel(&serde_json::to_value(&result).unwrap());

    let status = ContainerShareStatus {
        org_id: "o1".into(),
        org_name: "Siema".into(),
        folder_id: "f1".into(),
        container_id: "c1".into(),
        access: "view".into(),
        is_root: true,
        state: "published".into(),
    };
    assert_camel(&serde_json::to_value(&status).unwrap());

    let workspace = SharedWorkspace {
        spaces: vec![SharedContainerNode {
            container_id: Some("c1".into()),
            org_id: "o1".into(),
            org_name: "Siema".into(),
            name: "Klienci".into(),
            level: "space".into(),
            emoji: Some("📁".into()),
            tint: Some("teal".into()),
            access: "edit".into(),
            author_hint: "kgm004a".into(),
            folders: vec![],
            items: vec![SharedItemRow {
                item_id: "i1".into(),
                doc_id: Some("d1".into()),
                title: "Notatka".into(),
                kind: Some("document".into()),
                author_hint: "kgm004a".into(),
                created_at: "2026-08-29T10:00:00Z".into(),
                org_id: "o1".into(),
                org_name: "Siema".into(),
                access: "edit".into(),
                position: 0,
            }],
            local_parent_id: Some("local-f".into()),
            position: 0,
        }],
        shared_brains: SharedContainerNode {
            container_id: None,
            org_id: String::new(),
            org_name: String::new(),
            name: "Shared Brains".into(),
            level: "virtual".into(),
            emoji: None,
            tint: None,
            access: "view".into(),
            author_hint: String::new(),
            folders: vec![],
            items: vec![],
            local_parent_id: None,
            position: 0,
        },
    };
    assert_camel(&serde_json::to_value(&workspace).unwrap());
}

// ── the share plan: what may and may not be published ────────────────────────────────────────

#[test]
fn planning_a_sealed_container_refuses() {
    let db = fresh_db("sealed-root");
    container(&db, "p1", "Prywatne", "Prywatne", None, "project");
    note_in(&db, "n1", "Tajne", "p1", 1);
    seal(&db, "p1");
    let state = state_with(db);
    let err = plan_container_share(&state, "o1", "p1").unwrap_err();
    assert!(
        matches!(err, AppError::Locked(_)),
        "a sealed Space must refuse, not silently publish nothing"
    );
}

#[test]
fn a_sealed_subfolder_and_its_contents_never_reach_the_plan() {
    let db = fresh_db("sealed-child");
    container(&db, "p1", "Klienci", "Klienci", None, "project");
    container(&db, "f-open", "Otwarte", "Klienci/Otwarte", Some("p1"), "folder");
    container(&db, "f-seal", "Zamkniete", "Klienci/Zamkniete", Some("p1"), "folder");
    note_in(&db, "n-open", "Widoczna", "f-open", 1);
    note_in(&db, "n-seal", "Tajna", "f-seal", 2);
    seal(&db, "f-seal");
    let state = state_with(db);

    let plan = plan_container_share(&state, "o1", "p1").unwrap();

    // TWO gates cover this, and they cover different things. Verified by disabling each in turn:
    //
    //  • the WALK gate (`container.locked` in `plan_container_share`) is the only thing protecting
    //    the sealed folder's NAME — a manifest is published from `folders.name`, which no item
    //    reader ever sees. With the walk gate off, `skipped_sealed` drops to 0 and "Zamkniete"
    //    reaches the plan as a container to publish.
    //  • the READER gate (`visibility_clause`, inside `container_items_page`) is what protects the
    //    CONTENT. With the walk gate off, `n-seal` still does not appear — the item reader filters
    //    it out on its own.
    //
    // Both assertions stay, because each is the only witness to its own gate.
    assert_eq!(plan.skipped_sealed, 1);
    assert!(
        !plan.containers.iter().any(|c| c.folder_id == "f-seal"),
        "a sealed folder's own name must not be published"
    );
    assert!(
        !plan
            .documents
            .iter()
            .any(|d| d.document_id.as_deref() == Some("n-seal")),
        "a sealed descendant's content must never reach the publish plan"
    );
    assert!(plan
        .documents
        .iter()
        .any(|d| d.document_id.as_deref() == Some("n-open")));
}

#[test]
fn the_root_is_planned_before_every_descendant() {
    let db = fresh_db("order");
    container(&db, "p1", "Klienci", "Klienci", None, "project");
    container(&db, "f1", "A", "Klienci/A", Some("p1"), "folder");
    container(&db, "f2", "B", "Klienci/A/B", Some("f1"), "folder");
    let state = state_with(db);

    let plan = plan_container_share(&state, "o1", "p1").unwrap();
    assert_eq!(plan.containers[0].folder_id, "p1");
    assert!(plan.containers[0].is_root);
    for (i, planned) in plan.containers.iter().enumerate().skip(1) {
        let parent = planned.parent_folder_id.clone().unwrap();
        assert!(
            plan.containers[..i].iter().any(|p| p.folder_id == parent),
            "a child manifest must be planned after its parent"
        );
        assert!(!planned.is_root);
        assert!(planned.parent_container_id.is_some());
    }
}

#[test]
fn a_meeting_travels_as_a_document_and_is_counted_separately() {
    let db = fresh_db("meeting");
    container(&db, "p1", "Klienci", "Klienci", None, "project");
    meeting_in(&db, "m1", "Standup", "p1");
    note_in(&db, "n1", "Notatka", "p1", 1);
    let state = state_with(db);

    let plan = plan_container_share(&state, "o1", "p1").unwrap();
    assert_eq!(plan.meeting_count, 1);
    assert_eq!(plan.note_count, 1);
    let meeting = plan
        .documents
        .iter()
        .find(|d| d.meeting_id.as_deref() == Some("m1"))
        .expect("the meeting is planned");
    assert!(
        meeting.document_id.is_none(),
        "exactly one source id identifies a planned document"
    );
}

#[test]
fn re_planning_reuses_the_container_identity_already_published() {
    let db = fresh_db("stable-id");
    container(&db, "p1", "Klienci", "Klienci", None, "project");
    db.upsert_container_share(&ContainerShareRow {
        id: "cs1".into(),
        org_id: "o1".into(),
        folder_id: "p1".into(),
        container_id: "c-already-published".into(),
        access: "view".into(),
        scrub: true,
        is_root: true,
        state: "published".into(),
        item_id: Some("item-1".into()),
        rev: 1,
        generation: 1,
        content_sha256: None,
        position: 0,
        last_error: None,
        created_at: "t".into(),
        updated_at: "t".into(),
    })
    .unwrap();
    let state = state_with(db);

    let plan = plan_container_share(&state, "o1", "p1").unwrap();
    assert_eq!(
        plan.containers[0].container_id, "c-already-published",
        "a re-share must supersede the document peers already hold, not mint a second container"
    );
}

#[test]
fn an_oversized_container_refuses_before_any_egress() {
    let db = fresh_db("oversized");
    container(&db, "p1", "Klienci", "Klienci", None, "project");
    for i in 0..=MAX_CONTAINER_SHARE_ITEMS {
        note_in(&db, &format!("n{i}"), &format!("Note {i}"), "p1", i as i64);
    }
    let state = state_with(db);

    let err = plan_container_share(&state, "o1", "p1").unwrap_err();
    assert!(matches!(err, AppError::InvalidArg(_)));
}

#[test]
fn an_unknown_container_refuses() {
    let db = fresh_db("unknown");
    let state = state_with(db);
    assert!(plan_container_share(&state, "o1", "nope").is_err());
}

// ── the received forest ──────────────────────────────────────────────────────────────────────

#[test]
fn a_received_space_is_top_level_and_a_loose_folder_is_not() {
    let db = fresh_db("forest-roots");
    seed_org(&db, "o1", "Siema");
    received_container(&db, "o1", "c-space", "Klienci", "space", None);
    received_container(&db, "o1", "c-folder", "Notatki", "folder", None);
    let state = state_with(db);

    let workspace = build_shared_workspace(&state).unwrap();
    assert_eq!(workspace.spaces.len(), 1);
    assert_eq!(workspace.spaces[0].level, "space");
    assert_eq!(workspace.spaces[0].name, "Klienci");
    assert_eq!(
        workspace.shared_brains.folders.len(),
        1,
        "a received folder with no shared Space parent lives in Shared Brains"
    );
    assert_eq!(workspace.shared_brains.level, "virtual");
    assert!(workspace.shared_brains.container_id.is_none());
}

#[test]
fn a_nested_received_folder_hangs_under_its_space() {
    let db = fresh_db("forest-nested");
    seed_org(&db, "o1", "Siema");
    received_container(&db, "o1", "c-space", "Klienci", "space", None);
    received_container(&db, "o1", "c-child", "Umowy", "folder", Some("c-space"));
    received_item(&db, "o1", "i1", "Umowa 1", Some("c-child"));
    let state = state_with(db);

    let workspace = build_shared_workspace(&state).unwrap();
    assert_eq!(workspace.spaces.len(), 1);
    assert_eq!(workspace.spaces[0].folders.len(), 1);
    assert_eq!(workspace.spaces[0].folders[0].items.len(), 1);
    assert!(
        workspace.shared_brains.folders.is_empty(),
        "a folder that has a shared parent is not loose"
    );
}

#[test]
fn an_item_whose_parent_never_arrived_stays_reachable() {
    // A placement pointing at a manifest this member has not synced (or that was withdrawn) must
    // fall back to Shared Brains. Dropping it would make a shared note simply vanish.
    let db = fresh_db("forest-orphan");
    seed_org(&db, "o1", "Siema");
    received_item(&db, "o1", "i1", "Sierota", Some("c-never-synced"));
    let state = state_with(db);

    let workspace = build_shared_workspace(&state).unwrap();
    assert_eq!(workspace.shared_brains.items.len(), 1);
    assert_eq!(workspace.shared_brains.items[0].title, "Sierota");
}

#[test]
fn a_disabled_org_contributes_nothing_to_the_forest() {
    let db = fresh_db("forest-disabled");
    seed_org(&db, "o1", "Siema");
    received_container(&db, "o1", "c-space", "Klienci", "space", None);
    received_item(&db, "o1", "i1", "Notatka", None);
    db.set_org_context_enabled("o1", false).unwrap();
    let state = state_with(db);

    let workspace = build_shared_workspace(&state).unwrap();
    assert!(workspace.spaces.is_empty());
    assert!(workspace.shared_brains.items.is_empty());
    assert!(workspace.shared_brains.folders.is_empty());
}

#[test]
fn no_shared_row_carries_an_on_disk_path() {
    // `get_meeting_detail` nulls `audio_path` for a locked meeting because the frontend feeds any
    // path it receives into `convertFileSrc` — the one audio read that bypasses the gate. A shared
    // row must never reopen that door.
    let db = fresh_db("forest-no-paths");
    seed_org(&db, "o1", "Siema");
    received_container(&db, "o1", "c-space", "Klienci", "space", None);
    received_item(&db, "o1", "i1", "Notatka", Some("c-space"));
    let state = state_with(db);

    let json = serde_json::to_string(&build_shared_workspace(&state).unwrap()).unwrap();
    assert!(!json.contains("/Users/"));
    assert!(!json.contains(".wav"));
    assert!(!json.contains(".md"));
    assert!(!json.contains("audioPath"));
}

#[test]
fn a_private_placement_is_reported_and_queues_no_egress() {
    let db = fresh_db("forest-placement");
    seed_org(&db, "o1", "Siema");
    received_container(&db, "o1", "c-space", "Klienci", "space", None);
    db.set_local_placement("o1", "container", "c-space", Some("local-folder"), 4, "t")
        .unwrap();
    let state = state_with(db);

    let workspace = build_shared_workspace(&state).unwrap();
    assert_eq!(
        workspace.spaces[0].local_parent_id.as_deref(),
        Some("local-folder")
    );
    assert_eq!(workspace.spaces[0].position, 4);
    assert!(
        state.db.list_container_shares(None).unwrap().is_empty(),
        "arranging received content on this device must publish nothing"
    );
}

#[test]
fn a_cycle_in_the_replica_cannot_hang_the_reader() {
    // The replica is written from data another device sent. A parent chain that loops — through a
    // bug or a hostile peer — must terminate, not spin the sidebar.
    let db = fresh_db("forest-cycle");
    seed_org(&db, "o1", "Siema");
    received_container(&db, "o1", "a", "A", "space", Some("b"));
    received_container(&db, "o1", "b", "B", "space", Some("a"));
    let state = state_with(db);

    let workspace = build_shared_workspace(&state).unwrap();
    assert!(
        workspace.spaces.is_empty(),
        "every node in a cycle has a known parent, so none is a root"
    );
}

#[test]
fn received_items_inherit_their_containers_access() {
    let db = fresh_db("forest-access");
    seed_org(&db, "o1", "Siema");
    db.upsert_org_container(&OrgContainerRow {
        access: "edit".into(),
        ..{
            received_container(&db, "o1", "c-space", "Klienci", "space", None);
            db.list_org_containers("o1").unwrap().remove(0)
        }
    })
    .unwrap();
    received_item(&db, "o1", "i1", "Notatka", Some("c-space"));
    let state = state_with(db);

    let workspace = build_shared_workspace(&state).unwrap();
    assert_eq!(workspace.spaces[0].access, "edit");
    assert_eq!(
        workspace.spaces[0].items[0].access, "edit",
        "permission is inherited by the whole container, which is the point of sharing one"
    );
}

#[test]
fn a_container_this_device_published_does_not_come_back_as_a_second_copy() {
    // A self-share returns down the feed like anyone else's. Rendering it puts an empty duplicate
    // of the user's own Space beside the real one — two "Sharing things" rows, one with content
    // and one without, which is what a user actually hit.
    let db = fresh_db("self-share");
    seed_org(&db, "o1", "Siema");
    received_container(&db, "o1", "c-mine", "Sharing things", "space", None);
    received_container(&db, "o1", "c-theirs", "Partners", "space", None);
    db.upsert_container_share(&ContainerShareRow {
        id: "cs1".into(),
        org_id: "o1".into(),
        folder_id: "local-folder".into(),
        container_id: "c-mine".into(),
        access: "edit".into(),
        scrub: true,
        is_root: true,
        state: "published".into(),
        item_id: Some("item-c-mine".into()),
        rev: 1,
        generation: 1,
        content_sha256: None,
        position: 0,
        last_error: None,
        created_at: "t".into(),
        updated_at: "t".into(),
    })
    .unwrap();
    let state = state_with(db);

    let workspace = build_shared_workspace(&state).unwrap();
    let names: Vec<String> = workspace.spaces.iter().map(|s| s.name.clone()).collect();
    assert_eq!(
        names,
        vec!["Partners".to_string()],
        "only a container someone ELSE published belongs in the received forest"
    );
}
