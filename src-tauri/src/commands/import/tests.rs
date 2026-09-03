//! Integration oracles for the Notion import orchestration, against a real temp SQLCipher database.
//! No Tauri runtime (progress emits take the `None` handle), no network, no Keychain beyond the
//! debug `MURMUR_DEV_KEK` hatch — so the whole file runs under `cargo test --lib` on any machine.

use super::*;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use crate::settings::AppConfig;
use crate::storage::{Db, NoteFolder};
use zeroize::Zeroizing;

const DB_KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn build_state(tag: &str) -> AppState {
    crate::commands::dev_kek_fixture::ensure_dev_kek();
    let db_path = crate::storage::db::unique_temp_path(&format!("murmur-import-{tag}"), "sqlite");
    let _ = std::fs::remove_file(&db_path);
    AppState {
        recorder: Mutex::new(None),
        recording_stop: Mutex::new(None),
        voice_listener: Mutex::new(None),
        voice_listener_lifecycle: Mutex::new(()),
        recording_starting: std::sync::atomic::AtomicBool::new(false),
        voice_command_capture: Mutex::new(None),
        pending_manual_command: Mutex::new(None),
        live_running: std::sync::atomic::AtomicBool::new(false),
        db: Arc::new(Db::open_with_key(&db_path, DB_KEY).expect("open import test db")),
        config: Arc::new(Mutex::new(AppConfig::default())),
        reasoner: crate::reason::ReasonerCell::fixed(Arc::new(crate::reason::StubReasoner)),
        current_meeting: Mutex::new(None),
        focus_meeting: Mutex::new(None),
        live_transcript: Mutex::new(String::new()),
        live_bullets: Mutex::new(String::new()),
        live_bullets_tracker: Mutex::new(crate::transcribe::bullets::BulletsTracker::default()),
        capped_notified: std::sync::atomic::AtomicBool::new(false),
        capture_fault_notified: std::sync::atomic::AtomicBool::new(false),
        reactions_shadow_count: std::sync::atomic::AtomicU64::new(0),
        reactions_emitted: Mutex::new(HashSet::new()),
        in_flight_turns: Mutex::new(HashMap::new()),
        user_turn_in_progress: std::sync::atomic::AtomicBool::new(false),
        verify_cache: Mutex::new(HashMap::new()),
        unlocked_folders: Arc::new(Mutex::new(HashSet::new())),
        master_kek: Mutex::new(None),
        org_ock_cache: Mutex::new(HashMap::new()),
        account_session: Mutex::new(None),
        share_refresh_lock: tokio::sync::Mutex::new(()),
        org_share_mutation_lock: tokio::sync::Mutex::new(()),
        lifecycle: Mutex::new(()),
        active_salvages: Mutex::new(HashSet::new()),
        seal_epoch: std::sync::atomic::AtomicU64::new(0),
        heavy_inference: Arc::new(tokio::sync::Semaphore::new(1)),
    }
}

/// Materialize a synthetic export directory from `(relative path, body)` pairs.
fn export_dir(tag: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("murmur-export-{tag}-{}", uuid::Uuid::new_v4()));
    for (rel, body) in files {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, body).expect("write");
    }
    root
}

/// A note-folder that is durably LOCKED. `session_unlocked` decides whether this session may write
/// into it — the distinction the birth-seal oracle below turns on.
fn locked_note_folder(state: &AppState, name: &str, session_unlocked: bool) -> String {
    // Release the session master KEK the way an unlock would (via the debug hatch, so no Keychain
    // and no Touch ID): minting the folder content key and every birth-seal below both need it.
    {
        let kek = crate::secrets::get_or_create_master_kek().expect("test master kek");
        *state.master_kek.lock().expect("master kek mutex") = Some(Zeroizing::new(kek));
    }
    let id = uuid::Uuid::new_v4().to_string();
    let folder = NoteFolder {
        id: id.clone(),
        name: name.to_string(),
        path: format!("Notes/{name}"),
        parent_id: Some(state.db.ensure_notes_root().expect("root")),
        locked: true,
        unlocked: false,
        is_root: false,
        kind: "note".into(),
    };
    let wrapped = crate::commands::wrapped_key_for_new_sealed_container(state, &id)
        .expect("wrap a fresh content key");
    state
        .db
        .insert_sealed_note_folder(&folder, &chrono::Utc::now().to_rfc3339(), &wrapped)
        .expect("insert sealed folder");
    if session_unlocked {
        state
            .unlocked_folders
            .lock()
            .expect("unlocked set")
            .insert(id.clone());
    }
    id
}

