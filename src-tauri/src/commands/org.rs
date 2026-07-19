//! ORG (Shared Brain / M6) + 1:1 LINK/USER SHARE (M3-client / M5) Tauri command surface — extracted
//! VERBATIM from `commands` (God-file split, a PURE MOVE — every read-gate / egress-consent /
//! redaction-firewall / crypto-envelope body is UNCHANGED, only relocated). This is the sharing
//! domain: the zero-knowledge mode-A LINK shares + mode-B Murmur↔Murmur USER shares (create / list /
//! revoke / preview / rewrap / inbox / accept / decline), the share-egress consent latch, and the
//! whole M6 Organizations (Shared Brain) surface — org create/status/members, per-instance context
//! toggle, invite/remove/leave, the org-egress consent latch, preview + share (meeting/document) +
//! the OCK seal-and-open, list/revoke shares, source resolution, the folder active-shares report,
//! the pending-share sweep + background feed sync, and the org-item get/update/delete/list.
//!
//! LOCK / EGRESS / CRYPTO invariants (byte-identical to the pre-move form — audited by
//! `lock-security-reviewer`):
//!   • Every content read is GATED FIRST — `share_note_to_link_inner` / `share_note_to_user_inner` /
//!     `build_org_share_body` open with `super::meeting_is_unlocked` (→ `AppError::Locked`); the org
//!     list/report readers (`folder_active_shares_inner`, `list_org_items_inner`,
//!     `meeting_org_shares_inner`) mask a sealed-not-unlocked source's title through the same
//!     `super::meeting_is_unlocked` / `super::folder_is_unlocked` gate + the `context_enabled` filter,
//!     so a locked/disabled item leaks NOTHING and is invisible/unshared.
//!   • Every cloud egress is FAIL-CLOSED on the one-time consent (`share_egress_consented` /
//!     `org_egress_consented`), the payload is cleaned + PII-scrubbed (`scrub_org_markdown` reuses the
//!     SAME `summarize::redact` firewall as the cloud path), and each publish writes a CONTENT-FREE
//!     egress-ledger row — the consent → redaction → ledger ORDERING is unchanged.
//!   • The link key `L` NEVER leaves the device (URL fragment only); the account MK stays in RAM; the
//!     OCK is unwrapped on demand + RAM-cached (`AppState::org_ock_cache`), never persisted or logged;
//!     the wrapped-key / grant / envelope crypto (`e2ee::wrap` / `e2ee::org` / `share::envelope`) is
//!     moved verbatim. No key material is logged.
//!
//! The SHARED session/crypto helpers STAY in `commands/mod.rs` and are reached through `use super::*`
//! (a `commands` submodule sees its parent's private items): `share_base_url` / `valid_access_token` /
//! `refresh_session` (token lifecycle), `require_session_mk` / `SessionMk` / `session_server_user_id`
//! (account-MK session), `tofu_check` / `TofuState` (contact TOFU pin), and the gate helpers
//! `meeting_is_unlocked` / `folder_is_unlocked` / `session_folder_ck` / `sealed_document_blob` /
//! `aad_content` — all promoted to `pub(crate)` where a moved command reaches them (bodies
//! byte-identical). The ACCOUNT/AUTH commands (`account_status` / `account_signup` / `account_login`
//! / `account_logout` / `unlock_sharing_with_biometric` / `mark_sharing_choice_made`) also STAY.
//! Every moved symbol keeps its EXACT prior body/signature and is re-exported at `crate::commands` via
//! `pub use org_commands::*;` in `commands/mod.rs`, so `generate_handler![commands::org_create]` in
//! `lib.rs` and every `crate::commands::…` / sibling-module (`notes.rs` → `republish_org_shares_for_source`)
//! caller resolves UNCHANGED. Bound as `org_commands` (via `#[path]`) to avoid any name shadow with
//! the crate-level `crate::e2ee::org` / `crate::storage::org_store` (E0255). No gate/consent/redaction/
//! crypto LOGIC changed — only relocation.

use super::*;

/// One row of `list_my_shares` (camelCase). Content-free by construction — the server holds no
/// titles; the local title is added ONLY when the meeting is unlocked (else `null` + `locked:true`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MyShareEntry {
    pub share_id: String,
    /// The share's local meeting title — `None` (and `locked:true`) when the meeting is sealed and
    /// not session-unlocked, or when the share was created on another device (unknown locally).
    pub title: Option<String>,
    pub locked: bool,
    pub rev: u32,
    pub created_at: String,
    pub expires_at: Option<String>,
    pub revoked: bool,
    pub download_count: u32,
    /// The LOCAL meeting this share belongs to (so the FE can filter to THIS meeting). `None` when the
    /// share was created on another device OR is a NOTE share (no meeting anchor) — masked like `title`.
    pub meeting_id: Option<String>,
    /// The LOCAL authored-note `document_id` this share belongs to (WP6), so the FE can filter a note's
    /// share panel to THIS note. `None` for a meeting share or a share created on another device.
    #[serde(default)]
    pub document_id: Option<String>,
    /// The server-enforced open cap (`None` ⇒ uncapped); sourced from the server list row. Drives the
    /// `X / Y opens` label. The server enforces the cap atomically on `/fetch`; this is display-only.
    pub max_downloads: Option<u32>,
    /// `link` (mode-A zero-knowledge link) vs `user` (mode-B Murmur↔Murmur grant). Lets the FE split
    /// the "Active links" list from the person-share count. Serializes snake_case → "link"/"user".
    pub mode: murmur_protocol::dto::ShareMode,
}

/// `consent_to_share_egress` — grant the one-time SHARE-egress consent (§7 inv. 5). Fail-closed:
/// until this is set, `share_note_to_link` refuses. Mirror of `consent_to_cloud_egress`.
#[tauri::command]
pub fn consent_to_share_egress(state: State<'_, AppState>) -> Result<(), AppError> {
    let mut cfg = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
    cfg.grant_share_egress_consent(&state.db)?;
    Ok(())
}

/// `revoke_share_egress` — revoke the share-egress consent (the next share is refused fail-closed).
#[tauri::command]
pub fn revoke_share_egress(state: State<'_, AppState>) -> Result<(), AppError> {
    let mut cfg = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
    cfg.revoke_share_egress(&state.db)?;
    Ok(())
}

/// `share_note_to_link(meeting_id, expires_days?, password?, max_downloads?) -> String` — create a
/// zero-knowledge mode-A link share of a note and return the share URL. `max_downloads` sets an
/// optional server-enforced open cap. See the LOCK-CRITICAL invariants above.
#[tauri::command]
pub async fn share_note_to_link(
    state: State<'_, AppState>,
    meeting_id: String,
    expires_days: Option<u32>,
    password: Option<String>,
    max_downloads: Option<u32>,
) -> Result<String, AppError> {
    share_note_to_link_inner(
        state.inner(),
        meeting_id,
        expires_days,
        password,
        max_downloads,
    )
    .await
}

/// Core of [`share_note_to_link`] over `&AppState` so the lock gate + consent gate are unit-testable
/// headless (no Tauri `State`, no server). The gate order is normative — DO NOT reorder.
pub(crate) async fn share_note_to_link_inner(
    state: &AppState,
    meeting_id: String,
    expires_days: Option<u32>,
    password: Option<String>,
    max_downloads: Option<u32>,
) -> Result<String, AppError> {
    // (1) READ-GATE — FIRST statement (copies `export_note`). A sealed-not-unlocked meeting refuses.
    if !meeting_is_unlocked(state, &meeting_id)? {
        return Err(AppError::Locked(
            "this meeting's folder is locked — unlock it to share the note".into(),
        ));
    }

    // (7) First-ever share = explicit consent (fail-closed, mirrors cloud egress).
    // (8) Logged out ⇒ fail closed Unavailable. A mode-A link share needs a live session (the bearer
    //     token) but NOT the account MK — `L`/`NK` are per-share random (MK binds only mode-B grants),
    //     so we require login + hold the access token; MK stays untouched in the session.
    let base = {
        let cfg = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
        if !cfg.share_egress_consented {
            return Err(AppError::Unavailable(
                "sharing not consented — confirm the one-time upload notice first".into(),
            ));
        }
        cfg.share_base_url.clone()
    };
    // Proactively refresh the bearer if it is at/near its 30-min expiry — otherwise a long-lived or
    // biometric-restored session 401s here ("not authenticated") while still looking logged in. Fails
    // closed `Unavailable` when logged out (mirrors the old `require_login`).
    let access_token = valid_access_token(state).await?;

    let client = crate::share::client::ShareClient::new(&base)?;

    // (2) Fetch the note via the gated read, and its display title/timestamp.
    let note = state
        .db
        .get_latest_note_for_meeting(&meeting_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no note for meeting {meeting_id}")))?;
    let meeting = state.db.get_meeting(&meeting_id)?;
    let title = meeting
        .as_ref()
        .and_then(|m| m.title.clone())
        .unwrap_or_else(|| "Shared note".to_string());
    let created_at = meeting
        .as_ref()
        .map(|m| m.started_at.clone())
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());

    // (2 cont.) Clean the body: strip frontmatter + flatten wikilinks + strip obsidian:// (pure fn).
    let clean_body = crate::share::envelope::clean_note_body(&note.markdown);

    // (3) Build the inner envelope + seal a fresh link share (e2ee M2). rev starts at 1.
    let share_id = crate::share::new_share_id();
    let rev = 1u32;
    let env = murmur_protocol::envelope::ShareEnvelope::new(title, clean_body, created_at);
    let pw_ref = password.as_deref().filter(|s| !s.is_empty());
    let sealed = crate::e2ee::link::seal_link_share(&env, &share_id, rev, pw_ref)?;

    // (4) Upload: content cell + wrapped_nk + gate_salt + gate_secret + rev + passwordRequired.
    //     L is NOT in this request (CreateShareRequest has no `l` field) — it stays on-device.
    let expires_at = expires_days.map(|d| {
        let days = d.clamp(1, 365) as i64;
        (chrono::Utc::now() + chrono::Duration::days(days)).to_rfc3339()
    });
    let argon = if pw_ref.is_some() {
        Some(murmur_protocol::dto::ArgonParams {
            m: sealed.argon_params.m_cost_kib,
            t: sealed.argon_params.t_cost,
            p: sealed.argon_params.p_cost,
        })
    } else {
        None
    };
    let cell_bytes = sealed.ciphertext_cell.len();
    let create_req = murmur_protocol::dto::CreateShareRequest {
        share_id: share_id.clone(),
        mode: murmur_protocol::dto::ShareMode::Link,
        content_cell: sealed.ciphertext_cell,
        wrapped_nk: sealed.wrapped_nk,
        gate_salt: sealed.gate_salt.to_vec(),
        gate_secret: sealed.gate_secret.to_vec(),
        rev,
        password_required: pw_ref.is_some(),
        argon,
        expires_at,
        // Clamp a nonsensical 0 to 1 (mirrors the `expires_days` min(1) clamp); `None` ⇒ uncapped.
        // The server enforces the cap atomically on `/fetch` — nothing else changes here.
        max_downloads: max_downloads.map(|n| n.max(1)),
        // Mode A: no per-recipient wrapped keys (that is mode B / §4.8). Absent for link shares.
        recipients: None,
    };
    let created = client.create_share(&access_token, create_req).await?;

    // (6) CONTENT-FREE egress ledger row (host + byte size). NEVER the URL / L / title.
    crate::share::ledger_row(&state.db, &client.host(), "share_create", cell_bytes);
    // Local bookkeeping — share_id + meeting_id only (NO title column).
    state.db.insert_outbound_share(
        &share_id,
        &meeting_id,
        "link",
        rev,
        &chrono::Utc::now().to_rfc3339(),
    )?;

    // (5) Assemble the URL LOCALLY — L goes ONLY into the fragment; never logged/ledgered.
    let base_for_url = if created.share_base_url.trim().is_empty() {
        base
    } else {
        created.share_base_url
    };
    Ok(crate::share::assemble_share_url(
        &base_for_url,
        &share_id,
        &sealed.l,
    ))
}

/// WP6 — share an authored NOTE as a zero-knowledge link (mirrors [`share_note_to_link`] for
/// meetings). GATE order is normative (copies the meeting path): (1) the note's folder must be
/// UNLOCKED (a sealed-not-unlocked note is refused `AppError::Locked` — its text never leaves the
/// device); (2) share consent + login; then clean → envelope → seal → upload. Records the outbound
/// share against the note's `document_id`. Returns the assembled share URL (L only in the fragment).
#[tauri::command]
pub async fn share_note_to_link_doc(
    state: State<'_, AppState>,
    id: String,
    expires_days: Option<u32>,
    password: Option<String>,
    max_downloads: Option<u32>,
) -> Result<String, AppError> {
    share_note_to_link_doc_inner(state.inner(), id, expires_days, password, max_downloads).await
}

/// Core of [`share_note_to_link_doc`] over `&AppState` (unit-testable headless gate).
pub(crate) async fn share_note_to_link_doc_inner(
    state: &AppState,
    id: String,
    expires_days: Option<u32>,
    password: Option<String>,
    max_downloads: Option<u32>,
) -> Result<String, AppError> {
    // (1) READ-GATE — FIRST statement. A sealed-not-unlocked note refuses (its text never egresses).
    let Some(row) = state.db.get_note_row(&id)? else {
        return Err(AppError::InvalidArg(format!("no note {id}")));
    };
    if !folder_is_unlocked(state, &row.folder_id)? {
        return Err(AppError::Locked(
            "this note's folder is locked — unlock it to share the note".into(),
        ));
    }

    // (2) Consent (first-ever share) + logged-in bearer, exactly like the meeting path.
    let base = {
        let cfg = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
        if !cfg.share_egress_consented {
            return Err(AppError::Unavailable(
                "sharing not consented — confirm the one-time upload notice first".into(),
            ));
        }
        cfg.share_base_url.clone()
    };
    let access_token = valid_access_token(state).await?;
    let client = crate::share::client::ShareClient::new(&base)?;

    // (3) The note's display title + created timestamp; clean its full markdown (strip front-matter,
    //     flatten wikilinks, strip obsidian://) — the SAME pure transform meeting shares use.
    let title = note_display_title(&row);
    let created_at = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(row.created_at)
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc3339();
    let clean_body = crate::share::envelope::clean_note_body(&row.text);

    // (4) Seal a fresh link share.
    let share_id = crate::share::new_share_id();
    let rev = 1u32;
    let env = murmur_protocol::envelope::ShareEnvelope::new(title, clean_body, created_at);
    let pw_ref = password.as_deref().filter(|s| !s.is_empty());
    let sealed = crate::e2ee::link::seal_link_share(&env, &share_id, rev, pw_ref)?;

    let expires_at = expires_days.map(|d| {
        let days = d.clamp(1, 365) as i64;
        (chrono::Utc::now() + chrono::Duration::days(days)).to_rfc3339()
    });
    let argon = if pw_ref.is_some() {
        Some(murmur_protocol::dto::ArgonParams {
            m: sealed.argon_params.m_cost_kib,
            t: sealed.argon_params.t_cost,
            p: sealed.argon_params.p_cost,
        })
    } else {
        None
    };
    let cell_bytes = sealed.ciphertext_cell.len();
    let create_req = murmur_protocol::dto::CreateShareRequest {
        share_id: share_id.clone(),
        mode: murmur_protocol::dto::ShareMode::Link,
        content_cell: sealed.ciphertext_cell,
        wrapped_nk: sealed.wrapped_nk,
        gate_salt: sealed.gate_salt.to_vec(),
        gate_secret: sealed.gate_secret.to_vec(),
        rev,
        password_required: pw_ref.is_some(),
        argon,
        expires_at,
        max_downloads: max_downloads.map(|n| n.max(1)),
        recipients: None,
    };
    let created = client.create_share(&access_token, create_req).await?;

    // (5) CONTENT-FREE egress ledger + local bookkeeping (share_id + document_id only, NO title).
    crate::share::ledger_row(&state.db, &client.host(), "share_create", cell_bytes);
    state.db.insert_outbound_note_share(
        &share_id,
        &id,
        "link",
        rev,
        &chrono::Utc::now().to_rfc3339(),
    )?;

    let base_for_url = if created.share_base_url.trim().is_empty() {
        base
    } else {
        created.share_base_url
    };
    Ok(crate::share::assemble_share_url(
        &base_for_url,
        &share_id,
        &sealed.l,
    ))
}

/// `list_my_shares() -> Vec<MyShareEntry>` — the server's share list, with each entry's local title
/// added ONLY when the meeting is unlocked (a sealed-not-unlocked meeting is MASKED: `locked:true`,
/// no title). §7 inv. 6.
#[tauri::command]
pub async fn list_my_shares(state: State<'_, AppState>) -> Result<Vec<MyShareEntry>, AppError> {
    let base = share_base_url(state.inner())?;
    let access = valid_access_token(state.inner()).await?;
    let client = crate::share::client::ShareClient::new(&base)?;
    let resp = client.list_shares(&access).await?;

    let mut out = Vec::with_capacity(resp.shares.len());
    for s in resp.shares {
        // NOTE shares (WP6) are anchored on `document_id`; meeting shares on `meeting_id`. Prefer the
        // note anchor. In BOTH cases the title is surfaced ONLY when the source's folder is unlocked
        // (a sealed-not-unlocked source is MASKED: `locked:true`, no title) — same §7 inv. 6 as
        // meetings. A share created on another device (neither anchor local) is masked too.
        let local_document = state.db.outbound_share_document(&s.share_id)?;
        let local_meeting = state.db.outbound_share_meeting(&s.share_id)?;
        let (title, locked) = if let Some(doc_id) = &local_document {
            // Note share: resolve title only when the note's folder is unlocked (via get_note_inner's
            // masking) — a masked note returns title "🔒 Locked"/locked:true.
            match state.db.get_note_row(doc_id)? {
                Some(row) if folder_is_unlocked(state.inner(), &row.folder_id)? => {
                    (Some(note_display_title(&row)), false)
                }
                Some(_) => (None, true), // sealed-not-unlocked ⇒ masked.
                None => (None, true),    // note deleted / unknown.
            }
        } else {
            // Meeting share: gate on the meeting's lock state (the original path).
            match local_meeting.as_deref().filter(|m| !m.is_empty()) {
                Some(meeting_id) => {
                    if meeting_is_unlocked(state.inner(), meeting_id)? {
                        let t = state.db.get_meeting(meeting_id)?.and_then(|m| m.title);
                        (t, false)
                    } else {
                        (None, true)
                    }
                }
                None => (None, true),
            }
        };
        out.push(MyShareEntry {
            share_id: s.share_id,
            title,
            locked,
            rev: s.rev,
            created_at: s.created_at,
            expires_at: s.expires_at,
            revoked: s.revoked_at.is_some(),
            download_count: s.download_count,
            // The meeting anchor is masked-empty ('') for a note share — surface it as None there so
            // the FE never keys a note share on an empty meeting id.
            meeting_id: local_meeting.filter(|m| !m.is_empty()),
            document_id: local_document,
            max_downloads: s.max_downloads,
            mode: s.mode,
        });
    }
    Ok(out)
}

/// `revoke_share(share_id)` — DELETE the server ciphertext + flip the local state. Idempotent.
#[tauri::command]
pub async fn revoke_share(state: State<'_, AppState>, share_id: String) -> Result<(), AppError> {
    revoke_share_inner(state.inner(), share_id).await
}

/// Inner of [`revoke_share`] taking `&AppState` so bulk callers (`revoke_shares_for_folder`) can reuse
/// the exact link/user revoke path (server revoke → local `revoked` → content-free ledger).
pub(crate) async fn revoke_share_inner(state: &AppState, share_id: String) -> Result<(), AppError> {
    let base = share_base_url(state)?;
    let access = valid_access_token(state).await?;
    let client = crate::share::client::ShareClient::new(&base)?;
    client.revoke_share(&access, &share_id).await?;
    state.db.set_outbound_share_state(&share_id, "revoked")?;
    crate::share::ledger_row(&state.db, &client.host(), "share_revoke", 0);
    Ok(())
}

/// The read-only result of previewing a recipient (spec §4.8): is the address a Murmur account, its
/// safety-word fingerprint, and whether this is first contact (show + confirm) or a BLOCKING key
/// change (re-verify out of band). Mutates NO pin — the FE shows this before the user commits.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecipientPreview {
    pub registered: bool,
    pub fingerprint: Option<String>,
    pub first_contact: bool,
    pub key_changed: bool,
}

/// The outcome of `share_note_to_user`: `"sent"` (recipient was a registered account, wrapped now) or
/// `"invited"` (unregistered → a pending invite; a re-wrap follows when they register). The
/// `fingerprint` is present for a registered recipient (the safety word the FE can echo).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareToUserResult {
    pub status: String,
    pub fingerprint: Option<String>,
}

/// One incoming (pending-accept) share in the inbox. CONTENT-FREE by construction — no title exists
/// server-side; the title only materializes locally on accept (inside the verified envelope).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareInboxItem {
    pub share_id: String,
    pub sender_fingerprint: String,
    pub rev: u32,
    pub size: u64,
    pub created_at: String,
    /// Already accepted locally (idempotency) — the FE can render it as done.
    pub already_accepted: bool,
}

/// The result of accepting a share: the new local meeting + its title (now known, from the verified
/// envelope).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AcceptedShare {
    pub meeting_id: String,
    pub title: String,
}

/// Normalize an email for use as a stable pin key + server lookup (trim + lowercase).
fn norm_email(email: &str) -> String {
    email.trim().to_lowercase()
}

/// The configured vault path (empty ⇒ `None`), read over `&AppState`.
fn config_vault(state: &AppState) -> Option<String> {
    state
        .config
        .lock()
        .ok()
        .and_then(|c| c.vault_path.clone())
        .filter(|p| !p.trim().is_empty())
}

/// `preview_share_recipient(email)` — is the address a Murmur account, and (if so) its fingerprint +
/// TOFU state. Read-only (pins nothing). Requires login + a configured server.
#[tauri::command]
pub async fn preview_share_recipient(
    state: State<'_, AppState>,
    email: String,
) -> Result<RecipientPreview, AppError> {
    let base = share_base_url(state.inner())?;
    let access = valid_access_token(state.inner()).await?;
    let client = crate::share::client::ShareClient::new(&base)?;
    let resp = client.lookup_key(&access, email.trim()).await?;
    let Some(key) = resp.key.filter(|_| resp.registered) else {
        return Ok(RecipientPreview {
            registered: false,
            fingerprint: None,
            first_contact: false,
            key_changed: false,
        });
    };
    // Recompute the fingerprint locally (never trust the server's string blindly).
    let fp = crate::e2ee::key_fingerprint(&key.pk_enc, &key.pk_sig);
    // Pin/check on the STABLE server account id (not the email) so send + accept share one namespace.
    let (first_contact, key_changed) = match tofu_check(&state.db, &key.user_id, &fp)? {
        TofuState::FirstContact => (true, false),
        TofuState::Match => (false, false),
        TofuState::Changed => (false, true),
    };
    Ok(RecipientPreview {
        registered: true,
        fingerprint: Some(fp),
        first_contact,
        key_changed,
    })
}

/// `share_note_to_user(meeting_id, recipient_email, expires_days?)` — mode-B share (spec §4.8/§7).
#[tauri::command]
pub async fn share_note_to_user(
    state: State<'_, AppState>,
    meeting_id: String,
    recipient_email: String,
    expires_days: Option<u32>,
) -> Result<ShareToUserResult, AppError> {
    share_note_to_user_inner(state.inner(), meeting_id, recipient_email, expires_days).await
}

