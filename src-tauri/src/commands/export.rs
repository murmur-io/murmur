//! User-chosen-path & Canvas EXPORT commands — extracted verbatim from `commands` (God-file split,
//! a PURE MOVE — the read-gate logic is UNCHANGED, only relocated). This is the "export to a path the
//! user picked in a save dialog" surface: the meeting recording WAV, the per-stream MIC/SYS master
//! archives, the note markdown, and the Obsidian `.canvas`. EVERY command here is READ-GATED on
//! `super::meeting_is_unlocked` and FAILS CLOSED with `AppError::Locked` for a
//! sealed-and-not-session-unlocked meeting (its WAV/master is `.enc` at rest, its note markdown +
//! timeline are blanked) — the gate is byte-identical to its pre-move form, and no path is ever
//! handed to the FE for a locked meeting. The vault-relative Canvas dir stays inside the vault via
//! `super::assert_in_vault` (D5). Every symbol keeps its EXACT prior body/signature and is re-exported
//! at `crate::commands` via `pub use export_commands::*;` in `commands/mod.rs`, so
//! `generate_handler![commands::export_audio]` in `lib.rs` and every `crate::commands::…` caller
//! resolve UNCHANGED. `use super::*` brings in the shared types + the gate helper `meeting_is_unlocked`
//! and `assert_in_vault` (both kept in `commands/mod.rs`).
//!
//! NOT MOVED (deliberately LEFT in `commands/mod.rs`): the note→VAULT export cluster
//! (`export_note_doc` / `export_note_to_vault` / `write_note_to_vault`) — it is intertwined with the
//! notes/unseal domain (`write_note_to_vault` is SHARED with the unseal re-export path) — and the
//! shared vault helpers `vault_path` / `assert_in_vault` themselves. Those belong with a later vault
//! leaf; here they are only referenced through `super::`.

use super::*;

/// Copy a meeting's recording (WAV) to a user-chosen path (FE picks it via a save dialog).
#[tauri::command]
pub fn export_audio(
    state: State<'_, AppState>,
    meeting_id: String,
    dest_path: String,
) -> Result<(), AppError> {
    export_audio_inner(state.inner(), &meeting_id, &dest_path)
}