const PAGE_A: &str = "A abc123def4567890abcdef1234567891.md";
const PAGE_B: &str = "B abc123def4567890abcdef1234567892.md";

/// Two pages that link to each other — the fixture behind the cross-link oracle.
fn two_linked_pages() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            PAGE_A,
            "# Alpha\n\nsee [the beta page](B%20abc123def4567890abcdef1234567892.md)\n",
        ),
        (
            PAGE_B,
            "# Beta\n\nback to [Alpha](A%20abc123def4567890abcdef1234567891.md)\n",
        ),
    ]
}

/// Every note this session can see, read back as raw rows — the same visibility the app has.
fn imported_notes(state: &AppState) -> Vec<crate::storage::NoteRow> {
    let unlocked = state.unlocked_folders.lock().expect("unlocked set").clone();
    state
        .db
        .list_notes_visible(None, &unlocked)
        .expect("list notes")
        .into_iter()
        .filter_map(|n| state.db.get_note_row(&n.id).expect("row"))
        .collect()
}

/// `NoteRow.title` is nullable in the schema; every row this importer writes carries one.
fn title_of(row: &crate::storage::NoteRow) -> &str {
    row.title.as_deref().unwrap_or_default()
}

// ── the happy path ────────────────────────────────────────────────────────────

#[test]
fn imports_pages_as_notes_and_resolves_the_cross_link() {
    let state = build_state("cross-link");
    let dir = export_dir("cross-link", &two_linked_pages());

    let report = run_import_inner(None, &state, ImportSource::Notion, Some(dir.to_str().expect("path")), None, false)
        .expect("import");

    assert_eq!(report.imported, 2, "both pages became notes");
    assert_eq!(report.failed, 0);
    let rows = imported_notes(&state);
    assert_eq!(rows.len(), 2);

    // The link was rewritten to a wikilink pointing at the OTHER page's real title, which only
    // works because the id → title map is built from the whole export before anything is written.
    let alpha = rows
        .iter()
        .find(|r| title_of(r) == "Alpha")
        .expect("alpha imported");
    assert!(
        alpha.text.contains("[[Beta|the beta page]]"),
        "cross-link rewritten, got: {}",
        alpha.text
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stamps_provenance_so_the_page_can_be_recognized_later() {
    let state = build_state("provenance");
    let dir = export_dir("provenance", &two_linked_pages());

    run_import_inner(None, &state, ImportSource::Notion, Some(dir.to_str().expect("path")), None, false).expect("import");

    let found = state
        .db
        .note_by_external_id(ImportSource::Notion.as_str(), "abc123def4567890abcdef1234567891")
        .expect("lookup");
    assert!(found.is_some(), "the Notion page id is stored as provenance");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_re_import_updates_in_place_instead_of_duplicating() {
    // The single loudest complaint in every other Notion importer's issue tracker.
    let state = build_state("reimport");
    let dir = export_dir("reimport", &two_linked_pages());
    let path = dir.to_str().expect("path");

    let first = run_import_inner(None, &state, ImportSource::Notion, Some(path), None, false).expect("first import");
    assert_eq!((first.imported, first.updated), (2, 0));

    let second = run_import_inner(None, &state, ImportSource::Notion, Some(path), None, false).expect("second import");
    assert_eq!(
        (second.imported, second.updated),
        (0, 2),
        "the second run updates the same two notes"
    );
    assert_eq!(
        imported_notes(&state).len(),
        2,
        "no duplicates after re-import"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ── the lock model ────────────────────────────────────────────────────────────

#[test]
fn refuses_a_sealed_and_not_session_unlocked_target_before_writing_anything() {
    let state = build_state("sealed-refusal");
    let folder = locked_note_folder(&state, "Sealed", false);
    let dir = export_dir("sealed-refusal", &two_linked_pages());

    let report = run_import_inner(
        None,
        &state,
        ImportSource::Notion,
        Some(dir.to_str().expect("path")),
        Some(&folder),
        false,
    )
    .expect("the run itself completes; the pages fail");

    assert_eq!(report.imported, 0, "nothing was written behind the lock");
    assert_eq!(report.failed, 2, "every page was refused");
    assert!(
        report.failures.iter().all(|f| f.contains("locked")),
        "refusals name the lock, got: {:?}",
        report.failures
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn birth_seals_every_page_written_into_a_session_unlocked_locked_folder() {
    // THE load-bearing lock oracle. A folder that is durably locked but unlocked for this session
    // is a legitimate, gate-passing write target — and a row written there WITHOUT a `text_blob`
    // would survive a later relock as plaintext at rest, because the reblank pass deliberately
    // refuses to blank a blob-less row (it must never destroy an only copy). Importing a whole
    // workspace through a seam that skips the birth-seal would multiply that by the page count, so
    // every imported row must be sealed from birth.
    let state = build_state("birth-seal");
    let folder = locked_note_folder(&state, "Private", true);
    let dir = export_dir("birth-seal", &two_linked_pages());

    let report = run_import_inner(
        None,
        &state,
        ImportSource::Notion,
        Some(dir.to_str().expect("path")),
        Some(&folder),
        false,
    )
    .expect("import");

    assert_eq!(report.imported, 2, "both pages landed");
    assert_eq!(report.failed, 0, "failures: {:?}", report.failures);
    let rows = imported_notes(&state);
    assert_eq!(rows.len(), 2);
    for row in &rows {
        assert!(
            row.sealed,
            "note {} in a locked folder must carry a recoverable text_blob from birth",
            row.id
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// ── hierarchy ─────────────────────────────────────────────────────────────────

#[test]
fn mirrors_the_page_tree_into_note_folders_and_reuses_them_on_re_import() {
    let state = build_state("hierarchy");
    let files = vec![(
        "Workspace abc123def4567890abcdef1234567811/Team abc123def4567890abcdef1234567812/Note abc123def4567890abcdef1234567813.md",
        "# Deep note\n",
    )];
    let dir = export_dir("hierarchy", &files);
    let path = dir.to_str().expect("path");

    let first = run_import_inner(None, &state, ImportSource::Notion, Some(path), None, true).expect("import");
    assert_eq!(first.imported, 1);
    assert_eq!(first.folders_created, 2, "Workspace + Team");

    let names: Vec<String> = state
        .db
        .list_note_folders()
        .expect("folders")
        .into_iter()
        .map(|f| f.name)
        .collect();
    assert!(names.iter().any(|n| n == "Workspace"), "got {names:?}");
    assert!(names.iter().any(|n| n == "Team"), "got {names:?}");

    let second = run_import_inner(None, &state, ImportSource::Notion, Some(path), None, true).expect("re-import");
    assert_eq!(
        second.folders_created, 0,
        "the second run reuses the folders it made"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ── the wire contract ─────────────────────────────────────────────────────────

#[test]
fn dtos_cross_ipc_in_camel_case() {
    // rust-tauri §2b: a hand-written FE mock DEFINES a shape, it does not verify one. The only
    // honest check is on the producing side — assert the SERIALIZED key names.
    let scan = ImportScanReport {
        pages: 1,
        already_imported: 0,
        attachments: 2,
        attachment_bytes: 3,
        databases: 0,
        csv_all_duplicates: 0,
        nested_archives: 0,
        title_collisions: vec![],
        sample_titles: vec![],
        truncated: false,
        is_murmur_vault: false,
        default_destination: "Imported from Notion".into(),
    };
    let report = ImportReport {
        imported: 1,
        updated: 0,
        skipped: 0,
        failed: 0,
        failures: vec![],
        folders_created: 0,
        cancelled: false,
        embedding_deferred: true,
        destination_id: "f1".into(),
        destination_name: "Imported from Notion".into(),
    };
    for value in [
        serde_json::to_value(&scan).expect("scan json"),
        serde_json::to_value(&report).expect("report json"),
    ] {
        let object = value.as_object().expect("an object");
        for key in object.keys() {
            assert!(
                !key.contains('_'),
                "{key} crosses IPC in snake_case; the FE reads camelCase"
            );
        }
    }
    // Spot-check the keys the FE binds by name, so a rename cannot pass silently.
    let json = serde_json::to_value(&scan).expect("scan json");
    assert!(json.get("alreadyImported").is_some());
    assert!(json.get("attachmentBytes").is_some());
    // The destination trio. These exist so the UI can NAME and OPEN where an import landed; a
    // silent rename here puts the app straight back to "imported: 412" and an empty notes root.
    assert!(json.get("defaultDestination").is_some());
    let json = serde_json::to_value(&report).expect("report json");
    assert!(json.get("destinationId").is_some());
    assert!(json.get("destinationName").is_some());
}

// ── Obsidian ──────────────────────────────────────────────────────────────────

#[test]
fn imports_an_obsidian_vault_and_leaves_its_wikilinks_untouched() {
    // A vault is already in the target format. The only correct transformation is none.
    let state = build_state("obsidian");
    let dir = export_dir(
        "obsidian",
        &[
            ("Plan.md", "# Plan\n\nlinks to [[Other]] already\n"),
            ("Area/Other.md", "# Other\n"),
        ],
    );

    let report = run_import_inner(
        None,
        &state,
        ImportSource::Obsidian,
        Some(dir.to_str().expect("path")),
        None,
        false,
    )
    .expect("import");

    assert_eq!(report.imported, 2);
    assert_eq!(report.failed, 0, "failures: {:?}", report.failures);
    let rows = imported_notes(&state);
    let plan = rows
        .iter()
        .find(|r| title_of(r) == "Plan")
        .expect("plan imported");
    assert!(
        plan.text.contains("[[Other]]"),
        "the vault wikilink survives verbatim, got: {}",
        plan.text
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_obsidian_re_import_matches_on_the_vault_scoped_path() {
    // The vault has no per-note id, so the identity is the VAULT-SCOPED relative path. Re-importing
    // the same vault must update; the bare relative path must NOT be the key, because a second
    // vault holding the same relative path would then overwrite this note.
    let state = build_state("obsidian-reimport");
    let dir = export_dir("obsidian-reimport", &[("Area/Note.md", "# Note\n")]);
    let path = dir.to_str().expect("path");

    let first = run_import_inner(None, &state, ImportSource::Obsidian, Some(path), None, false)
        .expect("first");
    assert_eq!((first.imported, first.updated), (1, 0));

    let second = run_import_inner(None, &state, ImportSource::Obsidian, Some(path), None, false)
        .expect("second");
    assert_eq!((second.imported, second.updated), (0, 1));
    assert_eq!(imported_notes(&state).len(), 1, "no duplicate");

    let scope = crate::import::obsidian::vault_scope(&dir);
    let found = state
        .db
        .note_by_external_id(
            ImportSource::Obsidian.as_str(),
            &format!("{scope}:Area/Note"),
        )
        .expect("lookup");
    assert!(
        found.is_some(),
        "identity is the vault fingerprint plus the vault-relative path"
    );
    let unscoped = state
        .db
        .note_by_external_id(ImportSource::Obsidian.as_str(), "Area/Note")
        .expect("lookup");
    assert!(
        unscoped.is_none(),
        "the bare relative path must NOT match — that is what let a second vault overwrite this one"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn sources_do_not_collide_on_the_same_external_id() {
    // Provenance is (source, external_id), not external_id alone. Two sources that happen to use
    // the same key must produce two notes, not one overwriting the other.
    let state = build_state("source-scoped");
    let dir = export_dir("source-scoped", &[("Note.md", "# Note\n")]);
    let path = dir.to_str().expect("path");

    run_import_inner(None, &state, ImportSource::Obsidian, Some(path), None, false)
        .expect("obsidian");
    let key = format!("{}:Note", crate::import::obsidian::vault_scope(&dir));
    assert!(
        state
            .db
            .note_by_external_id(ImportSource::Obsidian.as_str(), &key)
            .expect("lookup")
            .is_some(),
        "CONTROL: the row IS reachable under its own source"
    );
    let notion = state
        .db
        .note_by_external_id(ImportSource::Notion.as_str(), &key)
        .expect("lookup");
    assert!(
        notion.is_none(),
        "an Obsidian row must not answer a Notion lookup"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_unknown_source_is_refused() {
    // Fail closed: an unrecognized wire value must not silently default to a source.
    assert!(ImportSource::parse("dropbox").is_none());
    assert_eq!(ImportSource::parse("notion"), Some(ImportSource::Notion));
    assert_eq!(ImportSource::parse("obsidian"), Some(ImportSource::Obsidian));
    assert_eq!(
        ImportSource::parse("apple-notes"),
        Some(ImportSource::AppleNotes)
    );
}

// ── the two-vault collision (BLOCKER oracle) ─────────────────────────────────

#[test]
fn a_second_vault_with_a_colliding_relative_path_does_not_overwrite_the_first() {
    // CONTENT-LOSS ORACLE. Two DIFFERENT vaults may both hold `Area/Note.md`. If identity is the
    // vault-RELATIVE path alone, importing the second silently REPLACES the first vault's title and
    // body and reports it as a benign "updated" — no warning, no undo. Identity must be scoped to
    // the vault root, so a second vault creates a NEW note.
    let state = build_state("obsidian-two-vaults");
    let vault_a = export_dir(
        "obsidian-vault-a",
        &[("Area/Note.md", "# Note\n\nvault A body\n")],
    );
    let vault_b = export_dir(
        "obsidian-vault-b",
        &[("Area/Note.md", "# Note\n\nvault B body\n")],
    );

    let first = run_import_inner(
        None,
        &state,
        ImportSource::Obsidian,
        Some(vault_a.to_str().expect("path")),
        None,
        false,
    )
    .expect("first vault");
    assert_eq!((first.imported, first.updated), (1, 0));

    let second = run_import_inner(
        None,
        &state,
        ImportSource::Obsidian,
        Some(vault_b.to_str().expect("path")),
        None,
        false,
    )
    .expect("second vault");
    assert_eq!(
        (second.imported, second.updated),
        (1, 0),
        "a second vault sharing a relative path must CREATE, never overwrite"
    );

    let rows = imported_notes(&state);
    assert_eq!(rows.len(), 2, "both vaults' notes exist");
    assert!(
        rows.iter().any(|r| r.text.contains("vault A body")),
        "the first vault's body survived the second import, got: {:?}",
        rows.iter().map(|r| r.text.as_str()).collect::<Vec<_>>()
    );
    assert!(
        rows.iter().any(|r| r.text.contains("vault B body")),
        "the second vault's body landed"
    );
    let _ = std::fs::remove_dir_all(&vault_a);
    let _ = std::fs::remove_dir_all(&vault_b);
}

#[test]
fn re_importing_the_same_vault_through_a_trailing_slash_still_matches() {
    // The scope must be CANONICAL: a trailing slash is the same vault, not a second identity.
    let state = build_state("obsidian-trailing-slash");
    let dir = export_dir("obsidian-trailing-slash", &[("Area/Note.md", "# Note\n")]);
    let plain = dir.to_str().expect("path").to_string();
    let slashed = format!("{plain}/");

    let first = run_import_inner(None, &state, ImportSource::Obsidian, Some(&plain), None, false)
        .expect("first");
    assert_eq!((first.imported, first.updated), (1, 0));

    let second = run_import_inner(
        None,
        &state,
        ImportSource::Obsidian,
        Some(&slashed),
        None,
        false,
    )
    .expect("second");
    assert_eq!(
        (second.imported, second.updated),
        (0, 1),
        "a trailing slash names the SAME vault"
    );
    assert_eq!(imported_notes(&state).len(), 1, "no duplicate");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn run_import_refuses_murmurs_own_vault_server_side() {
    // The FE-only `isMurmurVault` warning is not a guard: a direct `invoke` bypasses it and reads
    // Murmur's own exported notes back in as duplicates. The refusal must live server-side.
    let state = build_state("own-vault");
    let dir = export_dir("own-vault", &[("Note.md", "# Note\n")]);
    let path = dir.to_str().expect("path").to_string();
    state
        .config
        .lock()
        .expect("config")
        .vault_path
        .replace(path.clone());

    let err = run_import_inner(
        None,
        &state,
        ImportSource::Obsidian,
        Some(&path),
        None,
        false,
    )
    .expect_err("importing Murmur's own vault must be refused");
    assert!(
        matches!(err, AppError::InvalidArg(ref m) if m.contains("vault")),
        "the refusal should name the vault, got {err:?}"
    );
    assert_eq!(imported_notes(&state).len(), 0, "nothing was written");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_folder_that_is_not_the_murmur_vault_still_imports() {
    // CONTROL for the guard above: the ordinary case must keep working with a vault configured.
    let state = build_state("own-vault-control");
    let vault = export_dir("own-vault-control-vault", &[("Exported.md", "# Exported\n")]);
    let other = export_dir("own-vault-control-other", &[("Note.md", "# Note\n")]);
    state
        .config
        .lock()
        .expect("config")
        .vault_path
        .replace(vault.to_str().expect("path").to_string());

    let report = run_import_inner(
        None,
        &state,
        ImportSource::Obsidian,
        Some(other.to_str().expect("path")),
        None,
        false,
    )
    .expect("a different folder imports normally");
    assert_eq!(report.imported, 1);
    let _ = std::fs::remove_dir_all(&vault);
    let _ = std::fs::remove_dir_all(&other);
}

// ── the default destination ───────────────────────────────────────────────────

/// An import the user did not file lands in its OWN badged container, not loose in the notes root.
///
/// Dropping several hundred Notion pages straight into the root mixed them irreversibly with the
/// user's own notes, with nothing recording where they came from. The container is created on
/// demand, named per source, and REUSED on the next run so a re-import updates in place instead of
/// growing "Imported from Notion 2".
#[test]
fn an_unfiled_import_lands_in_a_named_badged_container_and_reuses_it() {
    let state = build_state("import-destination");
    let dir = export_dir("import-destination", &two_linked_pages());
    let path = dir.to_str().expect("path");

    let first = run_import_inner(None, &state, ImportSource::Notion, Some(path), None, false)
        .expect("first import");
    assert_eq!(first.imported, 2);

    let containers = state.db.list_containers().expect("containers");
    let landed: Vec<_> = containers
        .iter()
        .filter(|c| c.name == "Imported from Notion")
        .collect();
    assert_eq!(
        landed.len(),
        1,
        "exactly one destination container, got: {:?}",
        containers.iter().map(|c| &c.name).collect::<Vec<_>>()
    );
    let destination = landed[0];
    assert_eq!(
        destination.emoji.as_deref(),
        Some("\u{1F5C2}\u{FE0F}"),
        "the destination carries its badge"
    );

    // REUSE: a second run of the same export updates in place and creates no second container.
    let second = run_import_inner(None, &state, ImportSource::Notion, Some(path), None, false)
        .expect("second import");
    assert_eq!(
        second.updated, 2,
        "the re-import updated rather than duplicated"
    );
    assert_eq!(
        state
            .db
            .list_containers()
            .expect("containers")
            .iter()
            .filter(|c| c.name == "Imported from Notion")
            .count(),
        1,
        "the second run reused the container instead of making another"
    );

    // CONTROL — an EXPLICIT destination still wins, so the default cannot hijack a chosen folder.
    let chosen = crate::commands::create_note_folder_inner(&state, "Chosen", None)
        .expect("chosen folder");
    let third = run_import_inner(
        None,
        &state,
        ImportSource::Notion,
        Some(path),
        Some(&chosen.id),
        false,
    )
    .expect("third import");
    assert_eq!(
        third.updated, 2,
        "the same pages, filed where the user asked"
    );

    // The report NAMES where the notes landed, in both branches. Without this the UI can only say
    // "imported: 2" — which is what left users hunting an empty notes root for an import that had
    // in fact been filed correctly all along.
    assert_eq!(first.destination_id, destination.id);
    assert_eq!(first.destination_name, "Imported from Notion");
    assert_eq!(
        third.destination_id, chosen.id,
        "an explicit destination is reported as itself, not as the container"
    );
    assert_eq!(third.destination_name, "Chosen");

    let _ = std::fs::remove_dir_all(&dir);
}

/// The picker's default option must name the SAME container the import will actually use.
///
/// These two came from one constant but were read through different call paths — the scan reports
/// the name without creating anything, the import creates it — so a drift between them would show
/// the user one destination and use another. Pinning them together is what keeps the promise the UI
/// makes checkable.
#[test]
fn the_scanned_default_destination_matches_where_the_import_lands() {
    for source in [
        ImportSource::Notion,
        ImportSource::Obsidian,
        ImportSource::AppleNotes,
    ] {
        let state = build_state(&format!("default-dest-{}", source.as_str()));
        let promised = import_container(source).0;
        let landed = import_destination(&state, source)
            .expect("the destination resolves");
        let folder = state
            .db
            .note_folder_by_id(&landed)
            .expect("read the folder")
            .expect("the container is a NOTE folder the importer can resolve");
        assert_eq!(
            folder.name, promised,
            "{} promises {promised} but lands in {}",
            source.as_str(),
            folder.name
        );
    }
}
