use keyring::{Entry, Error as KeyringError};

use crate::error::{AppError, Result};

pub const SERVICE: &str = "com.meetnotes.app";

/// Keychain account holding the SQLCipher database encryption key (DEK).
pub const ACCOUNT_DB_DEK: &str = "murmur_db_dek";

/// Keychain account holding the master KEK that wraps per-folder content keys (Layer 2 lock).
///
/// v0.3.2: this account now names the BIOMETRIC-GATED item (stored via the macOS Security framework
/// with a `kSecAttrAccessControl` requiring user presence). The legacy PLAIN keyring item lived
/// under the SAME account name; the one-time migration (see [`resolve_kek`]) reads that
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

/// Keychain account holding the BYO web-search API key (Brave). Mirrors
/// [`crate::connectors::web::WEB_SEARCH_KEY_ACCOUNT`]. Named here so the data-protection routing
/// recognizes it as a known fixed account (no per-call string leak in [`leak_account`]).
pub const ACCOUNT_WEB_SEARCH_KEY: &str = "web_search_api_key";

/// Keychain account holding the BYO Jira connector API token. Mirrors
/// [`crate::connectors::jira::JIRA_TOKEN_ACCOUNT`]. Named here so the data-protection routing
/// recognizes it as a known fixed account (no per-call string leak in [`leak_account`]).
pub const ACCOUNT_JIRA_TOKEN: &str = "jira_api_token";

/// Keychain account holding the BYO Slack connector user token (`xoxp-…`). Mirrors
/// [`crate::connectors::slack::SLACK_TOKEN_ACCOUNT`]. Named here so the data-protection routing
/// recognizes it as a known fixed account (no per-call string leak in [`leak_account`]).
pub const ACCOUNT_SLACK_TOKEN: &str = "slack_user_token";

/// Keychain account holding the BYO Notion connector integration token. Mirrors
/// [`crate::connectors::notion::NOTION_TOKEN_ACCOUNT`]. Named here so the data-protection routing
/// recognizes it as a known fixed account (no per-call string leak in [`leak_account`]).
pub const ACCOUNT_NOTION_TOKEN: &str = "notion_api_token";

/// Keychain account holding the BYO ClickUp connector personal API token (`pk_…`). Mirrors
/// [`crate::connectors::clickup::CLICKUP_TOKEN_ACCOUNT`]. Named here so the data-protection routing
/// recognizes it as a known fixed account (no per-call string leak in [`leak_account`]).
pub const ACCOUNT_CLICKUP_TOKEN: &str = "clickup_api_token";

/// Keychain account holding the AI Gateway API key. Mirrors `summarize::GATEWAY_KEY_ACCOUNT` and
/// `commands::GATEWAY_KEY_ACCOUNT`. Strictly separate from [`ACCOUNT_ANTHROPIC_KEY`] — never a
/// fallback (R3). Named here so the data-protection routing recognizes it as a known fixed account
/// (no per-call string leak in [`leak_account`]).
pub const ACCOUNT_GATEWAY_KEY: &str = "gateway_api_key";

/// Default reason string shown on the Touch ID / passcode sheet when releasing the master KEK.
/// Callers may override per call-site (e.g. "Unlock this folder").
pub const KEK_DEFAULT_REASON: &str = "Unlock this folder";

