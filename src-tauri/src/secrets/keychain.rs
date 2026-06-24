use keyring::{Entry, Error as KeyringError};

use crate::error::{AppError, Result};

pub const SERVICE: &str = "com.meetnotes.app";

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
