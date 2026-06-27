use keyring::{Entry, Error as KeyringError};

use crate::error::{AppError, Result};

pub const SERVICE: &str = "com.meetnotes.app";

/// Keychain account holding the SQLCipher database encryption key (DEK).
pub const ACCOUNT_DB_DEK: &str = "murmur_db_dek";

/// Keychain account holding the master KEK that wraps per-folder content keys (Layer 2 lock).
///
/// v0.3.2: this account now names the BIOMETRIC-GATED item (stored via the macOS Security framework
/// with a `kSecAttrAccessControl` requiring user presence). The legacy PLAIN keyring item lived
/// under the SAME account name; the one-time migration (see [`migrate_or_create_kek`]) reads that
/// plain item, re-stores the identical bytes as the biometric-gated item, then deletes the plain one
/// — so the account string is stable across the migration and the KEK value is preserved byte-for-
/// byte (existing locked folders still unwrap).
pub const ACCOUNT_MASTER_KEK: &str = "murmur_master_kek";

/// Keychain account holding the optional MCP bearer token.
pub const ACCOUNT_MCP_TOKEN: &str = "murmur_mcp_token";

/// Keychain account holding the Anthropic API key (mirrors `summarize::ANTHROPIC_KEY_ACCOUNT` and
/// the `commands` constant). Named here so the data-protection routing recognizes it as a known
/// fixed account (no per-call string leak in [`leak_account`]).
pub const ACCOUNT_ANTHROPIC_KEY: &str = "anthropic_api_key";

/// Default reason string shown on the Touch ID / passcode sheet when releasing the master KEK.
/// Callers may override per call-site (e.g. "Unlock this folder").
pub const KEK_DEFAULT_REASON: &str = "Unlock this folder";

/// Return the SQLCipher DEK as a 64-char hex string (32 random bytes), creating + persisting it
/// in the Keychain on first use. Released at launch with no biometric prompt — this layer
/// protects against database FILE theft, not against an attacker on the unlocked machine
/// (per-folder biometric locking, added later, covers that). Hex form ⇒ SQLCipher treats it as a
/// raw key blob (`PRAGMA key = x'…'`) with no KDF.
///
/// DELIBERATELY a PLAIN keychain item (NOT biometric-gated): it is read once at every app launch in
/// [`crate::state::AppState::init`], so gating it behind Touch ID would force a biometric prompt on
/// every cold start. Only the master KEK (folder-unlock, on-demand) is biometric-gated in v0.3.2.
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
    // Mint a fresh DEK. Zeroize the raw byte buffer once the hex form is derived (the hex is the
    // returned secret — the caller is responsible for its lifetime, e.g. wrapping in Zeroizing at
    // the PRAGMA-key site in db.rs/migration.rs, C6).
    let mut bytes = zeroize::Zeroizing::new([0u8; 32]);
    getrandom::getrandom(&mut *bytes)
        .map_err(|e| AppError::Secrets(format!("RNG failed generating DEK: {e}")))?;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    set_secret(ACCOUNT_DB_DEK, &hex)?;
    Ok(hex)
}

/// Return the master KEK (32 raw bytes) that wraps per-folder content keys.
///
/// v0.3.2 — BIOMETRIC-GATED. The KEK lives in a generic-password Keychain item protected by a
/// `kSecAttrAccessControl` requiring **user presence** (Touch ID, with a device-passcode fallback so
/// a Mac without Touch ID is never locked out; accessibility
/// `kSecAttrAccessibleWhenUnlockedThisDeviceOnly`). Reading that item makes macOS present the Touch
/// ID sheet directly — with the supplied `reason` string — and return the key on success. THAT
/// single sheet IS the unlock auth: the caller must NOT also call [`crate::biometric::authenticate`]
/// (doing so would double-prompt). `reason` is shown verbatim on the sheet (e.g. "Unlock this
/// folder").
///
/// On first use this also runs a one-time, idempotent, value-preserving migration from the legacy
/// PLAIN item (see [`migrate_or_create_kek`]). This KEK never touches SQLCipher; it only
/// wraps/unwraps content keys via [`crate::crypto`].
///
/// Uses [`KEK_DEFAULT_REASON`] on the Touch ID sheet. Call
/// [`get_or_create_master_kek_with_reason`] to override the prompt text per call-site. This zero-arg
/// form is kept so existing call-sites (e.g. `remove_lock`) compile unchanged.
pub fn get_or_create_master_kek() -> Result<[u8; 32]> {
    get_or_create_master_kek_with_reason(KEK_DEFAULT_REASON)
}

/// As [`get_or_create_master_kek`], but the `reason` string is shown verbatim on the Touch ID /
/// passcode sheet that the biometric-gated keychain read presents (e.g. "Unlock this folder").
/// THAT sheet is the unlock auth — do NOT also call [`crate::biometric::authenticate`].
pub fn get_or_create_master_kek_with_reason(reason: &str) -> Result<[u8; 32]> {
    // Dev-only escape hatch mirroring MURMUR_DEV_DEK, but a SEPARATE env var so the at-rest DEK
    // and the lock KEK can be fixed independently in tests/dev. Returns FIRST so dev needs no Touch
    // ID and no Keychain access at all. NEVER compiled into release.
    #[cfg(debug_assertions)]
    if let Ok(dev) = std::env::var("MURMUR_DEV_KEK") {
        if let Some(k) = hex_to_key32(&dev) {
            return Ok(k);
        }
    }

    #[cfg(target_os = "macos")]
    {
        migrate_or_create_kek(&MacKekStore, reason)
    }

    // Non-macOS hosts have no Security-framework access control. Fall back to a PLAIN keyring item
    // (same shape as the legacy path) so `cargo build`/`test` on a CI Linux box still works. There
    // is no Touch ID off-platform; this is dev/CI convenience only — the product ships macOS-only.
    #[cfg(not(target_os = "macos"))]
    {
        let _ = reason;
        if let Some(hex) = get_secret(ACCOUNT_MASTER_KEK)? {
            if let Some(k) = hex_to_key32(&hex) {
                return Ok(k);
            }
            return Err(AppError::Secrets("stored master KEK is malformed".into()));
        }
        let bytes = crate::crypto::random_key()?;
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        set_secret(ACCOUNT_MASTER_KEK, &hex)?;
        Ok(bytes)
    }
}

// ───────────────────────────── biometric-gated KEK: storage seam ─────────────────────────────

