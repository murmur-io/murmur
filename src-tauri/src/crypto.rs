//! Application-layer AES-256-GCM for the per-folder lock (Layer 2, distinct from the whole-DB
//! SQLCipher layer). A locked folder's note markdown is encrypted under a per-folder content key
//! (CK); each CK is wrapped under a master KEK that is released only by biometric. Cell format:
//! `nonce(12) || ciphertext || tag(16)`, stored as a BLOB.
//!
//! ## Associated data (AAD) context-binding — B7/B8
//!
//! Every blob is now AEAD-bound to its STORAGE CONTEXT via AES-GCM additional authenticated data
//! (a wrapped CK to its `folder_id`, a content blob to `folder_id|meeting_id|provider_id|…`, audio
//! to `meeting_id|folder_id`). AAD is authenticated-but-not-encrypted: it never appears on disk and
//! does NOT change the cell format (`nonce || ct+tag` is byte-identical in size) — it only changes
//! the GCM tag. Binding the context defeats a "swap a ciphertext from folder A into folder B" or
//! "replay a different meeting's audio" attack: decryption under the WRONG context fails the tag
//! check and returns [`AppError::Locked`].
//!
//! ## BACKWARD-COMPATIBILITY — MANDATORY (never brick existing folders)
//!
//! Existing locked folders' wrapped-keys / content blobs / audio `.enc` were written BEFORE AAD
//! existed (empty AAD). Because an AAD-bound blob and a legacy no-AAD blob are byte-indistinguishable
//! on disk, [`decrypt`] tries the supplied AAD FIRST and, only if that fails AND the AAD is non-empty,
//! falls back to empty AAD (the legacy form). A successful legacy decrypt is reported to the caller
//! (`AadUsed::Legacy`) so it can RE-BIND the blob (re-encrypt with the real AAD) on the next write.
//! This makes the migration lazy and lossless: an old folder still unlocks, and re-binds itself the
//! first time it is re-sealed. A blob whose context was tampered/swapped fails BOTH the bound and the
//! legacy attempt (the legacy attempt only succeeds for genuinely-pre-AAD blobs, never for a blob
//! bound to a DIFFERENT context) → fails closed.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};

use crate::error::{AppError, Result};

const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;

/// Which AAD form successfully decrypted a blob — so callers can lazily RE-BIND legacy (pre-AAD)
/// blobs to their real context on the next write (see module docs, B7/B8 backward-compat).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AadUsed {
    /// Decrypted with the caller-supplied context AAD (already bound — nothing to migrate).
    Bound,
    /// Decrypted with empty AAD — a legacy pre-AAD blob. Caller SHOULD re-bind on next write.
    Legacy,
}

/// Encrypt `plaintext` under a 32-byte key, binding `aad` as AES-GCM additional authenticated data
/// → `nonce(12) || ciphertext+tag`. `aad` is authenticated but NOT stored (it is reconstructed from
/// the storage context at decrypt time). Pass `&[]` for no binding (legacy form).
pub fn encrypt(key: &[u8; 32], plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let mut nonce_bytes = [0u8; NONCE_LEN];
    getrandom::getrandom(&mut nonce_bytes)
        .map_err(|e| AppError::Storage(format!("nonce RNG: {e}")))?;
    let ct = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), Payload { msg: plaintext, aad })
        .map_err(|e| AppError::Storage(format!("AES-GCM encrypt: {e}")))?;
    let mut out = Vec::with_capacity(NONCE_LEN + ct.len());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ct);
    Ok(out)
}

