//! Trash — the 30-day recoverable holding area for deleted content, and its restore/purge surface.
//!
//! See `storage/trash_store.rs` for WHY this is a snapshot table and not a `deleted_at` column
//! (455 SQL sites + every derived surface would have to be gated; one miss is a leak).
//!
//! # The two invariants this file owns
//!
//! **1. VERIFY-BEFORE-DESTROY.** [`capture_meeting`] / [`capture_note`] / [`capture_folder`] write
//! the snapshot, then READ IT BACK, re-parse it, and assert it reconstructs the content that is
//! about to be destroyed. Only then does the caller run the destructive cascade. A snapshot that
//! does not verify REFUSES the delete — the user keeps their content and sees an error, which is
//! always the better failure than a "deleted" item that can never come back. This is the same
//! ordering `crypto::encrypt_file` and `lock_folder` use, applied to a different destroyer.
//!
//! **2. A SNAPSHOT IS GOVERNED BY ITS SOURCE FOLDER'S LOCK.** A snapshot is plaintext content, so
//! it must never outlive the lock that was protecting it:
//!   - captured from a SEALED (session-unlocked) folder ⇒ sealed under that folder's CK immediately,
//!     inside the capture, so it is never at rest in plaintext;
//!   - captured from an OPEN folder that is locked LATER ⇒ sealed by [`seal_trash_in_folder`], which
//!     `seal_folder_extras` calls alongside the note/document/timeline seals;
//!   - read while sealed-and-not-session-unlocked ⇒ MASKED (`locked: true`, no label, no payload),
//!     and restore is REFUSED with `AppError::Locked`.
//!
//! # What is deliberately NOT snapshotted
//!
//! Derived data — FTS rows, vec0 embeddings, `doc_chunks`, entity mentions, graph edges, analytics
//! aggregates. All of it is re-derivable from the canonical content, and re-deriving on restore is
//! strictly safer than freezing a copy that could disagree with a later schema. Restore re-indexes
//! best-effort and logs a warning on failure; the canonical rows are already back by then, so a
//! failed re-index degrades search, never content.

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};

use super::*;
use crate::storage::trash_store::{
    RawTrashEntry, TrashKind, DEFAULT_TRASH_RETENTION_DAYS, MAX_TRASH_RETENTION_DAYS,
    MIN_TRASH_RETENTION_DAYS,
};

/// How often the background loop looks for expired entries. HOURLY, not per-minute: retention is
/// measured in DAYS, so a finer cadence would only burn wakeups to purge something a few minutes
/// earlier. Matches the memory-consolidation cadence rather than inventing a new one.
pub const TRASH_PURGE_TICK_SECS: u64 = 3600;

/// Settings key holding the retention window in days.
const TRASH_RETENTION_KEY: &str = "trash_retention_days";

/// Snapshot format version. Bump ONLY additively (new optional fields); a restore reads any version
/// it understands and refuses one it does not, rather than guessing at a shape it was not written
/// for. `#[serde(default)]` on every field added after v1 is what keeps an OLD snapshot readable by
/// a NEW binary — the case that actually matters, since entries sit in the trash across updates.
const SNAPSHOT_VERSION: u32 = 1;

// ── SNAPSHOT PAYLOADS ────────────────────────────────────────────────────────────────────────────
//
// These are an AT-REST format, NOT an IPC DTO: deliberately snake_case with NO `rename_all`, so the
// camelCase wire convention (`rust-tauri.md` §2b) can change without invalidating snapshots already
// sitting in a user's trash. Nothing here crosses the IPC boundary — `TrashEntry` below does.

/// A recording's full restorable state.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct MeetingSnapshot {
    version: u32,
    id: String,
    started_at: String,
    ended_at: Option<String>,
    title: Option<String>,
    duration_s: i64,
    audio_path: Option<String>,
    status: String,
    folder_id: Option<String>,
    segments: Vec<SegmentSnapshot>,
    notes: Vec<NoteRecordSnapshot>,
    #[serde(default)]
    timeline: Option<String>,
    #[serde(default)]
    manual_notes: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    mic_master_path: Option<String>,
    #[serde(default)]
    sys_master_path: Option<String>,
    /// Inline images across ALL of this meeting's provider notes — they cascade-delete with the
    /// `notes` rows, so the snapshot is their only copy.
    #[serde(default)]
    attachments: Vec<AttachmentSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SegmentSnapshot {
    idx: i64,
    start_s: f64,
    end_s: f64,
    text: String,
    #[serde(default)]
    speaker: Option<String>,
    /// Per-segment ASR confidence. Non-content metadata, but it is persisted on the row and drives
    /// the low-confidence review UI, so a restore that dropped it would silently downgrade the
    /// transcript's provenance.
    #[serde(default)]
    confidence: Option<f32>,
}

/// One inline image attachment.
///
/// `note_attachments` FKs onto `notes(meeting_id, provider_id)` and `documents(id)` with
/// `ON DELETE CASCADE`, so the delete destroys these rows along with the content. Without them a
/// restored note would come back with its markdown intact and every `murmur-attachment://` image
/// broken — content loss that no assertion about the markdown would notice.
///
/// `data` is HEX because JSON cannot carry raw bytes. Hex, not base64, because there is no base64
/// crate in this workspace and adding a dependency needs explicit approval — and hex needs none.
/// The cost is honest: a hex payload is 2× the bytes (base64 would be ~1.33×). That is acceptable
/// here because the trash is bounded by retention and the alternative — a side table of raw BLOBs —
/// would need its own seal/unseal/relock lifecycle, i.e. a second copy of the machinery in this
/// file to get wrong. Keeping ONE at-rest format that the existing payload seal already covers is
/// worth the size.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct AttachmentSnapshot {
    id: String,
    /// The provider note this image hangs off (meeting-owned attachments only).
    #[serde(default)]
    provider_id: Option<String>,
    mime_type: String,
    extension: String,
    width: u32,
    height: u32,
    byte_len: u64,
    /// Hex-encoded SHA-256 of the plaintext bytes — re-verified on restore.
    sha256: String,
    /// Hex-encoded plaintext bytes.
    data_hex: String,
    #[serde(default)]
    exported_path: Option<String>,
    created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct NoteRecordSnapshot {
    provider_id: String,
    markdown: String,
    created_at: String,
    #[serde(default)]
    exported_path: Option<String>,
}

/// An authored note's full restorable state.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct NoteSnapshot {
    version: u32,
    id: String,
    folder_id: String,
    name: String,
    title: Option<String>,
    text: String,
    created_at: i64,
    #[serde(default)]
    updated_at: Option<i64>,
    #[serde(default)]
    exported_path: Option<String>,
    /// Inline images — they cascade-delete with the `documents` row.
    #[serde(default)]
    attachments: Vec<AttachmentSnapshot>,
}

/// A container's restorable state, plus WHERE its contents were rehomed to.
///
/// `delete_folder_inner` never destroys content — it rehomes meetings to the vault root and
/// reparents authored notes to the default note-folder, then drops the (empty) folder row. So the
/// snapshot only needs the row itself plus the member ids, and restore recreates the container and
/// moves those members back.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FolderSnapshot {
    version: u32,
    id: String,
    name: String,
    path: String,
    parent_id: Option<String>,
    created_at: String,
    kind: String,
    /// Meetings this folder governed, rehomed to the vault root by the delete.
    #[serde(default)]
    meeting_ids: Vec<String>,
    /// Authored notes reparented to the default note-folder by the delete.
    #[serde(default)]
    note_ids: Vec<String>,
    /// Whether the container was sealed at delete time. Informational only: `delete_folder_inner`
    /// permanently REMOVES the lock before dropping the row (it has to, or the sealed content would
    /// be orphaned from its key), so a restore brings the container back OPEN. Surfaced so the FE
    /// can tell the user their lock did not survive rather than letting them assume it did.
    #[serde(default)]
    was_locked: bool,
    /// The row's presentation/placement columns (`kind`/`level`/`emoji`/`tint`/`position`/`is_root`).
    /// Optional so a snapshot written before this field existed still restores — it falls back to the
    /// migration's own defaults. Without it a restored Workspace would silently demote from Project
    /// to Folder and lose its emoji/tint/ordering.
    #[serde(default)]
    presentation: Option<crate::storage::trash_store::FolderPresentation>,
}

// ── IPC DTO ──────────────────────────────────────────────────────────────────────────────────────

