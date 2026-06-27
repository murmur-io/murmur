use std::path::PathBuf;
use std::sync::{Arc, Mutex};

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
    /// Some while recording AND echo-cancellation (VPIO AEC) capture is enabled + available.
    pub aec_recorder: Mutex<Option<crate::audio::aec::AecRecorder>>,
    /// Some while the voice-trigger listener is running.
    pub voice_listener: Mutex<Option<VoiceListener>>,
    /// Db is internally Send+Sync (Mutex<Connection>).
    pub db: Db,
    /// In-memory cache of the settings table.
    pub config: Mutex<AppConfig>,
    pub current_meeting: Mutex<Option<uuid::Uuid>>,
    /// Folder ids unlocked in the current session: sealed folders decrypted for in-app view +
    /// MCP until relock (cleared on screen-share start or app exit). Arc so the MCP server
    /// thread shares the SAME set as the command surface.
    pub unlocked_folders: Arc<Mutex<std::collections::HashSet<String>>>,
    /// Master KEK released by biometric; None until first unlock, zeroized on relock.
    pub master_kek: Mutex<Option<[u8; 32]>>,
}

impl AppState {
    /// Open DB at the app-data dir, run migrations, load config. Called once in `lib::run`.
    ///
    /// Returns `Err` (never panics) on any failure so the caller can show a graceful dialog and
    /// exit cleanly: a [`AppError::KeychainDenied`] when macOS refused keychain access, or a
    /// storage/migration error when the database itself could not be opened (key mismatch /
    /// corruption). On EVERY failure path the on-disk database and its `.pre-encrypt.bak` backup
    /// are left byte-for-byte intact — opening with a wrong key fails read-only (proven by
    /// `init_with_wrong_key_errors_and_preserves_db`), and migration only swaps after a verified
    /// encrypted copy exists (see `storage::migration`).
    pub fn init() -> Result<Self> {
        let db_path = Self::db_path()?;
        // SQLCipher at-rest: fetch (or create) the DEK once (Keychain). A denial surfaces as a
        // typed AppError::KeychainDenied here — propagated, not unwrapped.
        let dek = crate::secrets::get_or_create_db_dek()?;
        Self::init_at(&db_path, &dek)
    }

    /// Core of [`AppState::init`] with the DB path + DEK injected, so it can be unit-tested
    /// against a temp database without touching the real Keychain or app-data dir. Migrates any
    /// existing PLAINTEXT DB to encrypted (safe: the original is untouched until a verified atomic
    /// swap), then opens the keyed connection. A wrong/garbage key makes `Db::open_with_key` fail
    /// before any write, so the file is left unchanged.
    fn init_at(db_path: &std::path::Path, dek: &str) -> Result<Self> {
        if crate::storage::migration::needs_encryption(db_path)? {
            tracing::info!(target: "state", "plaintext DB detected — encrypting at rest");
            crate::storage::migration::encrypt_in_place(db_path, dek)?;
        }
        let db = Db::open_with_key(db_path, dek)?;
        let config = AppConfig::load(&db)?;

        tracing::info!(target: "state", "app state initialized");

        Ok(Self {
            recorder: Mutex::new(None),
            system_recorder: Mutex::new(None),
            aec_recorder: Mutex::new(None),
            voice_listener: Mutex::new(None),
            db,
            config: Mutex::new(config),
            current_meeting: Mutex::new(None),
            unlocked_folders: Arc::new(Mutex::new(std::collections::HashSet::new())),
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

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    const GOOD_KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const WRONG_KEY: &str = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";

    fn tmp_path(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "murmur-state-{tag}-{}-{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&p);
        p
    }

    /// Build a real SQLCipher-encrypted DB (keyed with `GOOD_KEY`) by seeding a plaintext DB and
    /// running the production encrypt-in-place path.
    fn seed_encrypted_db(path: &std::path::Path) {
        let c = Connection::open(path).unwrap();
        c.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE meetings(id TEXT PRIMARY KEY, started_at TEXT, title TEXT);
             CREATE TABLE segments(meeting_id TEXT, idx INTEGER, text TEXT);
             CREATE TABLE notes(meeting_id TEXT, markdown TEXT);
             INSERT INTO meetings VALUES('m1','2026-07-01','Sync'),('m2','2026-07-02','Review');
             PRAGMA user_version=7;",
        )
        .unwrap();
        drop(c);
        crate::storage::migration::encrypt_in_place(path, GOOD_KEY).unwrap();
        // Fold any sidecar WAL/SHM into the main file so the byte-equality check below is stable.
        assert!(path.exists());
    }

    /// A wrong/garbage DEK must make `init_at` return `Err` (NOT panic) AND leave the on-disk
    /// database byte-for-byte unchanged — init fails read-only and never destroys data.
    #[test]
    fn init_with_wrong_key_errors_and_preserves_db() {
        let p = tmp_path("wrongkey-init");
        seed_encrypted_db(&p);

        let before = std::fs::read(&p).unwrap();

        let res = AppState::init_at(&p, WRONG_KEY);
        assert!(
            res.is_err(),
            "wrong key must return Err, never panic or yield an empty/garbage DB"
        );

        let after = std::fs::read(&p).unwrap();
        assert_eq!(
            before, after,
            "a failed open must leave the database file byte-for-byte intact"
        );

        // Proof the data survived: the CORRECT key still decrypts the untouched file.
        let conn = Connection::open(&p).unwrap();
        conn.pragma_update(None, "key", format!("x'{GOOD_KEY}'"))
            .unwrap();
        let n: i64 = conn
            .query_row("SELECT count(*) FROM meetings", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 2, "rows intact after the failed wrong-key open");

        let _ = std::fs::remove_file(&p);
    }
}