/// Decrypt a `nonce(12) || ciphertext+tag` blob under a 32-byte key, expecting it to be AEAD-bound
/// to `aad`. Tries the supplied `aad` first; on failure with a NON-EMPTY `aad`, retries with empty
/// AAD to transparently read LEGACY pre-AAD blobs (see module docs). Returns the plaintext plus an
/// [`AadUsed`] flag the caller can use to re-bind a legacy blob. A wrong key, tampered ciphertext, or
/// a blob bound to a DIFFERENT context fails closed (`AppError::Locked`).
pub fn decrypt_with_aad(key: &[u8; 32], blob: &[u8], aad: &[u8]) -> Result<(Vec<u8>, AadUsed)> {
    if blob.len() < NONCE_LEN + TAG_LEN {
        return Err(AppError::Storage("ciphertext too short".into()));
    }
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let (nonce_bytes, ct) = blob.split_at(NONCE_LEN);
    let nonce = Nonce::from_slice(nonce_bytes);

    // Primary: the caller's real context AAD.
    if let Ok(pt) = cipher.decrypt(nonce, Payload { msg: ct, aad }) {
        return Ok((pt, AadUsed::Bound));
    }
    // Backward-compat: a genuinely pre-AAD blob was written with empty AAD. Only attempt the legacy
    // form when the caller asked for a non-empty AAD (otherwise this is identical to the primary
    // attempt above and we would just be repeating a failure). A blob bound to a DIFFERENT non-empty
    // context never matches empty AAD, so this does NOT weaken context-swap detection.
    if !aad.is_empty() {
        if let Ok(pt) = cipher.decrypt(nonce, Payload { msg: ct, aad: &[] }) {
            return Ok((pt, AadUsed::Legacy));
        }
    }
    Err(AppError::Locked(
        "decryption failed (wrong key, tampered data, or wrong storage context)".into(),
    ))
}

/// Decrypt a blob expecting `aad` (with the same legacy fallback as [`decrypt_with_aad`]), discarding
/// the [`AadUsed`] flag. Use this at call-sites that do not re-bind. A wrong key / tampered
/// ciphertext / wrong context fails closed (`AppError::Locked`).
pub fn decrypt(key: &[u8; 32], blob: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
    decrypt_with_aad(key, blob, aad).map(|(pt, _)| pt)
}

/// A random 32-byte key (folder content key or master KEK).
pub fn random_key() -> Result<[u8; 32]> {
    let mut k = [0u8; 32];
    getrandom::getrandom(&mut k).map_err(|e| AppError::Storage(format!("key RNG: {e}")))?;
    Ok(k)
}

/// Encrypt the file at `src` under `key` (binding `aad` as context) into `dest` (the
/// `nonce(12) || ciphertext+tag` blob), then VERIFY the ciphertext decrypts back byte-identical to
/// the source BEFORE returning. This mirrors `seal_note`'s verify-before-destroy: the caller removes
/// the plaintext only after a successful return, so a corrupt write can never lose audio. The
/// plaintext WAV (separate file at `meetings.audio_path`, NOT in the SQLCipher DB) is encrypted at
/// rest for locked folders. `aad` should be the audio context (`meeting_id|folder_id`); `&[]` for
/// the legacy unbound form.
pub fn encrypt_file(
    key: &[u8; 32],
    src: &std::path::Path,
    dest: &std::path::Path,
    aad: &[u8],
) -> Result<()> {
    let plaintext = std::fs::read(src)
        .map_err(|e| AppError::Storage(format!("read audio for encrypt: {e}")))?;
    let blob = encrypt(key, &plaintext, aad)?;
    // Verify the blob decrypts back byte-identical (under the SAME aad) BEFORE we ever write it (and
    // before the caller destroys the plaintext). A tampered/short blob fails closed here.
    let check = decrypt(key, &blob, aad)?;
    if check != plaintext {
        return Err(AppError::Storage(
            "audio seal verification failed (decrypted blob mismatch)".into(),
        ));
    }
    std::fs::write(dest, &blob)
        .map_err(|e| AppError::Storage(format!("write encrypted audio: {e}")))?;
    Ok(())
}