/// One trash entry as the FE sees it. camelCase per `rust-tauri.md` §2b (asserted by
/// `trash_tests::trash_entry_dto_is_camel_case`).
///
/// A sealed-and-not-session-unlocked entry is MASKED: `label` is the lock sentinel, `locked` is
/// true, and NOTHING derived from the payload is present.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrashEntry {
    /// The trash entry id — what `restore_trash_item` / `delete_trash_item_forever` take. NOT the
    /// deleted entity's own id (that is `sourceId`).
    pub id: String,
    /// `"meeting"` | `"note"` | `"folder"` | `"noteFolder"`.
    pub kind: String,
    pub source_id: String,
    pub source_folder_id: Option<String>,
    /// Display title, or `"🔒 Locked"` when masked.
    pub label: String,
    pub deleted_at: String,
    /// RFC3339 instant this entry is purged at — `deletedAt` + the LIVE retention setting, so
    /// changing retention re-dates entries already in the trash instead of freezing each at capture.
    pub expires_at: String,
    /// Whole days remaining before the purge; `0` on the final day (never negative).
    pub days_left: i64,
    /// Masked — its source folder is sealed and not unlocked this session. Restore is refused.
    pub locked: bool,
    /// One-line, CONTENT-FREE summary for the row ("42 segments · 1 note"). Empty when masked.
    pub detail: String,
}

/// The lock sentinel shown instead of a masked entry's title. Mirrors `get_meeting_detail`'s
/// masked DTO.
const LOCKED_LABEL: &str = "🔒 Locked";

// ── RETENTION ────────────────────────────────────────────────────────────────────────────────────

/// The LIVE retention window. Out-of-range or unparseable stored values fall back to the default
/// rather than erroring — a corrupt setting must never make the trash unreadable (or, worse, purge
/// on a nonsense schedule).
pub(crate) fn retention_days(state: &AppState) -> i64 {
    state
        .db
        .get_setting(TRASH_RETENTION_KEY)
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|d| (MIN_TRASH_RETENTION_DAYS..=MAX_TRASH_RETENTION_DAYS).contains(d))
        .unwrap_or(DEFAULT_TRASH_RETENTION_DAYS)
}

/// When an entry deleted at `deleted_at` expires. An unparseable timestamp yields `None`, and every
/// caller treats that as NEVER-EXPIRES — the purge must fail CLOSED (keep the content) rather than
/// destroy a row it could not date.
fn expiry_of(deleted_at: &str, days: i64) -> Option<chrono::DateTime<chrono::Utc>> {
    let parsed = chrono::DateTime::parse_from_rfc3339(deleted_at).ok()?;
    Some(parsed.with_timezone(&chrono::Utc) + chrono::Duration::days(days))
}

// ── CAPTURE (called by the delete paths, BEFORE their destructive cascade) ───────────────────────

/// Snapshot a meeting into the trash and PROVE the snapshot restores it. Returns the new trash entry
/// id.
///
/// The caller (`delete_meeting_inner_notifying`) has already taken the lifecycle guard and proven the
/// meeting is unlocked; it must NOT remove the audio files when this succeeded — they are the
/// meeting's only copy and the snapshot references them by path.
pub(crate) fn capture_meeting(state: &AppState, meeting_id: &str) -> Result<String, AppError> {
    let meeting = match state.db.get_meeting(meeting_id)? {
        Some(m) => m,
        None => return Err(AppError::InvalidArg(format!("no meeting {meeting_id}"))),
    };
    let segments = state.db.get_segments(meeting_id)?;
    let notes = state.db.note_records_for_meeting(meeting_id)?;
    let (mic_master, sys_master) = state
        .db
        .get_meeting_master_paths(meeting_id)
        .unwrap_or((None, None));

    let snapshot = MeetingSnapshot {
        version: SNAPSHOT_VERSION,
        id: meeting.id.clone(),
        started_at: meeting.started_at.clone(),
        ended_at: meeting.ended_at.clone(),
        title: meeting.title.clone(),
        duration_s: meeting.duration_s,
        audio_path: meeting.audio_path.clone(),
        status: meeting.status.as_str().to_string(),
        folder_id: meeting.folder_id.clone(),
        segments: segments
            .iter()
            .map(|s| SegmentSnapshot {
                idx: s.idx,
                start_s: s.start_s,
                end_s: s.end_s,
                text: s.text.clone(),
                speaker: s.speaker.clone(),
                confidence: s.confidence,
            })
            .collect(),
        notes: notes
            .iter()
            .map(|n| NoteRecordSnapshot {
                provider_id: n.provider_id.clone(),
                markdown: n.markdown.clone(),
                created_at: n.created_at.clone(),
                exported_path: n.exported_path.clone(),
            })
            .collect(),
        timeline: state.db.get_timeline_data(meeting_id)?,
        manual_notes: state.db.get_manual_notes(meeting_id).unwrap_or_default(),
        tags: state.db.get_meeting_tags(meeting_id).unwrap_or_default(),
        mic_master_path: mic_master,
        sys_master_path: sys_master,
        attachments: state
            .db
            .attachments_for_meeting(meeting_id)?
            .iter()
            .map(attachment_to_snapshot)
            .collect(),
    };

    let label = meeting
        .title
        .clone()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| "Untitled recording".to_string());

    store_and_verify(
        state,
        TrashKind::Meeting,
        meeting_id,
        meeting.folder_id.as_deref(),
        &label,
        &snapshot,
        |restored: &MeetingSnapshot| {
            // The content that is about to be destroyed must be present in the copy we just read
            // back from SQLCipher — not merely in the struct we serialized from memory.
            if restored.segments.len() != snapshot.segments.len() {
                return Some(format!(
                    "transcript segments did not survive the snapshot ({} of {})",
                    restored.segments.len(),
                    snapshot.segments.len()
                ));
            }
            if restored.notes.len() != snapshot.notes.len() {
                return Some(format!(
                    "generated notes did not survive the snapshot ({} of {})",
                    restored.notes.len(),
                    snapshot.notes.len()
                ));
            }
            for (a, b) in restored.notes.iter().zip(snapshot.notes.iter()) {
                if a.markdown != b.markdown {
                    return Some("a note's markdown did not round-trip".to_string());
                }
            }
            for (a, b) in restored.segments.iter().zip(snapshot.segments.iter()) {
                if a.text != b.text {
                    return Some("a transcript segment's text did not round-trip".to_string());
                }
            }
            if restored.manual_notes != snapshot.manual_notes {
                return Some("manual notes did not round-trip".to_string());
            }
            if restored.attachments.len() != snapshot.attachments.len() {
                return Some(format!(
                    "inline images did not survive the snapshot ({} of {})",
                    restored.attachments.len(),
                    snapshot.attachments.len()
                ));
            }
            for (a, b) in restored.attachments.iter().zip(snapshot.attachments.iter()) {
                if a.data_hex != b.data_hex {
                    return Some("an inline image's bytes did not round-trip".to_string());
                }
            }
            if restored.timeline != snapshot.timeline {
                return Some("the speaker timeline did not round-trip".to_string());
            }
            None
        },
    )
}

/// Snapshot an authored note into the trash and PROVE the snapshot restores it.
///
/// The caller (`delete_note_inner_notifying`) has already gated the folder. The exported vault `.md`
/// IS still removed by the caller: the markdown lives in SQLCipher (canonical), so restore
/// re-exports it — leaving a plaintext `.md` for a "deleted" note in the user's Obsidian vault would
/// contradict the delete they asked for, and for a locked folder it would be a leak.
pub(crate) fn capture_note(state: &AppState, note_id: &str) -> Result<String, AppError> {
    let row = match state.db.get_note_row(note_id)? {
        Some(r) => r,
        None => return Err(AppError::InvalidArg(format!("no note {note_id}"))),
    };
    let snapshot = NoteSnapshot {
        version: SNAPSHOT_VERSION,
        id: row.id.clone(),
        folder_id: row.folder_id.clone(),
        name: row.name.clone(),
        title: row.title.clone(),
        text: row.text.clone(),
        created_at: row.created_at,
        updated_at: row.updated_at,
        exported_path: row.exported_path.clone(),
        attachments: state
            .db
            .list_attachments(&crate::storage::AttachmentOwner::Document {
                document_id: note_id.to_string(),
            })?
            .iter()
            .map(attachment_to_snapshot)
            .collect(),
    };
    let label = row
        .title
        .clone()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| row.name.clone());

    store_and_verify(
        state,
        TrashKind::Note,
        note_id,
        Some(&row.folder_id),
        &label,
        &snapshot,
        |restored: &NoteSnapshot| {
            if restored.text != snapshot.text {
                return Some("the note's body did not round-trip".to_string());
            }
            if restored.attachments.len() != snapshot.attachments.len() {
                return Some("inline images did not survive the snapshot".to_string());
            }
            for (a, b) in restored.attachments.iter().zip(snapshot.attachments.iter()) {
                if a.data_hex != b.data_hex {
                    return Some("an inline image's bytes did not round-trip".to_string());
                }
            }
            None
        },
    )
}