/// Core of [`share_note_to_user`] over `&AppState`. Gate order is normative — DO NOT reorder.
pub(crate) async fn share_note_to_user_inner(
    state: &AppState,
    meeting_id: String,
    recipient_email: String,
    expires_days: Option<u32>,
) -> Result<ShareToUserResult, AppError> {
    // (1) READ-GATE — FIRST statement (copies `export_note`). A sealed-not-unlocked meeting refuses.
    if !meeting_is_unlocked(state, &meeting_id)? {
        return Err(AppError::Locked(
            "this meeting's folder is locked — unlock it to share the note".into(),
        ));
    }

    // (2) consent (fail-closed, first-ever share) + login (needs MK to derive sk_sig for the grant).
    let base = {
        let cfg = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
        if !cfg.share_egress_consented {
            return Err(AppError::Unavailable(
                "sharing not consented — confirm the one-time upload notice first".into(),
            ));
        }
        cfg.share_base_url.clone()
    };
    let (account_id, generation, mk, access_token) = require_session_mk(state).await?;
    let client = crate::share::client::ShareClient::new(&base)?;

    // (3) Fetch + CLEAN the note (gated read), build the inner envelope, seal a fresh NK.
    let note = state
        .db
        .get_latest_note_for_meeting(&meeting_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no note for meeting {meeting_id}")))?;
    let meeting = state.db.get_meeting(&meeting_id)?;
    let title = meeting
        .as_ref()
        .and_then(|m| m.title.clone())
        .unwrap_or_else(|| "Shared note".to_string());
    let created_at = meeting
        .as_ref()
        .map(|m| m.started_at.clone())
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
    let clean_body = crate::share::envelope::clean_note_body(&note.markdown);

    let share_id = crate::share::new_share_id();
    let rev = 1u32;
    let nk = crate::e2ee::random_key32()?;
    let env = murmur_protocol::envelope::ShareEnvelope::new(title, clean_body, created_at);
    let content_cell = crate::e2ee::seal_content(&nk, &env, &share_id, rev)?;
    let content_hash = {
        use sha2::{Digest, Sha256};
        Sha256::digest(&content_cell).to_vec()
    };
    // (3b) Wrap the RETAINED NK under the account MK (share-scoped AAD) BEFORE it is persisted — so a
    // re-locked session (no MK) can no longer decrypt an already-shared envelope from the retained
    // blob. Only `share_rewrap_pending`, which holds the MK session, unwraps it. NK stays Zeroizing.
    let nk_wrapped = crate::e2ee::wrap_key32(
        &mk,
        &nk,
        crate::e2ee::outbound_nk_at_rest_aad(&share_id).as_bytes(),
    )?;

    // (4) Look up the recipient → TOFU pin/verify; wrap-now (registered) or invite (unregistered).
    let recipient_email = recipient_email.trim().to_string();
    let recipient_acct = norm_email(&recipient_email);
    let lookup = client.lookup_key(&access_token, &recipient_email).await?;

    let expires_at = expires_days.map(|d| {
        let days = d.clamp(1, 365) as i64;
        (chrono::Utc::now() + chrono::Duration::days(days)).to_rfc3339()
    });

    let (recipients, status, fingerprint) =
        if let Some(key) = lookup.key.filter(|_| lookup.registered) {
            // Registered → verify the fingerprint + enforce TOFU (BLOCK on a changed key), then wrap now.
            let fp = crate::e2ee::key_fingerprint(&key.pk_enc, &key.pk_sig);
            match tofu_check(&state.db, &key.user_id, &fp)? {
                TofuState::Changed => {
                    return Err(AppError::Other(anyhow::anyhow!(
                    "this contact's key changed since you last shared — re-verify the safety words \
                     out of band, then share again"
                )));
                }
                _ => state.db.pin_contact(
                    &key.user_id,
                    Some(&recipient_email),
                    &fp,
                    &chrono::Utc::now().to_rfc3339(),
                )?,
            }

            // Derive OUR identity from MK and sign the grant (fingerprints are the party ids in the grant).
            let sender = crate::e2ee::keys::derive_identity(&mk, &account_id, generation)?;
            let sender_fp = crate::e2ee::key_fingerprint(&sender.pk_enc, &sender.pk_sig);
            let grant = crate::e2ee::wrap::seal_to_recipient(
                &nk,
                &content_cell,
                &key.pk_enc,
                &fp, // recipient_acct_id = recipient fingerprint
                &sender,
                &sender_fp, // sender_acct_id = our fingerprint
                generation,
                &share_id,
                rev,
            )?;
            let wrapped_key =
                crate::e2ee::wrap::pack_wrapped_key(&sender.pk_enc, &sender.pk_sig, &grant)?;
            let recipients = vec![murmur_protocol::dto::ShareRecipientInput {
                email: recipient_email.clone(),
                wrapped_key: Some(wrapped_key),
                key_generation: Some(generation),
                grant_sig: Some(grant.signature),
            }];
            // Retain the MK-wrapped NK + content_hash locally so an "Update share" / re-wrap can reuse
            // them; state 'sent'.
            state.db.insert_outbound_user_share(
                &share_id,
                &meeting_id,
                rev,
                &chrono::Utc::now().to_rfc3339(),
                "sent",
                &nk_wrapped,
                &recipient_acct,
                &recipient_email,
                &content_hash,
            )?;
            (recipients, "sent".to_string(), Some(fp))
        } else {
            // Unregistered → an invite; retain the MK-wrapped NK + content_hash for the on-launch
            // re-wrap ('awaiting_key').
            let recipients = vec![murmur_protocol::dto::ShareRecipientInput {
                email: recipient_email.clone(),
                wrapped_key: None,
                key_generation: None,
                grant_sig: None,
            }];
            state.db.insert_outbound_user_share(
                &share_id,
                &meeting_id,
                rev,
                &chrono::Utc::now().to_rfc3339(),
                "awaiting_key",
                &nk_wrapped,
                &recipient_acct,
                &recipient_email,
                &content_hash,
            )?;
            (recipients, "invited".to_string(), None)
        };

    // (5) Upload — mode='user'; the link fields are unused (empty). NO note content/title in the body.
    let create_req =
        assemble_user_share_request(&share_id, rev, content_cell.clone(), recipients, expires_at);
    let _ = client.create_user_share(&access_token, create_req).await?;

    // (6) CONTENT-FREE egress ledger (host + cell byte size). NEVER a title / note text / key.
    crate::share::ledger_row(
        &state.db,
        &client.host(),
        if status == "sent" {
            "share_user_send"
        } else {
            "share_user_invite"
        },
        content_cell.len(),
    );

    Ok(ShareToUserResult {
        status,
        fingerprint,
    })
}

/// Assemble the `POST /v1/shares` body for a mode-B share. PURE (so a test can assert the serialized
/// request carries NO note title / body — only ciphertext + wrapped keys + the recipient email).
pub(crate) fn assemble_user_share_request(
    share_id: &str,
    rev: u32,
    content_cell: Vec<u8>,
    recipients: Vec<murmur_protocol::dto::ShareRecipientInput>,
    expires_at: Option<String>,
) -> murmur_protocol::dto::CreateShareRequest {
    murmur_protocol::dto::CreateShareRequest {
        share_id: share_id.to_string(),
        mode: murmur_protocol::dto::ShareMode::User,
        content_cell,
        // Mode-B: the link fields are unused (the NK is wrapped per-recipient via HPKE instead).
        wrapped_nk: Vec::new(),
        gate_salt: Vec::new(),
        gate_secret: Vec::new(),
        rev,
        password_required: false,
        argon: None,
        expires_at,
        max_downloads: None,
        recipients: Some(recipients),
    }
}

/// `share_rewrap_pending()` — for each locally-retained mode-B invite whose recipient has since
/// registered, re-wrap the retained NK to their now-published key and attach it (`PUT /shares/{id}/
/// keys`). Reads ONLY key material + retained NK (never meeting content) → no read-gate. Returns the
/// number of shares advanced to `sent`.
#[tauri::command]
pub async fn share_rewrap_pending(state: State<'_, AppState>) -> Result<u32, AppError> {
    share_rewrap_pending_inner(state.inner()).await
}

pub(crate) async fn share_rewrap_pending_inner(state: &AppState) -> Result<u32, AppError> {
    let base = share_base_url(state)?;
    if base.trim().is_empty() {
        return Ok(0);
    }
    // Logged out ⇒ nothing to do (not an error — this is a best-effort launch sweep).
    let Ok((account_id, generation, mk, access_token)) = require_session_mk(state).await else {
        return Ok(0);
    };
    let client = crate::share::client::ShareClient::new(&base)?;
    let sender = crate::e2ee::keys::derive_identity(&mk, &account_id, generation)?;
    let sender_fp = crate::e2ee::key_fingerprint(&sender.pk_enc, &sender.pk_sig);

    let mut advanced = 0u32;
    for (share_id, rev, nk_bytes, nk_is_wrapped, recipient_email, content_hash) in
        state.db.list_awaiting_rewrap()?
    {
        // Re-look-up the recipient. Not registered yet / lookup error ⇒ leave it pending.
        let Ok(lookup) = client.lookup_key(&access_token, &recipient_email).await else {
            continue;
        };
        let Some(key) = lookup.key.filter(|_| lookup.registered) else {
            continue;
        };
        let fp = crate::e2ee::key_fingerprint(&key.pk_enc, &key.pk_sig);
        // A changed key on a not-yet-pinned invitee is first contact; a CHANGED existing pin is
        // blocking — skip it (don't silently re-wrap to a rotated key). Pin on the STABLE server
        // account id (not email) so send + accept share one namespace.
        match tofu_check(&state.db, &key.user_id, &fp)? {
            TofuState::Changed => continue,
            _ => state.db.pin_contact(
                &key.user_id,
                Some(&recipient_email),
                &fp,
                &chrono::Utc::now().to_rfc3339(),
            )?,
        }
        // Unwrap the retained NK: MK-wrapped (0.7+ rows, needs the live MK session) or legacy raw
        // (pre-0.7 rows, unwrap = identity). A wrong-MK / tampered / malformed blob ⇒ leave pending.
        let nk = if nk_is_wrapped {
            match crate::e2ee::unwrap_key32(
                &mk,
                &nk_bytes,
                crate::e2ee::outbound_nk_at_rest_aad(&share_id).as_bytes(),
            ) {
                Ok(k) => k,
                Err(_) => continue,
            }
        } else {
            let Ok(nk_arr) = crate::e2ee::to_arr32(&nk_bytes) else {
                continue;
            };
            zeroize::Zeroizing::new(nk_arr)
        };
        let grant = crate::e2ee::wrap::seal_to_recipient_with_hash(
            &nk,
            &content_hash,
            &key.pk_enc,
            &fp,
            &sender,
            &sender_fp,
            generation,
            &share_id,
            rev,
        )?;
        let wrapped_key =
            crate::e2ee::wrap::pack_wrapped_key(&sender.pk_enc, &sender.pk_sig, &grant)?;
        // `PUT /shares/{id}/keys` keys the recipient row by the SERVER user id, which `keys/lookup`
        // now returns (`key.user_id`) — so the attach resolves correctly (closes the earlier no-op
        // gap). The re-wrap crypto above is complete + verified.
        let attach = client
            .attach_key(
                &access_token,
                &share_id,
                murmur_protocol::dto::AttachKeyRequest {
                    recipient_acct_id: key.user_id.clone(),
                    wrapped_key,
                    key_generation: generation,
                    grant_sig: grant.signature,
                },
            )
            .await;
        if attach.is_ok() {
            state.db.set_outbound_share_state(&share_id, "sent")?;
            crate::share::ledger_row(&state.db, &client.host(), "share_user_rewrap", 0);
            advanced += 1;
        }
    }
    Ok(advanced)
}

/// `list_share_inbox()` — the caller's incoming pending-accept shares (content-free). No gate: no
/// local content is read; each item's title is unknown until accept decrypts the envelope.
#[tauri::command]
pub async fn list_share_inbox(state: State<'_, AppState>) -> Result<Vec<ShareInboxItem>, AppError> {
    let base = share_base_url(state.inner())?;
    let access = valid_access_token(state.inner()).await?;
    let client = crate::share::client::ShareClient::new(&base)?;
    let resp = client.list_inbox(&access).await?;
    let mut out = Vec::with_capacity(resp.items.len());
    for i in resp.items {
        let already_accepted = state.db.inbound_share_meeting(&i.share_id)?.is_some();
        out.push(ShareInboxItem {
            share_id: i.share_id,
            sender_fingerprint: i.sender_fingerprint,
            rev: i.rev,
            size: i.size,
            created_at: i.created_at,
            already_accepted,
        });
    }
    Ok(out)
}

/// `accept_share(share_id, folder_id?)` — THE HIGH-BAR vault WRITE. See the module invariants above.
#[tauri::command]
pub async fn accept_share(
    state: State<'_, AppState>,
    share_id: String,
    folder_id: Option<String>,
) -> Result<AcceptedShare, AppError> {
    accept_share_inner(state.inner(), share_id, folder_id).await
}

pub(crate) async fn accept_share_inner(
    state: &AppState,
    share_id: String,
    folder_id: Option<String>,
) -> Result<AcceptedShare, AppError> {
    // (1) IDEMPOTENT on share_id — a re-accept returns the existing meeting, never a duplicate note.
    if let Some(mid) = state.db.inbound_share_meeting(&share_id)? {
        let title = state
            .db
            .get_meeting(&mid)?
            .and_then(|m| m.title)
            .unwrap_or_else(|| "Shared note".to_string());
        return Ok(AcceptedShare {
            meeting_id: mid,
            title,
        });
    }

    // (1b) RESUME a stranded accept: a prior attempt flipped the server row to `accepted` but failed
    //      before the local ingest committed. The server no longer lists an accepted share in the
    //      inbox and a re-accept 404s, so without the durable resume record the share would be lost.
    //      Re-fetch (the blob stays fetchable while `accepted`) + re-verify + ingest from the record.
    if let Some(pending) = state.db.get_pending_share_accept(&share_id)? {
        return resume_pending_accept(state, pending).await;
    }

    // (2) WRITE-GATE the target folder FIRST (mirror `ingest_into_folder`). Default = an auto-created
    //     UNSEALED "Shared" folder; a sealed-not-session-unlocked target is REFUSED (write nothing).
    let target = resolve_accept_folder(state, folder_id.as_deref())?;

    // (3) Need a session (MK derives the recipient identity for HPKE-open) + server.
    let (account_id, generation, mk, access) = require_session_mk(state).await?;
    let base = share_base_url(state)?;
    let client = crate::share::client::ShareClient::new(&base)?;

    // (4) Find the pending inbox item for this share.
    let inbox = client.list_inbox(&access).await?;
    let item = inbox
        .items
        .into_iter()
        .find(|i| i.share_id == share_id)
        .ok_or_else(|| {
            AppError::InvalidArg(
                "no pending share to accept (already accepted/declined, expired, or not addressed to you)"
                    .into(),
            )
        })?;

    // (5) Unpack the sender's public identity + grant from the opaque blob; ATTEST the fingerprint
    //     against the server-relayed value, then TOFU (BLOCK on a changed key) — all before any write.
    let up = crate::e2ee::wrap::unpack_wrapped_key(&item.wrapped_key, &item.grant_sig)?;
    let sender_fp = crate::e2ee::key_fingerprint(&up.sender_pk_enc, &up.sender_pk_sig);
    if sender_fp != item.sender_fingerprint {
        return Err(AppError::InvalidArg(
            "share sender identity does not match the server-attested fingerprint — refusing"
                .into(),
        ));
    }
    // TOFU: BLOCK on a changed key before doing any work. But DEFER pinning a first-contact key until
    // AFTER a successful ingest (step 9 below) — otherwise a malicious server could pre-poison a pin
    // with a first-contact item whose grant later fails verification (adversarial finding).
    if matches!(
        tofu_check(&state.db, &item.sender_user_id, &sender_fp)?,
        TofuState::Changed
    ) {
        return Err(AppError::Other(anyhow::anyhow!(
            "this sender's key changed since you last accepted from them — re-verify the safety \
             words out of band before accepting"
        )));
    }

    // (6) Flip the server row to accepted (authorizes the blob fetch). This is the point of no return
    //     server-side: a re-accept 404s and the inbox drops the item.
    let accepted = client.accept_share_server(&access, &share_id).await?;

    // (6b) DURABLY record the resume state BEFORE the fetch/verify/ingest below — so ANY failure in
    //      (7)/(8) is recoverable from the CLIENT via the resume path (step 1b), never a stranded
    //      share. Carries only the opaque server-relayed key material the inbox already held.
    state
        .db
        .insert_pending_share_accept(&crate::storage::PendingShareAccept {
            share_id: share_id.clone(),
            blob_id: accepted.blob_id.clone(),
            target_folder_id: target.id.clone(),
            sender_user_id: item.sender_user_id.clone(),
            sender_fingerprint: sender_fp.clone(),
            wrapped_key: item.wrapped_key.clone(),
            grant_sig: item.grant_sig.clone(),
            rev: item.rev,
            key_generation: item.key_generation,
            created_at: chrono::Utc::now().to_rfc3339(),
        })?;

    // (7)+(8)+(9) fetch + VERIFY (§4.8) + decrypt + ingest + pin + drop the resume record.
    let recipient = crate::e2ee::keys::derive_identity(&mk, &account_id, generation)?;
    finalize_accepted_share(
        state,
        &client,
        &access,
        &target,
        &recipient,
        &sender_fp,
        &item.sender_user_id,
        &up,
        &accepted.blob_id,
        &share_id,
        item.rev,
        item.key_generation,
    )
    .await
}

/// The shared TAIL of an accept: fetch the (recipiency-authorized) content blob, VERIFY §4.8 +
/// decrypt + ingest into the write-gated folder, pin the sender AFTER a verified ingest, drop the
/// durable resume record, and ledger. Used by both the normal path (right after the server flip) and
/// the RESUME path (after a strand). Writes NOTHING on any verification/fetch failure — and leaves the
/// resume record in place on failure so a retry can finish.
#[allow(clippy::too_many_arguments)]
async fn finalize_accepted_share(
    state: &AppState,
    client: &crate::share::client::ShareClient,
    access: &str,
    target: &Folder,
    recipient: &crate::e2ee::keys::IdentityKeypair,
    sender_fp: &str,
    sender_user_id: &str,
    up: &crate::e2ee::wrap::UnpackedGrant,
    blob_id: &str,
    share_id: &str,
    rev: u32,
    key_generation: u32,
) -> Result<AcceptedShare, AppError> {
    let content_cell = client.get_blob(access, blob_id).await?;
    let result = accept_ingest_verified(
        state,
        target,
        recipient,
        sender_fp,
        sender_user_id,
        up,
        &content_cell,
        share_id,
        rev,
        key_generation,
    )?;
    // Pin ONLY NOW — after the grant verified (§4.8) and the note landed — so a forged/failed item
    // never leaves a poisoned pin. Then drop the resume record (the strand window is closed).
    state.db.pin_contact(
        sender_user_id,
        None,
        sender_fp,
        &chrono::Utc::now().to_rfc3339(),
    )?;
    state.db.delete_pending_share_accept(share_id)?;
    crate::share::ledger_row(
        &state.db,
        &client.host(),
        "share_accept",
        content_cell.len(),
    );
    Ok(result)
}

/// RESUME a stranded accept from its durable [`PendingShareAccept`] record: the server row was flipped
/// to `accepted` on a prior attempt but the local verify+ingest failed after the flip. Re-runs the
/// write-gate + fingerprint-attest + TOFU-block (never trust the saved state blindly), then finishes
/// via [`finalize_accepted_share`]. This makes the server flip effectively idempotent from the client.
async fn resume_pending_accept(
    state: &AppState,
    pending: crate::storage::PendingShareAccept,
) -> Result<AcceptedShare, AppError> {
    // (2) WRITE-GATE the saved target folder FIRST — refuse if it was sealed since the flip.
    let target = state
        .db
        .folder_by_id(&pending.target_folder_id)?
        .ok_or_else(|| AppError::InvalidArg("the share's target folder no longer exists".into()))?;
    if target.locked && !folder_is_unlocked(state, &target.id)? {
        return Err(AppError::Locked(
            "the target folder is locked — unlock it first to finish accepting the share".into(),
        ));
    }
    // (3) Session (MK derives our identity for HPKE-open) + server.
    let (account_id, generation, mk, access) = require_session_mk(state).await?;
    let base = share_base_url(state)?;
    let client = crate::share::client::ShareClient::new(&base)?;

    // (5) Re-unpack + ATTEST the saved sender identity, then TOFU-BLOCK on a changed key — before any
    //     write, exactly like the normal path.
    let up = crate::e2ee::wrap::unpack_wrapped_key(&pending.wrapped_key, &pending.grant_sig)?;
    let sender_fp = crate::e2ee::key_fingerprint(&up.sender_pk_enc, &up.sender_pk_sig);
    if sender_fp != pending.sender_fingerprint {
        return Err(AppError::InvalidArg(
            "share sender identity does not match the retained fingerprint — refusing".into(),
        ));
    }
    if matches!(
        tofu_check(&state.db, &pending.sender_user_id, &sender_fp)?,
        TofuState::Changed
    ) {
        return Err(AppError::Other(anyhow::anyhow!(
            "this sender's key changed since you last accepted from them — re-verify the safety \
             words out of band before accepting"
        )));
    }

    // (8)+(9) fetch (the blob stays fetchable while `accepted`) + verify + ingest + pin + drop record.
    let recipient = crate::e2ee::keys::derive_identity(&mk, &account_id, generation)?;
    finalize_accepted_share(
        state,
        &client,
        &access,
        &target,
        &recipient,
        &sender_fp,
        &pending.sender_user_id,
        &up,
        &pending.blob_id,
        &pending.share_id,
        pending.rev,
        pending.key_generation,
    )
    .await
}

/// Resolve + WRITE-GATE the folder an accepted share lands in. `Some(id)` uses that folder (refusing a
/// sealed-not-unlocked one with `AppError::Locked`); `None` gets-or-creates the UNSEALED "Shared"
/// folder.
fn resolve_accept_folder(state: &AppState, folder_id: Option<&str>) -> Result<Folder, AppError> {
    match folder_id {
        Some(fid) => {
            let f = state
                .db
                .folder_by_id(fid)?
                .ok_or_else(|| AppError::InvalidArg(format!("no folder {fid}")))?;
            if f.locked && !folder_is_unlocked(state, fid)? {
                return Err(AppError::Locked(
                    "the target folder is locked — unlock it first to accept the share into it"
                        .into(),
                ));
            }
            Ok(f)
        }
        None => get_or_create_shared_folder(state),
    }
}

/// Get-or-create the UNSEALED "Shared" folder at the vault root (the default accept target). If it
/// already exists and is sealed-not-unlocked, the write-gate refuses (`AppError::Locked`).
pub(crate) fn get_or_create_shared_folder(state: &AppState) -> Result<Folder, AppError> {
    const SHARED: &str = "Shared";
    if let Some(f) = state.db.folder_by_path(SHARED)? {
        if f.locked && !folder_is_unlocked(state, &f.id)? {
            return Err(AppError::Locked(
                "your \"Shared\" folder is locked — unlock it (or pick another folder) to accept"
                    .into(),
            ));
        }
        return Ok(f);
    }
    let folder = Folder {
        id: uuid::Uuid::new_v4().to_string(),
        name: SHARED.to_string(),
        path: SHARED.to_string(),
        parent_id: None,
        locked: false,
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    if let Some(vault) = config_vault(state) {
        let dir = std::path::Path::new(&vault).join(SHARED);
        let _ = std::fs::create_dir_all(&dir);
    }
    state.db.insert_folder(&folder)?;
    Ok(folder)
}

/// The load-bearing crypto+write step, factored out so it is unit-testable with a crafted grant + no
/// network. It (a) VERIFIES the §4.8 grant via `open_from_sender` (HARD-FAILS unsigned / tampered /
/// replayed / swapped / gen-mismatch), (b) decrypts the content cell, and ONLY THEN (c) ingests the
/// note into the (already write-gated) folder. On ANY verification/decrypt failure it returns
/// `AppError::InvalidArg` and writes NOTHING.
#[allow(clippy::too_many_arguments)]
pub(crate) fn accept_ingest_verified(
    state: &AppState,
    target: &Folder,
    recipient: &crate::e2ee::keys::IdentityKeypair,
    sender_fp: &str,
    sender_user_id: &str,
    up: &crate::e2ee::wrap::UnpackedGrant,
    content_cell: &[u8],
    share_id: &str,
    rev: u32,
    key_generation: u32,
) -> Result<AcceptedShare, AppError> {
    let recipient_fp = crate::e2ee::key_fingerprint(&recipient.pk_enc, &recipient.pk_sig);
    // (a) §4.8 VERIFY before any write. The pinned pk_sig is the one we unpacked + fingerprint-attested.
    let nk = crate::e2ee::wrap::open_from_sender(
        &up.grant,
        content_cell,
        recipient,
        &recipient_fp, // recipient_acct_id
        &recipient_fp, // self_acct_id
        sender_fp,     // sender_acct_id (as signed)
        key_generation,
        sender_fp,         // pinned_sender_acct_id
        &up.sender_pk_sig, // pinned_sender_pk_sig (attested to the server fingerprint upstream)
        share_id,
        rev,
    )
    .map_err(|_| {
        AppError::InvalidArg(
            "share grant failed verification (unsigned / tampered / replayed) — refusing to ingest"
                .into(),
        )
    })?;
    // (b) Decrypt the content cell → the inner envelope (title travels INSIDE).
    let env = crate::e2ee::open_content(&nk, content_cell, share_id, rev).map_err(|_| {
        AppError::InvalidArg("shared note failed to decrypt — refusing to ingest".into())
    })?;
    // (c) Ingest into the write-gated folder.
    ingest_shared_note(state, target, &env, sender_fp, sender_user_id, share_id)
}

/// Write a VERIFIED shared note into the vault + DB: a new `Exported` meeting (audio `None`) + a
/// `"shared"` note carrying `shared-by`/`shared-at`/`share-id` provenance frontmatter, atomically
/// exported to the folder's vault subdir, and an `inbound_shares` idempotency record. The new meeting
/// is a NORMAL row → it participates in every existing gate automatically.
pub(crate) fn ingest_shared_note(
    state: &AppState,
    target: &Folder,
    env: &murmur_protocol::envelope::ShareEnvelope,
    sender_fp: &str,
    sender_user_id: &str,
    share_id: &str,
) -> Result<AcceptedShare, AppError> {
    // LOCK-SHARE-INGEST-1 (2026-07-11 audit, sealed-content leak): hold the lifecycle guard across the
    // whole ingest so a relock cannot land between the meeting insert and the (sealed) note write.
    let _lifecycle = lifecycle_guard(state);
    let meeting_id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    // A well-formed created_at (RFC3339) is kept; otherwise fall back to now (never trust the payload).
    let started_at = if chrono::DateTime::parse_from_rfc3339(env.created_at.trim()).is_ok() {
        env.created_at.trim().to_string()
    } else {
        now.clone()
    };
    let title = {
        let t = env.title.trim();
        if t.is_empty() {
            "Shared note".to_string()
        } else {
            t.to_string()
        }
    };
    // Provenance frontmatter. `shared-by` is the ATTESTED sender fingerprint (safe base32) — NEVER the
    // attacker-controlled envelope, so a malicious sender can't forge/inject provenance.
    let full_md = format!(
        "---\nshared-by: {sender_fp}\nshared-at: {now}\nshare-id: {share_id}\n---\n\n{}",
        env.markdown
    );

    // Meeting row (Exported, no audio), associated with the target folder.
    state.db.insert_meeting(&Meeting {
        id: meeting_id.clone(),
        started_at: started_at.clone(),
        ended_at: None,
        title: Some(title.clone()),
        duration_s: 0,
        audio_path: None,
        status: MeetingStatus::Exported,
        folder_id: Some(target.id.clone()),
    })?;

    // LOCK-SHARE-INGEST-1: a session-unlocked LOCKED target must NOT receive a plaintext `.md` on
    // disk NOR a plaintext note row — the pre-fix path wrote both via a RAW `upsert_note` + vault
    // export, so the plaintext survived the next relock at rest (a sealed-content leak). When the
    // target is locked, SEAL the note under the target folder CK from birth (verify-before-destroy
    // inside `upsert_note_reseal_if_locked`) and write NO vault file. An open target keeps the plain
    // write + export.
    let target_locked = state
        .db
        .folder_by_id(&target.id)?
        .map(|f| f.locked)
        .unwrap_or(false);

    // Set the meeting's folder FIRST so `upsert_note_reseal_if_locked` resolves the (locked) folder
    // via `folder_for_meeting`. It reads `notes.folder_id`, but there is no note row yet — so seed a
    // folder association through a folder-set on the (about-to-be-written) note. We do this by writing
    // the note with the folder resolved directly below instead of relying on a two-step.
    let exported_path = if target_locked {
        None // a locked folder has no on-disk export.
    } else {
        // Atomic vault export (best-effort — a missing/invalid vault just leaves exported_path None;
        // the note is still durable in the DB, the source of truth).
        config_vault(state).and_then(|vault| {
            crate::export::write_note(
                std::path::Path::new(&vault),
                Some(&target.path),
                &title,
                &started_at,
                &full_md,
            )
            .ok()
            .map(|p| p.to_string_lossy().to_string())
        })
    };

    let note = NoteRecord {
        meeting_id: meeting_id.clone(),
        provider_id: "shared".to_string(),
        markdown: full_md,
        created_at: now.clone(),
        exported_path,
        model_requested: None,
        model_served: None,
        gateway_host: None,
    };
    if target_locked {
        // Seal under the TARGET folder CK from birth. `upsert_note_sealed` also writes the governing
        // `folder_id`, so the gate + reblank lifecycle see this note as living in the target folder.
        // Fail-closed on a missing session KEK (never unsealed plaintext behind a lock).
        let ck = session_folder_ck(state, &target.id)?;
        // SAME AAD the folder seal / `upsert_note_reseal_if_locked` use:
        // aad_content(folder, meeting, provider, "note").
        let aad = aad_content(&target.id, &note.meeting_id, &note.provider_id, "note");
        let blob = crate::crypto::encrypt(&ck, note.markdown.as_bytes(), &aad)?;
        if crate::crypto::decrypt(&ck, &blob, &aad)? != note.markdown.as_bytes() {
            return Err(AppError::Storage(
                "shared-note seal-on-ingest verification failed (blob mismatch)".into(),
            ));
        }
        state.db.upsert_note_sealed(&note, &blob, &target.id)?;
    } else {
        state.db.upsert_note(&note)?;
        // The meeting's folder is resolved via `notes.folder_id` (`folder_for_meeting`) — set it so
        // every gate (`meeting_is_unlocked`, `visibility_clause`) sees this note in the target folder.
        state.db.set_note_folder(&meeting_id, Some(&target.id))?;
    }

    // Idempotency + provenance record (a re-accept of this share_id is INSERT-OR-IGNORE'd).
    state
        .db
        .insert_inbound_share(share_id, &meeting_id, sender_user_id, &now)?;

    tracing::info!(
        target: "share",
        share_id = %share_id,
        meeting_id = %meeting_id,
        folder_id = %target.id,
        "accepted a shared note into the vault"
    );
    Ok(AcceptedShare { meeting_id, title })
}

/// `decline_share(share_id)` — drop the wrapped key server-side + flip the local state. Idempotent.
#[tauri::command]
pub async fn decline_share(state: State<'_, AppState>, share_id: String) -> Result<(), AppError> {
    let base = share_base_url(state.inner())?;
    let access = valid_access_token(state.inner()).await?;
    let client = crate::share::client::ShareClient::new(&base)?;
    client.decline_share_server(&access, &share_id).await?;
    crate::share::ledger_row(&state.db, &client.host(), "share_decline", 0);
    Ok(())
}

// ════════════════════════════════ M6 Shared Brain (Organizations) ════════════════════════════════
//
// The org command surface: create/status/members + consent + preview + share (meeting/document) +
// list/revoke. Every content read is GATED first (`meeting_is_unlocked` / the sealed-doc refusal),
// egress is fail-closed on the one-time org consent, the payload passes `clean_note_body` + a regex
// PII scrub, and the envelope is sealed under the OCK with a LOCAL open-verify before it is uploaded.
// A content-free egress-ledger row records each publish. The OCK is unwrapped on demand + cached in
// RAM (`AppState::org_ock_cache`), NEVER persisted or logged.

/// Content-free org status for the FE (spec DTO `OrgStatus`).
#[derive(serde::Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct OrgStatus {
    pub org_id: String,
    pub name: String,
    pub role: String,
    pub member_count: u32,
    pub consented: bool,
    pub last_seq: i64,
    /// The caller's OWN outbound uploads into this org (`org_shares` in state `uploaded`).
    pub item_count: u32,
    /// RECEIVED items in the local org replica (`org_items`, non-tombstoned) — what colleagues shared
    /// IN. Distinct from `item_count`: a member who only RECEIVES shows `item_count = 0` but a real
    /// `received_count`, so the Settings count no longer lies "0 items" to a receiver.
    pub received_count: u32,
    pub pending_shares: u32,
    /// PER-INSTANCE org toggle: whether this org contributes content (browsing + brain context) on
    /// THIS Murmur install. `true` by default; flip via `org_set_context_enabled`.
    pub context_enabled: bool,
}

/// One org member row for the FE (spec DTO `OrgMember`).
#[derive(serde::Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct OrgMember {
    pub user_id: String,
    pub email: Option<String>,
    pub role: String,
    pub added_at: String,
    pub removed: bool,
}

