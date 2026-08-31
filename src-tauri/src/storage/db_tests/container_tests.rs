//! File-backed tests for SHARED CONTAINERS — the schema, the outbound journal, the inbound
//! manifest replica, and the recipient's private arrangement.
//!
//! Two properties here are load-bearing and are asserted rather than commented:
//!
//! 1. `migrate()` stays idempotent with the new tables (it runs on every open), and every
//!    pre-existing `org_shares` row lands on `explicit = 1` — each of them came from someone
//!    deliberately pressing "Add to Org Brain", so unsharing a container must never withdraw one.
//! 2. A private placement mutates nothing publishable. It is a rendering hint that never reaches
//!    the relay, and the test proves it by checking no share journal row appears.
//!
//! These use `open_with_key` + a fixed literal DEK, so they never touch the Keychain.

use super::*;
use crate::storage::models::{ContainerShareRow, OrgContainerRow, OrgState};

/// The same fixed placeholder the sibling file-backed suites use — the documented
/// MURMUR_DEV_DEK-shaped literal, never a real Keychain DEK.
const TEST_DEK: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"; // MURMUR_DEV_DEK placeholder

fn file_db(label: &str) -> Db {
    Db::open_with_key(
        &crate::storage::db::unique_temp_path(&format!("meetnotes-container-test-{label}"), "sqlite"),
        TEST_DEK,
    )
    .unwrap()
}

fn seed_org(db: &Db, org_id: &str) {
    db.upsert_org_state(&OrgState {
        org_id: org_id.into(),
        name: format!("Org {org_id}"),
        role: "owner".into(),
        joined_at: "2026-08-29T10:00:00Z".into(),
        consented: true,
        last_seq: 0,
        generation: 1,
        context_enabled: true,
    })
    .unwrap();
}

fn share_row(id: &str, org: &str, folder: &str, container: &str, is_root: bool) -> ContainerShareRow {
    ContainerShareRow {
        id: id.into(),
        org_id: org.into(),
        folder_id: folder.into(),
        container_id: container.into(),
        access: "view".into(),
        scrub: true,
        is_root,
        state: "queued".into(),
        item_id: None,
        rev: 1,
        generation: 1,
        content_sha256: None,
        position: 0,
        last_error: None,
        created_at: "2026-08-29T10:00:00Z".into(),
        updated_at: "2026-08-29T10:00:00Z".into(),
    }
}

fn container_row(org: &str, container: &str, parent: Option<&str>) -> OrgContainerRow {
    OrgContainerRow {
        org_id: org.into(),
        container_id: container.into(),
        item_id: format!("item-{container}"),
        level: "space".into(),
        name: "Klienci".into(),
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
    }
}

// ── schema ────────────────────────────────────────────────────────────────────────────────────

#[test]
fn container_share_schema_is_present_and_idempotent() {
    let db = file_db("schema");
    db.migrate().unwrap();
    db.migrate().unwrap();
    let conn = db.lock();
    for table in ["org_container_shares", "org_containers", "org_local_placements"] {
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                [table],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "{table} must exist exactly once");
    }
    for (table, column) in [
        ("org_items", "parent_container_id"),
        ("org_items", "position"),
        ("org_shares", "parent_container_id"),
        ("org_shares", "position"),
        ("org_shares", "explicit"),
    ] {
        let found: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name=?1"),
                [column],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(found, 1, "{table}.{column} must exist");
    }
}