/// Snapshot a container into the trash and PROVE the snapshot restores it.
///
/// Called by `delete_folder_inner` AFTER it has resolved the member ids but BEFORE it rehomes them —
/// the ids are what makes the restore able to put the contents back, so they must be captured while
/// the folder still governs them.
pub(crate) fn capture_folder(
    state: &AppState,
    folder: &crate::storage::Folder,
    kind: &str,
    meeting_ids: &[String],
    note_ids: &[String],
) -> Result<String, AppError> {
    let trash_kind = if kind == "note" {
        TrashKind::NoteFolder
    } else {
        TrashKind::Folder
    };
    let snapshot = FolderSnapshot {
        version: SNAPSHOT_VERSION,
        id: folder.id.clone(),
        name: folder.name.clone(),
        path: folder.path.clone(),
        parent_id: folder.parent_id.clone(),
        created_at: folder.created_at.clone(),
        kind: kind.to_string(),
        meeting_ids: meeting_ids.to_vec(),
        note_ids: note_ids.to_vec(),
        was_locked: folder.locked,
        presentation: state.db.folder_presentation(&folder.id)?,
    };
    let label = folder.name.clone();

    // A container snapshot is anchored to its PARENT, not to itself: the folder row is about to be
    // deleted, so anchoring to its own id would leave the entry pointing at a row that no longer
    // exists — and a sealed entry whose anchor is gone can never be unlocked again.
    let anchor = folder.parent_id.clone();

    store_and_verify(
        state,
        trash_kind,
        &folder.id,
        anchor.as_deref(),
        &label,
        &snapshot,
        |restored: &FolderSnapshot| {
            if restored.meeting_ids.len() != snapshot.meeting_ids.len()
                || restored.note_ids.len() != snapshot.note_ids.len()
            {
                return Some("the folder's member list did not round-trip".to_string());
            }
            if restored.path != snapshot.path {
                return Some("the folder's vault path did not round-trip".to_string());
            }
            None
        },
    )
}

/// Hex-encode bytes for the snapshot payload. Dependency-free; see [`AttachmentSnapshot`] for why
/// hex rather than base64.
pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Decode a hex payload. `None` on ANY malformation (odd length, non-hex digit) — the caller then
/// SKIPS that attachment rather than restoring truncated bytes as if they were an image.
pub(crate) fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(s.len() / 2);
    for pair in bytes.chunks(2) {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out.push(((hi << 4) | lo) as u8);
    }
    Some(out)
}

fn sha256_of(bytes: &[u8]) -> [u8; 32] {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    hasher.update(bytes);
    hasher.finalize().into()
}

/// Snapshot one attachment record. Reads the PLAINTEXT `data` column: everything entering the trash
/// is unlocked by the delete gate, so `data` is the real bytes.
fn attachment_to_snapshot(a: &crate::storage::AttachmentRecord) -> AttachmentSnapshot {
    let provider_id = match &a.owner {
        crate::storage::AttachmentOwner::Meeting { provider_id, .. } => Some(provider_id.clone()),
        _ => None,
    };
    AttachmentSnapshot {
        id: a.id.clone(),
        provider_id,
        mime_type: a.mime_type.clone(),
        extension: a.extension.clone(),
        width: a.width,
        height: a.height,
        byte_len: a.byte_len,
        sha256: hex_encode(&a.sha256),
        data_hex: hex_encode(&a.data),
        exported_path: a.exported_path.clone(),
        created_at: a.created_at,
    }
}

/// Re-insert the snapshotted attachments for one owner, VERIFYING each one's SHA-256 first.
///
/// The digest check is the point: an attachment that does not hash to what the snapshot recorded is
/// corrupt, and silently restoring corrupt image bytes is worse than restoring none — so it is
/// SKIPPED with a warning while every intact sibling still lands. Best-effort overall, because the
/// note markdown (the thing the user actually asked to get back) is already restored by the time
/// this runs; a failure here degrades the images, never the text.
fn restore_attachments(
    state: &AppState,
    owner_for: impl Fn(&AttachmentSnapshot) -> Option<crate::storage::AttachmentOwner>,
    attachments: &[AttachmentSnapshot],
) {
    for a in attachments {
        let Some(owner) = owner_for(a) else {
            tracing::warn!(target: "trash", attachment_id = %a.id, "restored attachment has no resolvable owner (skipped)");
            continue;
        };
        let Some(bytes) = hex_decode(&a.data_hex) else {
            tracing::warn!(target: "trash", attachment_id = %a.id, "restored attachment did not decode (skipped)");
            continue;
        };
        let digest = sha256_of(&bytes);
        let hex = hex_encode(&digest);
        if hex != a.sha256 {
            tracing::warn!(
                target: "trash",
                attachment_id = %a.id,
                "restored attachment failed its digest check (skipped rather than restoring corrupt bytes)"
            );
            continue;
        }
        let new = crate::storage::NewAttachment {
            id: &a.id,
            owner: &owner,
            mime_type: &a.mime_type,
            extension: &a.extension,
            width: a.width,
            height: a.height,
            sha256: &digest,
            byte_len: bytes.len(),
            data: &bytes,
            data_blob: None,
            created_at: a.created_at,
        };
        if let Err(e) = state.db.insert_attachment(&new) {
            tracing::warn!(target: "trash", attachment_id = %a.id, error = %e, "could not re-insert attachment (note text unaffected)");
        }
    }
}

/// The shared capture core: serialize → insert → **read back and verify** → seal if the source
/// folder is sealed. Returns the trash entry id.
///
/// On ANY verification failure the just-written row is removed and the error is propagated, so the
/// caller ABORTS its delete with nothing mutated. That ordering is the whole point: the destructive
/// cascade only ever runs after a proven-good snapshot exists.
fn store_and_verify<T, F>(
    state: &AppState,
    kind: TrashKind,
    source_id: &str,
    source_folder_id: Option<&str>,
    label: &str,
    snapshot: &T,
    verify: F,
) -> Result<String, AppError>
where
    T: Serialize + for<'de> Deserialize<'de>,
    F: FnOnce(&T) -> Option<String>,
{
    let payload = serde_json::to_string(snapshot).map_err(|e| {
        AppError::Storage(format!(
            "could not snapshot this {} for the trash: {e}",
            kind.noun()
        ))
    })?;
    let entry_id = uuid::Uuid::new_v4().to_string();
    let deleted_at = chrono::Utc::now().to_rfc3339();

    // Is the SOURCE folder already sealed? If so this snapshot must be ciphertext from its first
    // instant — never inserted as plaintext and sealed afterwards, which would leave a crash window
    // with readable content behind a lock (the reasoning `Db::insert_sealed_folder` documents).
    let sealed_ck = match source_folder_id {
        Some(folder_id)
            if state
                .db
                .folder_by_id(folder_id)?
                .map(|f| f.locked)
                .unwrap_or(false) =>
        {
            // FAIL-CLOSED: no cached KEK ⇒ `AppError::Locked` here, BEFORE anything is written and
            // before the caller destroys anything. A delete we cannot make an undo for is refused.
            Some(session_folder_ck(state, folder_id)?)
        }
        _ => None,
    };

    match (&sealed_ck, source_folder_id) {
        (Some(ck), Some(folder_id)) => {
            let (label_blob, payload_blob) =
                sealed_trash_blobs(folder_id, &entry_id, label, &payload, ck)?;
            state.db.insert_trash_entry_sealed(
                &entry_id,
                kind,
                source_id,
                source_folder_id,
                &label_blob,
                &payload_blob,
                &deleted_at,
            )?;
        }
        _ => {
            state.db.insert_trash_entry(
                &entry_id,
                kind,
                source_id,
                source_folder_id,
                label,
                &payload,
                &deleted_at,
            )?;
        }
    }

    // VERIFY-BEFORE-DESTROY: re-read what SQLCipher actually stored and re-parse it — decrypting
    // first when the row went in sealed. Checking the in-memory struct would prove nothing about the
    // row a later restore will actually read.
    let verified = || -> Result<(), AppError> {
        let stored = state.db.get_trash_entry(&entry_id)?.ok_or_else(|| {
            AppError::Storage("the trash snapshot disappeared immediately after writing".into())
        })?;
        let stored_payload = match (&sealed_ck, source_folder_id) {
            (Some(ck), Some(folder_id)) => {
                let blob = stored.payload_blob.as_deref().ok_or_else(|| {
                    AppError::Storage("the sealed trash snapshot stored no ciphertext".into())
                })?;
                let label_blob = stored.label_blob.as_deref().ok_or_else(|| {
                    AppError::Storage("the sealed trash label stored no ciphertext".into())
                })?;
                // Authenticate BOTH blobs against the real row, not just the payload: a label that
                // does not decrypt would render as an empty title after the next unlock.
                decrypt_utf8(
                    ck,
                    label_blob,
                    &aad_trash(folder_id, &entry_id, "label"),
                    "trash label",
                )?;
                decrypt_utf8(
                    ck,
                    blob,
                    &aad_trash(folder_id, &entry_id, "payload"),
                    "trash snapshot",
                )?
            }
            _ => stored.payload.clone(),
        };
        if stored_payload != payload {
            return Err(AppError::Storage(
                "the trash snapshot did not store byte-identically".into(),
            ));
        }
        let restored: T = serde_json::from_str(&stored_payload).map_err(|e| {
            AppError::Storage(format!("the trash snapshot did not parse back: {e}"))
        })?;
        if let Some(reason) = verify(&restored) {
            return Err(AppError::Storage(format!(
                "refusing to delete this {} — {reason}",
                kind.noun()
            )));
        }
        Ok(())
    }();

    if let Err(e) = verified {
        // Leave nothing half-written. The caller aborts, so the content is still intact.
        let _ = state.db.delete_trash_entry(&entry_id);
        return Err(e);
    }

    tracing::info!(
        target: "trash",
        entry_id = %entry_id,
        kind = kind.as_str(),
        "captured to trash"
    );
    Ok(entry_id)
}

