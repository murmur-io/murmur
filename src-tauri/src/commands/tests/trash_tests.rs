//! Trash oracles.
//!
//! These are deliberately the checks that would FAIL if the feature's two load-bearing claims broke:
//!
//! 1. **Content survives the round-trip.** `delete → restore` must return the recording/note
//!    byte-identically, including every provider note and transcript segment. Without this test the
//!    trash is a promise nobody measured — and a snapshot bug is silent until a user tries to
//!    restore, which is the worst possible moment to discover it.
//! 2. **The lock still holds.** A snapshot is plaintext content, so a trashed item from a locked
//!    folder must be MASKED on read and REFUSED on restore, and its plaintext must not be at rest.
//!    This is the `lock-model.md` "gate every read" invariant applied to a new read path.
//!
//! Plus the wire-contract oracle (`rust-tauri.md` §2b) — a serialized-key-name assertion, not a
//! round-trip through the same Rust type, which passes regardless of naming.

use super::*;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use crate::settings::AppConfig;
use crate::storage::{Db, Folder, Meeting, MeetingStatus, NoteRecord};
use crate::transcribe::types::Segment;

// BUILT, not written out: a literal 64-hex string in a diff is what the repo's secret scanner
// exists to catch, and a test fixture is not worth teaching it to ignore that shape. These are
// throwaway keys for a temp SQLCipher file, but they must not LOOK like a real DEK/KEK.
fn db_key() -> String {
    "0123456789abcdef".repeat(4)
}

fn build_state(tag: &str) -> AppState {
    crate::commands::dev_kek_fixture::ensure_dev_kek();
    let db_path = crate::storage::db::unique_temp_path(&format!("murmur-trash-{tag}"), "sqlite");
    let _ = std::fs::remove_file(&db_path);
    let db = Db::open_with_key(&db_path, &db_key()).expect("open trash test db");
    db.migrate().expect("migrate trash test db");
    AppState {
        recorder: Mutex::new(None),
        recording_stop: Mutex::new(None),
        voice_listener: Mutex::new(None),
        voice_listener_lifecycle: Mutex::new(()),
        recording_starting: std::sync::atomic::AtomicBool::new(false),
        voice_command_capture: Mutex::new(None),
        pending_manual_command: Mutex::new(None),
        live_running: std::sync::atomic::AtomicBool::new(false),
        db: Arc::new(db),
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

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("trash test runtime")
        .block_on(future)
}

fn open_folder(state: &AppState, id: &str, name: &str) {
    state
        .db
        .insert_folder(&Folder {
            id: id.to_string(),
            name: name.to_string(),
            path: name.to_string(),
            parent_id: None,
            locked: false,
            created_at: "2026-08-31T09:00:00Z".to_string(),
        })
        .expect("insert folder");
}

fn seed_visible_shared_document(
    state: &AppState,
    org_id: &str,
    doc_id: &str,
    item_id: &str,
    seq: u64,
    title: &str,
) -> String {
    state
        .db
        .upsert_org_state(&crate::storage::OrgState {
            org_id: org_id.to_string(),
            name: "Shared space".to_string(),
            role: "member".to_string(),
            joined_at: "2026-08-31T09:00:00Z".to_string(),
            consented: true,
            last_seq: 0,
            generation: 1,
            context_enabled: true,
        })
        .expect("join org");
    state
        .db
        .upsert_org_item(
            item_id,
            org_id,
            seq,
            "owner",
            title,
            "private shared body",
            "2026-08-31T09:01:00Z",
            seq as u32,
            1,
            &[7; 32],
            None,
            Some("owner"),
            None,
        )
        .expect("ingest Shared document");
    state
        .db
        .set_org_item_document_metadata(item_id, Some(doc_id), "view", Some("owner"))
        .expect("stamp stable Shared identity");
    state
        .db
        .repair_org_reconcile_metadata(
            item_id,
            org_id,
            1,
            Some(doc_id),
            "view",
            Some("owner"),
            true,
        )
        .expect("mark current Shared revision");
    let endpoint = format!("{org_id}:{doc_id}");
    assert!(state.db.org_link_target_visible(&endpoint).unwrap().is_some());
    endpoint
}

fn seed_meeting(state: &AppState, id: &str, folder_id: Option<&str>) {
    state
        .db
        .insert_meeting(&Meeting {
            id: id.to_string(),
            started_at: "2026-08-31T10:00:00Z".to_string(),
            ended_at: Some("2026-08-31T10:30:00Z".to_string()),
            title: Some("Roadmap review".to_string()),
            duration_s: 1800,
            audio_path: None,
            status: MeetingStatus::Summarized,
            folder_id: folder_id.map(|s| s.to_string()),
        })
        .expect("insert meeting");
    if let Some(f) = folder_id {
        state.db.set_meeting_folder(id, Some(f)).expect("file it");
    }
    state
        .db
        .insert_segments(
            id,
            &[
                Segment {
                    idx: 0,
                    start_s: 0.0,
                    end_s: 2.5,
                    text: "We should ship the trash feature.".to_string(),
                    speaker: Some("me".to_string()),
                    confidence: Some(0.91),
                },
                Segment {
                    idx: 1,
                    start_s: 2.5,
                    end_s: 6.0,
                    text: "Agreed — with a thirty day window.".to_string(),
                    speaker: Some("others".to_string()),
                    confidence: None,
                },
            ],
        )
        .expect("insert segments");
    // TWO provider notes: a snapshot that kept only the newest would silently lose the other, which
    // is exactly the content loss `note_records_for_meeting` exists to prevent.
    for (provider, body) in [("claude_code", "# Notes\n\nShip it."), ("ollama", "# Alt\n\nLocal.")] {
        state
            .db
            .upsert_note(&NoteRecord {
                meeting_id: id.to_string(),
                provider_id: provider.to_string(),
                markdown: body.to_string(),
                created_at: "2026-08-31T10:31:00Z".to_string(),
                exported_path: None,
                ..Default::default()
            })
            .expect("upsert note");
    }
    state
        .db
        .set_timeline_data(id, r#"{"lanes":[{"speaker":"me"}]}"#)
        .expect("timeline");
    state.db.set_manual_notes(id, "my own jottings").expect("manual");
    state
        .db
        .set_meeting_tags(id, &["planning".to_string()])
        .expect("tags");
}

/// ORACLE 1 — a deleted recording comes back COMPLETE. Segments (with their confidence), BOTH
/// provider notes, the timeline, manual notes and tags all survive delete → restore.
///
/// This is the test that fails if the snapshot format ever drops a field.
#[test]
fn meeting_delete_then_restore_round_trips_content() {
    let state = build_state("meeting-roundtrip");
    open_folder(&state, "f1", "Work");
    seed_meeting(&state, "m1", Some("f1"));

    let before_segments = state.db.get_segments("m1").unwrap();
    let before_notes = state.db.note_records_for_meeting("m1").unwrap();
    let before_timeline = state.db.get_timeline_data("m1").unwrap();
    let before_manual = state.db.get_manual_notes("m1").unwrap();
    let before_tags = state.db.get_meeting_tags("m1").unwrap();
    assert_eq!(before_notes.len(), 2, "seeded two provider notes");

    block_on(delete_meeting_inner(&state, "m1")).expect("delete to trash");

    // The rows are genuinely GONE — that is what keeps every derived read (FTS, graph, Ask, MCP)
    // correct without a single new predicate.
    assert!(state.db.get_meeting("m1").unwrap().is_none(), "row removed");
    assert!(state.db.get_segments("m1").unwrap().is_empty(), "segments removed");

    let entries = state.db.list_trash_entries().unwrap();
    assert_eq!(entries.len(), 1, "exactly one trash entry");
    assert_eq!(entries[0].kind, "meeting");
    assert_eq!(entries[0].source_id, "m1");
    assert_eq!(entries[0].label, "Roadmap review");
    assert!(!entries[0].is_sealed(), "open folder ⇒ snapshot not sealed");

    block_on(restore_trash_item_inner(&state, &entries[0].id)).expect("restore");

    let m = state.db.get_meeting("m1").unwrap().expect("meeting is back");
    assert_eq!(m.title.as_deref(), Some("Roadmap review"));
    assert_eq!(m.duration_s, 1800);
    assert_eq!(m.folder_id.as_deref(), Some("f1"), "re-filed into its folder");

    let after_segments = state.db.get_segments("m1").unwrap();
    assert_eq!(after_segments.len(), before_segments.len());
    for (a, b) in after_segments.iter().zip(before_segments.iter()) {
        assert_eq!(a.text, b.text, "segment text byte-identical");
        assert_eq!(a.speaker, b.speaker, "speaker attribution preserved");
        assert_eq!(a.confidence, b.confidence, "ASR confidence preserved");
        assert_eq!(a.idx, b.idx);
    }
    let after_notes = state.db.note_records_for_meeting("m1").unwrap();
    assert_eq!(after_notes.len(), 2, "BOTH provider notes restored");
    for (a, b) in after_notes.iter().zip(before_notes.iter()) {
        assert_eq!(a.provider_id, b.provider_id);
        assert_eq!(a.markdown, b.markdown, "note markdown byte-identical");
    }
    assert_eq!(state.db.get_timeline_data("m1").unwrap(), before_timeline);
    assert_eq!(state.db.get_manual_notes("m1").unwrap(), before_manual);
    assert_eq!(state.db.get_meeting_tags("m1").unwrap(), before_tags);

    assert!(
        state.db.list_trash_entries().unwrap().is_empty(),
        "the entry is consumed by a successful restore"
    );
}

/// ORACLE 2 — deleting a recording does NOT remove its audio, because the snapshot references those
/// files by path and they are the recording's only copy. A regression here is unrecoverable audio
/// loss that no restore can fix, and it would not show up in any content assertion.
#[test]
fn meeting_delete_to_trash_keeps_audio_on_disk() {
    let state = build_state("audio-kept");
    open_folder(&state, "f1", "Work");
    let wav = crate::storage::db::unique_temp_path("murmur-trash-audio", "wav");
    let wav = wav.to_string_lossy().to_string();
    std::fs::write(&wav, b"RIFF....fake pcm").expect("write wav");
    seed_meeting(&state, "m1", Some("f1"));
    state
        .db
        .set_meeting_audio_path("m1", Some(&wav))
        .expect("set audio path");

    block_on(delete_meeting_inner(&state, "m1")).expect("delete to trash");
    assert!(
        std::path::Path::new(&wav).exists(),
        "a to-trash delete must LEAVE the audio on disk — it is the only copy"
    );

    // Purging is what finally removes it.
    let entry_id = state.db.list_trash_entries().unwrap()[0].id.clone();
    block_on(purge_one_for_test(&state, &entry_id)).expect("purge");
    assert!(
        !std::path::Path::new(&wav).exists(),
        "purge removes the audio — the cleanup moved to the moment content stops being recoverable"
    );
    let _ = std::fs::remove_file(&wav);
}

/// Databases created before the one-journal capture guard may already contain duplicate snapshots.
/// Both point at the same on-disk audio, so purging either one must fail closed. Restoring the two
/// journals in turn is safe and leaves the shared recording bytes intact.
#[test]
fn legacy_duplicate_meeting_journals_cannot_purge_shared_audio() {
    let state = build_state("duplicate-journal-audio");
    open_folder(&state, "f1", "Work");
    let wav = crate::storage::db::unique_temp_path("murmur-trash-duplicate-audio", "wav");
    let wav = wav.to_string_lossy().to_string();
    std::fs::write(&wav, b"RIFF....shared fake pcm").expect("write wav");
    seed_meeting(&state, "m1", Some("f1"));
    state
        .db
        .set_meeting_audio_path("m1", Some(&wav))
        .expect("set audio path");

    block_on(delete_meeting_inner(&state, "m1")).expect("delete to trash");
    let first = state.db.list_trash_entries().unwrap()[0].clone();
    let duplicate_id = "legacy-duplicate-journal";
    state
        .db
        .lock()
        .execute(
            "INSERT INTO trash_items
               (id,kind,source_id,source_folder_id,label,label_blob,payload,payload_blob,deleted_at)
             SELECT ?1,kind,source_id,source_folder_id,label,label_blob,payload,payload_blob,deleted_at
               FROM trash_items WHERE id = ?2",
            rusqlite::params![duplicate_id, first.id],
        )
        .expect("seed a duplicate produced by an older build");
    assert_eq!(state.db.count_trash_entries_for_source("m1").unwrap(), 2);

    let error = block_on(purge_one_for_test(&state, &first.id))
        .expect_err("purge cannot guess which duplicate owns the audio");
    assert!(matches!(error, AppError::Unavailable(_)));
    assert!(std::path::Path::new(&wav).exists(), "audio remains recoverable");
    assert_eq!(state.db.count_trash_entries_for_source("m1").unwrap(), 2);

    block_on(restore_trash_item_inner(&state, &first.id)).expect("restore first journal");
    block_on(restore_trash_item_inner(&state, duplicate_id))
        .expect("consume matching duplicate through retry restore");
    assert!(state.db.get_meeting("m1").unwrap().is_some());
    assert_eq!(state.db.count_trash_entries_for_source("m1").unwrap(), 0);
    assert!(std::path::Path::new(&wav).exists(), "restored meeting still owns audio");
    let _ = std::fs::remove_file(&wav);
}

/// ORACLE 3 (RED-before-GREEN for the leak class) — a trashed item whose folder is LOCKED after the
/// delete must be MASKED on read and REFUSED on restore, and its plaintext must be gone from the
/// row. This is the gap the capture-time seal alone cannot close: at capture time the folder was
/// open, so only `seal_folder_extras` calling `seal_trash_in_folder` closes it.
#[test]
fn trash_entry_is_sealed_and_masked_when_its_folder_locks_afterwards() {
    let state = build_state("seal-after-lock");
    open_folder(&state, "f1", "Work");
    seed_meeting(&state, "m1", Some("f1"));
    block_on(delete_meeting_inner(&state, "m1")).expect("delete to trash");

    let entry_id = state.db.list_trash_entries().unwrap()[0].id.clone();
    let stored = state.db.get_trash_entry(&entry_id).unwrap().unwrap();
    assert!(
        stored.payload.contains("We should ship the trash feature."),
        "control: while the folder is OPEN the snapshot plaintext IS readable — \
         so the assertions below are measuring the seal, not an already-empty row"
    );

    // Lock the folder. `seal_folder_extras` is the production path that seals every governed
    // artifact; the trash entry must be one of them.
    let ck = [9u8; 32];
    seal_folder_extras(&state.db, "f1", &ck).expect("seal the folder's extras");

    let sealed = state.db.get_trash_entry(&entry_id).unwrap().unwrap();
    assert!(sealed.is_sealed(), "the snapshot is now ciphertext");
    assert!(
        sealed.payload.is_empty() && sealed.label.is_empty(),
        "no plaintext snapshot or label left at rest behind the lock"
    );
    assert!(
        !String::from_utf8_lossy(sealed.payload_blob.as_deref().unwrap())
            .contains("We should ship"),
        "the blob is real ciphertext, not the plaintext moved to another column"
    );

    // Mark the folder locked so the read gate sees it, WITHOUT session-unlocking it.
    state
        .db
        .set_folder_locked_for_test("f1", true)
        .expect("mark locked");

    let dto = list_trash_inner(&state).expect("list");
    assert_eq!(dto.len(), 1);
    assert!(dto[0].locked, "masked: source folder is sealed and not unlocked");
    assert_eq!(dto[0].label, "🔒 Locked", "no title leaks");
    assert!(dto[0].detail.is_empty(), "no content-derived detail leaks");

    let err = block_on(restore_trash_item_inner(&state, &entry_id))
        .expect_err("restore must be refused while locked");
    assert!(
        matches!(err, AppError::Locked(_)),
        "a locked refusal must be AppError::Locked, got {err:?}"
    );

    // Unsealing for the session brings it back — the seal is reversible, never lossy.
    unseal_trash_in_folder(&state.db, "f1", &ck).expect("session unseal");
    let unsealed = state.db.get_trash_entry(&entry_id).unwrap().unwrap();
    assert!(
        unsealed.payload.contains("We should ship the trash feature."),
        "session unseal restores the snapshot plaintext byte-identically"
    );
    assert_eq!(unsealed.label, "Roadmap review", "label round-trips too");
}

/// ORACLE 4 — a snapshot captured from an ALREADY-SEALED folder is sealed inside the capture, so it
/// is never at rest in plaintext even for an instant between the two steps.
#[test]
fn capture_from_sealed_folder_seals_the_snapshot_immediately() {
    let state = build_state("capture-sealed");
    open_folder(&state, "f1", "Work");
    seed_meeting(&state, "m1", Some("f1"));

    // Build a REAL sealed-but-session-unlocked folder through the PRODUCTION mint — that is the only
    // state in which the delete gate permits deleting a locked folder's content, so it is the state
    // the capture must handle, and using the real mint means `session_folder_ck` unwraps for real.
    let kek = zeroize::Zeroizing::new([3u8; 32]);
    *state.master_kek.lock().unwrap() = Some(kek.clone());
    let (_ck, wrapped) = mint_wrapped_ck(&kek, "f2").expect("mint wrapped ck");
    state
        .db
        .insert_sealed_folder(
            &Folder {
                id: "f2".to_string(),
                name: "Sealed".to_string(),
                path: "Sealed".to_string(),
                parent_id: None,
                locked: true,
                created_at: "2026-08-31T09:00:00Z".to_string(),
            },
            &wrapped,
        )
        .expect("insert sealed folder");
    state.unlocked_folders.lock().unwrap().insert("f2".to_string());
    state.db.set_meeting_folder("m1", Some("f2")).expect("re-file into the sealed folder");

    let entry_id = capture_meeting(&state, "m1").expect("capture from a sealed folder");
    let stored = state.db.get_trash_entry(&entry_id).unwrap().unwrap();
    assert!(
        stored.is_sealed() && stored.payload.is_empty(),
        "captured straight into ciphertext — never plaintext at rest behind an existing lock"
    );
    assert!(
        !String::from_utf8_lossy(stored.payload_blob.as_deref().unwrap()).contains("We should ship"),
        "the blob is ciphertext"
    );
}

/// ORACLE 5 — an authored note round-trips, and a note delete does NOT strand its content.
#[test]
fn note_delete_then_restore_round_trips_body() {
    let state = build_state("note-roundtrip");
    open_folder(&state, "nf1", "Notes");
    let body = "---\ntags: [spec]\n---\n\n# Trash design\n\nSnapshot, then verify.";
    state
        .db
        .insert_note("n1", "nf1", "Trash design", "Trash design", body, 1_760_000_000_000)
        .expect("insert note");

    block_on(delete_note_inner(&state, "n1")).expect("delete to trash");
    assert!(state.db.get_note_row("n1").unwrap().is_none(), "row removed");

    let entries = state.db.list_trash_entries().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].kind, "note");
    assert_eq!(entries[0].label, "Trash design");

    block_on(restore_trash_item_inner(&state, &entries[0].id)).expect("restore");
    let row = state.db.get_note_row("n1").unwrap().expect("note is back");
    assert_eq!(row.text, body, "note body byte-identical, front-matter included");
    assert_eq!(row.folder_id, "nf1", "restored into its original folder");
}