/// The exact post-clean, post-scrub preview of an outgoing org share (spec DTO `OrgSharePreview`).
/// Returned WITHOUT any egress — the FE renders this so the user sees precisely what would leave.
#[derive(serde::Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct OrgSharePreview {
    pub title: String,
    pub markdown: String,
    pub bytes: u32,
    pub chunk_count: u32,
    pub scrubbed: OrgScrubCounts,
    pub scrub: bool,
}

/// The count of PII placeholders the regex scrub removed, by kind (content-free).
#[derive(serde::Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct OrgScrubCounts {
    pub emails: u32,
    pub phones: u32,
    pub cards: u32,
}

/// One outbound org-share entry for the FE (spec DTO `OrgShareEntry`).
#[derive(serde::Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct OrgShareEntry {
    pub item_id: Option<String>,
    pub kind: String,
    pub title: Option<String>,
    pub shared_at: String,
    pub rev: u32,
    pub state: String,
}

/// Which org already holds a LIVE (`uploaded`) share of a given LOCAL source (meeting/note), so the FE
/// can mark that org "Already added ✓" and BLOCK a re-share (the double-click duplicate fix). Content-
/// free: only the org id + the server item id + rev — never a title or any other member's data. Powers
/// `org_live_shares_for_source` (a READ, no egress).
#[derive(serde::Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct OrgSourceShareStatus {
    pub org_id: String,
    pub item_id: Option<String>,
    pub rev: u32,
}

/// The LOCAL editable SOURCE behind an org item, if the CALLER is its author. `org_resolve_source`
/// returns `Some` only when THIS device holds the `org_shares` row for the item (i.e. the caller
/// published it) — so a member reading a colleague's org item gets `None` (no editable source). The
/// FE uses it to route an org-item viewer to the real note/meeting editor for one's OWN shares.
#[derive(serde::Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct OrgSourceRef {
    /// `"document"` (an authored note) or `"meeting"` (a meeting note).
    pub kind: String,
    /// The local `documents.id` (for a note) or `meetings.id` (for a meeting).
    pub source_id: String,
}

/// The org-scoped "grant author" identity id we pin the OCK granter on. For v1 (single-org, owner
/// issues all grants) this is the org's OWNER account id. Derived from the caller's own session for
/// the owner path; the recipient pins it on first grant open. Uses the account fingerprint namespace
/// consistent with mode-B (`key_fingerprint`).
///
/// The AAD item nonce that binds an org envelope's ciphertext — the LOWERCASE-HEX of the plaintext
/// `content_sha256`. DETERMINISTIC + shared across members: the publisher derives it from the
/// envelope it seals, and every consumer derives the SAME value from the feed's `content_sha256`
/// (which Core populates with the PLAINTEXT hash). This is what makes a cross-member `open_org_envelope`
/// succeed — the server's assigned `item_id` is NOT known to the publisher at seal time, so it can't
/// be the nonce. Collision-resistant per content (SHA-256) so the anti-replay AAD binding holds.
pub(crate) fn org_item_nonce(content_sha256: &[u8]) -> String {
    let mut s = String::with_capacity(content_sha256.len() * 2);
    for b in content_sha256 {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// A display label for `author_hint` in the OrgEnvelope — the email local-part, NEVER note content.
fn org_author_hint(email: &str) -> String {
    let e = email.trim();
    match e.split_once('@') {
        Some((local, _)) if !local.is_empty() => local.to_string(),
        _ => "member".to_string(),
    }
}

/// Regex-scrub emails / phones / cards out of `markdown` (names KEPT, per the user-approved org
/// redaction policy). Returns `(scrubbed_markdown, counts)`. Reuses the SAME regex firewall
/// primitive as the cloud path (`summarize::redact::redact`) so an org share is masked exactly like a
/// cloud-bound prompt — minus the name layer (org peers keep names on purpose). Pure, no egress.
fn scrub_org_markdown(markdown: &str) -> (String, OrgScrubCounts) {
    let (scrubbed, map) = crate::summarize::redact::redact(markdown);
    let mut counts = OrgScrubCounts::default();
    for key in map.keys() {
        let inner = key.trim_start_matches('\u{27ea}');
        if inner.starts_with("EMAIL") {
            counts.emails += 1;
        } else if inner.starts_with("CARD") {
            counts.cards += 1;
        } else if inner.starts_with("PHONE") {
            counts.phones += 1;
        }
    }
    (scrubbed, counts)
}

/// A rough chunk count for the preview (mirrors the retrieval chunker's ~paragraph granularity
/// without importing it — display-only). Non-empty blank-line-separated blocks, min 1 for any text.
fn rough_chunk_count(markdown: &str) -> u32 {
    let n = markdown
        .split("\n\n")
        .filter(|b| !b.trim().is_empty())
        .count();
    if n == 0 && !markdown.trim().is_empty() {
        1
    } else {
        n as u32
    }
}

/// The org's live OCK for `generation`, acquired for THIS session: served from the RAM cache
/// (`AppState::org_ock_cache`) when present, else unwrapped from the caller's server-relayed grant
/// (gated on the account MK session) and cached. NEVER persisted, NEVER logged. Fails closed
/// (`Unavailable`) logged out, (`Auth`) on a forged/mismatched grant.
async fn acquire_org_ock(
    state: &AppState,
    org_id: &str,
    generation: u32,
) -> Result<zeroize::Zeroizing<[u8; 32]>, AppError> {
    // Cache hit?
    {
        let cache = state
            .org_ock_cache
            .lock()
            .map_err(|_| AppError::Storage("org-ock cache mutex poisoned".into()))?;
        if let Some(k) = cache.get(&(org_id.to_string(), generation)) {
            return Ok(zeroize::Zeroizing::new(**k));
        }
    }

    // Miss → unwrap from the caller's server-relayed grant. Needs the MK session (to derive our
    // identity keypair) + a valid bearer.
    let (account_id, gen_id, mk, access_token) = require_session_mk(state).await?;
    // Grants are keyed by the server user id (UUID) — NOT the email `account_id`.
    let server_user_id = session_server_user_id(state)?;
    let base = share_base_url(state)?;
    let client = crate::share::client::ShareClient::new(&base)?;
    let recipient = crate::e2ee::keys::derive_identity(&mk, &account_id, gen_id)?;
    let self_fp = crate::e2ee::key_fingerprint(&recipient.pk_enc, &recipient.pk_sig);

    let grants = client.org_get_key_grants(&access_token, org_id).await?;
    // Find OUR grant for this generation (keyed by our server user id).
    let grant = grants
        .grants
        .into_iter()
        .find(|g| g.generation == generation && g.user_id == server_user_id)
        .ok_or_else(|| {
            AppError::Unavailable(format!(
                "no org key grant for generation {generation} — ask the owner to re-share the key"
            ))
        })?;

    // The granter identity: for v1 the OWNER issues grants (the wrapped_key frame carries the granter's
    // pubkeys, and the grant signature is verified against them fail-closed in `open_own_grant`).
    //
    // HONEST V1 BOUNDARY (lock-security 2026-07-10, tracked follow-up): the granter fingerprint is
    // pinned under ITSELF, so this TOFU detects nothing — a granter key substitution just yields a new
    // first-contact pin, never a `Changed` block. That is acceptable ONLY under the documented
    // honest-but-curious relay threat model (the server relays but does not forge). A MALICIOUS server
    // could name a forged granter and inject org items. The hardening (a separate slice, mirroring
    // mode-B) is to pin the granter under the org OWNER's STABLE account_id — resolvable from the
    // member list (role='owner') — and surface a safety-word block on owner key rotation. Until then,
    // org-item authenticity rests on the relay being honest, NOT on this pin.
    let unpacked = crate::e2ee::wrap::unpack_wrapped_key(&grant.wrapped_key, &grant.grant_sig)?;
    let granter_fp =
        crate::e2ee::key_fingerprint(&unpacked.sender_pk_enc, &unpacked.sender_pk_sig);
    match tofu_check(&state.db, &granter_fp, &granter_fp)? {
        TofuState::Changed => {
            return Err(AppError::Auth(
                "the org key granter's identity changed — re-verify before trusting new keys".into(),
            ));
        }
        _ => state.db.pin_contact(
            &granter_fp,
            None,
            &granter_fp,
            &chrono::Utc::now().to_rfc3339(),
        )?,
    }

    let ock = crate::e2ee::org::open_own_grant(
        &grant.wrapped_key,
        &grant.grant_sig,
        &recipient,
        &self_fp,
        &self_fp,
        &granter_fp,
        gen_id,
        &granter_fp,
        &unpacked.sender_pk_sig,
        org_id,
        generation,
    )?;

    // Cache in RAM for the session.
    {
        let mut cache = state
            .org_ock_cache
            .lock()
            .map_err(|_| AppError::Storage("org-ock cache mutex poisoned".into()))?;
        cache.insert((org_id.to_string(), generation), zeroize::Zeroizing::new(*ock));
    }
    Ok(zeroize::Zeroizing::new(*ock))
}

/// `org_create(name)` — create an org (caller becomes owner), then generate + self-grant the OCK so
/// the owner can immediately seal items. Caches the org + generation-1 OCK locally.
#[tauri::command]
pub async fn org_create(state: State<'_, AppState>, name: String) -> Result<OrgStatus, AppError> {
    org_create_inner(state.inner(), name).await
}

pub(crate) async fn org_create_inner(state: &AppState, name: String) -> Result<OrgStatus, AppError> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err(AppError::InvalidArg("org name required".into()));
    }
    let base = share_base_url(state)?;
    let (account_id, gen_id, mk, access_token) = require_session_mk(state).await?;
    // The server DB key for grants (matches `org_members.user_id`) — a UUID, NOT the email `account_id`.
    let server_user_id = session_server_user_id(state)?;
    let client = crate::share::client::ShareClient::new(&base)?;

    let created = client.org_create(&access_token, &name).await?;

    // Owner self-grant: generate the gen-1 OCK, wrap it to OURSELVES, PUT the grant so a second
    // device (or this device after a re-login) can recover the OCK from the server.
    let owner = crate::e2ee::keys::derive_identity(&mk, &account_id, gen_id)?;
    let owner_fp = crate::e2ee::key_fingerprint(&owner.pk_enc, &owner.pk_sig);
    let ock = crate::e2ee::org::generate_ock()?;
    let grant = crate::e2ee::org::wrap_ock_for_member(
        &ock,
        &created.org_id,
        created.current_generation,
        &owner.pk_enc,
        &owner_fp, // recipient_acct_id = our FINGERPRINT (the crypto binding `acquire_org_ock` opens with)
        &owner,
        &owner_fp,
        gen_id,
    )?;
    client
        .org_put_key_grants(
            &access_token,
            &created.org_id,
            vec![crate::share::org_dto::KeyGrantInput {
                // DB key = the server user id (UUID), so the grant links to our `org_members` row.
                user_id: server_user_id.clone(),
                generation: created.current_generation,
                wrapped_key: grant.wrapped_key,
                grant_sig: grant.grant_sig,
            }],
        )
        .await?;

    // Cache the org + OCK locally.
    state.db.upsert_org_state(&crate::storage::OrgState {
        org_id: created.org_id.clone(),
        name: created.name.clone(),
        role: created.role.clone(),
        joined_at: created.created_at.clone(),
        consented: false,
        last_seq: 0,
        generation: created.current_generation,
        context_enabled: true,
    })?;
    {
        let mut cache = state
            .org_ock_cache
            .lock()
            .map_err(|_| AppError::Storage("org-ock cache mutex poisoned".into()))?;
        cache.insert(
            (created.org_id.clone(), created.current_generation),
            zeroize::Zeroizing::new(*ock),
        );
    }
    crate::share::ledger_row(&state.db, &client.host(), "org_create", 0);

    org_status_inner(state).await.map(|o| o.unwrap_or(OrgStatus {
        org_id: created.org_id,
        name: created.name,
        role: created.role,
        member_count: 1,
        consented: false,
        last_seq: 0,
        item_count: 0,
        received_count: 0,
        pending_shares: 0,
        context_enabled: true,
    }))
}

/// `org_status()` — the caller's current org (the FIRST locally-joined org, kept for legacy FE
/// callers that predate the multi-org list), or null. New callers use `org_list_statuses`.
#[tauri::command]
pub async fn org_status(state: State<'_, AppState>) -> Result<Option<OrgStatus>, AppError> {
    org_status_inner(state.inner()).await
}

pub(crate) async fn org_status_inner(state: &AppState) -> Result<Option<OrgStatus>, AppError> {
    let Some(local) = state.db.list_org_states()?.into_iter().next() else {
        return Ok(None);
    };
    Ok(Some(org_status_for(state, local).await?))
}

/// `org_list_statuses()` — a content-free [`OrgStatus`] for EVERY locally-joined org (owned AND
/// invited). Replaces the single-org `org_status` for the multi-org FE. Each org's name/role/
/// generation is refreshed from the server best-effort; a per-org refresh failure falls back to the
/// cached row (never aborts the others). Consent is the GLOBAL org-egress flag for now (per-org
/// consent is a documented follow-up — mirrors `org_status_inner`).
#[tauri::command]
pub async fn org_list_statuses(state: State<'_, AppState>) -> Result<Vec<OrgStatus>, AppError> {
    org_list_statuses_inner(state.inner()).await
}

pub(crate) async fn org_list_statuses_inner(state: &AppState) -> Result<Vec<OrgStatus>, AppError> {
    let mut out = Vec::new();
    for local in state.db.list_org_states()? {
        out.push(org_status_for(state, local).await?);
    }
    Ok(out)
}

/// `org_set_context_enabled(org_id, enabled)` — the PER-INSTANCE org toggle (Settings →
/// Organization): whether a JOINED org contributes content on THIS Murmur install. Membership-checked
/// via [`resolve_org`] (refuses an org the caller isn't a local member of, exactly like every other
/// per-org command); the actual gate lives in `Db::set_org_context_enabled` + the `context_enabled = 1`
/// filter in `search_org_chunks_knn`/`_fts`/`list_org_items_inner` — this command only flips the flag.
/// Disabling NEVER deletes the local replica (re-enabling is instant, no re-sync); NO egress, NO
/// server call — purely local.
#[tauri::command]
pub fn org_set_context_enabled(
    state: State<'_, AppState>,
    org_id: String,
    enabled: bool,
) -> Result<(), AppError> {
    org_set_context_enabled_inner(state.inner(), &org_id, enabled)
}

pub(crate) fn org_set_context_enabled_inner(
    state: &AppState,
    org_id: &str,
    enabled: bool,
) -> Result<(), AppError> {
    resolve_org(state, org_id)?; // membership re-check
    state.db.set_org_context_enabled(org_id, enabled)
}