#[test]
fn a_pre_existing_org_share_row_defaults_to_explicit() {
    let db = file_db("explicit-default");
    {
        let conn = db.lock();
        conn.execute(
            "INSERT INTO org_shares(id,org_id,document_id,kind,state,created_at,updated_at)
             VALUES('s1','o1','d1','note','uploaded','t','t')",
            [],
        )
        .unwrap();
    }
    // A second migrate() is what a real upgrade looks like: the column already exists, and the row
    // predates it.
    db.migrate().unwrap();
    let conn = db.lock();
    let explicit: i64 = conn
        .query_row("SELECT explicit FROM org_shares WHERE id='s1'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        explicit, 1,
        "an existing share was published deliberately and must survive an unshare"
    );
    let parent: Option<String> = conn
        .query_row(
            "SELECT parent_container_id FROM org_shares WHERE id='s1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(parent.is_none(), "no container may be guessed for a legacy row");
}

// ── outbound journal ──────────────────────────────────────────────────────────────────────────

#[test]
fn container_share_round_trips_and_upsert_is_idempotent() {
    let db = file_db("outbound-roundtrip");
    let row = share_row("cs1", "o1", "f1", "c1", true);
    db.upsert_container_share(&row).unwrap();
    db.upsert_container_share(&row).unwrap();
    assert_eq!(db.list_container_shares(Some("o1")).unwrap().len(), 1);
    let got = db.get_container_share("o1", "f1").unwrap().unwrap();
    assert_eq!(got, row);
    assert_eq!(
        db.container_share_by_container("o1", "c1").unwrap().unwrap().folder_id,
        "f1"
    );
}

#[test]
fn resharing_a_container_keeps_its_published_identity() {
    let db = file_db("stable-identity");
    db.upsert_container_share(&share_row("cs1", "o1", "f1", "c-original", true))
        .unwrap();
    // A second share attempt arrives with a freshly minted id. The stored document identity is
    // what peers already hold, so it must win — otherwise every re-share would strand a ghost
    // container in every member's sidebar.
    db.upsert_container_share(&share_row("cs1", "o1", "f1", "c-new", true))
        .unwrap();
    assert_eq!(
        db.get_container_share("o1", "f1").unwrap().unwrap().container_id,
        "c-original"
    );
}

#[test]
fn container_share_roots_exclude_descendants() {
    let db = file_db("roots");
    db.upsert_container_share(&share_row("cs1", "o1", "f1", "c1", true))
        .unwrap();
    db.upsert_container_share(&share_row("cs2", "o1", "f2", "c2", false))
        .unwrap();
    let roots = db.list_container_share_roots().unwrap();
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].folder_id, "f1");
    assert_eq!(db.shared_container_folder_ids("o1").unwrap().len(), 2);
}

#[test]
fn container_share_state_advances_and_keeps_a_known_item_id() {
    let db = file_db("state");
    db.upsert_container_share(&share_row("cs1", "o1", "f1", "c1", true))
        .unwrap();
    db.set_container_share_state("cs1", "published", Some("item-1"), 1, Some(&[9u8; 32]), None, "t2")
        .unwrap();
    // A later state change that carries no item id must not erase the one we have.
    db.set_container_share_state("cs1", "revoke_pending", None, 1, None, None, "t3")
        .unwrap();
    let row = db.get_container_share("o1", "f1").unwrap().unwrap();
    assert_eq!(row.state, "revoke_pending");
    assert_eq!(row.item_id.as_deref(), Some("item-1"));
    assert_eq!(row.content_sha256.unwrap().len(), 32);
}

#[test]
fn container_share_access_is_bounded_and_deletable() {
    let db = file_db("access");
    db.upsert_container_share(&share_row("cs1", "o1", "f1", "c1", true))
        .unwrap();
    assert!(db.set_container_share_access("cs1", "galaxy", "t").is_err());
    db.set_container_share_access("cs1", "edit", "t2").unwrap();
    assert_eq!(
        db.get_container_share("o1", "f1").unwrap().unwrap().access,
        "edit"
    );
    db.delete_container_share("cs1").unwrap();
    assert!(db.get_container_share("o1", "f1").unwrap().is_none());
}

// ── inbound replica ───────────────────────────────────────────────────────────────────────────

#[test]
fn tombstoned_org_container_is_not_listed() {
    let db = file_db("inbound-tombstone");
    seed_org(&db, "o1");
    db.upsert_org_container(&container_row("o1", "c1", None)).unwrap();
    assert_eq!(db.list_org_containers("o1").unwrap().len(), 1);
    db.tombstone_org_container("o1", "c1").unwrap();
    assert!(db.list_org_containers("o1").unwrap().is_empty());
}

