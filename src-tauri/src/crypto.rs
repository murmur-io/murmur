//! Application-layer AES-256-GCM for the per-folder lock (Layer 2, distinct from the whole-DB
//! SQLCipher layer). A locked folder's note markdown is encrypted under a per-folder content key
//! (CK); each CK is wrapped under a master KEK that is released only by biometric. Cell format:
//! `nonce(12) || ciphertext || tag(16)`, stored as a BLOB.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};

use crate::error::{AppError, Result};

const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;

/// Encrypt `plaintext` under a 32-byte key → `nonce(12) || ciphertext+tag`.
pub fn encrypt(key: &[u8; 32], plaintext: &[u8]) -> Result<Vec<u8>> {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let mut nonce_bytes = [0u8; NONCE_LEN];
    getrandom::getrandom(&mut nonce_bytes)
        .map_err(|e| AppError::Storage(format!("nonce RNG: {e}")))?;
    let ct = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), plaintext)
        .map_err(|e| AppError::Storage(format!("AES-GCM encrypt: {e}")))?;
    let mut out = Vec::with_capacity(NONCE_LEN + ct.len());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ct);
    Ok(out)
}

/// Decrypt a `nonce(12) || ciphertext+tag` blob under a 32-byte key. A wrong key or tampered
/// ciphertext fails closed (`AppError::Locked`).
pub fn decrypt(key: &[u8; 32], blob: &[u8]) -> Result<Vec<u8>> {
    if blob.len() < NONCE_LEN + TAG_LEN {
        return Err(AppError::Storage("ciphertext too short".into()));
    }
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let (nonce_bytes, ct) = blob.split_at(NONCE_LEN);
    cipher
        .decrypt(Nonce::from_slice(nonce_bytes), ct)
        .map_err(|_| AppError::Locked("decryption failed (wrong key or tampered data)".into()))
}

/// A random 32-byte key (folder content key or master KEK).
pub fn random_key() -> Result<[u8; 32]> {
    let mut k = [0u8; 32];
    getrandom::getrandom(&mut k).map_err(|e| AppError::Storage(format!("key RNG: {e}")))?;
    Ok(k)
}

/// Encrypt the file at `src` under `key` into `dest` (the `nonce(12) || ciphertext+tag` blob),
/// then VERIFY the ciphertext decrypts back byte-identical to the source BEFORE returning. This
/// mirrors `seal_note`'s verify-before-destroy: the caller removes the plaintext only after a
/// successful return, so a corrupt write can never lose audio. The plaintext WAV (separate file
/// at `meetings.audio_path`, NOT in the SQLCipher DB) is encrypted at rest for locked folders.
pub fn encrypt_file(key: &[u8; 32], src: &std::path::Path, dest: &std::path::Path) -> Result<()> {
    let plaintext = std::fs::read(src)
        .map_err(|e| AppError::Storage(format!("read audio for encrypt: {e}")))?;
    let blob = encrypt(key, &plaintext)?;
    // Verify the blob decrypts back byte-identical BEFORE we ever write it (and before the caller
    // destroys the plaintext). A tampered/short blob fails closed here.
    let check = decrypt(key, &blob)?;
    if check != plaintext {
        return Err(AppError::Storage(
            "audio seal verification failed (decrypted blob mismatch)".into(),
        ));
    }
    std::fs::write(dest, &blob)
        .map_err(|e| AppError::Storage(format!("write encrypted audio: {e}")))?;
    Ok(())
}

/// Decrypt the encrypted-WAV file at `src` (a `nonce(12) || ciphertext+tag` blob) under `key`
/// into the plaintext WAV at `dest`. Used to materialize a playable WAV for the session on
/// unlock, and to permanently restore the plaintext on remove-lock.
pub fn decrypt_file(key: &[u8; 32], src: &std::path::Path, dest: &std::path::Path) -> Result<()> {
    let blob = std::fs::read(src)
        .map_err(|e| AppError::Storage(format!("read encrypted audio: {e}")))?;
    let plaintext = decrypt(key, &blob)?;
    std::fs::write(dest, &plaintext)
        .map_err(|e| AppError::Storage(format!("write decrypted audio: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_hides_plaintext() {
        let k = random_key().unwrap();
        let pt = "locked note markdown 🔒".as_bytes();
        let blob = encrypt(&k, pt).unwrap();
        assert_ne!(&blob[NONCE_LEN..], pt, "ciphertext must differ from plaintext");
        assert_eq!(decrypt(&k, &blob).unwrap(), pt);
    }

    #[test]
    fn wrong_key_fails_closed() {
        let blob = encrypt(&random_key().unwrap(), b"secret").unwrap();
        assert!(decrypt(&random_key().unwrap(), &blob).is_err());
    }

    #[test]
    fn kek_wraps_and_unwraps_a_content_key() {
        let kek = random_key().unwrap();
        let ck = random_key().unwrap();
        let wrapped = encrypt(&kek, &ck).unwrap();
        assert_eq!(decrypt(&kek, &wrapped).unwrap(), ck);
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
        // plaintext WAV.
        encrypt_file(&key, &wav, &enc).unwrap();
        let blob = std::fs::read(&enc).unwrap();
        assert!(
            !contains(&blob, &payload),
            "ciphertext must not leak the plaintext audio"
        );
        std::fs::remove_file(&wav).unwrap();
        assert!(!wav.exists(), "plaintext WAV removed while sealed");

        // DECRYPT for the session → byte-identical.
        decrypt_file(&key, &enc, &restored).unwrap();
        assert_eq!(std::fs::read(&restored).unwrap(), payload, "audio round-trips byte-identical");

        // Wrong key fails closed.
        assert!(decrypt_file(&random_key().unwrap(), &enc, &restored).is_err());

        let _ = std::fs::remove_file(&enc);
        let _ = std::fs::remove_file(&restored);
    }

    /// Naive subslice search for the leak assertion.
    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        if needle.is_empty() || needle.len() > haystack.len() {
            return false;
        }
        haystack.windows(needle.len()).any(|w| w == needle)
    }
}