/// Keychain account holding the BIOMETRIC-GATED cache of the sharing-account master key (MK).
///
/// The sharing account's MK lives in RAM for the session ([`crate::share::AccountSession`]); it is
/// lost on restart, forcing a password re-login. Caching it here — behind a `kSecAttrAccessControl`
/// requiring user presence, exactly like [`ACCOUNT_MASTER_KEK`] — lets a single Touch ID tap restore
/// the session ([`cache_account_mk_biometric`] / [`read_account_mk_biometric`]). SEPARATE from the
/// folder-lock KEK item (different account) so the two never collide. Cleared on logout.
pub const ACCOUNT_SHARE_MK: &str = "murmur_account_mk";

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

    // CATASTROPHIC-SAFETY (the DEK is the whole-DB SQLCipher key): if the read below ever WRONGLY
    // reports the existing DEK as absent, the mint at the end silently re-keys — permanently
    // orphaning the entire encrypted database. On a signed release macOS build the data-protection
    // keychain MUST be available (the embedded Developer-ID provisioning profile grants the
    // entitlement); an errSecMissingEntitlement (-34018) there means a MIS-SIGNED build, NOT a
    // genuinely-absent DEK. The generic `get_secret` would, on -34018, fall back to the (empty for a
    // DP-era user) legacy keyring and return Ok(None) → mint → DB loss. So for the DEK specifically we
    // read STRICTLY: -34018 becomes a hard error (graceful startup dialog), never a masked Ok(None).
    // A genuine legacy-keyring DEK still migrates — migrate_or_read_dp reads legacy when the DP query
    // SUCCEEDS-but-empty; only a FAILED (-34018) DP query is refused here.
    #[cfg(all(target_os = "macos", not(debug_assertions)))]
    let existing = {
        let store = MacDpStore {
            account: leak_account(ACCOUNT_DB_DEK),
        };
        match migrate_or_read_dp(&store) {
            Ok(v) => v,
            Err(e) if is_missing_entitlement(&e) => {
                tracing::error!(
                    target: "secrets",
                    "DEK read hit errSecMissingEntitlement on a release build (mis-signed / missing DP entitlement) — REFUSING to mint a replacement DEK (it would orphan the whole database)"
                );
                return Err(AppError::Secrets(
                    "the database key could not be read (the app appears mis-signed) — refusing to create a new one, which would make your existing database unreadable".into(),
                ));
            }
            Err(e) => return Err(e),
        }
    };
    #[cfg(not(all(target_os = "macos", not(debug_assertions))))]
    let existing = get_secret(ACCOUNT_DB_DEK)?;

    if let Some(dek) = existing {
        return Ok(dek);
    }
    // LAST LINE OF DEFENCE before an irreversible mint. The `-34018` refusal above covers one
    // known way a read wrongly reports "absent"; this covers every other way, by asking the only
    // question that actually matters — is there already an encrypted database whose sole key is the
    // one we are about to replace? If so, minting does not "recover" anything: it permanently
    // orphans the user's entire history, and no later fix can undo it.
    //
    // Deliberately NOT refused: a PLAINTEXT database. That is the pre-encryption shape, and
    // `storage::migration` encrypts it with the freshly-minted key by design.
    //
    // The dev hatch returned long before this point, so `MURMUR_DEV_DEK` is unaffected.
    if crate::state::encrypted_db_exists() {
        tracing::error!(
            target: "secrets",
            "the database key read as absent while an ENCRYPTED database exists — REFUSING to mint a replacement (it would orphan every meeting)"
        );
        return Err(AppError::Secrets(
            "your database is encrypted but its key could not be read. Murmur refuses to create a \
             new key, because that would make every existing meeting permanently unreadable. \
             Restore the key from your recovery export, or from a Keychain backup."
                .into(),
        ));
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
/// single sheet IS the unlock auth: the caller must NOT also run a separate biometric prompt
/// (doing so would double-prompt). `reason` is shown verbatim on the sheet (e.g. "Unlock this
/// folder").
///
/// On first use this also runs a one-time, idempotent, value-preserving migration from the legacy
/// PLAIN item (see [`resolve_kek`]). This KEK never touches SQLCipher; it only
/// wraps/unwraps content keys via [`crate::crypto`].
///
/// The debug KEK hatch: its environment-variable name, and the one value every in-repo writer
/// installs.
///
/// Both live here, next to the code that READS the hatch, so the writers cannot drift from the
/// reader or from each other. There are exactly two writers — the harness runtime probe in
/// `commands::reminders` and the test fixture in `commands::tests::dev_kek_fixture` — and before
/// this constant existed each hand-typed its own 64-character literal. They happened to agree,
/// which is not the same as being unable to disagree.
///
/// Never compiled into a release build.
#[cfg(debug_assertions)]
pub(crate) const DEV_KEK_ENV: &str = "MURMUR_DEV_KEK";

/// The fixed hatch key. Built rather than written out: a literal 64-hex string in a diff is the
/// shape the repo's secret scanner exists to catch, and this must not LOOK like a real KEK.
#[cfg(debug_assertions)]
pub(crate) fn dev_kek_hatch_value() -> String {
    "1".repeat(64)
}

/// Uses [`KEK_DEFAULT_REASON`] on the Touch ID sheet. Call
/// [`get_or_create_master_kek_with_reason`] to override the prompt text per call-site.
///
/// ⚠️ ALLOW-MINT semantics: this back-compat form may CREATE a fresh KEK when none exists. Every
/// production lock/unseal path must use [`master_kek_with_policy`] with `allow_mint` derived from
/// `Db::any_locked_folder()` instead (minting over sealed folders orphans them — the 2026-07-05
/// field incident). Kept for the test suite, which runs entirely on the `MURMUR_DEV_KEK` hatch.
pub fn get_or_create_master_kek() -> Result<[u8; 32]> {
    get_or_create_master_kek_with_reason(KEK_DEFAULT_REASON)
}

/// As [`get_or_create_master_kek`], but the `reason` string is shown verbatim on the Touch ID /
/// passcode sheet that the biometric-gated keychain read presents (e.g. "Unlock this folder").
/// THAT sheet is the unlock auth — do NOT also run a separate biometric prompt.
pub fn get_or_create_master_kek_with_reason(reason: &str) -> Result<[u8; 32]> {
    master_kek_with_policy(reason, true)
}

/// As [`get_or_create_master_kek_with_reason`] but with an explicit MINT POLICY. `allow_mint`
/// MUST be `false` whenever ANY sealed folder exists (the caller checks the DB): a freshly-minted
/// KEK cannot unwrap existing folders' content keys, so minting over sealed content silently
/// orphans it (the 2026-07-05 field incident). Unseal paths (`unlock_folder` / `remove_lock`)
/// always pass `false`; `lock_folder` passes `false` unless NOTHING is sealed yet.
pub fn master_kek_with_policy(reason: &str, allow_mint: bool) -> Result<[u8; 32]> {
    // Dev-only escape hatch mirroring MURMUR_DEV_DEK, but a SEPARATE env var so the at-rest DEK
    // and the lock KEK can be fixed independently in tests/dev. Returns FIRST so dev needs no Touch
    // ID and no Keychain access at all. NEVER compiled into release.
    #[cfg(debug_assertions)]
    if let Ok(dev) = std::env::var(DEV_KEK_ENV) {
        // Checked HERE, at the moment the key is actually used, not only where the test fixture
        // installs it. The fixture's own assertion covers the gap between one test's setup and the
        // next; this covers the gap between a test's setup and its own crypto, which is the window
        // that actually decides whether that test decrypts with the right key. Compiled out of
        // both the dev binary and release.
        #[cfg(test)]
        debug_assert_eq!(
            dev,
            dev_kek_hatch_value(),
            "the debug KEK changed between fixture setup and this read — something else in this \
             process installed a different {DEV_KEK_ENV}"
        );
        if let Some(k) = hex_to_key32(&dev) {
            return Ok(k);
        }
    }

    #[cfg(target_os = "macos")]
    {
        resolve_kek(&MacKekStore, reason, allow_mint)
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
        if !allow_mint {
            return Err(AppError::Secrets(
                "the folder master key was not found in the keychain — Murmur will NOT create a new one because locked folders exist (their data stays intact); try unlocking again to run key recovery"
                    .into(),
            ));
        }
        let bytes = crate::crypto::random_key()?;
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        set_secret(ACCOUNT_MASTER_KEK, &hex)?;
        Ok(bytes)
    }
}

/// EVERY master-KEK candidate the keychain stores currently hold, for the unlock RECOVERY path:
/// when the primary KEK fails to unwrap a folder's content key, the folder was sealed under a
/// DIFFERENT KEK — and on machines where the no-UI existence probe lies (see [`KekStore`]), several
/// generations of KEK items can coexist in the data-protection keychain (each "fresh mint" that
/// couldn't see its predecessors added another). This enumerates ALL of them (`kSecMatchLimitAll` +
/// data, ONE user-presence prompt) plus the legacy plain item, so the unlock can try each candidate
/// against the wrapped content key. Read-only — never writes or deletes anything. Candidate COUNT
/// may be logged by callers; the bytes never are, and the list zeroizes on drop.
///
/// LENIENT: an enumeration FAILURE (cancelled/failed Touch ID, transient fault) is swallowed and the
/// legacy candidate is still returned. That is SAFE for the unlock/remove-lock recovery paths — an
/// empty/partial set just makes the unlock fail (content untouched, retry). It is NOT safe for a
/// DESTRUCTIVE decision, which must NEVER read a failed enumeration as "no key exists" — the discard
/// path uses [`list_master_kek_candidates_strict`] instead (2026-07-05 lock-security finding).
pub fn list_master_kek_candidates(reason: &str) -> Result<zeroize::Zeroizing<Vec<[u8; 32]>>> {
    collect_master_kek_candidates(reason, false)
}

/// As [`list_master_kek_candidates`] but STRICT: a candidate-enumeration failure PROPAGATES as `Err`
/// instead of being swallowed. The DESTRUCTIVE discard path (`discard_unrecoverable_folder_lock`)
/// requires this — it may conclude a folder is unrecoverable ONLY from an enumeration that
/// AUTHORITATIVELY completed and returned no unwrapping candidate. A cancelled/failed Touch ID or a
/// transient keychain fault must abort the discard (`Err`), never be mistaken for "the keychain
/// holds no key" (which would irreversibly wipe a still-recoverable folder). A debug
/// `MURMUR_DEV_KEK` also returns `Err`: its isolated one-key universe cannot prove that an older
/// MeetNotes-dev folder was not sealed under a Keychain KEK before the hatch was enabled.
pub fn list_master_kek_candidates_strict(
    reason: &str,
) -> Result<zeroize::Zeroizing<Vec<[u8; 32]>>> {
    collect_master_kek_candidates(reason, true)
}

#[cfg(debug_assertions)]
fn dev_kek_candidates(
    raw: Option<&str>,
    strict: bool,
) -> Option<Result<zeroize::Zeroizing<Vec<[u8; 32]>>>> {
    let key = hex_to_key32(raw?)?;
    if strict {
        return Some(Err(AppError::Unavailable(
            "cannot prove a folder key absent while MURMUR_DEV_KEK isolates the Keychain".into(),
        )));
    }
    Some(Ok(zeroize::Zeroizing::new(vec![key])))
}

/// Shared enumeration for both candidate-list variants. `strict` decides whether a failure to
/// enumerate the biometric items propagates (`true`, for the destructive path) or is swallowed with
/// only the legacy candidate returned (`false`, for the recovery path).
fn collect_master_kek_candidates(
    reason: &str,
    strict: bool,
) -> Result<zeroize::Zeroizing<Vec<[u8; 32]>>> {
    let mut out: zeroize::Zeroizing<Vec<[u8; 32]>> = zeroize::Zeroizing::new(Vec::new());

    // Dev hatch first (mirrors the resolution order).
    #[cfg(debug_assertions)]
    if let Some(dev_result) =
        dev_kek_candidates(std::env::var(DEV_KEK_ENV).ok().as_deref(), strict)
    {
        // The debug hatch is an explicit isolated key universe. Touching
        // biometric/plain login-Keychain generations as well would make
        // cargo tests and tauri-dev depend on (and potentially prompt for)
        // release credentials despite the fixed dev key. It also defeats
        // the harness's hard Security-framework denial. A destructive
        // absence proof is different: an older MeetNotes-dev folder may
        // have been sealed before the hatch was set, using a Keychain KEK.
        // Since we intentionally skip that store, strict enumeration must
        // fail closed rather than call this one-key set authoritative.
        // Release builds do not compile this branch.
        return dev_result;
    }

    #[cfg(target_os = "macos")]
    {
        match MacKekStore.read_biometric_all(reason) {
            Ok(items) => out.extend(items),
            Err(e) if strict => {
                // Destructive path: a failed/cancelled enumeration is NOT proof of absence — abort.
                return Err(e);
            }
            Err(e) => {
                // Recovery path: enumeration failing must not mask the legacy candidate below.
                tracing::warn!(
                    target: "secrets",
                    error = %e,
                    "master-KEK candidate enumeration failed (continuing with the legacy store only)"
                );
            }
        }
        // The legacy plain KEK is also a candidate. In STRICT mode a read FAILURE here (a transient
        // keychain fault) must ALSO propagate — an `Ok(empty)` set may only mean "authoritatively no
        // key" (both reads returned a clean not-found), never "a read failed" (2026-07-05
        // lock-security finding, the read_plain half of the strict-enumeration gap).
        match MacKekStore.read_plain() {
            Ok(Some(plain)) => out.push(plain),
            Ok(None) => {}
            Err(e) if strict => return Err(e),
            Err(_) => {}
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = (reason, strict);
        if let Ok(Some(hex)) = get_secret(ACCOUNT_MASTER_KEK) {
            if let Some(k) = hex_to_key32(&hex) {
                out.push(k);
            }
        }
    }

    out.dedup();
    Ok(out)
}

// ─────────────────────── biometric-gated sharing-account MK cache (public API) ───────────────────────
//
// "Keep me unlocked for sharing with Touch ID": the sharing-account master key (MK) normally lives
// only in RAM ([`crate::share::AccountSession`]) and is lost on restart. These four functions cache it
// behind a biometric-gated keychain item so a single Touch ID tap can restore the session instead of a
// full password re-login. WRITE + existence-probe never prompt; only READ presents Touch ID (signed
// build). The MK is NEVER logged.
//
// DEBUG builds (`tauri dev`, `cargo test`): mirror the MURMUR_DEV_DEK / MURMUR_DEV_KEK posture — an
// unsigned dev binary can neither satisfy Touch ID nor reliably read a biometric-ACL item across
// rebuilds. So a debug build persists the MK (hex) in the plaintext DEV file store (the same store the
// non-gated dev secrets use), with an optional fixed `MURMUR_DEV_ACCOUNT_MK` (64-hex) env hatch. That
// keeps the session restorable across dev rebuilds WITHOUT a Touch ID prompt. This whole debug path is
// compiled out of release; a signed release uses the biometric-gated data-protection item below.

/// Cache the sharing-account master key (MK) so a later restart can restore the session with one Touch
/// ID tap. WRITE — MUST NOT prompt Touch ID. Idempotent (create-or-replace). Never logs the MK.
pub fn cache_account_mk_biometric(mk: &[u8; 32]) -> Result<()> {
    #[cfg(debug_assertions)]
    {
        cache_account_mk_at(&dev_secrets_path()?, mk)
    }
    #[cfg(all(target_os = "macos", not(debug_assertions)))]
    {
        dp_biometric_write(ACCOUNT_SHARE_MK, mk)
    }
    #[cfg(all(not(target_os = "macos"), not(debug_assertions)))]
    {
        // Non-macOS release never ships; keep the legacy keyring path so CI cross-builds compile.
        let hex: String = mk.iter().map(|b| format!("{b:02x}")).collect();
        legacy_set_secret(ACCOUNT_SHARE_MK, &hex)
    }
}

/// Release the cached sharing-account MK. READ — presents the Touch ID / passcode sheet with `reason`
/// on a signed build. Fails closed ([`AppError::Unavailable`] when no cache exists,
/// [`AppError::BiometricFailed`] when the tap is cancelled/fails) so the FE can fall back to password.
pub fn read_account_mk_biometric(reason: &str) -> Result<[u8; 32]> {
    #[cfg(debug_assertions)]
    {
        let _ = reason;
        // Fixed env hatch wins (no Touch ID, survives rebuilds), then the dev file store.
        if let Ok(dev) = std::env::var("MURMUR_DEV_ACCOUNT_MK") {
            if let Some(k) = hex_to_key32(&dev) {
                return Ok(k);
            }
        }
        read_account_mk_at(&dev_secrets_path()?)
    }
    #[cfg(all(target_os = "macos", not(debug_assertions)))]
    {
        dp_biometric_read(ACCOUNT_SHARE_MK, reason)
    }
    #[cfg(all(not(target_os = "macos"), not(debug_assertions)))]
    {
        let _ = reason;
        match legacy_get_secret(ACCOUNT_SHARE_MK)? {
            Some(hex) => hex_to_key32(&hex)
                .ok_or_else(|| AppError::Secrets("cached account MK is malformed".into())),
            None => Err(AppError::Unavailable(
                "no cached account key to unlock".into(),
            )),
        }
    }
}

/// Does a cached account MK exist? NO Touch ID prompt (existence probe only) — for
/// `account_status.biometric_unlock_available` so the FE can show/hide the "Unlock with Touch ID" button.
pub fn account_mk_cached() -> Result<bool> {
    #[cfg(debug_assertions)]
    {
        if let Ok(dev) = std::env::var("MURMUR_DEV_ACCOUNT_MK") {
            if hex_to_key32(&dev).is_some() {
                return Ok(true);
            }
        }
        account_mk_cached_at(&dev_secrets_path()?)
    }
    #[cfg(all(target_os = "macos", not(debug_assertions)))]
    {
        dp_biometric_exists(ACCOUNT_SHARE_MK)
    }
    #[cfg(all(not(target_os = "macos"), not(debug_assertions)))]
    {
        Ok(legacy_get_secret(ACCOUNT_SHARE_MK)?.is_some())
    }
}

/// Remove the cached account MK (logout / account reset). Idempotent; never prompts.
pub fn clear_account_mk() -> Result<()> {
    #[cfg(debug_assertions)]
    {
        clear_account_mk_at(&dev_secrets_path()?)
    }
    #[cfg(all(target_os = "macos", not(debug_assertions)))]
    {
        dp_biometric_delete(ACCOUNT_SHARE_MK)
    }
    #[cfg(all(not(target_os = "macos"), not(debug_assertions)))]
    {
        legacy_delete_secret(ACCOUNT_SHARE_MK)
    }
}

// DEBUG dev-file-store helpers for the account MK (explicit-path test seam, mirrors the non-gated
// dev-secret `*_at` helpers). No env hatch here — that is handled by the public wrappers — so these
// are a pure file round-trip a test can drive against a temp path.
#[cfg(debug_assertions)]
fn cache_account_mk_at(path: &std::path::Path, mk: &[u8; 32]) -> Result<()> {
    let hex: String = mk.iter().map(|b| format!("{b:02x}")).collect();
    dev_set_secret_at(path, ACCOUNT_SHARE_MK, &hex)
}
#[cfg(debug_assertions)]
fn read_account_mk_at(path: &std::path::Path) -> Result<[u8; 32]> {
    match dev_get_secret_at(path, ACCOUNT_SHARE_MK)? {
        Some(hex) => hex_to_key32(&hex)
            .ok_or_else(|| AppError::Secrets("cached account MK is malformed".into())),
        None => Err(AppError::Unavailable(
            "no cached account key to unlock".into(),
        )),
    }
}
#[cfg(debug_assertions)]
fn account_mk_cached_at(path: &std::path::Path) -> Result<bool> {
    Ok(dev_get_secret_at(path, ACCOUNT_SHARE_MK)?.is_some())
}
#[cfg(debug_assertions)]
fn clear_account_mk_at(path: &std::path::Path) -> Result<()> {
    dev_delete_secret_at(path, ACCOUNT_SHARE_MK)
}

// Generic biometric-gated data-protection ops parameterized by account (release macOS only). Mirror
// the `MacKekStore` methods exactly but for an arbitrary account, so the sharing-account MK cache
// reuses the proven user-presence pattern WITHOUT touching the folder-lock KEK path. Compiled only in
// a release macOS build (the debug/test path uses the dev file store above), which also keeps them
// dead-code-free in debug.
#[cfg(all(target_os = "macos", not(debug_assertions)))]
fn dp_biometric_write(account: &str, key: &[u8; 32]) -> Result<()> {
    use core_foundation::base::TCFType;
    use core_foundation::data::CFData;
    use security_framework::access_control::{ProtectionMode, SecAccessControl};
    use security_framework_sys::access_control::kSecAccessControlUserPresence;
    use security_framework_sys::base::{errSecDuplicateItem, errSecSuccess};
    use security_framework_sys::item::{kSecAttrAccessControl, kSecValueData};
    use security_framework_sys::keychain_item::SecItemAdd;

    let access = SecAccessControl::create_with_protection(
        Some(ProtectionMode::AccessibleWhenUnlockedThisDeviceOnly),
        kSecAccessControlUserPresence,
    )
    .map_err(|e| AppError::Secrets(format!("build account-MK access control: {e}")))?;

    let data = CFData::from_buffer(key);
    let add = |access: &SecAccessControl| -> i32 {
        let mut q = dp_base_query(account);
        unsafe {
            q.add(&(kSecAttrAccessControl as *const _), &access.as_CFTypeRef());
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

    let status = add(&access);
    if status == errSecSuccess {
        return Ok(());
    }
    if status == errSecDuplicateItem {
        // Replace via delete-then-add. This is DELIBERATELY different from the master-KEK write
        // (which REFUSES on duplicate to preserve hidden generations): the account-MK cache is a
        // SINGLE-VALUE cache that MUST be overwritten when it changes — logging in as a different
        // account (or after a key rotation) has to replace the cached MK, or a later biometric
        // restore would pair the NEW account's tokens with the OLD account's MK. Unlike the KEK,
        // there are no multi-generation semantics to preserve: `cache_account_mk_biometric` is
        // called exactly once per login with the account's deterministic MK (never in a probe-lie
        // mint loop), so no divergent generations ever accumulate under ACCOUNT_SHARE_MK, and the
        // MK is always re-derivable from a password login — a lost cache costs one re-login, never
        // data. `dp_biometric_delete` is scoped to this one account name.
        dp_biometric_delete(account)?;
        let status2 = add(&access);
        if status2 == errSecSuccess {
            return Ok(());
        }
        return Err(map_osstatus("add account-MK item (after replace)", status2));
    }
    Err(map_osstatus("add account-MK item", status))
}

#[cfg(all(target_os = "macos", not(debug_assertions)))]
fn dp_biometric_read(account: &str, reason: &str) -> Result<[u8; 32]> {
    use crate::secrets::keychain::sec_consts::{
        ERR_SEC_INTERACTION_NOT_ALLOWED, ERR_SEC_USER_CANCELED,
    };
    use core_foundation::base::{CFType, TCFType};
    use core_foundation::boolean::CFBoolean;
    use core_foundation::data::CFData;
    use core_foundation::string::CFString;
    use security_framework_sys::base::{errSecAuthFailed, errSecSuccess};
    use security_framework_sys::item::{kSecMatchLimit, kSecReturnData};
    use security_framework_sys::keychain_item::SecItemCopyMatching;

    let prompt = CFString::new(reason);
    let mut q = dp_base_query(account);
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
        if !out.is_null() {
            unsafe { drop(CFType::wrap_under_create_rule(out)) };
        }
        return match status {
            s if s == ERR_SEC_USER_CANCELED => {
                Err(AppError::BiometricFailed(crate::errcode::tag(
                    crate::errcode::TOUCH_ID_CANCELLED,
                    "Touch ID was cancelled",
                )))
            }
            s if s == errSecAuthFailed => {
                Err(AppError::BiometricFailed(crate::errcode::tag(
                    crate::errcode::TOUCH_ID_FAILED,
                    "authentication failed",
                )))
            }
            s if s == ERR_SEC_INTERACTION_NOT_ALLOWED => {
                Err(AppError::BiometricFailed(crate::errcode::tag(
                    crate::errcode::TOUCH_ID_FAILED,
                    "interaction not allowed (no UI context to present Touch ID)",
                )))
            }
            other => Err(map_osstatus("read account-MK item", other)),
        };
    }
    if out.is_null() {
        return Err(AppError::Secrets(
            "account-MK read returned success but no data".into(),
        ));
    }
    let data = unsafe { CFData::wrap_under_create_rule(out as *const _) };
    let bytes = data.bytes();
    let k: [u8; 32] = bytes
        .try_into()
        .map_err(|_| AppError::Secrets("cached account MK has wrong length".into()))?;
    Ok(k)
}

#[cfg(all(target_os = "macos", not(debug_assertions)))]
fn dp_biometric_exists(account: &str) -> Result<bool> {
    use crate::secrets::keychain::sec_consts::ERR_SEC_INTERACTION_NOT_ALLOWED;
    use core_foundation::base::{CFType, TCFType};
    use core_foundation::boolean::CFBoolean;
    use security_framework_sys::base::{errSecItemNotFound, errSecSuccess};
    use security_framework_sys::item::kSecUseAuthenticationUISkip;
    use security_framework_sys::item::{kSecReturnAttributes, kSecUseAuthenticationUI};
    use security_framework_sys::keychain_item::SecItemCopyMatching;

    let mut q = dp_base_query(account);
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
    let status = unsafe { SecItemCopyMatching(dict.as_concrete_TypeRef(), &mut out) };
    if !out.is_null() {
        unsafe { drop(CFType::wrap_under_create_rule(out)) };
    }
    match status {
        s if s == errSecSuccess => Ok(true),
        s if s == ERR_SEC_INTERACTION_NOT_ALLOWED => Ok(true),
        s if s == errSecItemNotFound => Ok(false),
        other => Err(map_osstatus("probe account-MK item", other)),
    }
}

#[cfg(all(target_os = "macos", not(debug_assertions)))]
fn dp_biometric_delete(account: &str) -> Result<()> {
    use core_foundation::base::TCFType;
    use security_framework_sys::base::{errSecItemNotFound, errSecSuccess};
    use security_framework_sys::keychain_item::SecItemDelete;

    let dict = dp_base_query(account).to_immutable();
    let status = unsafe { SecItemDelete(dict.as_concrete_TypeRef()) };
    if status == errSecSuccess || status == errSecItemNotFound {
        Ok(())
    } else {
        Err(map_osstatus("delete account-MK item", status))
    }
}

// ───────────────────────────── biometric-gated KEK: storage seam ─────────────────────────────

/// A thin storage seam over the master-KEK keychain operations, so the value-preserving migration
/// ([`resolve_kek`]) can be unit-tested against an in-memory fake WITHOUT a live keychain
/// or a Touch ID prompt (a biometric read can't run headlessly — there is no Touch ID in CI).
///
/// The operations the resolution needs:
/// - [`KekStore::read_plain`] — read the legacy PLAIN item's raw value (never prompts).
/// - [`KekStore::write_biometric`] — create/replace the biometric-gated item with the given bytes.
/// - [`KekStore::read_biometric`] — read the biometric-gated item's value (prompts Touch ID on real
///   macOS; the reason string is shown on the sheet). This is the actual unlock auth. Returns
///   `Ok(None)` for an AUTHORITATIVE not-found (`errSecItemNotFound` from the prompting read) —
///   the ONLY existence signal [`resolve_kek`] trusts. A cancelled/failed/denied auth is an `Err`,
///   NEVER `None`, so an auth hiccup can never be mistaken for "no key exists".
/// - [`KekStore::delete_plain`] — delete the legacy plain item (idempotent).
///
/// NOTE (2026-07-05 field incident): the trait previously had a no-prompt `biometric_exists` probe
/// (`kSecUseAuthenticationUISkip` + `kSecReturnAttributes`) and the resolution trusted it. On
/// current macOS that probe returns `errSecItemNotFound` for ACL-protected data-protection items
/// EVEN WHEN THE ITEM EXISTS (observed: a write reported success and the probe missed it 37 s later
/// in the same process), so the resolution repeatedly minted fresh KEKs "over" folders sealed under
/// the real one. The probe is GONE from the resolution path — existence is only ever decided by the
/// prompting read.
trait KekStore {
    fn read_plain(&self) -> Result<Option<[u8; 32]>>;
    fn write_biometric(&self, key: &[u8; 32]) -> Result<()>;
    fn read_biometric(&self, reason: &str) -> Result<Option<[u8; 32]>>;
    fn delete_plain(&self) -> Result<()>;
}

/// One-time, idempotent, value-preserving master-KEK resolution + migration — READ-FIRST.
///
/// Steady state (biometric item present): a SINGLE prompting biometric read → the lone Touch ID
/// sheet — and that read's result is the ONLY existence evidence used (see the [`KekStore`] note:
/// the old no-prompt probe lies for ACL'd items on current macOS and minted fresh KEKs over sealed
/// folders).
///
/// Legacy PLAIN item present (biometric read authoritatively not-found): read the plain 32 bytes,
/// re-store the SAME bytes as the biometric-gated item, then **confirm by VALUE** (read the
/// biometric item back and assert byte-identical) BEFORE deleting the plain item. If the confirm
/// fails for ANY reason we return the error and DO NOT delete the plain item, so access to existing
/// folders is never lost.
///
/// Neither item exists: `allow_mint` decides. `true` (nothing is sealed anywhere) → generate a
/// random 32-byte KEK, store it biometric-gated, return the in-memory bytes. `false` (sealed
/// folders EXIST — the caller checked the DB) → REFUSE with a loud error: a fresh KEK cannot
/// unwrap any existing folder's content key, so minting here silently orphans every sealed folder
/// (the 2026-07-05 field incident). The sealed data stays intact and recoverable; the error is the
/// correct outcome.
fn resolve_kek<S: KekStore>(store: &S, reason: &str, allow_mint: bool) -> Result<[u8; 32]> {
    // READ FIRST: the prompting read is the authoritative existence check.
    if let Some(kek) = store.read_biometric(reason)? {
        // A stray leftover plain item (e.g. a crash AFTER the biometric write but BEFORE the plain
        // delete on a previous run) is cleaned up opportunistically here, but ONLY after we have
        // confirmed the biometric value equals it — never a blind delete.
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

    // Authoritatively no biometric item. Is there a legacy plain item to migrate?
    match store.read_plain()? {
        Some(plain) => {
            // Migrate: write the SAME bytes biometric-gated, then CONFIRM BY VALUE before deleting.
            store.write_biometric(&plain)?;
            let confirm = store.read_biometric(reason)?.ok_or_else(|| {
                AppError::Secrets(
                    "master-KEK migration read-back found no item — keeping the plain item, retry next launch"
                        .into(),
                )
            })?;
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
        None if allow_mint => {
            // Genuinely fresh (nothing sealed anywhere): mint a random KEK, store it biometric-gated,
            // return the in-memory bytes (no read-back, no Touch ID prompt for the first lock).
            let fresh = crate::crypto::random_key()?;
            store.write_biometric(&fresh)?;
            tracing::info!(
                target: "secrets",
                "created a fresh biometric-gated master KEK"
            );
            Ok(fresh)
        }
        None => {
            tracing::error!(
                target: "secrets",
                "master KEK not found in ANY keychain store while sealed folders exist — REFUSING to mint a replacement (the sealed data would be orphaned); see the recovery path"
            );
            Err(AppError::Secrets(
                "the folder master key was not found in the keychain — Murmur will NOT create a new one because locked folders exist (their data stays intact); try unlocking again to run key recovery"
                    .into(),
            ))
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
        pub static kSecMatchLimitAll: CFStringRef;
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
    /// Read the legacy PLAIN item via the `keyring` crate (the exact account+store the old code
    /// wrote — the FILE-BASED keychain). MUST bypass the new data-protection routing in `get_secret`
    /// (the legacy KEK lives in the file-based store, and the gated KEK is read via `read_biometric`,
    /// never as a plain string). Never prompts. `Ok(None)` if absent.
    fn read_plain(&self) -> Result<Option<[u8; 32]>> {
        match legacy_get_secret(ACCOUNT_MASTER_KEK)? {
            Some(hex) => match hex_to_key32(&hex) {
                Some(k) => Ok(Some(k)),
                None => Err(AppError::Secrets(
                    "legacy plain master KEK is malformed".into(),
                )),
            },
            None => Ok(None),
        }
    }

    /// Create the biometric-gated item: build a `SecAccessControl` requiring user presence
    /// (`kSecAccessControlUserPresence` = Touch ID OR device passcode) with accessibility
    /// `kSecAttrAccessibleWhenUnlockedThisDeviceOnly`, then `SecItemAdd` the 32 raw bytes under it.
    ///
    /// `errSecDuplicateItem` is a CONTRADICTION and REFUSES (2026-07-05 hardening): this method is
    /// only ever reached after the prompting read said not-found, so a duplicate means the read and
    /// the add disagree about the store's contents — exactly the query-shape divergence behind the
    /// field incident. The old delete-and-re-add "idempotent replace" would `SecItemDelete` EVERY
    /// hidden generation under the account, destroying the very keys the recovery path needs.
    /// Never delete what the read cannot see.
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

        let mut q = self.base_query();
        unsafe {
            q.add(&(kSecAttrAccessControl as *const _), &access.as_CFTypeRef());
            q.add(&(kSecValueData as *const _), &data.as_CFTypeRef());
        }
        let dict = q.to_immutable();
        let mut out: core_foundation::base::CFTypeRef = std::ptr::null();
        let status = unsafe { SecItemAdd(dict.as_concrete_TypeRef(), &mut out) };
        if !out.is_null() {
            unsafe { drop(core_foundation::base::CFType::wrap_under_create_rule(out)) };
        }

        if status == errSecSuccess {
            return Ok(());
        }
        if status == errSecDuplicateItem {
            tracing::error!(
                target: "secrets",
                "master-KEK add hit errSecDuplicateItem though the prompting read found no item — keychain query shapes disagree; REFUSING to delete-and-replace (hidden key generations stay intact)"
            );
            return Err(AppError::Secrets(
                "a master-key item already exists in the keychain even though it could not be read — refusing to replace it; existing locked folders stay recoverable".into(),
            ));
        }
        Err(map_osstatus("add biometric KEK item", status))
    }

    /// Read the biometric-gated item's value. On real macOS this triggers the Touch ID / passcode
    /// sheet with `reason` shown via `kSecUseOperationPrompt`, and returns the 32 raw bytes on a
    /// successful presence check. `Ok(None)` ONLY for `errSecItemNotFound` from this prompting read
    /// — the authoritative "no item exists" signal (the no-UI probe lies for ACL'd items on current
    /// macOS). A user cancel / auth failure maps to [`AppError::BiometricFailed`], never `None`.
    fn read_biometric(&self, reason: &str) -> Result<Option<[u8; 32]>> {
        use crate::secrets::keychain::sec_consts::{
            ERR_SEC_INTERACTION_NOT_ALLOWED, ERR_SEC_USER_CANCELED,
        };
        use core_foundation::base::{CFType, TCFType};
        use core_foundation::boolean::CFBoolean;
        use core_foundation::data::CFData;
        use core_foundation::string::CFString;
        use security_framework_sys::base::{errSecAuthFailed, errSecItemNotFound, errSecSuccess};
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
                s if s == errSecItemNotFound => Ok(None),
                s if s == ERR_SEC_USER_CANCELED => {
                    Err(AppError::BiometricFailed(crate::errcode::tag(
                        crate::errcode::TOUCH_ID_CANCELLED,
                        "Touch ID was cancelled",
                    )))
                }
                s if s == errSecAuthFailed => {
                    Err(AppError::BiometricFailed(crate::errcode::tag(
                        crate::errcode::TOUCH_ID_FAILED,
                        "authentication failed",
                    )))
                }
                s if s == ERR_SEC_INTERACTION_NOT_ALLOWED => {
                    Err(AppError::BiometricFailed(crate::errcode::tag(
                        crate::errcode::TOUCH_ID_FAILED,
                        "interaction not allowed (no UI context to present Touch ID)",
                    )))
                }
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
        Ok(Some(k))
    }

    /// Delete the legacy PLAIN item via the `keyring` crate (FILE-BASED store; idempotent; absence
    /// is not an error). Bypasses the new DP routing for the same reason as `read_plain`.
    fn delete_plain(&self) -> Result<()> {
        legacy_delete_secret(ACCOUNT_MASTER_KEK)
    }
}

#[cfg(target_os = "macos")]
impl MacKekStore {
    /// EVERY biometric-gated master-KEK item's bytes (`kSecMatchLimitAll` + `kSecReturnData`, one
    /// user-presence prompt for the batch). On machines where the no-UI probe lies, several KEK
    /// generations can coexist (each blind "fresh mint" added one) — the RECOVERY path tries each
    /// against a folder's wrapped content key. `errSecItemNotFound` ⇒ empty vec. Read-only.
    fn read_biometric_all(&self, reason: &str) -> Result<Vec<[u8; 32]>> {
        use crate::secrets::keychain::sec_consts::{
            ERR_SEC_INTERACTION_NOT_ALLOWED, ERR_SEC_USER_CANCELED,
        };
        use core_foundation::array::{CFArrayGetCount, CFArrayGetTypeID, CFArrayGetValueAtIndex};
        use core_foundation::base::{CFGetTypeID, CFType, TCFType};
        use core_foundation::boolean::CFBoolean;
        use core_foundation::data::CFData;
        use core_foundation::string::CFString;
        use security_framework_sys::base::{errSecAuthFailed, errSecItemNotFound, errSecSuccess};
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
                &(sec_consts::kSecMatchLimitAll as *const _),
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
            if !out.is_null() {
                unsafe { drop(CFType::wrap_under_create_rule(out)) };
            }
            return match status {
                s if s == errSecItemNotFound => Ok(Vec::new()),
                s if s == ERR_SEC_USER_CANCELED => {
                    Err(AppError::BiometricFailed(crate::errcode::tag(
                        crate::errcode::TOUCH_ID_CANCELLED,
                        "Touch ID was cancelled",
                    )))
                }
                s if s == errSecAuthFailed => {
                    Err(AppError::BiometricFailed(crate::errcode::tag(
                        crate::errcode::TOUCH_ID_FAILED,
                        "authentication failed",
                    )))
                }
                s if s == ERR_SEC_INTERACTION_NOT_ALLOWED => {
                    Err(AppError::BiometricFailed(crate::errcode::tag(
                        crate::errcode::TOUCH_ID_FAILED,
                        "interaction not allowed (no UI context to present Touch ID)",
                    )))
                }
                other => Err(map_osstatus("enumerate biometric KEK items", other)),
            };
        }
        if out.is_null() {
            return Ok(Vec::new());
        }

        // SAFETY: success + non-null ⇒ a CF object we own under the create rule. With MatchLimitAll
        // it is a CFArray of CFData; be defensive and also accept a bare CFData (single item).
        let owned = unsafe { CFType::wrap_under_create_rule(out) };
        let mut keys: Vec<[u8; 32]> = Vec::new();
        let push_data = |keys: &mut Vec<[u8; 32]>, data: &CFData| {
            if let Ok(k) = <&[u8] as TryInto<[u8; 32]>>::try_into(data.bytes()) {
                keys.push(k);
            }
            // Wrong-length values are skipped silently — a foreign item under our account name is
            // not a KEK candidate; never log its bytes.
        };
        unsafe {
            let type_id = CFGetTypeID(out);
            if type_id == CFArrayGetTypeID() {
                let arr = out as core_foundation::array::CFArrayRef;
                let n = CFArrayGetCount(arr);
                for i in 0..n {
                    let item = CFArrayGetValueAtIndex(arr, i);
                    if !item.is_null() && CFGetTypeID(item as _) == CFData::type_id() {
                        let data = CFData::wrap_under_get_rule(item as *const _);
                        push_data(&mut keys, &data);
                    }
                }
            } else if type_id == CFData::type_id() {
                let data = CFData::wrap_under_get_rule(out as *const _);
                push_data(&mut keys, &data);
            }
        }
        drop(owned);
        Ok(keys)
    }

    // NOTE: there is deliberately NO `delete_biometric` here anymore (2026-07-05 hardening). The
    // only caller was `write_biometric`'s duplicate-replace arm, and a blind
    // `SecItemDelete(base_query)` destroys EVERY generation under the account — including hidden
    // ones the recovery path depends on. No production path may delete a master-KEK item.
}

/// Map a non-success Security `OSStatus` to a typed [`AppError`]. The message carries only the
/// numeric status + context — never the key value — so it is safe to log under the no-secret rule.
#[cfg(target_os = "macos")]
fn map_osstatus(ctx: &str, status: core_foundation::base::OSStatus) -> AppError {
    // Every data-protection-keychain failure funnels through here — trace it (ctx + raw OSStatus
    // only, NEVER key material) so a field failure on a signed build is diagnosable from
    // `murmur.log` instead of vanishing into a generic FE toast (the pre-0.7.3 blind spot).
    tracing::warn!(
        target: "secrets",
        context = %ctx,
        osstatus = status,
        "data-protection keychain operation failed"
    );
    if status == sec_consts::ERR_SEC_INTERACTION_NOT_ALLOWED {
        // The keychain is locked / no UI context — treat as a denied access, recoverable.
        return AppError::KeychainDenied(crate::errcode::tag(
            crate::errcode::KEYCHAIN_DENIED,
            format!("{ctx}: OSStatus {status}"),
        ));
    }
    if status == MISSING_ENTITLEMENT_STATUS {
        // Unsigned/ad-hoc dev build: the data-protection keychain entitlement is absent. Build the
        // dedicated marker error so the NON-GATED string-secret API can recognize exactly this status
        // and fall back to the legacy file-based keyring (dev-only; a signed release never gets here).
        return missing_entitlement_err(ctx);
    }
    AppError::Secrets(format!("{ctx}: OSStatus {status}"))
}

/// The data-protection `errSecMissingEntitlement` `OSStatus` (-34018, from `<Security/SecBase.h>`):
/// the calling binary lacks the data-protection-keychain entitlement. On an UNSIGNED / ad-hoc-signed
/// dev build (`tauri dev`) every data-protection `SecItem*` op returns this, so the DP keychain is
/// effectively unavailable. A SIGNED release build HAS the entitlement → this status never occurs →
/// the dev-only legacy fallback keyed on it is unreachable in release. Cross-platform `i32` so the
/// marker logic + tests compile on CI (Linux) too, even though the status only ever arises from a
/// macOS `SecItem*` call.
const MISSING_ENTITLEMENT_STATUS: i32 = -34018;

/// Stable, non-PII marker prefixed onto the [`AppError`] message for `errSecMissingEntitlement` so
/// the non-gated string-secret API ([`set_secret`]/[`get_secret`]/[`delete_secret`]) can detect
/// EXACTLY that status — and ONLY that status — and fall back to the legacy keyring on an unsigned
/// dev build. NOT a blanket catch-all: any other DP failure (e.g. -25300 notFound is handled inline
/// as `Ok(None)`; -50 param; a real DP error) is NOT matched here and surfaces as today.
const MISSING_ENTITLEMENT_MARKER: &str = "errSecMissingEntitlement";

/// Build the typed -34018 signal error. Carries only the marker, context, and numeric status — never
/// a secret value — so it is safe to log under the no-PII rule. `KeychainDenied` (recoverable) so a
/// non-falling-back caller still shows a clean message rather than crashing.
fn missing_entitlement_err(ctx: &str) -> AppError {
    AppError::KeychainDenied(format!(
        "{MISSING_ENTITLEMENT_MARKER}: {ctx}: OSStatus {MISSING_ENTITLEMENT_STATUS}"
    ))
}

/// True iff `err` is the data-protection `errSecMissingEntitlement` (-34018) signal produced by
/// [`missing_entitlement_err`]. Matches the specific marker + the exact numeric status, never a broad
/// class of keychain errors — so the legacy fallback can never mask a genuine DP failure in a signed
/// release.
#[cfg_attr(debug_assertions, allow(dead_code))] // release-only: debug routes string secrets to the dev file store
fn is_missing_entitlement(err: &AppError) -> bool {
    matches!(
        err,
        AppError::KeychainDenied(msg)
            if msg.starts_with(MISSING_ENTITLEMENT_MARKER)
                && msg.contains(&format!("OSStatus {MISSING_ENTITLEMENT_STATUS}"))
    )
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
    /// Write/replace the legacy file-based (keyring) item. Used ONLY by the unsigned-dev-build
    /// errSecMissingEntitlement (-34018) fallback, where the data-protection keychain is unavailable.
    fn write_legacy(&self, secret: &str) -> Result<()>;
}

// ───────────── unsigned-dev-build (-34018) fallback routing over the DpStringStore seam ─────────────
//
// On a SIGNED release build the data-protection entitlement is present → the DP ops below succeed →
// `is_missing_entitlement` is never true → these helpers behave identically to "DP only" and the
// legacy keyring is never written. On an UNSIGNED/ad-hoc dev build every DP `SecItem*` returns
// errSecMissingEntitlement (-34018) → the helpers route the op to the legacy keyring (which works
// without the entitlement), keeping set/get/delete consistent so a dev-written secret round-trips.
// Pulled out as free functions over the trait so they can be unit-tested with an in-memory fake DP
// store that returns -34018 (no live keychain, no signature).

/// `set_secret` over the store seam: DP write (then clear any legacy copy); on -34018 write to legacy.
#[cfg_attr(debug_assertions, allow(dead_code))] // release-only: debug routes string secrets to the dev file store
fn dp_set_or_legacy<S: DpStringStore>(store: &S, secret: &str) -> Result<()> {
    match store.write_dp(secret) {
        Ok(()) => {
            let _ = store.delete_legacy();
            Ok(())
        }
        Err(ref e) if is_missing_entitlement(e) => {
            tracing::warn!(
                target: "secrets",
                "data-protection keychain unavailable (errSecMissingEntitlement / unsigned dev build) — storing secret in the legacy keyring"
            );
            store.write_legacy(secret)
        }
        Err(e) => Err(e),
    }
}

/// `get_secret` over the store seam: migrate/read DP; on -34018 read from legacy (so a secret written
/// via the set fallback — also legacy — reads back).
#[cfg_attr(debug_assertions, allow(dead_code))] // release-only: debug routes string secrets to the dev file store
fn dp_get_or_legacy<S: DpStringStore>(store: &S) -> Result<Option<String>> {
    match migrate_or_read_dp(store) {
        Ok(v) => Ok(v),
        Err(ref e) if is_missing_entitlement(e) => {
            tracing::warn!(
                target: "secrets",
                "data-protection keychain unavailable (errSecMissingEntitlement / unsigned dev build) — reading secret from the legacy keyring"
            );
            store.read_legacy()
        }
        Err(e) => Err(e),
    }
}

/// `delete_secret` over the store seam: DP delete (then clear any legacy copy); on -34018 delete from
/// legacy.
#[cfg_attr(debug_assertions, allow(dead_code))] // release-only: debug routes string secrets to the dev file store
fn dp_delete_or_legacy<S: DpStringStore>(store: &S) -> Result<()> {
    match store.delete_dp() {
        Ok(()) => {
            let _ = store.delete_legacy();
            Ok(())
        }
        Err(ref e) if is_missing_entitlement(e) => {
            tracing::warn!(
                target: "secrets",
                "data-protection keychain unavailable (errSecMissingEntitlement / unsigned dev build) — deleting secret from the legacy keyring"
            );
            store.delete_legacy()
        }
        Err(e) => Err(e),
    }
}

/// Resolve a string secret, migrating a legacy keyring item to the data-protection keychain ONCE,
/// value-preservingly. Steady state (already in DP): a single `read_dp`. First run with a legacy
/// item: read it, write the SAME string to DP, CONFIRM BY VALUE (read DP back, assert equal) BEFORE
/// deleting the legacy item — so a crash mid-migration never loses the secret. Neither present ⇒
/// `Ok(None)` (caller mints a fresh one and `set_secret`s it).
#[cfg_attr(debug_assertions, allow(dead_code))] // release-only: debug routes string secrets to the dev file store
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
#[cfg_attr(debug_assertions, allow(dead_code))] // release-only: debug routes string secrets to the dev file store
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
#[cfg_attr(debug_assertions, allow(dead_code))] // release-only: debug routes string secrets to the dev file store
struct MacDpStore {
    account: &'static str,
}

#[cfg(target_os = "macos")]
#[cfg_attr(debug_assertions, allow(dead_code))] // release-only: debug routes string secrets to the dev file store
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
            // Lost a race with another writer — delete + retry once. Safe here (unlike the master-KEK
            // write, which REFUSES): these are NON-gated single-value secrets (DEK / MCP token / API
            // keys / share tokens) with no multi-generation semantics, and `delete_dp` is scoped to
            // this one account. The brief delete-before-add window is inherent to SecItem* (there is
            // no write-to-temp-then-atomic-rename); every value here is regenerable or re-enterable
            // (re-set / re-paste / re-login), and the DEK's only writer is its first-ever mint (never
            // re-written in steady state), so nothing at-rest is orphaned by a crash in the window.
            self.delete_dp()?;
            let s2 = add();
            if s2 == errSecSuccess {
                return Ok(());
            }
            return Err(map_osstatus(
                "add data-protection secret (after replace)",
                s2,
            ));
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

    fn write_legacy(&self, secret: &str) -> Result<()> {
        legacy_set_secret(self.account, secret)
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
    // DEBUG-ONLY: route the non-gated string secrets to the dev plaintext file store (NO keychain) so
    // an unsigned dev build never hits errSecMissingEntitlement (-34018) or the legacy-keyring prompt
    // spam. Compiled out of release; a signed release uses the data-protection keychain below.
    #[cfg(debug_assertions)]
    {
        dev_set_secret_at(&dev_secrets_path()?, account, secret)
    }
    #[cfg(all(target_os = "macos", not(debug_assertions)))]
    {
        // DP write; on an unsigned dev build the DP op returns errSecMissingEntitlement (-34018) and
        // the helper falls back to the legacy keyring. Unreachable in a signed release.
        let store = MacDpStore {
            account: leak_account(account),
        };
        dp_set_or_legacy(&store, secret)
    }
    #[cfg(all(not(target_os = "macos"), not(debug_assertions)))]
    {
        legacy_set_secret(account, secret)
    }
}

/// Read a secret. `Ok(None)` if absent. On macOS this also performs the one-time, value-preserving
/// migration of a legacy keyring item into the data-protection keychain.
pub fn get_secret(account: &str) -> Result<Option<String>> {
    // DEBUG-ONLY: read the dev plaintext file store (NO keychain) — see `set_secret`.
    #[cfg(debug_assertions)]
    {
        dev_get_secret_at(&dev_secrets_path()?, account)
    }
    #[cfg(all(target_os = "macos", not(debug_assertions)))]
    {
        // Migrate/read DP; on an unsigned dev build the DP read returns -34018 and the helper reads
        // from the legacy keyring (where the set fallback wrote it). Unreachable in a signed release.
        let store = MacDpStore {
            account: leak_account(account),
        };
        dp_get_or_legacy(&store)
    }
    #[cfg(all(not(target_os = "macos"), not(debug_assertions)))]
    {
        legacy_get_secret(account)
    }
}

/// Delete a secret (idempotent). On macOS removes BOTH the data-protection item and any legacy
/// file-based item.
pub fn delete_secret(account: &str) -> Result<()> {
    // DEBUG-ONLY: delete from the dev plaintext file store (NO keychain) — see `set_secret`.
    #[cfg(debug_assertions)]
    {
        dev_delete_secret_at(&dev_secrets_path()?, account)
    }
    #[cfg(all(target_os = "macos", not(debug_assertions)))]
    {
        // DP delete; on an unsigned dev build the DP op returns -34018 and the helper deletes from
        // the legacy keyring (where the dev secret lives). Unreachable in a signed release.
        let store = MacDpStore {
            account: leak_account(account),
        };
        dp_delete_or_legacy(&store)
    }
    #[cfg(all(not(target_os = "macos"), not(debug_assertions)))]
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
#[cfg_attr(debug_assertions, allow(dead_code))] // release-only: debug routes string secrets to the dev file store
fn leak_account(account: &str) -> &'static str {
    match account {
        ACCOUNT_DB_DEK => ACCOUNT_DB_DEK,
        ACCOUNT_MASTER_KEK => ACCOUNT_MASTER_KEK,
        ACCOUNT_MCP_TOKEN => ACCOUNT_MCP_TOKEN,
        ACCOUNT_ANTHROPIC_KEY => ACCOUNT_ANTHROPIC_KEY,
        ACCOUNT_WEB_SEARCH_KEY => ACCOUNT_WEB_SEARCH_KEY,
        ACCOUNT_JIRA_TOKEN => ACCOUNT_JIRA_TOKEN,
        ACCOUNT_SLACK_TOKEN => ACCOUNT_SLACK_TOKEN,
        ACCOUNT_NOTION_TOKEN => ACCOUNT_NOTION_TOKEN,
        ACCOUNT_CLICKUP_TOKEN => ACCOUNT_CLICKUP_TOKEN,
        ACCOUNT_GATEWAY_KEY => ACCOUNT_GATEWAY_KEY,
        // The 7 sharing-session accounts (share::mod KC_* constants). Matched by literal so the
        // hot set/get/delete paths (account_status → load_tokens is FE-polled) return a &'static str
        // with NO allocation — otherwise every call `Box::leak`s a fresh copy of the (non-secret,
        // fixed) account NAME, accruing a few KB/day for a heavy sharing user. The token VALUES are
        // never involved here. Keep these in sync with share/mod.rs.
        "murmur_share_access_token" => "murmur_share_access_token",
        "murmur_share_refresh_token" => "murmur_share_refresh_token",
        "murmur_share_device_id" => "murmur_share_device_id",
        "murmur_share_account_email" => "murmur_share_account_email",
        "murmur_share_account_id" => "murmur_share_account_id",
        "murmur_share_generation" => "murmur_share_generation",
        "murmur_share_access_expires_at" => "murmur_share_access_expires_at",
        // Last-resort for a genuinely-unknown account name (never a real Murmur account) — bounded,
        // one-time-per-distinct-name, non-secret.
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

// ───────────────────── DEBUG-ONLY dev file store for the non-gated string secrets ─────────────────
//
// Mirrors the MURMUR_DEV_DEK / MURMUR_DEV_KEK philosophy (dev avoids the keychain entirely) for the
// NON-biometric string secrets — MCP token, Anthropic key, Brave web-search key. In an UNSIGNED dev
// build (`tauri dev`) the macOS data-protection keychain returns errSecMissingEntitlement (-34018)
// and the legacy keyring re-prompts the login-keychain password on every rebuild (new ad-hoc
// signature ⇒ ACL mismatch). To kill that prompt-spam AND the -34018 failure, a DEBUG build routes
// these three secrets to a plaintext JSON map in the DEV data dir instead of ANY keychain.
//
// LOAD-BEARING: this whole region is compiled out of release (`#[cfg(debug_assertions)]`). A signed
// release build NEVER touches this file store — `set_secret`/`get_secret`/`delete_secret` keep the
// exact data-protection-keychain path below. Dev is not a security boundary (it is the same trust
// posture as the fixed MURMUR_DEV_DEK key); the file is plaintext on purpose, under the already-
// location-gitignored app-support dir, written 0600 where the platform allows it.
//
// The biometric master KEK is NOT routed here — it keeps its own MURMUR_DEV_KEK env hatch +
// kSecAttrAccessControl path. Only the three non-gated string accounts use this store.

/// Filename of the dev-only plaintext secret map under the dev data dir.
#[cfg(debug_assertions)]
const DEV_SECRETS_FILE: &str = "dev-secrets.json";

/// Resolve the dev-only secrets file path: `<app-data>/MeetNotes-dev/dev-secrets.json`. Mirrors the
/// DB/audio dir resolution (`dirs::data_dir().join(app_dir_name())`) so the dev secrets live beside
/// the dev DB, isolated from the release `MeetNotes` dir. Creates the parent dir if absent.
#[cfg(debug_assertions)]
fn dev_secrets_path() -> Result<std::path::PathBuf> {
    let base = dirs::data_dir()
        .ok_or_else(|| AppError::Secrets("could not resolve app-data directory".into()))?;
    let dir = base.join(crate::state::app_dir_name());
    std::fs::create_dir_all(&dir)
        .map_err(|e| AppError::Secrets(format!("create dev-secrets dir: {e}")))?;
    Ok(dir.join(DEV_SECRETS_FILE))
}

/// Read the dev secret map from `path`. A MISSING file ⇒ an empty map (NOT an error) — the very
/// first dev access has no file yet. A present-but-malformed file is a hard error (don't silently
/// drop a dev's saved keys). Never logs the values.
#[cfg(debug_assertions)]
fn dev_read_map(path: &std::path::Path) -> Result<std::collections::BTreeMap<String, String>> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| AppError::Secrets(format!("parse dev-secrets file: {e}"))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(std::collections::BTreeMap::new()),
        Err(e) => Err(AppError::Secrets(format!("read dev-secrets file: {e}"))),
    }
}

/// Persist the dev secret map to `path` as pretty JSON, best-effort 0600. Writes to a temp sibling
/// then renames so a crash mid-write never truncates the existing map. Never logs the values.
#[cfg(debug_assertions)]
fn dev_write_map(
    path: &std::path::Path,
    map: &std::collections::BTreeMap<String, String>,
) -> Result<()> {
    let json = serde_json::to_vec_pretty(map)
        .map_err(|e| AppError::Secrets(format!("serialize dev-secrets: {e}")))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &json)
        .map_err(|e| AppError::Secrets(format!("write dev-secrets temp: {e}")))?;
    // Best-effort owner-only perms (dev convenience; not a security boundary).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, path)
        .map_err(|e| AppError::Secrets(format!("commit dev-secrets file: {e}")))?;
    Ok(())
}

/// `set_secret` against an explicit dev-store path (test seam). Inserts/replaces `account`.
#[cfg(debug_assertions)]
fn dev_set_secret_at(path: &std::path::Path, account: &str, secret: &str) -> Result<()> {
    let mut map = dev_read_map(path)?;
    map.insert(account.to_string(), secret.to_string());
    dev_write_map(path, &map)
}

/// `get_secret` against an explicit dev-store path (test seam). Missing account / missing file ⇒
/// `Ok(None)`.
#[cfg(debug_assertions)]
fn dev_get_secret_at(path: &std::path::Path, account: &str) -> Result<Option<String>> {
    Ok(dev_read_map(path)?.get(account).cloned())
}

/// `delete_secret` against an explicit dev-store path (test seam). Absent account / missing file ⇒
/// `Ok(())` (idempotent).
#[cfg(debug_assertions)]
fn dev_delete_secret_at(path: &std::path::Path, account: &str) -> Result<()> {
    let mut map = dev_read_map(path)?;
    if map.remove(account).is_some() {
        dev_write_map(path, &map)?;
    }
    Ok(())
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

    /// DRIFT GUARD: the connector token accounts named here MUST equal the constants the connectors
    /// actually read/write, or the data-protection routing (`leak_account`) silently falls through to
    /// the unknown-account arm and the secret lands under a different item than it is read from.
    #[test]
    fn connector_token_accounts_mirror_the_connector_constants() {
        assert_eq!(
            ACCOUNT_JIRA_TOKEN,
            crate::connectors::jira::JIRA_TOKEN_ACCOUNT
        );
        assert_eq!(
            ACCOUNT_SLACK_TOKEN,
            crate::connectors::slack::SLACK_TOKEN_ACCOUNT
        );
        assert_eq!(
            ACCOUNT_NOTION_TOKEN,
            crate::connectors::notion::NOTION_TOKEN_ACCOUNT
        );
        assert_eq!(
            ACCOUNT_CLICKUP_TOKEN,
            crate::connectors::clickup::CLICKUP_TOKEN_ACCOUNT
        );
        assert_eq!(
            ACCOUNT_WEB_SEARCH_KEY,
            crate::connectors::web::WEB_SEARCH_KEY_ACCOUNT
        );
        // Every connector account is DISTINCT — no two BYO credentials may share a Keychain item.
        let accounts = [
            ACCOUNT_JIRA_TOKEN,
            ACCOUNT_SLACK_TOKEN,
            ACCOUNT_NOTION_TOKEN,
            ACCOUNT_CLICKUP_TOKEN,
            ACCOUNT_WEB_SEARCH_KEY,
            ACCOUNT_ANTHROPIC_KEY,
            ACCOUNT_GATEWAY_KEY,
        ];
        let unique: std::collections::BTreeSet<&str> = accounts.iter().copied().collect();
        assert_eq!(unique.len(), accounts.len(), "keychain accounts must be distinct");
    }

    #[test]
    fn hex_to_key32_rejects_malformed() {
        assert_eq!(hex_to_key32("tooshort"), None);
        assert_eq!(hex_to_key32(&"z".repeat(64)), None);
        assert_eq!(hex_to_key32(&"a".repeat(63)), None);
    }

    #[test]
    fn valid_dev_kek_is_isolated_but_never_authoritative_for_discard() {
        const DEV_KEK: &str = "1111111111111111111111111111111111111111111111111111111111111111";

        let candidates = dev_kek_candidates(Some(DEV_KEK), false)
            .expect("valid dev KEK is selected")
            .unwrap();
        assert_eq!(
            candidates.as_slice(),
            &[[0x11; 32]],
            "a valid debug KEK must not be mixed with release-Keychain generations"
        );
        assert!(
            matches!(
                dev_kek_candidates(Some(DEV_KEK), true),
                Some(Err(AppError::Unavailable(_)))
            ),
            "an isolated dev candidate cannot prove that an older Keychain KEK is absent"
        );
        assert!(
            dev_kek_candidates(Some("not-a-key"), false).is_none(),
            "a malformed hatch must fall through to the normal Keychain source"
        );
    }

    #[test]
    fn ct_eq_matches_and_differs() {
        let a: [u8; 32] = std::array::from_fn(|i| i as u8);
        let mut b = a;
        assert!(ct_eq(&a, &b));
        b[17] ^= 0x01;
        assert!(!ct_eq(&a, &b));
    }

    /// Debug-path (dev file store) round-trip for the biometric sharing-account MK cache. A LIVE
    /// biometric read can't run headlessly, so this exercises the debug seam the same way a dev build
    /// caches/restores the MK: cache → `cached()` true → read back byte-identical → clear → `cached()`
    /// false → a subsequent read fails closed with `Unavailable`. Runs against an explicit temp path
    /// (hermetic — never touches the real dev-secrets file).
    #[test]
    fn account_mk_dev_cache_round_trips() {
        let dir = std::env::temp_dir().join(format!("murmur-mk-cache-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("dev-secrets.json");

        let mk: [u8; 32] = std::array::from_fn(|i| (i as u8).wrapping_mul(7).wrapping_add(3));

        // Absent to start.
        assert!(!account_mk_cached_at(&path).unwrap());
        assert!(matches!(
            read_account_mk_at(&path),
            Err(AppError::Unavailable(_))
        ));

        // Cache → present → reads back byte-identical.
        cache_account_mk_at(&path, &mk).unwrap();
        assert!(account_mk_cached_at(&path).unwrap());
        assert_eq!(read_account_mk_at(&path).unwrap(), mk);

        // Overwrite is idempotent (create-or-replace).
        let mk2: [u8; 32] = std::array::from_fn(|i| (i as u8).wrapping_add(1));
        cache_account_mk_at(&path, &mk2).unwrap();
        assert_eq!(read_account_mk_at(&path).unwrap(), mk2);

        // Clear → absent → fails closed.
        clear_account_mk_at(&path).unwrap();
        assert!(!account_mk_cached_at(&path).unwrap());
        assert!(matches!(
            read_account_mk_at(&path),
            Err(AppError::Unavailable(_))
        ));
        // Clear is idempotent.
        clear_account_mk_at(&path).unwrap();

        let _ = std::fs::remove_dir_all(&dir);
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
        fn read_plain(&self) -> Result<Option<[u8; 32]>> {
            self.log.borrow_mut().push(Op::ReadPlain);
            Ok(*self.plain.borrow())
        }
        fn write_biometric(&self, key: &[u8; 32]) -> Result<()> {
            self.log.borrow_mut().push(Op::WriteBiometric(*key));
            *self.biometric.borrow_mut() = Some(*key);
            Ok(())
        }
        fn read_biometric(&self, _reason: &str) -> Result<Option<[u8; 32]>> {
            self.log.borrow_mut().push(Op::ReadBiometric);
            let Some(stored) = *self.biometric.borrow() else {
                return Ok(None); // authoritative not-found (mirrors errSecItemNotFound)
            };
            if self.corrupt_biometric_read {
                // Simulate a copy that read back DIFFERENT bytes than were written.
                let mut bad = stored;
                bad[0] ^= 0xFF;
                return Ok(Some(bad));
            }
            Ok(Some(stored))
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
        let out = resolve_kek(&store, "Unlock this folder", false).unwrap();

        // The returned KEK is byte-for-byte the original plain value.
        assert_eq!(out, KEK, "migrated KEK must equal the original plain KEK");
        // The biometric item now holds exactly the original bytes.
        assert_eq!(*store.biometric.borrow(), Some(KEK));
        // The plain item is gone.
        assert_eq!(*store.plain.borrow(), None);

        // Order proof: the write + a confirming read happen BEFORE the plain delete.
        let log = store.log();
        let wrote = log
            .iter()
            .position(|o| matches!(o, Op::WriteBiometric(_)))
            .unwrap();
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
        let res = resolve_kek(&store, "Unlock this folder", false);

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
        let out = resolve_kek(&store, "Unlock this folder", true).unwrap();

        // A biometric item now exists holding exactly the returned bytes.
        assert_eq!(*store.biometric.borrow(), Some(out));
        // Fresh path: the read-first existence check runs BEFORE the mint (a missing item returns
        // not-found without presenting any UI), the mint itself does no read-back after the write,
        // and there is no plain delete.
        let log = store.log();
        let wrote = log
            .iter()
            .position(|o| matches!(o, Op::WriteBiometric(_)))
            .expect("fresh create must write the biometric item");
        assert!(
            !log.iter().skip(wrote + 1).any(|o| *o == Op::ReadBiometric),
            "fresh create must not read back after the write (no Touch ID prompt for the first lock)"
        );
        assert!(
            !log.contains(&Op::DeletePlain),
            "fresh create has no plain item to delete"
        );
    }

    /// THE 2026-07-05 FIELD REGRESSION: the master KEK is missing from every store while sealed
    /// folders exist (`allow_mint = false`). The resolution MUST refuse — the old code minted a
    /// fresh KEK here, silently orphaning every folder sealed under the real one. Nothing may be
    /// written to any store on the refusal path.
    #[test]
    fn missing_kek_with_sealed_content_refuses_to_mint() {
        let store = FakeStore::new(None, None);
        let res = resolve_kek(&store, "Unlock this folder", false);
        assert!(
            res.is_err(),
            "a missing KEK with sealed content must be an ERROR, never a fresh mint"
        );
        assert!(
            !store
                .log()
                .iter()
                .any(|o| matches!(o, Op::WriteBiometric(_))),
            "the refusal path must not write anything (no replacement KEK may be created)"
        );
        assert_eq!(*store.biometric.borrow(), None, "store left untouched");
    }

    /// Read-first makes the probe-lie structurally impossible: when the biometric item EXISTS, the
    /// prompting read returns it and the resolution never consults any existence probe — so a
    /// lying no-UI probe (the 2026-07-05 incident's trigger) can no longer route to the mint arm.
    /// Also holds under the strict no-mint policy.
    #[test]
    fn existing_item_is_returned_read_first_even_under_no_mint_policy() {
        let store = FakeStore::new(None, Some(KEK));
        let out = resolve_kek(&store, "Unlock this folder", false).unwrap();
        assert_eq!(out, KEK);
        assert!(
            !store
                .log()
                .iter()
                .any(|o| matches!(o, Op::WriteBiometric(_))),
            "steady state must not write"
        );
    }

    #[test]
    fn steady_state_is_single_biometric_read() {
        // Biometric item already present, no plain leftover → exactly one biometric read.
        let store = FakeStore::new(None, Some(KEK));
        let out = resolve_kek(&store, "Unlock this folder", false).unwrap();
        assert_eq!(out, KEK);
        let reads = store
            .log()
            .iter()
            .filter(|o| **o == Op::ReadBiometric)
            .count();
        assert_eq!(
            reads, 1,
            "steady-state unlock must be a single biometric read"
        );
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
        let out = resolve_kek(&store, "Unlock this folder", false).unwrap();
        assert_eq!(out, KEK);
        assert_eq!(
            *store.plain.borrow(),
            None,
            "value-equal leftover plain is cleaned up"
        );

        // But if the leftover plain DIFFERS from the biometric item, it is LEFT IN PLACE (no blind
        // destroy). Use a distinct biometric value so the confirm read returns it (not the plain).
        let other: [u8; 32] = std::array::from_fn(|i| (i as u8).wrapping_add(7));
        let store2 = FakeStore::new(Some(KEK), Some(other));
        let out2 = resolve_kek(&store2, "Unlock this folder", false).unwrap();
        assert_eq!(
            out2, other,
            "steady-state read returns the biometric item value"
        );
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
        let migrated_kek = resolve_kek(&store, "Unlock this folder", false).unwrap();

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
        WriteLegacy(String),
    }

    struct FakeDp {
        dp: RefCell<Option<String>>,
        legacy: RefCell<Option<String>>,
        log: RefCell<Vec<DpOp>>,
        /// If `Some(status)`, EVERY data-protection op (`read_dp`/`write_dp`/`delete_dp`) fails as if
        /// the SecItem call returned that `OSStatus`. Models the unsigned-dev-build case
        /// (`-34018`) and the "a real, NON-34018 DP failure" case. The legacy ops still succeed
        /// (the file-based keyring needs no entitlement).
        dp_fail_status: Option<i32>,
    }
    impl FakeDp {
        fn new(dp: Option<&str>, legacy: Option<&str>) -> Self {
            Self {
                dp: RefCell::new(dp.map(str::to_string)),
                legacy: RefCell::new(legacy.map(str::to_string)),
                log: RefCell::new(Vec::new()),
                dp_fail_status: None,
            }
        }
        /// A store whose data-protection backend fails every op with `status` (the legacy keyring,
        /// modeled by `legacy`, still works). `-34018` ⇒ unsigned dev build (errSecMissingEntitlement).
        fn failing_dp(status: i32, legacy: Option<&str>) -> Self {
            Self {
                dp: RefCell::new(None),
                legacy: RefCell::new(legacy.map(str::to_string)),
                log: RefCell::new(Vec::new()),
                dp_fail_status: Some(status),
            }
        }
        fn log(&self) -> Vec<DpOp> {
            self.log.borrow().clone()
        }
        /// Mirror the real [`map_osstatus`] mapping so the test error carries the exact marker the
        /// production `is_missing_entitlement` matches on (proves the real classifier, not a stand-in).
        fn dp_err(status: i32) -> AppError {
            if status == MISSING_ENTITLEMENT_STATUS {
                missing_entitlement_err("fake dp op")
            } else {
                AppError::Secrets(format!("fake dp op: OSStatus {status}"))
            }
        }
    }
    impl DpStringStore for FakeDp {
        fn read_dp(&self) -> Result<Option<String>> {
            self.log.borrow_mut().push(DpOp::ReadDp);
            if let Some(s) = self.dp_fail_status {
                return Err(Self::dp_err(s));
            }
            Ok(self.dp.borrow().clone())
        }
        fn write_dp(&self, secret: &str) -> Result<()> {
            self.log
                .borrow_mut()
                .push(DpOp::WriteDp(secret.to_string()));
            if let Some(s) = self.dp_fail_status {
                return Err(Self::dp_err(s));
            }
            *self.dp.borrow_mut() = Some(secret.to_string());
            Ok(())
        }
        fn delete_dp(&self) -> Result<()> {
            self.log.borrow_mut().push(DpOp::DeleteDp);
            if let Some(s) = self.dp_fail_status {
                return Err(Self::dp_err(s));
            }
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
        fn write_legacy(&self, secret: &str) -> Result<()> {
            self.log
                .borrow_mut()
                .push(DpOp::WriteLegacy(secret.to_string()));
            *self.legacy.borrow_mut() = Some(secret.to_string());
            Ok(())
        }
    }

    #[test]
    fn dp_migration_preserves_value_and_deletes_legacy_only_after_confirm() {
        let secret = "anthropic-sk-deadbeef-test-value";
        let store = FakeDp::new(None, Some(secret));
        let out = migrate_or_read_dp(&store).unwrap();

        assert_eq!(
            out.as_deref(),
            Some(secret),
            "migrated value must be byte-identical"
        );
        assert_eq!(
            *store.dp.borrow(),
            Some(secret.to_string()),
            "DP item now holds the value"
        );
        assert_eq!(
            *store.legacy.borrow(),
            None,
            "legacy item removed after migration"
        );

        // Order proof: write DP → confirm-read DP → THEN delete legacy.
        let log = store.log();
        let wrote = log
            .iter()
            .position(|o| matches!(o, DpOp::WriteDp(_)))
            .unwrap();
        let confirmed = log
            .iter()
            .enumerate()
            .skip(wrote + 1)
            .find(|(_, o)| **o == DpOp::ReadDp)
            .map(|(i, _)| i)
            .expect("a confirming DP read must follow the write");
        let deleted = log.iter().position(|o| *o == DpOp::DeleteLegacy).unwrap();
        assert!(
            wrote < confirmed && confirmed < deleted,
            "write→confirm→delete order (got {log:?})"
        );
        assert!(
            log.contains(&DpOp::WriteDp(secret.to_string())),
            "DP write uses the legacy bytes verbatim"
        );
    }

    #[test]
    fn dp_steady_state_is_a_single_read_and_keeps_no_legacy() {
        // Already migrated: only a DP read; no legacy present.
        let store = FakeDp::new(Some("mcp-token-xyz"), None);
        let out = migrate_or_read_dp(&store).unwrap();
        assert_eq!(out.as_deref(), Some("mcp-token-xyz"));
        assert!(
            !store.log().contains(&DpOp::DeleteLegacy),
            "no legacy → no delete"
        );
        assert!(
            !store.log().iter().any(|o| matches!(o, DpOp::WriteDp(_))),
            "steady state writes nothing"
        );
    }

    #[test]
    fn dp_absent_everywhere_returns_none() {
        let store = FakeDp::new(None, None);
        assert_eq!(
            migrate_or_read_dp(&store).unwrap(),
            None,
            "no item anywhere → None (caller mints fresh)"
        );
    }

    #[test]
    fn dp_value_equal_legacy_leftover_is_cleaned_but_differing_is_kept() {
        // Crash recovery: DP written, legacy not yet deleted, values equal → cleaned.
        let store = FakeDp::new(Some("k"), Some("k"));
        migrate_or_read_dp(&store).unwrap();
        assert_eq!(
            *store.legacy.borrow(),
            None,
            "value-equal legacy leftover removed"
        );

        // Differing legacy is LEFT IN PLACE (never a blind destroy).
        let store2 = FakeDp::new(Some("dp-value"), Some("legacy-DIFFERENT"));
        migrate_or_read_dp(&store2).unwrap();
        assert_eq!(
            *store2.legacy.borrow(),
            Some("legacy-DIFFERENT".to_string()),
            "a differing legacy item is left untouched"
        );
    }

    // ── Unsigned-dev-build errSecMissingEntitlement (-34018) → legacy keyring fallback ────────────
    //
    // On an UNSIGNED / ad-hoc dev build every data-protection SecItem op returns -34018, so the MCP
    // token / Anthropic key / Brave key cannot be stored or read via the DP keychain. These exercise
    // the `dp_*_or_legacy` routing the PUBLIC `set_secret`/`get_secret`/`delete_secret` use, against a
    // fake DP store that returns -34018, and assert set/get/delete consistently round-trip through the
    // legacy keyring. The "DP succeeds" + "DP fails with a NON-34018 status" cases prove the fallback
    // is -34018-specific and unreachable in a signed release.

    #[test]
    fn marker_round_trips_and_is_specific_to_34018() {
        // The -34018 error built the way map_osstatus builds it IS recognized…
        assert!(is_missing_entitlement(&missing_entitlement_err(
            "read data-protection secret"
        )));
        // …while other keychain/secrets errors are NOT (no blanket catch-all).
        assert!(!is_missing_entitlement(&AppError::Secrets(
            "read data-protection secret: OSStatus -25300".into()
        )));
        assert!(!is_missing_entitlement(&AppError::Secrets(
            "read data-protection secret: OSStatus -50".into()
        )));
        assert!(!is_missing_entitlement(&AppError::KeychainDenied(
            "read data-protection secret: OSStatus -25308".into()
        )));
        // A KeychainDenied that does NOT carry the marker (e.g. a real deny) does not trigger fallback.
        assert!(!is_missing_entitlement(&AppError::KeychainDenied(
            "user denied".into()
        )));
    }

    #[test]
    fn unsigned_build_set_get_delete_round_trip_through_legacy_fallback() {
        let secret = "brave-search-api-key-DEADBEEF";

        // SET: DP write fails -34018 → the secret lands in the legacy keyring.
        let store = FakeDp::failing_dp(MISSING_ENTITLEMENT_STATUS, None);
        dp_set_or_legacy(&store, secret).unwrap();
        assert_eq!(
            *store.legacy.borrow(),
            Some(secret.to_string()),
            "fallback wrote to legacy"
        );
        assert_eq!(
            *store.dp.borrow(),
            None,
            "DP store stays empty on an unsigned build"
        );
        assert!(store.log().contains(&DpOp::WriteLegacy(secret.to_string())));

        // GET: DP read fails -34018 → the value is read back from the legacy keyring (round-trip).
        let got = dp_get_or_legacy(&store).unwrap();
        assert_eq!(
            got.as_deref(),
            Some(secret),
            "the secret written via the fallback reads back"
        );

        // DELETE: DP delete fails -34018 → the legacy item is removed; a subsequent get is None.
        dp_delete_or_legacy(&store).unwrap();
        assert_eq!(
            *store.legacy.borrow(),
            None,
            "fallback delete cleared the legacy item"
        );
        assert_eq!(
            dp_get_or_legacy(&store).unwrap(),
            None,
            "deleted secret no longer reads back"
        );
    }

    #[test]
    fn signed_build_dp_succeeds_and_legacy_is_never_used() {
        // RELEASE behaviour: DP works (entitlement present) → -34018 never occurs → no legacy write.
        let secret = "anthropic-sk-signed-release";
        let store = FakeDp::new(None, None); // dp_fail_status = None ⇒ DP ops succeed

        dp_set_or_legacy(&store, secret).unwrap();
        assert_eq!(
            *store.dp.borrow(),
            Some(secret.to_string()),
            "secret stays in the DP keychain"
        );
        assert_eq!(
            *store.legacy.borrow(),
            None,
            "legacy keyring is never written in a signed build"
        );
        assert!(
            !store
                .log()
                .iter()
                .any(|o| matches!(o, DpOp::WriteLegacy(_))),
            "the legacy fallback must NOT run when DP succeeds"
        );

        let got = dp_get_or_legacy(&store).unwrap();
        assert_eq!(got.as_deref(), Some(secret), "DP read returns the value");
        // The get path here is a DP read only (steady state) — never touches the legacy keyring as a
        // source of truth.
        assert!(
            !store
                .log()
                .iter()
                .any(|o| *o == DpOp::ReadLegacy && store.dp_fail_status.is_some()),
            "no DP failure ⇒ no legacy read"
        );
    }

    #[test]
    fn non_34018_dp_failure_does_not_fall_back_and_propagates() {
        // A REAL DP failure that is NOT the unsigned-build signal (e.g. errSecParam -50) must NOT be
        // masked by the legacy fallback — it surfaces as today so a genuine release DP fault is loud.
        let real_fault = -50; // errSecParam
        let legacy_present = "stale-legacy-value-that-must-not-be-resurrected";

        // SET: DP write fails -50 → error propagates, legacy is NOT written.
        let store = FakeDp::failing_dp(real_fault, Some(legacy_present));
        let set_res = dp_set_or_legacy(&store, "new-secret");
        assert!(
            set_res.is_err(),
            "a non-34018 DP write failure must propagate"
        );
        assert!(
            !is_missing_entitlement(&set_res.unwrap_err()),
            "and it is not the -34018 signal"
        );
        assert!(
            !store
                .log()
                .iter()
                .any(|o| matches!(o, DpOp::WriteLegacy(_))),
            "a non-34018 failure must NOT trigger the legacy fallback"
        );

        // GET: DP read fails -50 → error propagates; the stale legacy value is NOT resurrected.
        let store2 = FakeDp::failing_dp(real_fault, Some(legacy_present));
        let get_res = dp_get_or_legacy(&store2);
        assert!(
            get_res.is_err(),
            "a non-34018 DP read failure must propagate, not fall back"
        );

        // DELETE: DP delete fails -50 → error propagates.
        let store3 = FakeDp::failing_dp(real_fault, Some(legacy_present));
        assert!(
            dp_delete_or_legacy(&store3).is_err(),
            "a non-34018 DP delete failure must propagate"
        );
    }

    #[test]
    fn item_not_found_is_handled_as_today_not_as_a_fallback() {
        // -25300 (errSecItemNotFound) is handled INLINE by read_dp as Ok(None) (see MacDpStore), so a
        // real DP store never surfaces it as an error to the router. Model the steady state: DP empty,
        // no legacy → None, with no legacy fallback / write triggered.
        let store = FakeDp::new(None, None);
        assert_eq!(
            dp_get_or_legacy(&store).unwrap(),
            None,
            "absent everywhere → None"
        );
        assert!(
            !store
                .log()
                .iter()
                .any(|o| matches!(o, DpOp::WriteLegacy(_))),
            "a not-found is not a -34018 fallback"
        );
    }

    // ── DEBUG-ONLY dev plaintext file store (MCP token / Anthropic key / Brave key) ──────────────
    //
    // The dev file store is what a `tauri dev` build uses for the three non-gated string secrets so
    // it NEVER touches the data-protection OR legacy keychain (no -34018, no login-keychain prompt
    // spam). These tests drive the explicit-path seam (`dev_*_secret_at`) against a temp dir — never
    // the real app-support file — and assert: round-trip set→get, delete removes, the store is a map
    // (multiple accounts coexist), and a missing file ⇒ `Ok(None)` (not an error). The dev store is
    // `#[cfg(debug_assertions)]`; `cargo test` runs in debug, so it is compiled in here.

    #[test]
    fn dev_store_set_get_round_trips_a_value() {
        let dir = std::env::temp_dir().join(format!("murmur-dev-secrets-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("dev-secrets-rt.json");
        let _ = std::fs::remove_file(&path);

        dev_set_secret_at(&path, ACCOUNT_WEB_SEARCH_KEY, "brave-key-DEADBEEF").unwrap();
        assert_eq!(
            dev_get_secret_at(&path, ACCOUNT_WEB_SEARCH_KEY)
                .unwrap()
                .as_deref(),
            Some("brave-key-DEADBEEF"),
            "a value set in the dev store reads back identically"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn dev_store_delete_removes_the_value() {
        let dir =
            std::env::temp_dir().join(format!("murmur-dev-secrets-del-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("dev-secrets-del.json");
        let _ = std::fs::remove_file(&path);

        dev_set_secret_at(&path, ACCOUNT_ANTHROPIC_KEY, "anthropic-sk-test").unwrap();
        dev_delete_secret_at(&path, ACCOUNT_ANTHROPIC_KEY).unwrap();
        assert_eq!(
            dev_get_secret_at(&path, ACCOUNT_ANTHROPIC_KEY).unwrap(),
            None,
            "a deleted secret no longer reads back"
        );
        // Idempotent: deleting an absent account is Ok(()).
        dev_delete_secret_at(&path, ACCOUNT_ANTHROPIC_KEY).unwrap();

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn dev_store_is_a_map_multiple_accounts_coexist() {
        let dir =
            std::env::temp_dir().join(format!("murmur-dev-secrets-map-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("dev-secrets-map.json");
        let _ = std::fs::remove_file(&path);

        // MCP token + Anthropic key + Brave key all live side-by-side in one file.
        dev_set_secret_at(&path, ACCOUNT_MCP_TOKEN, "mcp-token-1").unwrap();
        dev_set_secret_at(&path, ACCOUNT_ANTHROPIC_KEY, "anthropic-2").unwrap();
        dev_set_secret_at(&path, ACCOUNT_WEB_SEARCH_KEY, "brave-3").unwrap();

        assert_eq!(
            dev_get_secret_at(&path, ACCOUNT_MCP_TOKEN)
                .unwrap()
                .as_deref(),
            Some("mcp-token-1")
        );
        assert_eq!(
            dev_get_secret_at(&path, ACCOUNT_ANTHROPIC_KEY)
                .unwrap()
                .as_deref(),
            Some("anthropic-2")
        );
        assert_eq!(
            dev_get_secret_at(&path, ACCOUNT_WEB_SEARCH_KEY)
                .unwrap()
                .as_deref(),
            Some("brave-3")
        );

        // Replacing one account leaves the others intact.
        dev_set_secret_at(&path, ACCOUNT_ANTHROPIC_KEY, "anthropic-REPLACED").unwrap();
        assert_eq!(
            dev_get_secret_at(&path, ACCOUNT_ANTHROPIC_KEY)
                .unwrap()
                .as_deref(),
            Some("anthropic-REPLACED")
        );
        assert_eq!(
            dev_get_secret_at(&path, ACCOUNT_MCP_TOKEN)
                .unwrap()
                .as_deref(),
            Some("mcp-token-1")
        );
        assert_eq!(
            dev_get_secret_at(&path, ACCOUNT_WEB_SEARCH_KEY)
                .unwrap()
                .as_deref(),
            Some("brave-3")
        );

        // Deleting one leaves the rest.
        dev_delete_secret_at(&path, ACCOUNT_MCP_TOKEN).unwrap();
        assert_eq!(dev_get_secret_at(&path, ACCOUNT_MCP_TOKEN).unwrap(), None);
        assert_eq!(
            dev_get_secret_at(&path, ACCOUNT_WEB_SEARCH_KEY)
                .unwrap()
                .as_deref(),
            Some("brave-3")
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn dev_store_missing_file_returns_none_not_an_error() {
        let path = std::env::temp_dir().join(format!(
            "murmur-dev-secrets-absent-{}.json",
            std::process::id()
        ));
        // Ensure the file does NOT exist.
        let _ = std::fs::remove_file(&path);
        assert!(!path.exists());

        // A missing file must yield Ok(None) for a get, and Ok(()) for a delete — never an error.
        assert_eq!(
            dev_get_secret_at(&path, ACCOUNT_MCP_TOKEN).unwrap(),
            None,
            "no dev-secrets file yet ⇒ get returns None"
        );
        dev_delete_secret_at(&path, ACCOUNT_MCP_TOKEN).unwrap();
        // And the missing-file get is still None after a no-op delete.
        assert_eq!(dev_get_secret_at(&path, ACCOUNT_MCP_TOKEN).unwrap(), None);
    }
}
