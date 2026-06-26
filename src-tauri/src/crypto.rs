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
}
