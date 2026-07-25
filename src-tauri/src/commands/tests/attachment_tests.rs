use super::*;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, Once};

use crate::settings::AppConfig;
use crate::storage::{AttachmentOwner, Db, Folder, Meeting, MeetingStatus, NoteRecord};
use zeroize::Zeroizing;

const DB_KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const DEV_KEK: &str = "1111111111111111111111111111111111111111111111111111111111111111";
static KEK_ENV: Once = Once::new();

fn build_state(tag: &str, vault: Option<&std::path::Path>) -> AppState {
    KEK_ENV.call_once(|| std::env::set_var("MURMUR_DEV_KEK", DEV_KEK));
    let db_path =
        crate::storage::db::unique_temp_path(&format!("murmur-attachments-{tag}"), "sqlite");
    let _ = std::fs::remove_file(&db_path);
    let config = AppConfig {
        vault_path: vault.map(|path| path.to_string_lossy().to_string()),
        ..AppConfig::default()
    };
    AppState {
        recorder: Mutex::new(None),
        recording_stop: Mutex::new(None),
        voice_listener: Mutex::new(None),
        voice_listener_lifecycle: Mutex::new(()),
        recording_starting: std::sync::atomic::AtomicBool::new(false),
        voice_command_capture: Mutex::new(None),
        pending_manual_command: Mutex::new(None),
        live_running: std::sync::atomic::AtomicBool::new(false),
        db: Arc::new(Db::open_with_key(&db_path, DB_KEY).expect("open attachment test db")),
        config: Arc::new(Mutex::new(config)),
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
        lifecycle: Mutex::new(()),
        active_salvages: Mutex::new(HashSet::new()),
        seal_epoch: std::sync::atomic::AtomicU64::new(0),
        heavy_inference: Arc::new(tokio::sync::Semaphore::new(1)),
    }
}

fn temp_vault(tag: &str) -> std::path::PathBuf {
    let path =
        std::env::temp_dir().join(format!("murmur-attachments-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).expect("create attachment test vault");
    path
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("attachment test runtime")
        .block_on(future)
}

fn webp(width: u32, height: u32) -> Vec<u8> {
    assert!((1..=0x01_00_00_00).contains(&width));
    assert!((1..=0x01_00_00_00).contains(&height));
    let mut bytes = Vec::with_capacity(30);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&22u32.to_le_bytes());
    bytes.extend_from_slice(b"WEBPVP8X");
    bytes.extend_from_slice(&10u32.to_le_bytes());
    bytes.extend_from_slice(&[0, 0, 0, 0]);
    let w = width - 1;
    let h = height - 1;
    bytes.extend_from_slice(&[w as u8, (w >> 8) as u8, (w >> 16) as u8]);
    bytes.extend_from_slice(&[h as u8, (h >> 8) as u8, (h >> 16) as u8]);
    bytes
}

fn png_crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    crc ^ 0xFFFF_FFFF
}

fn png_chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(12 + data.len());
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc_input = Vec::with_capacity(4 + data.len());
    crc_input.extend_from_slice(kind);
    crc_input.extend_from_slice(data);
    out.extend_from_slice(&png_crc32(&crc_input).to_be_bytes());
    out
}

/// A canvas-style PNG (`IHDR sRGB IDAT IEND`), optionally carrying one extra ancillary chunk after
/// `sRGB` — exactly the shape WebKit's `canvas.toBlob("image/png")` produces (it emits `eXIf`).
fn png_image(width: u32, height: u32, extra: Option<(&[u8; 4], &[u8])>) -> Vec<u8> {
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    out.extend_from_slice(&png_chunk(b"IHDR", &ihdr));
    out.extend_from_slice(&png_chunk(b"sRGB", &[0]));
    if let Some((kind, data)) = extra {
        out.extend_from_slice(&png_chunk(kind, data));
    }
    out.extend_from_slice(&png_chunk(b"IDAT", &[0x78, 0x9c, 0x63, 0x00, 0x01]));
    out.extend_from_slice(&png_chunk(b"IEND", &[]));
    out
}

fn base64url(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(ALPHABET[((n >> 18) & 63) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[((n >> 6) & 63) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(n & 63) as usize] as char);
        }
    }
    out
}

fn cache_kek_and_folder_ck(state: &AppState, folder_id: &str) -> [u8; 32] {
    let kek = crate::secrets::get_or_create_master_kek().expect("test master kek");
    let wrapped = state
        .db
        .folder_wrapped_key(folder_id)
        .expect("read wrapped ck")
        .expect("locked folder has wrapped ck");
    let ck = crate::crypto::decrypt(&kek, &wrapped, &aad_wrapped_ck(folder_id))
        .expect("unwrap folder ck");
    *state.master_kek.lock().expect("master kek mutex") = Some(Zeroizing::new(kek));
    ck.try_into().expect("folder ck is 32 bytes")
}

fn add_webp(state: &AppState, owner_kind: &str, owner_id: &str, bytes: &[u8]) -> AttachmentDto {
    add_note_attachment_inner(
        state,
        owner_kind,
        owner_id,
        "clipboard.webp",
        "image/webp",
        &base64url(bytes),
    )
    .expect("add normalized WebP")
}

