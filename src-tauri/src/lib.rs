pub mod agent;
pub mod audio;
pub mod brain_reactions;
pub mod calendar;
pub mod commands;
pub mod connectors;
pub mod crypto;
pub mod e2ee;
pub mod embed;
pub mod error;
pub mod eval;
pub mod events;
pub mod export;
pub mod facts;
pub mod mcp;
pub mod orchestrate;
pub mod pipeline;
pub mod proactive;
pub mod reason;
pub mod screenshare;
pub mod secrets;
pub mod settings;
pub mod share;
pub mod state;
pub mod storage;
pub mod summarize;
pub mod tools;
pub mod transcribe;
pub mod update;
pub mod user_memory;
pub mod voice_action;

use tauri::window::{Effect, EffectsBuilder};
use tauri::{Emitter, Manager, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

use crate::state::AppState;

/// Global hotkey that summons / dismisses the floating recorder bar (like a Spotlight bar).
const SUMMON_SHORTCUT: &str = "CmdOrCtrl+Shift+R";

/// Builds the tauri::Builder, manages AppState, registers commands + the floating bar, runs.
pub fn run() {
    // ── Whisper/ggml-metal residency-set abort guard (macOS 15+ / Apple-silicon) ──
    // whisper.cpp 1.8.3's ggml-metal "residency sets" path (compiled only on macOS >= 15.0)
    // asserts `[rsets->data count] == 0` when the Metal device is freed
    // (ggml-metal-device.m `ggml_metal_rsets_free` → GGML_ASSERT). The teardown ordering on
    // Apple-silicon frees the device while a buffer's residency set is still registered, so the
    // assert fires and `ggml_abort` ABORTS the process at whisper's per-transcription Metal free.
    // GGML_ASSERT (ggml.h:288) is NOT NDEBUG-gated, so this aborts in RELEASE too — it is not a
    // debug-only crash. The upstream-sanctioned switch is the `GGML_METAL_NO_RESIDENCY` env var,
    // read live inside `ggml_metal_device_init` (ggml-metal-device.m:768): when set, the device's
    // residency-set collection is never created (`dev->rsets = nil`), so `ggml_metal_rsets_free`
    // returns early and the assert can never fire. Residency sets are only a GPU-memory residency
    // *hint* (keep buffers wired to avoid OS reclamation); disabling them does not change
    // transcription output — it only forgoes a minor perf optimization. Set here at process entry,
    // strictly before any ggml-Metal device can be initialized. SAFETY: single-threaded at
    // startup, before any thread that could read the env. (Mirror guard in `Transcriber::load`.)
    std::env::set_var("GGML_METAL_NO_RESIDENCY", "1");

    let log_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    // Persist logs to a FILE as well as stderr. When the app is launched via LaunchServices (the normal
    // double-click / DMG install) its stderr is discarded, so a signed/release build was previously
    // un-diagnosable — every keychain/share failure was invisible. Logs carry NO PII (IDs, stages,
    // counts, durations only — see the no-PII rule), so persisting them on-device is safe. Fresh file
    // per launch (truncate at start); O_APPEND so concurrent writes stay line-atomic. Best-effort: if the
    // file can't be opened we fall back to stderr-only rather than fail startup.
    let log_file = dirs::data_dir()
        .map(|b| b.join(crate::state::app_dir_name()))
        .and_then(|dir| {
            std::fs::create_dir_all(&dir).ok()?;
            let path = dir.join("murmur.log");
            let _ = std::fs::write(&path, b""); // truncate for a fresh per-session log
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .ok()
        });
    match log_file {
        Some(file) => {
            use tracing_subscriber::fmt::writer::MakeWriterExt;
            tracing_subscriber::fmt()
                .with_env_filter(log_filter)
                .with_ansi(false)
                .with_writer(
                    std::io::stderr.and(move || file.try_clone().expect("clone murmur.log fd")),
                )
                .init()
        }
        None => tracing_subscriber::fmt().with_env_filter(log_filter).init(),
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    // Only one shortcut is registered; toggle the bar on key-down.
                    if event.state() == ShortcutState::Pressed {
                        toggle_bar(app);
                    }
                })
                .build(),
        )
        .invoke_handler(tauri::generate_handler![
            commands::start_recording,
            commands::stop_recording,
            commands::recording_level,
            commands::set_mic_muted,
            commands::is_mic_muted,
            commands::begin_voice_command,
            commands::end_voice_command,
            commands::ask_assistant_text,
            commands::ask_assistant_chat,
            commands::list_assistant_threads,
            commands::list_input_devices,
            commands::output_is_builtin_speakers,
            commands::get_last_note,
            commands::update_note,
            commands::save_manual_notes,
            commands::get_manual_notes,
            commands::import_document,
            commands::import_text,
            commands::brain_overview,
            commands::list_documents,
            commands::get_document,
            commands::delete_document,
            commands::get_user_memory,
            commands::forget_user_fact,
            commands::clear_user_memory,
            commands::get_config,
            commands::save_config,
            commands::get_storage_report,
            commands::free_up_space,
            commands::reveal_audio_dir,
            commands::consent_to_cloud_egress,
            commands::revoke_cloud_egress,
            commands::consent_to_web_search,
            commands::consent_to_jira,
            commands::consent_to_slack,
            // M3-CLIENT — sharing account + zero-knowledge link shares (mode A).
            commands::account_status,
            commands::account_signup,
            commands::account_send_code,
            commands::account_login,
            commands::account_logout,
            commands::unlock_sharing_with_biometric,
            commands::consent_to_share_egress,
            commands::revoke_share_egress,
            commands::mark_sharing_choice_made,
            commands::share_note_to_link,
            commands::list_my_shares,
            commands::revoke_share,
            // M5-CLIENT — Murmur↔Murmur (mode B): invite by email + accept into the vault.
            commands::preview_share_recipient,
            commands::share_note_to_user,
            commands::share_rewrap_pending,
            commands::list_share_inbox,
            commands::accept_share,
            commands::decline_share,
            commands::set_anthropic_key,
            commands::has_anthropic_key,
            commands::set_gateway_key,
            commands::has_gateway_key,
            commands::clear_gateway_key,
            commands::list_gateway_models,
            commands::list_models,
            commands::gateway_health,
            commands::get_egress_ledger,
            commands::set_web_search_api_key,
            commands::has_web_search_key,
            commands::set_jira_token,
            commands::has_jira_token,
            commands::set_slack_token,
            commands::has_slack_token,
            commands::provider_statuses,
            commands::resummarize,
            commands::list_meetings,
            commands::search_meetings,
            commands::delete_meeting,
            commands::rename_meeting,
            commands::chat_meeting,
            commands::export_audio,
            commands::export_mic_master,
            commands::export_sys_master,
            commands::export_note,
            commands::detect_meeting_app,
            commands::set_meeting_tags,
            commands::get_meeting_tags,
            commands::list_all_tags,
            commands::list_meetings_by_tag,
            commands::list_builtin_recipes,
            commands::list_saved_recipes,
            commands::save_recipe,
            commands::delete_recipe,
            commands::run_recipe,
            commands::get_action_items,
            commands::list_open_commitments,
            commands::patch_note_tasks,
            commands::add_reminder,
            commands::pin_moment,
            commands::link_meeting_entities,
            commands::get_graph,
            commands::get_entity_detail,
            commands::get_person_dossier,
            commands::list_people,
            commands::ask_vault,
            commands::entity_dossier,
            commands::generate_digest,
            commands::topic_threads,
            commands::export_canvas,
            commands::next_calendar_event,
            commands::list_calendar_events,
            commands::calendar_context_for,
            commands::get_meeting_detail,
            commands::get_analytics,
            commands::get_timeline,
            commands::rename_speaker,
            commands::suggest_speaker_labels,
            commands::list_voiceprints,
            commands::forget_voiceprint,
            commands::clear_voiceprints,
            commands::model_present,
            commands::download_model,
            commands::brain_model_present,
            commands::list_brain_models,
            commands::brain_model_retirement_nudge,
            commands::brain_posture,
            commands::resolved_ai_map,
            commands::set_brain_posture,
            commands::brain_live_ram_ok,
            commands::brain_reactions_shadow_count,
            commands::set_brain_contradiction_cards,
            commands::select_brain_model,
            commands::download_brain_model,
            commands::afm_available,
            commands::embed_model_present,
            commands::list_embed_models,
            commands::select_embed_model,
            commands::download_embed_model,
            commands::reindex_embeddings,
            commands::related_meetings,
            commands::ner_model_present,
            commands::download_ner_model,
            commands::toggle_bar,
            commands::list_folders,
            commands::create_folder,
            commands::rename_folder,
            commands::delete_folder,
            commands::move_note,
            commands::lock_folder,
            commands::unlock_folder,
            commands::unlock_meeting,
            commands::relock_folder,
            commands::relock_all,
            commands::remove_lock,
            update::check_for_update,
            update::app_info,
            update::open_release_page,
        ])
        .setup(|app| {
            // Open the encrypted library FIRST. If it fails (keychain access denied, or the DB
            // key doesn't match / the file is corrupt) we must NOT panic/abort — that is the
            // v0.3.0 hard-crash. Instead show a friendly dialog and exit cleanly, leaving the DB
            // and its backups untouched on disk.
            let state = match AppState::init() {
                Ok(s) => s,
                Err(e) => {
                    show_fatal_init_dialog(app.handle().clone(), &e);
                    // Return Ok so the event loop spins: the dialog runs on a worker thread and
                    // dispatches to the main run loop, then calls std::process::exit(1). We do NOT
                    // run the rest of setup (windows/tray/MCP) without a valid AppState.
                    return Ok(());
                }
            };
            app.manage(state);

            // Phase 2b — wire the content-free egress ledger. Done here, once, after a successful
            // AppState::init() so the DB is guaranteed open. Every subsequent cloud provider call
            // (routed through RedactingProvider) writes ONE content-free row to egress_log.
            {
                let state = app.state::<AppState>();
                crate::summarize::egress_log::set_egress_sink(std::sync::Arc::new(
                    crate::summarize::egress_log::DbEgressSink::new(state.db.clone()),
                ));
            }

            // Crash-recovery (STAGE 2 salvage + STAGE 1 reconcile) + orphan reap. A session that died
            // mid-record (crash / SIGKILL / `tauri dev` hot-rebuild) never ran `stop_recording`, so the
            // meeting row `start_recording` inserted up-front sits as a `RECORDING` "ghost" AND its mic
            // audio (RAM-only) + far-side scratch would be lost. ORDER IS LOAD-BEARING:
            //   1) CLAIM salvage: find inflight mic spills of crashed recordings + MOVE the paired
            //      far-side scratch out of $TMPDIR (before the reaper below deletes it). Runs FIRST so
            //      the reaper can't eat a recoverable far-side track.
            //   2) RECONCILE the remaining ghosts to terminal `ERROR` — SKIPPING the claimed rows
            //      (salvage sets their final status itself), so a claimed row isn't clobbered.
            //   3) REAP orphaned capture helpers + sweep stale scratch (post-claim, so only truly-
            //      abandoned files remain).
            //   4) SPAWN the async salvage worker: reconstruct each claimed recording + run it through
            //      the EXISTING post-Stop pipeline → a real transcript+note. After the reap so the
            //      paired helper is already down. Best-effort; never deletes un-salvaged audio.
            let salvage_jobs = {
                let state = app.state::<AppState>();
                let (jobs, claimed) = crate::audio::spill::claim_inflight(&state.db);
                if let Err(e) = state.db.reconcile_stuck_recordings_except(&claimed) {
                    tracing::warn!(target: "startup", error = %e, "could not reconcile stuck recordings");
                }
                jobs
            };

            // Reap any capture helper ORPHANED by a previous session that died without a clean Stop
            // (crash / force-quit / a `tauri dev` hot-rebuild SIGKILLing the app mid-record). Such a
            // helper reparents to launchd and keeps capturing to a temp WAV for up to 4h — GBs of
            // dead-session audio the file-age sweep below can't catch (its mtime stays fresh). Run
            // FIRST so the kill releases the file, THEN reclaim any stale scratch left behind.
            // Nothing records yet at setup, so any live capture helper is by definition an orphan.
            // (Salvage above already moved any RECOVERABLE far-side scratch out of the reaper's reach.)
            crate::audio::aec::reap_orphaned_capture_helpers();
            crate::audio::aec::sweep_stale_scratch();

            // STAGE 2 salvage worker (async, detached): now that the reaper has downed any orphan
            // helper, reconstruct + pipeline each claimed crashed recording. No-op when nothing crashed.
            crate::audio::spill::spawn_salvage(app.handle().clone(), salvage_jobs);

            create_bar_window(app.handle())?;
            if let Err(e) = app.global_shortcut().register(SUMMON_SHORTCUT) {
                tracing::warn!(target: "shortcut", error = %e, "could not register global shortcut");
            }
            commands::restart_voice_listener(app.handle().clone());
            setup_tray(app.handle())?;
            // Localhost MCP server (read-only meeting tools for Claude Desktop/Code; no egress).
            // Share the session unlock set so sealed-and-not-unlocked notes stay invisible.
            // Must mirror `AppState::db_path` exactly (same dir + filename) so the MCP server opens
            // the SAME DB the app did — `app_dir_name()` keeps the dev/release split consistent.
            if let Some(db_path) = dirs::data_dir()
                .map(|b| b.join(crate::state::app_dir_name()).join("meetnotes.sqlite"))
            {
                let state = app.state::<AppState>();
                let unlocked = state.unlocked_folders.clone();
                let require_token = state
                    .config
                    .lock()
                    .map(|c| c.mcp_require_token)
                    // Poisoned config ⇒ fail CLOSED (require the token) — aligned with the
                    // reasoner-dispatch poison posture (unreadable config never relaxes auth).
                    .unwrap_or(true);
                crate::mcp::spawn(db_path, unlocked, require_token);
            }
            // Screen-share auto-relock watcher: on capture START, relock all session-unlocked
            // folders + zeroize the KEK and toast the UI. Gated by K_RELOCK_ON_SCREENSHARE.
            crate::screenshare::spawn(app.handle().clone());
            // Closing the main window HIDES it (recoverable from the tray) instead of
            // quitting — so the floating bar is never the only way back into the app.
            // B12/C4: on a window close, relock all session-unlocked folders + zeroize the cached
            // master KEK + checkpoint the WAL — even though the app keeps running in the tray, the
            // unlocked content should not survive the user closing the window.
            if let Some(main) = app.get_webview_window("main") {
                let w = main.clone();
                let handle = app.handle().clone();
                main.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        relock_and_zeroize_on_lifecycle(&handle, "window-close");
                        let _ = w.hide();
                    }
                });
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|handle, event| {
            // B12/C4 app-quit hook: on ExitRequested, relock everything, zeroize the cached KEK, and
            // checkpoint+truncate the WAL so no plaintext lingers past the process. This is the
            // last-chance cleanup before the process tears down.
            if let tauri::RunEvent::ExitRequested { .. } = event {
                relock_and_zeroize_on_lifecycle(handle, "app-exit");
            }
        });
}