/// Build the content-free [`OrgStatus`] for ONE locally-joined org. Refreshes name/role/generation +
/// member_count from the server best-effort (offline / a per-org error falls back to the cached row),
/// then reads pending/item counts from the local outbound `org_shares` and the GLOBAL egress consent.
/// The single source of the per-org status body — `org_status_inner` (first) and `org_list_statuses`
/// (all) both go through here so their per-org semantics can never drift.
pub(crate) async fn org_status_for(
    state: &AppState,
    local: crate::storage::OrgState,
) -> Result<OrgStatus, AppError> {
    // Refresh membership/generation from the server when logged in (best-effort — offline shows the
    // cached row).
    let member_count = match (share_base_url(state), valid_access_token(state).await) {
        (Ok(base), Ok(access)) if !base.trim().is_empty() => {
            let client = crate::share::client::ShareClient::new(&base)?;
            match client.org_status(&access, &local.org_id).await {
                Ok(fresh) => {
                    state.db.upsert_org_state(&crate::storage::OrgState {
                        org_id: fresh.org_id.clone(),
                        name: fresh.name.clone(),
                        role: fresh.role.clone(),
                        joined_at: local.joined_at.clone(),
                        consented: local.consented,
                        last_seq: local.last_seq,
                        generation: fresh.current_generation,
                        context_enabled: true,
                    })?;
                    state.db.set_org_generation(&local.org_id, fresh.current_generation)?;
                    client
                        .org_list_members(&access, &local.org_id)
                        .await
                        .map(|m| m.members.len() as u32)
                        .unwrap_or(1)
                }
                Err(_) => 1,
            }
        }
        _ => 1,
    };
    let refreshed = state.db.get_org_state(&local.org_id)?.unwrap_or(local);
    let pending = state
        .db
        .list_org_shares_for_org(&refreshed.org_id)?
        .iter()
        .filter(|s| s.state == "queued" || s.state == "revoke_pending")
        .count() as u32;
    // `item_count` also counts a `failed`-with-`item_id` row: `set_org_share_failed` never clears
    // `item_id` on a republish failure, so such a row's PRIOR publish is still genuinely live on the
    // server — excluding it undercounted "N shared by you" for exactly the rows stuck by the
    // republish-failure bug (see `org_shares_for_source`'s doc for the full state-machine rationale).
    let item_count = state
        .db
        .list_org_shares_for_org(&refreshed.org_id)?
        .iter()
        .filter(|s| s.state == "uploaded" || (s.state == "failed" && s.item_id.is_some()))
        .count() as u32;
    // RECEIVED items in the local replica (what colleagues shared IN) — distinct from item_count
    // (the caller's OWN uploads). Fixes the "0 items" lie a receiving member saw.
    let received_count = state.db.count_org_items(&refreshed.org_id)?;
    // Consent is the GLOBAL org-egress config flag (mirrors share_egress_consented).
    let consented = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?
        .org_egress_consented;
    Ok(OrgStatus {
        org_id: refreshed.org_id,
        name: refreshed.name,
        role: refreshed.role,
        member_count,
        consented,
        last_seq: refreshed.last_seq,
        item_count,
        received_count,
        pending_shares: pending,
        context_enabled: refreshed.context_enabled,
    })
}

/// `org_refresh()` — pull the caller's org MEMBERSHIP from the server and reconcile it into local
/// `org_state` (add invited orgs, drop departed ones). Best-effort; offline / logged-out = no-op. The
/// FE triggers this on settings-open so an org you were just invited to appears without a re-login.
#[tauri::command]
pub async fn org_refresh(state: State<'_, AppState>) -> Result<(), AppError> {
    org_reconcile_memberships(state.inner()).await
}

/// Reconcile the LOCAL `org_state` set against the server's authoritative membership list
/// (`GET /v1/orgs`). Root fix for the single-org / no-membership-discovery bug: an org you were
/// INVITED to (never one you CREATED) was invisible locally and never synced.
///
/// - ADD every server org: `upsert_org_state`. For an org already known locally, the upsert PRESERVES
///   `joined_at`/`consented`/`last_seq` (its ON CONFLICT only refreshes name/role/generation) — so a
///   reconcile never rewinds a cursor or clears consent. A NEW org is inserted with the server's
///   `created_at` as `joined_at`, `consented=false`, `last_seq=0`, and its OCK is best-effort acquired
///   so its feed can later decrypt (a missing grant is NOT fatal — logged + skipped).
/// - REMOVE every local org NOT in the server list (you left / were removed) and PURGE its decrypted
///   replica (`purge_org_replica`, same as `org_leave`) so a departed org's docs don't linger/leak.
///   F2 GUARD: an EMPTY server list while local orgs exist is treated as suspicious (a hostile/buggy/
///   transient `{"orgs":[]}`) and SKIPS all removals — one bad response must never wipe every replica.
///
/// Offline / not-logged-in = NO-OP: the cached rows are kept untouched (never destructive on a
/// transient network failure). No PII in logs — ids/counts only.
pub(crate) async fn org_reconcile_memberships(state: &AppState) -> Result<(), AppError> {
    // Logged out / no server ⇒ keep the cached rows, do nothing (not an error).
    let base = match share_base_url(state) {
        Ok(b) if !b.trim().is_empty() => b,
        _ => return Ok(()),
    };
    let access = match valid_access_token(state).await {
        Ok(a) => a,
        Err(_) => return Ok(()),
    };
    let client = crate::share::client::ShareClient::new(&base)?;
    // A pull failure (network/5xx) is best-effort: keep the cached rows, retry next tick.
    let server_orgs = match client.org_list(&access).await {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!(target: "org", error = %brief_err(&e), "org membership pull failed — keeping cached rows");
            return Ok(());
        }
    };

    // Apply the ADD/REMOVE against the local DB (pure, testable without a network) and learn which
    // orgs are NEW (so we can best-effort acquire their OCK) and which we dropped (to purge OCKs).
    let outcome = reconcile_org_state_into_db(state, &server_orgs)?;

    // Best-effort: acquire each newly-discovered org's OCK so its feed can later decrypt. A grant not
    // yet issued (the owner hasn't PUT our wrapped key) must NOT fail the whole reconcile.
    for (org_id, generation) in &outcome.new_orgs {
        if let Err(e) = acquire_org_ock(state, org_id, *generation).await {
            tracing::info!(
                target: "org",
                error = %brief_err(&e),
                "org OCK not yet available for a newly-discovered org (will retry on sync)"
            );
        }
    }

    tracing::info!(
        target: "org",
        server = server_orgs.len(),
        added = outcome.new_orgs.len(),
        removed = outcome.removed,
        "org membership reconciled"
    );
    Ok(())
}

/// The DB-side outcome of a membership reconcile: the NEW orgs (id + generation, needing an OCK
/// fetch) and how many local orgs were REMOVED.
pub(crate) struct ReconcileOutcome {
    pub(crate) new_orgs: Vec<(String, u32)>,
    pub(crate) removed: u32,
}

/// Apply the server's authoritative org set to the local `org_state` table — the PURE, network-free
/// core of [`org_reconcile_memberships`] (so the add/remove/purge logic is unit-testable without a
/// live server). ADD/refresh every server org (`upsert_org_state` preserves joined_at/consented/
/// last_seq for a KNOWN org via its ON CONFLICT; a NEW org inserts with created_at/consented=false/
/// last_seq=0), and REMOVE + `purge_org_replica` every local org the server no longer lists. Returns
/// the NEW orgs (for OCK acquisition by the caller) + the removed count.
pub(crate) fn reconcile_org_state_into_db(
    state: &AppState,
    server_orgs: &[crate::share::org_dto::OrgSummary],
) -> Result<ReconcileOutcome, AppError> {
    // Snapshot the known-local set BEFORE the upserts so "new org" detection is against the pre-state.
    let known_before: std::collections::HashSet<String> = state
        .db
        .list_org_states()?
        .into_iter()
        .map(|o| o.org_id)
        .collect();
    let server_ids: std::collections::HashSet<String> =
        server_orgs.iter().map(|o| o.org_id.clone()).collect();

    let mut new_orgs = Vec::new();
    for o in server_orgs {
        let is_new = !known_before.contains(&o.org_id);
        state.db.upsert_org_state(&crate::storage::OrgState {
            org_id: o.org_id.clone(),
            name: o.name.clone(),
            role: o.role.clone(),
            joined_at: o.created_at.clone(),
            consented: false,
            last_seq: 0,
            generation: o.current_generation,
            context_enabled: true,
        })?;
        if is_new {
            new_orgs.push((o.org_id.clone(), o.current_generation));
        }
    }

    // F2 HARDENING — an EMPTY server list while we HAVE local orgs is SUSPICIOUS (a buggy/hostile
    // server, or a transient `{"orgs":[]}` 200). Removing every local org on that signal would purge
    // ALL replicas — an unrecoverable local wipe from a single bad response. SKIP the removals (keep
    // the cached rows), log a warning (ids/counts only — no PII), and let the next reconcile with a
    // real non-empty list do the honest cleanup. A NON-empty list that merely OMITS a specific org
    // (the real leave/remove case) still removes that org below — only the all-empty case is guarded.
    if server_orgs.is_empty() && !known_before.is_empty() {
        tracing::warn!(
            target: "org",
            local = known_before.len(),
            "server returned an EMPTY org membership list while local replicas exist — skipping removals (suspected transient/hostile empty response)"
        );
        return Ok(ReconcileOutcome {
            new_orgs,
            removed: 0,
        });
    }

    // Remove local orgs the server no longer lists (left / removed) + purge their decrypted replica so
    // a departed org's docs don't linger in the local retrieval partition (the same purge `org_leave`
    // does — leak/consent invariant) + drop its cached OCKs.
    let mut removed = 0u32;
    for org_id in &known_before {
        if !server_ids.contains(org_id) {
            state.db.delete_org_state(org_id)?;
            state.db.purge_org_replica(org_id)?;
            if let Ok(mut cache) = state.org_ock_cache.lock() {
                cache.retain(|(oid, _), _| oid != org_id);
            }
            removed += 1;
        }
    }

    Ok(ReconcileOutcome { new_orgs, removed })
}

/// Resolve the TARGETED org by id, MEMBERSHIP-CHECKED against the local `org_state`. The multi-org
/// fix: the FE passes the org the user picked, and every per-org command resolves THAT org (never the
/// first via `.next()`, which misrouted a destructive/egress op to org #1 on a multi-org account). A
/// blank id or an org we're not a local member of is an `InvalidArg` refusal — we never operate on an
/// org the caller isn't in.
pub(crate) fn resolve_org(state: &AppState, org_id: &str) -> Result<crate::storage::OrgState, AppError> {
    let org_id = org_id.trim();
    if org_id.is_empty() {
        return Err(AppError::InvalidArg("org id required".into()));
    }
    state
        .db
        .get_org_state(org_id)?
        .ok_or_else(|| AppError::InvalidArg("not a member of that org".into()))
}

/// `org_invite_member(org_id, email)` — owner adds a registered account into the TARGETED org, then
/// wraps that org's CURRENT-generation OCK to them + PUTs the grant so they can decrypt the feed.
/// Requires the OCK session for the targeted org.
#[tauri::command]
pub async fn org_invite_member(
    state: State<'_, AppState>,
    org_id: String,
    email: String,
) -> Result<(), AppError> {
    org_invite_member_inner(state.inner(), org_id, email).await
}

pub(crate) async fn org_invite_member_inner(
    state: &AppState,
    org_id: String,
    email: String,
) -> Result<(), AppError> {
    let email = email.trim().to_string();
    if email.is_empty() {
        return Err(AppError::InvalidArg("email required".into()));
    }
    let org = resolve_org(state, &org_id)?;
    let base = share_base_url(state)?;
    let (account_id, gen_id, mk, access_token) = require_session_mk(state).await?;
    let client = crate::share::client::ShareClient::new(&base)?;

    // Resolve the new member's account id (server-side email lookup).
    let added = client.org_add_member(&access_token, &org.org_id, &email).await?;

    // Look up the member's published identity key to wrap the OCK to them.
    let lookup = client.lookup_key(&access_token, &email).await?;
    let key = lookup
        .key
        .filter(|_| lookup.registered)
        .ok_or_else(|| AppError::InvalidArg("that address is not a registered account".into()))?;

    let generation = org.generation;
    let ock = acquire_org_ock(state, &org.org_id, generation).await?;
    let owner = crate::e2ee::keys::derive_identity(&mk, &account_id, gen_id)?;
    let owner_fp = crate::e2ee::key_fingerprint(&owner.pk_enc, &owner.pk_sig);
    // recipient_acct_id = the member's FINGERPRINT (the crypto binding THEY open with via `self_fp` in
    // `acquire_org_ock`); the DB grant key stays their server user id (`added.user_id`).
    let member_fp = crate::e2ee::key_fingerprint(&key.pk_enc, &key.pk_sig);
    let grant = crate::e2ee::org::wrap_ock_for_member(
        &ock,
        &org.org_id,
        generation,
        &key.pk_enc,
        &member_fp,
        &owner,
        &owner_fp,
        gen_id,
    )?;
    client
        .org_put_key_grants(
            &access_token,
            &org.org_id,
            vec![crate::share::org_dto::KeyGrantInput {
                user_id: added.user_id,
                generation,
                wrapped_key: grant.wrapped_key,
                grant_sig: grant.grant_sig,
            }],
        )
        .await?;
    crate::share::ledger_row(&state.db, &client.host(), "org_invite_member", 0);
    Ok(())
}

/// `org_list_members(org_id)` — the TARGETED org's active members (content-free). Resolves the org the
/// FE picked (membership-checked), never the first via `.next()`.
#[tauri::command]
pub async fn org_list_members(
    state: State<'_, AppState>,
    org_id: String,
) -> Result<Vec<OrgMember>, AppError> {
    let org = resolve_org(state.inner(), &org_id)?;
    let base = share_base_url(state.inner())?;
    let access = valid_access_token(state.inner()).await?;
    let client = crate::share::client::ShareClient::new(&base)?;
    let resp = client.org_list_members(&access, &org.org_id).await?;
    Ok(resp
        .members
        .into_iter()
        .map(|m| OrgMember {
            user_id: m.user_id,
            // The server now discloses member emails to fellow org members (2026-07-14) — the FE shows
            // the email, falling back to the id when an older server omits it (`None`).
            email: m.email,
            role: m.role,
            added_at: m.created_at,
            removed: false,
        })
        .collect())
}

/// `org_remove_member(org_id, user_id)` — owner soft-removes a member from the TARGETED org, then
/// ROTATES that org's OCK: generate gen N+1, wrap it to every REMAINING member, PUT the grants, and
/// bump the server generation. The removed member keeps only the old-gen OCK (can't read anything
/// sealed under N+1). Resolves the FE-picked org (membership-checked), never the first via `.next()`.
#[tauri::command]
pub async fn org_remove_member(
    state: State<'_, AppState>,
    org_id: String,
    user_id: String,
) -> Result<(), AppError> {
    org_remove_member_inner(state.inner(), org_id, user_id).await
}

pub(crate) async fn org_remove_member_inner(
    state: &AppState,
    org_id: String,
    user_id: String,
) -> Result<(), AppError> {
    let user_id = user_id.trim().to_string();
    let org = resolve_org(state, &org_id)?;
    let base = share_base_url(state)?;
    let (account_id, gen_id, mk, access_token) = require_session_mk(state).await?;
    // DB grant key = the owner's server user id (UUID), not the email `account_id`.
    let server_user_id = session_server_user_id(state)?;
    let client = crate::share::client::ShareClient::new(&base)?;

    client
        .org_remove_member(&access_token, &org.org_id, &user_id)
        .await?;

    // Rotate: new generation = current + 1. The owner generates the new OCK and MUST wrap it to every
    // REMAINING active member (keyed by their published identity key) before bumping the generation.
    //
    // HONEST V1 BOUNDARY: the members endpoint is content-min (user_id/role/created_at only) and there
    // is no "fetch a member's identity key by user_id" endpoint yet — only the email-keyed
    // `keys/lookup`. So this v1 rotation re-grants the new OCK to the OWNER (whose key we derive
    // locally) and bumps the generation; the removed member immediately loses access to anything sealed
    // under gen N+1. Re-granting to OTHER remaining members needs a by-user_id key-directory read that
    // the feed-sync slice adds — until then a rotation should be followed by re-inviting the remaining
    // members (which re-wraps the current-gen OCK to each). This is a documented scope cut, not a leak:
    // no content is exposed and the removed member is correctly locked out of future items.
    let new_gen = org.generation.saturating_add(1);
    let new_ock = crate::e2ee::org::generate_ock()?;
    let owner = crate::e2ee::keys::derive_identity(&mk, &account_id, gen_id)?;
    let owner_fp = crate::e2ee::key_fingerprint(&owner.pk_enc, &owner.pk_sig);

    let owner_grant = crate::e2ee::org::wrap_ock_for_member(
        &new_ock,
        &org.org_id,
        new_gen,
        &owner.pk_enc,
        &owner_fp, // recipient_acct_id = our FINGERPRINT (matches `acquire_org_ock`'s open)
        &owner,
        &owner_fp,
        gen_id,
    )?;
    client
        .org_put_key_grants(
            &access_token,
            &org.org_id,
            vec![crate::share::org_dto::KeyGrantInput {
                user_id: server_user_id.clone(),
                generation: new_gen,
                wrapped_key: owner_grant.wrapped_key,
                grant_sig: owner_grant.grant_sig,
            }],
        )
        .await?;
    // Bump the server generation (monotonic +1) — the server checks grant counts only.
    client.org_bump_generation(&access_token, &org.org_id).await?;

    // Update the cached generation + OCK.
    state.db.set_org_generation(&org.org_id, new_gen)?;
    {
        let mut cache = state
            .org_ock_cache
            .lock()
            .map_err(|_| AppError::Storage("org-ock cache mutex poisoned".into()))?;
        cache.insert((org.org_id.clone(), new_gen), zeroize::Zeroizing::new(*new_ock));
    }
    crate::share::ledger_row(&state.db, &client.host(), "org_remove_member", 0);
    Ok(())
}

/// `org_leave(org_id)` — the caller leaves the TARGETED org (member self-removal). Drops that org's
/// local row + cached OCKs + decrypted replica. Does NOT retroactively un-share the caller's
/// already-published items (use `revoke_org_share` for that first if desired). Resolves the FE-picked
/// org (membership-checked), never the first via `.next()` — a leave must purge the RIGHT org.
#[tauri::command]
pub async fn org_leave(state: State<'_, AppState>, org_id: String) -> Result<(), AppError> {
    let org = resolve_org(state.inner(), &org_id)?;
    let base = share_base_url(state.inner())?;
    let access = valid_access_token(state.inner()).await?;
    let client = crate::share::client::ShareClient::new(&base)?;
    client.org_leave(&access, &org.org_id).await?;
    state.inner().db.delete_org_state(&org.org_id)?;
    // LEAVE = full consent withdrawal: PURGE the decrypted org replica (items/chunks/vectors/FTS) so
    // a departed member keeps NO searchable copy of colleagues' shared content. Without this the
    // plaintext replica lingered forever and `org_search` / the `org_brain_search` tool would still
    // return it (leak/consent invariant). Belt-and-braces beside the `org_brain_available` gate on
    // the retrieval seam (a purged replica is empty either way).
    state.inner().db.purge_org_replica(&org.org_id)?;
    // Drop every cached OCK for this org.
    {
        let mut cache = state
            .inner()
            .org_ock_cache
            .lock()
            .map_err(|_| AppError::Storage("org-ock cache mutex poisoned".into()))?;
        cache.retain(|(oid, _), _| oid != &org.org_id);
    }
    crate::share::ledger_row(&state.inner().db, &client.host(), "org_leave", 0);
    Ok(())
}

/// `consent_to_org_egress` — grant the one-time ORG-egress consent. Fail-closed: until set, every
/// `share_meeting_to_org` / `share_document_to_org` refuses. Mirror of `consent_to_share_egress`.
#[tauri::command]
pub fn consent_to_org_egress(state: State<'_, AppState>) -> Result<(), AppError> {
    let mut cfg = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
    cfg.grant_org_egress_consent(&state.db)?;
    Ok(())
}

/// `revoke_org_egress` — revoke the org-egress consent (the next org share is refused fail-closed).
#[tauri::command]
pub fn revoke_org_egress(state: State<'_, AppState>) -> Result<(), AppError> {
    let mut cfg = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
    cfg.revoke_org_egress(&state.db)?;
    Ok(())
}

/// Build the exact outgoing markdown for a meeting/note org share: the GATED read → `clean_note_body`
/// → optional regex scrub. Returns `(title, clean_scrubbed_markdown, created_at, counts, kind)`. The
/// read-gate is the FIRST thing this does (a sealed-not-unlocked source refuses). NO egress.
pub(crate) fn build_org_share_body(
    state: &AppState,
    meeting_id: Option<&str>,
    document_id: Option<&str>,
    scrub: bool,
) -> Result<(String, String, String, OrgScrubCounts, crate::share::org_envelope::OrgItemKind), AppError>
{
    let (title, markdown, created_at, kind) = match (meeting_id, document_id) {
        (Some(mid), None) => {
            // (1) READ-GATE FIRST — a sealed-not-unlocked meeting refuses before any read/egress.
            if !meeting_is_unlocked(state, mid)? {
                return Err(AppError::Locked(
                    "this meeting's folder is locked — unlock it to share to the org".into(),
                ));
            }
            let note = state
                .db
                .get_latest_note_for_meeting(mid)?
                .ok_or_else(|| AppError::InvalidArg(format!("no note for meeting {mid}")))?;
            let meeting = state.db.get_meeting(mid)?;
            let title = meeting
                .as_ref()
                .and_then(|m| m.title.clone())
                .unwrap_or_else(|| "Shared note".to_string());
            let created_at = meeting
                .as_ref()
                .map(|m| m.started_at.clone())
                .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
            (
                title,
                note.markdown,
                created_at,
                crate::share::org_envelope::OrgItemKind::Note,
            )
        }
        (None, Some(did)) => {
            // (1) READ-GATE FIRST — a sealed authored note refuses (mirrors `share_note_to_link_doc`).
            let row = state
                .db
                .get_note_row(did)?
                .ok_or_else(|| AppError::InvalidArg(format!("no note {did}")))?;
            if !folder_is_unlocked(state, &row.folder_id)? {
                return Err(AppError::Locked(
                    "this note's folder is locked — unlock it to share to the org".into(),
                ));
            }
            let title = note_display_title(&row);
            let created_at = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(row.created_at)
                .unwrap_or_else(chrono::Utc::now)
                .to_rfc3339();
            (
                title,
                row.text,
                created_at,
                crate::share::org_envelope::OrgItemKind::Note,
            )
        }
        _ => {
            return Err(AppError::InvalidArg(
                "exactly one of meeting_id or document_id is required".into(),
            ));
        }
    };

    // (3) CLEAN (strip frontmatter + flatten wikilinks + drop obsidian:// refs — the leak-safe transform).
    let cleaned = crate::share::envelope::clean_note_body(&markdown);
    // (4) regex PII scrub (emails/phones/cards; names KEPT) when requested.
    let (final_md, counts) = if scrub {
        scrub_org_markdown(&cleaned)
    } else {
        (cleaned, OrgScrubCounts::default())
    };
    // (5) REFUSE AN EMPTY SHARE — root-cause fix for the "blank card" bug: a meeting with no
    // generated note yet, or a note that is frontmatter-only, cleans down to "" (see
    // `strip_frontmatter`'s `frontmatter_only_document_yields_empty_body`). Publishing that anyway
    // produces an envelope with full header metadata (title/created_at/rev) but zero content — a
    // silent, confusing blank share for the recipient. Refuse loudly instead of ever
    // sealing/publishing an empty body; do NOT fall back to the transcript (a distinct, separate
    // feature decision, not this fix's scope).
    if final_md.trim().is_empty() {
        return Err(AppError::InvalidArg(if meeting_id.is_some() {
            "this meeting doesn't have a generated note yet — generate one before sharing".into()
        } else {
            "this note has no content to share — add some content before sharing".into()
        }));
    }
    Ok((title, final_md, created_at, counts, kind))
}

/// `preview_org_share(meeting_id?, document_id?, scrub)` — the EXACT post-clean, post-scrub markdown +
/// byte count + scrub counts, with NO egress. The read-gate still applies (a sealed source refuses).
#[tauri::command]
pub fn preview_org_share(
    state: State<'_, AppState>,
    meeting_id: Option<String>,
    document_id: Option<String>,
    scrub: bool,
) -> Result<OrgSharePreview, AppError> {
    preview_org_share_inner(state.inner(), meeting_id, document_id, scrub)
}

