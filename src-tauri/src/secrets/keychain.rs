use keyring::{Entry, Error as KeyringError};

use crate::error::{AppError, Result};

pub const SERVICE: &str = "com.meetnotes.app";

/// Keychain account holding the SQLCipher database encryption key (DEK).
pub const ACCOUNT_DB_DEK: &str = "murmur_db_dek";

/// Return the SQLCipher DEK as a 64-char hex string (32 random bytes), creating + persisting it
/// in the Keychain on first use. Released at launch with no biometric prompt — this layer
/// protects against database FILE theft, not against an attacker on the unlocked machine
/// (per-folder biometric locking, added later, covers that). Hex form ⇒ SQLCipher treats it as a
/// raw key blob (`PRAGMA key = x'…'`) with no KDF.
pub fn get_or_create_db_dek() -> Result<String> {
    // Dev-only escape hatch: a fixed DEK via `MURMUR_DEV_DEK` (64 hex chars) avoids a macOS
    // Keychain prompt on every rebuild — each recompiled dev binary has a new signature, so the
    // OS re-prompts for access to the existing item. NEVER compiled into release builds.
    #[cfg(debug_assertions)]
    if let Ok(dev) = std::env::var("MURMUR_DEV_DEK") {
        if dev.len() == 64 && dev.chars().all(|c| c.is_ascii_hexdigit()) {
            return Ok(dev);
        }
    }
    if let Some(dek) = get_secret(ACCOUNT_DB_DEK)? {
        return Ok(dek);
    }
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes)
        .map_err(|e| AppError::Secrets(format!("RNG failed generating DEK: {e}")))?;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    set_secret(ACCOUNT_DB_DEK, &hex)?;
    Ok(hex)
}

/// Build a keyring entry for `(SERVICE, account)`. `account` is the provider key name,
/// e.g. "anthropic_api_key".
fn entry(account: &str) -> Result<Entry> {
    Entry::new(SERVICE, account)
        .map_err(|e| AppError::Secrets(format!("failed to open keychain entry: {e}")))
}

/// Store/replace a secret in the macOS Keychain.
pub fn set_secret(account: &str, secret: &str) -> Result<()> {
    entry(account)?
        .set_password(secret)
        .map_err(|e| AppError::Secrets(format!("failed to set secret: {e}")))
}

/// Read a secret from the Keychain. `Ok(None)` if no entry exists (not an error).
pub fn get_secret(account: &str) -> Result<Option<String>> {
    match entry(account)?.get_password() {
        Ok(secret) => Ok(Some(secret)),
        Err(KeyringError::NoEntry) => Ok(None),
        Err(e) => Err(AppError::Secrets(format!("failed to get secret: {e}"))),
    }
}

/// Delete a secret. `Ok(())` if it was already absent (idempotent).
pub fn delete_secret(account: &str) -> Result<()> {
    match entry(account)?.delete_credential() {
        Ok(()) => Ok(()),
        Err(KeyringError::NoEntry) => Ok(()),
        Err(e) => Err(AppError::Secrets(format!("failed to delete secret: {e}"))),
    }
}