// ── SEAL LIFECYCLE (the four phases, mirroring the document blob) ────────────────────────────────

/// Encrypt ONE entry's label + payload under `ck`, VERIFY both decrypt back byte-identical, and only
/// then blank the plaintext columns (verify-before-destroy).
fn seal_one_trash_entry(
    db: &Db,
    folder_id: &str,
    entry_id: &str,
    label: &str,
    payload: &str,
    ck: &[u8; 32],
) -> Result<(), AppError> {
    let (label_blob, payload_blob) = sealed_trash_blobs(folder_id, entry_id, label, payload, ck)?;
    db.seal_trash_entry(entry_id, &label_blob, &payload_blob)?;
    Ok(())
}

/// Encrypt an entry's label + payload and PROVE both decrypt back byte-identical, returning the
/// blobs. The shared verify step of both seal paths — the birth-seal in [`store_and_verify`] (which
/// inserts the blobs directly) and [`seal_one_trash_entry`] (which replaces a plaintext row).
fn sealed_trash_blobs(
    folder_id: &str,
    entry_id: &str,
    label: &str,
    payload: &str,
    ck: &[u8; 32],
) -> Result<(Vec<u8>, Vec<u8>), AppError> {
    let label_aad = aad_trash(folder_id, entry_id, "label");
    let payload_aad = aad_trash(folder_id, entry_id, "payload");
    let label_blob = crate::crypto::encrypt(ck, label.as_bytes(), &label_aad)?;
    let payload_blob = crate::crypto::encrypt(ck, payload.as_bytes(), &payload_aad)?;
    if crate::crypto::decrypt(ck, &label_blob, &label_aad)? != label.as_bytes() {
        return Err(AppError::Storage(
            "trash label seal verification failed (blob mismatch)".into(),
        ));
    }
    if crate::crypto::decrypt(ck, &payload_blob, &payload_aad)? != payload.as_bytes() {
        return Err(AppError::Storage(
            "trash snapshot seal verification failed (blob mismatch)".into(),
        ));
    }
    Ok((label_blob, payload_blob))
}

/// AAD for a trashed meeting's audio `.enc`. Bound to the TRASH ENTRY (not the meeting id) because
/// the meeting row is gone: the entry is the only surviving identity, and binding it keeps one
/// trashed recording's audio from being swapped onto another entry and decrypted there.
fn aad_trash_audio(folder_id: &str, entry_id: &str, role: &str) -> Vec<u8> {
    aad_trash(folder_id, entry_id, &format!("audio:{role}"))
}

/// Seal the on-disk audio of one trashed MEETING and rewrite the snapshot's recorded paths to the
/// `.enc` forms. Returns the updated payload JSON (unchanged when there was nothing to seal).
///
/// # Why this exists
///
/// `lock_folder` seals a folder's audio by walking `meeting_ids_in_folder`. A trashed meeting has NO
/// row, so it is invisible to that walk — which means without this, deleting a recording from an
/// OPEN folder and locking that folder afterwards would leave the recording's WAV sitting in the
/// audio directory in PLAINTEXT while the app tells the user the folder is sealed. That is a
/// regression in an EXISTING lock invariant caused by the trash, not a new seal path, so it is the
/// trash's job to close it.
///
/// `crypto::encrypt_file` carries verify-before-destroy internally (it re-reads the written `.enc`
/// and decrypts it before returning), so the plaintext is only unlinked after a proven-good
/// ciphertext exists on disk.
fn seal_trash_meeting_audio(
    folder_id: &str,
    entry_id: &str,
    payload: &str,
    ck: &[u8; 32],
) -> Result<String, AppError> {
    let mut snapshot: MeetingSnapshot = match serde_json::from_str(payload) {
        Ok(s) => s,
        // Not a meeting snapshot (a note/folder entry) — nothing to do.
        Err(_) => return Ok(payload.to_string()),
    };
    let mut changed = false;
    for (role, slot) in [
        ("playback", &mut snapshot.audio_path),
        ("mic", &mut snapshot.mic_master_path),
        ("sys", &mut snapshot.sys_master_path),
    ] {
        let Some(path) = slot.clone() else { continue };
        if path.ends_with(ENC_SUFFIX) {
            continue; // already sealed (idempotent repair).
        }
        let src = std::path::Path::new(&path);
        if !src.exists() {
            continue; // nothing on disk — the snapshot keeps the recorded path as-is.
        }
        let enc = format!("{path}{ENC_SUFFIX}");
        crate::crypto::encrypt_file(
            ck,
            src,
            std::path::Path::new(&enc),
            &aad_trash_audio(folder_id, entry_id, role),
        )?;
        // Only now is the plaintext disposable: `encrypt_file` has already proven the `.enc` on disk
        // decrypts back byte-identical.
        crate::crypto::remove_file_verified_absent(src, "remove trashed audio plaintext after seal")?;
        *slot = Some(enc);
        changed = true;
    }
    if !changed {
        return Ok(payload.to_string());
    }
    serde_json::to_string(&snapshot)
        .map_err(|e| AppError::Storage(format!("could not re-encode the trash snapshot: {e}")))
}

/// Decrypt a trashed meeting's `.enc` audio back to plaintext and rewrite the snapshot's paths.
/// `retain_ciphertext` keeps the `.enc` (session unseal — the folder is still locked on disk) or
/// drops it (permanent unseal via `remove_lock`).
fn unseal_trash_meeting_audio(
    folder_id: &str,
    entry_id: &str,
    payload: &str,
    ck: &[u8; 32],
    retain_ciphertext: bool,
) -> Result<String, AppError> {
    let mut snapshot: MeetingSnapshot = match serde_json::from_str(payload) {
        Ok(s) => s,
        Err(_) => return Ok(payload.to_string()),
    };
    let mut changed = false;
    for (role, slot) in [
        ("playback", &mut snapshot.audio_path),
        ("mic", &mut snapshot.mic_master_path),
        ("sys", &mut snapshot.sys_master_path),
    ] {
        let Some(enc_path) = slot.clone() else { continue };
        if !enc_path.ends_with(ENC_SUFFIX) {
            continue; // already plaintext.
        }
        let enc = std::path::Path::new(&enc_path);
        if !enc.exists() {
            continue;
        }
        let plain = enc_path.trim_end_matches(ENC_SUFFIX).to_string();
        crate::crypto::decrypt_file(
            ck,
            enc,
            std::path::Path::new(&plain),
            &aad_trash_audio(folder_id, entry_id, role),
        )?;
        if !retain_ciphertext {
            // Plaintext is durably written above; only then is the ciphertext disposable.
            let _ = std::fs::remove_file(enc);
            *slot = Some(plain);
            changed = true;
        }
        // SESSION unseal: the plaintext WAV now exists for playback/restore, but the snapshot keeps
        // pointing at the `.enc` so a relock has something to re-blank to without re-encrypting.
    }
    if !changed {
        return Ok(payload.to_string());
    }
    serde_json::to_string(&snapshot)
        .map_err(|e| AppError::Storage(format!("could not re-encode the trash snapshot: {e}")))
}

