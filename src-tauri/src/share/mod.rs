//! Milestone M3-CLIENT — wire Murmur to the sharing server for zero-knowledge LINK SHARES (mode A).
//!
//! Layering (all under `crate::error::Result`, no `unwrap`/`expect` in non-test code):
//! - [`envelope`] — the PURE "clean text" transform (strip frontmatter + flatten `[[wikilinks]]` +
//!   strip `obsidian://`) applied to a note before it enters a share envelope. The
//!   `vault-titles-egress-leak` class; TDD'd first.
//! - [`opaque_client`] — the OPAQUE (RFC 9807) CLIENT side (registration + login), running the SAME
//!   ciphersuite as the server (a verbatim, must-stay-in-sync copy — see that module's warning).
//! - [`client`] — the typed reqwest HTTP client for the auth + share endpoints (`murmur_protocol`
//!   DTOs on the wire; the base URL is validated like the AI gateway).
//!
//! The account key hierarchy + link-share SEAL are reused from [`crate::e2ee`] (M2) — this module
//! never re-implements crypto. The Tauri COMMANDS that stitch these together (with the
//! `meeting_is_unlocked` gate, the content-free egress ledger, and the fail-closed consent gate)
//! live in `commands.rs` alongside the rest of the command surface.
//!
//! SECRET DISCIPLINE (spec §3/§7): session tokens + device id live in the macOS Keychain (never
//! SQLite, never logged). The account master key `MK` lives ONLY in RAM ([`AccountSession`], zeroized
//! on drop). The link key `L` never leaves the device and is never persisted or logged.

pub mod client;
pub mod envelope;
pub mod opaque_client;
pub mod org_dto;
pub mod org_envelope;
pub mod task_envelope;

use crate::error::{AppError, Result};
use zeroize::Zeroizing;

// ─────────────────────────── Keychain accounts (tokens + device id) ───────────────────────────
//
// Stored under the existing `com.meetnotes.app` service via `secrets::{set,get,delete}_secret`.
// These are the ONLY places the session tokens + device id are persisted. Never SQLite, never logged.

/// Keychain account name for the current session's OPAQUE ACCESS token (30-min TTL server-side).
const KC_ACCESS_TOKEN: &str = "murmur_share_access_token";
/// Keychain account name for the rotating REFRESH token (per-device, reuse-detected server-side).
const KC_REFRESH_TOKEN: &str = "murmur_share_refresh_token";
/// Keychain account name for the server-assigned device id.
const KC_DEVICE_ID: &str = "murmur_share_device_id";
/// Keychain account name for the account email (non-secret, but session-scoped; kept out of SQLite).
const KC_ACCOUNT_EMAIL: &str = "murmur_share_account_email";
/// Keychain account name for the account id. NOTE: this is the account's EMAIL — it is baked into the
/// `mk_wrap_pw` AAD + the identity-key derivation at signup, so it MUST stay the email (a login can't
/// re-derive the MK otherwise). It is NOT the server user id despite the historical name.
const KC_ACCOUNT_ID: &str = "murmur_share_account_id";
/// Keychain account name for the account's stable SERVER USER ID (a UUID). Distinct from
/// `KC_ACCOUNT_ID` (the email): org key-grants key on this UUID (matching `org_members.user_id`). A
/// login learns it from `LoginFinishResponse.user_id`; absent for sessions persisted before this
/// field existed ⇒ `None` ⇒ org sharing prompts a re-login to backfill it.
const KC_SERVER_USER_ID: &str = "murmur_share_server_user_id";
/// Keychain account name for the identity-key GENERATION (non-secret key-rotation counter). Persisted
/// alongside the tokens so a biometric session restore can rebuild the identity-slot AAD without a
/// re-login — the MK itself is cached separately + biometric-gated (see `secrets::keychain`).
const KC_GENERATION: &str = "murmur_share_generation";
/// Keychain account name for the ACCESS token's RFC3339 expiry (non-secret). Persisted so the client
/// can PROACTIVELY redeem the refresh token via `/v1/auth/refresh` shortly BEFORE the 30-min access
/// token lapses — without it, every share op after 30 min 401s ("not authenticated") because the
/// bearer is dead but the in-RAM session still looks logged in. Absent for sessions persisted before
/// this field existed ⇒ `None` ⇒ treated as "refresh now" (fail-safe; see [`access_token_needs_refresh`]).
const KC_ACCESS_EXPIRES_AT: &str = "murmur_share_access_expires_at";

