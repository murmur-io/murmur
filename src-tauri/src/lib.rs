pub mod audio;
pub mod biometric;
pub mod commands;
pub mod crypto;
pub mod error;
pub mod events;
pub mod export;
pub mod mcp;
pub mod pipeline;
pub mod screenshare;
pub mod secrets;
pub mod settings;
pub mod state;
pub mod storage;
pub mod summarize;
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

    let state = AppState::init().expect("failed to initialize app state");

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
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            commands::start_recording,
            commands::stop_recording,
            commands::recording_level,
            commands::set_mic_muted,
            commands::is_mic_muted,
            commands::get_last_note,
            commands::update_note,
            commands::get_config,
            commands::save_config,
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
            commands::patch_note_tasks,
            commands::add_reminder,
            commands::pin_moment,
            commands::link_meeting_entities,
            commands::get_graph,
            commands::get_entity_detail,
            commands::ask_vault,
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
            commands::toggle_bar,
            commands::list_folders,
            commands::create_folder,
            commands::move_note,
            commands::lock_folder,
            commands::unlock_folder,
            commands::unlock_meeting,
            commands::relock_folder,
            commands::relock_all,
            commands::remove_lock,
        ])
        .setup(|app| {
            create_bar_window(app.handle())?;
            if let Err(e) = app.global_shortcut().register(SUMMON_SHORTCUT) {
                tracing::warn!(target: "shortcut", error = %e, "could not register global shortcut");
            }
            commands::restart_voice_listener(app.handle().clone());
            setup_tray(app.handle())?;
            // Localhost MCP server (read-only meeting tools for Claude Desktop/Code; no egress).
            // Share the session unlock set so sealed-and-not-unlocked notes stay invisible.
            if let Some(db_path) =
                dirs::data_dir().map(|b| b.join("MeetNotes").join("meetnotes.sqlite"))
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
            if let Some(main) = app.get_webview_window("main") {
                let w = main.clone();
                main.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = w.hide();
                    }
                });
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
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