/// The vault export is a successful restore side effect, not part of the shell's immutable
/// identity. If only final journal consumption fails, retry must accept its own exported shell and
/// finish instead of wedging restore, purge, and re-delete forever.
#[test]
fn note_restore_retries_after_final_journal_consumption_failure() {
    let state = build_state("note-final-journal-retry");
    let vault = crate::storage::db::unique_temp_path("murmur-trash-note-vault", "dir");
    std::fs::create_dir_all(&vault).expect("create vault");
    state.config.lock().unwrap().vault_path = Some(vault.to_string_lossy().to_string());
    open_folder(&state, "nf1", "Notes");
    let body = "# Retry me\n\nThe journal is the final step.";
    state
        .db
        .insert_note("n1", "nf1", "Retry me", "Retry me", body, 1_760_000_000_000)
        .expect("insert note");

    block_on(delete_note_inner(&state, "n1")).expect("delete to trash");
    let entry = state.db.list_trash_entries().unwrap()[0].clone();
    state
        .db
        .lock()
        .execute_batch(&format!(
            "CREATE TRIGGER fail_final_trash_consumption
             BEFORE DELETE ON trash_items
             WHEN OLD.id = '{}'
             BEGIN
               SELECT RAISE(ABORT, 'injected final journal delete failure');
             END;",
            entry.id
        ))
        .expect("inject final consumption failure");

    let error = block_on(restore_trash_item_inner(&state, &entry.id))
        .expect_err("final journal delete failure surfaces");
    assert!(matches!(error, AppError::Storage(_)));
    let partial = state.db.get_note_row("n1").unwrap().expect("restored shell");
    let exported = partial
        .exported_path
        .clone()
        .expect("restore exported the note before final journal consumption");
    assert!(std::path::Path::new(&exported).is_file());
    assert_eq!(partial.text, body);
    assert!(state.db.get_trash_entry(&entry.id).unwrap().is_some());

    state
        .db
        .lock()
        .execute_batch("DROP TRIGGER fail_final_trash_consumption;")
        .unwrap();
    block_on(restore_trash_item_inner(&state, &entry.id))
        .expect("retry accepts its already-exported shell");

    let restored = state.db.get_note_row("n1").unwrap().expect("note remains restored");
    assert_eq!(restored.text, body);
    assert!(restored.exported_path.is_some());
    assert!(state.db.get_trash_entry(&entry.id).unwrap().is_none());
    let _ = std::fs::remove_dir_all(&vault);
}

/// Authored-note deletion purges BOTH `note`/`document` link discriminators. A whole-container
/// decision is not derivable from the body, so it must ride in the verified note snapshot and
/// return in the same direction after restore.
#[test]
fn note_delete_then_restore_round_trips_its_container_relation() {
    let state = build_state("note-container-link-roundtrip");
    let notes_root = state.db.ensure_notes_root().expect("Notes root");
    open_folder(&state, "related-folder", "Related folder");
    state
        .db
        .insert_note(
            "n-related",
            &notes_root,
            "Related note",
            "Related note",
            "# Related note",
            1_760_000_000_000,
        )
        .expect("insert authored note");
    link_items_inner(
        &state,
        "note",
        "n-related",
        "container",
        "related-folder",
    )
    .expect("link note to whole folder");
    let before = state
        .db
        .link_rows_for_authored_note("n-related")
        .expect("snapshot note relation");

    block_on(delete_note_inner(&state, "n-related")).expect("delete note to trash");
    assert!(state.db.get_note_row("n-related").unwrap().is_none());
    assert!(
        state
            .db
            .link_rows_for_authored_note("n-related")
            .unwrap()
            .is_empty()
    );
    let entry = state
        .db
        .list_trash_entries()
        .unwrap()
        .into_iter()
        .find(|entry| entry.source_id == "n-related")
        .expect("note trash snapshot");

    block_on(restore_trash_item_inner(&state, &entry.id)).expect("restore note");

    assert!(state.db.get_note_row("n-related").unwrap().is_some());
    assert_eq!(
        state.db.link_rows_for_authored_note("n-related").unwrap(),
        before,
        "the exact note-to-container decision returns"
    );
    assert!(state.db.get_trash_entry(&entry.id).unwrap().is_none());
}