/// Shared lifecycle cleanup (B12/C4): relock every session-unlocked folder (which re-blanks plaintext
/// + zeroizes the cached master KEK) and checkpoint+truncate the WAL. Best-effort and panic-free —
///   invoked from both the window-close and app-exit paths, where there is no Result to surface. No-op
///   if AppState was never managed (the graceful-init failure path returns early without it).
fn relock_and_zeroize_on_lifecycle(handle: &tauri::AppHandle, ctx: &str) {
    use crate::state::AppState;
    let Some(state) = handle.try_state::<AppState>() else {
        return; // init failed / state not managed — nothing to clean up.
    };
    // relock_all_inner clears the unlock set, zeroizes the cached KEK, re-blanks all sealed notes,
    // and (as of B12) checkpoints the WAL.
    if let Err(e) = crate::commands::relock_all_inner(state.inner()) {
        tracing::warn!(target: "lock", error = %e, ctx, "lifecycle relock_all failed");
    }
    // Belt-and-suspenders WAL checkpoint in case relock_all short-circuited before its own.
    if let Err(e) = state.db.checkpoint_truncate() {
        tracing::warn!(target: "lock", error = %e, ctx, "lifecycle wal_checkpoint(TRUNCATE) failed");
    }
}

/// Startup-failure handler: AppState::init() returned Err. Show a clear, non-technical native
/// dialog explaining that the encrypted library couldn't be opened, then exit cleanly with code 1
/// — NEVER a Rust panic/abort (the v0.3.0 hard-crash). The two failure modes are distinguished in
/// both the message and the log:
///   (a) [`AppError::KeychainDenied`] — macOS refused keychain access (user clicked "Deny", or the
///       keychain is locked).
///   (b) anything else (storage / migration) — the DB couldn't be opened: the key doesn't match
///       the data (e.g. restored from another Mac) or the file is damaged.
/// This NEVER touches the database or its backups — it is a read-only, fail-safe exit path.
fn show_fatal_init_dialog(handle: tauri::AppHandle, err: &crate::error::AppError) {
    use crate::error::AppError;
    use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

    const TITLE: &str = "Murmur can't open your library";

    let (body, log_reason) = match err {
        AppError::KeychainDenied(_) => (
            "macOS didn't grant access to your keychain, so Murmur couldn't unlock its encrypted \
             database.\n\nYour notes are safe and have not been changed. Please reopen Murmur and \
             choose \"Always Allow\" when macOS asks for keychain access. If this keeps happening, \
             contact support.",
            "keychain access denied or unavailable",
        ),
        _ => (
            "Murmur couldn't unlock its encrypted database. This can happen if the database key \
             doesn't match the data on this Mac (for example after restoring from a backup or \
             another computer) or if the file is damaged.\n\nYour notes have NOT been changed or \
             deleted. Please reopen Murmur, and if this keeps happening, contact support.",
            "database could not be opened (key mismatch / corruption)",
        ),
    };

    // Technical detail goes to the log only (Display carries no PII / no secret material).
    tracing::error!(target: "state", error = %err, reason = log_reason, "startup aborted: AppState::init failed");

    // Hide the config-created main window so its webview can't flash a broken state behind the
    // dialog (its commands would have no managed AppState).
    if let Some(main) = handle.get_webview_window("main") {
        let _ = main.hide();
    }

    // blocking_show() MUST run off the main thread — it dispatches the native dialog to the main
    // run loop and blocks the caller, so calling it on the main thread would deadlock. Spawn a
    // worker that shows the modal, then exits cleanly (code 1) once the user clicks OK.
    let body = body.to_string();
    std::thread::spawn(move || {
        handle
            .dialog()
            .message(body)
            .title(TITLE)
            .kind(MessageDialogKind::Error)
            .buttons(MessageDialogButtons::Ok)
            .blocking_show();
        std::process::exit(1);
    });
}

