pub mod audio;
pub mod commands;
pub mod crypto;
pub mod embed;
pub mod error;
pub mod events;
pub mod export;
pub mod mcp;
pub mod orchestrate;
pub mod pipeline;
pub mod reason;
pub mod screenshare;
pub mod secrets;
pub mod settings;
pub mod state;
pub mod storage;
pub mod summarize;
pub mod tools;
pub mod transcribe;

use tauri::window::{Effect, EffectsBuilder};
use tauri::{Emitter, Manager, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

use crate::state::AppState;

/// Global hotkey that summons / dismisses the floating recorder bar (like a Spotlight bar).
const SUMMON_SHORTCUT: &str = "CmdOrCtrl+Shift+R";

/// Builds the tauri::Builder, manages AppState, registers commands + the floating bar, runs.
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

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
            commands::list_input_devices,
            commands::get_last_note,
            commands::update_note,
            commands::get_config,
            commands::save_config,
            commands::consent_to_cloud_egress,
            commands::set_anthropic_key,
            commands::has_anthropic_key,
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
            commands::ask_vault,
            commands::entity_dossier,
            commands::generate_digest,
            commands::topic_threads,
            commands::export_canvas,
            commands::pre_meeting_brief,
            commands::next_calendar_event,
            commands::get_meeting_detail,
            commands::get_analytics,
            commands::get_timeline,
            commands::rename_speaker,
            commands::model_present,
            commands::download_model,
            commands::brain_model_present,
            commands::list_brain_models,
            commands::select_brain_model,
            commands::download_brain_model,
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
                    .unwrap_or(false);
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
    let record =
        MenuItem::with_id(app, "record", "Start / Stop recording", true, None::<&str>)?;
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