/// Refresh the access token this many seconds BEFORE its server-side expiry, so a share op never races
/// the 30-min TTL boundary (also absorbs modest client/server clock skew). Refreshes are cheap and
/// rotate the token family safely, so a generous buffer costs nothing.
const ACCESS_REFRESH_SKEW_SECS: i64 = 120;

/// The persisted (Keychain) half of a login: what survives an app restart so a share can be created
/// without re-logging-in, as long as the MK can be recovered. NOTE: the MK is NOT persisted here — the
/// secret MK is cached SEPARATELY behind a biometric-gated keychain item
/// (`secrets::keychain::cache_account_mk_biometric`). After a restart, `account_status` reports
/// "logged in but locked"; the user restores the session with a single Touch ID tap
/// (`unlock_sharing_with_biometric`), which pairs the biometric-released MK with the non-secret
/// `generation` persisted here — or falls back to a full password re-login.
pub struct PersistedTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub device_id: String,
    pub email: String,
    pub account_id: String,
    /// The account's stable server user id (UUID), from `LoginFinishResponse.user_id`. Org key-grants
    /// key on it. `None` for a session persisted before this field existed ⇒ org sharing prompts a
    /// re-login to backfill it (see `load_tokens`).
    pub server_user_id: Option<String>,
    /// The identity-key generation at login (non-secret rotation counter). Needed to rebuild the
    /// `AccountSession` on a biometric restore; defaults to 1 for sessions persisted before this field
    /// existed (see `load_tokens`).
    pub generation: u32,
    /// RFC3339 expiry of `access_token` (non-secret), from the server's login/refresh response. `None`
    /// for a session persisted before this field existed ⇒ the next share op refreshes proactively
    /// (fail-safe). Drives [`access_token_needs_refresh`].
    pub access_expires_at: Option<String>,
}

/// Persist the session tokens + device id + account identity to the Keychain (delete-before-add
/// semantics inside `set_secret`). Called once after a successful login.
pub fn store_tokens(t: &PersistedTokens) -> Result<()> {
    crate::secrets::set_secret(KC_ACCESS_TOKEN, &t.access_token)?;
    crate::secrets::set_secret(KC_REFRESH_TOKEN, &t.refresh_token)?;
    crate::secrets::set_secret(KC_DEVICE_ID, &t.device_id)?;
    crate::secrets::set_secret(KC_ACCOUNT_EMAIL, &t.email)?;
    crate::secrets::set_secret(KC_ACCOUNT_ID, &t.account_id)?;
    // The server user id (UUID) when known; otherwise DELETE any stale prior value.
    match &t.server_user_id {
        Some(uid) => crate::secrets::set_secret(KC_SERVER_USER_ID, uid)?,
        None => crate::secrets::delete_secret(KC_SERVER_USER_ID)?,
    }
    crate::secrets::set_secret(KC_GENERATION, &t.generation.to_string())?;
    // Write the expiry when known; otherwise DELETE any stale prior value so a refresh that omitted it
    // never leaves an old expiry behind (which would suppress the next proactive refresh).
    match &t.access_expires_at {
        Some(exp) => crate::secrets::set_secret(KC_ACCESS_EXPIRES_AT, exp)?,
        None => crate::secrets::delete_secret(KC_ACCESS_EXPIRES_AT)?,
    }
    Ok(())
}

/// Read the persisted tokens, or `None` if not fully present (logged out).
pub fn load_tokens() -> Result<Option<PersistedTokens>> {
    let (Some(access_token), Some(refresh_token), Some(device_id), Some(email), Some(account_id)) = (
        crate::secrets::get_secret(KC_ACCESS_TOKEN)?,
        crate::secrets::get_secret(KC_REFRESH_TOKEN)?,
        crate::secrets::get_secret(KC_DEVICE_ID)?,
        crate::secrets::get_secret(KC_ACCOUNT_EMAIL)?,
        crate::secrets::get_secret(KC_ACCOUNT_ID)?,
    ) else {
        return Ok(None);
    };
    // `generation` is persisted since the biometric-restore feature. A session stored before it existed
    // has no entry → default to 1 (the pre-rotation generation). This never misleads the biometric
    // restore: that path only trusts `generation` when an MK was ALSO cached, and MK-caching + the
    // generation write happen in the SAME post-feature login (`account_login`), so whenever a cached MK
    // exists so does its matching generation.
    let generation = crate::secrets::get_secret(KC_GENERATION)?
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(1);
    // Absent for a session persisted before this field existed ⇒ `None` ⇒ refresh on the next share op.
    let access_expires_at = crate::secrets::get_secret(KC_ACCESS_EXPIRES_AT)?;
    // `None` for a session persisted before this field existed ⇒ org sharing prompts a re-login.
    let server_user_id = crate::secrets::get_secret(KC_SERVER_USER_ID)?;
    Ok(Some(PersistedTokens {
        access_token,
        refresh_token,
        device_id,
        email,
        account_id,
        server_user_id,
        generation,
        access_expires_at,
    }))
}