/// Seal every trash entry anchored to this folder. Called by `seal_folder_extras` — the case where a
/// snapshot was captured while the folder was OPEN and the user locks the folder afterwards.
///
/// Idempotent and repair-safe, exactly like the document loop it mirrors: an already-sealed entry is
/// AEAD-VERIFIED rather than skipped, because observing a non-NULL blob is not proof the only
/// surviving copy is decryptable.
pub(crate) fn seal_trash_in_folder(db: &Db, folder_id: &str, ck: &[u8; 32]) -> Result<(), AppError> {
    for entry in db.raw_trash_entries_in_folder(folder_id)? {
        if entry.payload.is_empty() {
            if let (Some(label_blob), Some(payload_blob)) = (&entry.label_blob, &entry.payload_blob)
            {
                crate::crypto::decrypt(ck, label_blob, &aad_trash(folder_id, &entry.id, "label"))?;
                crate::crypto::decrypt(
                    ck,
                    payload_blob,
                    &aad_trash(folder_id, &entry.id, "payload"),
                )?;
            }
            continue;
        }
        // ORDER MATTERS: seal the AUDIO first, because doing so rewrites the recorded paths to their
        // `.enc` forms — and the payload we then encrypt has to be the one carrying those paths. Seal
        // the payload first and the ciphertext would preserve the old plaintext pathnames, so an
        // unseal would look for a WAV that no longer exists.
        let payload = seal_trash_meeting_audio(folder_id, &entry.id, &entry.payload, ck)?;
        seal_one_trash_entry(db, folder_id, &entry.id, &entry.label, &payload, ck)?;
    }
    Ok(())
}

/// Decrypt this folder's trash entries back into their plaintext columns FOR THE SESSION, leaving
/// the blobs intact. Called by `unseal_folder_extras` on a session unlock.
pub(crate) fn unseal_trash_in_folder(
    db: &Db,
    folder_id: &str,
    ck: &[u8; 32],
) -> Result<(), AppError> {
    for entry in db.raw_trash_entries_in_folder(folder_id)? {
        let (Some(label_blob), Some(payload_blob)) = (&entry.label_blob, &entry.payload_blob) else {
            continue; // never sealed (captured while open, folder locked after) — nothing to do.
        };
        let label = decrypt_utf8(
            ck,
            label_blob,
            &aad_trash(folder_id, &entry.id, "label"),
            "trash label",
        )?;
        let payload = decrypt_utf8(
            ck,
            payload_blob,
            &aad_trash(folder_id, &entry.id, "payload"),
            "trash snapshot",
        )?;
        // Materialize the audio for the session (the `.enc` is KEPT — the folder is still locked on
        // disk), so a restore during this session gets a playable WAV. Best-effort: a failure here
        // must not abort the folder unlock, and the snapshot's transcript/notes are already restored.
        if let Err(e) =
            unseal_trash_meeting_audio(folder_id, &entry.id, &payload, ck, true)
        {
            tracing::warn!(target: "trash", entry_id = %entry.id, error = %e, "trashed audio session-unseal failed (snapshot content unaffected)");
        }
        db.set_trash_entry_plaintext(&entry.id, &label, &payload)?;
    }
    Ok(())
}

/// Re-blank this folder's trash plaintext on relock, keeping the blobs. Called by
/// `reblank_folder_extras`.
///
/// Guards on `payload_blob IS NOT NULL` (via [`RawTrashEntry::is_sealed`]) so it can never blank the
/// ONLY copy of an entry that was never sealed — the same guard the note/document reblank uses.
pub(crate) fn reblank_trash_in_folder(db: &Db, folder_id: &str) -> Result<(), AppError> {
    for entry in db.raw_trash_entries_in_folder(folder_id)? {
        if entry.is_sealed() && entry.label_blob.is_some() {
            // Drop the session-materialized plaintext WAV before blanking the payload — after the
            // blank we can no longer read which paths to remove. The `.enc` stays, so nothing is
            // lost; this only removes the decrypted copy the unlock made.
            remove_session_trash_audio(&entry.payload);
            db.set_trash_entry_plaintext(&entry.id, "", "")?;
        }
    }
    Ok(())
}

/// Remove the session-materialized plaintext WAVs a session-unseal created, leaving every `.enc`.
/// Best-effort and content-free: the ciphertext is the durable copy, so a failed unlink is disk
/// residue, never loss. Mirrors `relock_folder` dropping the decrypted session WAV.
fn remove_session_trash_audio(payload: &str) {
    let Ok(snapshot) = serde_json::from_str::<MeetingSnapshot>(payload) else {
        return; // not a meeting entry.
    };
    for slot in [
        &snapshot.audio_path,
        &snapshot.mic_master_path,
        &snapshot.sys_master_path,
    ] {
        let Some(path) = slot else { continue };
        // Only ever the PLAINTEXT twin of a recorded `.enc` — never the `.enc` itself.
        if let Some(plain) = path.strip_suffix(ENC_SUFFIX) {
            let _ = std::fs::remove_file(plain);
        }
    }
}

/// PERMANENTLY unseal this folder's trash entries — decrypt to plaintext AND drop the ciphertext.
/// Called by `unseal_folder_extras_permanent` (`remove_lock`), which is also the path
/// `delete_folder_inner` takes for a sealed folder.
pub(crate) fn unseal_trash_in_folder_permanent(
    db: &Db,
    folder_id: &str,
    ck: &[u8; 32],
) -> Result<(), AppError> {
    for entry in db.raw_trash_entries_in_folder(folder_id)? {
        if let (Some(label_blob), Some(payload_blob)) = (&entry.label_blob, &entry.payload_blob) {
            let label = decrypt_utf8(
                ck,
                label_blob,
                &aad_trash(folder_id, &entry.id, "label"),
                "trash label",
            )?;
            let payload = decrypt_utf8(
                ck,
                payload_blob,
                &aad_trash(folder_id, &entry.id, "payload"),
                "trash snapshot",
            )?;
            // The lock is going away for good, so the audio must come back to plaintext AND its
            // `.enc` must go — otherwise the entry keeps pointing at ciphertext whose key is about
            // to be destroyed, which is unrecoverable audio loss. This rewrites the recorded paths,
            // so persist THAT payload, not the one we decrypted.
            let payload = match unseal_trash_meeting_audio(folder_id, &entry.id, &payload, ck, false)
            {
                Ok(updated) => updated,
                Err(e) => {
                    // FAIL LOUD: silently keeping the `.enc` here would strand the audio behind a
                    // key that `remove_lock` is about to discard.
                    return Err(AppError::Storage(format!(
                        "could not permanently unseal a trashed recording's audio: {e}"
                    )));
                }
            };
            // Plaintext FIRST, ciphertext dropped only after it is durably readable — never the
            // reverse (that ordering is how a crash between the two loses the entry).
            db.set_trash_entry_plaintext(&entry.id, &label, &payload)?;
            db.clear_trash_entry_blobs(&entry.id)?;
        }
    }
    Ok(())
}

fn decrypt_utf8(
    ck: &[u8; 32],
    blob: &[u8],
    aad: &[u8],
    what: &str,
) -> Result<String, AppError> {
    let bytes = crate::crypto::decrypt(ck, blob, aad)?;
    String::from_utf8(bytes)
        .map_err(|_| AppError::Storage(format!("{what} did not decrypt to valid UTF-8")))
}

// ── READ ─────────────────────────────────────────────────────────────────────────────────────────

/// Is this entry readable right now? An entry whose source folder is sealed-and-not-session-unlocked
/// is MASKED and cannot be restored.
///
/// An entry with NO anchor (`source_folder_id: None` — a vault-root meeting, or a top-level
/// container) is governed by no folder, so it is readable. An entry whose anchor folder row has
/// VANISHED is also readable — but only when it is not sealed; a sealed entry with a missing anchor
/// has no reachable key, so it fails closed (see [`entry_is_readable`]).
fn entry_is_readable(state: &AppState, entry: &RawTrashEntry) -> Result<bool, AppError> {
    let Some(folder_id) = entry.source_folder_id.as_deref() else {
        return Ok(true);
    };
    match state.db.folder_by_id(folder_id)? {
        Some(folder) => {
            if !folder.locked {
                return Ok(true);
            }
            let unlocked = state
                .unlocked_folders
                .lock()
                .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?;
            Ok(unlocked.contains(folder_id))
        }
        // Anchor folder is gone. A never-sealed entry is plain content and stays readable; a sealed
        // one has no key path left, so it must present as locked rather than as empty content.
        None => Ok(!entry.is_sealed()),
    }
}