/// Older rows could name an authored note as `document`. The trash snapshot canonicalizes only that
/// endpoint to `note`; otherwise the restored row fails today's kind-qualified endpoint gate and the
/// user's decision silently disappears.
#[test]
fn note_restore_normalizes_legacy_document_discriminator() {
    let state = build_state("note-link-legacy-discriminator");
    let notes_root = state.db.ensure_notes_root().expect("Notes root");
    open_folder(&state, "related-folder", "Related folder");
    state
        .db
        .insert_note(
            "n-legacy",
            &notes_root,
            "Legacy note",
            "Legacy note",
            "# Legacy note",
            1_760_000_000_000,
        )
        .expect("insert authored note");
    state
        .db
        .lock()
        .execute(
            "INSERT INTO links
               (src_kind,src_id,dst_kind,dst_id,edge_type,score,created_by,status,created_at)
             VALUES('document','n-legacy','container','related-folder','manual',1.0,'user','active',1)",
            [],
        )
        .expect("seed legacy discriminator");

    block_on(delete_note_inner(&state, "n-legacy")).expect("delete legacy note");
    let entry = state
        .db
        .list_trash_entries()
        .unwrap()
        .into_iter()
        .find(|entry| entry.source_id == "n-legacy")
        .expect("note trash snapshot");
    block_on(restore_trash_item_inner(&state, &entry.id)).expect("restore legacy note");

    let rows = state
        .db
        .link_rows_for_authored_note("n-legacy")
        .expect("restored relation");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].src_kind, "note");
    assert_eq!(rows[0].src_id, "n-legacy");
    assert_eq!(rows[0].dst_kind, "container");
    assert_eq!(rows[0].dst_id, "related-folder");
}

/// The same normalization must happen when the FOLDER owns the only snapshot; otherwise a legacy
/// authored-note endpoint on the far side is mistaken for a permanently deleted import.
#[test]
fn folder_restore_normalizes_a_legacy_note_endpoint_on_the_far_side() {
    let state = build_state("folder-link-legacy-note-endpoint");
    let notes_root = state.db.ensure_notes_root().expect("Notes root");
    open_folder(&state, "related-folder", "Related folder");
    state
        .db
        .insert_note(
            "n-legacy",
            &notes_root,
            "Legacy note",
            "Legacy note",
            "# Legacy note",
            1_760_000_000_000,
        )
        .expect("insert authored note");
    state
        .db
        .lock()
        .execute(
            "INSERT INTO links
               (src_kind,src_id,dst_kind,dst_id,edge_type,score,created_by,status,created_at)
             VALUES('document','n-legacy','container','related-folder','manual',1.0,'user','active',1)",
            [],
        )
        .expect("seed legacy far endpoint");

    delete_folder_inner(&state, "related-folder".into()).expect("delete folder");
    let entry = state.db.list_trash_entries().unwrap()[0].id.clone();
    block_on(restore_trash_item_inner(&state, &entry)).expect("restore folder");

    let rows = state
        .db
        .link_rows_for_container("related-folder")
        .expect("restored relation");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].src_kind, "note");
    assert_eq!(rows[0].src_id, "n-legacy");
}

/// A transient relation replay error must not consume the journal. The exact minimal note shell is
/// safe to keep; retry recognizes it and finishes the relation instead of invoking a destructive
/// delete cascade that could erase unrelated state.
#[test]
fn note_restore_relation_failure_keeps_retryable_shell_and_journal() {
    let state = build_state("note-container-link-retry");
    let notes_root = state.db.ensure_notes_root().expect("Notes root");
    open_folder(&state, "related-folder", "Related folder");
    state
        .db
        .insert_note(
            "n-related",
            &notes_root,
            "Related note",
            "Related note",
            "# Related note",
            1_760_000_000_000,
        )
        .expect("insert authored note");
    link_items_inner(
        &state,
        "note",
        "n-related",
        "container",
        "related-folder",
    )
    .expect("link note to whole folder");
    let relation = state
        .db
        .link_rows_for_authored_note("n-related")
        .unwrap();

    block_on(delete_note_inner(&state, "n-related")).expect("delete note to trash");
    let entry = state
        .db
        .list_trash_entries()
        .unwrap()
        .into_iter()
        .find(|entry| entry.source_id == "n-related")
        .expect("note trash snapshot");
    state
        .db
        .lock()
        .execute_batch(
            "CREATE TRIGGER fail_note_relation_restore
             BEFORE INSERT ON links
             WHEN NEW.src_id = 'n-related' OR NEW.dst_id = 'n-related'
             BEGIN
               SELECT RAISE(ABORT, 'injected note relation replay failure');
             END;",
        )
        .unwrap();

    let error = block_on(restore_trash_item_inner(&state, &entry.id))
        .expect_err("injected note replay failure surfaces");
    assert!(matches!(error, AppError::Storage(_)));
    assert!(
        state.db.get_note_row("n-related").unwrap().is_some(),
        "the exact minimal shell remains available for a non-destructive retry"
    );
    assert!(
        state
            .db
            .link_rows_for_authored_note("n-related")
            .unwrap()
            .is_empty(),
        "the failing link transaction publishes no partial edge"
    );
    assert!(
        state.db.get_trash_entry(&entry.id).unwrap().is_some(),
        "the relation-bearing journal remains retryable"
    );

    state
        .db
        .lock()
        .execute_batch("DROP TRIGGER fail_note_relation_restore;")
        .unwrap();
    block_on(restore_trash_item_inner(&state, &entry.id)).expect("retry restores note and relation");

    assert!(state.db.get_note_row("n-related").unwrap().is_some());
    assert_eq!(
        state.db.link_rows_for_authored_note("n-related").unwrap(),
        relation
    );
    assert!(state.db.get_trash_entry(&entry.id).unwrap().is_none());
}

/// An unavailable unlock snapshot is an authorization/storage failure, never an empty unlock set.
/// Treating mutex poison as "nothing unlocked" can filter relations and then consume the only
/// journal as if those rows had been deliberately superseded.
#[test]
fn relation_restore_fails_without_consuming_snapshot_when_unlock_state_is_poisoned() {
    let state = build_state("relation-poisoned-unlock-state");
    let notes_root = state.db.ensure_notes_root().expect("Notes root");
    open_folder(&state, "related-folder", "Related folder");
    state
        .db
        .insert_note(
            "n-related",
            &notes_root,
            "Related note",
            "Related note",
            "# Related note",
            1_760_000_000_000,
        )
        .expect("insert note");
    link_items_inner(
        &state,
        "note",
        "n-related",
        "container",
        "related-folder",
    )
    .expect("link note to folder");
    block_on(delete_note_inner(&state, "n-related")).expect("delete note");
    let entry = state.db.list_trash_entries().unwrap()[0].clone();

    let unlock_state = Arc::clone(&state.unlocked_folders);
    let poisoned = std::thread::spawn(move || {
        let _guard = unlock_state.lock().expect("lock before poison");
        panic!("intentional unlock-state poison");
    })
    .join();
    assert!(poisoned.is_err());

    let error = block_on(restore_trash_item_inner(&state, &entry.id))
        .expect_err("poisoned authorization state must fail closed");
    assert!(matches!(error, AppError::Storage(_)));
    assert!(
        state.db.get_trash_entry(&entry.id).unwrap().is_some(),
        "the relation-bearing snapshot remains recoverable"
    );
    assert!(
        state
            .db
            .link_rows_for_authored_note("n-related")
            .unwrap()
            .is_empty(),
        "no relation is silently filtered into a successful restore"
    );
}

/// A user-selected whole-folder relation follows the folder through the REAL trash lifecycle.
///
/// This drives the command writer, `delete_folder_inner` (including its transactional container
/// edge purge), the serialized folder snapshot, and `restore_trash_item_inner`. A storage-only
/// replay test would not catch a missing capture/restore call in that production chain.
#[test]
fn folder_delete_then_restore_round_trips_its_direct_relation() {
    let state = build_state("folder-link-roundtrip");
    open_folder(&state, "home", "Home");
    open_folder(&state, "related-folder", "Related folder");
    seed_meeting(&state, "m1", Some("home"));

    link_items_inner(
        &state,
        "meeting",
        "m1",
        "container",
        "related-folder",
    )
    .expect("link the whole folder");
    let before = state
        .db
        .link_rows_for_container("related-folder")
        .expect("snapshot relation");
    assert_eq!(before.len(), 1, "one direct row, never descendant fan-out");

    delete_folder_inner(&state, "related-folder".into()).expect("delete folder to trash");
    assert!(state.db.folder_by_id("related-folder").unwrap().is_none());
    assert!(
        state
            .db
            .link_rows_for_container("related-folder")
            .unwrap()
            .is_empty(),
        "the delete transaction must leave no dangling container endpoint"
    );

    let entry = state
        .db
        .list_trash_entries()
        .unwrap()
        .into_iter()
        .find(|entry| entry.source_id == "related-folder")
        .expect("folder trash snapshot");
    block_on(restore_trash_item_inner(&state, &entry.id)).expect("restore folder");

    assert!(state.db.folder_by_id("related-folder").unwrap().is_some());
    assert_eq!(
        state
            .db
            .link_rows_for_container("related-folder")
            .unwrap(),
        before,
        "the exact directed user decision must return"
    );
    let visible = state
        .db
        .links_for_visible(crate::links::LinkKind::Meeting, "m1", &HashSet::new())
        .unwrap();
    assert!(visible.iter().any(|edge| {
        edge.other_kind == "container"
            && edge.other_id == "related-folder"
            && edge.other_title == "Related folder"
    }));
}