pub(crate) fn export_audio_inner(
    state: &AppState,
    meeting_id: &str,
    dest_path: &str,
) -> Result<(), AppError> {
    // User-selected plaintext publication is synchronous. Serialize the complete gate → source
    // read → copy interval with relock so cleanup cannot finish and then be followed by stale output.
    let _lifecycle = lifecycle_guard(state);
    // Phase 0.5 READ-GATE: refuse to export the audio of a sealed-and-not-unlocked meeting. Its
    // WAV is AES-GCM-encrypted at rest (audio_path → <file>.enc) and there is no plaintext on disk
    // to copy until the folder is session-unlocked; fail closed with a Locked error.
    if !meeting_is_unlocked(state, meeting_id)? {
        return Err(AppError::Locked(crate::errcode::tag(
            crate::errcode::MEETING_LOCKED,
            "unlock this meeting's folder to export the audio",
        )));
    }
    let meeting = state
        .db
        .get_meeting(meeting_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no meeting with id {meeting_id}")))?;
    let src = meeting
        .audio_path
        .ok_or_else(|| AppError::InvalidArg("this meeting has no audio file".into()))?;
    std::fs::copy(&src, dest_path)
        .map_err(|e| AppError::Storage(format!("copy audio failed: {e}")))?;
    Ok(())
}

/// Which per-stream master to export.
enum MasterStream {
    Mic,
    Sys,
}

/// Shared READ-GATED export for a per-stream master archive (faithful float32 WAV). Refuses a
/// sealed-and-not-unlocked meeting (the master is `.enc` at rest, no plaintext to copy) and never
/// hands a path to the FE — the masters are reachable ONLY through these gated commands.
fn export_master(
    state: &AppState,
    meeting_id: &str,
    dest_path: &str,
    which: MasterStream,
) -> Result<(), AppError> {
    let _lifecycle = lifecycle_guard(state);
    if !meeting_is_unlocked(state, meeting_id)? {
        return Err(AppError::Locked(crate::errcode::tag(
            crate::errcode::MEETING_LOCKED,
            "unlock this meeting's folder to export the master",
        )));
    }
    let (mic, sys) = state.db.get_meeting_master_paths(meeting_id)?;
    let src = match which {
        MasterStream::Mic => mic,
        MasterStream::Sys => sys,
    }
    .ok_or_else(|| AppError::InvalidArg("this meeting has no master for that stream".into()))?;
    std::fs::copy(&src, dest_path)
        .map_err(|e| AppError::Storage(format!("copy master failed: {e}")))?;
    Ok(())
}

/// Export a meeting's MIC master archive (faithful native-rate float32 WAV) to a chosen path.
#[tauri::command]
pub fn export_mic_master(
    state: State<'_, AppState>,
    meeting_id: String,
    dest_path: String,
) -> Result<(), AppError> {
    export_master(state.inner(), &meeting_id, &dest_path, MasterStream::Mic)
}

/// Export a meeting's SYSTEM master archive (faithful 48 kHz float32 WAV) to a chosen path.
#[tauri::command]
pub fn export_sys_master(
    state: State<'_, AppState>,
    meeting_id: String,
    dest_path: String,
) -> Result<(), AppError> {
    export_master(state.inner(), &meeting_id, &dest_path, MasterStream::Sys)
}

/// Write a meeting's note markdown to a user-chosen path (FE picks it via a save dialog).
#[tauri::command]
pub fn export_note(
    state: State<'_, AppState>,
    meeting_id: String,
    dest_path: String,
) -> Result<(), AppError> {
    // Serialize the complete gate -> plaintext read -> image publication -> Markdown publication
    // interval with lock/relock. This is synchronous I/O (no await), so holding the coarse lifecycle
    // guard is both safe and necessary: otherwise relock can finish cleanup after the one-time gate
    // and a stale exporter can recreate plaintext on disk while the folder is sealed.
    let _lifecycle = lifecycle_guard(state.inner());
    // D4 READ-GATE: refuse to export a sealed-and-not-unlocked meeting's note (its plaintext
    // markdown is blanked while sealed — exporting would write an empty/garbage file). Fail closed.
    if !meeting_is_unlocked(state.inner(), &meeting_id)? {
        return Err(AppError::Locked(crate::errcode::tag(
            crate::errcode::MEETING_LOCKED,
            "unlock this meeting's folder to export the note",
        )));
    }
    let note = state
        .db
        .get_latest_note_for_meeting(&meeting_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no note for meeting {meeting_id}")))?;
    let dest = std::path::Path::new(&dest_path);
    let root = dest
        .parent()
        .ok_or_else(|| AppError::Export("export destination has no parent directory".into()))?;
    let exported = render_markdown_with_attachments_for_user_export_under_lifecycle_authorized(
        state.inner(),
        &crate::storage::AttachmentOwner::Meeting {
            meeting_id: meeting_id.clone(),
            provider_id: note.provider_id.clone(),
        },
        &note.markdown,
        root,
    )?;
    crate::export::overwrite_note(dest, &exported)?;
    Ok(())
}

/// Export a meeting as an Obsidian Canvas (.canvas) — a spatial board of its topic spans.
/// Requires the timeline (open the meeting once). Returns the written path.
#[tauri::command]
pub fn export_canvas(state: State<'_, AppState>, meeting_id: String) -> Result<String, AppError> {
    export_canvas_inner(state.inner(), &meeting_id)
}

pub(crate) fn export_canvas_inner(
    state: &AppState,
    meeting_id: &str,
) -> Result<String, AppError> {
    // Keep the content read and final Canvas publication in one lifecycle interval. Otherwise a
    // relock can blank/delete managed plaintext and the stale exporter can recreate it afterwards.
    let _lifecycle = lifecycle_guard(state);
    // D4 READ-GATE: a sealed-and-not-unlocked meeting's timeline is blanked; refuse to build a
    // canvas from it. Fail closed.
    if !meeting_is_unlocked(state, meeting_id)? {
        return Err(AppError::Locked(crate::errcode::tag(
            crate::errcode::MEETING_LOCKED,
            "unlock this meeting's folder to export the canvas",
        )));
    }
    let meeting = state
        .db
        .get_meeting(meeting_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no meeting with id {meeting_id}")))?;
    let json = state.db.get_timeline_data(meeting_id)?.ok_or_else(|| {
        AppError::InvalidArg("open the meeting once to generate its timeline first".into())
    })?;
    let mut tl: MeetingTimeline = serde_json::from_str(&json)
        .map_err(|e| AppError::InvalidArg(format!("bad timeline data: {e}")))?;
    // Same coverage-repair the Detail view applies (heal a legacy cache that ends short of the
    // recording) so the exported canvas spans the meeting instead of the provider's early cluster.
    let segments = state.db.get_segments(meeting_id)?;
    crate::summarize::timeline::repair_coverage(&mut tl, &segments);
    let title = meeting.title.unwrap_or_else(|| "Meeting".to_string());
    let topics: Vec<(String, f64, f64)> = tl
        .topics
        .iter()
        .map(|t| (t.label.clone(), t.start_s, t.end_s))
        .collect();
    let canvas = crate::export::canvas::build_canvas(&title, &topics);
    let vault = {
        state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?
            .vault_path
            .clone()
    }
    .filter(|p| !p.is_empty())
    .ok_or_else(|| AppError::InvalidArg("set a vault folder in Settings first".into()))?;
    let vault_root = std::path::Path::new(&vault);
    // D5: the Canvas dir must resolve inside the vault root.
    let dir = assert_in_vault(vault_root, std::path::Path::new("Canvas"))?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| AppError::Export(format!("create Canvas dir failed: {e}")))?;
    let fname: String = title
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '#' | '^' | '[' | ']' => ' ',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let fname = if fname.is_empty() {
        "Meeting".to_string()
    } else {
        fname
    };
    // D5: re-assert the final file path stays inside the vault (fname is sanitized, but bind the
    // guarantee at the write site).
    let path = assert_in_vault(
        vault_root,
        &std::path::Path::new("Canvas").join(format!("{fname}.canvas")),
    )?;
    std::fs::write(&path, canvas)
        .map_err(|e| AppError::Export(format!("write canvas failed: {e}")))?;
    Ok(path.to_string_lossy().to_string())
}