/// Turn a stored row into the wire DTO, masking when unreadable.
fn to_dto(state: &AppState, entry: &RawTrashEntry, days: i64) -> Result<TrashEntry, AppError> {
    let readable = entry_is_readable(state, entry)?;
    // Belt AND braces: even a "readable" row is masked when its plaintext is blank while ciphertext
    // exists (an unlock that half-applied). Never surface an empty label as if it were the title.
    let masked = !readable || (entry.is_sealed() && entry.payload.is_empty());
    let expiry = expiry_of(&entry.deleted_at, days);
    let days_left = expiry
        .map(|e| (e - chrono::Utc::now()).num_days().max(0))
        .unwrap_or(days);
    Ok(TrashEntry {
        id: entry.id.clone(),
        kind: entry.kind.clone(),
        source_id: entry.source_id.clone(),
        source_folder_id: entry.source_folder_id.clone(),
        label: if masked {
            LOCKED_LABEL.to_string()
        } else {
            entry.label.clone()
        },
        deleted_at: entry.deleted_at.clone(),
        expires_at: expiry
            .map(|e| e.to_rfc3339())
            .unwrap_or_else(|| entry.deleted_at.clone()),
        days_left,
        locked: masked,
        detail: if masked {
            String::new()
        } else {
            content_free_detail(entry)
        },
    })
}

/// A CONTENT-FREE one-liner for the row: counts and kinds only, never transcript or note text.
fn content_free_detail(entry: &RawTrashEntry) -> String {
    match TrashKind::from_str(&entry.kind) {
        Some(TrashKind::Meeting) => match serde_json::from_str::<MeetingSnapshot>(&entry.payload) {
            Ok(s) => {
                let mut parts = Vec::new();
                if s.duration_s > 0 {
                    parts.push(format!("{} min", (s.duration_s + 59) / 60));
                }
                parts.push(format!(
                    "{} segment{}",
                    s.segments.len(),
                    if s.segments.len() == 1 { "" } else { "s" }
                ));
                if !s.notes.is_empty() {
                    parts.push(format!(
                        "{} note{}",
                        s.notes.len(),
                        if s.notes.len() == 1 { "" } else { "s" }
                    ));
                }
                if s.audio_path.is_some() {
                    parts.push("audio".to_string());
                }
                parts.join(" · ")
            }
            Err(_) => "Recording".to_string(),
        },
        Some(TrashKind::Note) => "Note".to_string(),
        Some(TrashKind::Folder) | Some(TrashKind::NoteFolder) => {
            match serde_json::from_str::<FolderSnapshot>(&entry.payload) {
                Ok(s) => {
                    let n = s.meeting_ids.len() + s.note_ids.len();
                    if n == 0 {
                        "Empty folder".to_string()
                    } else {
                        format!("{n} item{}", if n == 1 { "" } else { "s" })
                    }
                }
                Err(_) => "Folder".to_string(),
            }
        }
        None => String::new(),
    }
}

// ── COMMANDS ─────────────────────────────────────────────────────────────────────────────────────

/// Everything in the trash, newest deletion first. Sealed-and-not-unlocked entries come back MASKED
/// (`locked: true`, `label: "🔒 Locked"`, empty `detail`) — the same contract as
/// `get_meeting_detail`.
#[tauri::command]
pub fn list_trash(state: State<'_, AppState>) -> Result<Vec<TrashEntry>, AppError> {
    list_trash_inner(state.inner())
}

/// Inner of [`list_trash`] taking `&AppState`, so the masking oracle drives the REAL gate rather
/// than a reimplementation of it.
pub(crate) fn list_trash_inner(state: &AppState) -> Result<Vec<TrashEntry>, AppError> {
    let days = retention_days(state);
    let mut out = Vec::new();
    for entry in state.db.list_trash_entries()? {
        out.push(to_dto(state, &entry, days)?);
    }
    Ok(out)
}

/// How many entries are in the trash — the sidebar badge. Cheap; no payloads read.
#[tauri::command]
pub fn count_trash(state: State<'_, AppState>) -> Result<i64, AppError> {
    state.inner().db.count_trash_entries()
}

/// The live retention window in days.
#[tauri::command]
pub fn get_trash_retention_days(state: State<'_, AppState>) -> Result<i64, AppError> {
    Ok(retention_days(state.inner()))
}

/// Set the retention window. Applies to entries ALREADY in the trash (expiry is computed from the
/// live setting), so shortening it can make items purge on the next tick — which is why the FE
/// confirms the change.
#[tauri::command]
pub fn set_trash_retention_days(state: State<'_, AppState>, days: i64) -> Result<(), AppError> {
    if !(MIN_TRASH_RETENTION_DAYS..=MAX_TRASH_RETENTION_DAYS).contains(&days) {
        return Err(AppError::InvalidArg(format!(
            "retention must be between {MIN_TRASH_RETENTION_DAYS} and {MAX_TRASH_RETENTION_DAYS} days"
        )));
    }
    state
        .inner()
        .db
        .set_setting(TRASH_RETENTION_KEY, &days.to_string())?;
    Ok(())
}

/// Restore one entry: put the content back and drop the trash row.
///
/// REFUSED with `AppError::Locked` when the entry is masked — restoring would have to write the
/// snapshot's plaintext into rows the lock is currently protecting.
#[tauri::command]
pub async fn restore_trash_item(
    app: AppHandle,
    state: State<'_, AppState>,
    entry_id: String,
) -> Result<(), AppError> {
    let st = state.inner();
    restore_trash_item_inner(st, &entry_id).await?;
    emit_trash_updated(&app, st);
    Ok(())
}

/// Inner of [`restore_trash_item`] taking `&AppState` — the FULL body (gate + lifecycle guard +
/// per-kind restore + entry consumption), so the round-trip oracles bind the COMMAND behavior and
/// not just the per-kind helpers.
pub(crate) async fn restore_trash_item_inner(
    st: &AppState,
    entry_id: &str,
) -> Result<(), AppError> {
    let entry_id = entry_id.to_string();
    let entry = match st.db.get_trash_entry(&entry_id)? {
        Some(e) => e,
        None => return Ok(()), // already restored/purged elsewhere — idempotent.
    };
    if !entry_is_readable(st, &entry)? || entry.payload.is_empty() {
        return Err(AppError::Locked(
            "unlock this item's folder before restoring it from the trash".into(),
        ));
    }
    let kind = TrashKind::from_str(&entry.kind)
        .ok_or_else(|| AppError::Storage(format!("unknown trash kind {}", entry.kind)))?;

    // GUARD SCOPING is per-kind, and deliberately so — the lifecycle mutex is a NON-REENTRANT std
    // Mutex, so who holds it decides which helpers are callable:
    //   * meeting/note restore run UNDER it (serialized against a concurrent lock/relock landing
    //     mid-restore) and therefore must use the `_under_lifecycle_authorized` export seam;
    //   * folder restore must NOT hold it, because it re-files members through
    //     `move_note_doc_inner`, which takes the guard itself for its own double-gate.
    // Holding it across the whole match, as this first did, self-deadlocked both ways.
    match kind {
        TrashKind::Meeting => {
            let _lifecycle = lifecycle_guard(st);
            restore_meeting(st, &entry.payload)?;
        }
        TrashKind::Note => {
            let _lifecycle = lifecycle_guard(st);
            restore_note(st, &entry.payload)?;
        }
        TrashKind::Folder | TrashKind::NoteFolder => restore_folder(st, &entry.payload)?,
    }

    st.db.delete_trash_entry(&entry_id)?;
    bump_seal_epoch(st);
    tracing::info!(target: "trash", entry_id = %entry_id, kind = kind.as_str(), "restored from trash");
    Ok(())
}

/// Permanently destroy one entry — the trash row AND the content it was holding for restore. This is
/// the delete the original `delete_*` command used to do inline.
#[tauri::command]
pub async fn delete_trash_item_forever(
    app: AppHandle,
    state: State<'_, AppState>,
    entry_id: String,
) -> Result<(), AppError> {
    let st = state.inner();
    purge_one(st, &entry_id, Some(&app)).await?;
    emit_trash_updated(&app, st);
    Ok(())
}

