//! One-time, safety-first migration of an existing PLAINTEXT SQLite database to a SQLCipher
//! encrypted one. The original file is never mutated until a fully-verified encrypted copy
//! exists; the only mutating moment is the final atomic rename.
//!
//! B4 (P0 live-leak fix): the recovery snapshot is NO LONGER a plaintext `.pre-encrypt.bak`. Writing
//! the user's entire library to an UNENCRYPTED file next to the DB defeated the at-rest encryption
//! we were in the middle of applying (anyone with file access could read it). The recovery copy is
//! now a KEYED (SQLCipher-encrypted, same DEK) `.pre-encrypt.bak` produced via ATTACH + sqlcipher_-
//! export into a keyed attached DB — encrypted at rest like the real target. A startup sweep in
//! `state::init_at` additionally removes any stale plaintext `.pre-encrypt.bak` left by an older
//! build.

use std::io::Read;
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::error::{AppError, Result};

fn mig_err(ctx: &str, e: impl std::fmt::Display) -> AppError {
    AppError::Migration(format!("{ctx}: {e}"))
}

/// Should `path` be encrypted-in-place? True only if it exists AND is still a plaintext SQLite
/// file. A non-existent path (fresh install) or an already-encrypted file ⇒ false. Idempotent.
pub fn needs_encryption(path: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    is_plaintext_sqlite(path)
}

/// A plaintext SQLite file begins with the 16-byte magic "SQLite format 3\0". A SQLCipher file
/// begins with random salt, so the magic is absent. <16 bytes ⇒ not a populated plaintext DB.
fn is_plaintext_sqlite(path: &Path) -> Result<bool> {
    let mut f = std::fs::File::open(path).map_err(|e| mig_err("open for sniff", e))?;
    let mut buf = [0u8; 16];
    let n = f.read(&mut buf).map_err(|e| mig_err("read header", e))?;
    Ok(n == 16 && &buf == b"SQLite format 3\0")
}

fn backup_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(".pre-encrypt.bak");
    PathBuf::from(s)
}

fn tmp_target(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(".encrypting");
    PathBuf::from(s)
}

/// Encrypt the plaintext DB at `path` in place: checkpoint WAL → KEYED encrypted backup → export
/// into a fresh SQLCipher DB → verify (integrity + per-table row counts) → atomic swap. On any error
/// before the swap, the original is left bit-for-bit intact and the temp target is removed. The
/// recovery backup is SQLCipher-encrypted with the same DEK (B4 — never a plaintext snapshot).
pub fn encrypt_in_place(path: &Path, dek_hex: &str) -> Result<()> {
    let backup = backup_path(path);
    let tmp = tmp_target(path);
    let _ = std::fs::remove_file(&tmp);
    let _ = std::fs::remove_file(&backup);

    // Hold every `PRAGMA key` hex string in a Zeroizing buffer (C6) so the raw key is wiped from the
    // stack as soon as the ATTACH runs.
    let key_expr = zeroize::Zeroizing::new(format!("x'{dek_hex}'"));

    // 1–3: from the plaintext source — checkpoint WAL, KEYED encrypted backup, export to encrypted
    // temp. BOTH the backup and the temp target are SQLCipher-encrypted with the DEK.
    {
        let src = Connection::open(path).map_err(|e| mig_err("open source", e))?;
        let _ = src.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);"); // best-effort fold WAL

        // KEYED recovery backup (B4): export the source into an ATTACHed SQLCipher DB rather than a
        // plaintext `VACUUM INTO`. Encrypted at rest with the same DEK.
        src.execute(
            "ATTACH DATABASE ?1 AS bak KEY ?2",
            rusqlite::params![backup.to_string_lossy().as_ref(), key_expr.as_str()],
        )
        .map_err(|e| mig_err("attach keyed backup", e))?;
        src.query_row("SELECT sqlcipher_export('bak')", [], |_| Ok(()))
            .map_err(|e| mig_err("sqlcipher_export backup", e))?;
        let uv_bak: i64 = src
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap_or(0);
        let _ = src.execute_batch(&format!("PRAGMA bak.user_version = {uv_bak};"));
        src.execute_batch("DETACH DATABASE bak;")
            .map_err(|e| mig_err("detach backup", e))?;

        // Encrypted migration target.
        src.execute(
            "ATTACH DATABASE ?1 AS enc KEY ?2",
            rusqlite::params![tmp.to_string_lossy().as_ref(), key_expr.as_str()],
        )
        .map_err(|e| mig_err("attach encrypted target", e))?;
        src.query_row("SELECT sqlcipher_export('enc')", [], |_| Ok(()))
            .map_err(|e| mig_err("sqlcipher_export", e))?;
        // sqlcipher_export does NOT copy user_version — carry it across.
        let uv: i64 = src
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap_or(0);
        let _ = src.execute_batch(&format!("PRAGMA enc.user_version = {uv};"));
        src.execute_batch("DETACH DATABASE enc;")
            .map_err(|e| mig_err("detach", e))?;
    }

    // 4: verify the encrypted target independently (any failure ⇒ bail, original untouched).
    if let Err(e) = verify_encrypted(path, &tmp, dek_hex) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }

    // 5: atomic swap, then drop the stale plaintext WAL/SHM sidecars.
    std::fs::rename(&tmp, path).map_err(|e| mig_err("atomic rename", e))?;
    for suffix in ["-wal", "-shm"] {
        let mut s = path.as_os_str().to_os_string();
        s.push(suffix);
        let _ = std::fs::remove_file(PathBuf::from(s));
    }
    tracing::info!(target: "migration", "database encrypted; keyed recovery backup at {}", backup.display());
    Ok(())
}