#[test]
fn a_container_is_tombstoned_by_the_item_that_carried_it() {
    let db = file_db("inbound-tombstone-by-item");
    seed_org(&db, "o1");
    db.upsert_org_container(&container_row("o1", "c1", None)).unwrap();
    assert!(db.tombstone_org_container_by_item("item-c1").unwrap());
    assert!(db.list_org_containers("o1").unwrap().is_empty());
    assert!(
        !db.tombstone_org_container_by_item("item-unknown").unwrap(),
        "an item that never was a container must report no match"
    );
}

#[test]
fn a_disabled_org_contributes_no_containers() {
    let db = file_db("inbound-disabled");
    seed_org(&db, "o1");
    db.upsert_org_container(&container_row("o1", "c1", None)).unwrap();
    db.set_org_context_enabled("o1", false).unwrap();
    assert!(
        db.list_org_containers("o1").unwrap().is_empty(),
        "the per-instance org toggle gates containers exactly as it gates items"
    );
}

#[test]
fn re_ingesting_a_container_updates_it_in_place() {
    let db = file_db("inbound-update");
    seed_org(&db, "o1");
    db.upsert_org_container(&container_row("o1", "c1", None)).unwrap();
    let renamed = OrgContainerRow {
        name: "Klienci 2026".into(),
        rev: 2,
        item_id: "item-c1-v2".into(),
        ..container_row("o1", "c1", None)
    };
    db.upsert_org_container(&renamed).unwrap();
    let rows = db.list_org_containers("o1").unwrap();
    assert_eq!(rows.len(), 1, "a rename supersedes, it does not duplicate");
    assert_eq!(rows[0].name, "Klienci 2026");
    assert_eq!(rows[0].rev, 2);
}

#[test]
fn re_ingesting_a_tombstoned_container_revives_it() {
    // A withdrawn container that its owner shares again must come back, not stay invisible.
    let db = file_db("inbound-revive");
    seed_org(&db, "o1");
    db.upsert_org_container(&container_row("o1", "c1", None)).unwrap();
    db.tombstone_org_container("o1", "c1").unwrap();
    db.upsert_org_container(&container_row("o1", "c1", None)).unwrap();
    assert_eq!(db.list_org_containers("o1").unwrap().len(), 1);
}

#[test]
fn an_unknown_container_level_is_refused() {
    let db = file_db("inbound-level");
    seed_org(&db, "o1");
    let bad = OrgContainerRow {
        level: "galaxy".into(),
        ..container_row("o1", "c1", None)
    };
    assert!(db.upsert_org_container(&bad).is_err());
}

// ── private placement ─────────────────────────────────────────────────────────────────────────

#[test]
fn local_placement_is_one_row_per_target_and_clears() {
    let db = file_db("placement");
    db.set_local_placement("o1", "container", "c1", Some("local-f"), 2, "t")
        .unwrap();
    db.set_local_placement("o1", "container", "c1", Some("other"), 5, "t2")
        .unwrap();
    let rows = db.list_local_placements().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].local_parent_id.as_deref(), Some("other"));
    assert_eq!(rows[0].position, 5);
    db.clear_local_placement("o1", "container", "c1").unwrap();
    assert!(db.list_local_placements().unwrap().is_empty());
}

#[test]
fn a_container_and_a_doc_with_the_same_id_are_different_placements() {
    let db = file_db("placement-keys");
    db.set_local_placement("o1", "container", "x", Some("a"), 0, "t")
        .unwrap();
    db.set_local_placement("o1", "doc", "x", Some("b"), 0, "t")
        .unwrap();
    assert_eq!(db.list_local_placements().unwrap().len(), 2);
}

#[test]
fn local_placement_rejects_an_unknown_target_kind_or_empty_id() {
    let db = file_db("placement-validate");
    assert!(db.set_local_placement("o1", "galaxy", "x", None, 0, "t").is_err());
    assert!(db.set_local_placement("o1", "container", "  ", None, 0, "t").is_err());
    assert!(db.clear_local_placement("o1", "galaxy", "x").is_err());
}