/// The stored ACCESS token alone (the bearer for share ops). `None` when logged out.
pub fn access_token() -> Result<Option<String>> {
    crate::secrets::get_secret(KC_ACCESS_TOKEN)
}

/// Delete every session Keychain entry (logout / account reset). Idempotent.
pub fn clear_tokens() -> Result<()> {
    crate::secrets::delete_secret(KC_ACCESS_TOKEN)?;
    crate::secrets::delete_secret(KC_REFRESH_TOKEN)?;
    crate::secrets::delete_secret(KC_DEVICE_ID)?;
    crate::secrets::delete_secret(KC_ACCOUNT_EMAIL)?;
    crate::secrets::delete_secret(KC_ACCOUNT_ID)?;
    crate::secrets::delete_secret(KC_SERVER_USER_ID)?;
    crate::secrets::delete_secret(KC_GENERATION)?;
    crate::secrets::delete_secret(KC_ACCESS_EXPIRES_AT)?;
    Ok(())
}

// ─────────────────────────── In-RAM account session (holds MK) ───────────────────────────

/// The logged-in account for the current session. The master key `MK` is held zeroizing; the tokens
/// live in the Keychain but the access token is cached here to avoid a Keychain read per share op.
pub struct AccountSession {
    pub account_id: String,
    pub email: String,
    /// The account's stable server user id (UUID) for org key-grants. `None` for a session restored
    /// from tokens persisted before this field existed ⇒ org sharing prompts a re-login to backfill it.
    pub server_user_id: Option<String>,
    pub device_id: String,
    /// The account master key `MK`, unwrapped at login from the server's `mk_wrap_pw` via
    /// `keys::derive_kek_pw(export_key)`. Zeroized on drop.
    pub mk: Zeroizing<[u8; 32]>,
    /// The current identity-key generation (used in the identity-slot AAD).
    pub generation: u32,
    /// The session ACCESS token (mirror of the Keychain copy; the bearer for `POST /v1/shares`).
    pub access_token: String,
    /// RFC3339 expiry of `access_token` (mirror of the Keychain copy). `None` ⇒ unknown ⇒ the next
    /// share op refreshes proactively. Updated in place on a successful `/v1/auth/refresh`.
    pub access_expires_at: Option<String>,
    /// The CURRENT single-use refresh token. The RAM copy is the SOURCE OF TRUTH for rotation
    /// (`refresh_session` presents and replaces THIS value); the Keychain copy is a best-effort
    /// mirror for restart restore. Rationale: if a rotation succeeded server-side but the Keychain
    /// persist failed, re-presenting the stale Keychain token would trip the server's reuse
    /// detection and revoke the whole family (forced logout) — the RAM copy makes the live session
    /// immune to a flaky persist.
    pub refresh_token: String,
}

/// Does an access token with this RFC3339 `expires_at` need a proactive refresh at `now`? A missing or
/// unparseable expiry returns `true` (fail-safe: better one wasted refresh than a dead-token 401), as
/// does an expiry within [`ACCESS_REFRESH_SKEW_SECS`] of `now`. Pure so the decision is unit-testable
/// without a clock or a server.
pub(crate) fn access_token_needs_refresh(
    expires_at: Option<&str>,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    let Some(raw) = expires_at else {
        return true; // unknown expiry ⇒ refresh now (covers pre-feature sessions + omitted fields).
    };
    match chrono::DateTime::parse_from_rfc3339(raw) {
        Ok(exp) => {
            now + chrono::Duration::seconds(ACCESS_REFRESH_SKEW_SECS)
                >= exp.with_timezone(&chrono::Utc)
        }
        Err(_) => true, // malformed ⇒ refresh (never trust an unparseable expiry).
    }
}