/// A thin storage seam over the master-KEK keychain operations, so the value-preserving migration
/// ([`migrate_or_create_kek`]) can be unit-tested against an in-memory fake WITHOUT a live keychain
/// or a Touch ID prompt (a biometric read can't run headlessly — there is no Touch ID in CI).
///
/// The four operations are exactly what the migration needs:
/// - [`KekStore::biometric_exists`] — does the biometric-gated item exist? MUST NOT prompt (uses
///   `kSecUseAuthenticationUISkip` in the real impl) so it is a cheap, side-effect-free probe.
/// - [`KekStore::read_plain`] — read the legacy PLAIN item's raw value (never prompts).
/// - [`KekStore::write_biometric`] — create/replace the biometric-gated item with the given bytes.
/// - [`KekStore::read_biometric`] — read the biometric-gated item's value (prompts Touch ID on real
///   macOS; the reason string is shown on the sheet). This is the actual unlock auth.
/// - [`KekStore::delete_plain`] — delete the legacy plain item (idempotent).
trait KekStore {
    fn biometric_exists(&self) -> Result<bool>;
    fn read_plain(&self) -> Result<Option<[u8; 32]>>;
    fn write_biometric(&self, key: &[u8; 32]) -> Result<()>;
    fn read_biometric(&self, reason: &str) -> Result<[u8; 32]>;
    fn delete_plain(&self) -> Result<()>;
}

/// One-time, idempotent, value-preserving master-KEK resolution + migration.
///
/// Steady state (biometric item already present): a SINGLE biometric read → the lone Touch ID sheet.
///
/// First run with a legacy PLAIN item (existing locked folders): read the plain 32 bytes, re-store
/// the SAME bytes as the biometric-gated item, then **confirm by VALUE** (read the biometric item
/// back and assert the bytes are byte-for-byte identical) BEFORE deleting the plain item. The
/// confirm-read is itself a biometric read — on the migrating unlock it IS the unlock's Touch ID
/// sheet, so the user still sees exactly one prompt. If the confirm fails for ANY reason we return
/// the error and DO NOT delete the plain item, so access to existing folders is never lost.
///
/// Fresh install (neither item exists): generate a random 32-byte KEK, store it biometric-gated, and
/// return the in-memory bytes — no read-back, no prompt (first `lock_folder` needs no Touch ID).
///
/// Idempotency / crash-safety: the biometric write is create-or-replace and the plain delete is the
/// LAST step gated on a successful value-confirm, so a crash mid-migration leaves either (a) the
/// plain item still present (re-runs cleanly) or (b) both present (next run sees the biometric item,
/// confirms value-equality, then deletes the plain one). The plain item is NEVER deleted before a
/// confirmed, value-equal biometric copy exists.
fn migrate_or_create_kek<S: KekStore>(store: &S, reason: &str) -> Result<[u8; 32]> {
    // Fast path / steady state: the biometric item already exists → one biometric read.
    if store.biometric_exists()? {
        // A stray leftover plain item (e.g. a crash AFTER the biometric write but BEFORE the plain
        // delete on a previous run) is cleaned up opportunistically here, but ONLY after we have
        // confirmed the biometric value equals it — never a blind delete.
        let kek = store.read_biometric(reason)?;
        if let Some(plain) = store.read_plain()? {
            if ct_eq(&plain, &kek) {
                // Confirmed identical → safe to remove the redundant plain copy.
                store.delete_plain()?;
            } else {
                // Diverged (should not happen) — keep the plain item; do NOT destroy data. Log
                // only the fact, never the key bytes (no-PII / no-secret-in-logs).
                tracing::warn!(
                    target: "secrets",
                    "leftover plain master-KEK item differs from the biometric one — leaving it in place"
                );
            }
        }
        return Ok(kek);
    }

    // No biometric item yet. Is there a legacy plain item to migrate?
    match store.read_plain()? {
        Some(plain) => {
            // Migrate: write the SAME bytes biometric-gated, then CONFIRM BY VALUE before deleting.
            store.write_biometric(&plain)?;
            let confirm = store.read_biometric(reason)?;
            if !ct_eq(&plain, &confirm) {
                // The biometric copy does not match the plain bytes → ABORT the migration. Leave the
                // plain item untouched so the next launch retries and existing folders still unwrap.
                return Err(AppError::Secrets(
                    "master-KEK migration value mismatch — keeping the plain item, retry next launch"
                        .into(),
                ));
            }
            // Value-equal biometric copy confirmed → now (and only now) drop the plain item.
            store.delete_plain()?;
            tracing::info!(
                target: "secrets",
                "migrated master KEK to a biometric-gated keychain item (value preserved)"
            );
            Ok(confirm)
        }
        None => {
            // Fresh install: mint a random KEK, store it biometric-gated, return the in-memory bytes
            // (no read-back, no Touch ID prompt for the first lock).
            let fresh = crate::crypto::random_key()?;
            store.write_biometric(&fresh)?;
            tracing::info!(
                target: "secrets",
                "created a fresh biometric-gated master KEK"
            );
            Ok(fresh)
        }
    }
}