fn add_png(state: &AppState, owner_kind: &str, owner_id: &str, bytes: &[u8]) -> AttachmentDto {
    add_note_attachment_inner(
        state,
        owner_kind,
        owner_id,
        "clipboard.png",
        "image/png",
        &base64url(bytes),
    )
    .expect("add normalized PNG")
}

fn incoming_png(bytes: &[u8], width: u32, height: u32) -> crate::storage::IncomingAttachment {
    use sha2::{Digest, Sha256};
    crate::storage::IncomingAttachment {
        id: uuid::Uuid::new_v4().to_string(),
        mime_type: "image/png".into(),
        extension: "png".into(),
        width,
        height,
        sha256: Sha256::digest(bytes).into(),
        data: bytes.to_vec(),
    }
}

fn meeting_folder(db: &Db, id: &str) {
    db.insert_folder(&Folder {
        id: id.to_string(),
        name: id.to_string(),
        path: id.to_string(),
        parent_id: None,
        locked: false,
        created_at: "2026-07-22T12:00:00Z".into(),
    })
    .expect("insert meeting folder");
}

fn meeting_note(db: &Db, id: &str, markdown: &str) {
    db.insert_meeting(&Meeting {
        id: id.to_string(),
        started_at: "2026-07-22T12:00:00Z".into(),
        ended_at: None,
        title: Some("Attachment test".into()),
        duration_s: 60,
        audio_path: None,
        status: MeetingStatus::Summarized,
        folder_id: None,
    })
    .expect("insert meeting");
    db.upsert_note(&NoteRecord {
        meeting_id: id.to_string(),
        provider_id: "test-provider".into(),
        markdown: markdown.to_string(),
        created_at: "2026-07-22T12:01:00Z".into(),
        ..NoteRecord::default()
    })
    .expect("insert meeting note");
}

#[test]
fn attachment_export_lock_gate_and_round_trip_are_byte_identical() {
    let vault = temp_vault("lock-cycle");
    let state = build_state("lock-cycle", Some(&vault));
    let folder = create_note_folder_inner(&state, "Private", None).expect("create note folder");
    let note_id = create_note_inner(&state, Some(&folder.id), "Screenshots").expect("create note");
    let bytes = webp(320, 180);
    let attachment = add_webp(&state, "note", &note_id, &bytes);
    let markdown = format!(
        "# Screenshots\n\n![clipboard](murmur-attachment://{})\n",
        attachment.id
    );
    update_note_doc_inner(&state, &note_id, "Screenshots", &markdown).expect("export note");

    let owner = AttachmentOwner::Document {
        document_id: note_id.clone(),
    };
    let exported_attachment = state.db.list_attachments(&owner).expect("list raw images")[0]
        .exported_path
        .clone()
        .expect("asset exported before markdown");
    let exported_note = state
        .db
        .get_note_row(&note_id)
        .expect("read note")
        .expect("note exists")
        .exported_path
        .expect("note exported");
    assert_eq!(
        std::fs::read(&exported_attachment).expect("read asset"),
        bytes
    );
    let obsidian = std::fs::read_to_string(&exported_note).expect("read exported markdown");
    assert!(obsidian.contains("![[Murmur Attachments/"));
    assert!(!obsidian.contains("murmur-attachment://"));

    lock_folder_inner(&state, folder.id.clone()).expect("lock folder");
    assert!(!std::path::Path::new(&exported_attachment).exists());
    assert!(!std::path::Path::new(&exported_note).exists());
    assert!(matches!(
        list_note_attachments_inner(&state, "note", &note_id),
        Err(AppError::Locked(_))
    ));
    assert!(matches!(
        add_note_attachment_inner(
            &state,
            "note",
            &note_id,
            "blocked.webp",
            "image/webp",
            &base64url(&bytes)
        ),
        Err(AppError::Locked(_))
    ));
    let sealed = &state.db.list_attachments(&owner).expect("sealed row")[0];
    assert!(sealed.data.is_empty());
    assert!(sealed.data_blob.is_some());

    let ck = cache_kek_and_folder_ck(&state, &folder.id);
    unseal_attachments_in_folder(&state, &folder.id, &ck, false).expect("session restore");
    state
        .unlocked_folders
        .lock()
        .expect("unlock set")
        .insert(folder.id.clone());
    let listed = list_note_attachments_inner(&state, "note", &note_id).expect("gated list");
    assert_eq!(listed.len(), 1);
    assert_eq!(
        state.db.list_attachments(&owner).expect("restored row")[0].data,
        bytes
    );

    state
        .unlocked_folders
        .lock()
        .expect("unlock set")
        .remove(&folder.id);
    reblank_attachments_in_folder(&state, &folder.id).expect("relock attachment");
    assert!(state.db.list_attachments(&owner).expect("reblanked row")[0]
        .data
        .is_empty());
    let _ = std::fs::remove_dir_all(vault);
}