// ─────────────────────────── Content-free share egress ledger ───────────────────────────

/// Append a CONTENT-FREE share-egress ledger row (spec §7 inv. 4). Logs ONLY the server host, a byte
/// size, and a `kind` label. NEVER the share URL, the fragment key `L`, a title, or any note text.
/// Best-effort: a ledger write failure is logged (non-PII) and swallowed — it must never fail a share.
pub fn ledger_row(db: &crate::storage::Db, host: &str, kind: &str, byte_count: usize) {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    if let Err(e) = db.insert_share_egress(ts, host, kind, byte_count) {
        tracing::warn!(
            target: "share",
            error = %e,
            host = %host,
            kind = %kind,
            "share_egress_log insert failed; row dropped (share unaffected)"
        );
    }
}

/// Assemble the mode-A share URL: `<share_base_url>/s#<share_id>.<b64url(L)>`. The fragment (`#…`)
/// carries the link key `L` and is NEVER sent over the network / logged / ledgered (RFC: a fragment
/// is never transmitted). Kept as a pure fn so the caller assembles the URL LOCALLY (spec §7 inv. 5:
/// L never goes to the server).
pub fn assemble_share_url(share_base_url: &str, share_id: &str, l: &[u8; 32]) -> String {
    let base = share_base_url.trim_end_matches('/');
    let l_b64 = murmur_protocol::b64::encode(l);
    format!("{base}/s#{share_id}.{l_b64}")
}

/// A conservative device-platform label for `DeviceInfo` (PII-min: no user-set name).
pub fn device_platform() -> &'static str {
    "macos"
}

/// Guard: a `share_id` we mint MUST be a UUID (the server validates format; a non-UUID is a client
/// bug). Returns a fresh UUIDv4 string.
pub fn new_share_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Map an absent session to a fail-closed `Unavailable` (spec §7 inv. 8: share ops require login).
pub(crate) fn require_login(session: &Option<AccountSession>) -> Result<&AccountSession> {
    session
        .as_ref()
        .ok_or_else(|| AppError::Unavailable("not signed in to the sharing account".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assemble_share_url_puts_l_only_in_the_fragment() {
        let l = [0xABu8; 32];
        let url = assemble_share_url("https://share.example.com/", "abc-123", &l);
        // The path is `/s`, the key is in the fragment after `#`.
        assert!(url.starts_with("https://share.example.com/s#abc-123."));
        // The base's trailing slash is normalized (no `//s`).
        assert!(!url.contains("com//s"));
        // The fragment marker exists and the key material is AFTER it (never before `#`).
        let (before_hash, after_hash) = url.split_once('#').unwrap();
        assert!(!before_hash.contains(&murmur_protocol::b64::encode(&l)));
        assert!(after_hash.contains(&murmur_protocol::b64::encode(&l)));
    }

    #[test]
    fn new_share_id_is_a_uuid() {
        let id = new_share_id();
        assert!(uuid::Uuid::parse_str(&id).is_ok());
    }

    #[test]
    fn require_login_fails_closed_when_logged_out() {
        let none: Option<AccountSession> = None;
        assert!(matches!(
            require_login(&none),
            Err(AppError::Unavailable(_))
        ));
    }

    #[test]
    fn access_token_refresh_decision() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-07-05T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);

        // Unknown expiry (pre-feature session / omitted field) ⇒ refresh (fail-safe).
        assert!(access_token_needs_refresh(None, now));
        // Malformed expiry ⇒ refresh (never trust an unparseable value).
        assert!(access_token_needs_refresh(Some("not-a-timestamp"), now));
        // Already expired ⇒ refresh.
        assert!(access_token_needs_refresh(
            Some("2026-07-05T11:45:00Z"),
            now
        ));
        // Within the skew window (< ACCESS_REFRESH_SKEW_SECS to expiry) ⇒ refresh proactively.
        assert!(access_token_needs_refresh(
            Some("2026-07-05T12:01:00Z"),
            now
        ));
        // Comfortably in the future (a fresh 30-min token) ⇒ do NOT refresh.
        assert!(!access_token_needs_refresh(
            Some("2026-07-05T12:29:00Z"),
            now
        ));
        // A non-UTC offset that still resolves comfortably ahead ⇒ no refresh (offset handled).
        assert!(!access_token_needs_refresh(
            Some("2026-07-05T14:29:00+02:00"),
            now
        ));
    }
}
