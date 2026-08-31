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
use std::sync::{Arc, Mutex, Once};

use crate::settings::AppConfig;
use crate::storage::{Db, Folder, Meeting, MeetingStatus, NoteRecord};
use crate::transcribe::types::Segment;

// BUILT, not written out: a literal 64-hex string in a diff is what the repo's secret scanner
// exists to catch, and a test fixture is not worth teaching it to ignore that shape. These are
// throwaway keys for a temp SQLCipher file, but they must not LOOK like a real DEK/KEK.
fn db_key() -> String {
    "0123456789abcdef".repeat(4)
}

fn dev_kek() -> String {
    "2".repeat(64)
}

static KEK_ENV: Once = Once::new();

fn build_state(tag: &str) -> AppState {
    KEK_ENV.call_once(|| std::env::set_var("MURMUR_DEV_KEK", dev_kek()));
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