#[test]
fn deleting_marker_prunes_attachment_row_and_plaintext_export() {
    let vault = temp_vault("marker-prune");
    let state = build_state("marker-prune", Some(&vault));
    let folder = create_note_folder_inner(&state, "Images", None).expect("folder");
    let note_id = create_note_inner(&state, Some(&folder.id), "Prune").expect("note");
    let attachment = add_webp(&state, "note", &note_id, &webp(120, 90));
    let owner = AttachmentOwner::Document {
        document_id: note_id.clone(),
    };
    let with_image = format!(
        "# Prune\n\n![temporary](murmur-attachment://{})\n",
        attachment.id
    );
    update_note_doc_inner(&state, &note_id, "Prune", &with_image).expect("save marker");
    let exported = state.db.list_attachments(&owner).expect("rows")[0]
        .exported_path
        .clone()
        .expect("asset exported");
    assert!(std::path::Path::new(&exported).exists());

    save_note_text_inner(&state, &note_id, "Prune", "# Prune\n\nNo image.\n")
        .expect("save without marker");
    let detached = state
        .db
        .list_attachments(&owner)
        .expect("detached row retained");
    assert_eq!(
        detached.len(),
        1,
        "Cmd-Z must still be able to restore pixels"
    );
    assert!(detached[0].exported_path.is_none());
    assert!(!std::path::Path::new(&exported).exists());

    save_note_text_inner(&state, &note_id, "Prune", &with_image)
        .expect("undo restores the marker without a dangling reference");
    update_note_doc_inner(&state, &note_id, "Prune", &with_image)
        .expect("full save re-exports restored pixels");
    assert!(state.db.list_attachments(&owner).expect("restored row")[0]
        .exported_path
        .as_deref()
        .is_some_and(|path| std::path::Path::new(path).exists()));
    let _ = std::fs::remove_dir_all(vault);
}

#[test]
fn externally_edited_export_refuses_marker_removal_before_markdown_mutation() {
    let vault = temp_vault("edited-export");
    let state = build_state("edited-export", Some(&vault));
    let folder = create_note_folder_inner(&state, "Edited", None).expect("folder");
    let note_id = create_note_inner(&state, Some(&folder.id), "Edited").expect("note");
    let attachment = add_webp(&state, "note", &note_id, &webp(120, 90));
    let markdown = format!(
        "# Edited\n\n![keep](murmur-attachment://{})\n",
        attachment.id
    );
    update_note_doc_inner(&state, &note_id, "Edited", &markdown).expect("initial export");
    let owner = AttachmentOwner::Document {
        document_id: note_id.clone(),
    };
    let exported = state.db.list_attachments(&owner).expect("row")[0]
        .exported_path
        .clone()
        .expect("tracked export");
    let mut edited = std::fs::read(&exported).expect("read export");
    let last = edited.len() - 1;
    edited[last] ^= 1;
    std::fs::write(&exported, &edited).expect("simulate Obsidian-side edit");

    assert!(save_note_text_inner(&state, &note_id, "Changed", "# Changed\n").is_err());
    let persisted = state
        .db
        .get_note_row(&note_id)
        .expect("note row")
        .expect("note");
    assert_eq!(persisted.text, markdown);
    assert_eq!(
        std::fs::read(&exported).expect("edited file preserved"),
        edited
    );
    assert_eq!(
        state
            .db
            .list_attachments(&owner)
            .expect("row retained")
            .len(),
        1
    );
    assert!(lock_folder_inner(&state, folder.id.clone()).is_err());
    assert!(
        !state
            .db
            .folder_by_id(&folder.id)
            .expect("folder row")
            .expect("folder")
            .locked
    );
    let _ = std::fs::remove_dir_all(vault);
}

#[test]
fn relock_all_revokes_visibility_before_reporting_an_edited_attachment_export() {
    let vault = temp_vault("edited-relock-export");
    let state = build_state("edited-relock-export", Some(&vault));
    let folder = create_note_folder_inner(&state, "Private", None).expect("folder");
    let note_id = create_note_inner(&state, Some(&folder.id), "Private").expect("note");
    let attachment = add_webp(&state, "note", &note_id, &webp(120, 90));
    let markdown = format!(
        "# Private\n\n![keep](murmur-attachment://{})\n",
        attachment.id
    );
    update_note_doc_inner(&state, &note_id, "Private", &markdown).expect("initial export");
    lock_folder_inner(&state, folder.id.clone()).expect("initial lock");

    let ck = cache_kek_and_folder_ck(&state, &folder.id);
    unseal_folder_extras(&state, &folder.id, &ck, None).expect("session restore");
    state
        .unlocked_folders
        .lock()
        .expect("unlock set")
        .insert(folder.id.clone());

    let owner = AttachmentOwner::Document {
        document_id: note_id.clone(),
    };
    let row = state.db.list_attachments(&owner).expect("restored row")[0].clone();
    let exported_attachment = row.exported_path.clone().expect("re-exported image");
    let exported_note = state
        .db
        .get_note_row(&note_id)
        .expect("note row")
        .expect("note")
        .exported_path
        .expect("re-exported note");
    let mut edited = std::fs::read(&exported_attachment).expect("read image");
    edited[0] ^= 1;
    std::fs::write(&exported_attachment, &edited).expect("external image edit");

    let error = relock_all_inner(&state).unwrap_err();
    assert!(
        matches!(error, AppError::Storage(_) | AppError::Export(_)),
        "got {error:?}"
    );
    assert!(
        !state
            .unlocked_folders
            .lock()
            .expect("unlock set")
            .contains(&folder.id),
        "emergency relock revokes UI/MCP visibility before any fallible export cleanup"
    );
    assert_eq!(
        std::fs::read(&exported_attachment).expect("edited image survives"),
        edited
    );
    assert!(
        std::path::Path::new(&exported_note).exists(),
        "the all-files preflight keeps the unchanged note export too"
    );
    assert!(!state.db.list_attachments(&owner).expect("row")[0]
        .data
        .is_empty());
    assert!(
        matches!(
            list_note_attachments_inner(&state, "note", &note_id),
            Err(AppError::Locked(_))
        ),
        "the retained recovery bytes are inaccessible through the command read gate"
    );

    let _ = std::fs::remove_dir_all(vault);
}