/// Constant-time 32-byte comparison (avoids a timing oracle on the KEK bytes; also clearer intent
/// than `==` for a secret). Both inputs are fixed-length so this always touches every byte.
fn ct_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let mut diff = 0u8;
    for i in 0..32 {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

// ───────────────────────────── biometric-gated KEK: macOS backend ─────────────────────────────

/// Real macOS backend for the biometric-gated KEK, built on the raw Security framework FFI.
///
/// WHY raw FFI: the high-level `security-framework` crate's `ItemAddOptions` has no
/// `set_access_control` setter, so it cannot create an item protected by a `SecAccessControl`. The
/// task explicitly approves dropping to `SecAccessControlCreateWithFlags` + `SecItemAdd` /
/// `SecItemCopyMatching` / `SecItemDelete`. We build the CF dictionaries with `core-foundation` and
/// call the C functions from `security-framework-sys`, plus a couple of CFString constants the -sys
/// crate does not re-export (declared in [`sec_consts`]).
#[cfg(target_os = "macos")]
struct MacKekStore;

/// Extra Security.framework CFString constants not re-exported by `security-framework-sys`.
/// Verified to link against `Security.framework` (they are stable Apple symbols). `kSecMatchLimitOne`
/// bounds a copy-match to a single item; `kSecUseOperationPrompt` carries the reason string shown on
/// the Touch ID / passcode sheet for a gated keychain read.
#[cfg(target_os = "macos")]
mod sec_consts {
    use core_foundation::base::OSStatus;
    use core_foundation::string::CFStringRef;
    #[link(name = "Security", kind = "framework")]
    extern "C" {
        pub static kSecMatchLimitOne: CFStringRef;
        pub static kSecUseOperationPrompt: CFStringRef;
        // The `kSecAttrAccessible` DICTIONARY KEY is not re-exported by security-framework-sys
        // (only the value constants live in its `access_control` module). It is a stable Apple
        // symbol — declare it here so the non-gated data-protection writes can pin accessibility to
        // `WhenUnlockedThisDeviceOnly`.
        pub static kSecAttrAccessible: CFStringRef;
    }

    // Stable Apple `OSStatus` codes the -sys crate (2.17) does not export. Values from
    // `<Security/SecBase.h>` / `<MacErrors.h>`: the user pressed Cancel on the Touch ID / passcode
    // sheet (`errSecUserCanceled`), or the keychain item is gated but no UI/biometry context could
    // present (`errSecInteractionNotAllowed`).
    pub const ERR_SEC_USER_CANCELED: OSStatus = -128;
    pub const ERR_SEC_INTERACTION_NOT_ALLOWED: OSStatus = -25308;
}

/// Build the base query dictionary identifying a data-protection-keychain generic-password item:
/// `{ class: GenericPassword, service: SERVICE, account, kSecUseDataProtectionKeychain: true }`.
/// Callers extend it with class-specific keys (return-data, access-control, value, …).
///
/// CRITICAL — `kSecUseDataProtectionKeychain = true` is MANDATORY on every SecItem call routed
/// through here. For the KEK it is required because `kSecAttrAccessControl` (the user-presence gate)
/// is supported ONLY by the macOS data-protection keychain; the legacy FILE-BASED keychain rejects
/// it with `errSecParam` (-50). For the NON-gated secrets (DEK / MCP token / Anthropic key, A2/A3/A4/
/// A9) it is what moves them OFF the legacy file-based keychain and onto the modern, non-syncable,
/// `WhenUnlockedThisDeviceOnly` data-protection store (Apple DTS: "it talks to the data protection
/// keychain if you supply kSecUseDataProtectionKeychain or kSecAttrSynchronizable; if not, it talks
/// to the file-based keychain"). Pinning it keeps every op for an account consistently on the
/// data-protection keychain. The legacy items read/deleted via the `keyring` crate live in the
/// file-based keychain — a SEPARATE store — so there is no primary-key collision during migration.
#[cfg(target_os = "macos")]
fn dp_base_query(account: &str) -> core_foundation::dictionary::CFMutableDictionary {
    use core_foundation::base::TCFType;
    use core_foundation::boolean::CFBoolean;
    use core_foundation::dictionary::CFMutableDictionary;
    use core_foundation::string::CFString;
    use security_framework_sys::item::{
        kSecAttrAccount, kSecAttrService, kSecClass, kSecClassGenericPassword,
        kSecUseDataProtectionKeychain,
    };

    let service = CFString::new(SERVICE);
    let account = CFString::new(account);
    let mut q = CFMutableDictionary::new();
    unsafe {
        q.add(
            &(kSecClass as *const _),
            &(kSecClassGenericPassword as *const _),
        );
        q.add(&(kSecAttrService as *const _), &service.as_CFTypeRef());
        q.add(&(kSecAttrAccount as *const _), &account.as_CFTypeRef());
        // Target the data-protection keychain (required for kSecAttrAccessControl + to move the
        // non-gated secrets off the file-based keychain, see above).
        q.add(
            &(kSecUseDataProtectionKeychain as *const _),
            &CFBoolean::true_value().as_CFTypeRef(),
        );
    }
    q
}

#[cfg(target_os = "macos")]
impl MacKekStore {
    /// The master-KEK item's base query (data-protection keychain, account = [`ACCOUNT_MASTER_KEK`]).
    fn base_query(&self) -> core_foundation::dictionary::CFMutableDictionary {
        dp_base_query(ACCOUNT_MASTER_KEK)
    }
}

#[cfg(target_os = "macos")]
impl KekStore for MacKekStore {
    /// Existence probe that NEVER prompts: query with `kSecUseAuthenticationUI = Skip` and
    /// `kSecReturnAttributes = true` (no data ⇒ no biometric needed). `errSecSuccess` ⇒ exists;
    /// `errSecItemNotFound` ⇒ absent; `errSecInteractionNotAllowed` ⇒ exists but gated (still
    /// "exists"). Any other status is a real error.
    fn biometric_exists(&self) -> Result<bool> {
        use core_foundation::base::{CFType, TCFType};
        use core_foundation::boolean::CFBoolean;
        use security_framework_sys::base::{errSecItemNotFound, errSecSuccess};
        use security_framework_sys::item::kSecUseAuthenticationUISkip;
        use security_framework_sys::item::{kSecReturnAttributes, kSecUseAuthenticationUI};
        use crate::secrets::keychain::sec_consts::ERR_SEC_INTERACTION_NOT_ALLOWED;
        use security_framework_sys::keychain_item::SecItemCopyMatching;

        let mut q = self.base_query();
        unsafe {
            q.add(
                &(kSecReturnAttributes as *const _),
                &CFBoolean::true_value().as_CFTypeRef(),
            );
            q.add(
                &(kSecUseAuthenticationUI as *const _),
                &(kSecUseAuthenticationUISkip as *const _),
            );
        }
        let dict = q.to_immutable();
        let mut out: core_foundation::base::CFTypeRef = std::ptr::null();
        let status = unsafe {
            SecItemCopyMatching(dict.as_concrete_TypeRef(), &mut out)
        };
        // Release anything returned (we only care about the status code).
        if !out.is_null() {
            unsafe { drop(CFType::wrap_under_create_rule(out)) };
        }
        match status {
            s if s == errSecSuccess => Ok(true),
            s if s == ERR_SEC_INTERACTION_NOT_ALLOWED => Ok(true),
            s if s == errSecItemNotFound => Ok(false),
            other => Err(map_osstatus("probe biometric KEK item", other)),
        }
    }

    /// Read the legacy PLAIN item via the `keyring` crate (the exact account+store the old code
    /// wrote — the FILE-BASED keychain). MUST bypass the new data-protection routing in `get_secret`
    /// (the legacy KEK lives in the file-based store, and the gated KEK is read via `read_biometric`,
    /// never as a plain string). Never prompts. `Ok(None)` if absent.
    fn read_plain(&self) -> Result<Option<[u8; 32]>> {
        match legacy_get_secret(ACCOUNT_MASTER_KEK)? {
            Some(hex) => match hex_to_key32(&hex) {
                Some(k) => Ok(Some(k)),
                None => Err(AppError::Secrets("legacy plain master KEK is malformed".into())),
            },
            None => Ok(None),
        }
    }

    /// Create-or-replace the biometric-gated item: build a `SecAccessControl` requiring user
    /// presence (`kSecAccessControlUserPresence` = Touch ID OR device passcode) with accessibility
    /// `kSecAttrAccessibleWhenUnlockedThisDeviceOnly`, then `SecItemAdd` the 32 raw bytes under it.
    /// On `errSecDuplicateItem` we delete the existing item and retry once (idempotent replace).
    fn write_biometric(&self, key: &[u8; 32]) -> Result<()> {
        use core_foundation::base::TCFType;
        use core_foundation::data::CFData;
        use security_framework::access_control::{ProtectionMode, SecAccessControl};
        use security_framework_sys::access_control::kSecAccessControlUserPresence;
        use security_framework_sys::base::{errSecDuplicateItem, errSecSuccess};
        use security_framework_sys::item::{kSecAttrAccessControl, kSecValueData};
        use security_framework_sys::keychain_item::SecItemAdd;

        // user-presence: Touch ID with device-passcode fallback (so a Mac WITHOUT Touch ID can still
        // satisfy the gate via the login password and is never locked out).
        let access = SecAccessControl::create_with_protection(
            Some(ProtectionMode::AccessibleWhenUnlockedThisDeviceOnly),
            kSecAccessControlUserPresence,
        )
        .map_err(|e| AppError::Secrets(format!("build KEK access control: {e}")))?;

        let data = CFData::from_buffer(key);

        let add = |access: &SecAccessControl| -> i32 {
            let mut q = self.base_query();
            unsafe {
                q.add(
                    &(kSecAttrAccessControl as *const _),
                    &access.as_CFTypeRef(),
                );
                q.add(&(kSecValueData as *const _), &data.as_CFTypeRef());
            }
            let dict = q.to_immutable();
            let mut out: core_foundation::base::CFTypeRef = std::ptr::null();
            let s = unsafe { SecItemAdd(dict.as_concrete_TypeRef(), &mut out) };
            if !out.is_null() {
                unsafe {
                    drop(core_foundation::base::CFType::wrap_under_create_rule(out))
                };
            }
            s
        };

        let status = add(&access);
        if status == errSecSuccess {
            return Ok(());
        }
        if status == errSecDuplicateItem {
            // Replace: delete the existing item (by identity, no value read ⇒ no prompt) then re-add.
            self.delete_biometric()?;
            let status2 = add(&access);
            if status2 == errSecSuccess {
                return Ok(());
            }
            return Err(map_osstatus("add biometric KEK item (after replace)", status2));
        }
        Err(map_osstatus("add biometric KEK item", status))
    }

    /// Read the biometric-gated item's value. On real macOS this triggers the Touch ID / passcode
    /// sheet with `reason` shown via `kSecUseOperationPrompt`, and returns the 32 raw bytes on a
    /// successful presence check. A user cancel / auth failure maps to [`AppError::BiometricFailed`].
    fn read_biometric(&self, reason: &str) -> Result<[u8; 32]> {
        use core_foundation::base::{CFType, TCFType};
        use core_foundation::boolean::CFBoolean;
        use core_foundation::data::CFData;
        use core_foundation::string::CFString;
        use crate::secrets::keychain::sec_consts::{
            ERR_SEC_INTERACTION_NOT_ALLOWED, ERR_SEC_USER_CANCELED,
        };
        use security_framework_sys::base::{errSecAuthFailed, errSecSuccess};
        use security_framework_sys::item::{kSecMatchLimit, kSecReturnData};
        use security_framework_sys::keychain_item::SecItemCopyMatching;

        let prompt = CFString::new(reason);
        let mut q = self.base_query();
        unsafe {
            q.add(
                &(kSecReturnData as *const _),
                &CFBoolean::true_value().as_CFTypeRef(),
            );
            q.add(
                &(kSecMatchLimit as *const _),
                &(sec_consts::kSecMatchLimitOne as *const _),
            );
            q.add(
                &(sec_consts::kSecUseOperationPrompt as *const _),
                &prompt.as_CFTypeRef(),
            );
        }
        let dict = q.to_immutable();
        let mut out: core_foundation::base::CFTypeRef = std::ptr::null();
        let status = unsafe { SecItemCopyMatching(dict.as_concrete_TypeRef(), &mut out) };

        if status != errSecSuccess {
            // Release any partial out-ref defensively.
            if !out.is_null() {
                unsafe { drop(CFType::wrap_under_create_rule(out)) };
            }
            return match status {
                s if s == ERR_SEC_USER_CANCELED => {
                    Err(AppError::BiometricFailed("Touch ID was cancelled".into()))
                }
                s if s == errSecAuthFailed => {
                    Err(AppError::BiometricFailed("authentication failed".into()))
                }
                s if s == ERR_SEC_INTERACTION_NOT_ALLOWED => Err(AppError::BiometricFailed(
                    "interaction not allowed (no UI context to present Touch ID)".into(),
                )),
                other => Err(map_osstatus("read biometric KEK item", other)),
            };
        }

        if out.is_null() {
            return Err(AppError::Secrets(
                "biometric KEK read returned success but no data".into(),
            ));
        }
        // SAFETY: success + non-null ⇒ a CFData created by SecItemCopyMatching (we requested
        // kSecReturnData). We own it under the create rule and read its bytes.
        let data = unsafe { CFData::wrap_under_create_rule(out as *const _) };
        let bytes = data.bytes();
        let k: [u8; 32] = bytes
            .try_into()
            .map_err(|_| AppError::Secrets("biometric KEK has wrong length".into()))?;
        Ok(k)
    }

    /// Delete the legacy PLAIN item via the `keyring` crate (FILE-BASED store; idempotent; absence
    /// is not an error). Bypasses the new DP routing for the same reason as `read_plain`.
    fn delete_plain(&self) -> Result<()> {
        legacy_delete_secret(ACCOUNT_MASTER_KEK)
    }
}

#[cfg(target_os = "macos")]
impl MacKekStore {
    /// Delete the biometric-gated item by IDENTITY (class + service + account only — no value read,
    /// so NO Touch ID prompt). Used to replace a duplicate before a fresh add. Idempotent.
    fn delete_biometric(&self) -> Result<()> {
        use core_foundation::base::TCFType;
        use security_framework_sys::base::{errSecItemNotFound, errSecSuccess};
        use security_framework_sys::keychain_item::SecItemDelete;

        let dict = self.base_query().to_immutable();
        let status = unsafe { SecItemDelete(dict.as_concrete_TypeRef()) };
        if status == errSecSuccess || status == errSecItemNotFound {
            Ok(())
        } else {
            Err(map_osstatus("delete biometric KEK item", status))
        }
    }
}

/// Map a non-success Security `OSStatus` to a typed [`AppError`]. The message carries only the
/// numeric status + context — never the key value — so it is safe to log under the no-secret rule.
#[cfg(target_os = "macos")]
fn map_osstatus(ctx: &str, status: core_foundation::base::OSStatus) -> AppError {
    if status == sec_consts::ERR_SEC_INTERACTION_NOT_ALLOWED {
        // The keychain is locked / no UI context — treat as a denied access, recoverable.
        return AppError::KeychainDenied(format!("{ctx}: OSStatus {status}"));
    }
    AppError::Secrets(format!("{ctx}: OSStatus {status}"))
}

// ───────────────────── shared data-protection store for NON-gated string secrets (A2/A3/A4/A9) ────
//
// The DEK, MCP token, and Anthropic key are STRING secrets that need NO biometric ACL (the DEK is
// read on every cold start; gating it would prompt Touch ID at launch). They were previously stored
// via the `keyring` crate, which lands them in the LEGACY FILE-BASED keychain. This moves them onto
// the SAME data-protection keychain backend the KEK already uses — `SecItemAdd`/`CopyMatching`/
// `Delete` with `kSecUseDataProtectionKeychain=true` + `kSecAttrAccessibleWhenUnlockedThisDeviceOnly`
// (non-syncable, this-device-only) — but WITHOUT a `kSecAttrAccessControl` (so no prompt). A
// one-time, value-preserving migration reads any legacy keyring item, re-stores the identical string
// in the data-protection keychain, then deletes the legacy item. Delete-before-add on every write.

/// Trait seam over the data-protection STRING ops so the value-preserving migration can be unit-
/// tested against an in-memory fake (no live keychain). Mirrors the [`KekStore`] seam but for the
/// non-gated string accounts and with no biometric read.
#[allow(dead_code)] // some methods are exercised only on macOS / in tests.
trait DpStringStore {
    /// Read the data-protection item's string value (never prompts). `Ok(None)` if absent.
    fn read_dp(&self) -> Result<Option<String>>;
    /// Create/replace the data-protection item (delete-before-add).
    fn write_dp(&self, secret: &str) -> Result<()>;
    /// Delete the data-protection item (idempotent).
    fn delete_dp(&self) -> Result<()>;
    /// Read the legacy file-based (keyring) item's value (never prompts). `Ok(None)` if absent.
    fn read_legacy(&self) -> Result<Option<String>>;
    /// Delete the legacy file-based (keyring) item (idempotent).
    fn delete_legacy(&self) -> Result<()>;
}

/// Resolve a string secret, migrating a legacy keyring item to the data-protection keychain ONCE,
/// value-preservingly. Steady state (already in DP): a single `read_dp`. First run with a legacy
/// item: read it, write the SAME string to DP, CONFIRM BY VALUE (read DP back, assert equal) BEFORE
/// deleting the legacy item — so a crash mid-migration never loses the secret. Neither present ⇒
/// `Ok(None)` (caller mints a fresh one and `set_secret`s it).
fn migrate_or_read_dp<S: DpStringStore>(store: &S) -> Result<Option<String>> {
    // Fast path: already on the data-protection keychain.
    if let Some(v) = store.read_dp()? {
        // Opportunistically drop a leftover legacy item ONLY when its value matches (crash between
        // DP-write and legacy-delete on a prior run). Never a blind delete.
        if let Some(legacy) = store.read_legacy()? {
            if bool::from(ct_eq_str(&legacy, &v)) {
                store.delete_legacy()?;
            } else {
                tracing::warn!(
                    target: "secrets",
                    "leftover legacy keychain item differs from the data-protection one — leaving it"
                );
            }
        }
        return Ok(Some(v));
    }

    // Not in DP yet — is there a legacy keyring item to migrate?
    match store.read_legacy()? {
        Some(legacy) => {
            store.write_dp(&legacy)?;
            // Confirm by value BEFORE deleting the legacy copy.
            let confirm = store
                .read_dp()?
                .ok_or_else(|| AppError::Secrets("data-protection write did not persist".into()))?;
            if !bool::from(ct_eq_str(&confirm, &legacy)) {
                return Err(AppError::Secrets(
                    "keychain migration value mismatch — keeping the legacy item, retry next launch"
                        .into(),
                ));
            }
            store.delete_legacy()?;
            tracing::info!(
                target: "secrets",
                "migrated a secret to the data-protection keychain (value preserved)"
            );
            Ok(Some(confirm))
        }
        None => Ok(None),
    }
}

/// Constant-time string comparison for secret values (uses `subtle`). Equal-length required; a
/// length difference short-circuits to "not equal" (lengths are not secret here).
fn ct_eq_str(a: &str, b: &str) -> subtle::Choice {
    use subtle::ConstantTimeEq;
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return subtle::Choice::from(0u8);
    }
    a.ct_eq(b)
}