/// Permanently destroy EVERY entry. Entries that are masked (sealed source folder) are LEFT BEHIND
/// and reported, not force-destroyed: purging one means decrypting its payload to find the files it
/// owns, which the lock forbids. Returns how many were purged.
#[tauri::command]
pub async fn empty_trash(app: AppHandle, state: State<'_, AppState>) -> Result<i64, AppError> {
    let st = state.inner();
    let mut purged = 0i64;
    let mut skipped = 0i64;
    for entry in st.db.list_trash_entries()? {
        if !entry_is_readable(st, &entry)? || entry.payload.is_empty() {
            skipped += 1;
            continue;
        }
        match purge_one(st, &entry.id, Some(&app)).await {
            Ok(()) => purged += 1,
            Err(e) => {
                tracing::warn!(target: "trash", entry_id = %entry.id, error = %e, "purge failed during empty-trash");
                skipped += 1;
            }
        }
    }
    if skipped > 0 {
        tracing::info!(target: "trash", purged, skipped, "empty-trash left locked entries behind");
    }
    emit_trash_updated(&app, st);
    Ok(purged)
}

/// Purge every EXPIRED entry now. Called by the background tick and available to the FE so the
/// Trash view can reconcile on open instead of waiting up to an hour.
#[tauri::command]
pub async fn purge_expired_trash(app: AppHandle, state: State<'_, AppState>) -> Result<i64, AppError> {
    let st = state.inner();
    let purged = purge_expired(st, Some(&app)).await?;
    if purged > 0 {
        emit_trash_updated(&app, st);
    }
    Ok(purged)
}

// ── PURGE ────────────────────────────────────────────────────────────────────────────────────────

/// Purge every entry past its retention. Shared by the command and the background tick.
///
/// Fails SOFT per entry (warn + continue): one undeletable file must not wedge the whole purge, and
/// a masked entry is SKIPPED — it is left for the user to handle after unlocking, because purging it
/// would require reading a payload the lock is protecting. That means a locked folder can hold
/// expired entries past their date; keeping the lock's promise is worth more than the schedule.
pub async fn purge_expired(
    state: &AppState,
    app: Option<&AppHandle>,
) -> Result<i64, AppError> {
    let days = retention_days(state);
    let now = chrono::Utc::now();
    let mut purged = 0i64;
    for entry in state.db.list_trash_entries()? {
        // An undateable `deleted_at` yields None ⇒ never expires. Fail closed: keep the content.
        let Some(expiry) = expiry_of(&entry.deleted_at, days) else {
            tracing::warn!(target: "trash", entry_id = %entry.id, "unparseable deleted_at — never purging");
            continue;
        };
        if expiry > now {
            continue;
        }
        if !entry_is_readable(state, &entry)? || entry.payload.is_empty() {
            continue; // sealed — the lock outranks the schedule.
        }
        match purge_one(state, &entry.id, app).await {
            Ok(()) => purged += 1,
            Err(e) => {
                tracing::warn!(target: "trash", entry_id = %entry.id, error = %e, "expired purge failed");
            }
        }
    }
    if purged > 0 {
        tracing::info!(target: "trash", purged, retention_days = days, "purged expired trash");
    }
    Ok(purged)
}

/// TEST-ONLY alias for [`purge_one`] so the audio-lifetime oracle can drive the real purge.
#[cfg(test)]
pub(crate) async fn purge_one_for_test(state: &AppState, entry_id: &str) -> Result<(), AppError> {
    purge_one(state, entry_id, None).await
}

/// Destroy ONE entry's content and its row. The entry must already be proven readable.
async fn purge_one(
    state: &AppState,
    entry_id: &str,
    app: Option<&AppHandle>,
) -> Result<(), AppError> {
    let Some(entry) = state.db.get_trash_entry(entry_id)? else {
        return Ok(());
    };
    let kind = TrashKind::from_str(&entry.kind)
        .ok_or_else(|| AppError::Storage(format!("unknown trash kind {}", entry.kind)))?;
    match kind {
        TrashKind::Meeting => {
            // The meeting's rows are already gone; what survives is its audio on disk. Remove every
            // on-disk form (plaintext WAV, `.enc` twin, masters) exactly as the pre-trash
            // `delete_meeting` did.
            if let Ok(s) = serde_json::from_str::<MeetingSnapshot>(&entry.payload) {
                remove_meeting_audio_files(s.audio_path.as_deref());
                remove_meeting_audio_files(s.mic_master_path.as_deref());
                remove_meeting_audio_files(s.sys_master_path.as_deref());
            }
        }
        TrashKind::Note => {
            // The document row and its vault `.md` are already gone (the delete removed both).
        }
        TrashKind::Folder | TrashKind::NoteFolder => {
            // The folder row is already gone and its contents were rehomed, never destroyed. What
            // remains is any member snapshot still anchored to this now-unrestorable folder: it must
            // be re-anchored to the folder's PARENT so it does not keep a dangling anchor.
            if let Ok(s) = serde_json::from_str::<FolderSnapshot>(&entry.payload) {
                let moved = state
                    .db
                    .reanchor_trash_entries(&s.id, s.parent_id.as_deref())?;
                if moved > 0 {
                    tracing::info!(target: "trash", folder_id = %s.id, moved, "re-anchored trash entries after folder purge");
                }
            }
        }
    }
    state.db.delete_trash_entry(entry_id)?;
    if let Some(app) = app {
        crate::events::emit_content_deleted(app, kind.as_str(), &entry.source_id);
    }
    tracing::info!(target: "trash", entry_id = %entry_id, kind = kind.as_str(), "purged permanently");
    Ok(())
}

// ── RESTORE ──────────────────────────────────────────────────────────────────────────────────────

/// Re-insert a meeting and everything the snapshot carried, then re-derive its indexes best-effort.
fn restore_meeting(state: &AppState, payload: &str) -> Result<(), AppError> {
    let s: MeetingSnapshot = parse_snapshot(payload)?;
    if state.db.get_meeting(&s.id)?.is_some() {
        return Err(AppError::InvalidArg(
            "a recording with this id already exists — nothing to restore".into(),
        ));
    }
    // The snapshot's folder may have been deleted meanwhile. Restore to the vault ROOT rather than
    // failing: getting the recording back matters more than getting its filing back, and an
    // unfiled recording is a state the app already handles everywhere.
    let folder_id = match s.folder_id.as_deref() {
        Some(fid) if state.db.folder_by_id(fid)?.is_some() => Some(fid.to_string()),
        Some(fid) => {
            tracing::info!(target: "trash", meeting_id = %s.id, missing_folder = %fid, "restoring to vault root — original folder is gone");
            None
        }
        None => None,
    };
    // Refuse to write plaintext into a folder that is sealed and not unlocked this session.
    if let Some(fid) = folder_id.as_deref() {
        if !folder_is_unlocked(state, fid)? {
            return Err(AppError::Locked(
                "unlock the destination folder before restoring this recording".into(),
            ));
        }
    }

    // The delete opened an org-source CLOSURE on this id, and the `closing_*_guard` triggers abort
    // every write to a closing source — including the ones this restore is about to make. Retire it
    // first: the id is coming back to life, which is the one case the closure was not written for.
    // Safe because the delete already revoked every live org share (revoke-before-delete), so no
    // server item survives for the sync tick to re-pull.
    state.db.clear_org_source_closure("meeting", &s.id)?;

    let meeting = crate::storage::Meeting {
        id: s.id.clone(),
        started_at: s.started_at.clone(),
        ended_at: s.ended_at.clone(),
        title: s.title.clone(),
        duration_s: s.duration_s,
        audio_path: s.audio_path.clone(),
        status: s.status.parse()?,
        folder_id: folder_id.clone(),
    };
    state.db.insert_meeting(&meeting)?;
    if let Some(fid) = folder_id.as_deref() {
        state.db.set_meeting_folder(&s.id, Some(fid))?;
    }

    let segments: Vec<crate::transcribe::types::Segment> = s
        .segments
        .iter()
        .map(|x| crate::transcribe::types::Segment {
            idx: x.idx,
            start_s: x.start_s,
            end_s: x.end_s,
            text: x.text.clone(),
            speaker: x.speaker.clone(),
            confidence: x.confidence,
        })
        .collect();
    if !segments.is_empty() {
        state.db.insert_segments(&s.id, &segments)?;
    }
    for n in &s.notes {
        state.db.upsert_note(&crate::storage::NoteRecord {
            meeting_id: s.id.clone(),
            provider_id: n.provider_id.clone(),
            markdown: n.markdown.clone(),
            created_at: n.created_at.clone(),
            exported_path: None, // re-established by the re-export below.
            ..Default::default()
        })?;
    }
    if let Some(timeline) = &s.timeline {
        state.db.set_timeline_data(&s.id, timeline)?;
    }
    if !s.manual_notes.is_empty() {
        state.db.set_manual_notes(&s.id, &s.manual_notes)?;
    }
    if !s.tags.is_empty() {
        state.db.set_meeting_tags(&s.id, &s.tags)?;
    }
    if let Some(p) = &s.mic_master_path {
        let _ = state.db.set_meeting_mic_master_path(&s.id, Some(p));
    }
    if let Some(p) = &s.sys_master_path {
        let _ = state.db.set_meeting_sys_master_path(&s.id, Some(p));
    }

    // Inline images, per provider note. These cascade-deleted with the `notes` rows, so the snapshot
    // is their only copy — restore them BEFORE the vault re-export, so the exported `.md` is written
    // against images that already exist.
    restore_attachments(
        state,
        |a| {
            a.provider_id
                .as_ref()
                .map(|provider_id| crate::storage::AttachmentOwner::Meeting {
                    meeting_id: s.id.clone(),
                    provider_id: provider_id.clone(),
                })
        },
        &s.attachments,
    );

    // Derived state: re-export the vault `.md` and re-derive chunks/vectors. Best-effort BY DESIGN —
    // the canonical rows are already back, so a failure here degrades search/Obsidian, never content.
    // NOT `export_note_to_vault`: that takes the lifecycle guard itself, and the caller
    // (`restore_trash_item_inner`) already holds it. The guard is a non-reentrant std Mutex, so
    // calling the guard-taking variant here SELF-DEADLOCKS — the authorized variant is exactly the
    // seam for a caller that already owns the mutex.
    if let Err(e) = export_note_to_vault_under_lifecycle_authorized(state, &s.id) {
        tracing::warn!(target: "trash", meeting_id = %s.id, error = %e, "re-export after restore failed");
    }
    let embedder = crate::embed::active_persistence_embedder_if_available();
    reindex_meeting_after_edit(state, &s.id, embedder.as_deref());
    Ok(())
}