#[test]
fn lock_recovers_tracked_crash_temp_without_leaving_plaintext() {
    let vault = temp_vault("crash-temp");
    let state = build_state("crash-temp", Some(&vault));
    let folder = create_note_folder_inner(&state, "Crash", None).expect("folder");
    let note_id = create_note_inner(&state, Some(&folder.id), "Crash").expect("note");
    let bytes = webp(48, 32);
    let attachment = add_webp(&state, "note", &note_id, &bytes);
    let desired = vault
        .join("Murmur Attachments")
        .join(format!("{}.webp", attachment.id));
    std::fs::create_dir_all(desired.parent().expect("asset dir")).expect("asset dir");
    state
        .db
        .set_attachment_exported_path(&attachment.id, Some(&desired.to_string_lossy()))
        .expect("durably track before write");
    let temp = desired.with_file_name(format!(".{}.murmur.tmp", attachment.id));
    std::fs::write(&temp, &bytes).expect("simulate crash after temp fsync");

    lock_folder_inner(&state, folder.id.clone()).expect("lock cleans tracked crash state");
    assert!(!desired.exists());
    assert!(!temp.exists());
    assert!(state
        .db
        .list_attachments(&AttachmentOwner::Document {
            document_id: note_id,
        })
        .expect("sealed row")[0]
        .data
        .is_empty());
    let _ = std::fs::remove_dir_all(vault);
}

#[test]
fn one_off_export_uses_a_separate_user_owned_attachment_root() {
    let vault = temp_vault("tracked-root");
    let user_root = temp_vault("user-root");
    let state = build_state("cross-root", Some(&vault));
    let folder = create_note_folder_inner(&state, "Cross root", None).expect("folder");
    let note_id = create_note_inner(&state, Some(&folder.id), "Cross root").expect("note");
    let attachment = add_webp(&state, "note", &note_id, &webp(64, 36));
    let owner = AttachmentOwner::Document {
        document_id: note_id.clone(),
    };
    let markdown = format!("![x](murmur-attachment://{})", attachment.id);
    render_markdown_with_attachments_for_export(&state, &owner, &markdown, &vault)
        .expect("tracked vault export");
    let tracked = state.db.list_attachments(&owner).expect("row")[0]
        .exported_path
        .clone()
        .expect("tracked path");

    let rendered =
        render_markdown_with_attachments_for_user_export(&state, &owner, &markdown, &user_root)
            .expect("independent user export");
    let user_asset = user_root
        .join("Murmur Attachments")
        .join(format!("{}.webp", attachment.id));
    assert!(rendered.contains("![[Murmur Attachments/"));
    assert!(user_asset.exists());
    assert_eq!(
        state.db.list_attachments(&owner).expect("row unchanged")[0]
            .exported_path
            .as_deref(),
        Some(tracked.as_str())
    );

    lock_folder_inner(&state, folder.id.clone()).expect("lock tracked vault only");
    assert!(!std::path::Path::new(&tracked).exists());
    assert!(user_asset.exists(), "explicit export belongs to the user");
    let _ = std::fs::remove_dir_all(vault);
    let _ = std::fs::remove_dir_all(user_root);
}

#[test]
fn lock_refuses_before_publish_when_attachment_export_cannot_be_removed() {
    let vault = temp_vault("lock-unlink-failure");
    let state = build_state("lock-unlink-failure", Some(&vault));
    let folder = create_note_folder_inner(&state, "Private", None).expect("folder");
    let note_id = create_note_inner(&state, Some(&folder.id), "Private").expect("note");
    add_webp(&state, "note", &note_id, &webp(32, 24));
    let owner = AttachmentOwner::Document {
        document_id: note_id.clone(),
    };
    let attachment = state.db.list_attachments(&owner).expect("attachment")[0].clone();
    let directory_target = vault.join("cannot-remove-as-file");
    std::fs::create_dir_all(&directory_target).expect("directory target");
    state
        .db
        .set_attachment_exported_path(&attachment.id, Some(&directory_target.to_string_lossy()))
        .expect("record target");

    assert!(lock_folder_inner(&state, folder.id.clone()).is_err());
    assert!(
        !state
            .db
            .folder_by_id(&folder.id)
            .expect("folder row")
            .expect("folder")
            .locked
    );
    let retained = &state.db.list_attachments(&owner).expect("retained")[0];
    assert!(!retained.data.is_empty());
    assert_eq!(
        retained.exported_path.as_deref(),
        Some(directory_target.to_string_lossy().as_ref())
    );
    let _ = std::fs::remove_dir_all(vault);
}