pub(crate) fn preview_org_share_inner(
    state: &AppState,
    meeting_id: Option<String>,
    document_id: Option<String>,
    scrub: bool,
) -> Result<OrgSharePreview, AppError> {
    let (title, markdown, _created, counts, _kind) = build_org_share_body(
        state,
        meeting_id.as_deref(),
        document_id.as_deref(),
        scrub,
    )?;
    let bytes = markdown.len() as u32;
    let chunk_count = rough_chunk_count(&markdown);
    Ok(OrgSharePreview {
        title,
        markdown,
        bytes,
        chunk_count,
        scrubbed: counts,
        scrub,
    })
}

/// `share_meeting_to_org(org_id, meeting_id, scrub)` — the normative org share flow (spec gate order):
/// (1) read-gate, (2) consent fail-closed, (3) clean, (4) scrub, (5) seal under OCK + local
/// open-verify, (6) upload blob + publish item, (7) content-free egress-ledger entry. `org_id` is the
/// FE-picked target (membership-checked via `resolve_org`) — the multi-org fix for the `.next()`
/// misroute that shared into the FIRST org, not the chosen one.
#[tauri::command]
pub async fn share_meeting_to_org(
    app: AppHandle,
    state: State<'_, AppState>,
    org_id: String,
    meeting_id: String,
    scrub: bool,
) -> Result<OrgShareEntry, AppError> {
    let entry = share_to_org_inner(state.inner(), &org_id, Some(meeting_id), None, scrub).await?;
    // A successful share now lives in the local replica (`publish_org_body` upserts it) AND the server
    // feed — ping every open org view (Notes list + Settings shared-brain) to re-fetch immediately, so
    // the shared note appears without a manual "Sync now". Content-free count-only event; a best-effort
    // emit never affects the share result.
    crate::events::emit_org_feed_updated(&app, 1);
    Ok(entry)
}

/// `share_document_to_org(org_id, document_id, scrub)` — the note twin of [`share_meeting_to_org`].
#[tauri::command]
pub async fn share_document_to_org(
    app: AppHandle,
    state: State<'_, AppState>,
    org_id: String,
    document_id: String,
    scrub: bool,
) -> Result<OrgShareEntry, AppError> {
    let entry = share_to_org_inner(state.inner(), &org_id, None, Some(document_id), scrub).await?;
    // See `share_meeting_to_org`: ping open org views so the freshly-shared note appears without a
    // manual "Sync now". Content-free; best-effort.
    crate::events::emit_org_feed_updated(&app, 1);
    Ok(entry)
}

pub(crate) async fn share_to_org_inner(
    state: &AppState,
    org_id: &str,
    meeting_id: Option<String>,
    document_id: Option<String>,
    scrub: bool,
) -> Result<OrgShareEntry, AppError> {
    // IDEMPOTENCY (the double-click / re-click DUPLICATE fix). A user-initiated share of a source that
    // is ALREADY live in this org must NOT mint a second feed item. The pre-fix hole: `publish_org_body`
    // → `find_reusable_org_share` only reuses `queued`/`failed` rows, so a second click AFTER the first
    // upload succeeded found no reusable row, inserted a fresh one, and published a DISTINCT item →
    // the note appeared twice. Here we resolve the org (membership-checked) and look for an existing
    // `uploaded` share of this exact (org, source): if one exists we COLLAPSE any accidental extras
    // (keep the earliest, tombstone the rest — the user opted into auto-clean) and RETURN the survivor
    // WITHOUT publishing. This is a READ + a dedup of the caller's OWN shares, NOT fresh content egress,
    // so it runs BEFORE the org-egress consent gate — re-clicking an already-shared note never needs
    // re-consent. Freshness on EDIT is `republish_org_shares_for_source`'s job, never this button's.
    let org = resolve_org(state, org_id)?;
    if let Some(keeper) = collapse_org_share_dups_for_source(
        state,
        &org.org_id,
        meeting_id.as_deref(),
        document_id.as_deref(),
    )
    .await?
    {
        return Ok(OrgShareEntry {
            item_id: keeper.item_id,
            kind: keeper.kind,
            title: keeper.title,
            shared_at: keeper.created_at,
            rev: keeper.rev,
            state: keeper.state,
        });
    }

    // Not yet live in this org → the normal first share = rev 1. A re-publish-on-edit supersede bumps
    // the rev (see `republish_org_shares_for_source`, which calls `publish_org_body` with `old_rev + 1`).
    publish_org_body(state, org_id, meeting_id, document_id, scrub, 1).await
}

/// Collapse accidental DUPLICATE live (`uploaded`) org items for ONE (org, source) down to the earliest,
/// tombstoning the rest, and return the SURVIVOR (the earliest `uploaded` row) — or `None` when the
/// source is not currently live in this org. The keeper = the oldest published item (the identity other
/// members first synced); every later duplicate is revoked via `revoke_org_share_inner` (tombstone the
/// server item + mark the local row revoked). BEST-EFFORT on each tombstone: a network failure leaves
/// the extra `revoke_pending` (the launch sweep finishes it) and simply isn't cleaned this pass — it
/// NEVER fails the idempotent share. No content is read or egressed here (only the caller's own share
/// rows + a tombstone of a redundant copy), so it is safe to run before the egress-consent gate.
async fn collapse_org_share_dups_for_source(
    state: &AppState,
    org_id: &str,
    meeting_id: Option<&str>,
    document_id: Option<&str>,
) -> Result<Option<crate::storage::OrgShareRow>, AppError> {
    // Oldest-first, so `remove(0)` is the canonical keeper and the remainder are the extras.
    let mut rows =
        state
            .db
            .uploaded_org_shares_for_source_in_org(org_id, meeting_id, document_id)?;
    if rows.is_empty() {
        return Ok(None);
    }
    let keeper = rows.remove(0);
    for extra in rows {
        if let Some(item_id) = extra.item_id.clone() {
            // Tombstone the redundant copy. Swallow errors — `revoke_org_share_inner` marks the row
            // `revoke_pending` first, so an interrupted tombstone is completed by the launch sweep.
            let _ = revoke_org_share_inner(state, item_id).await;
        }
    }
    // Also cancel any NOT-yet-uploaded sibling (`queued`/`failed`) for this (org, source): the source is
    // already live (the keeper), so a pending row is redundant and would otherwise linger as a stuck
    // "pending" share that the launch sweep re-attempts every start. Local-only — these have no server
    // item to tombstone. Best-effort (never fails the idempotent return).
    let now = chrono::Utc::now().to_rfc3339();
    let _ = state
        .db
        .cancel_pending_org_shares_for_source_in_org(org_id, meeting_id, document_id, &now);
    Ok(Some(keeper))
}

/// TERMINAL `org_shares.last_error` reason (Brain v3 org push size pre-check): the sealed
/// ciphertext exceeds the server's per-item blob cap
/// (`murmur_protocol::caps::MAX_ORG_ITEM_BLOB_BYTES`, 1 MiB). The launch sweep NEVER retries a row
/// failed with this reason — retrying cannot shrink the content, so requeueing it every start was a
/// poison loop (the server 413s forever). Recovery is content-driven: a manual re-share
/// (`share_to_org_inner` reuses + re-arms the row) or an edit-save republish re-reads the trimmed
/// source and clears the reason on success.
pub(crate) const ORG_SHARE_ERR_TOO_LARGE: &str = "too_large";

/// The org publish CORE, shared by the FIRST share (`share_to_org_inner`, `rev = 1`) and the
/// re-publish-on-edit supersede (`republish_org_shares_for_source`, `rev = old_rev + 1`). It owns the
/// FULL gate chain so a republish INHERITS it rather than re-implementing (the leak-safety single
/// seam): (1) read-gate + (3) clean + (4) scrub via `build_org_share_body`, (2) org-egress consent
/// fail-closed, (5) OCK seal + LOCAL open-verify-before-publish, (6) blob upload + publish item,
/// (7) content-free egress ledger. `rev` is stamped into BOTH the `OrgEnvelope` (source_rev) and the
/// `PublishItemRequest` so members see the supersede.
pub(crate) async fn publish_org_body(
    state: &AppState,
    org_id: &str,
    meeting_id: Option<String>,
    document_id: Option<String>,
    scrub: bool,
    rev: u32,
) -> Result<OrgShareEntry, AppError> {
    // (1) READ-GATE + (3) clean + (4) scrub — all inside `build_org_share_body` (read-gate FIRST).
    let (title, markdown, created_at, _counts, kind) = build_org_share_body(
        state,
        meeting_id.as_deref(),
        document_id.as_deref(),
        scrub,
    )?;
    // `build_org_share_body` already enforces exactly one of meeting_id/document_id is `Some` (else it
    // errors before this line is reached), so this mirrors that same exclusivity to stamp the wire
    // envelope's SOURCE type (document vs meeting — a new axis, distinct from `kind`/content-shape).
    let source_kind = if meeting_id.is_some() {
        crate::share::org_envelope::OrgSourceKind::Meeting
    } else {
        crate::share::org_envelope::OrgSourceKind::Document
    };

    // (2) consent fail-closed (the global one-time org-egress consent).
    {
        let cfg = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
        if !cfg.org_egress_consented {
            return Err(AppError::Unavailable(
                "org sharing not consented — confirm the one-time upload notice first".into(),
            ));
        }
    }

    // MULTI-ORG: share into the FE-PICKED org (membership-checked), never the first via `.next()`.
    // This is a destructive EGRESS op — misrouting it to org #1 published a member's note into the
    // WRONG org (the root of the "B shared into Siema but it went to org #1" bug). `resolve_org`
    // refuses a blank id / an org the caller isn't a local member of.
    let org = resolve_org(state, org_id)?;
    let base = share_base_url(state)?;
    let (_account_id, _gen_id, _mk, access_token) = require_session_mk(state).await?;
    let client = crate::share::client::ShareClient::new(&base)?;
    let generation = org.generation;

    // Author hint = the account's email local-part (a display label, never note content).
    let author_hint = {
        let g = state
            .account_session
            .lock()
            .map_err(|_| AppError::Storage("account-session mutex poisoned".into()))?;
        crate::share::require_login(&g)
            .map(|s| org_author_hint(&s.email))
            .unwrap_or_else(|_| "member".to_string())
    };

    // Persist a queued row FIRST (so a crash between seal + publish is recoverable by the launch
    // sweep).
    let now = chrono::Utc::now().to_rfc3339();
    let env = crate::share::org_envelope::OrgEnvelope::new(
        kind,
        title.clone(),
        markdown,
        author_hint,
        created_at,
        rev,
        source_kind,
    );
    let content_sha = env.content_sha256();
    // SB-3 row-amplification fix: REUSE any existing retriable (`queued`/`failed`) row for this
    // logical share key (org + meeting-or-document) instead of minting a fresh row on every sweep
    // attempt. Pre-fix each retry inserted a NEW row while the old survived → unbounded row growth +
    // a duplicate publish on eventual recovery. On reuse we re-arm the SAME row (state → queued,
    // item_id/last_error cleared, per-attempt fields refreshed); a later success flips THAT one row to
    // uploaded. Only a truly-new share (no reusable row) inserts.
    let row_id = match state.db.find_reusable_org_share(
        &org.org_id,
        meeting_id.as_deref(),
        document_id.as_deref(),
    )? {
        Some(existing) => {
            state.db.reset_org_share_for_retry(
                &existing.id,
                Some(&title),
                rev,
                generation,
                &content_sha,
                &now,
            )?;
            existing.id
        }
        None => {
            let row_id = crate::share::new_share_id();
            state.db.insert_org_share(
                &row_id,
                &org.org_id,
                meeting_id.as_deref(),
                document_id.as_deref(),
                kind.as_str(),
                Some(&title),
                rev,
                generation,
                &content_sha,
                &now,
            )?;
            row_id
        }
    };

    // (5) Seal under the OCK + LOCAL OPEN-VERIFY (the egress verify-before-destroy — publish only a
    // blob we just proved we can decrypt back).
    //
    // AAD ITEM NONCE = hex(content_sha256_of_plaintext), NOT the local row_id: the server assigns its
    // OWN item_id on publish (the client never controls it), so a per-publish LOCAL id would be
    // unknowable to any OTHER member syncing the feed → they could never open the cell. The content
    // hash is deterministic + rides the feed (`OrgItemEntry.content_sha256`), so every member
    // reconstructs the SAME AAD. (2026-07-10 cross-slice fix — see org_sync_now's open side.)
    let ock = acquire_org_ock(state, &org.org_id, generation).await?;
    let item_nonce = org_item_nonce(&content_sha);
    let (ciphertext, _sha) =
        match crate::share::org_envelope::seal_org_envelope(&ock, &env, &org.org_id, &item_nonce) {
            Ok(v) => v,
            Err(e) => {
                state.db.set_org_share_failed(&row_id, "seal_failed", &now)?;
                return Err(e);
            }
        };

    // SIZE PRE-CHECK (Brain v3): the server hard-caps an org item blob at
    // `MAX_ORG_ITEM_BLOB_BYTES` — an oversized ciphertext would 413 on EVERY attempt, and the
    // launch sweep would requeue it forever (the poison loop). Fail CLIENT-SIDE, before any
    // egress, with the TERMINAL `too_large` reason the sweep excludes from retry. Sizes only —
    // never content.
    if ciphertext.len() > murmur_protocol::caps::MAX_ORG_ITEM_BLOB_BYTES {
        state
            .db
            .set_org_share_failed(&row_id, ORG_SHARE_ERR_TOO_LARGE, &now)?;
        return Err(AppError::InvalidArg(format!(
            "this item is too large to share ({} bytes sealed; the org limit is {} bytes) — shorten it and share again",
            ciphertext.len(),
            murmur_protocol::caps::MAX_ORG_ITEM_BLOB_BYTES
        )));
    }

    // (6) upload the ciphertext blob → publish the item. On failure, mark the row `failed` (the
    // launch sweep retries a `queued`/`failed` row later).
    let blob_id = match client.put_blob(&access_token, ciphertext).await {
        Ok(id) => id,
        Err(e) => {
            state.db.set_org_share_failed(&row_id, "upload_failed", &now)?;
            return Err(e);
        }
    };
    let published = match client
        .org_publish_item(
            &access_token,
            &org.org_id,
            crate::share::org_dto::PublishItemRequest {
                blob_id,
                content_sha256: content_sha.clone(),
                rev,
                generation,
            },
        )
        .await
    {
        Ok(p) => p,
        Err(e) => {
            state.db.set_org_share_failed(&row_id, "publish_failed", &now)?;
            return Err(e);
        }
    };
    state
        .db
        .set_org_share_uploaded(&row_id, &published.item_id, &now)?;

    // LOCAL REPLICA CONSISTENCY (owner-live-refresh): the author's OWN `org_items` replica would
    // otherwise stay EMPTY of this item until the next feed pull — so a note the owner just shared is
    // invisible in `list_org_items` (the Notes org view + Settings browse) until a manual "Sync now".
    // Upsert it LOCALLY now, mirroring the republish path, so `list_org_items` immediately resolves it
    // as an owned/editable card. FTS-only (`None` embedder) to keep this share path light; the next real
    // `org_sync_now` re-ingests authoritatively (idempotent upsert) and re-embeds. Best-effort: a local
    // replica error must never fail the share (the server copy is already live + correct).
    //
    // AUTHOR (root-cause fix, 2026-07-15): the CALLER of a share IS the author, so stamp the
    // current session's own server user id directly at upsert time — this row is born correct and
    // never depends on a later feed-ingest/backfill to learn who wrote it. Best-effort: an
    // unresolvable session id (`.ok()` → `None`) just leaves the column for the backfill, exactly
    // as before this fix; it never blocks the local-replica upsert itself.
    let my_author_id = session_server_user_id(state).ok();
    if let Err(e) = state.db.upsert_org_item(
        &published.item_id,
        &org.org_id,
        published.seq,
        &env.author_hint,
        &env.title,
        &env.markdown,
        &env.created_at,
        rev,
        generation,
        &content_sha,
        env.source_kind.map(crate::share::org_envelope::OrgSourceKind::as_str),
        my_author_id.as_deref(),
        None,
    ) {
        tracing::warn!(target: "org", error = %e, "share: local replica upsert failed (server copy live)");
    }

    // (7) CONTENT-FREE egress ledger (host + ciphertext byte size). NEVER a title / note text / OCK.
    crate::share::ledger_row(
        &state.db,
        &client.host(),
        "org_share_publish",
        content_sha.len(),
    );

    Ok(OrgShareEntry {
        item_id: Some(published.item_id),
        kind: kind.as_str().to_string(),
        title: Some(title),
        shared_at: now,
        rev,
        state: "uploaded".to_string(),
    })
}

/// RE-PUBLISH-ON-EDIT (the stale-org-copy fix). When the author edits a note/meeting they have
/// already shared into one or more orgs, the org copy would otherwise stay FROZEN at share time (the
/// blob is written ONCE by the initial share; nothing re-publishes; the feed has no in-place update —
/// `org_publish_item` mints a NEW server item_id every call and members key on item_id). The only
/// correct supersede is TOMBSTONE-OLD + PUBLISH-NEW-REV, done for EVERY org this source was shared to.
///
/// BEST-EFFORT: called AFTER a local write already succeeded. It NEVER fails the save — a network /
/// transient failure marks the row `failed` so the existing launch sweep re-publishes it later (the
/// intent is not lost), and the whole helper swallows its own error at the call site (`let _ = …`).
///
/// Per uploaded row (across ALL orgs — a note may be shared to several):
///  - Re-read the CURRENT plaintext THROUGH the read-gate (`build_org_share_body`). If it now refuses
///    (`Locked` — the folder/meeting got locked since the share) → SKIP (never tombstone-into-nothing;
///    leave the org copy as-is). Same skip if org-egress consent was withdrawn.
///  - Short-circuit: if the fresh body's `content_sha256` == the row's stored hash → NO egress, skip.
///  - Seal a fresh OCK envelope at the item's current generation with local open-verify (the egress
///    verify-before-destroy) → `put_blob` → `org_publish_item(rev = old_rev + 1)` → REPOINT THE SAME
///    ROW to the new item_id (`set_org_share_uploaded`, bumping rev + hash — no row amplification) →
///    THEN tombstone the OLD item so members evict the stale copy. ORDER IS LOAD-BEARING: publish-new
///    BEFORE tombstone-old (a crash between = a transient dup, recoverable; the reverse risks a window
///    with NO org copy). Both are ledgered content-free.
///  - SCRUB INTENT: `org_shares` does not persist the original scrub flag, so republish defaults scrub
///    ON (fail-safe toward LESS egress — matches the launch sweep's documented default). An edit must
///    never silently DOWNGRADE scrubbing.
pub(crate) async fn republish_org_shares_for_source(
    state: &AppState,
    meeting_id: Option<&str>,
    document_id: Option<&str>,
) -> Result<u32, AppError> {
    // DEDUP FIRST (auto-clean, user-opted-in): collapse any accidental duplicate live items for this
    // source — PER ORG — down to the earliest BEFORE republishing. Otherwise we'd republish (and keep
    // alive) every duplicate; the survivor is what the edit then supersedes. Best-effort — a tombstone
    // failure leaves the extra for the launch sweep's dedup pass and never blocks the save.
    let dup_orgs: Vec<String> = {
        let mut v: Vec<String> = state
            .db
            .org_shares_for_source(meeting_id, document_id)?
            .into_iter()
            .map(|r| r.org_id)
            .collect();
        v.sort();
        v.dedup();
        v
    };
    for org_id in &dup_orgs {
        let _ =
            collapse_org_share_dups_for_source(state, org_id, meeting_id, document_id).await;
    }

    let rows = state.db.org_shares_for_source(meeting_id, document_id)?;
    if rows.is_empty() {
        return Ok(0);
    }
    // Count of rows that produced a NEW published rev this call — returned so the caller can emit
    // `org-feed-updated` ONLY when > 0 (a save that changed nothing / skipped every row must not ping
    // the FE).
    let mut republished = 0u32;
    let now = chrono::Utc::now().to_rfc3339();
    for row in rows {
        // Re-read the CURRENT plaintext THROUGH the read-gate. `build_org_share_body` also does the
        // clean+scrub (scrub ON by default — fail-safe toward LESS egress). A `Locked` refusal (the
        // source got locked since the share) → SKIP without tombstone.
        let (title, markdown, created_at, _counts, kind) = match build_org_share_body(
            state,
            row.meeting_id.as_deref(),
            row.document_id.as_deref(),
            true,
        ) {
            Ok(v) => v,
            Err(AppError::Locked(_)) => {
                tracing::info!(
                    target: "org",
                    org_id = %row.org_id,
                    "republish skipped: source is locked (org copy left as-is)"
                );
                continue;
            }
            Err(e) => {
                tracing::warn!(target: "org", error = %e, "republish: could not re-read source; skipped");
                continue;
            }
        };
        // `org_shares_for_source` rows always anchor exactly one of meeting_id/document_id (mirrors the
        // exclusivity `build_org_share_body` already enforced when this row was first published).
        let source_kind = if row.meeting_id.is_some() {
            crate::share::org_envelope::OrgSourceKind::Meeting
        } else {
            crate::share::org_envelope::OrgSourceKind::Document
        };

        // Consent could have been withdrawn since the share → SKIP (never egress without consent).
        {
            let consented = state
                .config
                .lock()
                .map(|c| c.org_egress_consented)
                .unwrap_or(false);
            if !consented {
                tracing::info!(target: "org", org_id = %row.org_id, "republish skipped: org egress consent withdrawn");
                continue;
            }
        }

        // Author hint = the account's email local-part (a display label, never note content).
        let author_hint = {
            let g = match state.account_session.lock() {
                Ok(g) => g,
                Err(_) => continue,
            };
            crate::share::require_login(&g)
                .map(|s| org_author_hint(&s.email))
                .unwrap_or_else(|_| "member".to_string())
        };

        // Short-circuit: unchanged content ⇒ NO new item, NO egress. `content_sha256` folds
        // `source_rev` into the canonical bytes, so the stored hash (computed at `row.rev`) must be
        // compared against a hash at the SAME rev — else every republish would look "changed" purely
        // because the rev bumped. Build the comparison envelope at `row.rev`; only the PUBLISH envelope
        // uses `new_rev`.
        let cmp_env = crate::share::org_envelope::OrgEnvelope::new(
            kind,
            title.clone(),
            markdown.clone(),
            author_hint.clone(),
            created_at.clone(),
            row.rev,
            source_kind,
        );
        if row.content_sha256.as_deref() == Some(cmp_env.content_sha256().as_slice()) {
            continue;
        }

        let new_rev = row.rev.saturating_add(1);
        let env = crate::share::org_envelope::OrgEnvelope::new(
            kind,
            title,
            markdown,
            author_hint,
            created_at,
            new_rev,
            source_kind,
        );
        let content_sha = env.content_sha256();

        // Resolve the org + a live session (best-effort: a resolve/session failure just skips this row;
        // the note is already saved locally). `generation` = the item's CURRENT live generation.
        let org = match resolve_org(state, &row.org_id) {
            Ok(o) => o,
            Err(_) => continue,
        };
        let generation = org.generation;
        let base = match share_base_url(state) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let access_token = match require_session_mk(state).await {
            Ok((_, _, _, t)) => t,
            Err(_) => {
                // No live session → leave the row uploaded (stale); a later save with a session
                // republishes. NOT a failure (never blocks the save).
                continue;
            }
        };
        let client = match crate::share::client::ShareClient::new(&base) {
            Ok(c) => c,
            Err(_) => continue,
        };

        // (5) Seal under the OCK + LOCAL OPEN-VERIFY (egress verify-before-destroy). AAD nonce =
        // hex(content_sha256) — deterministic + rides the feed so every member reconstructs it.
        let ock = match acquire_org_ock(state, &org.org_id, generation).await {
            Ok(k) => k,
            Err(_) => {
                state.db.set_org_share_failed(&row.id, "republish_ock_failed", &now)?;
                continue;
            }
        };
        let item_nonce = org_item_nonce(&content_sha);
        let ciphertext =
            match crate::share::org_envelope::seal_org_envelope(&ock, &env, &org.org_id, &item_nonce) {
                Ok((ct, _)) => ct,
                Err(_) => {
                    state.db.set_org_share_failed(&row.id, "republish_seal_failed", &now)?;
                    continue;
                }
            };

        // SIZE PRE-CHECK (Brain v3): oversized ciphertext would 413 forever — mark the row with
        // the TERMINAL `too_large` reason (excluded from the launch sweep) instead of egressing.
        // The OLD item stays live on the server (never tombstone-into-nothing); a later edit-save
        // that shrinks the source re-enters via `org_shares_for_source` and heals the row.
        if ciphertext.len() > murmur_protocol::caps::MAX_ORG_ITEM_BLOB_BYTES {
            state
                .db
                .set_org_share_failed(&row.id, ORG_SHARE_ERR_TOO_LARGE, &now)?;
            tracing::warn!(
                target: "org",
                sealed_bytes = ciphertext.len(),
                cap_bytes = murmur_protocol::caps::MAX_ORG_ITEM_BLOB_BYTES,
                "republish skipped: sealed item exceeds the org blob cap (terminal too_large)"
            );
            continue;
        }

        // (6) upload → publish the NEW rev. On failure, mark the row `failed` (the launch sweep retries)
        // but do NOT tombstone (the OLD copy stays live — never leave the org with no copy).
        let blob_id = match client.put_blob(&access_token, ciphertext).await {
            Ok(id) => id,
            Err(_) => {
                state.db.set_org_share_failed(&row.id, "republish_upload_failed", &now)?;
                continue;
            }
        };
        let published = match client
            .org_publish_item(
                &access_token,
                &org.org_id,
                crate::share::org_dto::PublishItemRequest {
                    blob_id,
                    content_sha256: content_sha.clone(),
                    rev: new_rev,
                    generation,
                },
            )
            .await
        {
            Ok(p) => p,
            Err(_) => {
                state.db.set_org_share_failed(&row.id, "republish_publish_failed", &now)?;
                continue;
            }
        };

        // REPOINT THE SAME ROW to the new item (new item_id + bumped rev + fresh hash) — no new row.
        state.db.reset_org_share_for_retry(
            &row.id,
            row.title.as_deref(),
            new_rev,
            generation,
            &content_sha,
            &now,
        )?;
        state
            .db
            .set_org_share_uploaded(&row.id, &published.item_id, &now)?;
        crate::share::ledger_row(&state.db, &client.host(), "org_share_publish", content_sha.len());
        // This row produced a NEW published rev this call — counted so the caller can decide whether
        // to emit `org-feed-updated` (only when > 0; a save that changed nothing / skipped every row
        // must not ping the FE).
        republished += 1;

        // LOCAL REPLICA CONSISTENCY (F-org-editable): the author's OWN `org_items` replica would
        // otherwise stay frozen on the OLD item_id until the next feed pull — so the just-repointed
        // `org_shares` row no longer matches the replica the Notes list renders (`item_id` drift →
        // the card falls back to a stale, read-only viewer). Upsert the NEW item + tombstone the OLD
        // one LOCALLY now, so `list_org_items` immediately resolves this as an owned/editable card with
        // the fresh title. FTS-only (`None` embedder) to keep this editor-close path light; the next
        // real `org_sync_now` re-ingests authoritatively (idempotent upsert) and re-embeds. Best-effort:
        // a local-replica error must never fail the save (the server copy is already live + correct).
        //
        // AUTHOR (root-cause fix, 2026-07-15): this row's caller is the SAME session that just
        // successfully republished it (`require_session_mk` above already proved a live session),
        // so stamp its own server user id directly — the repointed row is correct from the moment
        // it's written, never dependent on a later backfill.
        let my_author_id = session_server_user_id(state).ok();
        if let Err(e) = state.db.upsert_org_item(
            &published.item_id,
            &org.org_id,
            published.seq,
            &env.author_hint,
            &env.title,
            &env.markdown,
            &env.created_at,
            new_rev,
            generation,
            &content_sha,
            env.source_kind.map(crate::share::org_envelope::OrgSourceKind::as_str),
            my_author_id.as_deref(),
            None,
        ) {
            tracing::warn!(target: "org", error = %e, "republish: local replica upsert failed (server copy live)");
        }
        if let Some(old_item) = row.item_id.as_deref() {
            // Guard: only tombstone the OLD id if it truly differs from the freshly-published one —
            // a defensive no-op if the server ever reused the item_id (it mints a new one per publish),
            // so we never tombstone the replica we just upserted.
            if old_item != published.item_id {
                if let Err(e) = state.db.tombstone_org_item(old_item) {
                    tracing::warn!(target: "org", error = %e, "republish: local old-item tombstone failed");
                }
            }
        }

        // THEN tombstone the OLD item so members evict the stale copy. Publish-BEFORE-tombstone: a
        // crash here leaves a transient dup (recoverable), never a window with no org copy. A tombstone
        // failure is non-fatal — the new copy is already live; the stale one lingers until a revoke.
        if let Some(old_item) = row.item_id.as_deref() {
            match client.org_tombstone_item(&access_token, &org.org_id, old_item).await {
                Ok(()) => {
                    crate::share::ledger_row(&state.db, &client.host(), "org_share_revoke", 0);
                }
                Err(e) => {
                    tracing::warn!(
                        target: "org",
                        error = %e,
                        org_id = %org.org_id,
                        "republish: superseded item published but old-item tombstone failed (transient dup)"
                    );
                }
            }
        }
        // This row published a NEW rev this call — count it so the caller emits `org-feed-updated`.
        republished += 1;
    }
    Ok(republished)
}