/// macOS data-protection backend for a single non-gated string account.
#[cfg(target_os = "macos")]
struct MacDpStore {
    account: &'static str,
}

#[cfg(target_os = "macos")]
impl DpStringStore for MacDpStore {
    fn read_dp(&self) -> Result<Option<String>> {
        use core_foundation::base::{CFType, TCFType};
        use core_foundation::boolean::CFBoolean;
        use core_foundation::data::CFData;
        use security_framework_sys::base::{errSecItemNotFound, errSecSuccess};
        use security_framework_sys::item::{kSecMatchLimit, kSecReturnData};
        use security_framework_sys::keychain_item::SecItemCopyMatching;

        let mut q = dp_base_query(self.account);
        unsafe {
            q.add(
                &(kSecReturnData as *const _),
                &CFBoolean::true_value().as_CFTypeRef(),
            );
            q.add(
                &(kSecMatchLimit as *const _),
                &(sec_consts::kSecMatchLimitOne as *const _),
            );
        }
        let dict = q.to_immutable();
        let mut out: core_foundation::base::CFTypeRef = std::ptr::null();
        let status = unsafe { SecItemCopyMatching(dict.as_concrete_TypeRef(), &mut out) };
        if status == errSecItemNotFound {
            if !out.is_null() {
                unsafe { drop(CFType::wrap_under_create_rule(out)) };
            }
            return Ok(None);
        }
        if status != errSecSuccess {
            if !out.is_null() {
                unsafe { drop(CFType::wrap_under_create_rule(out)) };
            }
            return Err(map_osstatus("read data-protection secret", status));
        }
        if out.is_null() {
            return Err(AppError::Secrets(
                "data-protection read returned success but no data".into(),
            ));
        }
        let data = unsafe { CFData::wrap_under_create_rule(out as *const _) };
        let s = String::from_utf8(data.bytes().to_vec())
            .map_err(|_| AppError::Secrets("data-protection secret is not valid UTF-8".into()))?;
        Ok(Some(s))
    }

