use std::path::PathBuf;
use std::sync::Mutex;

use crate::audio::listener::VoiceListener;
use crate::audio::system::SystemAudioRecorder;
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
    /// Some while recording AND system-audio capture is enabled + available.
    pub system_recorder: Mutex<Option<SystemAudioRecorder>>,
    /// Some while the voice-trigger listener is running.
    pub voice_listener: Mutex<Option<VoiceListener>>,
    /// Db is internally Send+Sync (Mutex<Connection>).
    pub db: Db,
    /// In-memory cache of the settings table.
    pub config: Mutex<AppConfig>,
    pub current_meeting: Mutex<Option<uuid::Uuid>>,
    /// Folder ids unlocked in the current session: sealed folders decrypted for in-app view +
    /// MCP until relock (cleared on screen-share start or app exit).
    pub unlocked_folders: Mutex<std::collections::HashSet<String>>,
    /// Master KEK released by biometric; None until first unlock, zeroized on relock.
    pub master_kek: Mutex<Option<[u8; 32]>>,
}

impl AppState {
    /// Open DB at the app-data dir, run migrations, load config. Called once in `lib::run`.
    pub fn init() -> Result<Self> {
        let db_path = Self::db_path()?;
        // SQLCipher at-rest: fetch (or create) the DEK once, migrate any existing PLAINTEXT DB to
        // encrypted (safe: original untouched until a verified atomic swap), then open keyed.
        let dek = crate::secrets::get_or_create_db_dek()?;
        if crate::storage::migration::needs_encryption(&db_path)? {
            tracing::info!(target: "state", "plaintext DB detected — encrypting at rest");
            crate::storage::migration::encrypt_in_place(&db_path, &dek)?;
        }
        let db = Db::open_with_key(&db_path, &dek)?;
        let config = AppConfig::load(&db)?;

        tracing::info!(target: "state", "app state initialized");

        Ok(Self {
            recorder: Mutex::new(None),
            system_recorder: Mutex::new(None),
            voice_listener: Mutex::new(None),
            db,
            config: Mutex::new(config),
            current_meeting: Mutex::new(None),
            unlocked_folders: Mutex::new(std::collections::HashSet::new()),
            master_kek: Mutex::new(None),
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