/// A Shared document can be temporarily withheld without revoking the user's private graph
/// decision. Folder restore must therefore replay the opaque id-only row while Shared context is
/// disabled; the ordinary both-endpoint reader keeps it invisible until the org is enabled again.
/// Treating current Shared visibility as permanent endpoint existence consumes the only recovery
/// journal and silently loses this explicit relation.
#[test]
fn folder_restore_preserves_relation_to_temporarily_withheld_shared_document() {
    let state = build_state("folder-link-withheld-org");
    let org_id = "11111111-1111-4111-8111-111111111111";
    let doc_id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let item_id = "22222222-2222-4222-8222-222222222222";
    let endpoint =
        seed_visible_shared_document(&state, org_id, doc_id, item_id, 1, "Shared roadmap");

    open_folder(&state, "related-folder", "Related folder");
    link_items_inner(&state, "org", &endpoint, "container", "related-folder")
        .expect("link Shared document to whole folder");
    let expected = state.db.link_rows_for_container("related-folder").unwrap();
    assert_eq!(expected.len(), 1);

    state
        .db
        .set_org_context_enabled(org_id, false)
        .expect("disable Shared context");
    delete_folder_inner(&state, "related-folder".into()).expect("delete folder while withheld");
    let entry = state
        .db
        .list_trash_entries()
        .unwrap()
        .into_iter()
        .find(|entry| entry.source_id == "related-folder")
        .expect("folder recovery journal");
    block_on(restore_trash_item_inner(&state, &entry.id)).expect("restore while Shared is withheld");

    assert_eq!(
        state.db.link_rows_for_container("related-folder").unwrap(),
        expected,
        "the opaque user decision must survive temporary Shared unavailability"
    );
    assert!(
        state
            .db
            .links_for_visible(
                crate::links::LinkKind::Container,
                "related-folder",
                &HashSet::new(),
            )
            .unwrap()
            .is_empty(),
        "the dormant row must reveal neither the Shared title nor its existence"
    );
    assert!(state.db.get_trash_entry(&entry.id).unwrap().is_none());

    state
        .db
        .set_org_context_enabled(org_id, true)
        .expect("re-enable Shared context");
    let visible = state
        .db
        .links_for_visible(
            crate::links::LinkKind::Container,
            "related-folder",
            &HashSet::new(),
        )
        .unwrap();
    assert_eq!(visible.len(), 1, "the exact relation becomes visible again");
    assert_eq!(visible[0].other_kind, "org");
    assert_eq!(visible[0].other_id, endpoint);
    assert_eq!(visible[0].other_title, "Shared roadmap");
}

/// A confirmed stable-document DELETE is not the reversible withholding case above. Even when the
/// folder edge exists only inside its recovery journal at delete time, the terminal document
/// witness must stop that older snapshot from recreating the user's now-invalid relation.
#[test]
fn folder_restore_does_not_resurrect_relation_after_terminal_shared_delete() {
    let state = build_state("folder-link-terminal-org-delete");
    let org_id = "11111111-1111-4111-8111-111111111111";
    let doc_id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let endpoint = seed_visible_shared_document(
        &state,
        org_id,
        doc_id,
        "22222222-2222-4222-8222-222222222222",
        1,
        "Shared roadmap",
    );
    open_folder(&state, "related-folder", "Related folder");
    link_items_inner(&state, "org", &endpoint, "container", "related-folder")
        .expect("link Shared document to folder");

    delete_folder_inner(&state, "related-folder".into()).expect("delete folder first");
    let entry = state
        .db
        .list_trash_entries()
        .unwrap()
        .into_iter()
        .find(|entry| entry.source_id == "related-folder")
        .expect("folder recovery journal");
    assert!(
        state.db.evict_org_document(org_id, doc_id).unwrap(),
        "authoritative stable-document delete evicts its live replica"
    );

    block_on(restore_trash_item_inner(&state, &entry.id)).expect("restore folder after terminal delete");
    assert!(
        state.db.link_rows_for_container("related-folder").unwrap().is_empty(),
        "a snapshot older than the terminal delete cannot resurrect the relation"
    );
    assert!(state.db.get_trash_entry(&entry.id).unwrap().is_none());
}