#[test]
fn a_placement_writes_nothing_publishable() {
    // The whole safety argument for private arrangement is that it cannot leak: it must not queue
    // a share, touch a container journal, or otherwise create anything the sweep would upload.
    let db = file_db("placement-no-egress");
    seed_org(&db, "o1");
    db.set_local_placement("o1", "container", "c1", Some("local-f"), 0, "t")
        .unwrap();
    assert!(db.list_container_shares(None).unwrap().is_empty());
    let conn = db.lock();
    let queued: i64 = conn
        .query_row("SELECT COUNT(*) FROM org_shares", [], |r| r.get(0))
        .unwrap();
    assert_eq!(queued, 0, "a private arrangement never queues egress");
}

#[test]
fn orphan_placements_are_pruned_when_their_local_folder_is_gone() {
    let db = file_db("placement-orphan");
    {
        let conn = db.lock();
        conn.execute(
            "INSERT INTO folders(id,name,path,created_at) VALUES('live','Live','live','t')",
            [],
        )
        .unwrap();
    }
    db.set_local_placement("o1", "container", "c-live", Some("live"), 0, "t")
        .unwrap();
    db.set_local_placement("o1", "container", "c-dead", Some("deleted"), 0, "t")
        .unwrap();
    db.set_local_placement("o1", "doc", "d-root", None, 0, "t")
        .unwrap();
    assert_eq!(db.prune_orphan_local_placements().unwrap(), 1);
    let remaining: Vec<String> = db
        .list_local_placements()
        .unwrap()
        .into_iter()
        .map(|r| r.target_id)
        .collect();
    assert!(remaining.contains(&"c-live".to_string()));
    assert!(
        remaining.contains(&"d-root".to_string()),
        "a Shared Brains-rooted placement has no local parent and is never an orphan"
    );
    assert!(!remaining.contains(&"c-dead".to_string()));
}

// ── the 2.1.0 regression: a manifest ingested as a note ───────────────────────────────────────

#[test]
fn a_manifest_mis_ingested_as_a_note_is_repaired_into_a_container() {
    // 2.1.0 added the container branch to the anti-entropy sweep but NOT to the live feed pull,
    // and the live pull is the path a healthy replica actually uses. A shared Space therefore
    // arrived as a note named after the folder. Existing databases hold those rows, so the
    // migration has to heal them rather than only stopping new ones.
    let db = file_db("repair-mis-ingested");
    seed_org(&db, "o1");
    let manifest = crate::share::container_envelope::ContainerEnvelope {
        v: crate::share::container_envelope::CONTAINER_ENVELOPE_VERSION,
        container_id: "c-space".into(),
        level: crate::share::container_envelope::ContainerLevel::Space,
        name: "Pomysły".into(),
        emoji: None,
        tint: None,
        parent_container_id: None,
        position: 0,
    };
    {
        let conn = db.lock();
        conn.execute(
            "INSERT INTO org_items(item_id, org_id, seq, author_hint, title, markdown, created_at,
                                   rev, generation, is_current, tombstoned, source_kind, access)
             VALUES ('item-1','o1',7,'kgm004a','Pomysły',?1,'2026-08-29T10:00:00Z',1,1,1,0,
                     'container','edit')",
            rusqlite::params![manifest.to_json()],
        )
        .unwrap();
    }

    db.migrate().unwrap();

    let containers = db.list_org_containers("o1").unwrap();
    assert_eq!(containers.len(), 1, "the manifest became a container");
    assert_eq!(containers[0].container_id, "c-space");
    assert_eq!(containers[0].name, "Pomysły");
    assert_eq!(containers[0].level, "space");
    assert_eq!(containers[0].access, "edit", "the access it arrived with is kept");
    assert_eq!(containers[0].seq, 7);

    assert!(
        db.list_org_items("o1").unwrap().is_empty(),
        "the mis-ingested row no longer renders as a note"
    );

    // Tombstoned, not deleted: a later feed replay of the same server item must be idempotent
    // rather than resurrect the note.
    let conn = db.lock();
    let tombstoned: i64 = conn
        .query_row(
            "SELECT tombstoned FROM org_items WHERE item_id='item-1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(tombstoned, 1);
}