    fn write_dp(&self, secret: &str) -> Result<()> {
        use core_foundation::base::TCFType;
        use core_foundation::data::CFData;
        use security_framework_sys::access_control::kSecAttrAccessibleWhenUnlockedThisDeviceOnly;
        use security_framework_sys::base::{errSecDuplicateItem, errSecSuccess};
        use security_framework_sys::item::kSecValueData;
        use security_framework_sys::keychain_item::SecItemAdd;

        // Delete-before-add so the value is always a clean replace (idempotent write).
        self.delete_dp()?;

        let data = CFData::from_buffer(secret.as_bytes());
        let add = || -> i32 {
            let mut q = dp_base_query(self.account);
            unsafe {
                // Accessibility: unlocked, THIS device only, non-syncable (no iCloud Keychain).
                q.add(
                    &(sec_consts::kSecAttrAccessible as *const _),
                    &(kSecAttrAccessibleWhenUnlockedThisDeviceOnly as *const _),
                );
                q.add(&(kSecValueData as *const _), &data.as_CFTypeRef());
            }
            let dict = q.to_immutable();
            let mut out: core_foundation::base::CFTypeRef = std::ptr::null();
            let s = unsafe { SecItemAdd(dict.as_concrete_TypeRef(), &mut out) };
            if !out.is_null() {
                unsafe { drop(core_foundation::base::CFType::wrap_under_create_rule(out)) };
            }
            s
        };
        let status = add();
        if status == errSecSuccess {
            return Ok(());
        }
        if status == errSecDuplicateItem {
            // Lost a race with another writer — delete + retry once.
            self.delete_dp()?;
            let s2 = add();
            if s2 == errSecSuccess {
                return Ok(());
            }
            return Err(map_osstatus("add data-protection secret (after replace)", s2));
        }
        Err(map_osstatus("add data-protection secret", status))
    }