#[test]
fn deleting_note_or_meeting_removes_tracked_plaintext_images_before_cascade() {
    let vault = temp_vault("owner-delete");
    let state = build_state("owner-delete", Some(&vault));
    let folder = create_note_folder_inner(&state, "Delete", None).expect("folder");
    let note_id = create_note_inner(&state, Some(&folder.id), "Delete").expect("note");
    let note_attachment = add_webp(&state, "note", &note_id, &webp(16, 12));
    let note_file = vault.join("note-delete.webp");
    std::fs::write(&note_file, webp(16, 12)).expect("note asset");
    state
        .db
        .set_attachment_exported_path(&note_attachment.id, Some(&note_file.to_string_lossy()))
        .expect("track note asset");
    block_on(delete_note_inner(&state, &note_id)).expect("delete note");
    assert!(!note_file.exists());

    meeting_note(&state.db, "meeting-delete", "# Delete meeting");
    let meeting_attachment = add_webp(&state, "meeting", "meeting-delete", &webp(20, 15));
    let meeting_file = vault.join("meeting-delete.webp");
    std::fs::write(&meeting_file, webp(20, 15)).expect("meeting asset");
    state
        .db
        .set_attachment_exported_path(
            &meeting_attachment.id,
            Some(&meeting_file.to_string_lossy()),
        )
        .expect("track meeting asset");
    block_on(delete_meeting_inner(&state, "meeting-delete")).expect("delete meeting");
    assert!(!meeting_file.exists());
    let _ = std::fs::remove_dir_all(vault);
}

#[test]
fn shorter_backtick_run_cannot_escape_a_longer_fenced_code_block() {
    let hidden = "11111111-1111-4111-8111-111111111111";
    let falsely_activated = "22222222-2222-4222-8222-222222222222";
    let active = "33333333-3333-4333-8333-333333333333";
    let markdown = format!(
        "````markdown\n![hidden](murmur-attachment://{hidden})\n```\n![still code](murmur-attachment://{falsely_activated})\n````\n![active](murmur-attachment://{active})"
    );

    let markers = super::parse_attachment_markers(&markdown).expect("parse markers");
    assert_eq!(
        markers
            .iter()
            .map(|marker| marker.id.as_str())
            .collect::<Vec<_>>(),
        vec![active],
        "a shorter closing run is literal fenced-code content and must not activate private bytes"
    );
}

#[test]
fn four_space_indented_fence_cannot_close_an_active_code_block() {
    let hidden = "44444444-4444-4444-8444-444444444444";
    let active = "55555555-5555-4555-8555-555555555555";
    let markdown = format!(
        "````\n    ````\n![still code](murmur-attachment://{hidden})\n````\n![active](murmur-attachment://{active})"
    );

    let markers = super::parse_attachment_markers(&markdown).expect("parse markers");
    assert_eq!(
        markers
            .iter()
            .map(|marker| marker.id.as_str())
            .collect::<Vec<_>>(),
        vec![active]
    );
}

#[test]
fn save_rejects_unknown_attachment_marker_before_mutating_canonical_markdown() {
    let state = build_state("unknown-marker-save", None);
    let folder = create_note_folder_inner(&state, "Unknown", None).expect("folder");
    let note_id = create_note_inner(&state, Some(&folder.id), "Original").expect("note");
    save_note_text_inner(&state, &note_id, "Original", "authored original").expect("seed note");

    let unknown = "99999999-9999-4999-8999-999999999999";
    let forged = format!("![foreign](murmur-attachment://{unknown})");
    assert!(save_note_text_inner(&state, &note_id, "Changed", &forged).is_err());
    assert_eq!(
        state
            .db
            .get_note_row(&note_id)
            .expect("authored row")
            .expect("authored note")
            .text,
        "authored original"
    );

    meeting_note(&state.db, "unknown-marker-meeting", "meeting original");
    assert!(update_note_inner_with(&state, "unknown-marker-meeting", &forged, None).is_err());
    assert_eq!(
        state
            .db
            .get_latest_note_for_meeting("unknown-marker-meeting")
            .expect("meeting row")
            .expect("meeting note")
            .markdown,
        "meeting original"
    );
}