/// A document may later have a new live incarnation under the same stable id. Only a fresh,
/// endpoint-gated click by the user authorizes that exact directed edge after a terminal DELETE.
/// The witness survives recoverable trash, but an explicit unlink removes it atomically.
#[test]
fn fresh_manual_link_reauthorizes_exact_terminal_shared_relation_until_unlink() {
    let state = build_state("folder-link-terminal-org-reauthorize");
    let org_id = "11111111-1111-4111-8111-111111111111";
    let doc_id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let endpoint = seed_visible_shared_document(
        &state,
        org_id,
        doc_id,
        "22222222-2222-4222-8222-222222222222",
        1,
        "Old shared roadmap",
    );
    assert!(state.db.evict_org_document(org_id, doc_id).unwrap());
    seed_visible_shared_document(
        &state,
        org_id,
        doc_id,
        "33333333-3333-4333-8333-333333333333",
        2,
        "New shared roadmap",
    );
    open_folder(&state, "related-folder", "Related folder");
    link_items_inner(&state, "org", &endpoint, "container", "related-folder")
        .expect("fresh user action reauthorizes this exact edge");
    let expected = state.db.link_rows_for_container("related-folder").unwrap();

    delete_folder_inner(&state, "related-folder".into()).expect("delete recoverably");
    let entry = state.db.list_trash_entries().unwrap()[0].clone();
    block_on(restore_trash_item_inner(&state, &entry.id))
        .expect("reauthorized edge restores with its folder");
    assert_eq!(state.db.link_rows_for_container("related-folder").unwrap(), expected);

    unlink_items_inner(&state, "org", &endpoint, "container", "related-folder")
        .expect("explicit unlink");
    let witnesses: i64 = state
        .db
        .lock()
        .query_row(
            "SELECT COUNT(*) FROM org_link_reauthorizations
              WHERE src_kind='org' AND src_id=?1
                AND dst_kind='container' AND dst_id='related-folder'",
            [&endpoint],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(witnesses, 0, "unlink revokes the exact replay authority");
}

/// The exact witness is allowed to outlive a recoverable delete only because the trash journal can
/// still restore its endpoint. Permanently discarding that journal must remove the witness in the
/// same transaction, so a later local id reuse inherits no replay authority.
#[test]
fn permanent_folder_purge_revokes_terminal_shared_reauthorization() {
    let state = build_state("folder-link-terminal-org-permanent-purge");
    let org_id = "11111111-1111-4111-8111-111111111111";
    let doc_id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let endpoint = seed_visible_shared_document(
        &state,
        org_id,
        doc_id,
        "22222222-2222-4222-8222-222222222222",
        1,
        "Old shared roadmap",
    );
    assert!(state.db.evict_org_document(org_id, doc_id).unwrap());
    seed_visible_shared_document(
        &state,
        org_id,
        doc_id,
        "33333333-3333-4333-8333-333333333333",
        2,
        "New shared roadmap",
    );
    open_folder(&state, "related-folder", "Related folder");
    link_items_inner(&state, "org", &endpoint, "container", "related-folder")
        .expect("fresh link creates replay authority");
    delete_folder_inner(&state, "related-folder".into()).expect("delete recoverably");
    let entry = state.db.list_trash_entries().unwrap()[0].clone();
    let before: i64 = state
        .db
        .lock()
        .query_row("SELECT COUNT(*) FROM org_link_reauthorizations", [], |row| row.get(0))
        .unwrap();
    assert_eq!(before, 1, "recoverable trash keeps the exact witness");

    block_on(purge_one_for_test(&state, &entry.id)).expect("discard folder forever");
    let after: i64 = state
        .db
        .lock()
        .query_row("SELECT COUNT(*) FROM org_link_reauthorizations", [], |row| row.get(0))
        .unwrap();
    assert_eq!(after, 0, "permanent purge removes replay authority");
}

/// Reauthorization belongs to one terminal-delete epoch. A second authoritative DELETE must
/// invalidate it even while the local folder (and therefore the edge) sits only in trash.
#[test]
fn second_terminal_shared_delete_invalidates_prior_reauthorization() {
    let state = build_state("folder-link-terminal-org-second-delete");
    let org_id = "11111111-1111-4111-8111-111111111111";
    let doc_id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let endpoint = seed_visible_shared_document(
        &state,
        org_id,
        doc_id,
        "22222222-2222-4222-8222-222222222222",
        1,
        "First incarnation",
    );
    assert!(state.db.evict_org_document(org_id, doc_id).unwrap());
    seed_visible_shared_document(
        &state,
        org_id,
        doc_id,
        "33333333-3333-4333-8333-333333333333",
        2,
        "Second incarnation",
    );
    open_folder(&state, "related-folder", "Related folder");
    link_items_inner(&state, "org", &endpoint, "container", "related-folder")
        .expect("reauthorize after first delete");
    delete_folder_inner(&state, "related-folder".into()).expect("snapshot reauthorized edge");
    let entry = state.db.list_trash_entries().unwrap()[0].clone();

    assert!(state.db.evict_org_document(org_id, doc_id).unwrap());
    seed_visible_shared_document(
        &state,
        org_id,
        doc_id,
        "44444444-4444-4444-8444-444444444444",
        3,
        "Third incarnation",
    );
    block_on(restore_trash_item_inner(&state, &entry.id)).expect("restore after second delete");
    assert!(
        state.db.link_rows_for_container("related-folder").unwrap().is_empty(),
        "the previous epoch's authorization must not survive another terminal delete"
    );
}

/// Delete the NOTE first, then its related folder. Only the first snapshot can carry the edge.
/// Restoring in that same order must publish a dormant id-only row, consume both journals, and make
/// the relation visible automatically when the folder returns.
#[test]
fn container_relation_restore_is_order_independent_when_note_was_deleted_first() {
    let state = build_state("relation-order-note-first");
    let notes_root = state.db.ensure_notes_root().expect("Notes root");
    open_folder(&state, "related-folder", "Related folder");
    state
        .db
        .lock()
        .execute("UPDATE folders SET kind = 'note' WHERE id = 'related-folder'", [])
        .expect("make the related container a note-folder");
    state
        .db
        .insert_note(
            "n-related",
            &notes_root,
            "Related note",
            "Related note",
            "# Related note",
            1_760_000_000_000,
        )
        .expect("insert note");
    link_items_inner(
        &state,
        "note",
        "n-related",
        "container",
        "related-folder",
    )
    .expect("link note to folder");
    let expected = state
        .db
        .link_rows_for_authored_note("n-related")
        .unwrap();

    block_on(delete_note_inner(&state, "n-related")).expect("delete note first");
    delete_folder_inner(&state, "related-folder".into()).expect("delete folder second");
    let entries = state.db.list_trash_entries().unwrap();
    let note_entry = entries
        .iter()
        .find(|entry| entry.source_id == "n-related")
        .expect("note snapshot")
        .id
        .clone();
    let folder_entry = entries
        .iter()
        .find(|entry| entry.source_id == "related-folder")
        .expect("folder snapshot")
        .id
        .clone();

    block_on(restore_trash_item_inner(&state, &note_entry))
        .expect("note restores while folder remains in trash");
    assert_eq!(
        state
            .db
            .link_rows_for_authored_note("n-related")
            .unwrap(),
        expected,
        "the decision remains durable while its container endpoint is absent"
    );
    assert!(
        state
            .db
            .links_for_visible(
                crate::links::LinkKind::Note,
                "n-related",
                &HashSet::new(),
            )
            .unwrap()
            .is_empty(),
        "the dormant row reveals nothing before the folder returns"
    );

    block_on(restore_trash_item_inner(&state, &folder_entry)).expect("restore folder second");
    assert_eq!(
        state
            .db
            .links_for_visible(
                crate::links::LinkKind::Note,
                "n-related",
                &HashSet::new(),
            )
            .unwrap()
            .len(),
        1,
        "the same row becomes visible only after both endpoints exist"
    );
    assert_eq!(state.db.count_trash_entries().unwrap(), 0);
}

/// Twin ordering: delete the CONTAINER first, then the recording, and restore the container first.
/// The folder snapshot is the sole relation journal, so dropping its row on a missing meeting would
/// make the subsequent recording restore permanently forget the decision.
#[test]
fn container_relation_restore_is_order_independent_when_folder_was_deleted_first() {
    let state = build_state("relation-order-folder-first");
    open_folder(&state, "home", "Home");
    open_folder(&state, "related-folder", "Related folder");
    seed_meeting(&state, "m-related", Some("home"));
    link_items_inner(
        &state,
        "meeting",
        "m-related",
        "container",
        "related-folder",
    )
    .expect("link recording to folder");
    let expected = state.db.link_rows_for_meeting("m-related").unwrap();

    delete_folder_inner(&state, "related-folder".into()).expect("delete folder first");
    block_on(delete_meeting_inner(&state, "m-related")).expect("delete recording second");
    let entries = state.db.list_trash_entries().unwrap();
    let folder_entry = entries
        .iter()
        .find(|entry| entry.source_id == "related-folder")
        .expect("folder snapshot")
        .id
        .clone();
    let meeting_entry = entries
        .iter()
        .find(|entry| entry.source_id == "m-related")
        .expect("meeting snapshot")
        .id
        .clone();

    block_on(restore_trash_item_inner(&state, &folder_entry))
        .expect("folder restores while recording remains in trash");
    assert_eq!(
        state.db.link_rows_for_container("related-folder").unwrap(),
        expected,
        "the folder-owned journal publishes the dormant decision"
    );
    assert!(
        state
            .db
            .links_for_visible(
                crate::links::LinkKind::Container,
                "related-folder",
                &HashSet::new(),
            )
            .unwrap()
            .is_empty(),
        "the missing recording keeps the relation invisible"
    );

    block_on(restore_trash_item_inner(&state, &meeting_entry)).expect("restore recording second");
    assert_eq!(state.db.link_rows_for_meeting("m-related").unwrap(), expected);
    assert_eq!(state.db.count_trash_entries().unwrap(), 0);
}

/// If the missing endpoint is discarded instead of restored, its dormant decision must disappear in
/// the SAME lifecycle interval as the last recovery journal.
#[test]
fn permanent_trash_purge_removes_a_dormant_container_relation() {
    let state = build_state("relation-pending-purge");
    let notes_root = state.db.ensure_notes_root().expect("Notes root");
    open_folder(&state, "related-folder", "Related folder");
    state
        .db
        .lock()
        .execute("UPDATE folders SET kind = 'note' WHERE id = 'related-folder'", [])
        .expect("make the pending endpoint a note-folder");
    state
        .db
        .insert_note(
            "n-related",
            &notes_root,
            "Related note",
            "Related note",
            "# Related note",
            1_760_000_000_000,
        )
        .expect("insert note");
    link_items_inner(
        &state,
        "note",
        "n-related",
        "container",
        "related-folder",
    )
    .expect("link note to folder");

    block_on(delete_note_inner(&state, "n-related")).expect("delete note first");
    delete_folder_inner(&state, "related-folder".into()).expect("delete folder second");
    let entries = state.db.list_trash_entries().unwrap();
    let note_entry = entries
        .iter()
        .find(|entry| entry.source_id == "n-related")
        .unwrap()
        .id
        .clone();
    let folder_entry = entries
        .iter()
        .find(|entry| entry.source_id == "related-folder")
        .unwrap()
        .id
        .clone();

    block_on(restore_trash_item_inner(&state, &note_entry)).expect("restore note");
    assert_eq!(
        state
            .db
            .link_rows_for_authored_note("n-related")
            .unwrap()
            .len(),
        1,
        "precondition: the dormant relation exists"
    );
    block_on(purge_one_for_test(&state, &folder_entry)).expect("discard folder forever");
    assert!(
        state
            .db
            .link_rows_for_authored_note("n-related")
            .unwrap()
            .is_empty(),
        "no relation may dangle after its last endpoint journal is destroyed"
    );
}

/// RED-before-GREEN: relation replay failure is a recoverable partial restore, not silent loss.
/// A real SQLite trigger aborts the production `restore_link_rows` INSERT. The first command must
/// retain its trash journal; after removing the fault, the same command resumes the matching folder
/// and completes both member and relation restoration exactly once.
#[test]
fn folder_restore_relation_failure_retains_snapshot_and_retry_completes() {
    let state = build_state("folder-link-retry");
    open_folder(&state, "home", "Home");
    open_folder(&state, "related-folder", "Related folder");
    seed_meeting(&state, "m-anchor", Some("home"));
    seed_meeting(&state, "m-member", Some("related-folder"));
    link_items_inner(&state, "meeting", "m-anchor", "container", "related-folder")
        .expect("link whole folder");
    let relation = state.db.link_rows_for_container("related-folder").unwrap();

    delete_folder_inner(&state, "related-folder".into()).expect("delete folder to trash");
    let entry = state
        .db
        .list_trash_entries()
        .unwrap()
        .into_iter()
        .find(|entry| entry.source_id == "related-folder")
        .expect("folder trash snapshot");
    state
        .db
        .lock()
        .execute_batch(
            "CREATE TRIGGER fail_container_relation_restore
             BEFORE INSERT ON links
             WHEN NEW.src_kind = 'container' OR NEW.dst_kind = 'container'
             BEGIN
               SELECT RAISE(ABORT, 'injected container relation replay failure');
             END;",
        )
        .unwrap();

    let error = block_on(restore_trash_item_inner(&state, &entry.id))
        .expect_err("the injected replay failure must surface");
    assert!(matches!(error, AppError::Storage(_)));
    assert!(
        state.db.get_trash_entry(&entry.id).unwrap().is_some(),
        "the recovery journal must survive until relation replay commits"
    );
    assert!(
        state.db.folder_by_id("related-folder").unwrap().is_some(),
        "the failure is intentionally a retryable partial restore"
    );
    assert_eq!(
        state
            .db
            .get_meeting("m-member")
            .unwrap()
            .unwrap()
            .folder_id
            .as_deref(),
        Some("related-folder"),
        "already-restored content remains filed, not lost"
    );
    assert!(
        state
            .db
            .link_rows_for_container("related-folder")
            .unwrap()
            .is_empty(),
        "the failed replay transaction commits no partial relation"
    );
    let purge_error = block_on(purge_one_for_test(&state, &entry.id))
        .expect_err("a live retry shell protects its only journal from purge");
    assert!(matches!(purge_error, AppError::Unavailable(_)));
    assert!(state.db.get_trash_entry(&entry.id).unwrap().is_some());

    state
        .db
        .lock()
        .execute_batch("DROP TRIGGER fail_container_relation_restore;")
        .unwrap();
    block_on(restore_trash_item_inner(&state, &entry.id)).expect("retry completes restore");

    assert!(state.db.get_trash_entry(&entry.id).unwrap().is_none());
    assert_eq!(
        state
            .db
            .get_meeting("m-member")
            .unwrap()
            .unwrap()
            .folder_id
            .as_deref(),
        Some("related-folder")
    );
    assert_eq!(
        state.db.link_rows_for_container("related-folder").unwrap(),
        relation,
        "the exact direct user decision returns on retry"
    );
}

/// Once a partial restore has successfully handled a member, its durable progress marker—not its
/// current folder—defines retry behavior. This matters when the user's newer choice is exactly the
/// same fallback used by delete, which is otherwise indistinguishable from an untouched member.
#[test]
fn folder_restore_retry_preserves_newer_explicit_fallback_placements() {
    let state = build_state("folder-retry-fallback-placement");
    let notes_root = state.db.ensure_notes_root().expect("Notes root");
    open_folder(&state, "anchor-home", "Anchor home");
    open_folder(&state, "old-home", "Old home");
    seed_meeting(&state, "m-anchor", Some("anchor-home"));
    seed_meeting(&state, "m-member", Some("old-home"));
    state
        .db
        .insert_note(
            "n-member",
            "old-home",
            "Member note",
            "Member note",
            "# Member note",
            1_760_000_000_000,
        )
        .expect("insert authored member");
    link_items_inner(&state, "meeting", "m-anchor", "container", "old-home")
        .expect("link whole folder");

    delete_folder_inner(&state, "old-home".into()).expect("delete old folder");
    let entry = state
        .db
        .list_trash_entries()
        .unwrap()
        .into_iter()
        .find(|entry| entry.source_id == "old-home")
        .expect("folder journal");
    state
        .db
        .lock()
        .execute_batch(
            "CREATE TRIGGER fail_fallback_relation_restore
             BEFORE INSERT ON links
             WHEN NEW.src_kind = 'container' OR NEW.dst_kind = 'container'
             BEGIN
               SELECT RAISE(ABORT, 'injected relation replay failure');
             END;",
        )
        .unwrap();

    block_on(restore_trash_item_inner(&state, &entry.id))
        .expect_err("first relation replay fails after member placement");
    assert_eq!(
        state.db.get_meeting_gate_anchor("m-member").unwrap().unwrap().folder_id.as_deref(),
        Some("old-home")
    );
    assert_eq!(
        state.db.note_gate_anchor("n-member").unwrap().unwrap().0,
        "old-home"
    );

    state
        .db
        .set_meeting_folder("m-member", None)
        .expect("user explicitly moves recording to Not classified");
    crate::commands::move_note_doc_inner(&state, "n-member", &notes_root)
        .expect("user explicitly moves note to Notes root");
    state
        .db
        .lock()
        .execute_batch("DROP TRIGGER fail_fallback_relation_restore;")
        .unwrap();

    block_on(restore_trash_item_inner(&state, &entry.id)).expect("retry completes");
    assert_eq!(
        state.db.get_meeting_gate_anchor("m-member").unwrap().unwrap().folder_id,
        None,
        "retry preserves explicit Not classified"
    );
    assert_eq!(
        state.db.note_gate_anchor("n-member").unwrap().unwrap().0,
        notes_root,
        "retry preserves explicit Notes-root placement"
    );
    assert!(state.db.get_trash_entry(&entry.id).unwrap().is_none());
    assert!(
        !state
            .db
            .trash_folder_member_was_resolved(&entry.id, "meeting", "m-member")
            .unwrap()
            && !state
                .db
                .trash_folder_member_was_resolved(&entry.id, "note", "n-member")
                .unwrap(),
        "consuming the journal also removes its progress witnesses"
    );
}

/// Member placement is structural authorization metadata. Restoring an old folder must not hydrate
/// titles, audio paths, or note bodies from a member re-filed into a sealed-not-unlocked folder.
/// Deliberately non-text content columns make the old full-row readers fail while gate anchors stay
/// readable, turning the no-hydration rule into a deterministic oracle.
#[test]
fn folder_restore_skips_sealed_newer_members_via_content_free_anchors() {
    let state = build_state("folder-sealed-newer-member");
    open_folder(&state, "old-home", "Old home");
    open_folder(&state, "sealed-home", "Sealed home");
    seed_meeting(&state, "m-member", Some("old-home"));
    state
        .db
        .insert_note(
            "n-member",
            "old-home",
            "Private note",
            "Private note",
            "# Private body",
            1_760_000_000_000,
        )
        .expect("insert authored member");

    delete_folder_inner(&state, "old-home".into()).expect("delete old folder");
    let entry = state
        .db
        .list_trash_entries()
        .unwrap()
        .into_iter()
        .find(|entry| entry.source_id == "old-home")
        .expect("folder journal");
    state
        .db
        .set_meeting_folder("m-member", Some("sealed-home"))
        .expect("re-file recording");
    crate::commands::move_note_doc_inner(&state, "n-member", "sealed-home")
        .expect("re-file note");
    state
        .db
        .lock()
        .execute_batch(
            "UPDATE meetings SET title=X'FF', audio_path=X'FF' WHERE id='m-member';
             UPDATE documents SET name=X'FF', title=X'FF', text=X'FF' WHERE id='n-member';",
        )
        .expect("make any accidental content hydration fail closed");
    state
        .db
        .set_folder_locked_for_test("sealed-home", true)
        .expect("seal structural destination");

    block_on(restore_trash_item_inner(&state, &entry.id))
        .expect("restore uses only content-free member anchors");
    assert_eq!(
        state.db.get_meeting_gate_anchor("m-member").unwrap().unwrap().folder_id.as_deref(),
        Some("sealed-home")
    );
    assert_eq!(
        state.db.note_gate_anchor("n-member").unwrap().unwrap().0,
        "sealed-home"
    );
    assert!(state.db.get_trash_entry(&entry.id).unwrap().is_none());
}

/// RED-before-GREEN: folder restore owns the SAME lifecycle interval as folder delete, from the
/// first recreated row through relation replay and consumption of the only recovery journal.
/// Holding that interval here must keep every restore mutation blocked. Before the fix, folder
/// restores skipped the guard and this worker consumed the snapshot while the guard was held — the
/// exact opening that allowed a concurrent delete to remove the endpoint between insert and replay.
#[test]
fn folder_restore_waits_for_lifecycle_before_recreating_or_consuming_snapshot() {
    let state = Arc::new(build_state("folder-restore-lifecycle"));
    open_folder(state.as_ref(), "home", "Home");
    open_folder(state.as_ref(), "related-folder", "Related folder");
    seed_meeting(state.as_ref(), "m-anchor", Some("home"));
    link_items_inner(
        state.as_ref(),
        "meeting",
        "m-anchor",
        "container",
        "related-folder",
    )
    .expect("link whole folder");
    let relation = state
        .db
        .link_rows_for_container("related-folder")
        .expect("snapshot original relation");

    delete_folder_inner(state.as_ref(), "related-folder".into())
        .expect("delete folder to trash");
    let entry = state
        .db
        .list_trash_entries()
        .unwrap()
        .into_iter()
        .find(|entry| entry.source_id == "related-folder")
        .expect("folder trash snapshot");

    let lifecycle = lifecycle_guard(state.as_ref());
    let worker_state = Arc::clone(&state);
    let worker_entry_id = entry.id.clone();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        started_tx.send(()).expect("signal restore start");
        let result = block_on(restore_trash_item_inner(
            worker_state.as_ref(),
            &worker_entry_id,
        ));
        done_tx.send(result).expect("return restore result");
    });

    started_rx.recv().expect("restore worker started");
    assert!(
        done_rx
            .recv_timeout(std::time::Duration::from_millis(100))
            .is_err(),
        "folder restore must wait for the active delete/lock lifecycle interval"
    );
    assert!(
        state.db.folder_by_id("related-folder").unwrap().is_none(),
        "the folder row must not appear before lifecycle authorization"
    );
    assert!(
        state.db.get_trash_entry(&entry.id).unwrap().is_some(),
        "the only relation-bearing recovery journal must not be consumed early"
    );

    drop(lifecycle);
    done_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("restore completes after lifecycle release")
        .expect("restore succeeds");
    worker.join().expect("restore worker joins");

    assert!(state.db.folder_by_id("related-folder").unwrap().is_some());
    assert!(state.db.get_trash_entry(&entry.id).unwrap().is_none());
    assert_eq!(
        state.db.link_rows_for_container("related-folder").unwrap(),
        relation,
        "the relation and its endpoint become durable before the journal is consumed"
    );
}