/// `list_org_shares()` — the caller's outbound org shares (local rows; titles render only to the
/// local owner). Content-free enough for the FE list.
#[tauri::command]
pub fn list_org_shares(state: State<'_, AppState>) -> Result<Vec<OrgShareEntry>, AppError> {
    // TODO(multi-org): let the user pick the target org — the FE does NOT yet pass an org_id here, so
    // this read still lists the FIRST local org's shares. A READ (not destructive/egress), so a
    // first-org default is acceptable until the FE gains an org picker; thread `org_id` through then.
    let Some(org) = state.inner().db.list_org_states()?.into_iter().next() else {
        return Ok(Vec::new());
    };
    let rows = state.inner().db.list_org_shares_for_org(&org.org_id)?;
    Ok(rows
        .into_iter()
        .map(|r| OrgShareEntry {
            item_id: r.item_id,
            kind: r.kind,
            title: r.title,
            shared_at: r.created_at,
            rev: r.rev,
            state: r.state,
        })
        .collect())
}

/// One org this meeting is actively shared into (`meeting_org_shares`) — just enough for the
/// Library row badge + the Detail "Shared with…" indicator. Content-free beyond the org's own
/// display name (which the caller already sees everywhere else in the org UI).
#[derive(serde::Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct MeetingOrgShareInfo {
    pub org_id: String,
    pub org_name: String,
}

/// `meeting_org_shares(meeting_id)` — every org THIS meeting is currently (actively) shared into,
/// i.e. the `uploaded` `org_shares` rows anchored on it. Same disclosure class as `get_meeting_tags`
/// (a metadata-only read of the user's OWN share state — never note/transcript content), but gated
/// exactly like `get_meeting_detail`: a sealed-and-not-session-unlocked meeting returns an EMPTY
/// list rather than leaking whether/where it's shared, mirroring `meeting_is_unlocked` at every
/// other content read site. A meeting the caller never shared (or an unknown id) also returns `[]`.
#[tauri::command]
pub fn meeting_org_shares(
    state: State<'_, AppState>,
    meeting_id: String,
) -> Result<Vec<MeetingOrgShareInfo>, AppError> {
    meeting_org_shares_inner(state.inner(), &meeting_id)
}

/// Inner of [`meeting_org_shares`] taking `&AppState` (unit-testable read-gate). See the command doc
/// for the disclosure-class rationale.
pub(crate) fn meeting_org_shares_inner(
    st: &AppState,
    meeting_id: &str,
) -> Result<Vec<MeetingOrgShareInfo>, AppError> {
    if !meeting_is_unlocked(st, meeting_id)? {
        return Ok(Vec::new());
    }
    let rows = st.db.org_shares_for_source(Some(meeting_id), None)?;
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for row in rows {
        if !seen.insert(row.org_id.clone()) {
            continue; // one badge per org even if somehow >1 uploaded row exists for it.
        }
        let name = st
            .db
            .get_org_state(&row.org_id)?
            .map(|o| o.name)
            .unwrap_or_else(|| "Shared brain".to_string());
        out.push(MeetingOrgShareInfo {
            org_id: row.org_id,
            org_name: name,
        });
    }
    Ok(out)
}

/// One row of [`list_meeting_org_shares`] — a single meeting-to-org share pairing (a meeting may
/// have several rows, one per org it's shared into).
#[derive(serde::Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct MeetingOrgShareRow {
    pub meeting_id: String,
    pub org_id: String,
    pub org_name: String,
}

/// `list_meeting_org_shares()` — EVERY active meeting→org share pairing across ALL of the caller's
/// meetings, in one bulk call (avoids an N+1 `meeting_org_shares` fetch per Library row). Same
/// disclosure class + gate as [`meeting_org_shares`]: a sealed-and-not-session-unlocked meeting's
/// rows are dropped, exactly mirroring `mask_locked_meetings` for `list_meetings`.
///
/// STUCK-REPUBLISH FIX: uses `Db::list_live_org_shares` (not a hardcoded `state = 'uploaded'`
/// filter) so a row whose most recent republish attempt failed transiently but still carries a
/// live server `item_id` (`set_org_share_failed` never clears it) keeps showing the badge — the
/// same "live" definition already applied by `org_shares_for_source` / `OrgStatus.item_count` /
/// `org_resolve_source_inner`. Before this fix, that exact reachable state (edit → republish →
/// transient network blip) made the Library badge vanish while the note-share-panel's own CTA
/// (which reads `org_shares_for_source`) still showed the item as shared — two surfaces disagreed.
#[tauri::command]
pub fn list_meeting_org_shares(
    state: State<'_, AppState>,
) -> Result<Vec<MeetingOrgShareRow>, AppError> {
    list_meeting_org_shares_inner(state.inner())
}

/// Inner of [`list_meeting_org_shares`] taking `&AppState` (unit-testable read-gate).
pub(crate) fn list_meeting_org_shares_inner(
    st: &AppState,
) -> Result<Vec<MeetingOrgShareRow>, AppError> {
    let rows = st.db.list_live_org_shares()?;
    let mut out = Vec::new();
    let mut org_names: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut seen = std::collections::HashSet::new();
    for row in rows {
        let Some(meeting_id) = row.meeting_id else {
            continue; // a document-anchored share — not a Library concern.
        };
        if !seen.insert((meeting_id.clone(), row.org_id.clone())) {
            continue; // one badge per (meeting, org) even if somehow >1 uploaded row exists.
        }
        // GATE: a sealed-and-not-session-unlocked meeting's share status must not leak, exactly
        // like every other content/metadata read on it (`meeting_is_unlocked`).
        if !meeting_is_unlocked(st, &meeting_id)? {
            continue;
        }
        let name = if let Some(n) = org_names.get(&row.org_id) {
            n.clone()
        } else {
            let n = st
                .db
                .get_org_state(&row.org_id)?
                .map(|o| o.name)
                .unwrap_or_else(|| "Shared brain".to_string());
            org_names.insert(row.org_id.clone(), n.clone());
            n
        };
        out.push(MeetingOrgShareRow {
            meeting_id,
            org_id: row.org_id,
            org_name: name,
        });
    }
    Ok(out)
}

/// `org_live_shares_for_source(meeting_id?, document_id?)` — which orgs already hold a LIVE (`uploaded`)
/// share of THIS exact local source, so the FE can mark those orgs "Already added ✓" and BLOCK a
/// re-share (the double-click duplicate fix). READ-ONLY, no egress. Content-free: only (org_id, item_id,
/// rev) — never a title or another member's data. Exactly one of meeting_id/document_id is set; both-None
/// ⇒ empty. Reuses `org_shares_for_source` (uploaded rows across ALL the caller's orgs).
#[tauri::command]
pub fn org_live_shares_for_source(
    state: State<'_, AppState>,
    meeting_id: Option<String>,
    document_id: Option<String>,
) -> Result<Vec<OrgSourceShareStatus>, AppError> {
    let rows = state
        .inner()
        .db
        .org_shares_for_source(meeting_id.as_deref(), document_id.as_deref())?;
    Ok(rows
        .into_iter()
        .map(|r| OrgSourceShareStatus {
            org_id: r.org_id,
            item_id: r.item_id,
            rev: r.rev,
        })
        .collect())
}

/// `revoke_org_share(item_id)` — tombstone a published org item (destroys its server ciphertext) and
/// mark the local row revoked. Marks `revoke_pending` first (crash-safe: the launch sweep completes a
/// `revoke_pending` if the tombstone call didn't land).
#[tauri::command]
pub async fn revoke_org_share(state: State<'_, AppState>, item_id: String) -> Result<(), AppError> {
    revoke_org_share_inner(state.inner(), item_id).await
}

pub(crate) async fn revoke_org_share_inner(state: &AppState, item_id: String) -> Result<(), AppError> {
    let item_id = item_id.trim().to_string();
    let Some(row) = state.db.org_share_by_item(&item_id)? else {
        return Err(AppError::InvalidArg("no local org share for that item".into()));
    };
    let now = chrono::Utc::now().to_rfc3339();
    state.db.set_org_share_state(&row.id, "revoke_pending", &now)?;

    let base = share_base_url(state)?;
    let access = valid_access_token(state).await?;
    let client = crate::share::client::ShareClient::new(&base)?;
    client
        .org_tombstone_item(&access, &row.org_id, &item_id)
        .await?;
    state.db.set_org_share_state(&row.id, "revoked", &now)?;
    crate::share::ledger_row(&state.db, &client.host(), "org_share_revoke", 0);
    Ok(())
}

/// `org_resolve_source(item_id)` — resolve an org item back to the LOCAL editable source (note or
/// meeting) IF the caller authored it. Only the author's device holds the `org_shares` row for the
/// item, so a `Some(...)` result means the caller can edit the underlying note/meeting; a member
/// reading a colleague's shared item gets `None`. READ-ONLY (a local row lookup; no egress, no
/// content). The FE routes its own shares from the org-item viewer into the real editor.
#[tauri::command]
pub fn org_resolve_source(
    state: State<'_, AppState>,
    item_id: String,
) -> Result<Option<OrgSourceRef>, AppError> {
    org_resolve_source_inner(state.inner(), &item_id)
}

pub(crate) fn org_resolve_source_inner(
    state: &AppState,
    item_id: &str,
) -> Result<Option<OrgSourceRef>, AppError> {
    let item_id = item_id.trim();
    let Some(row) = state.db.org_share_by_item(item_id)? else {
        return Ok(None);
    };
    // Only a `revoked` row is refused here — it was INTENTIONALLY torn down, so it must not redirect
    // the author back into "editing" it. Every other state (including `failed` with a still-set
    // `item_id` — a republish that failed transiently but whose PRIOR publish is still genuinely live
    // on the server, see `org_shares_for_source`) falls through: the row's `document_id`/`meeting_id`
    // (checked below) is what actually decides "nothing to route to", so a stuck-but-live row still
    // resolves to the real editable source instead of the read-only Org Brain viewer.
    if row.state == "revoked" {
        return Ok(None);
    }
    if let Some(document_id) = row.document_id {
        return Ok(Some(OrgSourceRef {
            kind: "document".to_string(),
            source_id: document_id,
        }));
    }
    if let Some(meeting_id) = row.meeting_id {
        return Ok(Some(OrgSourceRef {
            kind: "meeting".to_string(),
            source_id: meeting_id,
        }));
    }
    Ok(None)
}

/// One org-brain item still live for a folder (lock×shares dialog). `item_id` is the server item id
/// for an uploaded share, or the local row id for a still-queued one (a stable key for the FE list;
/// only uploaded ones are deep-linkable). Serializes camelCase → matches `models.ts` `OrgActiveShare`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrgActiveShare {
    pub item_id: String,
    pub title: String,
}

/// The active-shares report for the lock×shares dialog. `links`/`users` are counts of the folder's
/// live zero-knowledge LINK / Murmur↔Murmur USER shares; `org` lists the org-brain items shared from
/// the folder. Content-free enough for a dialog (titles render only to the local owner). Mirrors the
/// TS `ActiveSharesReport`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveSharesReport {
    pub links: u32,
    pub users: u32,
    pub org: Vec<OrgActiveShare>,
}

/// `folder_active_shares(folder_id)` — the folder's live outgoing shares, for the lock×shares dialog.
/// Closes the pre-existing hole where `lock_folder` sealed a folder without ever warning that its
/// notes were still shared (live 1:1 link/user shares were completely invisible; only org shares were
/// tracked). READ-ONLY, no egress: link/user COUNTS + the org items' ids+titles.
#[tauri::command]
pub fn folder_active_shares(
    state: State<'_, AppState>,
    folder_id: String,
) -> Result<ActiveSharesReport, AppError> {
    folder_active_shares_inner(state.inner(), &folder_id)
}

/// Inner of [`folder_active_shares`] taking `&AppState` (unit-testable gate).
pub(crate) fn folder_active_shares_inner(
    st: &AppState,
    folder_id: &str,
) -> Result<ActiveSharesReport, AppError> {
    // GATE (2026-07-11 audit): a sealed-and-not-session-unlocked folder surfaces NOTHING here — org
    // shares carry plaintext `title`s (stored un-sealed), which this command returned ungated for ANY
    // folder, leaking a locked folder's note titles. Mirror the `folder_is_unlocked` read gate: a
    // locked-not-unlocked folder returns an empty report (the FE shows the unlock affordance, never
    // the share list). An open / session-unlocked folder proceeds normally.
    if !folder_is_unlocked(st, folder_id)? {
        return Ok(ActiveSharesReport {
            links: 0,
            users: 0,
            org: Vec::new(),
        });
    }
    let (mut links, mut users) = (0u32, 0u32);
    for (_share_id, mode) in st.db.active_link_user_shares_for_folder(folder_id)? {
        match mode.as_str() {
            "link" => links += 1,
            "user" => users += 1,
            _ => {}
        }
    }
    let org = st
        .db
        .active_org_share_ids_for_folder(folder_id)?
        .into_iter()
        .map(|(row_id, item_id, title)| OrgActiveShare {
            item_id: item_id.unwrap_or(row_id),
            title,
        })
        .collect();
    Ok(ActiveSharesReport { links, users, org })
}