/// Decrypt the encrypted-WAV file at `src` (a `nonce(12) || ciphertext+tag` blob) under `key`,
/// expecting context `aad` (legacy empty-AAD fallback applies, see [`decrypt_with_aad`]), into the
/// plaintext WAV at `dest`. Used to materialize a playable WAV for the session on unlock, and to
/// permanently restore the plaintext on remove-lock.
pub fn decrypt_file(
    key: &[u8; 32],
    src: &std::path::Path,
    dest: &std::path::Path,
    aad: &[u8],
) -> Result<()> {
    let blob = std::fs::read(src)
        .map_err(|e| AppError::Storage(format!("read encrypted audio: {e}")))?;
    let plaintext = decrypt(key, &blob, aad)?;
    std::fs::write(dest, &plaintext)
        .map_err(|e| AppError::Storage(format!("write decrypted audio: {e}")))?;
    Ok(())
}

/// Decrypt an encrypted-WAV file at `src` trying a LADDER of candidate AADs in priority order,
/// writing the plaintext to `dest` on the first candidate that succeeds. Each candidate carries the
/// same empty-AAD legacy fallback as [`decrypt_with_aad`].
///
/// This is the audio backward-compatibility ladder for the stream-role AAD hardening. The three
/// per-meeting audio files (playback WAV + mic/sys masters) are now sealed with a ROLE-bound AAD so
/// they can't be swapped for one another. But a master/playback `.enc` sealed BEFORE the role existed
/// carries the role-LESS `aad_audio(meeting,folder)` — a NON-EMPTY AAD that a role-bound decrypt
/// alone would miss AND the empty-AAD fallback would also miss → DATA LOSS. Passing the role-less AAD
/// as a lower rung makes the migration lossless; the file re-binds to the role form on its next seal.
/// A swapped file (mic ciphertext presented as sys) matches NEITHER rung and fails closed. Fails
/// closed (`AppError::Locked`) only if NO candidate (and no empty fallback) matches.
pub fn decrypt_file_multi(
    key: &[u8; 32],
    src: &std::path::Path,
    dest: &std::path::Path,
    aads: &[&[u8]],
) -> Result<()> {
    let blob = std::fs::read(src)
        .map_err(|e| AppError::Storage(format!("read encrypted audio: {e}")))?;
    for aad in aads {
        if let Ok(pt) = decrypt(key, &blob, aad) {
            std::fs::write(dest, &pt)
                .map_err(|e| AppError::Storage(format!("write decrypted audio: {e}")))?;
            return Ok(());
        }
    }
    Err(AppError::Locked(
        "audio decryption failed (wrong key, tampered data, or wrong storage context)".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_hides_plaintext() {
        let k = random_key().unwrap();
        let pt = "locked note markdown 🔒".as_bytes();
        let blob = encrypt(&k, pt, b"folder-42").unwrap();
        assert_ne!(&blob[NONCE_LEN..], pt, "ciphertext must differ from plaintext");
        assert_eq!(decrypt(&k, &blob, b"folder-42").unwrap(), pt);
    }

    #[test]
    fn wrong_key_fails_closed() {
        let blob = encrypt(&random_key().unwrap(), b"secret", b"ctx").unwrap();
        assert!(decrypt(&random_key().unwrap(), &blob, b"ctx").is_err());
    }

    #[test]
    fn kek_wraps_and_unwraps_a_content_key() {
        let kek = random_key().unwrap();
        let ck = random_key().unwrap();
        let wrapped = encrypt(&kek, &ck, b"folder-id").unwrap();
        assert_eq!(decrypt(&kek, &wrapped, b"folder-id").unwrap(), ck);
    }

    // ── B7/B8 AAD context-binding + backward-compat regression ─────────────────────────────────

    /// A pre-AAD blob (written with EMPTY aad — the legacy form) must STILL decrypt when the caller
    /// now supplies a real context AAD — and report `AadUsed::Legacy` so the caller re-binds. This
    /// is the "never brick existing folders" guarantee.
    #[test]
    fn legacy_pre_aad_blob_still_decrypts_under_new_context() {
        let k = random_key().unwrap();
        let pt = b"existing locked folder note written before AAD existed";

        // LEGACY blob: encrypted with empty AAD, exactly as the shipped v0.3.2 code wrote it.
        let legacy_blob = encrypt(&k, pt, b"").unwrap();

        // New code reads it WITH a context AAD → must succeed via the empty-AAD fallback…
        let (out, used) = decrypt_with_aad(&k, &legacy_blob, b"folder-7|meeting-1").unwrap();
        assert_eq!(out, pt, "a pre-AAD blob must still decrypt (no bricking)");
        assert_eq!(used, AadUsed::Legacy, "must report it read a legacy blob so caller re-binds");

        // …and the plain `decrypt` wrapper agrees.
        assert_eq!(decrypt(&k, &legacy_blob, b"folder-7|meeting-1").unwrap(), pt);
    }

    /// A blob bound to context A must FAIL to decrypt when presented as context B (a swapped/replayed
    /// ciphertext). The legacy empty-AAD fallback must NOT rescue it (it is bound, not legacy).
    #[test]
    fn swapped_context_blob_fails_closed() {
        let k = random_key().unwrap();
        let pt = b"secret bound to folder A";
        let blob_a = encrypt(&k, pt, b"folder-A").unwrap();

        // Correct context → ok.
        assert_eq!(decrypt(&k, &blob_a, b"folder-A").unwrap(), pt);
        // Wrong context (attacker moved the ciphertext into folder B) → fails closed.
        let res = decrypt(&k, &blob_a, b"folder-B");
        assert!(res.is_err(), "a blob bound to folder A must not decrypt as folder B");
        assert!(
            matches!(res, Err(AppError::Locked(_))),
            "context mismatch must fail closed with Locked, got {res:?}"
        );
        // And the AadUsed-returning form also rejects it (legacy fallback must not rescue a BOUND
        // blob under the wrong context).
        assert!(decrypt_with_aad(&k, &blob_a, b"folder-B").is_err());
    }

    /// Re-binding: a legacy blob, once re-encrypted with its real context, decrypts as `Bound` and
    /// then refuses the wrong context — proving the lazy migration actually upgrades protection.
    #[test]
    fn rebinding_a_legacy_blob_upgrades_to_bound() {
        let k = random_key().unwrap();
        let pt = b"note to be migrated";
        let legacy = encrypt(&k, pt, b"").unwrap();

        // Read legacy → decide to re-bind to the real context.
        let (recovered, used) = decrypt_with_aad(&k, &legacy, b"ctx-real").unwrap();
        assert_eq!(used, AadUsed::Legacy);
        let rebound = encrypt(&k, &recovered, b"ctx-real").unwrap();

        // Now it is bound: correct context reads as Bound, wrong context fails.
        let (out2, used2) = decrypt_with_aad(&k, &rebound, b"ctx-real").unwrap();
        assert_eq!(out2, pt);
        assert_eq!(used2, AadUsed::Bound, "after re-bind the blob is context-bound");
        assert!(decrypt(&k, &rebound, b"ctx-wrong").is_err(), "re-bound blob rejects wrong context");
    }

    fn temp_path(label: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "meetnotes-crypto-test-{}-{}-{}",
            label,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        p
    }

    #[test]
    fn audio_encrypt_decrypt_round_trips_byte_identical() {
        // Synthesize a small "WAV" payload (bytes are opaque to the crypto layer — a real WAV
        // header would be identical content). Encrypt → .enc, remove plaintext, decrypt → assert
        // byte-identical, and assert the ciphertext does NOT contain the plaintext.
        let key = random_key().unwrap();
        let wav = temp_path("audio.wav");
        let enc = temp_path("audio.wav.enc");
        let restored = temp_path("audio-restored.wav");
        let payload: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&wav, &payload).unwrap();

        // ENCRYPT (verify-before-destroy happens inside) → then simulate the lock removing the
        // plaintext WAV. Bind the audio context AAD (meeting|folder).
        let aad = b"meeting-x|folder-y";
        encrypt_file(&key, &wav, &enc, aad).unwrap();
        let blob = std::fs::read(&enc).unwrap();
        assert!(
            !contains(&blob, &payload),
            "ciphertext must not leak the plaintext audio"
        );
        std::fs::remove_file(&wav).unwrap();
        assert!(!wav.exists(), "plaintext WAV removed while sealed");

        // DECRYPT for the session → byte-identical (same AAD).
        decrypt_file(&key, &enc, &restored, aad).unwrap();
        assert_eq!(std::fs::read(&restored).unwrap(), payload, "audio round-trips byte-identical");

        // Wrong key fails closed.
        assert!(decrypt_file(&random_key().unwrap(), &enc, &restored, aad).is_err());
        // Wrong AAD (audio replayed into a different meeting/folder) fails closed.
        assert!(decrypt_file(&key, &enc, &restored, b"meeting-OTHER|folder-y").is_err());

        let _ = std::fs::remove_file(&enc);
        let _ = std::fs::remove_file(&restored);
    }

    /// Stream-role AAD backward-compat ladder ([`decrypt_file_multi`]): a `.enc` sealed under ANY of
    /// the three historical/new forms must still decrypt, while a swapped/wrong-context file fails
    /// closed. The ladder is `[role-bound, role-less]` (each rung also tries empty-AAD internally):
    ///   - a NEW role-bound master (`…|stream=mic`) decrypts on rung 1;
    ///   - a LEGACY role-LESS master (`…folder=…`, NON-empty) decrypts on rung 2 (the migration that
    ///     would otherwise be DATA LOSS);
    ///   - a PRE-AAD master (empty AAD) decrypts via the empty fallback built into rung 1;
    ///   - a mic file presented under the SYS ladder fails closed (no rung matches).
    #[test]
    fn audio_role_aad_ladder_reads_all_legacy_forms_and_rejects_swaps() {
        let key = random_key().unwrap();
        let payload: Vec<u8> = (0..2048u32).map(|i| (i % 251) as u8).collect();

        let role_mic: &[u8] = b"murmur:audio:v1|meeting=m|folder=f|stream=mic";
        let role_sys: &[u8] = b"murmur:audio:v1|meeting=m|folder=f|stream=sys";
        let role_less: &[u8] = b"murmur:audio:v1|meeting=m|folder=f";
        // The mic ladder a caller would use: role-bound first, then the role-less legacy form.
        let mic_ladder: &[&[u8]] = &[role_mic, role_less];
        let sys_ladder: &[&[u8]] = &[role_sys, role_less];

        let src = temp_path("ladder-src.wav");
        let enc = temp_path("ladder.wav.enc");
        let out = temp_path("ladder-out.wav");
        std::fs::write(&src, &payload).unwrap();

        // (1) NEW role-bound mic master → decrypts on rung 1 of the mic ladder.
        encrypt_file(&key, &src, &enc, role_mic).unwrap();
        decrypt_file_multi(&key, &enc, &out, mic_ladder).unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), payload, "role-bound master round-trips");
        // …and a SYS ladder must NOT read a MIC file (swap rejected, fails closed).
        assert!(
            decrypt_file_multi(&key, &enc, &out, sys_ladder).is_err(),
            "a mic master must not decrypt under the sys ladder (no swaps within a meeting)"
        );

        // (2) LEGACY role-LESS master (sealed before the stream role existed) → rung 2 reads it.
        encrypt_file(&key, &src, &enc, role_less).unwrap();
        decrypt_file_multi(&key, &enc, &out, mic_ladder).unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), payload, "role-less legacy master still decrypts");

        // (3) PRE-AAD master (empty AAD) → the empty fallback inside rung 1 reads it.
        encrypt_file(&key, &src, &enc, b"").unwrap();
        decrypt_file_multi(&key, &enc, &out, mic_ladder).unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), payload, "pre-AAD master still decrypts");

        // (4) Wrong KEY fails closed regardless of ladder.
        assert!(decrypt_file_multi(&random_key().unwrap(), &enc, &out, mic_ladder).is_err());

        let _ = std::fs::remove_file(&src);
        let _ = std::fs::remove_file(&enc);
        let _ = std::fs::remove_file(&out);
    }

    /// Naive subslice search for the leak assertion.
    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        if needle.is_empty() || needle.len() > haystack.len() {
            return false;
        }
        haystack.windows(needle.len()).any(|w| w == needle)
    }
}