/// Re-insert an authored note, then re-export + re-index best-effort.
fn restore_note(state: &AppState, payload: &str) -> Result<(), AppError> {
    let s: NoteSnapshot = parse_snapshot(payload)?;
    if state.db.get_note_row(&s.id)?.is_some() {
        return Err(AppError::InvalidArg(
            "a note with this id already exists — nothing to restore".into(),
        ));
    }
    // The note's folder may be gone; fall back to the notes root, which always exists.
    let folder_id = if state.db.folder_by_id(&s.folder_id)?.is_some() {
        s.folder_id.clone()
    } else {
        let root = state.db.ensure_notes_root()?;
        tracing::info!(target: "trash", note_id = %s.id, missing_folder = %s.folder_id, "restoring to notes root — original folder is gone");
        root
    };
    if !folder_is_unlocked(state, &folder_id)? {
        return Err(AppError::Locked(
            "unlock the destination folder before restoring this note".into(),
        ));
    }

    // Same closure retirement as the meeting restore — see the comment there.
    state.db.clear_org_source_closure("document", &s.id)?;

    let title = s.title.clone().unwrap_or_else(|| s.name.clone());
    state.db.insert_note(
        &s.id,
        &folder_id,
        &s.name,
        &title,
        &s.text,
        s.created_at,
    )?;
    // A restore into a SEALED (session-unlocked) folder must not leave plaintext at rest — re-seal
    // it through the same helper every authored-note write path uses.
    let now = chrono::Utc::now().timestamp_millis();
    reseal_document_if_locked(state, &folder_id, &s.id, &title, &s.text, now)?;

    // Inline images before the export, so the `.md` is written against images that exist.
    restore_attachments(
        state,
        |_| {
            Some(crate::storage::AttachmentOwner::Document {
                document_id: s.id.clone(),
            })
        },
        &s.attachments,
    );

    // The authorized variant — the caller already holds the non-reentrant lifecycle guard. See the
    // note in `restore_meeting`.
    if let Err(e) = export_note_to_vault_under_lifecycle_authorized(state, &s.id) {
        tracing::warn!(target: "trash", note_id = %s.id, error = %e, "re-export after restore failed");
    }
    // KIND-ROUTED so a note is chunked from its BODY (front-matter stripped), never the raw `text` —
    // the same seam every other note-index path goes through.
    let embedder = crate::embed::active_persistence_embedder_if_available();
    if let Err(e) = index_document_row_kind_routed(&state.db, &s.id, embedder.as_deref()) {
        tracing::warn!(target: "trash", note_id = %s.id, error = %e, "re-index after restore failed");
    }
    Ok(())
}

/// Recreate a container and move its recorded members back into it.
fn restore_folder(state: &AppState, payload: &str) -> Result<(), AppError> {
    let s: FolderSnapshot = parse_snapshot(payload)?;
    if state.db.folder_by_id(&s.id)?.is_some() {
        return Err(AppError::InvalidArg(
            "a folder with this id already exists — nothing to restore".into(),
        ));
    }
    // The parent may itself have been deleted. Restore at the ROOT rather than failing — the user
    // can move it back, and refusing would strand the folder in the trash forever.
    let parent_id = match s.parent_id.as_deref() {
        Some(pid) if state.db.folder_by_id(pid)?.is_some() => Some(pid.to_string()),
        Some(pid) => {
            tracing::info!(target: "trash", folder_id = %s.id, missing_parent = %pid, "restoring at root — original parent is gone");
            None
        }
        None => None,
    };
    // Restoring INTO a sealed parent would create an open container inside a sealed one — the exact
    // state the container-creation gate exists to prevent. Refuse and let the user unlock first.
    if let Some(pid) = parent_id.as_deref() {
        if !folder_is_unlocked(state, pid)? {
            return Err(AppError::Locked(
                "unlock the parent folder before restoring this folder".into(),
            ));
        }
    }

    // A `path` collision means the user recreated a folder with the same name meanwhile. Restore
    // under a suffixed path rather than failing the UNIQUE constraint.
    let path = unique_folder_path(state, &s.path)?;
    // `presentation` was captured with the row; a snapshot written before those columns existed
    // falls back to the same defaults the migration uses, so an old entry still restores.
    let presentation = s.presentation.clone().unwrap_or_else(|| {
        crate::storage::trash_store::FolderPresentation {
            kind: s.kind.clone(),
            level: "folder".to_string(),
            is_root: false,
            emoji: None,
            tint: None,
            position: 0,
        }
    });
    state.db.insert_restored_folder(
        &s.id,
        &s.name,
        &path,
        parent_id.as_deref(),
        &s.created_at,
        &presentation,
    )?;

    // Move the members back — each best-effort and individually gated: a member that has since been
    // deleted, re-filed, or locked must not abort the whole restore.
    for mid in &s.meeting_ids {
        if state.db.get_meeting(mid)?.is_none() {
            continue;
        }
        if let Err(e) = state.db.set_meeting_folder(mid, Some(&s.id)) {
            tracing::warn!(target: "trash", meeting_id = %mid, error = %e, "could not re-file meeting on folder restore");
        }
    }
    for nid in &s.note_ids {
        match state.db.get_note_row(nid) {
            Ok(Some(_)) => {
                if let Err(e) = move_note_doc_inner(state, nid, &s.id) {
                    tracing::warn!(target: "trash", note_id = %nid, error = %e, "could not re-file note on folder restore");
                }
            }
            _ => continue,
        }
    }
    Ok(())
}

/// Parse a snapshot, refusing a version this binary does not understand rather than guessing.
fn parse_snapshot<T: for<'de> Deserialize<'de>>(payload: &str) -> Result<T, AppError> {
    serde_json::from_str(payload).map_err(|e| {
        AppError::Storage(format!(
            "this trash snapshot could not be read back (it may come from a newer version): {e}"
        ))
    })
}

/// Find a free vault path for a restored folder: `p`, else `p (restored)`, else `p (restored 2)`…
/// Bounded so a pathological collision loop cannot spin.
fn unique_folder_path(state: &AppState, path: &str) -> Result<String, AppError> {
    if state.db.folder_by_path(path)?.is_none() {
        return Ok(path.to_string());
    }
    for n in 1..=50 {
        let candidate = if n == 1 {
            format!("{path} (restored)")
        } else {
            format!("{path} (restored {n})")
        };
        if state.db.folder_by_path(&candidate)?.is_none() {
            return Ok(candidate);
        }
    }
    Err(AppError::Storage(
        "could not find a free vault path for the restored folder".into(),
    ))
}

// ── EVENTS ───────────────────────────────────────────────────────────────────────────────────────

/// Tell any open surface the trash changed, carrying only the COUNT (never a label or payload) so
/// the sidebar badge and the Trash view refetch through the gated read.
fn emit_trash_updated(app: &AppHandle, state: &AppState) {
    let count = state.db.count_trash_entries().unwrap_or(0);
    crate::events::emit_trash_updated(app, count);
}