#[test]
fn direct_ipc_note_writes_reject_pending_attachment_markers_before_mutation() {
    let state = build_state("pending-marker-save", None);
    let folder = create_note_folder_inner(&state, "Pending", None).expect("folder");
    let note_id = create_note_inner(&state, Some(&folder.id), "Original").expect("note");
    save_note_text_inner(&state, &note_id, "Original", "authored original").expect("seed note");
    let pending = "![uploading](murmur-pending://local-editor-token)";

    assert!(save_note_text_inner(&state, &note_id, "Changed", pending).is_err());
    assert!(update_note_doc_inner(&state, &note_id, "Changed", pending).is_err());
    assert_eq!(
        state
            .db
            .get_note_row(&note_id)
            .expect("authored row")
            .expect("authored note")
            .text,
        "authored original"
    );

    meeting_note(&state.db, "pending-marker-meeting", "meeting original");
    assert!(update_note_inner_with(&state, "pending-marker-meeting", pending, None).is_err());
    assert_eq!(
        state
            .db
            .get_latest_note_for_meeting("pending-marker-meeting")
            .expect("meeting row")
            .expect("meeting note")
            .markdown,
        "meeting original"
    );
}

#[test]
fn authored_note_attachment_rekeys_locked_to_open_moves() {
    let state = build_state("authored-move", None);
    let source = create_note_folder_inner(&state, "Source", None).expect("source folder");
    let target = create_note_folder_inner(&state, "Target", None).expect("target folder");
    let note_id = create_note_inner(&state, Some(&source.id), "Move me").expect("create note");
    let bytes = webp(64, 48);
    add_webp(&state, "note", &note_id, &bytes);
    lock_folder_inner(&state, target.id.clone()).expect("lock target");
    let ck = cache_kek_and_folder_ck(&state, &target.id);
    state
        .unlocked_folders
        .lock()
        .expect("unlock set")
        .insert(target.id.clone());

    move_note_doc_inner(&state, &note_id, &target.id).expect("move into locked target");
    let owner = AttachmentOwner::Document {
        document_id: note_id.clone(),
    };
    let moved = &state.db.list_attachments(&owner).expect("moved row")[0];
    assert_eq!(moved.data, bytes);
    assert!(moved.data_blob.is_some());

    state
        .unlocked_folders
        .lock()
        .expect("unlock set")
        .remove(&target.id);
    reblank_attachments_in_folder(&state, &target.id).expect("reblank target");
    unseal_attachments_in_folder(&state, &target.id, &ck, false).expect("restore target");
    state
        .unlocked_folders
        .lock()
        .expect("unlock set")
        .insert(target.id.clone());
    move_note_doc_inner(&state, &note_id, &source.id).expect("move back to open source");
    let opened = &state.db.list_attachments(&owner).expect("opened row")[0];
    assert_eq!(opened.data, bytes);
    assert!(opened.data_blob.is_none());
}

#[test]
fn meeting_attachment_rekeys_locked_to_open_moves() {
    let state = build_state("meeting-move", None);
    meeting_note(&state.db, "meeting-a", "# Meeting with image");
    meeting_folder(&state.db, "locked-target");
    let bytes = webp(80, 45);
    add_webp(&state, "meeting", "meeting-a", &bytes);
    lock_folder_inner(&state, "locked-target".into()).expect("lock target");
    cache_kek_and_folder_ck(&state, "locked-target");
    state
        .unlocked_folders
        .lock()
        .expect("unlock set")
        .insert("locked-target".into());

    move_note_inner_impl(&state, "meeting-a".into(), Some("locked-target".into()))
        .expect("move meeting into locked folder");
    let owner = AttachmentOwner::Meeting {
        meeting_id: "meeting-a".into(),
        provider_id: "test-provider".into(),
    };
    let locked = &state.db.list_attachments(&owner).expect("locked row")[0];
    assert_eq!(locked.data, bytes);
    assert!(locked.data_blob.is_some());

    move_note_inner_impl(&state, "meeting-a".into(), None).expect("move meeting to open root");
    let opened = &state.db.list_attachments(&owner).expect("open row")[0];
    assert_eq!(opened.data, bytes);
    assert!(opened.data_blob.is_none());
    assert_eq!(
        state.db.folder_for_meeting("meeting-a").expect("folder"),
        None
    );
}