/// `revoke_shares_for_folder(folder_id)` — bulk-revoke every live share from a folder (the "Revoke &
/// lock" path of the lock×shares dialog). Best-effort: attempts EVERY share even if one fails, then
/// returns the FIRST error, so the FE never proceeds to seal on a partial revoke. Link/user → server
/// revoke + local `revoked`; org uploaded → server tombstone; org still-queued → cancelled locally so
/// the launch sweep never egresses it. Idempotent (an already-revoked share is simply re-listed out).
#[tauri::command]
pub async fn revoke_shares_for_folder(
    state: State<'_, AppState>,
    folder_id: String,
) -> Result<(), AppError> {
    let st = state.inner();
    let link_user = st.db.active_link_user_shares_for_folder(&folder_id)?;
    let org = st.db.active_org_share_ids_for_folder(&folder_id)?;
    let mut first_err: Option<AppError> = None;

    for (share_id, _mode) in link_user {
        if let Err(e) = revoke_share_inner(st, share_id).await {
            first_err.get_or_insert(e);
        }
    }
    for (row_id, item_id, _title) in org {
        let res = match item_id {
            Some(id) => revoke_org_share_inner(st, id).await,
            None => {
                // Never uploaded — cancel locally so the launch sweep skips it (no server item).
                let now = chrono::Utc::now().to_rfc3339();
                st.db.set_org_share_state(&row_id, "revoked", &now)
            }
        };
        if let Err(e) = res {
            first_err.get_or_insert(e);
        }
    }
    match first_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// DELETE-CASCADE FIX (2026-07-15): bulk-revoke every LIVE org share of ONE exact source
/// (`meeting_id` XOR `document_id`), across ALL orgs it was shared into. This is the delete-time twin
/// of [`revoke_shares_for_folder`] (which is folder-scoped, for the lock×shares dialog) — here the
/// scope is a single note/meeting/document that is about to be permanently, locally deleted.
///
/// ROOT CAUSE this closes: `delete_note_inner`/`delete_meeting_inner`/`delete_document_inner` used to
/// hard-delete the LOCAL rows (vault `.md`, WAV/`.enc`, DB rows) without ever touching `org_shares` —
/// so a shared note/meeting's server ciphertext stayed `uploaded` and the 60s background org-sync tick
/// (`org_background_sync_tick`) re-pulled the author's OWN still-live item back into the local
/// `org_items` replica, resurrecting "deleted" content in Shared Brain / Ask / MCP on the SAME machine.
///
/// Reuses [`revoke_org_share_inner`]'s server-tombstone + local-`revoked` flip (never duplicated) —
/// mirrors the `revoke_shares_for_folder` org loop exactly: an uploaded row is tombstoned on the
/// server; a still-`queued`/never-uploaded row (no live server item) is simply cancelled LOCALLY so
/// the launch sweep never egresses it after the source is gone.
///
/// BEST-EFFORT-BUT-LOUD: attempts EVERY live share even if one fails (never abandon a revoke sweep
/// partway), then returns the FIRST error to the caller so a revoke failure (e.g. offline) is
/// surfaced — the caller (delete) fails loud rather than silently deleting local content while a
/// dangling live share survives on the server. Idempotent: an already-revoked row is excluded by
/// `org_shares_for_source` (only `uploaded`/stuck-`failed` rows are live).
pub(crate) async fn revoke_org_shares_for_source(
    state: &AppState,
    meeting_id: Option<&str>,
    document_id: Option<&str>,
) -> Result<(), AppError> {
    let rows = state.db.org_shares_for_source(meeting_id, document_id)?;
    let mut first_err: Option<AppError> = None;
    for row in rows {
        let res = match row.item_id {
            Some(item_id) => revoke_org_share_inner(state, item_id).await,
            None => {
                // Never uploaded (a queued/stuck row with no live server item) — cancel locally so
                // the launch sweep never tries to publish it for a source that no longer exists.
                let now = chrono::Utc::now().to_rfc3339();
                state.db.set_org_share_state(&row.id, "revoked", &now)
            }
        };
        if let Err(e) = res {
            tracing::warn!(
                target: "org",
                org_id = %row.org_id,
                error = %brief_err(&e),
                "delete-cascade: failed to revoke a live org share of the deleted source"
            );
            first_err.get_or_insert(e);
        }
    }
    match first_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Cadence of the background org-feed sync loop (M6 Shared Brain, spawned in `lib.rs` setup). The
/// first tick fires `ORG_SYNC_FIRST_DELAY_SECS` after launch (no startup contention); subsequent
/// ticks every `ORG_SYNC_TICK_SECS`. Every tick is a cheap no-op while logged out / no org joined.
/// 60s (not 120s) so a member — who has no server push, only this pull — sees a colleague's newly
/// shared/edited note within ~1 min without a manual "Sync now"; the owner's own shares refresh
/// instantly via the `org-feed-updated` emit on the share/edit commands.
pub const ORG_SYNC_FIRST_DELAY_SECS: u64 = 20;
pub const ORG_SYNC_TICK_SECS: u64 = 60;

/// One background org-sync tick: drain the outbound share queue, then pull + ingest the inbound feed
/// into the local int8 partition. This is what makes the org brain a REPLICATED brain — every
/// member's app stays fresh for Ask/MCP WITHOUT anyone opening Settings. Best-effort: each half
/// warns-and-continues (a transient failure never kills the loop), and both inners gate to an early
/// `Ok` when logged out / no org joined, so this is a no-op until a session is live. Logs only
/// non-PII counts on a productive tick.
///
/// Returns `true` when the inbound sync actually CHANGED the local replica this tick (≥1 ingest or
/// tombstone) — the caller (`lib.rs` loop) uses this to fire a content-free
/// [`crate::events::EVENT_ORG_FEED_UPDATED`] so an open FE view re-fetches WITHOUT polling. Returns
/// `false` on a no-op / error tick. Emitting is done by the loop (which holds the `AppHandle`); the
/// tick signature stays `&AppState`, unchanged for the other internal callers.
pub(crate) async fn org_background_sync_tick(state: &AppState) -> bool {
    // Reconcile membership FIRST so a newly-invited org is present (and synced this same tick) and a
    // departed org is dropped before we pull its feed. Best-effort — a failure never blocks the sync.
    if let Err(e) = org_reconcile_memberships(state).await {
        tracing::warn!(target: "org", error = %brief_err(&e), "org membership reconcile tick failed");
    }
    if let Err(e) = org_sweep_pending_inner(state).await {
        tracing::warn!(target: "org", error = %e, "org outbound sweep tick failed");
    }
    match org_sync_now_inner(state).await {
        Ok(r) if r.ingested > 0 || r.tombstoned > 0 => {
            tracing::info!(
                target: "org",
                ingested = r.ingested,
                tombstoned = r.tombstoned,
                "org feed synced"
            );
            true
        }
        Ok(_) => false,
        Err(e) => {
            tracing::warn!(target: "org", error = %e, "org feed sync tick failed");
            false
        }
    }
}

/// `org_sweep_pending()` — the on-launch org queue sweep (extends the mode-B `share_rewrap_pending`
/// launch pattern). Idempotent + OFFLINE-TOLERANT: logged out / no server / a per-row failure leaves
/// the row where it is for the next pass (never an error). Three queues:
///   - `queued`, or a `failed` row that NEVER published (`item_id` still `NULL`) → fresh publish via
///     `share_to_org_inner` (re-seal under the current OCK + upload + publish → `uploaded`);
///   - a `failed` row that WAS live before (`item_id` set — the row's LAST republish attempt failed,
///     but `set_org_share_failed` never clears the OLD, still-server-live `item_id`) → retried via
///     `republish_org_shares_for_source` instead, so it supersedes (bumps `rev`, tombstones the old
///     item after the new one lands) rather than restarting at `rev = 1` and minting a duplicate item
///     alongside the still-live stuck one;
///   - `revoke_pending` → tombstone the item server-side → `revoked`.
/// Returns the number of rows ADVANCED. Reads ONLY the retained local rows' key/hash context — for a
/// re-seal it re-reads the SOURCE note, so the per-row re-share still passes the READ-GATE (a source
/// sealed since queueing refuses and the row is left `failed`, never egressing sealed content).
#[tauri::command]
pub async fn org_sweep_pending(state: State<'_, AppState>) -> Result<u32, AppError> {
    org_sweep_pending_inner(state.inner()).await
}

pub(crate) async fn org_sweep_pending_inner(state: &AppState) -> Result<u32, AppError> {
    let base = share_base_url(state)?;
    if base.trim().is_empty() {
        return Ok(0);
    }
    // Logged out ⇒ nothing to do (best-effort launch sweep, not an error).
    if valid_access_token(state).await.is_err() {
        return Ok(0);
    }
    let mut advanced = 0u32;

    // 0) DEDUP (auto-clean, user-opted-in): collapse accidental DUPLICATE live items — same org + same
    //    source — down to the earliest, tombstoning the extras. Fixes duplicates created BEFORE the
    //    idempotency guard existed (e.g. a double-click on Share), which self-healing needs a proactive
    //    pass to reach (the FE now blocks re-share, so the share-time collapse never re-fires for them).
    //    `duplicate_uploaded_org_shares` returns exactly the extras (never a keeper); each is torn down
    //    via the crash-safe revoke path (marks `revoke_pending` first, so an interrupted tombstone is
    //    completed by step 1 on the next pass). Best-effort — a network failure just retries next launch.
    for extra in state.db.duplicate_uploaded_org_shares()? {
        if let Some(item_id) = extra.item_id.clone() {
            if revoke_org_share_inner(state, item_id).await.is_ok() {
                advanced += 1;
            }
        }
    }

    // 1) Finish any pending revokes (a tombstone that didn't land before a crash).
    for row in state.db.list_org_shares_in_state("revoke_pending")? {
        let Some(item_id) = row.item_id.clone() else {
            // No server item id ⇒ nothing to tombstone; just mark it revoked locally.
            let now = chrono::Utc::now().to_rfc3339();
            let _ = state.db.set_org_share_state(&row.id, "revoked", &now);
            advanced += 1;
            continue;
        };
        if revoke_org_share_inner(state, item_id).await.is_ok() {
            advanced += 1;
        }
    }

    // 2) Re-attempt any queued/failed publishes. Re-run the full gated share so a source sealed since
    //    queueing NEVER egresses (the read-gate refuses → the row stays `failed`).
    for state_label in ["queued", "failed"] {
        for row in state.db.list_org_shares_in_state(state_label)? {
            // Brain v3 size pre-check: `too_large` is TERMINAL for the sweep — retrying cannot
            // shrink the content, so requeueing it every launch is exactly the poison loop the
            // client-side cap check exists to kill. Recovery is content-driven (a manual re-share /
            // an edit-save republish re-reads the possibly-trimmed source and re-arms the row).
            if row.last_error.as_deref() == Some(ORG_SHARE_ERR_TOO_LARGE) {
                continue;
            }
            if row.item_id.is_some() {
                // Was live before: this row's LAST attempt was a REPUBLISH (not the initial publish)
                // and it failed — `set_org_share_failed` deliberately retains the OLD, still-server-live
                // `item_id` (only the success path's `reset_org_share_for_retry` clears it). The correct
                // retry here is `republish_org_shares_for_source` (bumps `rev`, tombstones the OLD item
                // only AFTER the new one lands) — `share_to_org_inner` would wrongly restart at `rev = 1`
                // and mint a genuine DUPLICATE item since the old one is still live on the server.
                // `org_shares_for_source` (the enumerator it reads through) now surfaces exactly this
                // shape (`failed` + non-null `item_id`), so this retry can actually find the row.
                let advanced_this_source = republish_org_shares_for_source(
                    state,
                    row.meeting_id.as_deref(),
                    row.document_id.as_deref(),
                )
                .await
                .map(|n| n > 0)
                .unwrap_or(false);
                if advanced_this_source {
                    advanced += 1;
                }
                continue;
            }
            let res = share_to_org_inner(
                state,
                // Re-publish targets the SAME org the row was queued under (never the first via
                // `.next()`) — a multi-org account's sweep must re-share into the right org.
                &row.org_id,
                row.meeting_id.clone(),
                row.document_id.clone(),
                // A re-publish preserves the ORIGINAL scrub intent: if we can't know it, scrub ON
                // (fail-safe toward LESS egress). The queued row doesn't record the flag, so default
                // to scrubbing.
                true,
            )
            .await;
            if res.is_ok() {
                // SB-3: `share_to_org_inner` REUSED this same row (dedup on the logical key) and
                // flipped it to `uploaded` on success — so there is NO stale row to revoke here (the
                // pre-fix code minted a fresh row per attempt and revoked the old one, which is
                // exactly the amplification we removed). Just count the advance.
                advanced += 1;
            }
        }
    }

    Ok(advanced)
}

/// `org_sync_now(org_id?)` — pull the org feed from the last synced cursor, OPEN each ciphertext blob
/// with the (RAM-cached / grant-unwrapped) OCK, and INGEST it into the local decrypted replica + int8
/// retrieval partition. A TOMBSTONE evicts the item's chunks/vectors/FTS. Returns a content-free
/// [`OrgSyncReport`] (counts + `fts_only` + per-item error strings). Best-effort per item: a single
/// item whose OCK is unavailable / whose blob won't open is SKIPPED (recorded in `errors`), never
/// crashing the whole sync — the cursor still advances past a tombstone but STOPS at the first
/// un-openable LIVE item so a transient key gap is retried next sync (no silent skip-forward).
///
/// `org_id`: `Some(id)` syncs ONLY that (FE-picked, membership-checked) org; `None` syncs ALL joined
/// orgs (the background tick / internal callers). The tick's all-orgs iteration is unchanged.
#[tauri::command]
pub async fn org_sync_now(
    state: State<'_, AppState>,
    org_id: Option<String>,
) -> Result<crate::storage::models::OrgSyncReport, AppError> {
    // The FE passes a SPECIFIC org id (→ sync only that org); the background tick / internal callers
    // pass `None` (→ sync ALL orgs, the tick's iteration is unchanged). This is the command-boundary
    // dispatch of the multi-org fix: a user-triggered "Sync now" from a picked org must not sync (or
    // report against) the wrong org.
    match org_id {
        Some(id) => org_sync_one_now_inner(state.inner(), &id).await,
        None => org_sync_now_inner(state.inner()).await,
    }
}

/// Server feed page size per `org_feed` request. The loop pages until the feed is drained.
const ORG_FEED_PAGE: u32 = 200;

/// Sync exactly ONE (FE-targeted) org's feed — the single-org boundary of [`org_sync_now`]. Resolves
/// the org (membership-checked, never `.next()`), then runs the same per-org pull/ingest via
/// `org_sync_one` used by the all-orgs loop. Offline / logged-out ⇒ an empty report (no-op), matching
/// the all-orgs path.
pub(crate) async fn org_sync_one_now_inner(
    state: &AppState,
    org_id: &str,
) -> Result<crate::storage::models::OrgSyncReport, AppError> {
    let mut report = crate::storage::models::OrgSyncReport::default();
    let org = resolve_org(state, org_id)?;
    let base = share_base_url(state)?;
    if base.trim().is_empty() {
        return Ok(report);
    }
    let access = match valid_access_token(state).await {
        Ok(a) => a,
        Err(_) => return Ok(report),
    };
    let client = crate::share::client::ShareClient::new(&base)?;
    report.fts_only = !crate::embed::embed_model_present();
    org_sync_one(state, &client, &access, &org, &mut report).await?;
    tracing::info!(
        target: "org",
        pulled = report.pulled,
        ingested = report.ingested,
        tombstoned = report.tombstoned,
        fts_only = report.fts_only,
        errors = report.errors.len(),
        "org feed sync (single org)"
    );
    Ok(report)
}

pub(crate) async fn org_sync_now_inner(
    state: &AppState,
) -> Result<crate::storage::models::OrgSyncReport, AppError> {
    let mut report = crate::storage::models::OrgSyncReport::default();

    // No org joined ⇒ nothing to sync (not an error).
    let orgs = state.db.list_org_states()?;
    if orgs.is_empty() {
        return Ok(report);
    }
    let base = share_base_url(state)?;
    if base.trim().is_empty() {
        return Ok(report);
    }
    // Logged out ⇒ best-effort no-op (the FE surfaces "sign in to sync").
    let access = match valid_access_token(state).await {
        Ok(a) => a,
        Err(_) => return Ok(report),
    };
    let client = crate::share::client::ShareClient::new(&base)?;

    // Whether THIS member has a real embedder. StubEmbedder ⇒ FTS-only (no int8 vectors written);
    // flag it so the FE can offer a re-embed once a model lands.
    report.fts_only = !crate::embed::embed_model_present();

    // MULTI-ORG: sync EVERY locally-joined org (each with its own cursor via `org_last_seq_for`). A
    // per-org failure must NOT abort the others — it's recorded in `report.errors` and the loop
    // continues; `report.last_seq` reflects the LAST org synced (the field is per-org, but the counts
    // aggregate). This is the fix for the single-org `.next()` that hid every invited org's feed.
    for org in orgs {
        if let Err(e) = org_sync_one(state, &client, &access, &org, &mut report).await {
            report
                .errors
                .push(format!("org sync failed ({})", brief_err(&e)));
        }
    }

    tracing::info!(
        target: "org",
        pulled = report.pulled,
        ingested = report.ingested,
        tombstoned = report.tombstoned,
        fts_only = report.fts_only,
        errors = report.errors.len(),
        "org feed sync (multi-org)"
    );
    Ok(report)
}

/// Sync ONE org's append-only feed from its own cursor into the local decrypted replica + retrieval
/// partition. Extracted from `org_sync_now_inner` so the multi-org loop can call it per org while the
/// per-org pull/ingest logic stays byte-identical. Aggregates counts into the shared `report`.
async fn org_sync_one(
    state: &AppState,
    client: &crate::share::client::ShareClient,
    access: &str,
    org: &crate::storage::OrgState,
    report: &mut crate::storage::models::OrgSyncReport,
) -> Result<(), AppError> {
    // ── ASYNC PULL PHASE — drain the feed, opening each cell; buffer decrypted items ──────────────
    // The embedder (`dyn Embedder`, !Send) is deliberately NOT constructed here: the whole async
    // section is Send-safe, and the INGEST phase below owns the embedder entirely INSIDE its
    // `perf::run_heavy` blocking closure (never held across an `.await`). A tombstone applies
    // immediately (no key/blob needed).
    //
    // FIX D — per-item failures are classified TRANSIENT vs TERMINAL so one poison item never stalls
    // the WHOLE feed forever (pre-fix: EVERY failure did `break 'pages`, so a single un-openable cell
    // blocked all newer items indefinitely):
    //   • TRANSIENT (OCK unavailable / blob fetch network error) → `break 'pages`, cursor NOT advanced:
    //     the same item is retried next sync (correct — it may succeed once the key/network recovers).
    //   • TERMINAL (missing blob / missing hash / envelope open failed / content-hash mismatch —
    //     permanent for THAT cell) → record the error, `SkipTerminal` (advance the cursor PAST this
    //     seq), and `continue` to the next item. It will NEVER ingest, so blocking newer items on it is
    //     pure stall. The cursor only advances past a terminal item that appears BEFORE the first
    //     transient stop (the loop processes items in seq order and breaks on the first transient), so
    //     we never skip forward past a transient un-ingested item.
    enum FeedAction {
        Tombstone {
            item_id: String,
            seq: u64,
        },
        /// A permanently un-ingestable item: advance the cursor past its seq (no DB write) so it never
        /// stalls the feed. Recorded in `report.errors`.
        SkipTerminal {
            seq: u64,
        },
        Ingest {
            item_id: String,
            seq: u64,
            rev: u32,
            generation: u32,
            env: crate::share::org_envelope::OrgEnvelope,
            sha: Vec<u8>,
            /// The author's server account id, off the feed entry — stored on the local replica so the
            /// author's OTHER machines can recognise + edit their own item (2026-07-14).
            author_user_id: String,
        },
    }
    let mut actions: Vec<FeedAction> = Vec::new();
    let mut cursor = state.db.org_last_seq_for(&org.org_id)?;

    'pages: loop {
        let feed = client
            .org_feed(access, &org.org_id, cursor, ORG_FEED_PAGE)
            .await?;
        if feed.items.is_empty() {
            break;
        }
        for item in &feed.items {
            report.pulled += 1;

            if item.tombstoned {
                actions.push(FeedAction::Tombstone {
                    item_id: item.item_id.clone(),
                    seq: item.seq,
                });
                continue;
            }

            let Some(blob_id) = item.blob_id.clone() else {
                // TERMINAL: a live entry with no blob is structurally broken — it can never open. Skip
                // past it (advance) rather than stall every newer item behind it.
                report
                    .errors
                    .push(format!("item {}: live entry missing blob", item.item_id));
                actions.push(FeedAction::SkipTerminal { seq: item.seq });
                continue;
            };
            // The AAD item nonce is hex(content_sha256) — the SAME value the publisher sealed under.
            let Some(sha) = item.content_sha256.clone() else {
                // TERMINAL: no content hash ⇒ we can't derive the AAD nonce for this cell — permanent.
                report
                    .errors
                    .push(format!("item {}: live entry missing content hash", item.item_id));
                actions.push(FeedAction::SkipTerminal { seq: item.seq });
                continue;
            };
            let item_nonce = org_item_nonce(&sha);

            // Resolve the OCK for THIS item's generation (RAM cache / grant unwrap; gated on MK
            // session). Unavailable ⇒ TRANSIENT key gap → record + STOP (retried next sync).
            let ock = match acquire_org_ock(state, &org.org_id, item.generation).await {
                Ok(k) => k,
                Err(e) => {
                    report
                        .errors
                        .push(format!("item {}: key unavailable ({})", item.item_id, brief_err(&e)));
                    break 'pages;
                }
            };
            let ciphertext = match client.get_blob(access, &blob_id).await {
                Ok(c) => c,
                Err(e) => {
                    // TRANSIENT: a network blob-fetch failure may succeed next sync → STOP, don't skip.
                    report
                        .errors
                        .push(format!("item {}: blob fetch failed ({})", item.item_id, brief_err(&e)));
                    break 'pages;
                }
            };
            // OPEN (verify-before-trust: fails closed on wrong OCK / tampered cell / wrong AAD).
            let env = match crate::share::org_envelope::open_org_envelope(
                &ock,
                &ciphertext,
                &org.org_id,
                &item_nonce,
            ) {
                Ok(e) => e,
                Err(e) => {
                    // TERMINAL: this exact ciphertext will never open under this key/AAD (a tampered or
                    // corrupt cell is permanent for THAT seq). Skip past it rather than stall the feed.
                    report.errors.push(format!(
                        "item {}: envelope open failed ({})",
                        item.item_id,
                        brief_err(&e)
                    ));
                    actions.push(FeedAction::SkipTerminal { seq: item.seq });
                    continue;
                }
            };
            // INTEGRITY: the opened plaintext's own hash must equal the feed-supplied one the AAD was
            // derived from (a successful AAD open already implies this, but assert so a server pairing
            // a valid cell with a lying feed hash is caught).
            if env.content_sha256() != sha {
                // TERMINAL: the cell/hash pairing is permanently inconsistent for this seq.
                report
                    .errors
                    .push(format!("item {}: content hash mismatch", item.item_id));
                actions.push(FeedAction::SkipTerminal { seq: item.seq });
                continue;
            }
            actions.push(FeedAction::Ingest {
                item_id: item.item_id.clone(),
                seq: item.seq,
                rev: item.rev,
                generation: item.generation,
                env,
                sha,
                author_user_id: item.author_user_id.clone(),
            });
        }
        if (feed.items.len() as u32) < ORG_FEED_PAGE {
            break; // fewer than a full page ⇒ feed drained.
        }
        cursor = cursor.max(feed.next_seq);
    }

    // ── INGEST PHASE — on the blocking pool, through the ONE global heavy-inference gate ──────────
    // Ingesting an item embeds it via Candle/Metal (`upsert_org_item` → `embed_passage`), which used
    // to run INLINE on this async command's Tokio worker AND outside `perf::run_heavy` — a large feed
    // pull could run an ungated Metal forward pass concurrently with transcription/diarization. Route
    // the whole apply loop through the shared gate like every other heavy native call site (the
    // `unlock_folder` restore is the exemplar). SKIPPED entirely when the pull produced no actions —
    // the every-60s background tick's common case — so an idle tick no longer constructs an embedder
    // (or takes the heavy permit) per org. The embedder lives entirely INSIDE the blocking closure,
    // so the old "must drop before the backfill's `.await` or the future stops being `Send`" block
    // dance is now moot by construction; `active_embedder()` itself is a cheap handle to the ONE
    // process-wide cached engine (see `embed::REAL_EMBEDDER_CACHE`), not a fresh per-org instance.
    if !actions.is_empty() {
        let db = state.db.clone();
        let org_id = org.org_id.clone();
        let fts_only = report.fts_only;
        let (tombstoned, ingested) = crate::perf::run_heavy(
            &state.heavy_inference,
            move || -> Result<(u32, u32), AppError> {
                let embedder: Option<Box<dyn crate::embed::Embedder>> = if fts_only {
                    None
                } else {
                    Some(crate::embed::active_embedder())
                };
                let embedder_ref: Option<&dyn crate::embed::Embedder> = embedder.as_deref();
                let mut tombstoned: u32 = 0;
                let mut ingested: u32 = 0;
                let mut applied = cursor;
                for action in actions {
                    match action {
                        FeedAction::Tombstone { item_id, seq } => {
                            db.tombstone_org_item(&item_id)?;
                            tombstoned += 1;
                            applied = applied.max(seq);
                        }
                        // FIX D: a permanently un-ingestable item advances the cursor past its seq (no DB
                        // write), so it never stalls the feed — the good item behind it ingests on the SAME sync.
                        FeedAction::SkipTerminal { seq } => {
                            applied = applied.max(seq);
                            db.set_org_last_seq(&org_id, applied as i64)?;
                        }
                        FeedAction::Ingest {
                            item_id,
                            seq,
                            rev,
                            generation,
                            env,
                            sha,
                            author_user_id,
                        } => {
                            // AUTHOR (root-cause fix, 2026-07-15): pass the feed's server-authoritative author
                            // id DIRECTLY into the upsert (in both the INSERT and the `ON CONFLICT`'s
                            // `COALESCE`, so it can never be clobbered back to NULL by a later light re-upsert)
                            // instead of a separate follow-up `set_org_item_author` call — this row is correct
                            // the moment it's written, not one extra statement later. Server never sends an
                            // empty author id, but guard anyway — an empty string is passed through as `None`
                            // rather than stamping a blank value.
                            let author_ref = if author_user_id.is_empty() {
                                None
                            } else {
                                Some(author_user_id.as_str())
                            };
                            db.upsert_org_item(
                                &item_id,
                                &org_id,
                                seq,
                                &env.author_hint,
                                &env.title,
                                &env.markdown,
                                &env.created_at,
                                rev,
                                generation,
                                &sha,
                                // Straight off the opened envelope: `Some("document"|"meeting")` for an item
                                // published from a v2 wire envelope (this device's own publishes, or a peer
                                // already on v2); `None` for one opened off an old v1 envelope (no wire signal) —
                                // honest "unclassified", never guessed.
                                env.source_kind
                                    .map(crate::share::org_envelope::OrgSourceKind::as_str),
                                author_ref,
                                embedder_ref,
                            )?;
                            ingested += 1;
                            applied = applied.max(seq);
                        }
                    }
                    // Advance the cursor per successfully-applied item (monotonic; Core's setter no-ops backward).
                    db.set_org_last_seq(&org_id, applied as i64)?;
                }
                Ok((tombstoned, ingested))
            },
        )
        .await?;
        report.tombstoned += tombstoned;
        report.ingested += ingested;
    }

    // STALE-INGEST BACKFILL (2026-07-15): the cursor-based pull above only ever asks for `seq > cursor`,
    // so an item ingested BEFORE `author_user_id` existed (or via a local-replica upsert at
    // share/republish time) has its seq already behind the cursor and is NEVER re-visited by the normal
    // pull — its `author_user_id` would stay NULL forever, permanently blocking `org_get_item`'s
    // editable check for that item on every OTHER machine of its own author (see `org_get_item`,
    // `session_server_user_id`). Root-cause fix (not a workaround): actively re-derive the missing
    // authors from the server's full feed, which DOES carry `author_user_id` for every item regardless
    // of cursor position (`ShareClient::org_feed` takes an explicit `since_seq`; passing 0 replays the
    // whole feed). Cheap on the common case (no NULL rows ⇒ zero extra requests) and bounded (stops the
    // moment every local NULL row has been matched, never scans past what it needs).
    backfill_null_org_item_authors(state, client, access, &org.org_id, report).await;

    // `report.last_seq` reflects the LAST org synced (per-org field on an aggregate report).
    report.last_seq = state.db.org_last_seq_for(&org.org_id)?;
    Ok(())
}

/// Re-derive `author_user_id` for any of this org's locally-held LIVE items still missing it, from a
/// full-feed re-pull starting at `since_seq=0`. Never touches the org's real sync cursor
/// (`org_last_seq_for`/`set_org_last_seq`) — this is a read-only side query purely to recover author
/// ids the cursor-based pull will never see again. Best-effort: any error here is swallowed into
/// `report.errors` (content-free) rather than failing the whole sync, since a missing author id only
/// degrades edit-in-place, it never blocks reading the item.
async fn backfill_null_org_item_authors(
    state: &AppState,
    client: &crate::share::client::ShareClient,
    access: &str,
    org_id: &str,
    report: &mut crate::storage::models::OrgSyncReport,
) {
    let missing = match state.db.org_item_ids_with_null_author(org_id) {
        Ok(ids) => ids,
        Err(e) => {
            report
                .errors
                .push(format!("author backfill: local lookup failed ({})", brief_err(&e)));
            return;
        }
    };
    if missing.is_empty() {
        return; // the common case — nothing to do, no extra network round-trip.
    }
    let mut remaining: std::collections::HashSet<String> = missing.into_iter().collect();
    let mut cursor = 0u64;
    // Bounded: stop once every target is found, OR the feed is exhausted (a page shorter than the
    // page size), OR a hard page cap in case the org's feed is huge and pathologically never contains
    // some of the ids (e.g. they were tombstoned between the local ingest and now — `org_item_ids_with_
    // null_author` only reads live rows, so this should not happen, but a cap keeps this provably
    // bounded regardless).
    const MAX_PAGES: u32 = 50; // 50 * 200 = 10,000 items of feed history — generous, still bounded.
    for _ in 0..MAX_PAGES {
        if remaining.is_empty() {
            break;
        }
        let feed = match client.org_feed(access, org_id, cursor, ORG_FEED_PAGE).await {
            Ok(f) => f,
            Err(e) => {
                report
                    .errors
                    .push(format!("author backfill: feed re-pull failed ({})", brief_err(&e)));
                return;
            }
        };
        if feed.items.is_empty() {
            break;
        }
        for item in &feed.items {
            if item.author_user_id.is_empty() {
                continue;
            }
            if remaining.remove(&item.item_id) {
                if let Err(e) = state.db.set_org_item_author(&item.item_id, &item.author_user_id) {
                    report
                        .errors
                        .push(format!("author backfill: stamp failed ({})", brief_err(&e)));
                    continue;
                }
                report.authors_backfilled += 1;
            }
        }
        let page_len = feed.items.len() as u32;
        cursor = cursor.max(feed.next_seq);
        if page_len < ORG_FEED_PAGE {
            break; // fewer than a full page ⇒ feed drained.
        }
    }
}

/// A short, PII-free rendering of an error for a sync report string (never note content — AppError
/// Display here carries only stage/status labels the client controls).
pub(crate) fn brief_err(e: &AppError) -> String {
    match e {
        AppError::Locked(_) => "locked".to_string(),
        AppError::Auth(_) => "auth".to_string(),
        AppError::Unavailable(_) => "unavailable".to_string(),
        _ => "error".to_string(),
    }
}

/// `org_get_item(item_id)` — the full decrypted org item for the read-only FE viewer. Org items are
/// deliberately org-disclosed content (no folder lock gate applies). Returns `None` for an unknown or
/// tombstoned item.
#[tauri::command]
pub fn org_get_item(
    state: State<'_, AppState>,
    item_id: String,
) -> Result<Option<crate::storage::models::OrgItemDetail>, AppError> {
    let st = state.inner();
    let Some(mut detail) = st.db.get_org_item(&item_id)? else {
        return Ok(None);
    };
    // EDITABLE (F-org-editable-any-device, 2026-07-14): the caller may edit this item in-place when
    // they AUTHORED it — proven by the server-authoritative `author_user_id` stored at feed-ingest
    // matching the caller's own `server_user_id`, NOT by a local `org_shares` anchor (which only exists
    // on the machine that first shared it). On the origin machine the viewer redirects to the local
    // source before this ever renders; a second machine has no anchor, so this is what unlocks editing
    // there. Fail-closed: any missing piece (no stored author, no live session) ⇒ not editable.
    if let Some(ctx) = st.db.org_item_edit_ctx(&item_id)? {
        if let (Some(author), Ok(me)) =
            (ctx.author_user_id.as_deref(), session_server_user_id(st))
        {
            detail.editable = author == me;
        }
    }
    Ok(Some(detail))
}

/// `org_update_own_item(item_id, title, markdown)` — edit-in-place + re-publish for an org item the
/// caller AUTHORED, from ANY of their machines (the "can't edit my own org note on my other Mac" fix).
/// Same egress discipline as the share/republish paths (spec gate order): ownership gate → consent
/// fail-closed → clean + scrub → seal under the OCK with LOCAL open-verify (verify-before-egress) →
/// upload blob + publish the NEXT rev → tombstone the OLD item (publish-BEFORE-tombstone, so a crash
/// leaves a recoverable transient dup, never a window with no org copy) → refresh the local replica.
/// Returns the NEW server item id (the server mints one per publish) so the FE can navigate to it.
///
/// This deliberately does NOT touch any local vault note (Variant 1 — the user chose edit-in-place, not
/// "adopt into this machine's vault"): the org item is edited as its own thing; nothing is materialised
/// as a `documents` row here.
#[tauri::command]
pub async fn org_update_own_item(
    app: AppHandle,
    state: State<'_, AppState>,
    item_id: String,
    title: String,
    markdown: String,
) -> Result<String, AppError> {
    let new_item_id = org_update_own_item_inner(state.inner(), &item_id, &title, &markdown).await?;
    // Ping every open org view (Notes list + Settings shared-brain) to re-fetch — the edit superseded
    // the item (new id) + tombstoned the old. Content-free; best-effort (never affects the result).
    crate::events::emit_org_feed_updated(&app, 1);
    Ok(new_item_id)
}

pub(crate) async fn org_update_own_item_inner(
    state: &AppState,
    item_id: &str,
    title: &str,
    markdown: &str,
) -> Result<String, AppError> {
    // Resolve the item's edit context (org, current rev, original created_at/source_kind, author id).
    let ctx = state
        .db
        .org_item_edit_ctx(item_id)?
        .ok_or_else(|| AppError::InvalidArg("no such org item (or it was removed)".into()))?;

    // (0) OWNERSHIP GATE — only the AUTHOR may re-publish. Server-authoritative id (stored at ingest),
    // compared to this session's `server_user_id`. A missing stored author ⇒ refuse (fail-closed): we
    // must never let a member overwrite a colleague's item.
    let me = session_server_user_id(state)?;
    if ctx.author_user_id.as_deref() != Some(me.as_str()) {
        return Err(AppError::Auth(
            "you can only edit org notes you authored".into(),
        ));
    }

    // (2) CONSENT fail-closed (the same global one-time org-egress consent the share path checks).
    {
        let cfg = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
        if !cfg.org_egress_consented {
            return Err(AppError::Unavailable(
                "org sharing not consented — confirm the one-time upload notice first".into(),
            ));
        }
    }

    // (3) CLEAN + (4) SCRUB the edited body — same leak-safe transform + PII scrub the share/republish
    // paths apply (scrub ON: fail-safe toward LESS egress; an edit must never DOWNGRADE scrubbing).
    let cleaned = crate::share::envelope::clean_note_body(markdown);
    let (final_md, _counts) = scrub_org_markdown(&cleaned);

    // (4b) REFUSE AN EMPTY EDIT — sibling of `build_org_share_body`'s "refuse an empty share"
    // guard (2026-07-16): this is a SEPARATE org-publish path (edit-in-place on an already-shared
    // item, reached straight from the FE's `orgUpdateOwnItem`, never routing through
    // `build_org_share_body`), so it needs its own copy of the same refusal or an author could
    // reproduce the identical "blank card" bug by editing their own item down to nothing. Refuse
    // loudly instead of ever sealing/publishing an empty body.
    if final_md.trim().is_empty() {
        return Err(AppError::InvalidArg(
            "this note has no content to share — add some content before saving".into(),
        ));
    }

    // Resolve the org (membership-checked) + a live session; seal at the org's CURRENT generation.
    let org = resolve_org(state, &ctx.org_id)?;
    let generation = org.generation;
    let base = share_base_url(state)?;
    let (_account_id, _gen_id, _mk, access_token) = require_session_mk(state).await?;
    let client = crate::share::client::ShareClient::new(&base)?;

    // Author hint = the account email local-part (display label; never note content) — recomputed from
    // the session like the share path, not read from the (potentially stale) stored hint.
    let author_hint = {
        let g = state
            .account_session
            .lock()
            .map_err(|_| AppError::Storage("account-session mutex poisoned".into()))?;
        crate::share::require_login(&g)
            .map(|s| org_author_hint(&s.email))
            .unwrap_or_else(|_| "member".to_string())
    };
    let source_kind = match ctx.source_kind.as_deref() {
        Some("meeting") => crate::share::org_envelope::OrgSourceKind::Meeting,
        _ => crate::share::org_envelope::OrgSourceKind::Document,
    };

    let new_rev = ctx.rev.saturating_add(1);
    let env = crate::share::org_envelope::OrgEnvelope::new(
        crate::share::org_envelope::OrgItemKind::Note,
        title.to_string(),
        final_md,
        author_hint,
        ctx.created_at.clone(),
        new_rev,
        source_kind,
    );
    let content_sha = env.content_sha256();

    // (5) SEAL under the OCK + LOCAL OPEN-VERIFY (verify-before-egress). AAD nonce = hex(content_sha256),
    // deterministic + rides the feed so every member reconstructs it.
    let ock = acquire_org_ock(state, &org.org_id, generation).await?;
    let item_nonce = org_item_nonce(&content_sha);
    let (ciphertext, _sha) =
        crate::share::org_envelope::seal_org_envelope(&ock, &env, &org.org_id, &item_nonce)?;

    // (6) upload → publish the NEXT rev. A failure here leaves the OLD item live (never tombstoned yet),
    // so the org is never left with no copy.
    let now = chrono::Utc::now().to_rfc3339();
    let blob_id = client.put_blob(&access_token, ciphertext).await?;
    let published = client
        .org_publish_item(
            &access_token,
            &org.org_id,
            crate::share::org_dto::PublishItemRequest {
                blob_id,
                content_sha256: content_sha.clone(),
                rev: new_rev,
                generation,
            },
        )
        .await?;
    crate::share::ledger_row(&state.db, &client.host(), "org_share_publish", content_sha.len());

    // LOCAL REPLICA CONSISTENCY: upsert the NEW item (so the Notes list resolves it immediately) +
    // stamp our authorship on it directly (root-cause fix, 2026-07-15 — `me` was already proven to be
    // this item's author by the ownership gate above, so pass it straight into the upsert rather than
    // a separate follow-up `set_org_item_author` call; the row is correct the instant it's written) +
    // tombstone the OLD replica. FTS-only (`None` embedder) to keep the editor-close path light; the
    // next `org_sync_now` re-ingests + embeds. Best-effort — a local-replica error must never fail the
    // save (the server copy is already live).
    if let Err(e) = state.db.upsert_org_item(
        &published.item_id,
        &org.org_id,
        published.seq,
        &env.author_hint,
        &env.title,
        &env.markdown,
        &env.created_at,
        new_rev,
        generation,
        &content_sha,
        env.source_kind
            .map(crate::share::org_envelope::OrgSourceKind::as_str),
        Some(me.as_str()),
        None,
    ) {
        tracing::warn!(target: "org", error = %e, "org edit: local replica upsert failed (server copy live)");
    }
    // Repoint any local `org_shares` anchor for the OLD id (usually none on a non-origin machine, but if
    // this IS the origin machine keep the anchor pointing at the live item so the vault-note republish
    // path stays consistent).
    if let Ok(Some(row)) = state.db.org_share_by_item(item_id) {
        let _ = state.db.reset_org_share_for_retry(
            &row.id,
            row.title.as_deref(),
            new_rev,
            generation,
            &content_sha,
            &now,
        );
        let _ = state.db.set_org_share_uploaded(&row.id, &published.item_id, &now);
    }
    if published.item_id != item_id {
        if let Err(e) = state.db.tombstone_org_item(item_id) {
            tracing::warn!(target: "org", error = %e, "org edit: local old-item tombstone failed");
        }
    }

    // THEN tombstone the OLD item on the server so members evict the stale copy. Publish-BEFORE-tombstone
    // (done above): a crash here leaves a transient dup (recoverable), never a window with no org copy. A
    // tombstone failure is non-fatal — the new copy is already live; the stale one lingers until a sweep.
    if published.item_id != item_id {
        match client
            .org_tombstone_item(&access_token, &org.org_id, item_id)
            .await
        {
            Ok(()) => crate::share::ledger_row(&state.db, &client.host(), "org_share_revoke", 0),
            Err(e) => tracing::warn!(
                target: "org",
                error = %e,
                org_id = %org.org_id,
                "org edit: superseded item published but old-item tombstone failed (transient dup)"
            ),
        }
    }

    Ok(published.item_id)
}

/// `delete_org_item_as_author(item_id)` — let an author remove their own shared note/meeting from
/// the org space FROM A DEVICE THAT NEVER SHARED IT (the "author has no delete affordance on a
/// second machine" gap). The pre-existing delete paths (`delete_note_inner`/`delete_meeting_inner`/
/// `delete_document_inner` → `revoke_org_shares_for_source`) only ever act through a LOCAL
/// `documents`/`meetings` row + a LOCAL `org_shares` anchor — both of which exist ONLY on the
/// origin device that first shared the item. A different machine that merely ingested the
/// `org_items` REPLICA (e.g. the author's other Mac, or the author signed in fresh) has neither, so
/// none of those commands can act on it, and `org_resolve_source_inner` correctly returns `None`
/// there too (same local-anchor gap, out of scope here) — leaving the author with no way at all to
/// take an item they wrote down from the shared space.
///
/// DELIBERATE, ASYMMETRIC SEMANTICS (this is "leave/remove from org", NOT "destroy the original"):
/// this command tombstones the item in the SHARED org space — this device's own `org_items` replica
/// immediately, and (via the existing tombstone-apply path in `org_sync_one`'s `FeedAction::Tombstone`
/// arm) the ORIGIN device's replica on its own next sync. It NEVER touches the origin device's local
/// `documents`/`meetings` source row or its vault `.md` file — that local source is simply out of
/// reach from a non-origin device, and is not what "delete" means here. An author who wants to
/// destroy the original note/meeting itself still does that from the origin device, which cascades
/// into `revoke_org_shares_for_source` as before.
///
/// ORDER (fail-closed, verify-before-destroy discipline mirrored for a REMOVAL instead of a seal):
/// (1) ownership gate — the stored `author_user_id` (now reliably populated at write time by the
/// `upsert_org_item` fix above) must equal this session's `server_user_id`, else `AppError::Auth`,
/// no mutation at all. (2) tombstone on the SERVER FIRST (`org_tombstone_item`, idempotent — a 404
/// counts as already-gone) — a network failure here propagates and stops BEFORE any local mutation,
/// so this device's copy is never removed while the server (and thus every other member/device)
/// still serves it; that would be a worse, silently-reappearing state than simply failing loud. (3)
/// only once the server confirms gone does the LOCAL `org_items` replica get tombstoned
/// (`Db::tombstone_org_item`, already used by the feed's own tombstone-apply arm), so it drops out of
/// this device's Notes/Meetings list immediately. (4) best-effort `org-feed-updated` emit so every
/// open org view re-fetches, exactly like every other org-item mutation in this file.
#[tauri::command]
pub async fn delete_org_item_as_author(
    app: AppHandle,
    state: State<'_, AppState>,
    item_id: String,
) -> Result<(), AppError> {
    delete_org_item_as_author_inner(state.inner(), &item_id).await?;
    crate::events::emit_org_feed_updated(&app, 1);
    Ok(())
}

/// Inner of [`delete_org_item_as_author`] taking `&AppState` (unit-testable ownership gate + the
/// fail-closed server-then-local tombstone ordering).
pub(crate) async fn delete_org_item_as_author_inner(
    state: &AppState,
    item_id: &str,
) -> Result<(), AppError> {
    let item_id = item_id.trim();
    if item_id.is_empty() {
        return Err(AppError::InvalidArg("item id required".into()));
    }

    // (1) OWNERSHIP GATE — identical signal Bug-A's fix now makes reliable at write time: the
    // server-authoritative `author_user_id` stored on the local replica must match this session's
    // own server user id. A missing stored author, an unknown/tombstoned item, or no live session
    // all refuse — fail-closed, never let a member remove a colleague's item (or a stale/ambiguous
    // one).
    let ctx = state
        .db
        .org_item_edit_ctx(item_id)?
        .ok_or_else(|| AppError::InvalidArg("no such org item (or it was already removed)".into()))?;
    let me = session_server_user_id(state)?;
    if ctx.author_user_id.as_deref() != Some(me.as_str()) {
        return Err(AppError::Auth(
            "you can only remove org notes you authored".into(),
        ));
    }

    // (2) SERVER TOMBSTONE FIRST — fail loud, no local mutation yet. `org_tombstone_item` is
    // idempotent (a 404 — already gone — is treated as success), so a repeat call after a prior
    // partial failure (server succeeded, local step below didn't run yet) is safe to retry.
    let org = resolve_org(state, &ctx.org_id)?;
    let base = share_base_url(state)?;
    let access_token = valid_access_token(state).await?;
    let client = crate::share::client::ShareClient::new(&base)?;
    client
        .org_tombstone_item(&access_token, &org.org_id, item_id)
        .await?;
    crate::share::ledger_row(&state.db, &client.host(), "org_share_revoke", 0);

    // (3) ONLY NOW tombstone the local replica — the server confirmed gone, so dropping this
    // device's own copy can never leave a dangling "removed here but still live on the server"
    // state. Reuses the SAME local tombstone primitive the feed's own `FeedAction::Tombstone` arm
    // uses (`Db::tombstone_org_item`) — not a bespoke delete.
    state.db.tombstone_org_item(item_id)?;

    // The ORIGIN device (if this isn't it) still has its own separate local `documents`/`meetings`
    // source row, untouched by design — it picks up this tombstone on its OWN next `org_sync_one`
    // pull via the existing `FeedAction::Tombstone` handling and drops its own `org_items` replica
    // then. See the doc comment above: this is "leave/remove from org", not "destroy the original".
    Ok(())
}

/// `list_org_items(org_id)` — the browsable LIST of one org's live items (headers only), so a member
/// can SEE what colleagues shared into the org (the root of the "can't see B's note" bug: org content
/// was search-only, with no browsable list). Resolves the FE-picked org membership-checked
/// (`resolve_org`) — never the first via `.next()` — then returns newest-first headers. Org items are
/// deliberately org-disclosed content (no folder lock gate applies); headers carry NO markdown body
/// (that's `org_get_item`), keeping the list content-min. Not-a-member ⇒ `InvalidArg` (never a leak
/// of another org's list).
#[tauri::command]
pub fn list_org_items(
    state: State<'_, AppState>,
    org_id: String,
) -> Result<Vec<crate::storage::models::OrgItemHeader>, AppError> {
    list_org_items_inner(state.inner(), &org_id)
}

/// Inner of [`list_org_items`] taking `&AppState` (unit-testable membership gate). Refuses an org the
/// caller isn't a local member of; org items are org-disclosed content so no folder lock gate applies.
pub(crate) fn list_org_items_inner(
    st: &AppState,
    org_id: &str,
) -> Result<Vec<crate::storage::models::OrgItemHeader>, AppError> {
    // Membership re-check: refuse if the caller isn't a local member of this org.
    let org = resolve_org(st, org_id)?;
    // PER-INSTANCE ORG TOGGLE: a disabled org is EMPTY here — not an error (it's a deliberate,
    // reversible local setting, not a membership problem) — the local replica is never deleted, so
    // this is silent and instant to reverse. Mirrors the `context_enabled = 1` SQL filter in
    // `search_org_chunks_knn`/`_fts`; this is the direct-browse (Library "Shared brains") twin.
    if !org.context_enabled {
        return Ok(Vec::new());
    }
    let mut items = st.db.list_org_items(&org.org_id)?;
    // OWNERSHIP ENRICHMENT (F-org-editable): for each replica the CALLER published, resolve its local
    // editable source + CURRENT title so the FE links straight to the editable original and never shows
    // a stale publish-time snapshot. GATED — a locked-not-unlocked source resolves to `None` (its title
    // must never leak through this org-disclosed list). A non-author's item has no local share ⇒ `None`
    // ⇒ stays a read-only replica.
    //
    // KIND (source-type) ENRICHMENT: separate from the above — `kind` is METADATA (meeting vs document),
    // never content. `Db::list_org_items` now populates `item.kind` DIRECTLY from the stored
    // `org_items.source_kind` column for EVERY item (opened off a v2 `OrgEnvelope` at ingest, so a
    // colleague's item now classifies too). This local `org_shares`-anchored resolver is kept as a
    // fallback/override ONLY for the CALLER'S OWN items — it stays correct (and UNGATED, unlike
    // `owned_source`: a locked source still reports its kind) even if the stored column is somehow null
    // for a row ingested before this column existed. It must NOT clobber a correctly-populated stored
    // value for a colleague's item with `None`.
    for item in &mut items {
        if let Some(own_kind) = resolve_own_item_kind(st, &item.item_id)? {
            item.kind = Some(own_kind);
        }
        if let Some(src) = resolve_owned_source(st, &item.item_id)? {
            item.title = src.title;
            item.owned_source = Some(src.owned);
        }
    }
    Ok(items)
}

/// The source kind (`"document"` | `"meeting"`) of an org item THIS device published, from the local
/// `org_shares` anchor — metadata only (no title/body), so UNGATED (a locked source still reports its
/// kind; only `owned_source`'s title is lock-gated). `None` when no local share row exists (an item
/// published by a colleague) or the row anchors neither `meeting_id` nor `document_id` (shouldn't
/// happen — `insert_org_share` always sets exactly one — but fails closed to `None` rather than guess).
/// A fallback/override for the CALLER'S OWN items only — see the call site in `list_org_items_inner`;
/// `Db::list_org_items` now populates `kind` for colleagues' items too, straight from storage.
fn resolve_own_item_kind(st: &AppState, item_id: &str) -> Result<Option<String>, AppError> {
    let Some(row) = st.db.org_share_by_item(item_id)? else {
        return Ok(None);
    };
    if row.document_id.is_some() {
        Ok(Some("document".to_string()))
    } else if row.meeting_id.is_some() {
        Ok(Some("meeting".to_string()))
    } else {
        Ok(None)
    }
}

/// The caller's editable local source + its CURRENT title for an org item they published, if any. `None`
/// when the item was shared by someone else (no local `org_shares` row), the source row is gone, or the
/// source is locked-and-not-session-unlocked (never leak a sealed title through the org list). Powers the
/// `OrgItemHeader.owned_source` enrichment; the gate mirrors the note/meeting content-read gates.
struct OwnedSourceResolved {
    title: String,
    owned: crate::storage::models::OrgOwnedSource,
}

fn resolve_owned_source(
    st: &AppState,
    item_id: &str,
) -> Result<Option<OwnedSourceResolved>, AppError> {
    let Some(row) = st.db.org_share_by_item(item_id)? else {
        return Ok(None);
    };
    // Mirrors `org_resolve_source_inner`: only a `revoked` row is refused — it was INTENTIONALLY
    // torn down, so it must not enrich as owned. A `failed` row with its `item_id` still set (a
    // republish that failed transiently but whose PRIOR publish is still genuinely live on the
    // server, see `org_shares_for_source`) falls through, so the author's own stuck row still
    // resolves to its real editable source + current title instead of quietly looking unowned in
    // the list and routing the click to the read-only viewer.
    if row.state == "revoked" {
        return Ok(None);
    }
    if let Some(document_id) = row.document_id {
        let Some(note) = st.db.get_note_row(&document_id)? else {
            return Ok(None);
        };
        // GATE: a sealed-not-unlocked note's title must not leak into the org list.
        if !folder_is_unlocked(st, &note.folder_id)? {
            return Ok(None);
        }
        return Ok(Some(OwnedSourceResolved {
            title: note_display_title(&note),
            owned: crate::storage::models::OrgOwnedSource {
                kind: "document".to_string(),
                id: document_id,
            },
        }));
    }
    if let Some(meeting_id) = row.meeting_id {
        // GATE: a sealed-not-unlocked meeting refuses (masked title never leaks).
        if !meeting_is_unlocked(st, &meeting_id)? {
            return Ok(None);
        }
        let title = st
            .db
            .get_meeting(&meeting_id)?
            .and_then(|m| m.title.clone())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| "Shared note".to_string());
        return Ok(Some(OwnedSourceResolved {
            title,
            owned: crate::storage::models::OrgOwnedSource {
                kind: "meeting".to_string(),
                id: meeting_id,
            },
        }));
    }
    Ok(None)
}
