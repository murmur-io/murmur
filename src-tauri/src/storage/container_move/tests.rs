use super::*;
const KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

struct Fixture {
    root: PathBuf,
    db_path: PathBuf,
    vault: PathBuf,
}
impl Fixture {
    fn new() -> (Self, Db) {
        let root = crate::storage::db::unique_temp_path("container-move", "test");
        std::fs::create_dir_all(&root).unwrap();
        let vault = root.join("vault");
        std::fs::create_dir(&vault).unwrap();
        let db_path = root.join("db.sqlite");
        let db = Db::open_with_key(&db_path, KEY).unwrap();
        db.lock().execute("DELETE FROM folders", []).unwrap();
        (
            Self {
                root,
                db_path,
                vault,
            },
            db,
        )
    }
    fn reopen(&self) -> Db {
        Db::open_with_key(&self.db_path, KEY).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn folder(db: &Db, id: &str, path: &str, parent: Option<&str>, kind: &str) {
    db.lock().execute("INSERT INTO folders(id,name,path,parent_id,kind,level,created_at) VALUES(?1,?1,?2,?3,?4,'folder','2026-09-18')", params![id,path,parent,kind]).unwrap();
}
fn prepare(db: &Db, f: &Fixture) {
    folder(db, "space", "Space", None, "meeting");
    folder(db, "moving", "Sprzedaż", None, "meeting");
    folder(db, "child", "Sprzedaż/Łódź", Some("moving"), "note");
    let child = f.vault.join("Sprzedaż/Łódź");
    std::fs::create_dir_all(&child).unwrap();
    std::fs::create_dir(f.vault.join("Space")).unwrap();
    std::fs::write(child.join("note.md"), b"# original\n\0\xff").unwrap();
    std::fs::write(child.join("meeting.md"), "# Nagranie — óż").unwrap();
    db.lock().execute("INSERT INTO documents(id,folder_id,name,kind,created_at,exported_path) VALUES('doc','child','note','note',0,?1)", [child.join("note.md").to_str().unwrap()]).unwrap();
    db.lock().execute("INSERT INTO meetings(id,started_at,status,folder_id) VALUES('meeting','2026-09-18','summarized','child')", []).unwrap();
    db.lock().execute("INSERT INTO notes(meeting_id,provider_id,markdown,created_at,exported_path) VALUES('meeting','test','content','2026-09-18',?1)", [child.join("meeting.md").to_str().unwrap()]).unwrap();
}
fn assert_moved(db: &Db, f: &Fixture) {
    assert_eq!(
        db.folder_by_id("moving").unwrap().unwrap().path,
        "Space/Sprzedaż"
    );
    assert_eq!(
        db.folder_by_id("child").unwrap().unwrap().path,
        "Space/Sprzedaż/Łódź"
    );
    let new = f.vault.join("Space/Sprzedaż/Łódź");
    assert_eq!(
        std::fs::read(new.join("note.md")).unwrap(),
        b"# original\n\0\xff"
    );
    assert_eq!(
        std::fs::read_to_string(new.join("meeting.md")).unwrap(),
        "# Nagranie — óż"
    );
    let conn = db.lock();
    let note: String = conn
        .query_row(
            "SELECT exported_path FROM documents WHERE id='doc'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let meeting: String = conn
        .query_row(
            "SELECT exported_path FROM notes WHERE meeting_id='meeting'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(Path::new(&note), new.join("note.md"));
    assert_eq!(Path::new(&meeting), new.join("meeting.md"));
}

#[test]
fn unicode_mixed_subtree_moves_bytes_and_both_export_kinds() {
    let (f, db) = Fixture::new();
    prepare(&db, &f);
    db.move_container_local("moving", Some("space"), Some(&f.vault))
        .unwrap();
    assert_moved(&db, &f);
    assert!(!f.vault.join("Sprzedaż").exists());
    // The path read by lock cleanup now addresses the real file, not the old plaintext location.
    for path in db.note_exported_paths_in_folder("child").unwrap() {
        std::fs::remove_file(path).unwrap();
    }
    assert!(!f.vault.join("Space/Sprzedaż/Łódź/note.md").exists());
}

#[test]
fn durable_recovery_at_every_commit_boundary() {
    for boundary in ["prepared", "moved", "committed", "acknowledgement"] {
        let (f, db) = Fixture::new();
        prepare(&db, &f);
        assert!(db
            .move_container_local_at_boundary(
                "moving",
                Some("space"),
                Some(&f.vault),
                Some(boundary)
            )
            .is_err());
        drop(db);
        let db = f.reopen();
        if matches!(boundary, "prepared" | "moved") {
            assert_eq!(
                db.folder_by_id("moving").unwrap().unwrap().parent_id,
                None,
                "startup migrations must preserve journaled ancestry until recovery"
            );
        }
        db.recover_container_moves().unwrap();
        if boundary == "prepared" {
            assert_eq!(db.folder_by_id("moving").unwrap().unwrap().path, "Sprzedaż");
            assert_eq!(
                std::fs::read(f.vault.join("Sprzedaż/Łódź/note.md")).unwrap(),
                b"# original\n\0\xff"
            );
        } else {
            assert_moved(&db, &f);
        }
        db.recover_container_moves().unwrap();
        assert_eq!(
            db.lock()
                .query_row("SELECT count(*) FROM container_move_journal", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}

#[test]
fn refuses_collision_cycle_reserved_and_sealed_subtrees() {
    let (f, db) = Fixture::new();
    prepare(&db, &f);
    assert!(db
        .move_container_local("moving", Some("moving"), Some(&f.vault))
        .is_err());
    assert!(db
        .move_container_local("moving", Some("child"), Some(&f.vault))
        .is_err());
    db.lock()
        .execute("UPDATE folders SET locked=1 WHERE id='child'", [])
        .unwrap();
    assert!(matches!(
        db.move_container_local("moving", Some("space"), Some(&f.vault)),
        Err(AppError::Locked(_))
    ));
    db.lock()
        .execute("UPDATE folders SET locked=0 WHERE id='child'", [])
        .unwrap();
    db.lock()
        .execute("UPDATE folders SET locked=1 WHERE id='space'", [])
        .unwrap();
    assert!(matches!(
        db.move_container_local("moving", Some("space"), Some(&f.vault)),
        Err(AppError::Locked(_))
    ));
    db.lock()
        .execute("UPDATE folders SET locked=0 WHERE id='space'", [])
        .unwrap();
    db.lock()
        .execute("UPDATE folders SET is_root=1 WHERE id='moving'", [])
        .unwrap();
    assert!(db
        .move_container_local("moving", Some("space"), Some(&f.vault))
        .is_err());
    db.lock()
        .execute("UPDATE folders SET is_root=0 WHERE id='moving'", [])
        .unwrap();
    std::fs::create_dir_all(f.vault.join("Space/Sprzedaż")).unwrap();
    std::fs::write(f.vault.join("Space/Sprzedaż/external.md"), "external").unwrap();
    assert!(db
        .move_container_local("moving", Some("space"), Some(&f.vault))
        .is_err());
    assert_eq!(
        std::fs::read_to_string(f.vault.join("Space/Sprzedaż/external.md")).unwrap(),
        "external"
    );
    assert_eq!(db.folder_by_id("moving").unwrap().unwrap().path, "Sprzedaż");
}

#[test]
fn root_maps_by_kind_and_updates_hierarchy_levels() {
    let (f, db) = Fixture::new();
    folder(&db, "space", "Space", None, "meeting");
    folder(&db, "meeting", "Space/Meeting", Some("space"), "meeting");
    folder(&db, "note", "Space/Note", Some("space"), "note");
    db.move_container_local("meeting", None, Some(&f.vault))
        .unwrap();
    assert_eq!(db.folder_by_id("meeting").unwrap().unwrap().parent_id, None);
    assert_eq!(db.folder_levels().unwrap()["meeting"], "project");
    db.move_container_local("meeting", Some("space"), Some(&f.vault))
        .unwrap();
    assert_eq!(db.folder_levels().unwrap()["meeting"], "folder");
    db.move_container_local("note", None, Some(&f.vault))
        .unwrap();
    assert_eq!(
        db.folder_by_id("note").unwrap().unwrap().parent_id,
        db.note_root_id().unwrap()
    );
    assert_eq!(db.folder_levels().unwrap()["note"], "folder");
}

#[test]
fn recovery_never_merges_two_occupied_names() {
    let (f, db) = Fixture::new();
    prepare(&db, &f);
    assert!(db
        .move_container_local_at_boundary("moving", Some("space"), Some(&f.vault), Some("moved"))
        .is_err());
    std::fs::create_dir(f.vault.join("Sprzedaż")).unwrap();
    std::fs::write(f.vault.join("Sprzedaż/external.md"), "external").unwrap();
    drop(db);
    let db = f.reopen();
    assert!(db.recover_container_moves().is_err());
    assert_eq!(
        std::fs::read_to_string(f.vault.join("Sprzedaż/external.md")).unwrap(),
        "external"
    );
    assert_eq!(
        std::fs::read(f.vault.join("Space/Sprzedaż/Łódź/note.md")).unwrap(),
        b"# original\n\0\xff"
    );
}

#[test]
fn db_commit_failure_rolls_back_the_directory_without_losing_exports() {
    let (f, db) = Fixture::new();
    prepare(&db, &f);
    db.lock().execute_batch("CREATE TRIGGER refuse_test_move BEFORE UPDATE OF path ON folders BEGIN SELECT RAISE(ABORT,'injected database failure'); END").unwrap();
    assert!(db
        .move_container_local("moving", Some("space"), Some(&f.vault))
        .is_err());
    assert!(f.vault.join("Sprzedaż/Łódź/note.md").exists());
    assert!(!f.vault.join("Space/Sprzedaż").exists());
    assert_eq!(db.folder_by_id("moving").unwrap().unwrap().path, "Sprzedaż");
    assert_eq!(
        db.lock()
            .query_row("SELECT count(*) FROM container_move_journal", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn traversal_and_symlink_destinations_refuse_before_touching_bytes() {
    let (f, db) = Fixture::new();
    prepare(&db, &f);
    folder(&db, "bad", "../outside", None, "meeting");
    assert!(db
        .move_container_local("moving", Some("bad"), Some(&f.vault))
        .is_err());
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&f.root, f.vault.join("Link")).unwrap();
        folder(&db, "link", "Link", None, "meeting");
        assert!(db
            .move_container_local("moving", Some("link"), Some(&f.vault))
            .is_err());
    }
    assert!(f.vault.join("Sprzedaż/Łódź/note.md").exists());
}

#[test]
fn provider_placement_diverging_from_canonical_meeting_keeps_export_cleanup_authority() {
    let (f, db) = Fixture::new();
    prepare(&db, &f);
    db.lock()
        .execute(
            "UPDATE notes SET folder_id='child' WHERE meeting_id='meeting'",
            [],
        )
        .unwrap();
    db.lock()
        .execute(
            "UPDATE meetings SET folder_id='space' WHERE id='meeting'",
            [],
        )
        .unwrap();
    db.move_container_local("moving", Some("space"), Some(&f.vault))
        .unwrap();
    assert_moved(&db, &f);
}

#[test]
fn committed_database_with_old_only_filesystem_converges_forward() {
    let (f, db) = Fixture::new();
    prepare(&db, &f);
    assert!(db
        .move_container_local_at_boundary(
            "moving",
            Some("space"),
            Some(&f.vault),
            Some("committed")
        )
        .is_err());
    std::fs::rename(f.vault.join("Space/Sprzedaż"), f.vault.join("Sprzedaż")).unwrap();
    drop(db);
    let db = f.reopen();
    db.recover_container_moves().unwrap();
    assert_moved(&db, &f);
}
