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
    /// Master KEK released by biometric; None until first unlock, zeroized on relock. Held in a
    /// `Zeroizing` so the bytes are wiped from RAM whenever the cached copy is dropped/replaced (C4),
    /// in addition to the explicit `zeroize()` on relock-all.
    pub master_kek: Mutex<Option<zeroize::Zeroizing<[u8; 32]>>>,
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
        // B4 startup sweep: remove any PLAINTEXT-era `*.pre-encrypt.bak` snapshot an older build may
        // have left in the app-data dir (the live at-rest leak this audit closes). Runs BEFORE the
        // migration so the fresh KEYED backup that `encrypt_in_place` writes survives. LEAVES
        // `*.session-encrypted.bak` untouched — those are ENCRYPTED user data, not a leak.
        if let Some(dir) = db_path.parent() {
            sweep_pre_encrypt_baks(dir);
        }
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

/// Does `path` start with the plaintext-SQLite magic ("SQLite format 3\0")? A SQLCipher-encrypted
/// file begins with random salt, so the magic is absent. Used by the B4 sweep to delete ONLY the
/// genuinely-plaintext recovery snapshots an older build leaked — never a keyed (encrypted) one.
fn is_plaintext_sqlite_file(path: &std::path::Path) -> bool {
    use std::io::Read;
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return false,
    };
    let mut buf = [0u8; 16];
    matches!(f.read(&mut buf), Ok(16)) && &buf == b"SQLite format 3\0"
}

/// B4 startup sweep: delete every PLAINTEXT `*.pre-encrypt.bak` snapshot in `dir` (the live at-rest
/// leak older builds wrote during migration). A `*.pre-encrypt.bak` that is KEYED/encrypted (the
/// modern recovery copy) is LEFT IN PLACE — it is not a leak. `*.session-encrypted.bak` files are
/// NEVER touched (they are encrypted user data). Best-effort: a failed remove is logged, not fatal.
fn sweep_pre_encrypt_baks(dir: &std::path::Path) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        // Only `*.pre-encrypt.bak`. Explicitly skip `*.session-encrypted.bak` (encrypted user data).
        if !name.ends_with(".pre-encrypt.bak") {
            continue;
        }
        // Only remove the genuinely-plaintext leak; preserve a keyed recovery backup.
        if !is_plaintext_sqlite_file(&path) {
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => tracing::warn!(
                target: "state",
                "B4 sweep: removed a leaked plaintext pre-encrypt backup"
            ),
            Err(e) => tracing::warn!(
                target: "state",
                error = %e,
                "B4 sweep: failed to remove a plaintext pre-encrypt backup"
            ),
        }
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

    /// B4 sweep: a PLAINTEXT `*.pre-encrypt.bak` (the live at-rest leak older builds wrote) is
    /// removed at startup; a KEYED (encrypted) `*.pre-encrypt.bak` and any `*.session-encrypted.bak`
    /// (encrypted user data) are LEFT IN PLACE.
    #[test]
    fn b4_sweep_removes_plaintext_bak_keeps_encrypted_and_session() {
        let dir = std::env::temp_dir().join(format!(
            "murmur-sweep-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        // (a) a genuinely-plaintext pre-encrypt backup → MUST be swept.
        let plaintext_bak = dir.join("meetnotes.sqlite.pre-encrypt.bak");
        {
            let c = Connection::open(&plaintext_bak).unwrap();
            c.execute_batch("CREATE TABLE t(x); INSERT INTO t VALUES(1);").unwrap();
        }
        assert!(is_plaintext_sqlite_file(&plaintext_bak), "fixture is plaintext");

        // (b) a KEYED (encrypted) pre-encrypt backup → MUST be kept (not a leak).
        let keyed_bak = dir.join("other.sqlite.pre-encrypt.bak");
        {
            let c = Connection::open(&keyed_bak).unwrap();
            c.pragma_update(None, "key", "x'00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff'").unwrap();
            c.execute_batch("CREATE TABLE t(x); INSERT INTO t VALUES(1);").unwrap();
        }
        assert!(!is_plaintext_sqlite_file(&keyed_bak), "keyed backup is not plaintext");

        // (c) a session-encrypted backup (encrypted user data) → MUST be kept regardless of content.
        let session_bak = dir.join("meetnotes.sqlite.session-encrypted.bak");
        std::fs::write(&session_bak, b"SQLite format 3\0 but this is session-encrypted user data").unwrap();

        sweep_pre_encrypt_baks(&dir);

        assert!(!plaintext_bak.exists(), "plaintext pre-encrypt backup must be swept (B4)");
        assert!(keyed_bak.exists(), "a keyed pre-encrypt backup is encrypted at rest — keep it");
        assert!(session_bak.exists(), "session-encrypted backups are user data — never swept");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `encrypt_in_place` (run via `init_at`) must NEVER leave a plaintext `.pre-encrypt.bak` — the
    /// recovery backup is keyed. After a full init, no plaintext bak exists in the dir.
    #[test]
    fn init_leaves_no_plaintext_pre_encrypt_bak() {
        let p = tmp_path("nobak");
        // Seed a PLAINTEXT db so init_at performs the migration (which writes the keyed backup).
        {
            let c = Connection::open(&p).unwrap();
            c.execute_batch(
                "PRAGMA journal_mode=WAL;
                 CREATE TABLE meetings(id TEXT PRIMARY KEY, started_at TEXT, title TEXT);
                 CREATE TABLE segments(meeting_id TEXT, idx INTEGER, text TEXT);
                 CREATE TABLE notes(meeting_id TEXT, markdown TEXT);
                 INSERT INTO meetings VALUES('m1','2026-07-01','Sync');
                 PRAGMA user_version=7;",
            )
            .unwrap();
        }
        let _state = AppState::init_at(&p, GOOD_KEY).expect("init should encrypt + open");

        // The main DB is now encrypted, and the recovery backup (if present) is NOT plaintext.
        let bak = {
            let mut s = p.as_os_str().to_os_string();
            s.push(".pre-encrypt.bak");
            PathBuf::from(s)
        };
        if bak.exists() {
            let mut f = std::fs::File::open(&bak).unwrap();
            use std::io::Read;
            let mut buf = [0u8; 16];
            let n = f.read(&mut buf).unwrap();
            assert!(
                !(n == 16 && &buf == b"SQLite format 3\0"),
                "B4: init must never leave a PLAINTEXT pre-encrypt backup"
            );
        }

        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(&bak);
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
