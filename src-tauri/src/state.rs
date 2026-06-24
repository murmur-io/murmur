use std::path::PathBuf;
use std::sync::Mutex;

use crate::audio::Recorder;
use crate::error::{AppError, Result};
use crate::settings::AppConfig;
use crate::storage::Db;

/// App-data folder name (mirrors the human-friendly name used by `transcribe::model`).
const APP_DIR: &str = "MeetNotes";
/// SQLite database filename inside the app-data folder.
const DB_FILE: &str = "meetnotes.sqlite";

pub struct AppState {
    /// Some while recording.
    pub recorder: Mutex<Option<Recorder>>,
    /// Db is internally Send+Sync (Mutex<Connection>).
    pub db: Db,
    /// In-memory cache of the settings table.
    pub config: Mutex<AppConfig>,
    pub current_meeting: Mutex<Option<uuid::Uuid>>,
}

impl AppState {
    /// Open DB at the app-data dir, run migrations, load config. Called once in `lib::run`.
    pub fn init() -> Result<Self> {
        let db_path = Self::db_path()?;
        let db = Db::open(&db_path)?;
        let config = AppConfig::load(&db)?;

        tracing::info!(target: "state", "app state initialized");

        Ok(Self {
            recorder: Mutex::new(None),
            db,
            config: Mutex::new(config),
            current_meeting: Mutex::new(None),
        })
    }

    /// `<app-data>/MeetNotes/meetnotes.sqlite`, creating the directory if absent.
    fn db_path() -> Result<PathBuf> {
        let base = dirs::data_dir()
            .ok_or_else(|| AppError::Storage("could not resolve app-data directory".into()))?;
        let dir = base.join(APP_DIR);
        std::fs::create_dir_all(&dir)
            .map_err(|e| AppError::Storage(format!("create app-data dir: {e}")))?;
        Ok(dir.join(DB_FILE))
    }
}