fn table_count(conn: &Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap_or(-1)
}

/// Independently open the encrypted target with the key and prove it is a sound, complete copy:
/// SQLCipher integrity check passes AND every core table's row count matches the source.
fn verify_encrypted(source: &Path, encrypted: &Path, dek_hex: &str) -> Result<()> {
    let src = Connection::open(source).map_err(|e| mig_err("verify open source", e))?;
    let enc = Connection::open(encrypted).map_err(|e| mig_err("verify open encrypted", e))?;
    // C6: wipe the formatted PRAGMA-key hex from the stack right after keying the verify handle.
    let key_expr = zeroize::Zeroizing::new(format!("x'{dek_hex}'"));
    enc.pragma_update(None, "key", key_expr.as_str())
        .map_err(|e| mig_err("verify key", e))?;
    drop(key_expr);

    // Integrity: any returned row signals a problem.
    {
        let mut stmt = enc
            .prepare("PRAGMA cipher_integrity_check")
            .map_err(|e| mig_err("integrity prepare", e))?;
        let mut rows = stmt.query([]).map_err(|e| mig_err("integrity query", e))?;
        if rows.next().map_err(|e| mig_err("integrity next", e))?.is_some() {
            return Err(AppError::Migration(
                "cipher_integrity_check reported problems".into(),
            ));
        }
    }

    // Row counts must match exactly for every core table.
    for t in ["meetings", "segments", "notes"] {
        let s = table_count(&src, t);
        let e = table_count(&enc, t);
        if s < 0 || s != e {
            return Err(AppError::Migration(format!(
                "row-count mismatch in {t}: source={s} encrypted={e}"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "murmur-mig-{tag}-{}-{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&p);
        p
    }

    const KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    /// Build a realistic plaintext DB with fixtures across the core tables.
    fn seed_plaintext(path: &Path) {
        let c = Connection::open(path).unwrap();
        c.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE meetings(id TEXT PRIMARY KEY, started_at TEXT, title TEXT);
             CREATE TABLE segments(meeting_id TEXT, idx INTEGER, text TEXT);
             CREATE TABLE notes(meeting_id TEXT, markdown TEXT);
             INSERT INTO meetings VALUES('m1','2026-07-01','Sync'),('m2','2026-07-02','Review');
             INSERT INTO segments VALUES('m1',0,'hello world'),('m1',1,'second line');
             INSERT INTO notes VALUES('m1','# Note one'),('m2','# Note two');
             PRAGMA user_version=7;",
        )
        .unwrap();
    }

    #[test]
    fn fresh_install_needs_no_migration() {
        let p = tmp_path("fresh");
        assert!(!needs_encryption(&p).unwrap()); // non-existent
    }

    #[test]
    fn round_trips_and_is_idempotent() {
        let p = tmp_path("roundtrip");
        seed_plaintext(&p);
        assert!(needs_encryption(&p).unwrap(), "plaintext should need encryption");

        encrypt_in_place(&p, KEY).unwrap();

        // No longer plaintext; backup exists and is KEYED (encrypted), NOT plaintext (B4).
        assert!(!is_plaintext_sqlite(&p).unwrap(), "should be encrypted now");
        assert!(!needs_encryption(&p).unwrap(), "idempotent: encrypted file → no re-migration");
        let bak = backup_path(&p);
        assert!(bak.exists(), "a recovery backup is produced");
        assert!(
            !is_plaintext_sqlite(&bak).unwrap(),
            "B4: the recovery backup MUST be encrypted at rest, never a plaintext snapshot"
        );
        // The keyed backup decrypts with the SAME DEK and holds the same rows.
        let bak_conn = Connection::open(&bak).unwrap();
        bak_conn.pragma_update(None, "key", format!("x'{KEY}'")).unwrap();
        let bc: i64 = bak_conn
            .query_row("SELECT count(*) FROM meetings", [], |r| r.get(0))
            .unwrap();
        assert_eq!(bc, 2, "keyed backup holds the migrated rows");
        // And the WRONG key cannot read the backup (proves it is genuinely encrypted).
        let bad = Connection::open(&bak).unwrap();
        bad.pragma_update(None, "key", "x'ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff'")
            .unwrap();
        assert!(
            bad.query_row::<i64, _, _>("SELECT count(*) FROM meetings", [], |r| r.get(0)).is_err(),
            "the keyed backup must not be readable without the DEK"
        );

        // Reopen encrypted, verify byte-identical reads + carried user_version.
        let enc = Connection::open(&p).unwrap();
        enc.pragma_update(None, "key", format!("x'{KEY}'")).unwrap();
        let mc: i64 = enc.query_row("SELECT count(*) FROM meetings", [], |r| r.get(0)).unwrap();
        assert_eq!(mc, 2);
        let title: String = enc
            .query_row("SELECT title FROM meetings WHERE id='m2'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(title, "Review");
        let seg: String = enc
            .query_row("SELECT text FROM segments WHERE meeting_id='m1' AND idx=1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(seg, "second line");
        let uv: i64 = enc.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(uv, 7, "user_version carried across export");
    }

    #[test]
    fn wrong_key_fails_closed() {
        let p = tmp_path("wrongkey");
        seed_plaintext(&p);
        encrypt_in_place(&p, KEY).unwrap();

        let bad = Connection::open(&p).unwrap();
        bad.pragma_update(None, "key", "x'ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff'")
            .unwrap();
        let res: rusqlite::Result<i64> =
            bad.query_row("SELECT count(*) FROM meetings", [], |r| r.get(0));
        assert!(res.is_err(), "wrong key must fail, never return an empty/garbage DB");
    }
}
