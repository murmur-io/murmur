use keyring::{Entry, Error as KeyringError};

use crate::error::{AppError, Result};

pub const SERVICE: &str = "com.meetnotes.app";

/// Keychain account holding the SQLCipher database encryption key (DEK).
pub const ACCOUNT_DB_DEK: &str = "murmur_db_dek";

/// Keychain account holding the master KEK that wraps per-folder content keys (Layer 2 lock).
pub const ACCOUNT_MASTER_KEK: &str = "murmur_master_kek";

/// Keychain account holding the optional MCP bearer token.
pub const ACCOUNT_MCP_TOKEN: &str = "murmur_mcp_token";

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

/// Return the master KEK (32 raw bytes) that wraps per-folder content keys, creating +
/// persisting it (as a 64-char hex string) in the Keychain on first use. Mirrors
/// [`get_or_create_db_dek`]: released at launch in this stage (Stage E will move it behind a
/// biometric ACL — do NOT add biometric here). This KEK never touches SQLCipher; it only
/// wraps/unwraps content keys via [`crate::crypto`].
pub fn get_or_create_master_kek() -> Result<[u8; 32]> {
    // Dev-only escape hatch mirroring MURMUR_DEV_DEK, but a SEPARATE env var so the at-rest DEK
    // and the lock KEK can be fixed independently in tests/dev. NEVER compiled into release.
    #[cfg(debug_assertions)]
    if let Ok(dev) = std::env::var("MURMUR_DEV_KEK") {
        if let Some(k) = hex_to_key32(&dev) {
            return Ok(k);
        }
    }
    if let Some(hex) = get_secret(ACCOUNT_MASTER_KEK)? {
        if let Some(k) = hex_to_key32(&hex) {
            return Ok(k);
        }
        return Err(AppError::Secrets("stored master KEK is malformed".into()));
    }
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes)
        .map_err(|e| AppError::Secrets(format!("RNG failed generating KEK: {e}")))?;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    set_secret(ACCOUNT_MASTER_KEK, &hex)?;
    Ok(bytes)
}

/// Return the MCP bearer token (a random 64-char hex string), minting + persisting it in the
/// Keychain on first use. Used to gate MCP `tools/call` when `K_MCP_REQUIRE_TOKEN` is on.
pub fn get_or_create_mcp_token() -> Result<String> {
    if let Some(tok) = get_secret(ACCOUNT_MCP_TOKEN)? {
        if !tok.is_empty() {
            return Ok(tok);
        }
    }
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes)
        .map_err(|e| AppError::Secrets(format!("RNG failed generating MCP token: {e}")))?;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    set_secret(ACCOUNT_MCP_TOKEN, &hex)?;
    Ok(hex)
}

/// Parse a 64-char hex string into a 32-byte key, or `None` if malformed.
fn hex_to_key32(hex: &str) -> Option<[u8; 32]> {
    let hex = hex.trim();
    if hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// Build a keyring entry for `(SERVICE, account)`. `account` is the provider key name,
/// e.g. "anthropic_api_key".
fn entry(account: &str) -> Result<Entry> {
    Entry::new(SERVICE, account).map_err(|e| classify("open keychain entry", e))
}

/// Map a keyring error to a typed [`AppError`]. Runtime access failures — the user clicked
/// "Deny" on the macOS keychain prompt, or the keychain is locked/unreachable — become
/// [`AppError::KeychainDenied`] so startup can show a specific, recoverable message and exit
/// cleanly instead of crashing. Everything else (malformed item, length, ambiguity) stays a
/// generic [`AppError::Secrets`]. `NoEntry` is handled by callers (it means "not set", not error).
/// The message carries only the platform error text — never the secret value — so it is safe to
/// log under the no-PII rule.
fn classify(ctx: impl std::fmt::Display, e: KeyringError) -> AppError {
    match e {
        KeyringError::PlatformFailure(_) | KeyringError::NoStorageAccess(_) => {
            AppError::KeychainDenied(format!("{ctx}: {e}"))
        }
        other => AppError::Secrets(format!("{ctx}: {other}")),
    }
}

/// Store/replace a secret in the macOS Keychain.
pub fn set_secret(account: &str, secret: &str) -> Result<()> {
    entry(account)?
        .set_password(secret)
        .map_err(|e| classify("set secret", e))
}

/// Read a secret from the Keychain. `Ok(None)` if no entry exists (not an error).
pub fn get_secret(account: &str) -> Result<Option<String>> {
    match entry(account)?.get_password() {
        Ok(secret) => Ok(Some(secret)),
        Err(KeyringError::NoEntry) => Ok(None),
        Err(e) => Err(classify("get secret", e)),
    }
}

/// Delete a secret. `Ok(())` if it was already absent (idempotent).
pub fn delete_secret(account: &str) -> Result<()> {
    match entry(account)?.delete_credential() {
        Ok(()) => Ok(()),
        Err(KeyringError::NoEntry) => Ok(()),
        Err(e) => Err(classify("delete secret", e)),
    }
}

#[cfg(test)]
mod tests {
    use super::hex_to_key32;

    #[test]
    fn hex_to_key32_round_trips() {
        let bytes: [u8; 32] = std::array::from_fn(|i| i as u8);
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex_to_key32(&hex), Some(bytes));
        // Trims surrounding whitespace (env-var convenience).
        assert_eq!(hex_to_key32(&format!("  {hex}\n")), Some(bytes));
    }

    #[test]
    fn hex_to_key32_rejects_malformed() {
        assert_eq!(hex_to_key32("tooshort"), None);
        assert_eq!(hex_to_key32(&"z".repeat(64)), None);
        assert_eq!(hex_to_key32(&"a".repeat(63)), None);
    }
}