    fn delete_dp(&self) -> Result<()> {
        use core_foundation::base::TCFType;
        use security_framework_sys::base::{errSecItemNotFound, errSecSuccess};
        use security_framework_sys::keychain_item::SecItemDelete;

        let dict = dp_base_query(self.account).to_immutable();
        let status = unsafe { SecItemDelete(dict.as_concrete_TypeRef()) };
        if status == errSecSuccess || status == errSecItemNotFound {
            Ok(())
        } else {
            Err(map_osstatus("delete data-protection secret", status))
        }
    }

    fn read_legacy(&self) -> Result<Option<String>> {
        legacy_get_secret(self.account)
    }

    fn delete_legacy(&self) -> Result<()> {
        legacy_delete_secret(self.account)
    }
}

/// Return the MCP bearer token (a random 64-char hex string), minting + persisting it in the
/// Keychain on first use. Used to gate MCP `tools/call` when `K_MCP_REQUIRE_TOKEN` is on.
pub fn get_or_create_mcp_token() -> Result<String> {
    if let Some(tok) = get_secret(ACCOUNT_MCP_TOKEN)? {
        if !tok.is_empty() {
            return Ok(tok);
        }
    }
    let mut bytes = zeroize::Zeroizing::new([0u8; 32]);
    getrandom::getrandom(&mut *bytes)
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

// ─────────────────────────── public string-secret API (DP-backed on macOS) ───────────────────────
//
// A2/A3/A4/A9: on macOS the DEK / MCP token / Anthropic key now live in the SAME data-protection
// keychain backend as the KEK (no biometric ACL). `get_secret` migrates any legacy keyring item to
// the data-protection keychain on first read; `set_secret`/`delete_secret` write/delete the DP item
// AND clear any legacy item so the two stores never diverge. Off macOS (CI/dev) the keyring crate is
// the only backend.

/// Store/replace a secret. On macOS: writes the data-protection keychain item (delete-before-add,
/// `WhenUnlockedThisDeviceOnly`, non-syncable) and clears any legacy file-based item so reads can't
/// resurrect a stale value. Off macOS: the keyring crate.
pub fn set_secret(account: &str, secret: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let store = MacDpStore { account: leak_account(account) };
        store.write_dp(secret)?;
        // Drop any legacy file-based copy so a future get_secret can't see the old value.
        let _ = store.delete_legacy();
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        legacy_set_secret(account, secret)
    }
}

/// Read a secret. `Ok(None)` if absent. On macOS this also performs the one-time, value-preserving
/// migration of a legacy keyring item into the data-protection keychain.
pub fn get_secret(account: &str) -> Result<Option<String>> {
    #[cfg(target_os = "macos")]
    {
        let store = MacDpStore { account: leak_account(account) };
        migrate_or_read_dp(&store)
    }
    #[cfg(not(target_os = "macos"))]
    {
        legacy_get_secret(account)
    }
}

/// Delete a secret (idempotent). On macOS removes BOTH the data-protection item and any legacy
/// file-based item.
pub fn delete_secret(account: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let store = MacDpStore { account: leak_account(account) };
        store.delete_dp()?;
        let _ = store.delete_legacy();
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        legacy_delete_secret(account)
    }
}

/// `MacDpStore` holds a `&'static str` account (so it is cheap to construct per call). All callers
/// pass one of the fixed `ACCOUNT_*` constants or the Anthropic-key constant defined in `commands` /
/// `summarize`, which are all `&'static`. To accept an arbitrary `&str` at the public boundary we
/// match it to its known static; an unknown account is leaked once (bounded — accounts come from a
/// small fixed set of string literals, never user input).
#[cfg(target_os = "macos")]
fn leak_account(account: &str) -> &'static str {
    match account {
        ACCOUNT_DB_DEK => ACCOUNT_DB_DEK,
        ACCOUNT_MASTER_KEK => ACCOUNT_MASTER_KEK,
        ACCOUNT_MCP_TOKEN => ACCOUNT_MCP_TOKEN,
        ACCOUNT_ANTHROPIC_KEY => ACCOUNT_ANTHROPIC_KEY,
        other => Box::leak(other.to_string().into_boxed_str()),
    }
}

/// Store/replace a secret in the LEGACY file-based macOS Keychain (keyring crate). Only used on the
/// non-macOS (CI/dev) path; on macOS writes go straight to the data-protection keychain.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn legacy_set_secret(account: &str, secret: &str) -> Result<()> {
    entry(account)?
        .set_password(secret)
        .map_err(|e| classify("set secret", e))
}

/// Read a secret from the LEGACY file-based Keychain. `Ok(None)` if no entry exists.
fn legacy_get_secret(account: &str) -> Result<Option<String>> {
    match entry(account)?.get_password() {
        Ok(secret) => Ok(Some(secret)),
        Err(KeyringError::NoEntry) => Ok(None),
        Err(e) => Err(classify("get secret", e)),
    }
}