#[test]
fn the_repair_leaves_a_row_it_cannot_parse_exactly_as_it_is() {
    // A repair that cannot understand a row has no business rewriting it.
    let db = file_db("repair-unparseable");
    seed_org(&db, "o1");
    {
        let conn = db.lock();
        conn.execute(
            "INSERT INTO org_items(item_id, org_id, seq, author_hint, title, markdown, created_at,
                                   rev, generation, is_current, tombstoned, source_kind)
             VALUES ('item-bad','o1',1,'h','t','{not json','now',1,1,1,0,'container')",
            [],
        )
        .unwrap();
    }
    db.migrate().unwrap();
    assert!(db.list_org_containers("o1").unwrap().is_empty());
    let conn = db.lock();
    let tombstoned: i64 = conn
        .query_row(
            "SELECT tombstoned FROM org_items WHERE item_id='item-bad'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(tombstoned, 0, "an unparseable row is left untouched");
}

#[test]
fn a_container_row_never_renders_as_an_item_even_if_one_survives() {
    // Belt-and-braces for the reader itself: whatever the repair did or did not do, a row marked
    // as a container must not come back from the item list.
    let db = file_db("reader-excludes-container");
    seed_org(&db, "o1");
    {
        let conn = db.lock();
        conn.execute(
            "INSERT INTO org_items(item_id, org_id, seq, author_hint, title, markdown, created_at,
                                   rev, generation, is_current, tombstoned, source_kind)
             VALUES ('item-c','o1',1,'h','Pomysły','{not json','now',1,1,1,0,'container')",
            [],
        )
        .unwrap();
    }
    assert!(db.list_org_items("o1").unwrap().is_empty());
}

#[test]
fn placement_is_backfilled_for_documents_this_device_published() {
    // 2.1.1 taught the live pull to WRITE placement, but only for items it ingests from then on.
    // An already-converged item is never re-read, so its missing placement is never filled in —
    // and the visible result is a shared Space that arrives EMPTY while its documents sit loose.
    let db = file_db("backfill-placement");
    seed_org(&db, "o1");
    {
        let conn = db.lock();
        conn.execute(
            "INSERT INTO org_items(item_id, org_id, seq, author_hint, title, markdown, created_at,
                                   rev, generation, is_current, tombstoned, source_kind)
             VALUES ('item-doc','o1',3,'h','Note-share','body','t',1,1,1,0,'document')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO org_shares(id,org_id,document_id,kind,state,item_id,
                                    parent_container_id,position,explicit,created_at,updated_at)
             VALUES ('s1','o1','d1','note','uploaded','item-doc','c-space',4,0,'t','t')",
            [],
        )
        .unwrap();
    }

    db.migrate().unwrap();

    let placement = db.org_item_container_placement("item-doc").unwrap().unwrap();
    assert_eq!(placement.0.as_deref(), Some("c-space"));
    assert_eq!(placement.1, 4, "the position travels with the container");
}

#[test]
fn the_backfill_never_invents_a_placement_for_a_standalone_share() {
    // A document shared on its own has no container, and the repair must not give it one.
    let db = file_db("backfill-standalone");
    seed_org(&db, "o1");
    {
        let conn = db.lock();
        conn.execute(
            "INSERT INTO org_items(item_id, org_id, seq, author_hint, title, markdown, created_at,
                                   rev, generation, is_current, tombstoned, source_kind)
             VALUES ('item-solo','o1',1,'h','Solo','body','t',1,1,1,0,'document')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO org_shares(id,org_id,document_id,kind,state,item_id,explicit,
                                    created_at,updated_at)
             VALUES ('s2','o1','d2','note','uploaded','item-solo',1,'t','t')",
            [],
        )
        .unwrap();
    }
    db.migrate().unwrap();
    let placement = db.org_item_container_placement("item-solo").unwrap().unwrap();
    assert!(placement.0.is_none(), "a standalone share stays uncontained");
}