/// Even code that bypasses the command lifecycle must not turn a vanished restored endpoint into a
/// successful restore. The trigger models the folder disappearing inside relation replay itself;
/// the command must retain its journal, and a later retry must make the exact relation live again.
#[test]
fn folder_restore_retains_snapshot_if_endpoint_disappears_during_relation_replay() {
    let state = build_state("folder-restore-endpoint-disappears");
    open_folder(&state, "home", "Home");
    open_folder(&state, "related-folder", "Related folder");
    seed_meeting(&state, "m-anchor", Some("home"));
    link_items_inner(
        &state,
        "meeting",
        "m-anchor",
        "container",
        "related-folder",
    )
    .expect("link whole folder");
    let relation = state.db.link_rows_for_container("related-folder").unwrap();

    delete_folder_inner(&state, "related-folder".into()).expect("delete folder to trash");
    let entry = state
        .db
        .list_trash_entries()
        .unwrap()
        .into_iter()
        .find(|entry| entry.source_id == "related-folder")
        .expect("folder trash snapshot");
    state
        .db
        .lock()
        .execute_batch(
            "CREATE TRIGGER remove_restored_container_during_link_replay
             AFTER INSERT ON links
             WHEN NEW.dst_kind = 'container' AND NEW.dst_id = 'related-folder'
             BEGIN
               DELETE FROM folders WHERE id = 'related-folder';
             END;",
        )
        .unwrap();

    let error = block_on(restore_trash_item_inner(&state, &entry.id))
        .expect_err("a vanished restored endpoint must fail closed");
    assert!(matches!(error, AppError::Storage(_)));
    assert!(state.db.folder_by_id("related-folder").unwrap().is_none());
    assert!(
        state.db.get_trash_entry(&entry.id).unwrap().is_some(),
        "the only recovery journal survives endpoint disappearance"
    );

    state
        .db
        .lock()
        .execute_batch("DROP TRIGGER remove_restored_container_during_link_replay;")
        .unwrap();
    block_on(restore_trash_item_inner(&state, &entry.id)).expect("retry restores endpoint and link");

    assert!(state.db.folder_by_id("related-folder").unwrap().is_some());
    assert!(state.db.get_trash_entry(&entry.id).unwrap().is_none());
    assert_eq!(
        state.db.link_rows_for_container("related-folder").unwrap(),
        relation
    );
}

/// Folder restore now holds the non-reentrant lifecycle mutex itself, so authored-note re-filing
/// must use the explicit lifecycle-authorized move seam. This oracle would hang if it accidentally
/// called the public inner helper that tries to acquire the same mutex a second time.
#[test]
fn note_folder_restore_refiles_authored_notes_without_relocking_lifecycle() {
    let state = build_state("note-folder-restore-lifecycle");
    let default_notes = state
        .db
        .ensure_default_note_folder()
        .expect("default Notes folder");
    open_folder(&state, "custom-notes", "Custom notes");
    state
        .db
        .lock()
        .execute(
            "UPDATE folders SET kind = 'note' WHERE id = 'custom-notes'",
            [],
        )
        .expect("mark note folder");
    state
        .db
        .insert_note(
            "n-member",
            "custom-notes",
            "Member",
            "Member",
            "# Member\n\nKeep me.",
            1_760_000_000_000,
        )
        .expect("insert authored note");

    delete_folder_inner(&state, "custom-notes".into()).expect("delete note folder to trash");
    assert_eq!(
        state
            .db
            .get_note_row("n-member")
            .unwrap()
            .unwrap()
            .folder_id,
        default_notes,
        "delete safely rehomes the authored note first"
    );
    let entry = state
        .db
        .list_trash_entries()
        .unwrap()
        .into_iter()
        .find(|entry| entry.source_id == "custom-notes")
        .expect("note-folder snapshot");

    block_on(restore_trash_item_inner(&state, &entry.id)).expect("restore does not self-deadlock");

    assert!(state.db.folder_by_id("custom-notes").unwrap().is_some());
    assert_eq!(
        state
            .db
            .get_note_row("n-member")
            .unwrap()
            .unwrap()
            .folder_id,
        "custom-notes",
        "the authored note returns under the restored lifecycle interval"
    );
    assert!(state.db.get_trash_entry(&entry.id).unwrap().is_none());
}

/// A transient meeting re-file failure must leave the folder snapshot available. The folder row is
/// an intentional retryable partial restore; after the DB fault clears, the member and journal
/// converge without losing the original placement.
#[test]
fn folder_restore_meeting_refile_failure_retains_snapshot_for_retry() {
    let state = build_state("folder-meeting-refile-retry");
    open_folder(&state, "restore-folder", "Restore folder");
    seed_meeting(&state, "m-member", Some("restore-folder"));
    delete_folder_inner(&state, "restore-folder".into()).expect("delete folder to trash");
    let entry = state
        .db
        .list_trash_entries()
        .unwrap()
        .into_iter()
        .find(|entry| entry.source_id == "restore-folder")
        .expect("folder snapshot");
    state
        .db
        .lock()
        .execute_batch(
            "CREATE TRIGGER fail_folder_meeting_refile
             BEFORE UPDATE OF folder_id ON meetings
             WHEN NEW.id = 'm-member' AND NEW.folder_id = 'restore-folder'
             BEGIN
               SELECT RAISE(ABORT, 'injected meeting re-file failure');
             END;",
        )
        .unwrap();

    let error = block_on(restore_trash_item_inner(&state, &entry.id))
        .expect_err("meeting re-file failure surfaces");
    assert!(matches!(error, AppError::Storage(_)));
    assert!(state.db.folder_by_id("restore-folder").unwrap().is_some());
    assert_eq!(
        state
            .db
            .get_meeting("m-member")
            .unwrap()
            .unwrap()
            .folder_id,
        None,
        "the failed transaction leaves the member at the delete fallback"
    );
    assert!(state.db.get_trash_entry(&entry.id).unwrap().is_some());

    state
        .db
        .lock()
        .execute_batch("DROP TRIGGER fail_folder_meeting_refile;")
        .unwrap();
    block_on(restore_trash_item_inner(&state, &entry.id)).expect("retry re-files meeting");

    assert_eq!(
        state
            .db
            .get_meeting("m-member")
            .unwrap()
            .unwrap()
            .folder_id
            .as_deref(),
        Some("restore-folder")
    );
    assert!(state.db.get_trash_entry(&entry.id).unwrap().is_none());
}

/// The authored-note twin: an operational move failure retains the journal, while retry uses the
/// lifecycle-authorized move seam and restores the original note-folder placement.
#[test]
fn folder_restore_note_refile_failure_retains_snapshot_for_retry() {
    let state = build_state("folder-note-refile-retry");
    let notes_root = state.db.ensure_notes_root().expect("Notes root");
    open_folder(&state, "restore-notes", "Restore notes");
    state
        .db
        .lock()
        .execute(
            "UPDATE folders SET kind = 'note' WHERE id = 'restore-notes'",
            [],
        )
        .expect("mark note folder");
    state
        .db
        .insert_note(
            "n-member",
            "restore-notes",
            "Member",
            "Member",
            "# Member",
            1_760_000_000_000,
        )
        .expect("insert authored note");
    delete_folder_inner(&state, "restore-notes".into()).expect("delete note folder to trash");
    let entry = state
        .db
        .list_trash_entries()
        .unwrap()
        .into_iter()
        .find(|entry| entry.source_id == "restore-notes")
        .expect("note-folder snapshot");
    state
        .db
        .lock()
        .execute_batch(
            "CREATE TRIGGER fail_folder_note_refile
             BEFORE UPDATE OF folder_id ON documents
             WHEN NEW.id = 'n-member' AND NEW.folder_id = 'restore-notes'
             BEGIN
               SELECT RAISE(ABORT, 'injected note re-file failure');
             END;",
        )
        .unwrap();

    let error = block_on(restore_trash_item_inner(&state, &entry.id))
        .expect_err("note re-file failure surfaces");
    assert!(matches!(error, AppError::Storage(_)));
    assert!(state.db.folder_by_id("restore-notes").unwrap().is_some());
    assert_eq!(
        state
            .db
            .get_note_row("n-member")
            .unwrap()
            .unwrap()
            .folder_id,
        notes_root,
        "the failed move leaves the note at the canonical delete fallback"
    );
    assert!(state.db.get_trash_entry(&entry.id).unwrap().is_some());

    state
        .db
        .lock()
        .execute_batch("DROP TRIGGER fail_folder_note_refile;")
        .unwrap();
    block_on(restore_trash_item_inner(&state, &entry.id)).expect("retry re-files note");

    assert_eq!(
        state
            .db
            .get_note_row("n-member")
            .unwrap()
            .unwrap()
            .folder_id,
        "restore-notes"
    );
    assert!(state.db.get_trash_entry(&entry.id).unwrap().is_none());
}