/// Delete a LEGACY file-based secret. `Ok(())` if it was already absent (idempotent).
fn legacy_delete_secret(account: &str) -> Result<()> {
    match entry(account)?.delete_credential() {
        Ok(()) => Ok(()),
        Err(KeyringError::NoEntry) => Ok(()),
        Err(e) => Err(classify("delete secret", e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

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

    #[test]
    fn ct_eq_matches_and_differs() {
        let a: [u8; 32] = std::array::from_fn(|i| i as u8);
        let mut b = a;
        assert!(ct_eq(&a, &b));
        b[17] ^= 0x01;
        assert!(!ct_eq(&a, &b));
    }

    // ── Migration value-preservation tests (the keychain calls behind a thin seam) ──
    //
    // A LIVE biometric read can't run headlessly (no Touch ID in CI), so these exercise the
    // MIGRATION LOGIC against an in-memory fake that records the ORDER of operations. We assert:
    //  (1) the plain bytes are re-stored as the biometric item byte-for-byte (value preserved),
    //  (2) the plain item is deleted ONLY AFTER a value-confirmed biometric copy exists
    //      (never delete-before-confirm),
    //  (3) a confirm-read mismatch ABORTS and leaves the plain item intact (no data loss),
    //  (4) a fresh install creates a biometric item and never reads-back/prompts,
    //  (5) the steady state (biometric already present) is a single biometric read.

    #[derive(Debug, Clone, PartialEq)]
    enum Op {
        ProbeExists,
        ReadPlain,
        WriteBiometric([u8; 32]),
        ReadBiometric,
        DeletePlain,
    }

    /// In-memory fake. `biometric` / `plain` are the stored values; `log` records every op in order
    /// so a test can assert delete-after-confirm. `corrupt_biometric_read` forces a mismatch on the
    /// next biometric read to exercise the abort-without-delete path.
    struct FakeStore {
        plain: RefCell<Option<[u8; 32]>>,
        biometric: RefCell<Option<[u8; 32]>>,
        corrupt_biometric_read: bool,
        log: RefCell<Vec<Op>>,
    }

    impl FakeStore {
        fn new(plain: Option<[u8; 32]>, biometric: Option<[u8; 32]>) -> Self {
            Self {
                plain: RefCell::new(plain),
                biometric: RefCell::new(biometric),
                corrupt_biometric_read: false,
                log: RefCell::new(Vec::new()),
            }
        }
        fn log(&self) -> Vec<Op> {
            self.log.borrow().clone()
        }
    }

    impl KekStore for FakeStore {
        fn biometric_exists(&self) -> Result<bool> {
            self.log.borrow_mut().push(Op::ProbeExists);
            Ok(self.biometric.borrow().is_some())
        }
        fn read_plain(&self) -> Result<Option<[u8; 32]>> {
            self.log.borrow_mut().push(Op::ReadPlain);
            Ok(*self.plain.borrow())
        }
        fn write_biometric(&self, key: &[u8; 32]) -> Result<()> {
            self.log.borrow_mut().push(Op::WriteBiometric(*key));
            *self.biometric.borrow_mut() = Some(*key);
            Ok(())
        }
        fn read_biometric(&self, _reason: &str) -> Result<[u8; 32]> {
            self.log.borrow_mut().push(Op::ReadBiometric);
            let stored = self
                .biometric
                .borrow()
                .ok_or_else(|| AppError::Secrets("fake: no biometric item".into()))?;
            if self.corrupt_biometric_read {
                // Simulate a copy that read back DIFFERENT bytes than were written.
                let mut bad = stored;
                bad[0] ^= 0xFF;
                return Ok(bad);
            }
            Ok(stored)
        }
        fn delete_plain(&self) -> Result<()> {
            self.log.borrow_mut().push(Op::DeletePlain);
            *self.plain.borrow_mut() = None;
            Ok(())
        }
    }

    const KEK: [u8; 32] = [
        0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF,
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E,
        0x0F, 0x10,
    ];

    #[test]
    fn migration_preserves_value_and_deletes_plain_only_after_confirm() {
        let store = FakeStore::new(Some(KEK), None);
        let out = migrate_or_create_kek(&store, "Unlock this folder").unwrap();

        // The returned KEK is byte-for-byte the original plain value.
        assert_eq!(out, KEK, "migrated KEK must equal the original plain KEK");
        // The biometric item now holds exactly the original bytes.
        assert_eq!(*store.biometric.borrow(), Some(KEK));
        // The plain item is gone.
        assert_eq!(*store.plain.borrow(), None);

        // Order proof: the write + a confirming read happen BEFORE the plain delete.
        let log = store.log();
        let wrote = log.iter().position(|o| matches!(o, Op::WriteBiometric(_))).unwrap();
        let confirmed = log
            .iter()
            .enumerate()
            .skip(wrote + 1)
            .find(|(_, o)| matches!(o, Op::ReadBiometric))
            .map(|(i, _)| i)
            .expect("a confirming biometric read must follow the write");
        let deleted = log.iter().position(|o| *o == Op::DeletePlain).unwrap();
        assert!(
            wrote < confirmed && confirmed < deleted,
            "must write → confirm-by-value → THEN delete plain (got {log:?})"
        );
        // The written bytes equal the plain bytes (value preservation at the seam).
        assert!(
            log.contains(&Op::WriteBiometric(KEK)),
            "the biometric write must use the original plain bytes verbatim"
        );
    }

    #[test]
    fn migration_mismatch_aborts_and_keeps_plain_item() {
        let mut store = FakeStore::new(Some(KEK), None);
        store.corrupt_biometric_read = true;
        let res = migrate_or_create_kek(&store, "Unlock this folder");

        assert!(res.is_err(), "a confirm mismatch must abort the migration");
        // CRITICAL: the plain item must STILL exist (never delete-before-confirm) so existing
        // locked folders keep unwrapping on the next attempt.
        assert_eq!(
            *store.plain.borrow(),
            Some(KEK),
            "the plain item must be preserved when the confirm read mismatches"
        );
        // And no delete was ever issued.
        assert!(
            !store.log().contains(&Op::DeletePlain),
            "the plain item must never be deleted before a confirmed value-equal copy"
        );
    }

    #[test]
    fn fresh_install_creates_biometric_without_readback_or_delete() {
        let store = FakeStore::new(None, None);
        let out = migrate_or_create_kek(&store, "Unlock this folder").unwrap();

        // A biometric item now exists holding exactly the returned bytes.
        assert_eq!(*store.biometric.borrow(), Some(out));
        // Fresh path: no biometric read-back (no Touch ID for the first lock) and no plain delete.
        let log = store.log();
        assert!(
            !log.contains(&Op::ReadBiometric),
            "fresh create must not read back (no Touch ID prompt for the first lock)"
        );
        assert!(
            !log.contains(&Op::DeletePlain),
            "fresh create has no plain item to delete"
        );
        assert!(
            log.iter().any(|o| matches!(o, Op::WriteBiometric(_))),
            "fresh create must write the biometric item"
        );
    }

    #[test]
    fn steady_state_is_single_biometric_read() {
        // Biometric item already present, no plain leftover → exactly one biometric read.
        let store = FakeStore::new(None, Some(KEK));
        let out = migrate_or_create_kek(&store, "Unlock this folder").unwrap();
        assert_eq!(out, KEK);
        let reads = store
            .log()
            .iter()
            .filter(|o| **o == Op::ReadBiometric)
            .count();
        assert_eq!(reads, 1, "steady-state unlock must be a single biometric read");
        assert!(
            !store.log().contains(&Op::DeletePlain),
            "no plain item present → no delete"
        );
    }

    #[test]
    fn leftover_plain_after_partial_migration_is_cleaned_only_when_value_equal() {
        // Crash recovery: a previous run wrote the biometric item but crashed BEFORE deleting the
        // plain one. Both present + value-equal → the next run confirms equality then removes plain.
        let store = FakeStore::new(Some(KEK), Some(KEK));
        let out = migrate_or_create_kek(&store, "Unlock this folder").unwrap();
        assert_eq!(out, KEK);
        assert_eq!(*store.plain.borrow(), None, "value-equal leftover plain is cleaned up");

        // But if the leftover plain DIFFERS from the biometric item, it is LEFT IN PLACE (no blind
        // destroy). Use a distinct biometric value so the confirm read returns it (not the plain).
        let other: [u8; 32] = std::array::from_fn(|i| (i as u8).wrapping_add(7));
        let store2 = FakeStore::new(Some(KEK), Some(other));
        let out2 = migrate_or_create_kek(&store2, "Unlock this folder").unwrap();
        assert_eq!(out2, other, "steady-state read returns the biometric item value");
        assert_eq!(
            *store2.plain.borrow(),
            Some(KEK),
            "a differing leftover plain item is left untouched, never blindly deleted"
        );
    }

    /// End-to-end value-preservation through the CRYPTO layer: a folder CK wrapped by the ORIGINAL
    /// plain KEK must still unwrap after migration, because the migrated KEK bytes are identical.
    #[test]
    fn wrapped_folder_ck_round_trips_with_preserved_kek() {
        // 1. Pre-migration: a folder CK wrapped under the legacy plain KEK, bound to the folder id.
        let ck = crate::crypto::random_key().unwrap();
        let wrapped = crate::crypto::encrypt(&KEK, &ck, b"folder-123").unwrap();

        // 2. Migrate the plain KEK to the biometric item (value preserved).
        let store = FakeStore::new(Some(KEK), None);
        let migrated_kek = migrate_or_create_kek(&store, "Unlock this folder").unwrap();

        // 3. Post-migration: unwrap the SAME wrapped key with the migrated KEK → original CK back.
        let unwrapped = crate::crypto::decrypt(&migrated_kek, &wrapped, b"folder-123").unwrap();
        assert_eq!(
            unwrapped.as_slice(),
            ck.as_slice(),
            "a folder CK wrapped pre-migration must unwrap with the migrated KEK"
        );

        // 4. And content encrypted under that CK still decrypts (full chain intact).
        let blob = crate::crypto::encrypt(&ck, b"secret note markdown", b"folder-123|m1").unwrap();
        let ck32: [u8; 32] = unwrapped.as_slice().try_into().unwrap();
        let pt = crate::crypto::decrypt(&ck32, &blob, b"folder-123|m1").unwrap();
        assert_eq!(pt, b"secret note markdown");
    }

    // ── A2/A3/A4/A9 data-protection STRING-secret migration value-preservation ───────────────────
    //
    // The DEK / MCP token / Anthropic key migration (`migrate_or_read_dp`) is exercised here against
    // an in-memory fake (a live keychain can't run headlessly). We assert the SAME safety property
    // as the KEK migration: the legacy value is re-stored in the data-protection store byte-for-byte
    // and the legacy item is deleted ONLY AFTER a value-confirmed DP copy exists.

    #[derive(Debug, Clone, PartialEq)]
    enum DpOp {
        ReadDp,
        WriteDp(String),
        DeleteDp,
        ReadLegacy,
        DeleteLegacy,
    }

    struct FakeDp {
        dp: RefCell<Option<String>>,
        legacy: RefCell<Option<String>>,
        log: RefCell<Vec<DpOp>>,
    }
    impl FakeDp {
        fn new(dp: Option<&str>, legacy: Option<&str>) -> Self {
            Self {
                dp: RefCell::new(dp.map(str::to_string)),
                legacy: RefCell::new(legacy.map(str::to_string)),
                log: RefCell::new(Vec::new()),
            }
        }
        fn log(&self) -> Vec<DpOp> {
            self.log.borrow().clone()
        }
    }
    impl DpStringStore for FakeDp {
        fn read_dp(&self) -> Result<Option<String>> {
            self.log.borrow_mut().push(DpOp::ReadDp);
            Ok(self.dp.borrow().clone())
        }
        fn write_dp(&self, secret: &str) -> Result<()> {
            self.log.borrow_mut().push(DpOp::WriteDp(secret.to_string()));
            *self.dp.borrow_mut() = Some(secret.to_string());
            Ok(())
        }
        fn delete_dp(&self) -> Result<()> {
            self.log.borrow_mut().push(DpOp::DeleteDp);
            *self.dp.borrow_mut() = None;
            Ok(())
        }
        fn read_legacy(&self) -> Result<Option<String>> {
            self.log.borrow_mut().push(DpOp::ReadLegacy);
            Ok(self.legacy.borrow().clone())
        }
        fn delete_legacy(&self) -> Result<()> {
            self.log.borrow_mut().push(DpOp::DeleteLegacy);
            *self.legacy.borrow_mut() = None;
            Ok(())
        }
    }

    #[test]
    fn dp_migration_preserves_value_and_deletes_legacy_only_after_confirm() {
        let secret = "anthropic-sk-deadbeef-test-value";
        let store = FakeDp::new(None, Some(secret));
        let out = migrate_or_read_dp(&store).unwrap();

        assert_eq!(out.as_deref(), Some(secret), "migrated value must be byte-identical");
        assert_eq!(*store.dp.borrow(), Some(secret.to_string()), "DP item now holds the value");
        assert_eq!(*store.legacy.borrow(), None, "legacy item removed after migration");

        // Order proof: write DP → confirm-read DP → THEN delete legacy.
        let log = store.log();
        let wrote = log.iter().position(|o| matches!(o, DpOp::WriteDp(_))).unwrap();
        let confirmed = log
            .iter()
            .enumerate()
            .skip(wrote + 1)
            .find(|(_, o)| **o == DpOp::ReadDp)
            .map(|(i, _)| i)
            .expect("a confirming DP read must follow the write");
        let deleted = log.iter().position(|o| *o == DpOp::DeleteLegacy).unwrap();
        assert!(wrote < confirmed && confirmed < deleted, "write→confirm→delete order (got {log:?})");
        assert!(log.contains(&DpOp::WriteDp(secret.to_string())), "DP write uses the legacy bytes verbatim");
    }

    #[test]
    fn dp_steady_state_is_a_single_read_and_keeps_no_legacy() {
        // Already migrated: only a DP read; no legacy present.
        let store = FakeDp::new(Some("mcp-token-xyz"), None);
        let out = migrate_or_read_dp(&store).unwrap();
        assert_eq!(out.as_deref(), Some("mcp-token-xyz"));
        assert!(!store.log().contains(&DpOp::DeleteLegacy), "no legacy → no delete");
        assert!(!store.log().iter().any(|o| matches!(o, DpOp::WriteDp(_))), "steady state writes nothing");
    }

    #[test]
    fn dp_absent_everywhere_returns_none() {
        let store = FakeDp::new(None, None);
        assert_eq!(migrate_or_read_dp(&store).unwrap(), None, "no item anywhere → None (caller mints fresh)");
    }

    #[test]
    fn dp_value_equal_legacy_leftover_is_cleaned_but_differing_is_kept() {
        // Crash recovery: DP written, legacy not yet deleted, values equal → cleaned.
        let store = FakeDp::new(Some("k"), Some("k"));
        migrate_or_read_dp(&store).unwrap();
        assert_eq!(*store.legacy.borrow(), None, "value-equal legacy leftover removed");

        // Differing legacy is LEFT IN PLACE (never a blind destroy).
        let store2 = FakeDp::new(Some("dp-value"), Some("legacy-DIFFERENT"));
        migrate_or_read_dp(&store2).unwrap();
        assert_eq!(
            *store2.legacy.borrow(),
            Some("legacy-DIFFERENT".to_string()),
            "a differing legacy item is left untouched"
        );
    }
}