#[test]
fn tampered_plaintext_or_blob_fails_before_read_export_or_blank() {
    let vault = temp_vault("tamper");
    let state = build_state("tamper", Some(&vault));
    let folder = create_note_folder_inner(&state, "Tamper", None).expect("folder");
    let note_id = create_note_inner(&state, Some(&folder.id), "Tamper").expect("note");
    let bytes = webp(40, 30);
    let attachment = add_webp(&state, "note", &note_id, &bytes);
    let owner = AttachmentOwner::Document {
        document_id: note_id.clone(),
    };
    let mut tampered = bytes.clone();
    let last = tampered.len() - 1;
    tampered[last] ^= 1;
    state
        .db
        .lock()
        .execute(
            "UPDATE note_attachments SET data=?2 WHERE id=?1",
            rusqlite::params![attachment.id, tampered],
        )
        .expect("tamper plaintext");
    assert!(
        attachment_bundle_for_owner(&state, &owner, &HashSet::from([attachment.id.clone()]))
            .is_err()
    );
    assert!(render_markdown_with_attachments_for_export(
        &state,
        &owner,
        &format!("![x](murmur-attachment://{})", attachment.id),
        &vault
    )
    .is_err());
    assert!(lock_folder_inner(&state, folder.id.clone()).is_err());
    assert!(
        !state
            .db
            .folder_by_id(&folder.id)
            .expect("folder row")
            .expect("folder exists")
            .locked
    );
    assert!(!state.db.list_attachments(&owner).expect("tampered row")[0]
        .data
        .is_empty());

    state
        .db
        .lock()
        .execute(
            "UPDATE note_attachments SET data=?2 WHERE id=?1",
            rusqlite::params![attachment.id, bytes],
        )
        .expect("restore plaintext");
    lock_folder_inner(&state, folder.id.clone()).expect("lock after repair");
    let mut blob = state.db.list_attachments(&owner).expect("sealed row")[0]
        .data_blob
        .clone()
        .expect("sealed blob");
    let last = blob.len() - 1;
    blob[last] ^= 1;
    state
        .db
        .lock()
        .execute(
            "UPDATE note_attachments SET data_blob=?2 WHERE id=?1",
            rusqlite::params![attachment.id, blob],
        )
        .expect("tamper blob");
    cache_kek_and_folder_ck(&state, &folder.id);
    state
        .unlocked_folders
        .lock()
        .expect("unlock set")
        .insert(folder.id.clone());
    assert!(list_note_attachments_inner(&state, "note", &note_id).is_err());
    let _ = std::fs::remove_dir_all(vault);
}

#[test]
fn org_feed_attachment_bundle_remaps_and_authoritatively_replaces_local_replica() {
    use sha2::{Digest, Sha256};

    let state = build_state("org-feed-bundle", None);
    state
        .db
        .upsert_org_state(&crate::storage::OrgState {
            org_id: "org-images".into(),
            name: "Images".into(),
            role: "member".into(),
            joined_at: "2026-07-23T00:00:00Z".into(),
            consented: true,
            last_seq: 0,
            generation: 1,
            context_enabled: true,
        })
        .expect("org state");

    let wire_id = uuid::Uuid::new_v4().to_string();
    let bytes = webp(96, 64);
    let markdown = format!("before ![diagram](murmur-attachment://{wire_id}) after");
    let wire = murmur_protocol::envelope::ShareAttachment {
        id: wire_id.clone(),
        mime_type: "image/webp".into(),
        width: 96,
        height: 64,
        sha256: Sha256::digest(&bytes).to_vec(),
        data: bytes.clone(),
    };
    let (local_markdown, incoming) =
        crate::commands::org_commands::prepare_incoming_attachment_bundle(&markdown, &[wire])
            .expect("validate and remap authenticated feed bundle");
    assert_eq!(incoming.len(), 1);
    assert_ne!(incoming[0].id, wire_id);
    assert!(!local_markdown.contains(&wire_id));
    assert!(local_markdown.contains(&incoming[0].id));

    state
        .db
        .upsert_org_item(
            "org-item-images",
            "org-images",
            1,
            "member",
            "Image note",
            &local_markdown,
            "2026-07-23T00:00:00Z",
            1,
            1,
            &[7; 32],
            Some("document"),
            Some("author"),
            None,
        )
        .expect("org replica");
    state
        .db
        .replace_org_item_attachment_bundle("org-item-images", &incoming)
        .expect("materialize exact org image bundle");
    let owner = AttachmentOwner::OrgItem {
        item_id: "org-item-images".into(),
    };
    let rows = state.db.list_attachments(&owner).expect("org images");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].data, bytes);

    // A later authoritative feed revision with no image manifest removes the old local image set.
    state
        .db
        .replace_org_item_attachment_bundle("org-item-images", &[])
        .expect("replace with empty authoritative bundle");
    assert!(state
        .db
        .list_attachments(&owner)
        .expect("empty org images")
        .is_empty());
}

#[test]
fn add_note_attachment_accepts_metadata_free_png_and_rejects_exif() {
    let state = build_state("png-upload", None);
    let folder = create_note_folder_inner(&state, "PNG", None).expect("folder");
    let note_id = create_note_inner(&state, Some(&folder.id), "PNG").expect("note");

    // The WebKit fallback path: a clean, metadata-free PNG must round-trip through the gated add.
    let clean = png_image(48, 32, None);
    let dto = add_png(&state, "note", &note_id, &clean);
    assert_eq!(dto.mime_type, "image/png");
    assert_eq!(dto.extension, "png");
    assert_eq!((dto.width, dto.height), (48, 32));

    // WebP still works alongside the new PNG path.
    add_webp(&state, "note", &note_id, &webp(40, 30));

    // A PNG that still carries an eXIf chunk (the exact chunk WebKit emits before the FE strips it)
    // is rejected: the strip is enforced by the backend, not merely assumed.
    let with_exif = png_image(48, 32, Some((b"eXIf", b"\x00\x01\x02")));
    assert!(matches!(
        add_note_attachment_inner(
            &state,
            "note",
            &note_id,
            "clipboard.png",
            "image/png",
            &base64url(&with_exif),
        ),
        Err(AppError::InvalidArg(_))
    ));
}