/// Create the always-on-top, frameless, transparent floating recorder bar (hidden until
/// summoned with the global shortcut). It loads the `/bar` route of the same frontend.
fn create_bar_window(app: &tauri::AppHandle) -> tauri::Result<()> {
    if app.get_webview_window("bar").is_some() {
        return Ok(());
    }
    let win = WebviewWindowBuilder::new(app, "bar", WebviewUrl::App("bar".into()))
        .title("Murmur")
        .inner_size(540.0, 58.0)
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .shadow(true)
        .effects(
            EffectsBuilder::new()
                .effect(Effect::HudWindow)
                .radius(29.0)
                .build(),
        )
        .visible(false)
        .build()?;
    position_bar_top_center(&win);
    Ok(())
}

/// Toggle the floating bar: hide if visible, otherwise reposition top-centre, show + focus.
/// Bound to the global ⌘⇧R shortcut and exposed to the UI via `commands::toggle_bar`.
pub fn toggle_bar(app: &tauri::AppHandle) {
    if let Some(win) = app.get_webview_window("bar") {
        if matches!(win.is_visible(), Ok(true)) {
            let _ = win.hide();
        } else {
            position_bar_top_center(&win);
            let _ = win.show();
            let _ = win.set_focus();
        }
    }
}

/// Menu-bar (tray) icon so Murmur is always reachable — even with no window open or only
/// the floating bar showing. Left-click opens the main window; the menu offers Open, the
/// recorder bar, and Quit.
fn setup_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

    let open = MenuItem::with_id(app, "open", "Open Murmur", true, None::<&str>)?;
    let record = MenuItem::with_id(app, "record", "Start / Stop recording", true, None::<&str>)?;
    let bar = MenuItem::with_id(app, "bar", "Recorder bar  (⌘⇧R)", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Murmur", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &open,
            &record,
            &bar,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;

    let mut builder = TrayIconBuilder::new()
        .menu(&menu)
        .show_menu_on_left_click(false)
        .tooltip("Murmur")
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_main(app),
            "record" => toggle_record(app),
            "bar" => toggle_bar(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

/// Tray "Start / Stop recording": ask the (possibly hidden) main window to toggle a
/// recording. The webview stays alive while hidden, so this records without opening a window.
fn toggle_record(app: &tauri::AppHandle) {
    if let Some(main) = app.get_webview_window("main") {
        let _ = main.emit(crate::events::EVENT_TOGGLE_RECORD, ());
    }
}

/// Show + focus the main window (after a hide/minimize), recreating it if it was closed.
fn show_main(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

/// Centre the bar horizontally near the top of the monitor it's on.
fn position_bar_top_center(win: &tauri::WebviewWindow) {
    if let (Ok(Some(monitor)), Ok(size)) = (win.current_monitor(), win.outer_size()) {
        let screen = monitor.size();
        let scale = monitor.scale_factor();
        let x = ((screen.width as f64 - size.width as f64) / 2.0).max(0.0);
        let y = (48.0 * scale).min(screen.height as f64 * 0.3);
        let _ = win.set_position(tauri::PhysicalPosition::new(x, y));
    }
}