/// A folder snapshot records where members lived at delete time, but it is not authority to undo a
/// later explicit filing decision. Both content kinds must stay in their newer container while the
/// old folder itself and its journal finish restoring normally.
#[test]
fn folder_restore_preserves_newer_member_placement() {
    let state = build_state("folder-newer-member-placement");
    let notes_root = state.db.ensure_notes_root().expect("Notes root");
    open_folder(&state, "old-home", "Old home");
    open_folder(&state, "new-home", "New home");
    seed_meeting(&state, "m-moved", Some("old-home"));
    state
        .db
        .insert_note(
            "n-moved",
            "old-home",
            "Moved note",
            "Moved note",
            "# Moved note",
            1_760_000_000_000,
        )
        .expect("insert authored note");

    delete_folder_inner(&state, "old-home".into()).expect("delete old folder to trash");
    let entry = state
        .db
        .list_trash_entries()
        .unwrap()
        .into_iter()
        .find(|entry| entry.source_id == "old-home")
        .expect("folder snapshot");
    assert_eq!(
        state.db.get_meeting("m-moved").unwrap().unwrap().folder_id,
        None,
        "delete fallback for a recording is Not classified"
    );
    assert_eq!(
        state.db.get_note_row("n-moved").unwrap().unwrap().folder_id,
        notes_root,
        "delete fallback for a note is the canonical Notes root"
    );

    state
        .db
        .set_meeting_folder("m-moved", Some("new-home"))
        .expect("user re-files recording while old folder is in trash");
    crate::commands::move_note_doc_inner(&state, "n-moved", "new-home")
        .expect("user re-files note while old folder is in trash");

    block_on(restore_trash_item_inner(&state, &entry.id)).expect("restore old folder");

    assert_eq!(
        state
            .db
            .get_meeting("m-moved")
            .unwrap()
            .unwrap()
            .folder_id
            .as_deref(),
        Some("new-home"),
        "restore must not undo the newer recording filing"
    );
    assert_eq!(
        state.db.get_note_row("n-moved").unwrap().unwrap().folder_id,
        "new-home",
        "restore must not undo the newer note filing"
    );
    assert!(state.db.folder_by_id("old-home").unwrap().is_some());
    assert!(state.db.get_trash_entry(&entry.id).unwrap().is_none());
}

/// ORACLE 6 — expiry honours the LIVE retention setting, and an entry is not purged early.
#[test]
fn purge_respects_retention_and_only_takes_expired_entries() {
    let state = build_state("retention");
    open_folder(&state, "f1", "Work");
    seed_meeting(&state, "m1", Some("f1"));
    block_on(delete_meeting_inner(&state, "m1")).expect("delete to trash");

    assert_eq!(retention_days(&state), 30, "default retention is 30 days");
    let purged = block_on(purge_expired(&state, None)).expect("purge pass");
    assert_eq!(purged, 0, "a just-deleted entry is nowhere near expiry");
    assert_eq!(state.db.count_trash_entries().unwrap(), 1);

    let dto = list_trash_inner(&state).unwrap();
    assert_eq!(dto[0].days_left, 29, "29 whole days left on day zero of 30");

    // Backdate past the window: now it expires.
    state
        .db
        .set_trash_deleted_at_for_test(&dto[0].id, "2026-01-01T00:00:00Z")
        .unwrap();
    let purged = block_on(purge_expired(&state, None)).expect("purge pass");
    assert_eq!(purged, 1, "an entry past its retention is purged");
    assert_eq!(state.db.count_trash_entries().unwrap(), 0);
}

/// ORACLE 7 — an UNPARSEABLE `deleted_at` must never be purged. The purge has to fail CLOSED: a row
/// it cannot date is a row whose expiry it does not know, and destroying it would be exactly the
/// unrecoverable loss this table exists to prevent.
#[test]
fn purge_never_destroys_an_entry_it_cannot_date() {
    let state = build_state("undateable");
    open_folder(&state, "f1", "Work");
    seed_meeting(&state, "m1", Some("f1"));
    block_on(delete_meeting_inner(&state, "m1")).expect("delete to trash");
    let entry_id = state.db.list_trash_entries().unwrap()[0].id.clone();
    state
        .db
        .set_trash_deleted_at_for_test(&entry_id, "not-a-timestamp")
        .unwrap();

    let purged = block_on(purge_expired(&state, None)).expect("purge pass");
    assert_eq!(purged, 0, "fail closed — keep the content");
    assert_eq!(state.db.count_trash_entries().unwrap(), 1);
}

/// ORACLE 8 (`rust-tauri.md` §2b) — the DTO crosses IPC as camelCase. Asserts the SERIALIZED key
/// names, not a round-trip through the same Rust type (which passes regardless of naming). This is
/// the check that would have caught the `started_at`/`startedAt` tile bug.
#[test]
fn trash_entry_dto_is_camel_case() {
    let dto = TrashEntry {
        id: "t1".into(),
        kind: "meeting".into(),
        source_id: "m1".into(),
        source_folder_id: Some("f1".into()),
        label: "Roadmap review".into(),
        deleted_at: "2026-08-31T10:00:00Z".into(),
        expires_at: "2026-09-30T10:00:00Z".into(),
        days_left: 29,
        locked: false,
        detail: "30 min · 2 segments".into(),
    };
    let json = serde_json::to_value(&dto).expect("serialize");
    let obj = json.as_object().expect("an object");
    for key in obj.keys() {
        assert!(
            !key.contains('_'),
            "IPC DTO key `{key}` is snake_case — the FE reads camelCase"
        );
        assert!(
            key.chars().next().unwrap().is_ascii_lowercase(),
            "IPC DTO key `{key}` must start lowercase"
        );
    }
    // Name the fields explicitly: a rename that silently drops one would otherwise still pass.
    for expected in [
        "id",
        "kind",
        "sourceId",
        "sourceFolderId",
        "label",
        "deletedAt",
        "expiresAt",
        "daysLeft",
        "locked",
        "detail",
    ] {
        assert!(obj.contains_key(expected), "missing wire key `{expected}`");
    }
}

/// ORACLE 9 — a folder restore brings back its `level`/`emoji`/`tint`, not just its name. A
/// regression here silently demotes a restored Project to a Folder, which the content assertions
/// would never notice.
#[test]
fn folder_restore_preserves_presentation_columns() {
    let state = build_state("folder-presentation");
    state
        .db
        .insert_space(&Folder {
            id: "p1".to_string(),
            name: "Acme".to_string(),
            path: "Acme".to_string(),
            parent_id: None,
            locked: false,
            created_at: "2026-08-31T09:00:00Z".to_string(),
        })
        .expect("insert project");
    let before = state.db.folder_presentation("p1").unwrap().unwrap();
    assert_eq!(before.level, "project", "seeded as a Project");

    let folder = state.db.folder_by_id("p1").unwrap().unwrap();
    let entry_id = capture_folder(&state, &folder, "meeting", &[], &[]).expect("capture folder");
    state.db.delete_folder("p1").expect("drop the row");
    assert!(state.db.folder_by_id("p1").unwrap().is_none());

    block_on(restore_trash_item_inner(&state, &entry_id)).expect("restore");
    let after = state.db.folder_presentation("p1").unwrap().unwrap();
    assert_eq!(after.level, "project", "restored as a Project, not demoted to Folder");
    assert_eq!(after.kind, before.kind);
    assert_eq!(after.position, before.position);
}

/// ORACLE 10 — inline images survive delete → restore.
///
/// `note_attachments` FKs onto `documents(id)` / `notes(meeting_id, provider_id)` with
/// `ON DELETE CASCADE`, so the delete destroys them and the snapshot is their only copy. Nothing
/// about the markdown would reveal this: the text comes back perfectly and every image is broken.
/// That is precisely why it needs its own oracle.
#[test]
fn note_restore_brings_back_its_inline_images() {
    let state = build_state("note-attachments");
    open_folder(&state, "nf1", "Notes");
    let png = b"\x89PNG\r\n\x1a\nfake-image-bytes".to_vec();
    let digest = {
        use sha2::Digest;
        let mut h = sha2::Sha256::new();
        h.update(&png);
        let out: [u8; 32] = h.finalize().into();
        out
    };
    let body = "# With an image\n\n![shot](murmur-attachment://img-1)";
    state
        .db
        .insert_note("n1", "nf1", "With an image", "With an image", body, 1_760_000_000_000)
        .expect("insert note");
    let owner = crate::storage::AttachmentOwner::Document {
        document_id: "n1".to_string(),
    };
    state
        .db
        .insert_attachment(&crate::storage::NewAttachment {
            id: "img-1",
            owner: &owner,
            mime_type: "image/png",
            extension: "png",
            width: 4,
            height: 4,
            sha256: &digest,
            byte_len: png.len(),
            data: &png,
            data_blob: None,
            created_at: 1_760_000_000_000,
        })
        .expect("insert attachment");
    assert_eq!(
        state.db.list_attachments(&owner).unwrap().len(),
        1,
        "control: the attachment is there before the delete"
    );

    block_on(delete_note_inner(&state, "n1")).expect("delete to trash");
    assert!(
        state.db.list_attachments(&owner).unwrap().is_empty(),
        "the FK cascade destroyed it — so the snapshot is the only copy"
    );

    let entry_id = state.db.list_trash_entries().unwrap()[0].id.clone();
    block_on(restore_trash_item_inner(&state, &entry_id)).expect("restore");

    let restored = state.db.list_attachments(&owner).unwrap();
    assert_eq!(restored.len(), 1, "the inline image is back");
    assert_eq!(restored[0].id, "img-1");
    assert_eq!(restored[0].data, png, "image bytes byte-identical");
    assert_eq!(restored[0].sha256, digest, "digest re-verified on restore");
    assert_eq!(restored[0].mime_type, "image/png");
    assert_eq!(state.db.get_note_row("n1").unwrap().unwrap().text, body);
}