#[test]
fn locked_folder_png_attachment_seals_and_round_trips_byte_identical() {
    let vault = temp_vault("png-seal");
    let state = build_state("png-seal", Some(&vault));
    let folder = create_note_folder_inner(&state, "Private PNG", None).expect("folder");
    let note_id = create_note_inner(&state, Some(&folder.id), "Screens").expect("note");
    let bytes = png_image(64, 40, None);
    add_png(&state, "note", &note_id, &bytes);
    let owner = AttachmentOwner::Document {
        document_id: note_id.clone(),
    };

    lock_folder_inner(&state, folder.id.clone()).expect("lock folder");
    let sealed = &state.db.list_attachments(&owner).expect("sealed row")[0];
    assert!(sealed.data.is_empty(), "plaintext blanked after verified seal");
    assert!(sealed.data_blob.is_some(), "recoverable seal retained");

    let ck = cache_kek_and_folder_ck(&state, &folder.id);
    unseal_attachments_in_folder(&state, &folder.id, &ck, false).expect("session restore");
    assert_eq!(
        state.db.list_attachments(&owner).expect("restored row")[0].data,
        bytes,
        "PNG seal decrypts byte-identical to the original plaintext"
    );
    let _ = std::fs::remove_dir_all(vault);
}

#[test]
fn share_ingest_accepts_clean_png_and_rejects_metadata_png() {
    let clean = incoming_png(&png_image(96, 64, None), 96, 64);
    validate_incoming_attachment_bundle(std::slice::from_ref(&clean))
        .expect("a clean PNG share bundle materializes on the recipient");

    let with_exif = incoming_png(&png_image(96, 64, Some((b"eXIf", b"payload"))), 96, 64);
    assert!(matches!(
        validate_incoming_attachment_bundle(std::slice::from_ref(&with_exif)),
        Err(AppError::InvalidArg(_))
    ));

    // A WebP bundle still validates unchanged.
    let webp_bytes = webp(96, 64);
    let webp_item = crate::storage::IncomingAttachment {
        id: uuid::Uuid::new_v4().to_string(),
        mime_type: "image/webp".into(),
        extension: "webp".into(),
        width: 96,
        height: 64,
        sha256: {
            use sha2::{Digest, Sha256};
            Sha256::digest(&webp_bytes).into()
        },
        data: webp_bytes,
    };
    validate_incoming_attachment_bundle(std::slice::from_ref(&webp_item))
        .expect("WebP share bundle still validates");
}

/// RED→GREEN for the org / Murmur↔Murmur E2EE share-ingest ENTRYPOINT (not the inner validator the
/// other test exercises). WebKit clients now share metadata-free PNGs; `prepare_incoming_attachment_bundle`
/// used to hard-reject non-WebP and hardcode `extension: "webp"`, so accepting a shared note that
/// carried a PNG image failed (`ingest_shared_note` → `?`). This asserts the real entrypoint now
/// remaps + validates a clean PNG bundle end-to-end, deriving the correct extension.
#[test]
fn org_share_ingest_entrypoint_accepts_clean_png_bundle() {
    use sha2::{Digest, Sha256};

    let wire_id = uuid::Uuid::new_v4().to_string();
    let bytes = png_image(96, 64, None);
    let markdown = format!("before ![shot](murmur-attachment://{wire_id}) after");
    let wire = murmur_protocol::envelope::ShareAttachment {
        id: wire_id.clone(),
        mime_type: "image/png".into(),
        width: 96,
        height: 64,
        sha256: Sha256::digest(&bytes).to_vec(),
        data: bytes.clone(),
    };
    let (local_markdown, incoming) =
        crate::commands::org_commands::prepare_incoming_attachment_bundle(&markdown, &[wire])
            .expect("a PNG share bundle must materialize through the real ingest entrypoint");
    assert_eq!(incoming.len(), 1);
    assert_eq!(incoming[0].mime_type, "image/png");
    assert_eq!(incoming[0].extension, "png");
    assert_eq!(incoming[0].data, bytes);
    assert_ne!(incoming[0].id, wire_id);
    assert!(local_markdown.contains(&incoming[0].id));

    // A PNG that still carries an eXIf chunk is rejected by the entrypoint (metadata rejector runs
    // inside prepare's validate_incoming_attachment_bundle), not merely by the inner validator.
    let exif_id = uuid::Uuid::new_v4().to_string();
    let exif_bytes = png_image(96, 64, Some((b"eXIf", b"payload")));
    let exif_md = format!("![x](murmur-attachment://{exif_id})");
    let exif_wire = murmur_protocol::envelope::ShareAttachment {
        id: exif_id,
        mime_type: "image/png".into(),
        width: 96,
        height: 64,
        sha256: Sha256::digest(&exif_bytes).to_vec(),
        data: exif_bytes,
    };
    assert!(matches!(
        crate::commands::org_commands::prepare_incoming_attachment_bundle(&exif_md, &[exif_wire]),
        Err(AppError::InvalidArg(_))
    ));
}
