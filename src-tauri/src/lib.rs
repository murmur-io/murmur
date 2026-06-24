pub mod audio;
pub mod commands;
pub mod error;
pub mod events;
pub mod export;
pub mod pipeline;
pub mod secrets;
pub mod settings;
pub mod state;
pub mod storage;
pub mod summarize;
pub mod transcribe;

use crate::state::AppState;

/// Builds the tauri::Builder, manages AppState, registers commands, and runs the app.
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
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            commands::start_recording,
            commands::stop_recording,
            commands::recording_level,
            commands::get_last_note,
            commands::get_config,
            commands::save_config,
            commands::set_anthropic_key,
            commands::has_anthropic_key,
            commands::provider_statuses,
            commands::resummarize,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