/// ORACLE 11 — the hex codec round-trips, and REJECTS malformation instead of returning truncated
/// bytes. A silent partial decode would restore a corrupt image that looks like a successful
/// restore.
#[test]
fn attachment_hex_codec_round_trips_and_rejects_malformed_input() {
    let bytes: Vec<u8> = (0u8..=255).collect();
    let encoded = hex_encode(&bytes);
    assert_eq!(encoded.len(), bytes.len() * 2);
    assert_eq!(hex_decode(&encoded).as_deref(), Some(bytes.as_slice()));
    assert_eq!(hex_encode(&[]), "");
    assert_eq!(hex_decode("").as_deref(), Some(&[][..]));

    assert!(hex_decode("abc").is_none(), "odd length is refused");
    assert!(hex_decode("zz").is_none(), "non-hex digit is refused");
    assert!(hex_decode("00ff0g").is_none(), "a late bad digit is refused");
}

/// Trash → restore must bring back the graph edges too, not just the recording.
///
/// Before this, the snapshot carried the row, its segments, notes, timeline and tags — everything
/// that renders the meeting itself — and nothing that connects it to anything. Deleting a meeting
/// purges its links outright (`preserve_decisions=false`, because the endpoint is gone) and cascades
/// away its `entity_mentions`, so restore produced a meeting that looked perfectly intact and had
/// silently forgotten every person it mentioned and every note it was linked to. Re-indexing does
/// not bring those back: it rebuilds what can be INFERRED from text, so a manual link somebody drew
/// by hand, or an inbound edge from another note, is gone for good.
///
/// The assertions deliberately cover BOTH link directions. An inbound edge is somebody else's link
/// into this meeting, and losing only that would leave the meeting looking connected while the rest
/// of the vault had forgotten it.
#[test]
fn restoring_a_meeting_brings_back_its_links_and_mentions() {
    let state = build_state("trash-graph-restore");
    open_folder(&state, "f1", "Work");
    seed_meeting(&state, "m1", Some("f1"));
    state
        .db
        .insert_note("n-other", "f1", "Other", "Other", "body", 1_760_000_000_000)
        .expect("insert the note on the far side of the inbound edge");

    let entity_id = state
        .db
        .upsert_entity("Anna", crate::storage::models::EntityKind::Person)
        .expect("entity");
    state.db.add_mention(&entity_id, "m1").expect("mention");

    // One edge OUT of the meeting and one INTO it.
    state
        .db
        .upsert_manual_link("meeting", "m1", "note", "n-other")
        .expect("outbound manual link");
    state
        .db
        .upsert_manual_link("note", "n-other", "meeting", "m1")
        .expect("inbound manual link");

    let links_before = state.db.link_rows_for_meeting("m1").unwrap();
    let mentions_before = state.db.entity_mentions_for_meeting("m1").unwrap();
    assert_eq!(links_before.len(), 2, "one edge each way");
    assert_eq!(mentions_before.len(), 1, "one mention");

    block_on(delete_meeting_inner(&state, "m1")).expect("delete to trash");
    assert!(
        state.db.link_rows_for_meeting("m1").unwrap().is_empty(),
        "precondition: the delete really does purge the edges — otherwise this test proves nothing"
    );

    let entries = state.db.list_trash_entries().unwrap();
    block_on(restore_trash_item_inner(&state, &entries[0].id)).expect("restore");

    let links_after = state.db.link_rows_for_meeting("m1").unwrap();
    let mentions_after = state.db.entity_mentions_for_meeting("m1").unwrap();
    assert_eq!(
        links_after, links_before,
        "every edge comes back, in both directions, byte-for-byte"
    );
    assert_eq!(
        mentions_after, mentions_before,
        "and the mention keeps its ORIGINAL created_at — re-dating it would move an old meeting to \
         the top of the entity's timeline"
    );
}

/// Meeting snapshots already carry relations; they are useful only if a replay failure cannot be
/// swallowed. Inject a real DB error and prove the exact shell + journal are retryable WITHOUT the
/// broad meeting-delete cascade (which would erase fresh Ask history and every memory rollup).
#[test]
fn meeting_restore_relation_failure_keeps_retryable_shell_without_broad_purge() {
    let state = build_state("meeting-container-link-retry");
    open_folder(&state, "home", "Home");
    open_folder(&state, "related-folder", "Related folder");
    seed_meeting(&state, "m-related", Some("home"));
    let wav = crate::storage::db::unique_temp_path("murmur-trash-retry-audio", "wav");
    let wav = wav.to_string_lossy().to_string();
    std::fs::write(&wav, b"RIFF....retry fake pcm").expect("write retry wav");
    state
        .db
        .set_meeting_audio_path("m-related", Some(&wav))
        .expect("set retry audio path");
    link_items_inner(
        &state,
        "meeting",
        "m-related",
        "container",
        "related-folder",
    )
    .expect("link recording to whole folder");
    let relation = state.db.link_rows_for_meeting("m-related").unwrap();

    block_on(delete_meeting_inner(&state, "m-related")).expect("delete meeting to trash");
    let entry = state
        .db
        .list_trash_entries()
        .unwrap()
        .into_iter()
        .find(|entry| entry.source_id == "m-related")
        .expect("meeting trash snapshot");
    state
        .db
        .upsert_memory_rollup(
            "weekly:after-delete",
            "fresh unrelated synthesis",
            "fresh-hash",
            "2026-09-04T09:00:00Z",
        )
        .expect("post-delete memory rollup");
    state
        .db
        .persist_ask_exchange(
            &crate::storage::models::AskConversationScope::Vault,
            None,
            "What changed after deletion?",
            "A fresh durable answer.",
            &[],
            &[],
            &[],
            &["home".to_string()],
            "2026-09-04T09:01:00Z",
        )
        .expect("post-delete Ask conversation");
    state
        .db
        .lock()
        .execute_batch(
            "CREATE TRIGGER fail_meeting_relation_restore
             BEFORE INSERT ON links
             WHEN NEW.src_id = 'm-related' OR NEW.dst_id = 'm-related'
             BEGIN
               SELECT RAISE(ABORT, 'injected meeting relation replay failure');
             END;",
        )
        .unwrap();

    let error = block_on(restore_trash_item_inner(&state, &entry.id))
        .expect_err("injected meeting replay failure surfaces");
    assert!(matches!(error, AppError::Storage(_)));
    assert!(
        state.db.get_meeting("m-related").unwrap().is_some(),
        "the exact minimal shell remains available for a non-destructive retry"
    );
    assert!(
        state.db.get_trash_entry(&entry.id).unwrap().is_some(),
        "the relation-bearing journal remains retryable"
    );
    assert_eq!(
        state.db.list_memory_rollups().unwrap().len(),
        1,
        "a replay failure must not invoke the global rollup purge"
    );
    assert_eq!(
        state
            .db
            .list_ask_conversation_ids(
                &crate::storage::models::AskConversationScope::Vault,
                &HashSet::new(),
            )
            .unwrap()
            .len(),
        1,
        "a replay failure must not delete fresh Ask history"
    );
    let purge_error = block_on(purge_one_for_test(&state, &entry.id))
        .expect_err("a live retry shell protects its full meeting snapshot from purge");
    assert!(matches!(purge_error, AppError::Unavailable(_)));
    assert!(state.db.get_trash_entry(&entry.id).unwrap().is_some());
    assert!(std::path::Path::new(&wav).exists(), "retry audio stays owned");

    let redeletion = block_on(delete_meeting_inner(&state, "m-related"))
        .expect_err("a retry shell cannot be snapshotted a second time");
    assert!(matches!(redeletion, AppError::Unavailable(_)));
    assert_eq!(
        state
            .db
            .list_trash_entries()
            .unwrap()
            .into_iter()
            .filter(|candidate| candidate.source_id == "m-related")
            .count(),
        1,
        "failed re-delete creates no competing journal"
    );
    assert!(state.db.get_meeting("m-related").unwrap().is_some());
    assert!(std::path::Path::new(&wav).exists(), "failed re-delete keeps audio");

    state
        .db
        .lock()
        .execute_batch("DROP TRIGGER fail_meeting_relation_restore;")
        .unwrap();
    block_on(restore_trash_item_inner(&state, &entry.id))
        .expect("retry restores recording and relation");

    assert!(state.db.get_meeting("m-related").unwrap().is_some());
    assert_eq!(state.db.link_rows_for_meeting("m-related").unwrap(), relation);
    assert!(state.db.get_trash_entry(&entry.id).unwrap().is_none());
    assert_eq!(state.db.list_memory_rollups().unwrap().len(), 1);
    assert_eq!(
        state
            .db
            .list_ask_conversation_ids(
                &crate::storage::models::AskConversationScope::Vault,
                &HashSet::new(),
            )
            .unwrap()
            .len(),
        1
    );
    assert!(std::path::Path::new(&wav).exists());
    let _ = std::fs::remove_file(&wav);
}

/// A DERIVED edge to a neighbour sealed since the delete must not come back; a user's own must.
///
/// `purge_links_tx` strips derived edges when a folder seals, but a meeting sitting in the trash is
/// invisible to that pass — so a plain verbatim restore resurrects a `wikilink`/`companion`/
/// suggested `semantic` edge pointing at something that has been sealed meanwhile, which the
/// ordinary lifecycle would have removed. Lock-security review traced that such a row is INERT today
/// (every reader re-gates both endpoints live) — but inertness is a property of today's readers, and
/// the row still should not exist.
///
/// The manual edge is the control, and it is the half that makes this test mean something: it proves
/// the filter is discriminating between derived and decided rather than simply dropping edges to
/// sealed folders. A seal already preserves user decisions (`LINK_DECISION_KEEP`); a restore must
/// not be stricter than a seal.
#[test]
fn restore_drops_derived_edges_to_a_since_sealed_neighbour_but_keeps_the_users_own() {
    let state = build_state("trash-derived-sealed");
    open_folder(&state, "f1", "Work");
    open_folder(&state, "f2", "Later-sealed");
    seed_meeting(&state, "m1", Some("f1"));
    state
        .db
        .insert_note("n-far", "f2", "Far", "Far", "body", 1_760_000_000_000)
        .expect("note in the folder that will be sealed");

    state
        .db
        .upsert_manual_link("meeting", "m1", "document", "n-far")
        .expect("the user's own link");
    {
        let mut conn = state.db.lock();
        let tx = conn.transaction().unwrap();
        crate::storage::Db::upsert_link_tx(
            &tx, "meeting", "m1", "document", "n-far", "semantic", 0.9, "auto", "active",
            1_760_000_000_000,
        )
        .expect("a derived suggestion between the same pair");
        tx.commit().unwrap();
    }
    assert_eq!(
        state.db.link_rows_for_meeting("m1").unwrap().len(),
        2,
        "one decided edge and one derived one"
    );

    block_on(delete_meeting_inner(&state, "m1")).expect("delete to trash");

    // The far side seals while the meeting sits in the trash, and stays NOT session-unlocked.
    state
        .db
        .set_folder_locked_for_test("f2", true)
        .expect("seal the neighbour's folder");

    let entries = state.db.list_trash_entries().unwrap();
    block_on(restore_trash_item_inner(&state, &entries[0].id)).expect("restore");

    let after = state.db.link_rows_for_meeting("m1").unwrap();
    let kinds: Vec<&str> = after.iter().map(|r| r.edge_type.as_str()).collect();
    assert!(
        kinds.contains(&"manual"),
        "the user's own link survives, exactly as a seal preserves it. Got {kinds:?}"
    );
    assert!(
        !kinds.contains(&"semantic"),
        "the DERIVED edge must not be resurrected into a folder that sealed meanwhile — the \
         ordinary lifecycle would have purged it. Got {kinds:?}"
    );
}
