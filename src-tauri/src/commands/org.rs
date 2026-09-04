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
// Brought into scope (rather than spelled out at the call site) so the `org-feed-updated` notice is
// callable on a `&dyn` notifier — the seam that makes "did this command tell the FE to re-fetch?"
// unit-testable without a Tauri runtime.
#[cfg(test)]
use crate::events::OrgFeedNotifier;

/// Resolve only exact-owner images referenced by outgoing Markdown. Missing/foreign markers are
/// flattened to inert alt text; arbitrary URLs are never fetched.
fn attachment_bundle_for_markdown(
    state: &AppState,
    owner: &crate::storage::AttachmentOwner,
    markdown: &str,
) -> Result<(String, Vec<murmur_protocol::envelope::ShareAttachment>), AppError> {
    let referenced = crate::commands::referenced_attachment_ids(markdown)?;
    // The exact-owner helper acquires the lifecycle guard and gates BEFORE its first attachment
    // query. Never pre-list rows here: attachment records carry plaintext bytes and sealed blobs.
    let items = crate::commands::attachment_bundle_for_owner(state, owner, &referenced)?;
    let allowed: std::collections::HashSet<String> =
        items.iter().map(|item| item.id.clone()).collect();
    let markdown = crate::share::envelope::sanitize_share_images(markdown, &allowed);
    let attachments = items
        .into_iter()
        .map(|item| murmur_protocol::envelope::ShareAttachment {
            id: item.id,
            mime_type: item.mime_type,
            width: item.width,
            height: item.height,
            sha256: item.sha256.to_vec(),
            data: item.data,
        })
        .collect();
    Ok((markdown, attachments))
}

fn task_attachment_bundle_for_markdown(
    state: &AppState,
    owner: &crate::storage::AttachmentOwner,
    org_id: &str,
    markdown: &str,
) -> Result<(String, Vec<murmur_protocol::envelope::ShareAttachment>), AppError> {
    let referenced = crate::commands::referenced_attachment_ids(markdown)?;
    let items = crate::commands::attachment_bundle_for_task_source_authorized(
        state,
        owner,
        org_id,
        &referenced,
    )?;
    let allowed: std::collections::HashSet<String> =
        items.iter().map(|item| item.id.clone()).collect();
    let markdown = crate::share::envelope::sanitize_share_images(markdown, &allowed);
    let attachments = items
        .into_iter()
        .map(|item| murmur_protocol::envelope::ShareAttachment {
            id: item.id,
            mime_type: item.mime_type,
            width: item.width,
            height: item.height,
            sha256: item.sha256.to_vec(),
            data: item.data,
        })
        .collect();
    Ok((markdown, attachments))
}

fn share_envelope_with_attachments(
    state: &AppState,
    owner: &crate::storage::AttachmentOwner,
    title: String,
    markdown: String,
    created_at: String,
) -> Result<murmur_protocol::envelope::ShareEnvelope, AppError> {
    let (markdown, attachments) = attachment_bundle_for_markdown(state, owner, &markdown)?;
    let envelope = murmur_protocol::envelope::ShareEnvelope::new(title, markdown, created_at);
    Ok(if attachments.is_empty() {
        envelope
    } else {
        envelope.with_attachments(attachments)
    })
}

/// Validate the complete authenticated manifest, assign fresh local ids, and rewrite Markdown.
pub(crate) fn prepare_incoming_attachment_bundle(
    markdown: &str,
    attachments: &[murmur_protocol::envelope::ShareAttachment],
) -> Result<(String, Vec<crate::storage::IncomingAttachment>), AppError> {
    let referenced = crate::commands::referenced_attachment_ids(markdown)?;
    let mut wire_ids = std::collections::HashSet::new();
    let mut id_map = std::collections::HashMap::new();
    let mut incoming = Vec::with_capacity(attachments.len());
    for attachment in attachments {
        let wire_id = attachment.id.to_ascii_lowercase();
        let parsed = uuid::Uuid::parse_str(&wire_id)
            .map_err(|_| AppError::InvalidArg("shared image id is not a UUID".into()))?;
        if parsed.get_version_num() != 4 || !wire_ids.insert(wire_id.clone()) {
            return Err(AppError::InvalidArg(
                "shared image ids must be unique UUIDv4 values".into(),
            ));
        }
        if !referenced.contains(&wire_id) {
            return Err(AppError::InvalidArg(
                "shared image manifest contains unreferenced data".into(),
            ));
        }
        let sha256: [u8; 32] =
            attachment.sha256.as_slice().try_into().map_err(|_| {
                AppError::InvalidArg("shared image hash has the wrong length".into())
            })?;
        // Derive the canonical extension from the container. WebKit clients cannot canvas-encode
        // WebP and now share metadata-free PNGs (see attachments::add_note_attachment_inner), so both
        // are accepted here; the authoritative check — magic bytes, dims, sha, and the
        // container-matched metadata rejector — runs in validate_incoming_attachment_bundle below,
        // before any DB write.
        let extension = match attachment.mime_type.as_str() {
            "image/webp" => "webp",
            "image/png" => "png",
            _ => {
                return Err(AppError::InvalidArg(
                    "shared images must be normalized WebP or PNG".into(),
                ))
            }
        };
        let local_id = uuid::Uuid::new_v4().to_string();
        id_map.insert(wire_id, local_id.clone());
        incoming.push(crate::storage::IncomingAttachment {
            id: local_id,
            mime_type: attachment.mime_type.clone(),
            extension: extension.to_string(),
            width: attachment.width,
            height: attachment.height,
            sha256,
            data: attachment.data.clone(),
        });
    }
    crate::commands::validate_incoming_attachment_bundle(&incoming)?;
    Ok((
        crate::share::envelope::remap_share_images(markdown, &id_map),
        incoming,
    ))
}

#[cfg(test)]
pub(crate) const PRE_TASK_READER_RELEASE_TAG: &str = "v1.1.0";
#[cfg(test)]
pub(crate) const PRE_TASK_READER_REVISION: &str =
    "abb6971607d221bfe728d4ff5202ccde7872fcf8";
#[cfg(test)]
pub(crate) const PRE_TASK_READER_COMPAT_REVISION: &str =
    "2e6ff1eaa35273347a19e7284837f86432f80195";
#[cfg(test)]
pub(crate) const PRE_TASK_READER_RELEASE_SUBJECT: &str =
    "Merge pull request #533 from murmur-io/chore/release-1.1.0";
#[cfg(test)]
pub(crate) const PRE_TASK_ORG_ENVELOPE_BLOB_SHA1: &str =
    "7a49b5b7b70ab3ea0cd66e77280866b61c6780ee";
#[cfg(test)]
pub(crate) const PRE_TASK_ORG_ENVELOPE_SOURCE_SHA256: &str =
    "18e76f37177cee674bec35df3849fcbbbe50d504603f6e40fb36d7660a41070f";
#[cfg(test)]
pub(crate) const PRE_TASK_RELEASE_ORG_COMMANDS_BLOB_SHA1: &str =
    "141290c2e518dfab109df9ff93be09b50d586a65";
#[cfg(test)]
pub(crate) const PRE_TASK_RELEASE_ORG_COMMANDS_SOURCE_SHA256: &str =
    "a88eacd9920b9a47221804910e467f52f7eb546ba9dec676cab7a250a6029392";
#[cfg(test)]
pub(crate) const PRE_TASK_COMPAT_ORG_COMMANDS_BLOB_SHA1: &str =
    "59538a6b230459f96bd4349e85fb4eb9b570951c";
#[cfg(test)]
pub(crate) const PRE_TASK_COMPAT_ORG_COMMANDS_SOURCE_SHA256: &str =
    "3b2cb4688e360e2e16b57136401df938b5a1c33b2c876b64c6346bcec2d1b49d";
#[cfg(test)]
pub(crate) const PRE_TASK_KIND_DISCRIMINATOR_SHA256: &str =
    "6fe0b52de9d7165a21e04dc6b11f92252ca796d2308a02c8427671f233426210";
#[cfg(test)]
pub(crate) const PRE_TASK_TERMINAL_SKIP_SHA256: &str =
    "ee38b7539c693e708a0c9df61fa5404224eb27f82e52fa61c1fe265fd1e5254a";

#[derive(Clone, Copy)]
enum OrgEnvelopeReader {
    Current,
    /// Exact compatibility boundary from the client immediately before Task tag 3 shipped.
    /// The pinned source accepted only kind tags 1/2; everything else failed closed after AEAD open.
    #[cfg(test)]
    PreTaskV110,
}

impl OrgEnvelopeReader {
    fn open(
        self,
        ock: &[u8; 32],
        ciphertext: &[u8],
        org_id: &str,
        item_nonce: &str,
    ) -> Result<crate::share::org_envelope::OrgEnvelope, AppError> {
        match self {
            Self::Current => crate::share::org_envelope::open_org_envelope(
                ock,
                ciphertext,
                org_id,
                item_nonce,
            ),
            #[cfg(test)]
            Self::PreTaskV110 => {
                // Frozen from the released v1.1.0 client revision
                // abb6971607d221bfe728d4ff5202ccde7872fcf8,
                // org_envelope.rs Git blob 7a49b5b7b70ab3ea0cd66e77280866b61c6780ee.
                // Its production `OrgItemKind::from_tag` accepted exactly Note=1/Summary=2.
                // Decrypt under the real production AAD first, then apply that old discriminator.
                let aad = crate::share::org_envelope::org_item_aad(org_id, item_nonce);
                let plaintext = crate::crypto::decrypt(ock, ciphertext, aad.as_bytes())?;
                match plaintext.get(2) {
                    Some(1 | 2) => {
                        crate::share::org_envelope::OrgEnvelope::from_canonical_bytes(&plaintext)
                    }
                    _ => Err(AppError::InvalidArg(
                        "unknown org item kind tag".into(),
                    )),
                }
            }
        }
    }
}

/// Manual org operations remain immediately usable; scheduled work carries one recording-priority
/// epoch from the beginning of its tick. Every background DB commit revalidates that epoch through
/// the coordinator, while network/model work happens outside the short commit lease.
#[derive(Clone, Copy)]
pub(crate) struct OrgWorkPolicy {
    background_epoch: Option<u64>,
    reader: OrgEnvelopeReader,
}

impl OrgWorkPolicy {
    pub(crate) const fn manual() -> Self {
        Self {
            background_epoch: None,
            reader: OrgEnvelopeReader::Current,
        }
    }

    const fn background(epoch: u64) -> Self {
        Self {
            background_epoch: Some(epoch),
            reader: OrgEnvelopeReader::Current,
        }
    }

    #[cfg(test)]
    const fn pre_task_reader() -> Self {
        Self {
            background_epoch: None,
            reader: OrgEnvelopeReader::PreTaskV110,
        }
    }

    fn is_current(self) -> bool {
        match self.background_epoch {
            Some(epoch) => crate::perf::background_epoch_is_current(epoch),
            None => true,
        }
    }

    fn commit<T>(
        self,
        commit: impl FnOnce() -> Result<T, AppError>,
    ) -> Result<Option<T>, AppError> {
        match self.background_epoch {
            Some(epoch) => crate::perf::with_current_background_epoch(epoch, commit),
            None => commit().map(Some),
        }
    }
}

pub(crate) trait AskHistoryInvalidationNotifier: Send + Sync {
    fn ask_history_invalidated(&self);
}

impl AskHistoryInvalidationNotifier for AppHandle {
    fn ask_history_invalidated(&self) {
        emit_ask_history_invalidated_fail_closed(self);
    }
}

pub(crate) fn commit_org_visibility_reduction(
    state: &AppState,
    notifier: Option<&dyn AskHistoryInvalidationNotifier>,
    mutation: impl FnOnce() -> Result<bool, AppError>,
) -> Result<bool, AppError> {
    let _lifecycle = lifecycle_guard(state);
    let changed = mutation()?;
    if changed {
        bump_seal_epoch(state);
        if let Some(notifier) = notifier {
            notifier.ask_history_invalidated();
        }
    }
    Ok(changed)
}

pub(crate) fn commit_org_metadata_mutation(
    state: &AppState,
    notifier: Option<&dyn AskHistoryInvalidationNotifier>,
    mutation: impl FnOnce() -> Result<crate::storage::org_store::OrgMetadataCommitOutcome, AppError>,
) -> Result<crate::storage::org_store::OrgMetadataCommitOutcome, AppError> {
    let _lifecycle = lifecycle_guard(state);
    let outcome = mutation()?;
    if outcome.visibility_reduced {
        bump_seal_epoch(state);
        if let Some(notifier) = notifier {
            notifier.ask_history_invalidated();
        }
    }
    Ok(outcome)
}

pub(crate) fn notify_org_views_if_changed(
    notifier: Option<&dyn crate::events::OrgFeedNotifier>,
    changed: bool,
) {
    if changed {
        if let Some(notifier) = notifier {
            // Zero means the visibility/membership/head changed without consuming a remote feed
            // page. The event remains content-free and every consumer treats it as "refetch".
            notifier.org_feed_updated(0);
        }
    }
}

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
    /// A durable local setup/DELETE cleanup whose remote outcome is still unproven. This wins over
    /// a stale/absent server list row and must never be presented as an active or revoked share.
    pub revoke_pending: bool,
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
    let _mutation = state.lock_org_mutation().await;
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
    // (1) READ-GATE + plaintext snapshot under the lock lifecycle. Its epoch/folder identity is
    // revalidated immediately before upload, after every async auth/network preparation step.
    let source = build_org_share_snapshot(state, Some(&meeting_id), None, false)?;

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
            return Err(AppError::Unavailable(crate::errcode::tag(
                crate::errcode::SHARE_CONSENT,
                "confirm the one-time upload notice first",
            )));
        }
        cfg.share_base_url.clone()
    };
    // Proactively refresh the bearer if it is at/near its 30-min expiry — otherwise a long-lived or
    // biometric-restored session 401s here ("not authenticated") while still looking logged in. Fails
    // closed `Unavailable` when logged out (mirrors the old `require_login`).
    let (access_token, owner_user_id) = authenticated_org_actor(state).await?;

    let client = crate::share::client::ShareClient::new(&base)?;
    let share_id = crate::share::new_share_id();
    let rev = 1u32;
    require_current_org_share_snapshot(state, Some(&meeting_id), None, &source.source_version)?;
    // Capability discovery FIRST, then mint the owner-bound cleanup authority. Discovery is a pure
    // read (`client.health` -> GET /healthz) that mutates nothing remote, so a journal row written
    // ahead of it could only ever describe work that never started.
    //
    // Writing it first permanently locked the source out (2.0 audit). Against a relay that does not
    // advertise the capability: the first click journalled a 'create_pending' row and then failed;
    // the second failed EARLIER and DIFFERENTLY, on `insert_outbound_share_attempt` returning false,
    // with the bare untagged "an interrupted link-share cleanup is already pending"; and the only
    // recovery path, `revoke_share_inner`, needs the SAME missing capability, so it could never
    // converge. Two clicks and that source could not be shared again without a DB repair.
    //
    // The guarantee the original ordering existed for is untouched: once the capability IS present
    // the journal row is still written before the content POST, so a POST failing mid-flight still
    // leaves a visible, retryable local journal rather than losing the id.
    require_share_owner_claim_capability(state, &client).await?;
    if !state.db.insert_outbound_share_attempt(
        &share_id,
        Some(&meeting_id),
        None,
        "link",
        rev,
        &owner_user_id,
        &chrono::Utc::now().to_rfc3339(),
    )? {
        return Err(AppError::Unavailable(
            "an interrupted link-share cleanup is already pending; retry it before sharing again"
                .into(),
        ));
    }

    let OrgShareBodySnapshot {
        title,
        markdown: clean_body,
        created_at,
        counts: _,
        kind: _,
        attachment_owner,
        source_version,
    } = source;

    // (3) Build the inner envelope + seal a fresh link share (e2ee M2). rev starts at 1.
    let env =
        share_envelope_with_attachments(state, &attachment_owner, title, clean_body, created_at)?;
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
    require_current_org_share_snapshot(state, Some(&meeting_id), None, &source_version)?;
    // A failed/ambiguous reservation deliberately leaves `create_pending`; restart recovery repeats
    // the idempotent reservation before attempting DELETE.
    let reserved = reserve_outbound_share_id(
        state,
        &client,
        &access_token,
        &share_id,
        &owner_user_id,
        murmur_protocol::dto::ShareMode::Link,
    )
    .await?;
    if let Err(error) =
        require_current_org_share_snapshot(state, Some(&meeting_id), None, &source_version)
    {
        let _ = retire_ambiguous_outbound_share(state, &client, &access_token, &share_id).await;
        return Err(error);
    }
    let (content_permit, source_commitment) = permit_share_content_dispatch(
        state,
        reserved,
        &source_version,
        &create_req,
        "share_create",
        cell_bytes,
    )?;
    let created = match client
        .create_share(
            &access_token,
            &owner_user_id,
            source_commitment,
            create_req,
            content_permit,
        )
        .await
    {
        Ok(created) => created,
        Err(error) => {
            let _ = retire_ambiguous_outbound_share(state, &client, &access_token, &share_id).await;
            return Err(error);
        }
    };
    state.db.set_outbound_share_state(&share_id, "active")?;

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
    let _mutation = state.lock_org_mutation().await;
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
    // (1) Resolve only the folder anchor before the gate; the helper reads `NoteRow` only after the
    // gate while holding lifecycle, and binds the resulting plaintext to an epoch/folder snapshot.
    let source = build_org_share_snapshot(state, None, Some(&id), false)?;

    // (2) Consent (first-ever share) + logged-in bearer, exactly like the meeting path.
    let base = {
        let cfg = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
        if !cfg.share_egress_consented {
            return Err(AppError::Unavailable(crate::errcode::tag(
                crate::errcode::SHARE_CONSENT,
                "confirm the one-time upload notice first",
            )));
        }
        cfg.share_base_url.clone()
    };
    let (access_token, owner_user_id) = authenticated_org_actor(state).await?;
    let client = crate::share::client::ShareClient::new(&base)?;
    let share_id = crate::share::new_share_id();
    let rev = 1u32;
    require_current_org_share_snapshot(state, None, Some(&id), &source.source_version)?;
    // Capability discovery FIRST, then mint the owner-bound cleanup authority. Discovery is a pure
    // read (`client.health` -> GET /healthz) that mutates nothing remote, so a journal row written
    // ahead of it could only ever describe work that never started.
    //
    // Writing it first permanently locked the source out (2.0 audit). Against a relay that does not
    // advertise the capability: the first click journalled a 'create_pending' row and then failed;
    // the second failed EARLIER and DIFFERENTLY, on `insert_outbound_share_attempt` returning false,
    // with the bare untagged "an interrupted link-share cleanup is already pending"; and the only
    // recovery path, `revoke_share_inner`, needs the SAME missing capability, so it could never
    // converge. Two clicks and that source could not be shared again without a DB repair.
    //
    // The guarantee the original ordering existed for is untouched: once the capability IS present
    // the journal row is still written before the content POST, so a POST failing mid-flight still
    // leaves a visible, retryable local journal rather than losing the id.
    require_share_owner_claim_capability(state, &client).await?;
    if !state.db.insert_outbound_share_attempt(
        &share_id,
        None,
        Some(&id),
        "link",
        rev,
        &owner_user_id,
        &chrono::Utc::now().to_rfc3339(),
    )? {
        return Err(AppError::Unavailable(
            "an interrupted link-share cleanup is already pending; retry it before sharing again"
                .into(),
        ));
    }

    let OrgShareBodySnapshot {
        title,
        markdown: clean_body,
        created_at,
        counts: _,
        kind: _,
        attachment_owner,
        source_version,
    } = source;

    // (4) Seal a fresh link share.
    let env =
        share_envelope_with_attachments(state, &attachment_owner, title, clean_body, created_at)?;
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
    require_current_org_share_snapshot(state, None, Some(&id), &source_version)?;
    let reserved = reserve_outbound_share_id(
        state,
        &client,
        &access_token,
        &share_id,
        &owner_user_id,
        murmur_protocol::dto::ShareMode::Link,
    )
    .await?;
    if let Err(error) = require_current_org_share_snapshot(state, None, Some(&id), &source_version)
    {
        let _ = retire_ambiguous_outbound_share(state, &client, &access_token, &share_id).await;
        return Err(error);
    }
    let (content_permit, source_commitment) = permit_share_content_dispatch(
        state,
        reserved,
        &source_version,
        &create_req,
        "share_create",
        cell_bytes,
    )?;
    let created = match client
        .create_share(
            &access_token,
            &owner_user_id,
            source_commitment,
            create_req,
            content_permit,
        )
        .await
    {
        Ok(created) => created,
        Err(error) => {
            let _ = retire_ambiguous_outbound_share(state, &client, &access_token, &share_id).await;
            return Err(error);
        }
    };
    state.db.set_outbound_share_state(&share_id, "active")?;

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
    list_my_shares_inner(state.inner()).await
}

pub(crate) async fn list_my_shares_inner(state: &AppState) -> Result<Vec<MyShareEntry>, AppError> {
    let base = share_base_url(state)?;
    let (access, actor_user_id) = authenticated_org_actor(state).await?;
    let client = crate::share::client::ShareClient::new(&base)?;
    let local_pending = state
        .db
        .outbound_cleanup_pending_for_owner(&actor_user_id)?;
    let resp = match client.list_shares(&access).await {
        Ok(resp) => resp,
        Err(_) if !local_pending.is_empty() => {
            murmur_protocol::dto::SharesResponse { shares: Vec::new() }
        }
        Err(error) => return Err(error),
    };

    let _lifecycle = lifecycle_guard(state);
    let mut out = Vec::with_capacity(resp.shares.len() + local_pending.len());
    let mut by_share_id = std::collections::HashMap::new();
    for s in resp.shares {
        // NOTE shares (WP6) are anchored on `document_id`; meeting shares on `meeting_id`. Prefer the
        // note anchor. In BOTH cases the title is surfaced ONLY when the source's folder is unlocked
        // (a sealed-not-unlocked source is MASKED: `locked:true`, no title) — same §7 inv. 6 as
        // meetings. A share created on another device (neither anchor local) is masked too.
        let local_document = state.db.outbound_share_document(&s.share_id)?;
        let local_meeting = state.db.outbound_share_meeting(&s.share_id)?;
        let (title, locked) = my_share_local_title_under_lifecycle(
            state,
            local_document.as_deref(),
            local_meeting.as_deref(),
        )?;
        by_share_id.insert(s.share_id.clone(), out.len());
        out.push(MyShareEntry {
            share_id: s.share_id,
            title,
            locked,
            rev: s.rev,
            created_at: s.created_at,
            expires_at: s.expires_at,
            revoked: s.revoked_at.is_some(),
            revoke_pending: false,
            download_count: s.download_count,
            // The meeting anchor is masked-empty ('') for a note share — surface it as None there so
            // the FE never keys a note share on an empty meeting id.
            meeting_id: local_meeting.filter(|m| !m.is_empty()),
            document_id: local_document,
            max_downloads: s.max_downloads,
            mode: s.mode,
        });
    }
    for pending in local_pending {
        let mode = match pending.mode.as_str() {
            "link" => murmur_protocol::dto::ShareMode::Link,
            "user" => murmur_protocol::dto::ShareMode::User,
            _ => {
                return Err(AppError::Storage(
                    "outbound revoke journal has an invalid share mode".into(),
                ));
            }
        };
        let (title, locked) = my_share_local_title_under_lifecycle(
            state,
            pending.document_id.as_deref(),
            pending.meeting_id.as_deref(),
        )?;
        if let Some(index) = by_share_id.get(&pending.share_id).copied() {
            let row = &mut out[index];
            row.title = title;
            row.locked = locked;
            row.rev = pending.rev;
            row.created_at = pending.created_at;
            row.revoked = false;
            row.revoke_pending = true;
            row.meeting_id = pending.meeting_id;
            row.document_id = pending.document_id;
            row.mode = mode;
        } else {
            out.push(MyShareEntry {
                share_id: pending.share_id,
                title,
                locked,
                rev: pending.rev,
                created_at: pending.created_at,
                expires_at: None,
                revoked: false,
                revoke_pending: true,
                download_count: 0,
                meeting_id: pending.meeting_id,
                document_id: pending.document_id,
                max_downloads: None,
                mode,
            });
        }
    }
    Ok(out)
}

/// Resolve only gated local presentation metadata. The caller holds the lifecycle guard; keeping
/// the helper guard-free avoids a non-reentrant self-deadlock while preserving the read boundary.
fn my_share_local_title_under_lifecycle(
    state: &AppState,
    local_document: Option<&str>,
    local_meeting: Option<&str>,
) -> Result<(Option<String>, bool), AppError> {
    if let Some(doc_id) = local_document {
        return match state.db.note_gate_anchor(doc_id)? {
            Some((folder_id, _created_at, _updated_at))
                if folder_is_unlocked(state, &folder_id)? =>
            {
                match state.db.get_note_row(doc_id)? {
                    Some(row) => Ok((Some(note_display_title(&row)), false)),
                    None => Ok((None, true)),
                }
            }
            Some(_) | None => Ok((None, true)),
        };
    }
    match local_meeting.filter(|meeting_id| !meeting_id.is_empty()) {
        Some(meeting_id) if meeting_is_unlocked(state, meeting_id)? => Ok((
            state
                .db
                .get_meeting(meeting_id)?
                .and_then(|meeting| meeting.title),
            false,
        )),
        Some(_) | None => Ok((None, true)),
    }
}

/// `revoke_share(share_id)` — DELETE the server ciphertext + flip the local state. Idempotent.
#[tauri::command]
pub async fn revoke_share(state: State<'_, AppState>, share_id: String) -> Result<(), AppError> {
    let _mutation = state.lock_org_mutation().await;
    revoke_share_inner(state.inner(), share_id).await
}

/// Inner of [`revoke_share`] taking `&AppState` so bulk callers (`revoke_shares_for_folder`) can reuse
/// the exact link/user revoke path (server revoke → local `revoked` → content-free ledger).
pub(crate) async fn revoke_share_inner(state: &AppState, share_id: String) -> Result<(), AppError> {
    let (expected_owner, mode, phase, rev) = state
        .db
        .outbound_share_cleanup_context(&share_id)?
        .ok_or_else(|| {
            AppError::Unavailable("the local share recovery witness is missing".into())
        })?;
    if expected_owner.is_empty() {
        return Err(AppError::Unavailable(
            "this legacy share has no durable owner witness; remote deletion cannot be proven"
                .into(),
        ));
    }
    let base = share_base_url(state)?;
    let (access, actor) = authenticated_org_actor(state).await?;
    if actor != expected_owner {
        return Err(AppError::Unavailable(
            "the sharing account changed; sign back into the share owner before revoking".into(),
        ));
    }
    let client = crate::share::client::ShareClient::new(&base)?;
    let mode = parse_outbound_share_mode(&mode)?;
    if phase == "create_pending" {
        require_share_owner_claim_capability(state, &client).await?;
        reserve_outbound_share_id(state, &client, &access, &share_id, &expected_owner, mode)
            .await?;
    }
    let delete_permit =
        permit_share_delete_dispatch(state, &client.host(), &share_id, &expected_owner, mode, rev)?;
    match client
        .revoke_share(
            &access,
            &share_id,
            &expected_owner,
            mode,
            rev,
            delete_permit,
        )
        .await?
    {
        crate::share::client::RevokeShareResult::Deleted => {}
        crate::share::client::RevokeShareResult::NotFound => {
            // The relay deliberately uses the same 404 for absence and wrong ownership. Even an
            // owner-scoped list absence is not a linearizable proof: a delayed create POST can
            // still land after that read. Keep the durable revoke intent and source closure until
            // the relay provides an owner-bound delete reservation/receipt contract.
            return Err(AppError::Unavailable(
                "remote share deletion could not be proven; retry after the relay confirms it"
                    .into(),
            ));
        }
    }
    state.db.set_outbound_share_state(&share_id, "revoked")?;
    Ok(())
}

/// Prove the configured relay advertises the owner-bound reservation/tombstone contract before a
/// client-minted share id or any ciphertext can be dispatched. This check is deliberately per
/// operation rather than cached: a mixed rollout may route consecutive requests to different pods,
/// and the reservation request itself remains the authoritative fail-closed boundary.
async fn require_share_owner_claim_capability(
    state: &AppState,
    client: &crate::share::client::ShareClient,
) -> Result<(), AppError> {
    let permit = permit_share_capability_read(state, &client.host())?;
    let health = client.health(permit).await?;
    if health.status != "ok"
        || !health
            .capabilities
            .iter()
            .any(|cap| cap == murmur_protocol::CAP_SHARE_OWNER_CLAIM_V1)
    {
        return Err(AppError::Unavailable(crate::errcode::tag(
            crate::errcode::SHARING_UPGRADE_REQUIRED,
            "the relay does not advertise owner-bound share reservations",
        )));
    }
    Ok(())
}

fn permit_share_capability_read(
    state: &AppState,
    host: &str,
) -> Result<ShareCapabilityReadPermit, AppError> {
    {
        let cfg = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
        if !cfg.share_egress_consented {
            return Err(AppError::Unavailable(crate::errcode::tag(
                crate::errcode::SHARE_CONSENT,
                "confirm the one-time upload notice first",
            )));
        }
    }
    let dispatch_id = uuid::Uuid::new_v4().to_string();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    let mut conn = state.db.lock();
    let tx = conn
        .transaction()
        .map_err(|_| AppError::Storage("start share capability dispatch".into()))?;
    crate::storage::db::insert_share_egress_dispatch_tx(
        &tx,
        ts,
        host,
        "share_capability_read",
        0,
        &dispatch_id,
    )?;
    tx.commit()
        .map_err(|_| AppError::Storage("commit share capability dispatch".into()))?;
    Ok(ShareCapabilityReadPermit {
        dispatch_id,
        host: host.to_string(),
        path: "/healthz",
        consumed: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    })
}

async fn reserve_outbound_share_id(
    state: &AppState,
    client: &crate::share::client::ShareClient,
    access_token: &str,
    share_id: &str,
    owner_user_id: &str,
    mode: murmur_protocol::dto::ShareMode,
) -> Result<ReservedShareId, AppError> {
    let permit = permit_share_reservation(state, &client.host(), share_id, owner_user_id, mode)?;
    client
        .reserve_share_id(access_token, share_id, owner_user_id, mode, permit)
        .await?;
    Ok(ReservedShareId {
        host: client.host(),
        share_id: share_id.to_string(),
        owner_user_id: owner_user_id.to_string(),
        mode,
    })
}

fn permit_share_reservation(
    state: &AppState,
    host: &str,
    share_id: &str,
    owner_user_id: &str,
    mode: murmur_protocol::dto::ShareMode,
) -> Result<ShareReservationPermit, AppError> {
    {
        let cfg = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
        if !cfg.share_egress_consented {
            return Err(AppError::Unavailable(crate::errcode::tag(
                crate::errcode::SHARE_CONSENT,
                "confirm the one-time upload notice first",
            )));
        }
    }
    if uuid::Uuid::parse_str(share_id).is_err() || owner_user_id.trim().is_empty() {
        return Err(AppError::InvalidArg(
            "share reservation witness is incomplete".into(),
        ));
    }
    let dispatch_id = uuid::Uuid::new_v4().to_string();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    let mut conn = state.db.lock();
    let tx = conn
        .transaction()
        .map_err(|_| AppError::Storage("start share reservation dispatch".into()))?;
    crate::storage::db::insert_share_egress_dispatch_tx(
        &tx,
        ts,
        host,
        "share_reserve",
        0,
        &dispatch_id,
    )?;
    tx.commit()
        .map_err(|_| AppError::Storage("commit share reservation dispatch".into()))?;
    Ok(ShareReservationPermit {
        dispatch_id,
        host: host.to_string(),
        share_id: share_id.to_string(),
        owner_user_id: owner_user_id.to_string(),
        mode,
        consumed: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    })
}

fn permit_share_content_dispatch(
    state: &AppState,
    reserved: ReservedShareId,
    source: &OrgShareSourceVersion,
    request: &murmur_protocol::dto::CreateShareRequest,
    kind: &str,
    byte_count: usize,
) -> Result<(ShareContentDispatchPermit, [u8; 32]), AppError> {
    if reserved.share_id != request.share_id || reserved.mode != request.mode {
        return Err(AppError::Storage(
            "reserved share id does not match content dispatch".into(),
        ));
    }
    let source_commitment = share_source_commitment(source);
    let request_commitment = share_content_request_commitment(request)?;
    let dispatch_id = uuid::Uuid::new_v4().to_string();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    if !state.db.persist_outbound_content_dispatch(
        &reserved.share_id,
        &reserved.owner_user_id,
        share_mode_label(reserved.mode),
        request.rev,
        &dispatch_id,
        &request_commitment,
        &source_commitment,
        ts,
        &reserved.host,
        kind,
        byte_count,
    )? {
        return Err(AppError::Unavailable(
            "outbound share changed before content dispatch".into(),
        ));
    }
    Ok((
        ShareContentDispatchPermit {
            dispatch_id,
            host: reserved.host,
            share_id: reserved.share_id,
            owner_user_id: reserved.owner_user_id,
            mode: reserved.mode,
            rev: request.rev,
            source_commitment,
            request_commitment,
            consumed: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        },
        source_commitment,
    ))
}

fn permit_share_delete_dispatch(
    state: &AppState,
    host: &str,
    share_id: &str,
    owner_user_id: &str,
    mode: murmur_protocol::dto::ShareMode,
    rev: u32,
) -> Result<ShareDeleteDispatchPermit, AppError> {
    let dispatch_id = uuid::Uuid::new_v4().to_string();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    if !state.db.persist_outbound_delete_dispatch(
        share_id,
        owner_user_id,
        share_mode_label(mode),
        rev,
        &dispatch_id,
        ts,
        host,
    )? {
        return Err(AppError::Unavailable(
            "outbound share changed before delete dispatch".into(),
        ));
    }
    Ok(ShareDeleteDispatchPermit {
        dispatch_id,
        host: host.to_string(),
        share_id: share_id.to_string(),
        owner_user_id: owner_user_id.to_string(),
        mode,
        rev,
        consumed: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    })
}

fn parse_outbound_share_mode(mode: &str) -> Result<murmur_protocol::dto::ShareMode, AppError> {
    match mode {
        "link" => Ok(murmur_protocol::dto::ShareMode::Link),
        "user" => Ok(murmur_protocol::dto::ShareMode::User),
        _ => Err(AppError::Storage(
            "outbound cleanup journal has an invalid share mode".into(),
        )),
    }
}

fn share_mode_label(mode: murmur_protocol::dto::ShareMode) -> &'static str {
    match mode {
        murmur_protocol::dto::ShareMode::Link => "link",
        murmur_protocol::dto::ShareMode::User => "user",
    }
}

/// A reservation or content POST may have committed even when the client saw an error. Preserve the
/// exact id as `revoke_pending`, then try the owner-authenticated terminal DELETE once. Failure is
/// intentionally retained for startup/background retry; no caller may infer absence from 404.
async fn retire_ambiguous_outbound_share(
    state: &AppState,
    client: &crate::share::client::ShareClient,
    access_token: &str,
    share_id: &str,
) -> Result<(), AppError> {
    let (owner_user_id, mode, _, rev) = state
        .db
        .outbound_share_cleanup_context(share_id)?
        .ok_or_else(|| {
            AppError::Unavailable("the local share recovery witness is missing".into())
        })?;
    let mode = parse_outbound_share_mode(&mode)?;
    let permit =
        permit_share_delete_dispatch(state, &client.host(), share_id, &owner_user_id, mode, rev)?;
    match client
        .revoke_share(access_token, share_id, &owner_user_id, mode, rev, permit)
        .await?
    {
        crate::share::client::RevokeShareResult::Deleted => {
            state.db.set_outbound_share_state(share_id, "revoked")?;
            Ok(())
        }
        crate::share::client::RevokeShareResult::NotFound => Err(AppError::Unavailable(
            "remote share deletion could not be proven; retry after the relay confirms it".into(),
        )),
    }
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
    let _mutation = state.lock_org_mutation().await;
    share_note_to_user_inner(state.inner(), meeting_id, recipient_email, expires_days).await
}

/// Core of [`share_note_to_user`] over `&AppState`. Gate order is normative — DO NOT reorder.
pub(crate) async fn share_note_to_user_inner(
    state: &AppState,
    meeting_id: String,
    recipient_email: String,
    expires_days: Option<u32>,
) -> Result<ShareToUserResult, AppError> {
    // (1) Gated lifecycle snapshot. Recipient lookup can await, but the upload below is refused if
    // any seal/relock or folder move invalidates this plaintext snapshot in the meantime.
    let source = build_org_share_snapshot(state, Some(&meeting_id), None, false)?;

    // (2) consent (fail-closed, first-ever share) + login (needs MK to derive sk_sig for the grant).
    let base = {
        let cfg = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
        if !cfg.share_egress_consented {
            return Err(AppError::Unavailable(crate::errcode::tag(
                crate::errcode::SHARE_CONSENT,
                "confirm the one-time upload notice first",
            )));
        }
        cfg.share_base_url.clone()
    };
    let (account_id, generation, mk, access_token) = require_session_mk(state).await?;
    let owner_user_id = {
        let session = state
            .account_session
            .lock()
            .map_err(|_| AppError::Storage("account-session mutex poisoned".into()))?;
        let session = crate::share::require_login(&session)?;
        if session.access_token != access_token {
            return Err(AppError::Unavailable(
                "sharing account changed while preparing the share".into(),
            ));
        }
        session.server_user_id.clone().ok_or_else(|| {
            AppError::Unavailable("sign out and sign back in before sharing".into())
        })?
    };
    let client = crate::share::client::ShareClient::new(&base)?;
    let share_id = crate::share::new_share_id();
    let rev = 1u32;
    require_current_org_share_snapshot(state, Some(&meeting_id), None, &source.source_version)?;
    // Capability discovery FIRST, then mint the owner-bound cleanup authority. Discovery is a pure
    // read (`client.health` -> GET /healthz) that mutates nothing remote, so a journal row written
    // ahead of it could only ever describe work that never started.
    //
    // Writing it first permanently locked the source out (2.0 audit). Against a relay that does not
    // advertise the capability: the first click journalled a 'create_pending' row and then failed;
    // the second failed EARLIER and DIFFERENTLY, on `insert_outbound_share_attempt` returning false,
    // with the bare untagged "an interrupted link-share cleanup is already pending"; and the only
    // recovery path, `revoke_share_inner`, needs the SAME missing capability, so it could never
    // converge. Two clicks and that source could not be shared again without a DB repair.
    //
    // The guarantee the original ordering existed for is untouched: once the capability IS present
    // the journal row is still written before the content POST, so a POST failing mid-flight still
    // leaves a visible, retryable local journal rather than losing the id.
    require_share_owner_claim_capability(state, &client).await?;
    if !state.db.insert_outbound_share_attempt(
        &share_id,
        Some(&meeting_id),
        None,
        "user",
        rev,
        &owner_user_id,
        &chrono::Utc::now().to_rfc3339(),
    )? {
        return Err(AppError::Unavailable(
            "an interrupted person-share cleanup is already pending; retry it before sharing again"
                .into(),
        ));
    }

    let OrgShareBodySnapshot {
        title,
        markdown: clean_body,
        created_at,
        counts: _,
        kind: _,
        attachment_owner,
        source_version,
    } = source;

    let nk = crate::e2ee::random_key32()?;
    let env =
        share_envelope_with_attachments(state, &attachment_owner, title, clean_body, created_at)?;
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
            (recipients, "invited".to_string(), None)
        };

    // (5) Upload — mode='user'; the link fields are unused (empty). NO note content/title in the body.
    let create_req =
        assemble_user_share_request(&share_id, rev, content_cell.clone(), recipients, expires_at);
    require_current_org_share_snapshot(state, Some(&meeting_id), None, &source_version)?;
    // Enrich the already-durable journal with SQLCipher-protected retry material before the
    // reservation socket. The phase stays `create_pending`; only an unambiguous 201 advances it to
    // `sent`/`awaiting_key`. Recovery never redispatches this content.
    if !state.db.prepare_outbound_user_share_attempt(
        &share_id,
        &meeting_id,
        rev,
        &nk_wrapped,
        &recipient_acct,
        &recipient_email,
        &content_hash,
        &owner_user_id,
    )? {
        return Err(AppError::Unavailable(
            "the outbound share attempt changed before reservation".into(),
        ));
    }
    let reserved = reserve_outbound_share_id(
        state,
        &client,
        &access_token,
        &share_id,
        &owner_user_id,
        murmur_protocol::dto::ShareMode::User,
    )
    .await?;
    if let Err(error) =
        require_current_org_share_snapshot(state, Some(&meeting_id), None, &source_version)
    {
        let _ = retire_ambiguous_outbound_share(state, &client, &access_token, &share_id).await;
        return Err(error);
    }
    let (content_permit, source_commitment) = permit_share_content_dispatch(
        state,
        reserved,
        &source_version,
        &create_req,
        if status == "sent" {
            "share_user_send"
        } else {
            "share_user_invite"
        },
        content_cell.len(),
    )?;
    if let Err(error) = client
        .create_share(
            &access_token,
            &owner_user_id,
            source_commitment,
            create_req,
            content_permit,
        )
        .await
    {
        let _ = retire_ambiguous_outbound_share(state, &client, &access_token, &share_id).await;
        return Err(error);
    }
    state.db.set_outbound_share_state(
        &share_id,
        if status == "sent" {
            "sent"
        } else {
            "awaiting_key"
        },
    )?;

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
    let _mutation = state.lock_org_mutation().await;
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
    app: AppHandle,
    state: State<'_, AppState>,
    share_id: String,
    folder_id: Option<String>,
) -> Result<AcceptedShare, AppError> {
    accept_share_inner_with_app(state.inner(), share_id, folder_id, Some(&app)).await
}

#[cfg(test)]
pub(crate) async fn accept_share_inner(
    state: &AppState,
    share_id: String,
    folder_id: Option<String>,
) -> Result<AcceptedShare, AppError> {
    accept_share_inner_with_app(state, share_id, folder_id, None).await
}

async fn accept_share_inner_with_app(
    state: &AppState,
    share_id: String,
    folder_id: Option<String>,
    app: Option<&AppHandle>,
) -> Result<AcceptedShare, AppError> {
    // (1) IDEMPOTENT on share_id — a re-accept returns the existing meeting, never a duplicate note.
    if let Some(mid) = state.db.inbound_share_meeting(&share_id)? {
        let title = {
            let _lifecycle = lifecycle_guard(state);
            if state.db.get_meeting_gate_anchor(&mid)?.is_none() {
                "Shared note".to_string()
            } else if !meeting_is_unlocked(state, &mid)? {
                "🔒 Locked".to_string()
            } else {
                state
                    .db
                    .get_meeting(&mid)?
                    .and_then(|meeting| meeting.title)
                    .unwrap_or_else(|| "Shared note".to_string())
            }
        };
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
        return resume_pending_accept(state, pending, app).await;
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
        app,
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
    app: Option<&AppHandle>,
) -> Result<AcceptedShare, AppError> {
    let content_cell = client.get_blob(access, blob_id).await?;
    let result = accept_ingest_verified_with_app(
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
        app,
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
    app: Option<&AppHandle>,
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
        app,
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
#[cfg(test)]
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
    accept_ingest_verified_with_app(
        state,
        target,
        recipient,
        sender_fp,
        sender_user_id,
        up,
        content_cell,
        share_id,
        rev,
        key_generation,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn accept_ingest_verified_with_app(
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
    app: Option<&AppHandle>,
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
    ingest_shared_note_with_app(
        state,
        target,
        &env,
        sender_fp,
        sender_user_id,
        share_id,
        app,
    )
}

/// Write a VERIFIED shared note into the vault + DB: a new `Exported` meeting (audio `None`) + a
/// `"shared"` note carrying `shared-by`/`shared-at`/`share-id` provenance frontmatter, atomically
/// exported to the folder's vault subdir, and an `inbound_shares` idempotency record. The new meeting
/// is a NORMAL row → it participates in every existing gate automatically.
#[cfg(test)]
pub(crate) fn ingest_shared_note(
    state: &AppState,
    target: &Folder,
    env: &murmur_protocol::envelope::ShareEnvelope,
    sender_fp: &str,
    sender_user_id: &str,
    share_id: &str,
) -> Result<AcceptedShare, AppError> {
    ingest_shared_note_with_app(
        state,
        target,
        env,
        sender_fp,
        sender_user_id,
        share_id,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn ingest_shared_note_with_app(
    state: &AppState,
    target: &Folder,
    env: &murmur_protocol::envelope::ShareEnvelope,
    sender_fp: &str,
    sender_user_id: &str,
    share_id: &str,
    app: Option<&AppHandle>,
) -> Result<AcceptedShare, AppError> {
    // Authenticate and validate the complete manifest before the first DB/vault write. Wire ids
    // are remapped so accepting the same payload twice cannot collide with another local owner.
    let (local_markdown, incoming_attachments) =
        prepare_incoming_attachment_bundle(&env.markdown, &env.attachments)?;
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
        local_markdown
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

    // The meeting shell above is born with canonical `meetings.folder_id`. Write its provider note
    // into the same target so canonical and legacy note-level ownership stay synchronized.
    // Keep SQLite canonical and delay vault export until all referenced image files exist.
    let mut note = NoteRecord {
        meeting_id: meeting_id.clone(),
        provider_id: "shared".to_string(),
        markdown: full_md,
        created_at: now.clone(),
        exported_path: None,
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
        // Keep every provider row synchronized with canonical meeting placement for legacy readers.
        state.db.set_note_folder(&meeting_id, Some(&target.id))?;
    }

    let attachment_owner = crate::storage::AttachmentOwner::Meeting {
        meeting_id: meeting_id.clone(),
        provider_id: note.provider_id.clone(),
    };
    if let Err(e) = crate::commands::materialize_attachment_bundle_under_lifecycle(
        state,
        &attachment_owner,
        &incoming_attachments,
    ) {
        // This meeting was minted by this ingest and has not been exported. Roll it back so a
        // residual DB failure cannot leave a note with broken private markers.
        // Retrieval can run off-thread after releasing the lifecycle guard, so even this short-lived
        // row may have entered an in-flight Ask context. Delete + purge atomically, advance the
        // generation, and invalidate the renderer while this lifecycle interval is still held.
        bump_seal_epoch(state);
        let _ = rollback_unpublished_shared_meeting(state, &meeting_id);
        if let Some(app) = app {
            emit_ask_history_invalidated_fail_closed(app);
        }
        return Err(e);
    }

    if !target_locked {
        // Best-effort vault export: publish verified image files first, then rewritten Markdown.
        if let Some(vault) = config_vault(state) {
            let vault_root = std::path::Path::new(&vault);
            if let Ok(exported_markdown) =
                crate::commands::render_markdown_with_attachments_for_export_under_lifecycle_authorized(
                    state,
                    &attachment_owner,
                    &note.markdown,
                    vault_root,
                )
            {
                if let Ok(path) = crate::export::write_note(
                    vault_root,
                    Some(&target.path),
                    &title,
                    &started_at,
                    &exported_markdown,
                ) {
                    note.exported_path = Some(path.to_string_lossy().to_string());
                    state.db.upsert_note(&note)?;
                    state.db.set_note_folder(&meeting_id, Some(&target.id))?;
                }
            }
        }
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

pub(crate) fn rollback_unpublished_shared_meeting(
    state: &AppState,
    meeting_id: &str,
) -> Result<(), AppError> {
    let mut conn = state.db.lock();
    let tx = conn
        .transaction()
        .map_err(|error| AppError::Storage(format!("begin unpublished rollback: {error}")))?;
    tx.execute(
        "DELETE FROM meetings WHERE id = ?1",
        rusqlite::params![meeting_id],
    )
    .map_err(|error| AppError::Storage(format!("rollback unpublished shared meeting: {error}")))?;
    crate::storage::Db::purge_all_ask_conversations_tx(&tx)?;
    tx.commit()
        .map_err(|error| AppError::Storage(format!("commit unpublished rollback: {error}")))?;
    Ok(())
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
    pub attachment_count: u32,
    pub attachment_bytes: u64,
    /// Text scrubbing does not mutate image pixels.
    pub image_pixels_scrubbed: bool,
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
    pub access: String,
    /// Content-free terminal CAS state. The raw storage error never crosses IPC.
    pub conflicted: bool,
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
pub(crate) fn org_author_hint(email: &str) -> String {
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
/// Redact a short, non-markdown string — a container name — with the SAME redactor the note body
/// passes through. A folder can be called "invoices for jan@acme.com", and a name that crosses to
/// another member's device is egress like any other.
pub(crate) fn scrub_org_text(text: &str) -> String {
    let (scrubbed, _map) = crate::summarize::redact::redact(text);
    scrubbed
}

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

fn add_scrub_counts(total: &mut OrgScrubCounts, next: &OrgScrubCounts) {
    total.emails += next.emails;
    total.phones += next.phones;
    total.cards += next.cards;
}

/// Redact only Task free-text fields while preserving the typed JSON document, stable ids,
/// status/dates, same-org references, and attachment UUIDs byte-for-byte. Running the Markdown
/// scrubber over the serialized JSON would corrupt those protocol fields.
pub(crate) fn scrub_task_envelope_json(
    markdown: &str,
    org_id: &str,
) -> Result<
    (
        crate::share::task_envelope::TaskEnvelope,
        String,
        OrgScrubCounts,
    ),
    AppError,
> {
    let mut task = crate::share::task_envelope::TaskEnvelope::from_json(markdown, org_id)?;
    let mut counts = OrgScrubCounts::default();
    let mut scrub = |value: &str| {
        let (value, next) = scrub_org_markdown(value);
        add_scrub_counts(&mut counts, &next);
        value
    };

    task.title = scrub(&task.title);
    task.description = scrub(&task.description);
    for subtask in &mut task.subtasks {
        subtask.title = scrub(&subtask.title);
    }
    for image in &mut task.images {
        let token = image.reference.trim();
        let body = token
            .strip_prefix("![")
            .and_then(|value| value.strip_suffix(')'))
            .ok_or_else(|| AppError::InvalidArg("task image reference is invalid".into()))?;
        let (label, attachment_id) = body
            .split_once("](murmur-attachment://")
            .ok_or_else(|| AppError::InvalidArg("task image reference is invalid".into()))?;
        let redacted_label = scrub(label);
        image.reference = format!("![{redacted_label}](murmur-attachment://{attachment_id})");
        image.alt = scrub(&image.alt);
    }
    let canonical = task.to_canonical_json(org_id)?;
    Ok((task, canonical, counts))
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
pub(crate) async fn acquire_org_ock(
    state: &AppState,
    org_id: &str,
    generation: u32,
) -> Result<zeroize::Zeroizing<[u8; 32]>, AppError> {
    acquire_org_ock_with_policy(state, org_id, generation, OrgWorkPolicy::manual()).await
}

async fn acquire_org_ock_with_policy(
    state: &AppState,
    org_id: &str,
    generation: u32,
    policy: OrgWorkPolicy,
) -> Result<zeroize::Zeroizing<[u8; 32]>, AppError> {
    if !policy.is_current() {
        return Err(AppError::Unavailable(
            "background org key acquisition deferred for recording".into(),
        ));
    }
    let (admitted_membership_generation, admitted_seal_epoch) = {
        let _lifecycle = lifecycle_guard(state);
        let org = state
            .db
            .get_org_state(org_id)?
            .ok_or_else(|| AppError::Unavailable("org membership is no longer live".into()))?;
        if !org.context_enabled || generation == 0 || generation > org.generation {
            return Err(AppError::Unavailable(
                "org key generation is not admissible for the live membership".into(),
            ));
        }
        (
            org.generation,
            state.seal_epoch.load(std::sync::atomic::Ordering::SeqCst),
        )
    };
    let membership_is_same = |state: &AppState| -> Result<bool, AppError> {
        Ok(state.db.get_org_state(org_id)?.is_some_and(|org| {
            org.context_enabled
                && org.generation == admitted_membership_generation
                && state.seal_epoch.load(std::sync::atomic::Ordering::SeqCst) == admitted_seal_epoch
        }))
    };
    // Cache hit? Membership/generation/context is the admission witness, not mere key presence.
    let cached = {
        let _lifecycle = lifecycle_guard(state);
        if !membership_is_same(state)? {
            return Err(AppError::Unavailable(
                "org membership is no longer live".into(),
            ));
        }
        let cache = state
            .org_ock_cache
            .lock()
            .map_err(|_| AppError::Storage("org-ock cache mutex poisoned".into()))?;
        cache
            .get(&(org_id.to_string(), generation))
            .map(|key| **key)
    };
    if let Some(key) = cached {
        if !policy.is_current() {
            return Err(AppError::Unavailable(
                "background org key acquisition deferred for recording".into(),
            ));
        }
        return Ok(zeroize::Zeroizing::new(key));
    }

    // Miss → unwrap from the caller's server-relayed grant. Needs the MK session (to derive our
    // identity keypair) + a valid bearer.
    let (account_id, gen_id, mk, access_token) = require_session_mk(state).await?;
    if !policy.is_current() {
        return Err(AppError::Unavailable(
            "background org key acquisition deferred for recording".into(),
        ));
    }
    // Grants are keyed by the server user id (UUID) — NOT the email `account_id`.
    let server_user_id = session_server_user_id(state)?;
    let base = share_base_url(state)?;
    let client = crate::share::client::ShareClient::new(&base)?;
    let recipient = crate::e2ee::keys::derive_identity(&mk, &account_id, gen_id)?;
    let self_fp = crate::e2ee::key_fingerprint(&recipient.pk_enc, &recipient.pk_sig);

    let grants = client.org_get_key_grants(&access_token, org_id).await?;
    if !policy.is_current() {
        return Err(AppError::Unavailable(
            "background org key acquisition deferred for recording".into(),
        ));
    }
    {
        let _lifecycle = lifecycle_guard(state);
        if !membership_is_same(state)? {
            return Err(AppError::Unavailable(
                "org membership changed during key fetch".into(),
            ));
        }
    }
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
    let granter_fp = crate::e2ee::key_fingerprint(&unpacked.sender_pk_enc, &unpacked.sender_pk_sig);
    match tofu_check(&state.db, &granter_fp, &granter_fp)? {
        TofuState::Changed => {
            return Err(AppError::Auth(
                "the org key granter's identity changed — re-verify before trusting new keys"
                    .into(),
            ));
        }
        _ => {
            let now = chrono::Utc::now().to_rfc3339();
            if policy
                .commit(|| state.db.pin_contact(&granter_fp, None, &granter_fp, &now))?
                .is_none()
            {
                return Err(AppError::Unavailable(
                    "background org key acquisition deferred for recording".into(),
                ));
            }
        }
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

    // Cache in RAM for the session, but never after a scheduled tick lost its recording epoch.
    if policy
        .commit(|| {
            let _lifecycle = lifecycle_guard(state);
            if !membership_is_same(state)? {
                return Err(AppError::Unavailable(
                    "org membership changed before key admission".into(),
                ));
            }
            let mut cache = state
                .org_ock_cache
                .lock()
                .map_err(|_| AppError::Storage("org-ock cache mutex poisoned".into()))?;
            cache.insert(
                (org_id.to_string(), generation),
                zeroize::Zeroizing::new(*ock),
            );
            Ok(())
        })?
        .is_none()
    {
        return Err(AppError::Unavailable(
            "background org key acquisition deferred for recording".into(),
        ));
    }
    Ok(zeroize::Zeroizing::new(*ock))
}

#[cfg(test)]
pub(crate) async fn acquire_org_ock_for_test(
    state: &AppState,
    org_id: &str,
    generation: u32,
) -> Result<zeroize::Zeroizing<[u8; 32]>, AppError> {
    acquire_org_ock_with_policy(state, org_id, generation, OrgWorkPolicy::manual()).await
}

/// `org_create(name)` — create an org (caller becomes owner), then generate + self-grant the OCK so
/// the owner can immediately seal items. Caches the org + generation-1 OCK locally.
#[tauri::command]
pub async fn org_create(state: State<'_, AppState>, name: String) -> Result<OrgStatus, AppError> {
    let _mutation = state.lock_org_mutation().await;
    org_create_inner(state.inner(), name).await
}

pub(crate) async fn org_create_inner(
    state: &AppState,
    name: String,
) -> Result<OrgStatus, AppError> {
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

    org_status_inner(state).await.map(|o| {
        o.unwrap_or(OrgStatus {
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
        })
    })
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
        match org_status_for(state, local.clone()).await {
            Ok(status) => out.push(status),
            Err(AppError::Unavailable(_)) => {
                // One org may disappear/race or be temporarily unavailable while every other org is
                // healthy. Keep the aggregate usable from its local, content-free cached metadata.
                if let Some(cached) = state.db.get_org_state(&local.org_id)? {
                    let _lifecycle = lifecycle_guard(state);
                    out.push(org_status_from_local(state, cached, 1)?);
                }
            }
            Err(error) => return Err(error),
        }
    }
    Ok(out)
}

/// `org_list_cached_statuses()` — the local-replica-only counterpart to
/// [`org_list_statuses`]. It performs no token refresh, HTTP request, or membership
/// reconciliation; it only renders the currently admitted `org_state` rows and
/// their local counts. Passive navigation surfaces use this command so merely
/// opening Shared Brains never creates network egress.
#[tauri::command]
pub fn org_list_cached_statuses(
    state: State<'_, AppState>,
) -> Result<Vec<OrgStatus>, AppError> {
    org_list_cached_statuses_inner(state.inner())
}

pub(crate) fn org_list_cached_statuses_inner(
    state: &AppState,
) -> Result<Vec<OrgStatus>, AppError> {
    let _lifecycle = lifecycle_guard(state);
    state
        .db
        .list_org_states()?
        .into_iter()
        .map(|local| org_status_from_local(state, local, 1))
        .collect()
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
    app: AppHandle,
    state: State<'_, AppState>,
    org_id: String,
    enabled: bool,
) -> Result<(), AppError> {
    org_set_context_enabled_notifying(state.inner(), &org_id, enabled, Some(&app))
}

#[cfg(test)]
pub(crate) fn org_set_context_enabled_inner(
    state: &AppState,
    org_id: &str,
    enabled: bool,
) -> Result<(), AppError> {
    org_set_context_enabled_notifying(state, org_id, enabled, None)
}

fn org_set_context_enabled_notifying(
    state: &AppState,
    org_id: &str,
    enabled: bool,
    app: Option<&AppHandle>,
) -> Result<(), AppError> {
    resolve_org(state, org_id)?; // membership re-check
    let changed = if enabled {
        state.db.set_org_context_enabled(org_id, true)?
    } else {
        commit_org_visibility_reduction(
            state,
            app.map(|app| app as &dyn AskHistoryInvalidationNotifier),
            || state.db.set_org_context_enabled(org_id, false),
        )?
    };
    // A Connections panel may be kept alive in another tab. Clear/refetch its org endpoints
    // immediately when this local visibility gate changes.
    notify_org_views_if_changed(
        app.map(|app| app as &dyn crate::events::OrgFeedNotifier),
        changed,
    );
    Ok(())
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
    let (original_generation, original_context, original_seal_epoch) = {
        let _lifecycle = lifecycle_guard(state);
        (
            local.generation,
            local.context_enabled,
            state.seal_epoch.load(std::sync::atomic::Ordering::SeqCst),
        )
    };
    // Refresh membership/generation from the server when logged in (best-effort — offline shows the
    // cached row).
    let member_count = match (share_base_url(state), valid_access_token(state).await) {
        (Ok(base), Ok(access)) if !base.trim().is_empty() => {
            let client = crate::share::client::ShareClient::new(&base)?;
            match client.org_status(&access, &local.org_id).await {
                Ok(fresh) => {
                    let _lifecycle = lifecycle_guard(state);
                    if fresh.org_id != local.org_id {
                        return Err(AppError::Unavailable(
                            "org status returned a mismatched membership".into(),
                        ));
                    }
                    let still_same = state.db.get_org_state(&local.org_id)?.is_some_and(|held| {
                        held.generation == original_generation
                            && held.context_enabled == original_context
                            && state.seal_epoch.load(std::sync::atomic::Ordering::SeqCst)
                                == original_seal_epoch
                    });
                    if !still_same {
                        return Err(AppError::Unavailable(
                            "org membership changed during status refresh".into(),
                        ));
                    }
                    state.db.upsert_org_state(&crate::storage::OrgState {
                        org_id: fresh.org_id.clone(),
                        name: fresh.name.clone(),
                        role: fresh.role.clone(),
                        joined_at: local.joined_at.clone(),
                        consented: local.consented,
                        last_seq: local.last_seq,
                        generation: fresh.current_generation,
                        context_enabled: original_context,
                    })?;
                    state
                        .db
                        .set_org_generation(&local.org_id, fresh.current_generation)?;
                    drop(_lifecycle);
                    let count = client
                        .org_list_members(&access, &local.org_id)
                        .await
                        .map(|m| m.members.len() as u32)
                        .unwrap_or(1);
                    let _lifecycle = lifecycle_guard(state);
                    let still_same = state.db.get_org_state(&local.org_id)?.is_some_and(|held| {
                        held.generation == fresh.current_generation
                            && held.context_enabled == original_context
                            && state.seal_epoch.load(std::sync::atomic::Ordering::SeqCst)
                                == original_seal_epoch
                    });
                    if !still_same {
                        return Err(AppError::Unavailable(
                            "org membership changed during member refresh".into(),
                        ));
                    }
                    count
                }
                Err(_) => 1,
            }
        }
        _ => 1,
    };
    let _lifecycle = lifecycle_guard(state);
    let refreshed = state.db.get_org_state(&local.org_id)?.ok_or_else(|| {
        AppError::Unavailable("org membership was removed during status refresh".into())
    })?;
    org_status_from_local(state, refreshed, member_count)
}

fn org_status_from_local(
    state: &AppState,
    refreshed: crate::storage::OrgState,
    member_count: u32,
) -> Result<OrgStatus, AppError> {
    let shares = state.db.list_org_shares_for_org(&refreshed.org_id)?;
    let mut pending = 0u32;
    let mut item_count = 0u32;
    for share in &shares {
        if !org_share_source_is_visible(state, share)? {
            continue;
        }
        if share.state == "queued" || share.state == "revoke_pending" {
            pending += 1;
        }
        if share.state == "uploaded" || (share.state == "failed" && share.item_id.is_some()) {
            item_count += 1;
        }
    }
    // `item_count` also counts a `failed`-with-`item_id` row: `set_org_share_failed` never clears
    // `item_id` on a republish failure, so such a row's PRIOR publish is still genuinely live on the
    // server — excluding it undercounted "N shared by you" for exactly the rows stuck by the
    // republish-failure bug (see `org_shares_for_source`'s doc for the full state-machine rationale).
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
pub async fn org_refresh(app: AppHandle, state: State<'_, AppState>) -> Result<(), AppError> {
    // BOUNDED. This used to be a bare `.lock().await` — the only wait on the whole org-panel path
    // with no timeout, taken BEFORE any timeout-protected code. A wedged holder therefore produced
    // an unbounded "Loading organizations…" that no error path could reach: the frontend clears its
    // loading flag in a `finally`, and a `finally` does not run for a future that never settles.
    // Logging out did not help either, because a mutex is process state — only a restart cleared it.
    //
    // Refusing is strictly better than waiting: the caller already treats a failed refresh as
    // non-fatal and falls through to the local replica, so the panel renders either way.
    let _mutation = acquire_share_mutation_within(state.inner(), SHARE_MUTATION_WAIT).await?;
    org_reconcile_memberships_notifying(state.inner(), Some(&app)).await
}

/// What a reconcile should do with the result of [`valid_access_token`].
///
/// Pure, so the policy can be asserted directly rather than inferred from a live session.
pub(crate) enum TokenOutcome {
    /// A live bearer — go on and talk to the server.
    Proceed(String),
    /// The session is unrecoverable and the user must sign in again. This MUST reach them: the
    /// reconcile used to answer `Ok(())` here, which rendered a permanently dead session as an
    /// empty panel indistinguishable from being offline.
    Fatal(AppError),
    /// Offline, logged out, a 5xx, a keychain hiccup — expected and transient. Keep the cached
    /// rows and try again next tick; never turn this into an error banner.
    SkipQuietly,
}

/// Classify a token result for a reconcile. Only a definitive `Auth` refusal is fatal — that is the
/// same line [`valid_access_token`] already draws internally, where `refresh_failure_fallback`
/// propagates `Auth` and falls back to the cached bearer for everything else.
pub(crate) fn reconcile_token_outcome(result: Result<String, AppError>) -> TokenOutcome {
    match result {
        Ok(token) => TokenOutcome::Proceed(token),
        Err(e @ AppError::Auth(_)) => TokenOutcome::Fatal(e),
        Err(_) => TokenOutcome::SkipQuietly,
    }
}

/// How long a read-side refresh waits for an in-flight sharing mutation before giving up. Long
/// enough for a normal mutation (its own network calls are capped at 30 s) to finish and hand over;
/// short enough that the panel never looks dead.
pub(crate) const SHARE_MUTATION_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// Acquire `org_share_mutation_lock`, or refuse once `wait` elapses.
pub(crate) async fn acquire_share_mutation_within(
    state: &AppState,
    wait: std::time::Duration,
) -> Result<crate::state::OrgMutationGuard<'_>, AppError> {
    // Through the guard like every other door: this bounded-wait variant is a SECOND way to
    // take the same mutex, and a re-entrancy check that misses it misses exactly the kind of
    // path the deadlock it exists for was reached through.
    match state.lock_org_mutation_within(wait).await {
        Some(guard) => Ok(guard),
        None => Err(AppError::Unavailable(crate::errcode::tag(
            crate::errcode::SHARE_BUSY,
            "another sharing operation is still running",
        ))),
    }
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
/// - REMOVE every local org NOT in a non-empty authenticated server list. An all-empty aggregate is
///   destructive only after every cached org's exact status route independently confirms 404; otherwise
///   cached rows and private local links are preserved fail-safe.
///
/// Offline / not-logged-in = NO-OP: the cached rows are kept untouched (never destructive on a
/// transient network failure). No PII in logs — ids/counts only.
pub(crate) async fn org_reconcile_memberships_notifying(
    state: &AppState,
    app: Option<&AppHandle>,
) -> Result<(), AppError> {
    org_reconcile_memberships_with_policy(state, OrgWorkPolicy::manual(), app).await
}

async fn org_reconcile_memberships_with_policy(
    state: &AppState,
    policy: OrgWorkPolicy,
    app: Option<&AppHandle>,
) -> Result<(), AppError> {
    if !policy.is_current() {
        return Ok(());
    }
    // Logged out / no server ⇒ keep the cached rows, do nothing (not an error).
    let base = match share_base_url(state) {
        Ok(b) if !b.trim().is_empty() => b,
        _ => return Ok(()),
    };
    let access = match reconcile_token_outcome(valid_access_token(state).await) {
        TokenOutcome::Proceed(a) => a,
        TokenOutcome::Fatal(e) => return Err(e),
        TokenOutcome::SkipQuietly => return Ok(()),
    };
    if !policy.is_current() {
        return Ok(());
    }
    let client = crate::share::client::ShareClient::new(&base)?;
    // A pull failure (network/5xx) is best-effort: keep the cached rows, retry next tick.
    let mut server_orgs = match client.org_list(&access).await {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!(target: "org", error = %brief_err(&e), "org membership pull failed — keeping cached rows");
            return Ok(());
        }
    };
    if !policy.is_current() {
        return Ok(());
    }

    // An authenticated but all-empty aggregate response is not, by itself, enough evidence to
    // destroy every local org replica/private link. Corroborate every cached membership through its
    // exact member-gated status route. A confirmed 404 remains omitted (authoritative removal);
    // success reconstructs the live row, and a transient/malformed per-org response preserves the
    // cached membership fail-safe for the next tick.
    let aggregate_was_empty = server_orgs.is_empty();
    if aggregate_was_empty {
        // The aggregate pull is an authenticated account refresh, but the additional per-org
        // corroboration is a distinct cloud call used to authorize destructive local cleanup.
        // Route that call through the canonical org-egress consent latch; without consent the empty
        // aggregate remains non-authoritative and every cached membership/private link is preserved.
        for local in state.db.list_org_states()? {
            if !policy.is_current() {
                return Ok(());
            }
            let corroboration_consented = state
                .config
                .lock()
                .map_err(|_| AppError::Config("config mutex poisoned".into()))?
                .org_egress_consented;
            if !corroboration_consented {
                return Ok(());
            }
            let permit = permit_simple_org_dispatch(
                state,
                &client.host(),
                "org_membership_corroborate",
                OrgDispatchOperation::MembershipCorroborate {
                    org_id: local.org_id.clone(),
                },
            )?;
            let corroboration = client
                .org_status_optional(&access, &local.org_id, permit)
                .await;
            match corroboration {
                Ok(Some(fresh)) => server_orgs.push(crate::share::org_dto::OrgSummary {
                    org_id: fresh.org_id,
                    name: fresh.name,
                    role: fresh.role,
                    created_at: fresh.created_at,
                    current_generation: fresh.current_generation,
                }),
                Ok(None) => {}
                Err(_) => server_orgs.push(crate::share::org_dto::OrgSummary {
                    org_id: local.org_id,
                    name: local.name,
                    role: local.role,
                    created_at: local.joined_at,
                    current_generation: local.generation,
                }),
            }
        }
    }
    let empty_corroborated = aggregate_was_empty && server_orgs.is_empty();

    // Apply the ADD/REMOVE against the local DB (pure, testable without a network) and learn which
    // orgs are NEW (so we can best-effort acquire their OCK) and which we dropped (to purge OCKs).
    let Some(outcome) = reconcile_org_state_into_db_with_policy(
        state,
        &server_orgs,
        empty_corroborated,
        policy,
        app,
    )?
    else {
        return Ok(());
    };

    // Membership discovery/removal changes which private org-link endpoints are readable even
    // when no feed row was ingested. Notify immediately after the atomic local mutation, before
    // any best-effort OCK await below: a cancelled/stale key fetch must not leave an open view
    // rendering an endpoint whose membership was already removed.
    notify_org_views_if_changed(
        app.map(|app| app as &dyn crate::events::OrgFeedNotifier),
        !outcome.new_orgs.is_empty() || outcome.removed > 0,
    );

    // Best-effort: acquire each newly-discovered org's OCK so its feed can later decrypt. A grant not
    // yet issued (the owner hasn't PUT our wrapped key) must NOT fail the whole reconcile.
    for (org_id, generation) in &outcome.new_orgs {
        if !policy.is_current() {
            return Ok(());
        }
        if let Err(e) = acquire_org_ock_with_policy(state, org_id, *generation, policy).await {
            if !policy.is_current() {
                return Ok(());
            }
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
/// the NEW orgs (for OCK acquisition by the caller) + the removed count. A bare empty slice is
/// deliberately non-destructive; only the corroborated network wrapper may authorize empty removal.
#[cfg(test)]
pub(crate) fn reconcile_org_state_into_db(
    state: &AppState,
    server_orgs: &[crate::share::org_dto::OrgSummary],
) -> Result<ReconcileOutcome, AppError> {
    match reconcile_org_state_into_db_with_policy(
        state,
        server_orgs,
        false,
        OrgWorkPolicy::manual(),
        None,
    )? {
        Some(outcome) => Ok(outcome),
        None => Err(AppError::Unavailable(
            "manual org membership reconciliation was unexpectedly deferred".into(),
        )),
    }
}

#[cfg(test)]
pub(crate) fn reconcile_org_state_into_db_corroborated_empty(
    state: &AppState,
) -> Result<ReconcileOutcome, AppError> {
    match reconcile_org_state_into_db_with_policy(state, &[], true, OrgWorkPolicy::manual(), None)?
    {
        Some(outcome) => Ok(outcome),
        None => Err(AppError::Unavailable(
            "manual org membership reconciliation was unexpectedly deferred".into(),
        )),
    }
}

fn reconcile_org_state_into_db_with_policy(
    state: &AppState,
    server_orgs: &[crate::share::org_dto::OrgSummary],
    empty_corroborated: bool,
    policy: OrgWorkPolicy,
    app: Option<&AppHandle>,
) -> Result<Option<ReconcileOutcome>, AppError> {
    if !policy.is_current() {
        return Ok(None);
    }
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
        if !policy.is_current() {
            return Ok(None);
        }
        let is_new = !known_before.contains(&o.org_id);
        let refreshed = crate::storage::OrgState {
            org_id: o.org_id.clone(),
            name: o.name.clone(),
            role: o.role.clone(),
            joined_at: o.created_at.clone(),
            consented: false,
            last_seq: 0,
            generation: o.current_generation,
            context_enabled: true,
        };
        if policy
            .commit(|| state.db.upsert_org_state(&refreshed))?
            .is_none()
        {
            return Ok(None);
        }
        if is_new {
            new_orgs.push((o.org_id.clone(), o.current_generation));
        }
    }

    // A bare empty aggregate is fail-safe: only the network caller can set `empty_corroborated`
    // after every cached org's exact status route independently confirmed absence.
    if server_orgs.is_empty() && !empty_corroborated {
        return Ok(Some(ReconcileOutcome {
            new_orgs,
            removed: 0,
        }));
    }

    // Remove local orgs the server no longer lists (left / removed) + purge their decrypted replica so
    // a departed org's docs don't linger in the local retrieval partition (the same purge `org_leave`
    // does — leak/consent invariant) + drop its cached OCKs.
    let mut removed = 0u32;
    for org_id in &known_before {
        if !server_ids.contains(org_id) {
            if !policy.is_current() {
                return Ok(None);
            }
            let Some(was_removed) = policy.commit(|| {
                let removed = commit_org_visibility_reduction(
                    state,
                    app.map(|app| app as &dyn AskHistoryInvalidationNotifier),
                    || {
                        let removed = state.db.delete_org_state(org_id)?;
                        let mut cache = state
                            .org_ock_cache
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        cache.retain(|(oid, _), _| oid != org_id);
                        Ok(removed)
                    },
                )?;
                Ok(removed)
            })?
            else {
                return Ok(None);
            };
            if was_removed {
                removed += 1;
            }
        }
    }

    Ok(Some(ReconcileOutcome { new_orgs, removed }))
}

/// Resolve the TARGETED org by id, MEMBERSHIP-CHECKED against the local `org_state`. The multi-org
/// fix: the FE passes the org the user picked, and every per-org command resolves THAT org (never the
/// first via `.next()`, which misrouted a destructive/egress op to org #1 on a multi-org account). A
/// blank id or an org we're not a local member of is an `InvalidArg` refusal — we never operate on an
/// org the caller isn't in.
pub(crate) fn resolve_org(
    state: &AppState,
    org_id: &str,
) -> Result<crate::storage::OrgState, AppError> {
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
    let _mutation = state.lock_org_mutation().await;
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

    // Look up and VERIFY the key BEFORE adding the member.
    //
    // The order matters, on review's finding. Adding first left a refused invite with an active
    // member row server-side, and the relay counts every active member toward a rotation's coverage
    // regardless of whether they hold a grant — so a refused invitee silently blocked every future
    // rotation until removed, and re-drove the rotation path against the very key just refused.
    // Verifying first means a refusal leaves no server-side trace at all.
    let lookup = client.lookup_key(&access_token, &email).await?;
    let key = lookup
        .key
        .filter(|_| lookup.registered)
        .ok_or_else(|| AppError::InvalidArg("that address is not a registered account".into()))?;

    // TOFU on the invitee's key, BEFORE the OCK is wrapped to it.
    //
    // `lookup_key` answers with whatever the relay says the address's key is. Without this check a
    // relay that substitutes `pk_enc` between the lookup and the wrap receives the org content key
    // wrapped to a key it holds — the exact substitution the mode-B share path already refuses
    // (`share_note_with_account`). This path skipped it, so the org's whole shared brain was one
    // dishonest lookup away from a stranger.
    //
    // First contact PINS. A change REFUSES and sends nothing: a changed key is either a legitimate
    // re-key or an attack, and the two are indistinguishable from here, so the only honest move is
    // to stop and ask the human to re-verify out of band.
    let member_fp_pre = crate::e2ee::key_fingerprint(&key.pk_enc, &key.pk_sig);
    match crate::commands::tofu_check(&state.db, &key.user_id, &member_fp_pre)? {
        crate::commands::TofuState::Changed => {
            return Err(AppError::Auth(crate::errcode::tag(
                crate::errcode::ORG_INVITE_KEY_CHANGED,
                "this person's key changed since you last invited them — re-verify the safety words \
                 with them out of band, then invite again",
            )));
        }
        _ => state.db.pin_contact(
            &key.user_id,
            Some(&email),
            &member_fp_pre,
            &chrono::Utc::now().to_rfc3339(),
        )?,
    }

    // Only now does the member row exist server-side.
    let added = client
        .org_add_member(&access_token, &org.org_id, &email)
        .await?;

    let generation = org.generation;
    let ock = acquire_org_ock(state, &org.org_id, generation).await?;
    let owner = crate::e2ee::keys::derive_identity(&mk, &account_id, gen_id)?;
    let owner_fp = crate::e2ee::key_fingerprint(&owner.pk_enc, &owner.pk_sig);
    // recipient_acct_id = the member's FINGERPRINT (the crypto binding THEY open with via `self_fp` in
    // `acquire_org_ock`); the DB grant key stays their server user id (`added.user_id`).
    let member_fp = crate::e2ee::key_fingerprint(&key.pk_enc, &key.pk_sig);
    // Remember the key this invite just resolved. A later rotation has to wrap the new OCK for
    // EVERY remaining member in one pass, and the only directory the relay offers is this same
    // email-keyed lookup, capped at 20 calls a day against orgs of up to 50 members — so a rotation
    // that re-looked-up everybody would fail on quota long before it failed on anything interesting.
    // Remembering it here also means rotation prefers the key this device already saw over whatever
    // the relay answers later, which is the strictly safer of the two.
    state.db.upsert_org_member_key(
        &org.org_id,
        &added.user_id,
        Some(&email),
        &key.pk_enc,
        &key.pk_sig,
        &member_fp,
    )?;
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
    org_list_members_inner(state.inner(), &org_id).await
}

pub(crate) async fn org_list_members_inner(
    state: &AppState,
    org_id: &str,
) -> Result<Vec<OrgMember>, AppError> {
    let org = resolve_org(state, org_id)?;
    let base = share_base_url(state)?;
    let access = valid_access_token(state).await?;
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

/// Task assignee lookup is a remote, content-free org read. Keep it on a dedicated typed seam so
/// Task commands cannot accidentally bypass the one-time org consent or issue an unledgered GET.
pub(crate) async fn org_task_list_members_inner(
    state: &AppState,
    org_id: &str,
) -> Result<Vec<OrgMember>, AppError> {
    super::tasks::require_task_read_context(state, org_id)?;
    require_org_egress_consent(state)?;
    let org = resolve_org(state, org_id)?;
    let base = share_base_url(state)?;
    let access = valid_access_token(state).await?;
    let client = crate::share::client::ShareClient::new(&base)?;
    let permit = permit_org_task_assignee_read(state, &client.host(), &org.org_id)?;
    let resp = task_org_members_read_with_permit(&client, &access, &org.org_id, permit).await?;
    Ok(resp
        .members
        .into_iter()
        .map(|member| OrgMember {
            user_id: member.user_id,
            email: member.email,
            role: member.role,
            added_at: member.created_at,
            removed: false,
        })
        .collect())
}

/// The only Task assignee socket boundary. Consuming the move-only permit here makes an
/// unconsented or unledgered Task lookup impossible even though ordinary Org administration has
/// its own member-list command.
async fn task_org_members_read_with_permit(
    client: &crate::share::client::ShareClient,
    access_token: &str,
    org_id: &str,
    permit: OrgMembersReadPermit,
) -> Result<crate::share::org_dto::OrgMembersResponse, AppError> {
    permit.authorize(&client.host(), org_id)?;
    client.org_list_members(access_token, org_id).await
}

/// Mint a new org content key, wrap it for EVERY remaining active member, and commit the new
/// generation.
///
/// # Why this is a whole function and not three lines inside the removal
///
/// The relay enforces the only rule that matters here: `POST /v1/orgs/{id}/generation` succeeds
/// only when a key grant for the new generation already exists for every member whose `removed_at`
/// is null, checked inside the same transaction that flips the generation. So a rotation is
/// all-or-nothing by construction, and a rotation that reaches only the owner is not a partial
/// success — it is a 409 and no rotation at all.
///
/// # Order
///
/// The removal MUST already have happened. While the departing member is still active the relay
/// counts them in the coverage check, so a "rotate first, then remove" ordering could only succeed
/// by granting the new key to the very person being removed. That is why the caller journals the
/// debt first and rotates second, rather than the other way round.
///
/// # Resolving keys
///
/// Each member's public key comes from this device's own `org_member_keys` cache when it has one,
/// and only otherwise from `POST /v1/keys/lookup` (which is then remembered). Preferring the cached
/// copy is deliberate: it means a relay cannot substitute a key for a member this device has
/// already met, and it keeps a 50-member org inside the 20-lookups-a-day quota.
///
/// Returns the generation the relay committed.
/// How many identity lookups one rotation attempt may spend. The relay allows 20 a day per
/// account against orgs of up to 50 members, so an org invited before member keys were remembered
/// cannot resolve everyone in one pass. Bounding it per attempt means a first rotation converges
/// over a few attempts instead of spending the whole day's quota — and leaving none for the
/// invites and shares the same quota pays for.
const ROTATION_LOOKUP_BUDGET: usize = 8;

/// What one rotation attempt achieved when it did not finish: whether it LEARNED a key it did not
/// have. A slow-but-progressing rotation must not be backed off; a doomed one must.
pub(crate) struct RotationProgress {
    pub learned_keys: usize,
}

pub(crate) async fn rotate_org_generation(
    state: &AppState,
    org_id: &str,
    progress: &mut RotationProgress,
) -> Result<u32, AppError> {
    let org = resolve_org(state, org_id)?;
    let base = share_base_url(state)?;
    let (account_id, gen_id, mk, access_token) = require_session_mk(state).await?;
    let server_user_id = session_server_user_id(state)?;
    let client = crate::share::client::ShareClient::new(&base)?;

    // The generation to target is the RELAY's current one, never the local cache: a device that
    // missed a rotation would otherwise aim at an already-used generation and take a 409 forever.
    let status = client.org_status(&access_token, &org.org_id).await?;
    let new_gen = status.current_generation.saturating_add(1);

    let members = client
        .org_list_members(&access_token, &org.org_id)
        .await?
        .members;
    if members.is_empty() {
        // The relay refuses a bump for a memberless org anyway; saying so here keeps the failure
        // legible instead of arriving as an opaque 409.
        return Err(AppError::Unavailable(crate::errcode::tag(
            crate::errcode::ORG_ROTATION_PENDING,
            "org has no active members to rotate to",
        )));
    }

    let new_ock = crate::e2ee::org::generate_ock()?;
    let owner = crate::e2ee::keys::derive_identity(&mk, &account_id, gen_id)?;
    let owner_fp = crate::e2ee::key_fingerprint(&owner.pk_enc, &owner.pk_sig);

    let mut grants: Vec<crate::share::org_dto::KeyGrantInput> = Vec::with_capacity(members.len());
    for member in &members {
        // (pk_enc, recipient fingerprint) for this member. Ours is derived locally; everyone
        // else's is remembered or, failing that, looked up once and then remembered.
        let (pk_enc, recipient_fp) = if member.user_id == server_user_id {
            (owner.pk_enc.to_vec(), owner_fp.clone())
        } else if let Some(known) = state.db.get_org_member_key(&org.org_id, &member.user_id)? {
            (known.pk_enc, known.fingerprint)
        } else {
            let Some(email) = member.email.as_deref().filter(|e| !e.trim().is_empty()) else {
                // Without an address there is no directory to ask. Refusing keeps the debt on the
                // journal instead of committing a generation somebody cannot open.
                return Err(AppError::Unavailable(crate::errcode::tag(
                    crate::errcode::ORG_ROTATION_PENDING,
                    "a remaining member's identity key is unknown and cannot be resolved",
                )));
            };
            if progress.learned_keys >= ROTATION_LOOKUP_BUDGET {
                return Err(AppError::Unavailable(crate::errcode::tag(
                    crate::errcode::ORG_ROTATION_PENDING,
                    "resolved this attempt's share of the remaining members' keys; \
                     the rotation continues on the next pass",
                )));
            }
            let lookup = client.lookup_key(&access_token, email).await?;
            let Some(key) = lookup.key.filter(|_| lookup.registered) else {
                return Err(AppError::Unavailable(crate::errcode::tag(
                    crate::errcode::ORG_ROTATION_PENDING,
                    "a remaining member is not a registered account",
                )));
            };
            let fp = crate::e2ee::key_fingerprint(&key.pk_enc, &key.pk_sig);
            // SAME gate as the invite path, and for a sharper reason: without it, gating the invite
            // alone was FALSE ASSURANCE. A refused invite still leaves the member row active
            // server-side, the relay's coverage check counts every active member regardless of
            // whether they hold a grant, so the next ordinary removal schedules a rotation that
            // re-drives this very branch — cache misses, ungated lookup fires, and the brand-new OCK
            // is wrapped to the same substituted key the invite had just refused. The attack simply
            // arrived one admin action later.
            //
            // The cached branch above needs no check because a cached key is one this device already
            // TOFU-verified. This branch is the cold-cache case: a second Mac, a reinstall, or an org
            // whose members predate the key cache — all of which land here.
            //
            // Refusing keeps the rotation OWED rather than granting to an unverified key. The debt is
            // journalled and retried, so a legitimate re-key resolves once a human has verified it,
            // while a substitution never silently completes.
            match crate::commands::tofu_check(&state.db, &member.user_id, &fp)? {
                crate::commands::TofuState::Changed => {
                    return Err(AppError::Auth(crate::errcode::tag(
                        crate::errcode::ORG_INVITE_KEY_CHANGED,
                        "a remaining member's key changed since you last verified it — re-verify the \
                         safety words with them out of band before this rotation can complete",
                    )));
                }
                _ => state.db.pin_contact(
                    &member.user_id,
                    Some(email),
                    &fp,
                    &chrono::Utc::now().to_rfc3339(),
                )?,
            }
            state.db.upsert_org_member_key(
                &org.org_id,
                &member.user_id,
                Some(email),
                &key.pk_enc,
                &key.pk_sig,
                &fp,
            )?;
            progress.learned_keys += 1;
            (key.pk_enc, fp)
        };

        let grant = crate::e2ee::org::wrap_ock_for_member(
            &new_ock,
            &org.org_id,
            new_gen,
            &pk_enc,
            // recipient_acct_id = the member's FINGERPRINT, matching what `acquire_org_ock` opens
            // with as `self_fp`. Keying it on anything else produces a grant nobody can open.
            &recipient_fp,
            &owner,
            &owner_fp,
            gen_id,
        )?;
        grants.push(crate::share::org_dto::KeyGrantInput {
            user_id: member.user_id.clone(),
            generation: new_gen,
            wrapped_key: grant.wrapped_key,
            grant_sig: grant.grant_sig,
        });
    }

    client
        .org_put_key_grants(&access_token, &org.org_id, grants)
        .await?;
    let committed = client
        .org_bump_generation(&access_token, &org.org_id, new_gen)
        .await?;
    if committed != new_gen {
        // The grants were wrapped for `new_gen`. A relay that committed something else has left a
        // live generation nobody holds a key for, so record nothing locally and keep the debt.
        return Err(AppError::Unavailable(crate::errcode::tag(
            crate::errcode::ORG_ROTATION_PENDING,
            "the server committed a different generation than the one the grants cover",
        )));
    }

    state.db.set_org_generation(&org.org_id, committed)?;
    // The freshly minted OCK is deliberately NOT cached here. The relay upserts grants, so a second
    // owner device that PUT its own grants for the same generation between our PUT and our POST
    // would have its key committed while ours is the one we hold — and everything we then sealed
    // under this generation would be unreadable to everybody, ourselves included after a restart.
    // Leaving the cache cold costs one round-trip on next use and makes `acquire_org_ock` fetch and
    // OPEN the grant the relay actually stored, which is the only copy that can be authoritative.
    state.db.clear_org_rotation_pending(&org.org_id)?;
    crate::share::ledger_row(&state.db, &client.host(), "org_rotate_generation", 0);
    Ok(committed)
}

/// Re-drive every org that still owes a key rotation. Returns how many settled.
///
/// A debt survives a restart on purpose: the window between "member removed" and "new generation
/// committed" is exactly the window in which anything newly published stays readable with the
/// removed member's old key, so it must be closed by a retry rather than by the user noticing.
pub(crate) async fn drive_pending_org_rotations(state: &AppState) -> Result<u32, AppError> {
    let mut settled = 0u32;
    for org_id in state.db.list_org_rotations_due()? {
        let mut progress = RotationProgress { learned_keys: 0 };
        match rotate_org_generation(state, &org_id, &mut progress).await {
            Ok(_) => settled = settled.saturating_add(1),
            Err(e) => {
                // Keep the debt and record WHICH failure, in the same content-free vocabulary the
                // org log uses. A rotation that keeps failing for one reason is a different problem
                // from one that fails for a new reason every time, and the row is the only place
                // that distinction survives a restart. An attempt that learned a key it did not
                // have is converging, not stuck, so it keeps its place at the front of the queue.
                let _ = state.db.record_org_rotation_failure(
                    &org_id,
                    &brief_err(&e),
                    progress.learned_keys > 0,
                );
            }
        }
    }
    Ok(settled)
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
    let _mutation = state.lock_org_mutation().await;
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
    let access_token = valid_access_token(state).await?;
    let client = crate::share::client::ShareClient::new(&base)?;

    // Only an owner can rotate, and the relay answers a non-owner's removal with a uniform 404 that
    // the client maps to Ok. Journaling first in that case would write a debt no later sweep could
    // ever settle, and every retry would spend key lookups on it. Refusing here is visible and
    // self-correcting (a membership refresh fixes a stale role); a permanent silent debt is not.
    if !org.role.eq_ignore_ascii_case("owner") {
        return Err(AppError::InvalidArg(
            "only the organization owner can remove a member".into(),
        ));
    }

    // Journal the rotation debt BEFORE asking the relay to remove anybody. Everything after this
    // point can be interrupted — a dropped connection, a quit, a crash — and the one outcome that
    // must never happen is a member removed from an org that then stays on the generation their key
    // still opens. The row is what makes that window closable by a retry instead of by luck. It is
    // deliberately NOT cleared when the removal itself fails: a failure whose response was lost is
    // indistinguishable from a success, and a redundant rotation costs one generation while a
    // skipped one costs the whole point of removing the member.
    state.db.mark_org_rotation_pending(&org.org_id)?;

    client
        .org_remove_member(&access_token, &org.org_id, &user_id)
        .await?;

    // Forget the departing member's key so a later re-invite has to learn it afresh rather than
    // reusing one this device kept while they were outside the org.
    state.db.forget_org_member_key(&org.org_id, &user_id)?;

    // The removal is done and durable at the relay; the ledger records it whether or not the
    // rotation that follows lands, because it happened.
    crate::share::ledger_row(&state.db, &client.host(), "org_remove_member", 0);

    let mut progress = RotationProgress { learned_keys: 0 };
    match rotate_org_generation(state, &org.org_id, &mut progress).await {
        Ok(_) => Ok(()),
        Err(e) => {
            let _ = state.db.record_org_rotation_failure(
                &org.org_id,
                &brief_err(&e),
                progress.learned_keys > 0,
            );
            // NOT a removal failure — saying so would invite the user to remove somebody who is
            // already gone. This names the half that is outstanding, and the copy for the code says
            // what it means for them: new posts stay readable with the old key until it lands.
            Err(AppError::Unavailable(crate::errcode::tag(
                crate::errcode::ORG_ROTATION_PENDING,
                "member removed; the key rotation did not complete and will be retried",
            )))
        }
    }
}

/// `org_leave(org_id)` — the caller leaves the TARGETED org (member self-removal). Drops that org's
/// local row + cached OCKs + decrypted replica. Does NOT retroactively un-share the caller's
/// already-published items (use `revoke_org_share` for that first if desired). Resolves the FE-picked
/// org (membership-checked), never the first via `.next()` — a leave must purge the RIGHT org.
#[tauri::command]
pub async fn org_leave(
    app: AppHandle,
    state: State<'_, AppState>,
    org_id: String,
) -> Result<(), AppError> {
    let _mutation = state.lock_org_mutation().await;
    let org = resolve_org(state.inner(), &org_id)?;
    let base = share_base_url(state.inner())?;
    let access = valid_access_token(state.inner()).await?;
    let client = crate::share::client::ShareClient::new(&base)?;
    client.org_leave(&access, &org.org_id).await?;
    // LEAVE = full consent withdrawal: atomically drop membership + PURGE the decrypted org replica
    // (items/chunks/vectors/FTS), so a departed member keeps NO searchable copy of colleagues' shared
    // content. Without this the
    // plaintext replica lingered forever and `org_search` / the `org_brain_search` tool would still
    // return it (leak/consent invariant). Belt-and-braces beside the `org_brain_available` gate on
    // the retrieval seam (a purged replica is empty either way).
    let removed = commit_org_visibility_reduction(state.inner(), Some(&app), || {
        let removed = state.inner().db.delete_org_state(&org.org_id)?;
        let mut cache = state
            .inner()
            .org_ock_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cache.retain(|(oid, _), _| oid != &org.org_id);
        Ok(removed)
    })?;
    notify_org_views_if_changed(Some(&app), removed);
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

/// Lock-lifecycle identity attached to one plaintext org-share snapshot. The folder association is
/// recorded separately from the global seal epoch because an ordinary move between OPEN folders does
/// not bump the epoch. Both must still match immediately before cloud egress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrgShareSourceVersion {
    folder_id: Option<String>,
    seal_epoch: u64,
    content_version: u64,
}

/// One cleaned/scrubbed plaintext snapshot plus the lock-lifecycle identity it was read under.
pub(crate) struct OrgShareBodySnapshot {
    title: String,
    markdown: String,
    created_at: String,
    counts: OrgScrubCounts,
    kind: crate::share::org_envelope::OrgItemKind,
    attachment_owner: crate::storage::AttachmentOwner,
    pub(crate) source_version: OrgShareSourceVersion,
}

/// Read and transform an org-share source while holding the same coarse lifecycle guard as the
/// folder lock/move state machine. In particular, the document row's folder metadata, read-gate and
/// plaintext body come from one lifecycle-consistent interval; a seal/relock cannot land between
/// the gate and the body read.
pub(crate) fn build_org_share_snapshot(
    state: &AppState,
    meeting_id: Option<&str>,
    document_id: Option<&str>,
    scrub: bool,
) -> Result<OrgShareBodySnapshot, AppError> {
    let _lifecycle = lifecycle_guard(state);
    let seal_epoch = state.seal_epoch.load(std::sync::atomic::Ordering::SeqCst);
    let content_version_before = state.db.org_source_version(meeting_id, document_id)?;
    let (title, markdown, created_at, kind, folder_id, attachment_owner) =
        match (meeting_id, document_id) {
            (Some(mid), None) => {
                // (1) READ-GATE FIRST — a sealed-not-unlocked meeting refuses before any read/egress.
                if !meeting_is_unlocked(state, mid)? {
                    return Err(AppError::Locked(crate::errcode::tag(
                            crate::errcode::MEETING_LOCKED,
                            "this meeting's folder is locked — unlock it to share to the org",
                        )));
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
                    state.db.folder_for_meeting(mid)?,
                    crate::storage::AttachmentOwner::Meeting {
                        meeting_id: mid.to_string(),
                        provider_id: note.provider_id,
                    },
                )
            }
            (None, Some(did)) => {
                // (1) READ-GATE FIRST — a sealed authored note refuses (mirrors `share_note_to_link_doc`).
                // Resolve ONLY the non-content folder anchor first. Do not load `NoteRow` (which contains
                // title + plaintext body) until the folder gate has passed.
                let folder_id = state
                    .db
                    .folder_for_document(did)?
                    .ok_or_else(|| AppError::InvalidArg(format!("no note {did}")))?;
                if !folder_is_unlocked(state, &folder_id)? {
                    return Err(AppError::Locked(
                        "this note's folder is locked — unlock it to share to the org".into(),
                    ));
                }
                if let Some((title, payload, created_at_ms)) = state.db.task_source(did)? {
                    let task = crate::share::task_envelope::TaskEnvelope::from_json(
                        &payload,
                        // Task source ids are only published through one selected org; validation
                        // against that org runs again in the publish core before egress.
                        &task_payload_org_id(&payload)?,
                    )?;
                    super::tasks::validate_task_org_refs(state, &task)?;
                    let created_at =
                        chrono::DateTime::<chrono::Utc>::from_timestamp_millis(created_at_ms)
                            .unwrap_or_else(chrono::Utc::now)
                            .to_rfc3339();
                    (
                        title,
                        serde_json::to_string(&task).map_err(|_| {
                            AppError::Unavailable("task payload encoding failed".into())
                        })?,
                        created_at,
                        crate::share::org_envelope::OrgItemKind::Task,
                        Some(folder_id),
                        crate::storage::AttachmentOwner::Document {
                            document_id: did.to_string(),
                        },
                    )
                } else {
                    let row = state
                        .db
                        .get_note_row(did)?
                        .ok_or_else(|| AppError::InvalidArg(format!("no note {did}")))?;
                    if row.folder_id != folder_id {
                        return Err(AppError::Unavailable(
                            "the note moved while preparing the org share — retry".into(),
                        ));
                    }
                    let title = note_display_title(&row);
                    let created_at =
                        chrono::DateTime::<chrono::Utc>::from_timestamp_millis(row.created_at)
                            .unwrap_or_else(chrono::Utc::now)
                            .to_rfc3339();
                    (
                        title,
                        row.text,
                        created_at,
                        crate::share::org_envelope::OrgItemKind::Note,
                        Some(folder_id),
                        crate::storage::AttachmentOwner::Document {
                            document_id: did.to_string(),
                        },
                    )
                }
            }
            _ => {
                return Err(AppError::InvalidArg(
                    "exactly one of meeting_id or document_id is required".into(),
                ));
            }
        };

    // (3) CLEAN (strip frontmatter + flatten wikilinks + drop obsidian:// refs — the leak-safe transform).
    let cleaned = if kind == crate::share::org_envelope::OrgItemKind::Task {
        markdown
    } else {
        crate::share::envelope::clean_note_body(&markdown)
    };
    // (4) regex PII scrub (emails/phones/cards; names KEPT) when requested.
    let (final_title, final_md, counts) = if kind == crate::share::org_envelope::OrgItemKind::Task {
        // Task sharing is always redacted. The hidden source is not a public generic-note surface,
        // so no caller may opt a structured Task out through the legacy `scrub=false` parameter.
        let (task, canonical, counts) =
            scrub_task_envelope_json(&cleaned, &task_payload_org_id(&cleaned)?)?;
        (task.title, canonical, counts)
    } else if scrub {
        let (markdown, counts) = scrub_org_markdown(&cleaned);
        (title.clone(), markdown, counts)
    } else {
        (title.clone(), cleaned, OrgScrubCounts::default())
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
    let content_version_after = state.db.org_source_version(meeting_id, document_id)?;
    if content_version_after != content_version_before {
        return Err(AppError::Unavailable(
            "the note changed while preparing the org share — retry".into(),
        ));
    }
    Ok(OrgShareBodySnapshot {
        title: final_title,
        markdown: final_md,
        created_at,
        counts,
        kind,
        attachment_owner,
        source_version: OrgShareSourceVersion {
            folder_id,
            seal_epoch,
            content_version: content_version_before,
        },
    })
}

fn task_payload_org_id(payload: &str) -> Result<String, AppError> {
    let value: serde_json::Value = serde_json::from_str(payload)
        .map_err(|_| AppError::InvalidArg("shared task payload is invalid".into()))?;
    value
        .get("orgId")
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| AppError::InvalidArg("shared task payload omitted its organization".into()))
}

/// Revalidate a plaintext snapshot immediately before egress. This deliberately reacquires the
/// lifecycle guard only for the short, synchronous check (never across `.await`): a seal/relock
/// bumps `seal_epoch`, while an open-folder move changes the separately-bound folder association.
/// The live read-gate is checked again as a fail-closed backstop for direct/storage recovery writes.
pub(crate) fn org_share_snapshot_is_current(
    state: &AppState,
    meeting_id: Option<&str>,
    document_id: Option<&str>,
    snapshot: &OrgShareSourceVersion,
) -> Result<bool, AppError> {
    let _lifecycle = lifecycle_guard(state);
    if state.seal_epoch.load(std::sync::atomic::Ordering::SeqCst) != snapshot.seal_epoch {
        return Ok(false);
    }
    if state.db.org_source_version(meeting_id, document_id)? != snapshot.content_version {
        return Ok(false);
    }
    let current_folder = match (meeting_id, document_id) {
        (Some(mid), None) => {
            if !meeting_is_unlocked(state, mid)? {
                return Ok(false);
            }
            state.db.folder_for_meeting(mid)?
        }
        (None, Some(did)) => {
            let Some(folder_id) = state.db.folder_for_document(did)? else {
                return Ok(false);
            };
            if !folder_is_unlocked(state, &folder_id)? {
                return Ok(false);
            }
            Some(folder_id)
        }
        _ => return Ok(false),
    };
    Ok(current_folder == snapshot.folder_id)
}

fn require_current_org_share_snapshot(
    state: &AppState,
    meeting_id: Option<&str>,
    document_id: Option<&str>,
    snapshot: &OrgShareSourceVersion,
) -> Result<(), AppError> {
    if org_share_snapshot_is_current(state, meeting_id, document_id, snapshot)? {
        Ok(())
    } else {
        Err(AppError::Locked(
            "the shared source moved or was locked while preparing the upload — retry after unlocking"
                .into(),
        ))
    }
}

/// Build the exact outgoing markdown for a meeting/note org share: the GATED read → `clean_note_body`
/// → optional regex scrub. Returns `(title, clean_scrubbed_markdown, created_at, counts, kind)`. The
/// read-gate is the FIRST thing this does (a sealed-not-unlocked source refuses). NO egress.
#[cfg(test)]
pub(crate) fn build_org_share_body(
    state: &AppState,
    meeting_id: Option<&str>,
    document_id: Option<&str>,
    scrub: bool,
) -> Result<
    (
        String,
        String,
        String,
        OrgScrubCounts,
        crate::share::org_envelope::OrgItemKind,
    ),
    AppError,
> {
    let snapshot = build_org_share_snapshot(state, meeting_id, document_id, scrub)?;
    Ok((
        snapshot.title,
        snapshot.markdown,
        snapshot.created_at,
        snapshot.counts,
        snapshot.kind,
    ))
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
    let snapshot =
        build_org_share_snapshot(state, meeting_id.as_deref(), document_id.as_deref(), scrub)?;
    let (markdown, attachments) =
        attachment_bundle_for_markdown(state, &snapshot.attachment_owner, &snapshot.markdown)?;
    let bytes = markdown.len() as u32;
    let attachment_bytes = attachments
        .iter()
        .map(|attachment| attachment.data.len() as u64)
        .sum();
    let chunk_count = rough_chunk_count(&markdown);
    Ok(OrgSharePreview {
        title: snapshot.title,
        markdown,
        bytes,
        chunk_count,
        scrubbed: snapshot.counts,
        scrub,
        attachment_count: attachments.len() as u32,
        attachment_bytes,
        image_pixels_scrubbed: false,
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
    access: Option<crate::share::org_dto::OrgItemAccess>,
) -> Result<OrgShareEntry, AppError> {
    let entry = share_to_org_notifying(
        state.inner(),
        &org_id,
        Some(meeting_id),
        None,
        scrub,
        org_access_or_view(access),
        Some(&app),
    )
    .await?;
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
    access: Option<crate::share::org_dto::OrgItemAccess>,
) -> Result<OrgShareEntry, AppError> {
    let entry = share_to_org_notifying(
        state.inner(),
        &org_id,
        None,
        Some(document_id),
        scrub,
        org_access_or_view(access),
        Some(&app),
    )
    .await?;
    // See `share_meeting_to_org`: ping open org views so the freshly-shared note appears without a
    // manual "Sync now". Content-free; best-effort.
    crate::events::emit_org_feed_updated(&app, 1);
    Ok(entry)
}

pub(crate) async fn share_task_source_to_org_notifying(
    state: &AppState,
    org_id: &str,
    source_document_id: &str,
    access: crate::share::org_dto::OrgItemAccess,
    app: &AppHandle,
) -> Result<(String, String), AppError> {
    let entry = share_to_org_notifying(
        state,
        org_id,
        None,
        Some(source_document_id.to_string()),
        true,
        access,
        Some(app),
    )
    .await?;
    let item_id = entry.item_id.ok_or_else(|| {
        AppError::Unavailable("task publish is pending authoritative recovery".into())
    })?;
    let row = state
        .db
        .org_share_by_item(&item_id)?
        .ok_or_else(|| AppError::Storage("published task journal disappeared".into()))?;
    let doc_id = row
        .doc_id
        .ok_or_else(|| AppError::Storage("published task omitted its stable document id".into()))?;
    crate::events::emit_org_feed_updated(app, 1);
    Ok((format!("{org_id}:{doc_id}"), item_id))
}

fn org_access_or_view(
    access: Option<crate::share::org_dto::OrgItemAccess>,
) -> crate::share::org_dto::OrgItemAccess {
    access.unwrap_or_default()
}

#[cfg(test)]
pub(crate) fn org_access_or_view_for_test(
    access: Option<crate::share::org_dto::OrgItemAccess>,
) -> crate::share::org_dto::OrgItemAccess {
    org_access_or_view(access)
}

#[cfg(test)]
pub(crate) async fn share_to_org_inner(
    state: &AppState,
    org_id: &str,
    meeting_id: Option<String>,
    document_id: Option<String>,
    scrub: bool,
) -> Result<OrgShareEntry, AppError> {
    share_to_org_notifying(
        state,
        org_id,
        meeting_id,
        document_id,
        scrub,
        crate::share::org_dto::OrgItemAccess::View,
        None,
    )
    .await
}

#[cfg(test)]
pub(crate) async fn share_to_org_with_access_inner(
    state: &AppState,
    org_id: &str,
    document_id: String,
    scrub: bool,
    access: crate::share::org_dto::OrgItemAccess,
) -> Result<OrgShareEntry, AppError> {
    share_to_org_inner_with_policy(
        state,
        org_id,
        None,
        Some(document_id),
        scrub,
        access,
        None,
        OrgWorkPolicy::manual(),
        None,
    )
    .await
}

async fn share_to_org_notifying(
    state: &AppState,
    org_id: &str,
    meeting_id: Option<String>,
    document_id: Option<String>,
    scrub: bool,
    access: crate::share::org_dto::OrgItemAccess,
    app: Option<&AppHandle>,
) -> Result<OrgShareEntry, AppError> {
    share_to_org_placed_notifying(
        state, org_id, meeting_id, document_id, scrub, access, None, app,
    )
    .await
}

/// The container-aware twin of [`share_to_org_notifying`]: the same gate order, plus WHERE the
/// document is filed and whether the user asked for this share themselves.
///
/// A `placement` of `None` is exactly today's standalone share, down to the wire version — the
/// envelope only reaches v4 when a real placement exists, so a member on an older client keeps
/// receiving every share that is not container-owned.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn share_to_org_placed_notifying(
    state: &AppState,
    org_id: &str,
    meeting_id: Option<String>,
    document_id: Option<String>,
    scrub: bool,
    access: crate::share::org_dto::OrgItemAccess,
    placement: Option<ContainerPlacement>,
    app: Option<&AppHandle>,
) -> Result<OrgShareEntry, AppError> {
    let _mutation = state.lock_org_mutation().await;
    share_to_org_inner_with_policy(
        state,
        org_id,
        meeting_id,
        document_id,
        scrub,
        access,
        placement,
        OrgWorkPolicy::manual(),
        app,
    )
    .await
}

// Flat publish context is intentionally explicit: these values jointly bind one source snapshot,
// permission and cancellation policy at the egress seam.
#[allow(clippy::too_many_arguments)]
async fn share_to_org_inner_with_policy(
    state: &AppState,
    org_id: &str,
    meeting_id: Option<String>,
    document_id: Option<String>,
    scrub: bool,
    access: crate::share::org_dto::OrgItemAccess,
    placement: Option<ContainerPlacement>,
    policy: OrgWorkPolicy,
    app: Option<&AppHandle>,
) -> Result<OrgShareEntry, AppError> {
    if !policy.is_current() {
        return Err(AppError::Unavailable(
            "background org publish deferred for recording".into(),
        ));
    }
    // SECURITY: gate and snapshot the CURRENT source before even reading an existing share row.
    // `OrgShareRow.title` is real source metadata, so the idempotent/dedup fast path must not return
    // it for a source that is now sealed-not-unlocked. The snapshot is revalidated after the async
    // duplicate cleanup as well, closing a lock/move-during-dedup title leak.
    let dedup_source =
        build_org_share_snapshot(state, meeting_id.as_deref(), document_id.as_deref(), scrub)?;
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
        policy,
        app,
    )
    .await?
    {
        if keeper.last_error.as_deref() == Some(ORG_SHARE_INITIAL_POST_REPLAYABLE) {
            // Authenticated absence is the only phase that may re-arm the exact initial POST.
            // Continue into the witness-bound replay path below; every other live/ambiguous phase
            // remains an idempotent no-mutation return.
        } else {
            require_current_org_share_snapshot(
                state,
                meeting_id.as_deref(),
                document_id.as_deref(),
                &dedup_source.source_version,
            )?;
            return Ok(OrgShareEntry {
                item_id: keeper.item_id,
                kind: keeper.kind,
                title: keeper.title,
                shared_at: keeper.created_at,
                rev: keeper.rev,
                state: keeper.state,
            });
        }
    }

    // Not yet live in this org → the normal first share = rev 1. A re-publish-on-edit supersede bumps
    // the rev (see `republish_org_shares_for_source`, which calls `publish_org_body` with `old_rev + 1`).
    if !policy.is_current() {
        return Err(AppError::Unavailable(
            "background org publish deferred for recording".into(),
        ));
    }
    publish_org_body_with_policy(
        state,
        org_id,
        meeting_id,
        document_id,
        scrub,
        1,
        access,
        placement,
        policy,
        app,
    )
    .await
}

/// Where a document is filed inside a shared container, and whether the user asked for its share.
///
/// `explicit` is separate from the placement itself because the two answer different questions:
/// the placement says WHERE the document sits, `explicit` says whether unsharing the container may
/// withdraw it. A note the user shared deliberately and later dragged into a shared folder is
/// placed but still explicit — unsharing the folder must leave it live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContainerPlacement {
    pub(crate) parent_container_id: String,
    pub(crate) position: i64,
    pub(crate) explicit: bool,
}

impl ContainerPlacement {
    fn envelope(&self) -> crate::share::org_envelope::OrgPlacement {
        crate::share::org_envelope::OrgPlacement {
            parent_container_id: self.parent_container_id.clone(),
            position: self.position,
        }
    }
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
    policy: OrgWorkPolicy,
    app: Option<&AppHandle>,
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
        // A direct-PUT journal still anchors the live predecessor and is not an accidental
        // duplicate. Reconciliation alone may advance it; generic dedup must never withdraw it.
        if matches!(
            extra.last_error.as_deref(),
            Some(
                ORG_SHARE_DIRECT_PUT_PENDING
                    | ORG_SHARE_REPUBLISH_PUT_PENDING
                    | ORG_SHARE_ERR_EDIT_CONFLICT
            )
        ) {
            continue;
        }
        if !policy.is_current() {
            return Ok(Some(keeper));
        }
        if let Some(item_id) = extra.item_id.clone() {
            // Tombstone the redundant copy. Swallow errors — `revoke_org_share_inner` marks the row
            // `revoke_pending` first, so an interrupted tombstone is completed by the launch sweep.
            let _ = revoke_org_share_inner_with_policy(state, item_id, policy, app).await;
            if !policy.is_current() {
                return Ok(Some(keeper));
            }
        }
    }
    // Also cancel any NOT-yet-uploaded sibling (`queued`/`failed`) for this (org, source): the source is
    // already live (the keeper), so a pending row is redundant and would otherwise linger as a stuck
    // "pending" share that the launch sweep re-attempts every start. Local-only — these have no server
    // item to tombstone. Best-effort (never fails the idempotent return).
    if keeper.state == "uploaded" && keeper.item_id.is_some() {
        let now = chrono::Utc::now().to_rfc3339();
        let _ = policy.commit(|| {
            state
                .db
                .cancel_pending_org_shares_for_source_in_org(org_id, meeting_id, document_id, &now)
                .map(|_| ())
        });
    }
    Ok(Some(keeper))
}

/// TERMINAL `org_shares.last_error` reason (Brain v3 org push size pre-check): the sealed
/// ciphertext exceeds the server's per-item blob cap
/// (`murmur_protocol::caps::MAX_ORG_ITEM_BLOB_BYTES`). The launch sweep NEVER retries a row
/// failed with this reason — retrying cannot shrink the content, so requeueing it every start was a
/// poison loop (the server 413s forever). Recovery is content-driven: a manual re-share
/// (`share_to_org_inner` reuses + re-arms the row) or an edit-save republish re-reads the trimmed
/// source and clears the reason on success.
pub(crate) const ORG_SHARE_ERR_TOO_LARGE: &str = "too_large";
pub(crate) const ORG_SHARE_ERR_EDIT_CONFLICT: &str = "org_edit_conflict";
const ORG_SHARE_REPUBLISH_PUT_PENDING: &str = "republish_put_pending";
const ORG_SHARE_REPUBLISH_POST_PENDING: &str = "republish_post_pending";
const ORG_SHARE_INITIAL_POST_PENDING: &str = "initial_post_pending";
const ORG_SHARE_INITIAL_POST_REPLAYABLE: &str = "initial_post_replayable";
const ORG_SHARE_PROJECTION_PENDING: &str = "projection_pending";
const ORG_SHARE_PUBLISH_REJECTED: &str = "publish_rejected";
const ORG_SHARE_DIRECT_PUT_PENDING: &str = "direct_put_pending";

/// Single-use authority for content-free relay capability discovery.
///
/// The permit is minted only after sharing consent and a unique durable egress receipt. The
/// private witness binds the exact relay host and `/healthz` operation; the client consumes it at
/// the socket boundary so no second or redirected capability read can reuse the authority.
#[derive(Debug, Clone)]
#[must_use]
pub(crate) struct ShareCapabilityReadPermit {
    dispatch_id: String,
    host: String,
    path: &'static str,
    consumed: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl ShareCapabilityReadPermit {
    pub(crate) fn authorize(self, host: &str, path: &str) -> Result<(), AppError> {
        consume_share_dispatch(&self.consumed, "share capability read")?;
        if self.host != host || self.path != path {
            return Err(AppError::Storage(
                "share capability read permit mismatch".into(),
            ));
        }
        let _ = self.dispatch_id;
        Ok(())
    }
}

/// Single-use authority for the content-free owner-bound share-id reservation request.
///
/// The permit is minted only after the one-time sharing consent and a unique durable dispatch
/// receipt have both succeeded. Its private witness binds the exact remote sink fields; cloning is
/// safe because every clone shares the same atomic consumed bit and therefore at most one request
/// can pass the HTTP boundary.
#[derive(Debug, Clone)]
#[must_use]
pub(crate) struct ShareReservationPermit {
    dispatch_id: String,
    host: String,
    share_id: String,
    owner_user_id: String,
    mode: murmur_protocol::dto::ShareMode,
    consumed: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// In-memory proof that the relay returned terminal 204 for the exact owner-bound reservation.
/// Only [`reserve_outbound_share_id`] can construct this in production, so a content POST cannot be
/// authorized before the reservation socket has completed successfully.
struct ReservedShareId {
    host: String,
    share_id: String,
    owner_user_id: String,
    mode: murmur_protocol::dto::ShareMode,
}

#[derive(Debug, Clone)]
#[must_use]
pub(crate) struct ShareContentDispatchPermit {
    dispatch_id: String,
    host: String,
    share_id: String,
    owner_user_id: String,
    mode: murmur_protocol::dto::ShareMode,
    rev: u32,
    source_commitment: [u8; 32],
    request_commitment: [u8; 32],
    consumed: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl ShareContentDispatchPermit {
    pub(crate) fn authorize(
        self,
        host: &str,
        owner_user_id: &str,
        source_commitment: [u8; 32],
        request: &murmur_protocol::dto::CreateShareRequest,
    ) -> Result<(), AppError> {
        consume_share_dispatch(&self.consumed, "share content dispatch")?;
        if self.host != host
            || self.share_id != request.share_id
            || self.owner_user_id != owner_user_id
            || self.mode != request.mode
            || self.rev != request.rev
            || self.source_commitment != source_commitment
            || self.request_commitment != share_content_request_commitment(request)?
        {
            return Err(AppError::Storage(
                "share content dispatch permit mismatch".into(),
            ));
        }
        let _ = self.dispatch_id;
        Ok(())
    }
}

#[derive(Debug, Clone)]
#[must_use]
pub(crate) struct ShareDeleteDispatchPermit {
    dispatch_id: String,
    host: String,
    share_id: String,
    owner_user_id: String,
    mode: murmur_protocol::dto::ShareMode,
    rev: u32,
    consumed: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl ShareDeleteDispatchPermit {
    pub(crate) fn authorize(
        self,
        host: &str,
        share_id: &str,
        owner_user_id: &str,
        mode: murmur_protocol::dto::ShareMode,
        rev: u32,
    ) -> Result<(), AppError> {
        consume_share_dispatch(&self.consumed, "share delete dispatch")?;
        if self.host != host
            || self.share_id != share_id
            || self.owner_user_id != owner_user_id
            || self.mode != mode
            || self.rev != rev
        {
            return Err(AppError::Storage(
                "share delete dispatch permit mismatch".into(),
            ));
        }
        let _ = self.dispatch_id;
        Ok(())
    }
}

fn consume_share_dispatch(
    consumed: &std::sync::atomic::AtomicBool,
    label: &str,
) -> Result<(), AppError> {
    consumed
        .compare_exchange(
            false,
            true,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
        )
        .map(|_| ())
        .map_err(|_| AppError::Storage(format!("{label} permit was already consumed")))
}

impl ShareReservationPermit {
    pub(crate) fn authorize(
        self,
        host: &str,
        share_id: &str,
        owner_user_id: &str,
        mode: murmur_protocol::dto::ShareMode,
    ) -> Result<(), AppError> {
        consume_share_dispatch(&self.consumed, "share reservation dispatch")?;
        if self.host != host
            || self.share_id != share_id
            || self.owner_user_id != owner_user_id
            || self.mode != mode
        {
            return Err(AppError::Storage(
                "share reservation dispatch permit mismatch".into(),
            ));
        }
        let _ = self.dispatch_id;
        Ok(())
    }
}

fn share_content_request_commitment(
    request: &murmur_protocol::dto::CreateShareRequest,
) -> Result<[u8; 32], AppError> {
    use sha2::{Digest, Sha256};

    let wire = serde_json::to_vec(request)
        .map_err(|_| AppError::Storage("serialize share content dispatch witness".into()))?;
    Ok(Sha256::digest(wire).into())
}

fn share_source_commitment(source: &OrgShareSourceVersion) -> [u8; 32] {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    match &source.folder_id {
        Some(folder_id) => {
            hasher.update([1]);
            hasher.update((folder_id.len() as u64).to_be_bytes());
            hasher.update(folder_id.as_bytes());
        }
        None => hasher.update([0]),
    }
    hasher.update(source.seal_epoch.to_be_bytes());
    hasher.update(source.content_version.to_be_bytes());
    hasher.finalize().into()
}

#[cfg(test)]
pub(crate) fn test_share_reservation_permit(
    host: &str,
    share_id: &str,
    owner_user_id: &str,
    mode: murmur_protocol::dto::ShareMode,
) -> ShareReservationPermit {
    ShareReservationPermit {
        dispatch_id: "test-share-reservation-dispatch".to_string(),
        host: host.to_string(),
        share_id: share_id.to_string(),
        owner_user_id: owner_user_id.to_string(),
        mode,
        consumed: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    }
}

#[cfg(test)]
pub(crate) fn test_share_capability_read_permit(host: &str) -> ShareCapabilityReadPermit {
    ShareCapabilityReadPermit {
        dispatch_id: "test-share-capability-dispatch".to_string(),
        host: host.to_string(),
        path: "/healthz",
        consumed: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    }
}

#[cfg(test)]
pub(crate) fn test_share_content_dispatch_permit(
    host: &str,
    owner_user_id: &str,
    source_commitment: [u8; 32],
    request: &murmur_protocol::dto::CreateShareRequest,
) -> ShareContentDispatchPermit {
    ShareContentDispatchPermit {
        dispatch_id: "test-share-content-dispatch".to_string(),
        host: host.to_string(),
        share_id: request.share_id.clone(),
        owner_user_id: owner_user_id.to_string(),
        mode: request.mode,
        rev: request.rev,
        source_commitment,
        request_commitment: share_content_request_commitment(request).unwrap(),
        consumed: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    }
}

#[cfg(test)]
pub(crate) fn test_share_delete_dispatch_permit(
    host: &str,
    share_id: &str,
    owner_user_id: &str,
    mode: murmur_protocol::dto::ShareMode,
    rev: u32,
) -> ShareDeleteDispatchPermit {
    ShareDeleteDispatchPermit {
        dispatch_id: "test-share-delete-dispatch".to_string(),
        host: host.to_string(),
        share_id: share_id.to_string(),
        owner_user_id: owner_user_id.to_string(),
        mode,
        rev,
        consumed: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    }
}

#[must_use]
pub(crate) struct OrgDispatchPermit {
    dispatch_id: String,
    host: String,
    operation: OrgDispatchOperation,
}

pub(crate) enum OrgDispatchOperation {
    Publish {
        org_id: String,
        doc_id: Option<String>,
        access: Option<crate::share::org_dto::OrgItemAccess>,
        rev: u32,
        generation: u32,
        content_sha256: Vec<u8>,
        cell_len: usize,
        cell_sha256: [u8; 32],
        owner_user_id: Option<String>,
    },
    Update {
        org_id: String,
        doc_id: String,
        expected_rev: u32,
        generation: u32,
        content_sha256: Vec<u8>,
        cell_len: usize,
        cell_sha256: [u8; 32],
        access: crate::share::org_dto::OrgItemAccess,
        owner_user_id: String,
    },
    Access {
        org_id: String,
        doc_id: String,
        access: crate::share::org_dto::OrgItemAccess,
        owner_user_id: String,
    },
    DeleteDocument {
        org_id: String,
        doc_id: String,
    },
    Tombstone {
        org_id: String,
        item_id: String,
    },
    MembershipCorroborate {
        org_id: String,
    },
}

impl OrgDispatchPermit {
    pub(crate) fn authorize_publish(
        self,
        host: &str,
        org_id: &str,
        request: &crate::share::org_dto::PublishItemRequest,
    ) -> Result<Option<String>, AppError> {
        let OrgDispatchOperation::Publish {
            org_id: expected_org,
            doc_id,
            access,
            rev,
            generation,
            content_sha256,
            cell_len,
            cell_sha256,
            owner_user_id,
        } = self.operation
        else {
            return Err(AppError::Storage(
                "org dispatch permit operation mismatch".into(),
            ));
        };
        if self.host != host
            || expected_org != org_id
            || request
                .mutation_id
                .as_deref()
                .is_some_and(|id| id != self.dispatch_id)
            || doc_id.as_deref() != request.doc_id.as_deref()
            || access != request.access
            || rev != request.rev
            || generation != request.generation
            || content_sha256 != request.content_sha256
            || request.content_cell.as_ref().map(Vec::len) != Some(cell_len)
            || request
                .content_cell
                .as_deref()
                .map(org_dispatch_cell_sha256)
                != Some(cell_sha256)
        {
            return Err(AppError::Storage(
                "org publish dispatch permit mismatch".into(),
            ));
        }
        Ok(owner_user_id)
    }

    pub(crate) fn authorize_update(
        self,
        host: &str,
        org_id: &str,
        doc_id: &str,
        request: &crate::share::org_dto::UpdateOrgItemRequest,
    ) -> Result<(crate::share::org_dto::OrgItemAccess, String), AppError> {
        let OrgDispatchOperation::Update {
            org_id: expected_org,
            doc_id: expected_doc,
            expected_rev,
            generation,
            content_sha256,
            cell_len,
            cell_sha256,
            access,
            owner_user_id,
        } = self.operation
        else {
            return Err(AppError::Storage(
                "org dispatch permit operation mismatch".into(),
            ));
        };
        if self.host != host
            || expected_org != org_id
            || request
                .mutation_id
                .as_deref()
                .is_some_and(|id| id != self.dispatch_id)
            || expected_doc != doc_id
            || expected_rev != request.expected_rev
            || generation != request.generation
            || content_sha256 != request.content_sha256
            || cell_len != request.content_cell.len()
            || cell_sha256 != org_dispatch_cell_sha256(&request.content_cell)
        {
            return Err(AppError::Storage(
                "org update dispatch permit mismatch".into(),
            ));
        }
        Ok((access, owner_user_id))
    }

    pub(crate) fn authorize_access(
        self,
        host: &str,
        org_id: &str,
        doc_id: &str,
        request: &crate::share::org_dto::SetOrgItemAccessRequest,
    ) -> Result<String, AppError> {
        let OrgDispatchOperation::Access {
            org_id: expected_org,
            doc_id: expected_doc,
            access,
            owner_user_id,
        } = self.operation
        else {
            return Err(AppError::Storage(
                "org dispatch permit operation mismatch".into(),
            ));
        };
        if self.host != host
            || expected_org != org_id
            || expected_doc != doc_id
            || access != request.access
        {
            return Err(AppError::Storage(
                "org access dispatch permit mismatch".into(),
            ));
        }
        let _ = self.dispatch_id;
        Ok(owner_user_id)
    }

    pub(crate) fn authorize_delete_document(
        self,
        host: &str,
        org_id: &str,
        doc_id: &str,
    ) -> Result<(), AppError> {
        match self.operation {
            OrgDispatchOperation::DeleteDocument {
                org_id: o,
                doc_id: d,
            } if self.host == host && o == org_id && d == doc_id => Ok(()),
            _ => Err(AppError::Storage(
                "org delete dispatch permit mismatch".into(),
            )),
        }
    }

    pub(crate) fn authorize_tombstone(
        self,
        host: &str,
        org_id: &str,
        item_id: &str,
    ) -> Result<(), AppError> {
        match self.operation {
            OrgDispatchOperation::Tombstone {
                org_id: o,
                item_id: i,
            } if self.host == host && o == org_id && i == item_id => Ok(()),
            _ => Err(AppError::Storage(
                "org tombstone dispatch permit mismatch".into(),
            )),
        }
    }

    pub(crate) fn authorize_membership_corroborate(
        self,
        host: &str,
        org_id: &str,
    ) -> Result<(), AppError> {
        match self.operation {
            OrgDispatchOperation::MembershipCorroborate { org_id: o }
                if self.host == host && o == org_id =>
            {
                Ok(())
            }
            _ => Err(AppError::Storage(
                "org status dispatch permit mismatch".into(),
            )),
        }
    }
}

pub(crate) fn org_dispatch_cell_sha256(cell: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};

    Sha256::digest(cell).into()
}

#[must_use]
pub(crate) struct OrgReadPermit {
    dispatch_id: String,
    host: String,
    org_id: String,
    doc_id: String,
    purpose: OrgReadPurpose,
    since_seq: u64,
    limit: u32,
}

#[must_use]
pub(crate) struct OrgMembersReadPermit {
    dispatch_id: String,
    host: String,
    org_id: String,
}

impl OrgMembersReadPermit {
    pub(crate) fn authorize(self, host: &str, org_id: &str) -> Result<(), AppError> {
        if self.host == host && self.org_id == org_id {
            let _ = self.dispatch_id;
            Ok(())
        } else {
            Err(AppError::Storage("org member read permit mismatch".into()))
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum OrgReadPurpose {
    CurrentHead,
    History,
    Delete404Corroboration,
    Tombstone404Corroboration,
}

impl OrgReadPermit {
    pub(crate) fn authorize_page(
        self,
        host: &str,
        org_id: &str,
        doc_id: &str,
        purpose: OrgReadPurpose,
        since_seq: u64,
        limit: u32,
    ) -> Result<(), AppError> {
        if self.host == host
            && self.org_id == org_id
            && self.doc_id == doc_id
            && self.purpose == purpose
            && self.since_seq == since_seq
            && self.limit == limit
        {
            let _ = self.dispatch_id;
            Ok(())
        } else {
            Err(AppError::Storage(
                "org recovery read permit mismatch".into(),
            ))
        }
    }
}

#[cfg(test)]
pub(crate) fn test_org_publish_dispatch_permit(
    host: &str,
    org_id: &str,
    request: &crate::share::org_dto::PublishItemRequest,
    owner_user_id: Option<&str>,
) -> OrgDispatchPermit {
    OrgDispatchPermit {
        dispatch_id: "test-org-dispatch".to_string(),
        host: host.to_string(),
        operation: OrgDispatchOperation::Publish {
            org_id: org_id.to_string(),
            doc_id: request.doc_id.clone(),
            access: request.access,
            rev: request.rev,
            generation: request.generation,
            content_sha256: request.content_sha256.clone(),
            cell_len: request.content_cell.as_ref().map(Vec::len).unwrap_or(0),
            cell_sha256: org_dispatch_cell_sha256(
                request.content_cell.as_deref().unwrap_or_default(),
            ),
            owner_user_id: owner_user_id.map(str::to_string),
        },
    }
}

#[cfg(test)]
pub(crate) fn test_org_update_dispatch_permit(
    host: &str,
    org_id: &str,
    doc_id: &str,
    access: crate::share::org_dto::OrgItemAccess,
    owner_user_id: &str,
    request: &crate::share::org_dto::UpdateOrgItemRequest,
) -> OrgDispatchPermit {
    OrgDispatchPermit {
        dispatch_id: "test-update-dispatch".to_string(),
        host: host.to_string(),
        operation: OrgDispatchOperation::Update {
            org_id: org_id.to_string(),
            doc_id: doc_id.to_string(),
            expected_rev: request.expected_rev,
            generation: request.generation,
            content_sha256: request.content_sha256.clone(),
            cell_len: request.content_cell.len(),
            cell_sha256: org_dispatch_cell_sha256(&request.content_cell),
            access,
            owner_user_id: owner_user_id.to_string(),
        },
    }
}

#[allow(clippy::too_many_arguments)] // One cohesive durable state + dispatch-ledger transaction.
fn persist_initial_org_publish_intent(
    state: &AppState,
    row_id: &str,
    org_id: &str,
    meeting_id: Option<&str>,
    document_id: Option<&str>,
    doc_id: &str,
    access: crate::share::org_dto::OrgItemAccess,
    rev: u32,
    generation: u32,
    content_sha256: &[u8],
    scrub: bool,
    actor_user_id: &str,
    updated_at: &str,
    ledger_host: &str,
    sealed_bytes: usize,
    sealed_cell_sha256: [u8; 32],
    expected_source_version: u64,
    expected_row_source_version: u64,
    expected_dirty_counter: u64,
) -> Result<(OrgDispatchPermit, String), AppError> {
    require_org_egress_consent(state)?;
    let dispatch_id = uuid::Uuid::new_v4().to_string();
    let mut conn = state.db.lock();
    let tx = conn
        .transaction()
        .map_err(|_| AppError::Storage("start initial org publish dispatch".into()))?;
    let changed = tx
        .execute(
            "UPDATE org_shares
                SET state = 'failed', last_error = ?2,
                    expected_actor_user_id = COALESCE(expected_actor_user_id, ?3),
                    expected_owner_user_id = COALESCE(expected_owner_user_id, ?3),
                    dispatch_id = ?5, republish_dirty = 0, updated_at = ?4
              WHERE id = ?1 AND state = 'queued' AND last_error IS NULL
                AND doc_id = ?6 AND rev = ?7 AND generation = ?8
                AND access = ?9 AND content_sha256 = ?10 AND scrub = ?11
                AND (expected_actor_user_id IS NULL OR expected_actor_user_id = ?3)
                AND (expected_owner_user_id IS NULL OR expected_owner_user_id = ?3)
                AND COALESCE((SELECT version FROM org_source_versions
                      WHERE (source_kind='meeting' AND source_id=org_shares.meeting_id)
                         OR (source_kind='document' AND source_id=org_shares.document_id)), 0) = ?12
                AND republish_dirty = ?13 AND source_version = ?14
                AND org_id=?15 AND meeting_id IS ?16 AND document_id IS ?17",
            rusqlite::params![
                row_id,
                ORG_SHARE_INITIAL_POST_PENDING,
                actor_user_id,
                updated_at,
                dispatch_id,
                doc_id,
                rev as i64,
                generation as i64,
                access.as_str(),
                content_sha256,
                scrub as i64,
                expected_source_version as i64,
                expected_dirty_counter as i64,
                expected_row_source_version as i64,
                org_id,
                meeting_id,
                document_id,
            ],
        )
        .map_err(|_| AppError::Storage("persist initial org publish intent".into()))?;
    if changed != 1 {
        return Err(AppError::Unavailable(
            "initial org publish changed before dispatch".into(),
        ));
    }
    let ledger_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    crate::storage::db::insert_share_egress_dispatch_tx(
        &tx,
        ledger_ts,
        ledger_host,
        "org_share_publish",
        sealed_bytes,
        &dispatch_id,
    )?;
    tx.commit()
        .map_err(|_| AppError::Storage("commit initial org publish dispatch".into()))?;
    let permit = OrgDispatchPermit {
        dispatch_id: dispatch_id.clone(),
        host: ledger_host.to_string(),
        operation: OrgDispatchOperation::Publish {
            org_id: org_id.to_string(),
            doc_id: Some(doc_id.to_string()),
            access: Some(access),
            rev,
            generation,
            content_sha256: content_sha256.to_vec(),
            cell_len: sealed_bytes,
            cell_sha256: sealed_cell_sha256,
            owner_user_id: Some(actor_user_id.to_string()),
        },
    };
    Ok((permit, dispatch_id))
}

/// Mint a publish permit for one CONTAINER manifest, CAS-ing its journal row out of `queued` and
/// ledgering the dispatch in the SAME transaction.
///
/// This mirrors [`persist_initial_org_publish_intent`] and exists separately for one reason: a
/// manifest has no local `meeting_id`/`document_id`, so it cannot ride `org_shares`'s logical key.
/// Everything that makes that path crash-safe is preserved — the row is durably marked
/// "dispatched, outcome unknown" BEFORE the socket, and the egress ledger row commits with it, so a
/// crash mid-publish is recoverable rather than invisible.
#[allow(clippy::too_many_arguments)]
pub(crate) fn persist_container_publish_intent(
    state: &AppState,
    share_id: &str,
    org_id: &str,
    doc_id: &str,
    access: crate::share::org_dto::OrgItemAccess,
    rev: u32,
    generation: u32,
    content_sha256: &[u8],
    actor_user_id: &str,
    updated_at: &str,
    ledger_host: &str,
    sealed_bytes: usize,
    sealed_cell_sha256: [u8; 32],
) -> Result<(OrgDispatchPermit, String), AppError> {
    require_org_egress_consent(state)?;
    let dispatch_id = uuid::Uuid::new_v4().to_string();
    let mut conn = state.db.lock();
    let tx = conn
        .transaction()
        .map_err(|_| AppError::Storage("start container publish dispatch".into()))?;
    let changed = tx
        .execute(
            "UPDATE org_container_shares
                SET state = 'failed', last_error = ?2, rev = ?4, content_sha256 = ?5,
                    access = ?6, updated_at = ?3
              WHERE id = ?1 AND org_id = ?7 AND container_id = ?8
                AND state IN ('queued','failed')",
            rusqlite::params![
                share_id,
                CONTAINER_SHARE_POST_PENDING,
                updated_at,
                rev as i64,
                content_sha256,
                access.as_str(),
                org_id,
                doc_id,
            ],
        )
        .map_err(|_| AppError::Storage("persist container publish intent".into()))?;
    if changed != 1 {
        return Err(AppError::Unavailable(
            "container publish changed before dispatch".into(),
        ));
    }
    let ledger_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    crate::storage::db::insert_share_egress_dispatch_tx(
        &tx,
        ledger_ts,
        ledger_host,
        "org_share_publish",
        sealed_bytes,
        &dispatch_id,
    )?;
    tx.commit()
        .map_err(|_| AppError::Storage("commit container publish dispatch".into()))?;
    let permit = OrgDispatchPermit {
        dispatch_id: dispatch_id.clone(),
        host: ledger_host.to_string(),
        operation: OrgDispatchOperation::Publish {
            org_id: org_id.to_string(),
            doc_id: Some(doc_id.to_string()),
            access: Some(access),
            rev,
            generation,
            content_sha256: content_sha256.to_vec(),
            cell_len: sealed_bytes,
            cell_sha256: sealed_cell_sha256,
            owner_user_id: Some(actor_user_id.to_string()),
        },
    };
    Ok((permit, dispatch_id))
}

/// The durable "dispatched, outcome unknown" marker for a container manifest publish. Named
/// separately from the note path's constant so a reader can tell the two journals apart.
pub(crate) const CONTAINER_SHARE_POST_PENDING: &str = "container_post_pending";

/// The content-free reason a manifest publish failed before any egress.
pub(crate) const CONTAINER_SHARE_SEAL_FAILED: &str = "container_seal_failed";

#[allow(clippy::too_many_arguments)]
fn fail_initial_org_publish_pre_dispatch_if_current(
    state: &AppState,
    row_id: &str,
    error: &str,
    org_id: &str,
    meeting_id: Option<&str>,
    document_id: Option<&str>,
    doc_id: &str,
    access: crate::share::org_dto::OrgItemAccess,
    rev: u32,
    generation: u32,
    content_sha256: &[u8],
    scrub: bool,
    source_version: u64,
    row_source_version: u64,
    dirty_counter: u64,
    updated_at: &str,
) -> Result<(), AppError> {
    let conn = state.db.lock();
    let changed = conn
        .execute(
            "UPDATE org_shares SET state='failed', last_error=?2,
            republish_dirty=CASE WHEN ?2='too_large' THEN 0 ELSE republish_dirty END,
            updated_at=?3
          WHERE id=?1 AND state='queued' AND last_error IS NULL AND dispatch_id IS NULL
            AND doc_id=?4 AND access=?5 AND rev=?6 AND generation=?7
            AND content_sha256=?8 AND scrub=?9
            AND COALESCE((SELECT version FROM org_source_versions
              WHERE (source_kind='meeting' AND source_id=org_shares.meeting_id)
                 OR (source_kind='document' AND source_id=org_shares.document_id)),0)=?10
            AND republish_dirty=?11 AND source_version=?12
            AND org_id=?13 AND meeting_id IS ?14 AND document_id IS ?15",
            rusqlite::params![
                row_id,
                error,
                updated_at,
                doc_id,
                access.as_str(),
                rev as i64,
                generation as i64,
                content_sha256,
                scrub as i64,
                source_version as i64,
                dirty_counter as i64,
                row_source_version as i64,
                org_id,
                meeting_id,
                document_id
            ],
        )
        .map_err(|_| AppError::Storage("fail initial org publish pre-dispatch".into()))?;
    if changed != 1 {
        return Err(AppError::Unavailable(
            "initial org share changed before local failure could be recorded".into(),
        ));
    }
    Ok(())
}

#[allow(dead_code, clippy::too_many_arguments)] // Retained for storage-level CAS regression oracles.
pub(crate) fn complete_initial_org_publish_intent(
    state: &AppState,
    row_id: &str,
    published: &crate::share::org_dto::PublishItemResponse,
    expected_actor_user_id: &str,
    expected_owner_user_id: &str,
    expected_doc_id: &str,
    expected_access: crate::share::org_dto::OrgItemAccess,
    expected_rev: u32,
    expected_generation: u32,
    expected_content_sha256: &[u8],
    expected_scrub: bool,
    expected_dispatch_id: &str,
    updated_at: &str,
) -> Result<(), AppError> {
    let conn = state.db.lock();
    let changed = conn
        .execute(
            "UPDATE org_shares
                SET state = 'uploaded', item_id = ?2, last_error = NULL, updated_at = ?3
              WHERE id = ?1 AND state = 'failed' AND last_error = ?4
                AND expected_actor_user_id = ?5 AND expected_owner_user_id = ?6
                AND doc_id = ?7 AND access = ?8 AND rev = ?9 AND generation = ?10
                AND content_sha256 = ?11 AND scrub = ?12 AND dispatch_id = ?13",
            rusqlite::params![
                row_id,
                published.item_id,
                updated_at,
                ORG_SHARE_INITIAL_POST_PENDING,
                expected_actor_user_id,
                expected_owner_user_id,
                expected_doc_id,
                expected_access.as_str(),
                expected_rev as i64,
                expected_generation as i64,
                expected_content_sha256,
                expected_scrub as i64,
                expected_dispatch_id,
            ],
        )
        .map_err(|_| AppError::Storage("complete initial org publish intent".into()))?;
    if changed != 1 {
        return Err(AppError::Unavailable(
            "initial org publish attempt changed before completion".into(),
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn transition_initial_org_publish_intent(
    state: &AppState,
    row_id: &str,
    next_error: &str,
    expected_actor_user_id: &str,
    expected_owner_user_id: &str,
    expected_doc_id: &str,
    expected_access: crate::share::org_dto::OrgItemAccess,
    expected_rev: u32,
    expected_generation: u32,
    expected_content_sha256: &[u8],
    expected_scrub: bool,
    expected_dispatch_id: &str,
    updated_at: &str,
) -> Result<(), AppError> {
    let conn = state.db.lock();
    let changed = conn
        .execute(
            "UPDATE org_shares SET last_error = ?2, updated_at = ?3
              WHERE id = ?1 AND state = 'failed' AND last_error = ?4
                AND expected_actor_user_id = ?5 AND expected_owner_user_id = ?6
                AND doc_id = ?7 AND access = ?8 AND rev = ?9 AND generation = ?10
                AND content_sha256 = ?11 AND scrub = ?12 AND dispatch_id = ?13",
            rusqlite::params![
                row_id,
                next_error,
                updated_at,
                ORG_SHARE_INITIAL_POST_PENDING,
                expected_actor_user_id,
                expected_owner_user_id,
                expected_doc_id,
                expected_access.as_str(),
                expected_rev as i64,
                expected_generation as i64,
                expected_content_sha256,
                expected_scrub as i64,
                expected_dispatch_id,
            ],
        )
        .map_err(|_| AppError::Storage("transition initial org publish intent".into()))?;
    if changed != 1 {
        return Err(AppError::Unavailable(
            "initial org publish attempt changed before resolution".into(),
        ));
    }
    Ok(())
}

fn permit_org_publish(
    state: &AppState,
    host: &str,
    org_id: &str,
    request: &crate::share::org_dto::PublishItemRequest,
    owner_user_id: Option<&str>,
) -> Result<OrgDispatchPermit, AppError> {
    require_org_egress_consent(state)?;
    let dispatch_id = uuid::Uuid::new_v4().to_string();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    let sealed_bytes = request.content_cell.as_ref().map(Vec::len).unwrap_or(0);
    let mut conn = state.db.lock();
    let tx = conn
        .transaction()
        .map_err(|_| AppError::Storage("start legacy org publish dispatch".into()))?;
    crate::storage::db::insert_share_egress_dispatch_tx(
        &tx,
        ts,
        host,
        "org_share_publish",
        sealed_bytes,
        &dispatch_id,
    )?;
    tx.commit()
        .map_err(|_| AppError::Storage("commit legacy org publish dispatch".into()))?;
    Ok(OrgDispatchPermit {
        dispatch_id,
        host: host.to_string(),
        operation: OrgDispatchOperation::Publish {
            org_id: org_id.to_string(),
            doc_id: request.doc_id.clone(),
            access: request.access,
            rev: request.rev,
            generation: request.generation,
            content_sha256: request.content_sha256.clone(),
            cell_len: sealed_bytes,
            cell_sha256: org_dispatch_cell_sha256(
                request.content_cell.as_deref().unwrap_or_default(),
            ),
            owner_user_id: owner_user_id.map(str::to_string),
        },
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn permit_org_access(
    state: &AppState,
    host: &str,
    org_id: &str,
    doc_id: &str,
    access: crate::share::org_dto::OrgItemAccess,
    old_access: crate::share::org_dto::OrgItemAccess,
    actor_user_id: &str,
    owner_user_id: &str,
) -> Result<(OrgDispatchPermit, String), AppError> {
    require_org_egress_consent(state)?;
    let dispatch_id = uuid::Uuid::new_v4().to_string();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    let admitted = state.db.persist_org_access_attempt_if_current(
        ts,
        host,
        &dispatch_id,
        org_id,
        doc_id,
        old_access.as_str(),
        access.as_str(),
        actor_user_id,
        owner_user_id,
        &chrono::Utc::now().to_rfc3339(),
    )?;
    if !admitted {
        return Err(AppError::Unavailable(
            "shared document changed before access dispatch".into(),
        ));
    }
    Ok((
        OrgDispatchPermit {
            dispatch_id: dispatch_id.clone(),
            host: host.to_string(),
            operation: OrgDispatchOperation::Access {
                org_id: org_id.to_string(),
                doc_id: doc_id.to_string(),
                access,
                owner_user_id: owner_user_id.to_string(),
            },
        },
        dispatch_id,
    ))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_org_access_attempt(
    state: &AppState,
    dispatch_id: &str,
    org_id: &str,
    doc_id: &str,
    old_access: crate::share::org_dto::OrgItemAccess,
    new_access: crate::share::org_dto::OrgItemAccess,
    actor_user_id: &str,
    owner_user_id: &str,
) -> Result<bool, AppError> {
    state.db.apply_org_access_attempt_if_current(
        dispatch_id,
        org_id,
        doc_id,
        old_access.as_str(),
        new_access.as_str(),
        actor_user_id,
        owner_user_id,
    )
}

#[allow(clippy::too_many_arguments)]
fn fail_org_access_attempt(
    state: &AppState,
    dispatch_id: &str,
    org_id: &str,
    doc_id: &str,
    old_access: crate::share::org_dto::OrgItemAccess,
    new_access: crate::share::org_dto::OrgItemAccess,
    actor_user_id: &str,
    owner_user_id: &str,
) -> Result<(), AppError> {
    let _ = state.db.fail_org_access_attempt_if_current(
        dispatch_id,
        org_id,
        doc_id,
        old_access.as_str(),
        new_access.as_str(),
        actor_user_id,
        owner_user_id,
    )?;
    Ok(())
}

pub(crate) type PendingOrgAccessAttempt = crate::storage::org_store::OrgAccessAttemptRow;

pub(crate) fn pending_org_access_attempts(
    state: &AppState,
) -> Result<Vec<PendingOrgAccessAttempt>, AppError> {
    state.db.pending_org_access_attempts()
}

pub(crate) fn apply_authoritative_org_access(
    state: &AppState,
    attempt: &PendingOrgAccessAttempt,
    head: &crate::share::org_dto::OrgItemEntry,
) -> Result<bool, AppError> {
    if head.doc_id.as_deref() != Some(attempt.doc_id.as_str())
        || head.tombstoned
        || head.is_current != Some(true)
        || head.document_owner_user_id.as_deref() != Some(attempt.owner_user_id.as_str())
    {
        return Ok(false);
    }
    state
        .db
        .apply_authoritative_org_access_if_current(attempt, head.access.as_str())
}

async fn reconcile_pending_org_access_attempt(
    state: &AppState,
    attempt: &PendingOrgAccessAttempt,
) -> Result<bool, AppError> {
    let (access_token, actor) = authenticated_org_actor(state).await?;
    if actor != attempt.actor_user_id {
        return Ok(false);
    }
    let base = share_base_url(state)?;
    let client = crate::share::client::ShareClient::new(&base)?;
    let Some(head) = authoritative_org_document_head(
        state,
        &client,
        &access_token,
        &attempt.org_id,
        &attempt.doc_id,
    )
    .await?
    else {
        return Ok(false);
    };
    apply_authoritative_org_access(state, attempt, &head)
}

fn permit_org_read(
    state: &AppState,
    host: &str,
    org_id: &str,
    doc_id: &str,
    purpose: OrgReadPurpose,
    since_seq: u64,
    limit: u32,
) -> Result<OrgReadPermit, AppError> {
    require_org_egress_consent(state)?;
    let dispatch_id = uuid::Uuid::new_v4().to_string();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    let mut conn = state.db.lock();
    let tx = conn
        .transaction()
        .map_err(|_| AppError::Storage("start org recovery read dispatch".into()))?;
    crate::storage::db::insert_org_read_egress_dispatch_tx(
        &tx,
        ts,
        host,
        match purpose {
            OrgReadPurpose::CurrentHead => "org_document_head_read",
            OrgReadPurpose::History => "org_document_history_read",
            OrgReadPurpose::Delete404Corroboration => "org_document_delete_404_read",
            OrgReadPurpose::Tombstone404Corroboration => "org_item_tombstone_404_read",
        },
        &dispatch_id,
        org_id,
        doc_id,
        since_seq,
        limit,
    )?;
    tx.commit()
        .map_err(|_| AppError::Storage("commit org recovery read dispatch".into()))?;
    Ok(OrgReadPermit {
        dispatch_id,
        host: host.to_string(),
        org_id: org_id.to_string(),
        doc_id: doc_id.to_string(),
        purpose,
        since_seq,
        limit,
    })
}

fn permit_org_task_assignee_read(
    state: &AppState,
    host: &str,
    org_id: &str,
) -> Result<OrgMembersReadPermit, AppError> {
    require_org_egress_consent(state)?;
    let dispatch_id = uuid::Uuid::new_v4().to_string();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    let mut conn = state.db.lock();
    let tx = conn
        .transaction()
        .map_err(|_| AppError::Storage("start task assignee read dispatch".into()))?;
    let ledger_id = crate::storage::db::insert_share_egress_dispatch_tx(
        &tx,
        ts,
        host,
        "org_task_assignee_read",
        0,
        &dispatch_id,
    )?;
    tx.execute(
        "UPDATE share_egress_log SET org_id=?2 WHERE id=?1 AND dispatch_id=?3",
        rusqlite::params![ledger_id, org_id, dispatch_id],
    )
    .map_err(|_| AppError::Storage("bind task assignee read dispatch".into()))?;
    tx.commit()
        .map_err(|_| AppError::Storage("commit task assignee read dispatch".into()))?;
    Ok(OrgMembersReadPermit {
        dispatch_id,
        host: host.to_string(),
        org_id: org_id.to_string(),
    })
}

pub(crate) fn permit_simple_org_dispatch(
    state: &AppState,
    host: &str,
    kind: &str,
    operation: OrgDispatchOperation,
) -> Result<OrgDispatchPermit, AppError> {
    require_org_egress_consent(state)?;
    let dispatch_id = uuid::Uuid::new_v4().to_string();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    let mut conn = state.db.lock();
    let tx = conn
        .transaction()
        .map_err(|_| AppError::Storage("start org dispatch".into()))?;
    crate::storage::db::insert_share_egress_dispatch_tx(&tx, ts, host, kind, 0, &dispatch_id)?;
    tx.commit()
        .map_err(|_| AppError::Storage("commit org dispatch".into()))?;
    Ok(OrgDispatchPermit {
        dispatch_id,
        host: host.to_string(),
        operation,
    })
}

fn persist_org_revoke_dispatch(
    state: &AppState,
    row_id: &str,
    updated_at: &str,
    host: &str,
    operation: OrgDispatchOperation,
) -> Result<OrgDispatchPermit, AppError> {
    require_org_egress_consent(state)?;
    let dispatch_id = uuid::Uuid::new_v4().to_string();
    let mut conn = state.db.lock();
    let tx = conn
        .transaction()
        .map_err(|_| AppError::Storage("start org revoke dispatch".into()))?;
    let changed = tx
        .execute(
            "UPDATE org_shares
                SET state = 'revoke_pending', dispatch_id = ?2, updated_at = ?3
              WHERE id = ?1 AND state IN ('uploaded', 'failed', 'revoke_pending')",
            rusqlite::params![row_id, dispatch_id, updated_at],
        )
        .map_err(|_| AppError::Storage("persist org revoke dispatch".into()))?;
    if changed != 1 {
        return Err(AppError::Unavailable(
            "org share changed before revoke dispatch".into(),
        ));
    }
    let ledger_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    crate::storage::db::insert_share_egress_dispatch_tx(
        &tx,
        ledger_ts,
        host,
        "org_share_revoke",
        0,
        &dispatch_id,
    )?;
    tx.commit()
        .map_err(|_| AppError::Storage("commit org revoke dispatch".into()))?;
    Ok(OrgDispatchPermit {
        dispatch_id,
        host: host.to_string(),
        operation,
    })
}

fn persist_org_revoke_intent(
    state: &AppState,
    row_id: &str,
    updated_at: &str,
) -> Result<(), AppError> {
    let conn = state.db.lock();
    let changed = conn
        .execute(
            "UPDATE org_shares
                SET state = 'revoke_pending', dispatch_id = NULL, updated_at = ?2
              WHERE id = ?1 AND state IN ('uploaded', 'failed', 'revoke_pending')",
            rusqlite::params![row_id, updated_at],
        )
        .map_err(|_| AppError::Storage("persist org revoke intent".into()))?;
    if changed != 1 {
        return Err(AppError::Unavailable(
            "org share changed before revoke intent".into(),
        ));
    }
    Ok(())
}

pub(crate) fn require_org_egress_consent(state: &AppState) -> Result<(), AppError> {
    let consented = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?
        .org_egress_consented;
    if !consented {
        return Err(AppError::Unavailable(crate::errcode::tag(
            crate::errcode::ORG_CONSENT,
            "confirm the one-time upload notice first",
        )));
    }
    Ok(())
}

struct OrgRepublishAttempt {
    permit: OrgDispatchPermit,
    dispatch_id: String,
}

#[allow(clippy::too_many_arguments)]
fn persist_org_republish_intent(
    state: &AppState,
    row: &crate::storage::OrgShareRow,
    target_rev: u32,
    generation: u32,
    content_sha256: &[u8],
    doc_id: Option<&str>,
    pending_reason: &str,
    updated_at: &str,
    ledger_host: &str,
    sealed_bytes: usize,
    operation: OrgDispatchOperation,
    expected_actor_user_id: &str,
    expected_owner_user_id: &str,
    observed_source_version: u64,
    observed_dirty_counter: u64,
) -> Result<OrgRepublishAttempt, AppError> {
    require_org_egress_consent(state)?;
    let dispatch_id = uuid::Uuid::new_v4().to_string();
    let mut conn = state.db.lock();
    let tx = conn
        .transaction()
        .map_err(|_| AppError::Storage("start org republish dispatch".into()))?;
    let changed = tx
        .execute(
            "UPDATE org_shares
                SET state = 'failed', last_error = ?2, rev = ?3, generation = ?4,
                    content_sha256 = ?5, scrub = 1, doc_id = COALESCE(?6, doc_id),
                    dispatch_id = ?11, expected_actor_user_id = ?12,
                    expected_owner_user_id = ?13, republish_dirty = 0,
                    updated_at = ?7
              WHERE id = ?1 AND item_id IS ?8 AND rev = ?9
                AND content_sha256 IS ?10 AND org_id = ?14
                AND meeting_id IS ?15 AND document_id IS ?16
                AND doc_id IS ?17 AND access = ?18 AND generation = ?19
                AND source_version = ?20 AND republish_dirty = ?21
                AND state = ?22 AND last_error IS ?23",
            rusqlite::params![
                row.id,
                pending_reason,
                target_rev as i64,
                generation as i64,
                content_sha256,
                doc_id,
                updated_at,
                row.item_id,
                row.rev as i64,
                row.content_sha256,
                dispatch_id,
                expected_actor_user_id,
                expected_owner_user_id,
                row.org_id,
                row.meeting_id,
                row.document_id,
                row.doc_id,
                row.access,
                row.generation as i64,
                observed_source_version as i64,
                observed_dirty_counter as i64,
                row.state,
                row.last_error,
            ],
        )
        .map_err(|_| AppError::Storage("persist org republish intent".into()))?;
    if changed != 1 {
        return Err(AppError::Unavailable(
            "org share changed before republish intent could be persisted".into(),
        ));
    }
    let ledger_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    crate::storage::db::insert_share_egress_dispatch_tx(
        &tx,
        ledger_ts,
        ledger_host,
        "org_share_publish",
        sealed_bytes,
        &dispatch_id,
    )?;
    tx.commit()
        .map_err(|_| AppError::Storage("commit org republish dispatch".into()))?;
    Ok(OrgRepublishAttempt {
        permit: OrgDispatchPermit {
            dispatch_id: dispatch_id.clone(),
            host: ledger_host.to_string(),
            operation,
        },
        dispatch_id,
    })
}

fn fail_republish_pre_dispatch_if_current(
    state: &AppState,
    row: &crate::storage::OrgShareRow,
    error: &str,
    observed_source_version: u64,
    observed_dirty_counter: u64,
    updated_at: &str,
) -> Result<bool, AppError> {
    let conn = state.db.lock();
    let changed = conn
        .execute(
            "UPDATE org_shares SET state='failed', last_error=?2,
            republish_dirty=CASE WHEN ?2='too_large' THEN 0 ELSE republish_dirty END,
            updated_at=?3
          WHERE id=?1 AND org_id=?4 AND meeting_id IS ?5 AND document_id IS ?6
            AND item_id IS ?7 AND doc_id IS ?8 AND access=?9 AND rev=?10
            AND generation=?11 AND content_sha256 IS ?12 AND state=?13
            AND last_error IS ?14 AND source_version=?15 AND republish_dirty=?16",
            rusqlite::params![
                row.id,
                error,
                updated_at,
                row.org_id,
                row.meeting_id,
                row.document_id,
                row.item_id,
                row.doc_id,
                row.access,
                row.rev as i64,
                row.generation as i64,
                row.content_sha256,
                row.state,
                row.last_error,
                observed_source_version as i64,
                observed_dirty_counter as i64
            ],
        )
        .map_err(|_| AppError::Storage("fail org republish pre-dispatch".into()))?;
    Ok(changed == 1)
}

#[allow(clippy::too_many_arguments)]
fn conflict_republish_put_intent(
    state: &AppState,
    row_id: &str,
    expected_doc_id: &str,
    expected_access: crate::share::org_dto::OrgItemAccess,
    expected_actor_user_id: &str,
    expected_owner_user_id: &str,
    expected_predecessor_item_id: &str,
    target_rev: u32,
    generation: u32,
    content_sha256: &[u8],
    expected_dispatch_id: &str,
    expected_pending_reason: &str,
    updated_at: &str,
) -> Result<(), AppError> {
    let conn = state.db.lock();
    let changed = conn
        .execute(
            "UPDATE org_shares SET last_error = ?2, updated_at = ?3
              WHERE id = ?1 AND state = 'failed' AND last_error = ?4
                AND doc_id = ?5 AND access = ?6 AND rev = ?7 AND generation = ?8
                AND content_sha256 = ?9 AND expected_actor_user_id = ?10
                AND expected_owner_user_id = ?11 AND item_id = ?12 AND dispatch_id = ?13",
            rusqlite::params![
                row_id,
                ORG_SHARE_ERR_EDIT_CONFLICT,
                updated_at,
                expected_pending_reason,
                expected_doc_id,
                expected_access.as_str(),
                target_rev as i64,
                generation as i64,
                content_sha256,
                expected_actor_user_id,
                expected_owner_user_id,
                expected_predecessor_item_id,
                expected_dispatch_id,
            ],
        )
        .map_err(|_| AppError::Storage("conflict org republish PUT intent".into()))?;
    if changed != 1 {
        return Err(AppError::Unavailable(
            "org republish PUT attempt changed before conflict resolution".into(),
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn confirm_org_mutation_for_projection(
    state: &AppState,
    row: &crate::storage::OrgShareRow,
    published: &crate::share::org_dto::PublishItemResponse,
    expected_pending_reason: &str,
    expected_dispatch_id: &str,
    updated_at: &str,
) -> Result<(), AppError> {
    let conn = state.db.lock();
    let changed = conn
        .execute(
            "UPDATE org_shares SET item_id=?2, last_error=?3, updated_at=?4
          WHERE id=?1 AND state='failed' AND last_error=?5 AND item_id IS ?6
            AND org_id=?7 AND doc_id IS ?8 AND access=?9 AND rev=?10 AND generation=?11
            AND content_sha256 IS ?12 AND expected_actor_user_id IS ?13
            AND expected_owner_user_id IS ?14 AND dispatch_id=?15",
            rusqlite::params![
                row.id,
                published.item_id,
                ORG_SHARE_PROJECTION_PENDING,
                updated_at,
                expected_pending_reason,
                row.item_id,
                row.org_id,
                row.doc_id,
                row.access,
                row.rev as i64,
                row.generation as i64,
                row.content_sha256,
                row.expected_actor_user_id,
                row.expected_owner_user_id,
                expected_dispatch_id
            ],
        )
        .map_err(|_| AppError::Storage("confirm org mutation for projection".into()))?;
    if changed != 1 {
        return Err(AppError::Unavailable(
            "org mutation changed before projection confirmation".into(),
        ));
    }
    Ok(())
}

/// Bind one direct stable-document PUT before dispatch. The existing SQLCipher `org_shares` row is
/// the durable attempt journal on the origin device; a second device creates a source-less row that
/// is deliberately invisible to source/title IPC reads. `target_rev - 1` is the exact expected rev,
/// while `item_id` retains the expected head witness and the hash commits the complete sealed
/// envelope (title/body/author/created/source metadata). No plaintext body is persisted here.
#[allow(clippy::too_many_arguments)]
fn persist_direct_org_update_intent(
    state: &AppState,
    existing: Option<&crate::storage::OrgShareRow>,
    org_id: &str,
    doc_id: &str,
    expected_item_id: &str,
    expected_rev: u32,
    target_rev: u32,
    generation: u32,
    content_sha256: &[u8],
    access: crate::share::org_dto::OrgItemAccess,
    expected_actor_user_id: &str,
    expected_owner_user_id: &str,
    title: &str,
    updated_at: &str,
    ledger_host: &str,
    sealed_bytes: usize,
    sealed_cell_sha256: [u8; 32],
) -> Result<(String, OrgDispatchPermit, String), AppError> {
    require_org_egress_consent(state)?;
    if existing.is_some_and(|row| {
        matches!(
            row.last_error.as_deref(),
            Some(
                ORG_SHARE_DIRECT_PUT_PENDING
                    | ORG_SHARE_REPUBLISH_PUT_PENDING
                    | ORG_SHARE_ERR_EDIT_CONFLICT
            )
        )
    }) {
        return Err(AppError::Unavailable(
            "shared document has an unresolved edit attempt".into(),
        ));
    }
    let row_id = existing
        .map(|row| row.id.clone())
        .unwrap_or_else(crate::share::new_share_id);
    let existing_dispatch_id = match existing {
        Some(row) => state.db.org_share_dispatch_id(&row.id)?,
        None => None,
    };
    let dispatch_id = uuid::Uuid::new_v4().to_string();
    let mut conn = state.db.lock();
    let tx = conn
        .transaction()
        .map_err(|_| AppError::Storage("start direct org update dispatch".into()))?;
    if let Some(row) = existing {
        let changed = tx
            .execute(
                "UPDATE org_shares
                    SET state = 'failed', last_error = ?2, title = ?3, rev = ?4,
                        generation = ?5, content_sha256 = ?6, doc_id = ?7, access = ?8,
                        expected_actor_user_id = ?9, expected_owner_user_id = ?10,
                        dispatch_id = ?14, updated_at = ?11
                  WHERE id = ?1 AND item_id = ?12 AND rev = ?13
                    AND org_id = ?15 AND meeting_id IS ?16 AND document_id IS ?17
                    AND doc_id IS ?18 AND access = ?19 AND generation = ?20
                    AND content_sha256 IS ?21 AND state = ?22 AND last_error IS ?23
                    AND dispatch_id IS ?24",
                rusqlite::params![
                    row_id,
                    ORG_SHARE_DIRECT_PUT_PENDING,
                    title,
                    target_rev as i64,
                    generation as i64,
                    content_sha256,
                    doc_id,
                    access.as_str(),
                    expected_actor_user_id,
                    expected_owner_user_id,
                    updated_at,
                    expected_item_id,
                    expected_rev as i64,
                    dispatch_id,
                    row.org_id,
                    row.meeting_id,
                    row.document_id,
                    row.doc_id,
                    row.access,
                    row.generation as i64,
                    row.content_sha256,
                    row.state,
                    row.last_error,
                    existing_dispatch_id,
                ],
            )
            .map_err(|_| AppError::Storage("persist direct org update intent".into()))?;
        if changed != 1 {
            return Err(AppError::Unavailable(
                "org document changed before the edit attempt could be persisted".into(),
            ));
        }
    } else {
        tx.execute(
            "INSERT INTO org_shares
           (id, org_id, meeting_id, document_id, kind, title, rev, generation,
           content_sha256, item_id, doc_id, access, scrub, state, last_error,
            expected_actor_user_id, expected_owner_user_id, dispatch_id,
            created_at, updated_at)
         VALUES (?1, ?2, NULL, NULL, 'note', ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1,
                 'failed', ?10, ?11, ?12, ?14, ?13, ?13)",
            rusqlite::params![
                row_id,
                org_id,
                title,
                target_rev as i64,
                generation as i64,
                content_sha256,
                expected_item_id,
                doc_id,
                access.as_str(),
                ORG_SHARE_DIRECT_PUT_PENDING,
                expected_actor_user_id,
                expected_owner_user_id,
                updated_at,
                dispatch_id,
            ],
        )
        .map_err(|_| AppError::Storage("persist direct org update intent".into()))?;
    }
    let ledger_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    crate::storage::db::insert_share_egress_dispatch_tx(
        &tx,
        ledger_ts,
        ledger_host,
        "org_share_publish",
        sealed_bytes,
        &dispatch_id,
    )?;
    tx.commit()
        .map_err(|_| AppError::Storage("commit direct org update dispatch".into()))?;
    Ok((
        row_id,
        OrgDispatchPermit {
            dispatch_id: dispatch_id.clone(),
            host: ledger_host.to_string(),
            operation: OrgDispatchOperation::Update {
                org_id: org_id.to_string(),
                doc_id: doc_id.to_string(),
                expected_rev,
                generation,
                content_sha256: content_sha256.to_vec(),
                cell_len: sealed_bytes,
                cell_sha256: sealed_cell_sha256,
                access,
                owner_user_id: expected_owner_user_id.to_string(),
            },
        },
        dispatch_id,
    ))
}

#[allow(dead_code, clippy::too_many_arguments)]
fn complete_direct_org_update_intent(
    state: &AppState,
    row_id: &str,
    published: &crate::share::org_dto::PublishItemResponse,
    expected_doc_id: &str,
    expected_access: crate::share::org_dto::OrgItemAccess,
    expected_actor_user_id: &str,
    expected_owner_user_id: &str,
    target_rev: u32,
    generation: u32,
    content_sha256: &[u8],
    expected_predecessor_item_id: &str,
    expected_dispatch_id: &str,
    discard_source_less_anchor: bool,
    updated_at: &str,
) -> Result<(), AppError> {
    if published.doc_id.as_deref() != Some(expected_doc_id)
        || published.access != expected_access
        || published.document_owner_user_id.as_deref() != Some(expected_owner_user_id)
    {
        return Err(AppError::Unavailable(
            "direct org update returned inconsistent document metadata".into(),
        ));
    }
    let mut conn = state.db.lock();
    let tx = conn
        .transaction()
        .map_err(|_| AppError::Storage("start direct org update completion".into()))?;
    if discard_source_less_anchor {
        let changed = tx
            .execute(
                "DELETE FROM org_shares
                  WHERE id = ?1 AND state = 'failed' AND last_error = ?2
                    AND rev = ?3 AND generation = ?4 AND content_sha256 = ?5
                    AND doc_id = ?6 AND access = ?7
                    AND expected_actor_user_id = ?8 AND expected_owner_user_id = ?9
                    AND item_id = ?10 AND dispatch_id = ?11",
                rusqlite::params![
                    row_id,
                    ORG_SHARE_DIRECT_PUT_PENDING,
                    target_rev as i64,
                    generation as i64,
                    content_sha256,
                    expected_doc_id,
                    expected_access.as_str(),
                    expected_actor_user_id,
                    expected_owner_user_id,
                    expected_predecessor_item_id,
                    expected_dispatch_id,
                ],
            )
            .map_err(|_| AppError::Storage("complete direct org update intent".into()))?;
        if changed != 1 {
            return Err(AppError::Unavailable(
                "direct org update attempt changed before completion".into(),
            ));
        }
    } else {
        let changed = tx
            .execute(
                "UPDATE org_shares
                    SET state = CASE WHEN ?10 THEN 'revoked' ELSE 'uploaded' END,
                        last_error = NULL, item_id = CASE WHEN ?10 THEN NULL ELSE ?2 END, doc_id = ?3,
                        access = ?4, updated_at = ?5
                  WHERE id = ?1 AND state = 'failed' AND last_error = ?6
                    AND rev = ?7 AND generation = ?8 AND content_sha256 = ?9
                    AND doc_id = ?11 AND access = ?12
                    AND expected_actor_user_id = ?13 AND expected_owner_user_id = ?14
                    AND item_id = ?15 AND dispatch_id = ?16",
                rusqlite::params![
                    row_id,
                    published.item_id,
                    expected_doc_id,
                    expected_access.as_str(),
                    updated_at,
                    ORG_SHARE_DIRECT_PUT_PENDING,
                    target_rev as i64,
                    generation as i64,
                    content_sha256,
                    discard_source_less_anchor,
                    expected_doc_id,
                    expected_access.as_str(),
                    expected_actor_user_id,
                    expected_owner_user_id,
                    expected_predecessor_item_id,
                    expected_dispatch_id,
                ],
            )
            .map_err(|_| AppError::Storage("complete direct org update intent".into()))?;
        if changed != 1 {
            return Err(AppError::Unavailable(
                "direct org update attempt changed before completion".into(),
            ));
        }
    }
    tx.commit()
        .map_err(|_| AppError::Storage("commit direct org update completion".into()))?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn conflict_direct_org_update_intent(
    state: &AppState,
    row_id: &str,
    expected_doc_id: &str,
    expected_access: crate::share::org_dto::OrgItemAccess,
    expected_actor_user_id: &str,
    expected_owner_user_id: &str,
    expected_predecessor_item_id: &str,
    target_rev: u32,
    generation: u32,
    content_sha256: &[u8],
    expected_dispatch_id: &str,
    updated_at: &str,
) -> Result<(), AppError> {
    let conn = state.db.lock();
    let changed = conn
        .execute(
            "UPDATE org_shares SET last_error = ?2, updated_at = ?3
              WHERE id = ?1 AND state = 'failed' AND last_error = ?4
                AND doc_id = ?5 AND access = ?6 AND rev = ?7 AND generation = ?8
                AND content_sha256 = ?9 AND expected_actor_user_id = ?10
                AND expected_owner_user_id = ?11 AND item_id = ?12 AND dispatch_id = ?13",
            rusqlite::params![
                row_id,
                ORG_SHARE_ERR_EDIT_CONFLICT,
                updated_at,
                ORG_SHARE_DIRECT_PUT_PENDING,
                expected_doc_id,
                expected_access.as_str(),
                target_rev as i64,
                generation as i64,
                content_sha256,
                expected_actor_user_id,
                expected_owner_user_id,
                expected_predecessor_item_id,
                expected_dispatch_id,
            ],
        )
        .map_err(|_| AppError::Storage("conflict direct org update intent".into()))?;
    if changed != 1 {
        return Err(AppError::Unavailable(
            "direct org update changed before conflict resolution".into(),
        ));
    }
    Ok(())
}

enum DirectOrgUpdateResolution {
    Exact(crate::share::org_dto::PublishItemResponse),
    Inconclusive,
    Conflict,
}

#[derive(Default)]
struct AuthoritativeOrgDocumentScan {
    saw_document: bool,
    current: Option<crate::share::org_dto::OrgItemEntry>,
    history: Vec<crate::share::org_dto::OrgItemEntry>,
}

/// Walk the authenticated metadata feed without exposing an unledgered pagination path. A fresh
/// consent check, durable content-free receipt, and exact single-use permit are created immediately
/// before each actual page GET; a failed page still has exactly one receipt.
async fn authoritative_org_document_scan(
    state: &AppState,
    client: &crate::share::client::ShareClient,
    access_token: &str,
    org_id: &str,
    doc_id: &str,
    purpose: OrgReadPurpose,
) -> Result<AuthoritativeOrgDocumentScan, AppError> {
    const PAGE: u32 = 200;
    const MAX_PAGES: usize = 128;
    let mut since = 0u64;
    let mut scan = AuthoritativeOrgDocumentScan::default();
    for _ in 0..MAX_PAGES {
        let permit = permit_org_read(state, &client.host(), org_id, doc_id, purpose, since, PAGE)?;
        let page = client
            .org_document_recovery_page(access_token, org_id, doc_id, purpose, since, PAGE, permit)
            .await?;
        page.validate_authoritative_metadata().map_err(|_| {
            AppError::Unavailable(
                "org-document-head: feed omitted authoritative document metadata".into(),
            )
        })?;
        for item in &page.items {
            if item.doc_id.as_deref() != Some(doc_id) {
                continue;
            }
            scan.saw_document = true;
            scan.history.push(item.clone());
            if item.is_current == Some(true) && !item.tombstoned {
                if scan.current.is_some() {
                    return Err(AppError::Unavailable(
                        "org-document-head: feed returned multiple current heads".into(),
                    ));
                }
                scan.current = Some(item.clone());
            }
        }
        if page.items.len() < PAGE as usize {
            return Ok(scan);
        }
        if page.next_seq <= since {
            return Err(AppError::Unavailable(
                "org-document-head: full feed page did not advance its cursor".into(),
            ));
        }
        since = page.next_seq;
    }
    Err(AppError::Unavailable(
        "org-document-head: feed scan exceeded its safety bound".into(),
    ))
}

async fn authoritative_org_document_head(
    state: &AppState,
    client: &crate::share::client::ShareClient,
    access_token: &str,
    org_id: &str,
    doc_id: &str,
) -> Result<Option<crate::share::org_dto::OrgItemEntry>, AppError> {
    Ok(authoritative_org_document_scan(
        state,
        client,
        access_token,
        org_id,
        doc_id,
        OrgReadPurpose::CurrentHead,
    )
    .await?
    .current)
}

#[cfg(test)]
pub(crate) async fn authoritative_org_document_head_inner(
    state: &AppState,
    org_id: &str,
    doc_id: &str,
) -> Result<Option<crate::share::org_dto::OrgItemEntry>, AppError> {
    let base = share_base_url(state)?;
    let (access_token, _) = authenticated_org_actor(state).await?;
    let client = crate::share::client::ShareClient::new(&base)?;
    authoritative_org_document_head(state, &client, &access_token, org_id, doc_id).await
}

async fn delete_stable_org_document(
    state: &AppState,
    client: &crate::share::client::ShareClient,
    access_token: &str,
    org_id: &str,
    doc_id: &str,
    permit: OrgDispatchPermit,
) -> Result<(), AppError> {
    match client
        .org_delete_document(access_token, org_id, doc_id, permit)
        .await?
    {
        crate::share::client::OrgDeleteDocumentResult::Deleted => Ok(()),
        crate::share::client::OrgDeleteDocumentResult::NotFound => {
            let scan = authoritative_org_document_scan(
                state,
                client,
                access_token,
                org_id,
                doc_id,
                OrgReadPurpose::Delete404Corroboration,
            )
            .await?;
            if scan.saw_document && scan.current.is_none() {
                Ok(())
            } else {
                Err(AppError::Unavailable(
                    "org-delete-document: 404 was not corroborated by authoritative history".into(),
                ))
            }
        }
    }
}

async fn corroborate_legacy_org_item_absent_or_tombstoned(
    state: &AppState,
    client: &crate::share::client::ShareClient,
    access_token: &str,
    org_id: &str,
    item_id: &str,
) -> Result<bool, AppError> {
    const PAGE: u32 = 200;
    const MAX_PAGES: usize = 128;
    let mut since = 0u64;
    for _ in 0..MAX_PAGES {
        let permit = permit_org_read(
            state,
            &client.host(),
            org_id,
            item_id,
            OrgReadPurpose::Tombstone404Corroboration,
            since,
            PAGE,
        )?;
        let page = client
            .org_document_recovery_page(
                access_token,
                org_id,
                item_id,
                OrgReadPurpose::Tombstone404Corroboration,
                since,
                PAGE,
                permit,
            )
            .await?;
        if let Some(item) = page.items.iter().find(|item| item.item_id == item_id) {
            return Ok(item.tombstoned);
        }
        if page.items.len() < PAGE as usize {
            return Ok(true);
        }
        if page.next_seq <= since {
            return Err(AppError::Unavailable(
                "org-tombstone-item: feed scan did not advance".into(),
            ));
        }
        since = page.next_seq;
    }
    Err(AppError::Unavailable(
        "org-tombstone-item: feed scan exceeded its safety bound".into(),
    ))
}

pub(crate) async fn delete_legacy_org_item(
    state: &AppState,
    client: &crate::share::client::ShareClient,
    access_token: &str,
    org_id: &str,
    item_id: &str,
    permit: OrgDispatchPermit,
) -> Result<(), AppError> {
    match client
        .org_tombstone_item(access_token, org_id, item_id, permit)
        .await?
    {
        crate::share::client::OrgTombstoneItemResult::Deleted => Ok(()),
        crate::share::client::OrgTombstoneItemResult::NotFound => {
            if corroborate_legacy_org_item_absent_or_tombstoned(
                state,
                client,
                access_token,
                org_id,
                item_id,
            )
            .await?
            {
                Ok(())
            } else {
                Err(AppError::Unavailable(
                    "org-tombstone-item: 404 was not corroborated by authenticated history".into(),
                ))
            }
        }
    }
}

async fn reconcile_direct_org_update_attempt(
    state: &AppState,
    client: &crate::share::client::ShareClient,
    access_token: &str,
    row: &crate::storage::OrgShareRow,
    expected_actor_user_id: &str,
) -> Result<DirectOrgUpdateResolution, AppError> {
    let Some(doc_id) = row.doc_id.as_deref() else {
        return Ok(DirectOrgUpdateResolution::Conflict);
    };
    let Some(content_sha256) = row.content_sha256.as_deref() else {
        return Ok(DirectOrgUpdateResolution::Conflict);
    };
    let Some(access) = crate::share::org_dto::OrgItemAccess::parse(&row.access) else {
        return Ok(DirectOrgUpdateResolution::Conflict);
    };
    let Some(expected_item_id) = row.item_id.as_deref() else {
        return Ok(DirectOrgUpdateResolution::Conflict);
    };
    let Some(expected_owner_user_id) = row.expected_owner_user_id.as_deref() else {
        return Ok(DirectOrgUpdateResolution::Conflict);
    };
    let scan = authoritative_org_document_scan(
        state,
        client,
        access_token,
        &row.org_id,
        doc_id,
        OrgReadPurpose::History,
    )
    .await?;
    let predecessor_matches = row.rev.checked_sub(1).is_some_and(|expected_rev| {
        scan.history.iter().any(|item| {
            item.item_id == expected_item_id
                && item.doc_id.as_deref() == Some(doc_id)
                && item.rev == expected_rev
                && item.is_current == Some(false)
        })
    });
    if predecessor_matches {
        if let Some(target) = scan.current.as_ref().filter(|item| {
            authoritative_org_item_matches_attempt(
                item,
                doc_id,
                row.rev,
                row.generation,
                content_sha256,
                access,
                expected_actor_user_id,
                expected_owner_user_id,
            )
        }) {
            return Ok(DirectOrgUpdateResolution::Exact(
                publish_response_from_authoritative_head(target.clone()),
            ));
        }
        if scan.history.iter().any(|item| {
            authoritative_org_item_matches_attempt(
                item,
                doc_id,
                row.rev,
                row.generation,
                content_sha256,
                access,
                expected_actor_user_id,
                expected_owner_user_id,
            )
        }) {
            return Ok(DirectOrgUpdateResolution::Inconclusive);
        }
    }
    let incompatible_successor = scan.history.iter().any(|item| {
        item.doc_id.as_deref() == Some(doc_id)
            && (item.rev > row.rev
                || (item.rev == row.rev
                    && !authoritative_org_item_matches_attempt(
                        item,
                        doc_id,
                        row.rev,
                        row.generation,
                        content_sha256,
                        access,
                        expected_actor_user_id,
                        expected_owner_user_id,
                    )))
    });
    if incompatible_successor {
        Ok(DirectOrgUpdateResolution::Conflict)
    } else {
        Ok(DirectOrgUpdateResolution::Inconclusive)
    }
}

fn is_org_edit_conflict(error: &AppError) -> bool {
    matches!(error, AppError::InvalidArg(message) if message.starts_with(&format!(
        "[{}] ", crate::errcode::ORG_EDIT_CONFLICT
    )))
}

#[allow(clippy::too_many_arguments)]
fn authoritative_org_item_matches_attempt(
    head: &crate::share::org_dto::OrgItemEntry,
    doc_id: &str,
    target_rev: u32,
    generation: u32,
    content_sha256: &[u8],
    access: crate::share::org_dto::OrgItemAccess,
    expected_actor_user_id: &str,
    expected_owner_user_id: &str,
) -> bool {
    head.doc_id.as_deref() == Some(doc_id)
        && !head.tombstoned
        && head.rev == target_rev
        && head.generation == generation
        && head.content_sha256.as_deref() == Some(content_sha256)
        && head.access == access
        && head.author_user_id == expected_actor_user_id
        && head.document_owner_user_id.as_deref() == Some(expected_owner_user_id)
}

fn publish_response_from_authoritative_head(
    head: crate::share::org_dto::OrgItemEntry,
) -> crate::share::org_dto::PublishItemResponse {
    crate::share::org_dto::PublishItemResponse {
        item_id: head.item_id,
        seq: head.seq,
        doc_id: head.doc_id,
        access: head.access,
        document_owner_user_id: head.document_owner_user_id,
    }
}

fn validate_org_feed_page_metadata(
    feed: &crate::share::org_dto::OrgItemsResponse,
) -> Result<(), AppError> {
    feed.validate_authoritative_metadata()
        .and_then(|_| {
            feed.items
                .iter()
                .try_for_each(|item| match item.doc_id.as_deref() {
                    Some(doc_id) if crate::share::org_dto::parse_stable_uuid(doc_id).is_none() => {
                        Err("org feed carried an invalid durable document id")
                    }
                    _ => Ok(()),
                })
        })
        .map_err(|_| {
            AppError::Unavailable("org feed omitted authoritative durable-document metadata".into())
        })
}

fn resolved_org_item_is_current(item: &crate::share::org_dto::OrgItemEntry) -> bool {
    item.doc_id.as_ref().and(item.is_current).unwrap_or(false)
}

/// The org publish CORE, shared by the FIRST share (`share_to_org_inner`, `rev = 1`) and the
/// re-publish-on-edit supersede (`republish_org_shares_for_source`, `rev = old_rev + 1`). It owns the
/// FULL gate chain so a republish INHERITS it rather than re-implementing (the leak-safety single
/// seam): (1) read-gate + (3) clean + (4) scrub via `build_org_share_body`, (2) org-egress consent
/// fail-closed, (5) OCK seal + LOCAL open-verify-before-publish, (6) blob upload + publish item,
/// (7) content-free egress ledger. `rev` is stamped into BOTH the `OrgEnvelope` (source_rev) and the
/// `PublishItemRequest` so members see the supersede.
// Keep the complete sealed-publish witness explicit rather than hiding security inputs in a
// partially initialized options bag.
#[allow(clippy::too_many_arguments)]
async fn publish_org_body_with_policy(
    state: &AppState,
    org_id: &str,
    meeting_id: Option<String>,
    document_id: Option<String>,
    scrub: bool,
    rev: u32,
    access: crate::share::org_dto::OrgItemAccess,
    placement: Option<ContainerPlacement>,
    policy: OrgWorkPolicy,
    app: Option<&AppHandle>,
) -> Result<OrgShareEntry, AppError> {
    if !policy.is_current() {
        return Err(AppError::Unavailable(
            "background org publish deferred for recording".into(),
        ));
    }
    // (1) READ-GATE + (3) clean + (4) scrub — all inside one lifecycle-consistent snapshot
    // (read-gate FIRST). Its folder + seal epoch are rebound immediately before each egress call.
    let source =
        build_org_share_snapshot(state, meeting_id.as_deref(), document_id.as_deref(), scrub)?;
    let OrgShareBodySnapshot {
        title,
        markdown,
        created_at,
        counts: _,
        kind,
        attachment_owner,
        source_version,
    } = source;
    // `build_org_share_body` already enforces exactly one of meeting_id/document_id is `Some` (else it
    // errors before this line is reached), so this mirrors that same exclusivity to stamp the wire
    // envelope's SOURCE type (document vs meeting — a new axis, distinct from `kind`/content-shape).
    let source_kind = match kind {
        crate::share::org_envelope::OrgItemKind::Task => {
            let task = crate::share::task_envelope::TaskEnvelope::from_json(&markdown, org_id)?;
            if task.org_id != org_id {
                return Err(AppError::InvalidArg(
                    "task belongs to a different organization".into(),
                ));
            }
            crate::share::org_envelope::OrgSourceKind::Task
        }
        _ if meeting_id.is_some() => crate::share::org_envelope::OrgSourceKind::Meeting,
        _ => crate::share::org_envelope::OrgSourceKind::Document,
    };

    // (2) consent fail-closed (the global one-time org-egress consent).
    {
        let cfg = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
        if !cfg.org_egress_consented {
            return Err(AppError::Unavailable(crate::errcode::tag(
                crate::errcode::ORG_CONSENT,
                "confirm the one-time upload notice first",
            )));
        }
    }

    // MULTI-ORG: share into the FE-PICKED org (membership-checked), never the first via `.next()`.
    // This is a destructive EGRESS op — misrouting it to org #1 published a member's note into the
    // WRONG org (the root of the "B shared into Siema but it went to org #1" bug). `resolve_org`
    // refuses a blank id / an org the caller isn't a local member of.
    let org = resolve_org(state, org_id)?;
    let base = share_base_url(state)?;
    let (access_token, publisher_user_id) = authenticated_org_actor(state).await?;
    if !policy.is_current() {
        return Err(AppError::Unavailable(
            "background org publish deferred for recording".into(),
        ));
    }
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
    if kind == crate::share::org_envelope::OrgItemKind::Task {
        crate::share::task_envelope::TaskEnvelope::from_json(&markdown, org_id)?;
    }
    let (markdown, attachments) = if kind == crate::share::org_envelope::OrgItemKind::Task {
        task_attachment_bundle_for_markdown(state, &attachment_owner, org_id, &markdown)?
    } else {
        attachment_bundle_for_markdown(state, &attachment_owner, &markdown)?
    };
    let env = crate::share::org_envelope::OrgEnvelope::new(
        kind,
        title.clone(),
        markdown,
        author_hint,
        created_at,
        rev,
        source_kind,
    )
    .with_attachments(attachments)
    .with_placement(placement.as_ref().map(ContainerPlacement::envelope));
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
            let replay_witnesses =
                if existing.last_error.as_deref() == Some(ORG_SHARE_INITIAL_POST_REPLAYABLE) {
                    Some((
                        existing.expected_actor_user_id.as_deref().ok_or_else(|| {
                            AppError::Unavailable(
                                "ambiguous org publish is missing its actor witness".into(),
                            )
                        })?,
                        existing.expected_owner_user_id.as_deref().ok_or_else(|| {
                            AppError::Unavailable(
                                "ambiguous org publish is missing its owner witness".into(),
                            )
                        })?,
                    ))
                } else {
                    None
                };
            if let Some((expected_actor, expected_owner)) = replay_witnesses {
                if expected_actor != publisher_user_id
                    || expected_owner != publisher_user_id
                    || existing.access != access.as_str()
                {
                    return Err(AppError::Unavailable(
                        "ambiguous org publish actor, owner, or access changed before replay"
                            .into(),
                    ));
                }
            }
            if policy
                .commit(|| match replay_witnesses {
                    Some((expected_actor, expected_owner)) => {
                        state.db.reset_initial_org_share_for_replay(
                            &existing.id,
                            rev,
                            generation,
                            &content_sha,
                            scrub,
                            existing.doc_id.as_deref().ok_or_else(|| {
                                AppError::Unavailable(
                                    "ambiguous org publish is missing its document witness".into(),
                                )
                            })?,
                            &existing.access,
                            expected_actor,
                            expected_owner,
                            &publisher_user_id,
                            &now,
                        )
                    }
                    None => state.db.reset_org_share_for_retry(
                        &existing.id,
                        Some(&title),
                        rev,
                        generation,
                        &content_sha,
                        scrub,
                        &now,
                    ),
                })?
                .is_none()
            {
                return Err(AppError::Unavailable(
                    "background org publish deferred for recording".into(),
                ));
            }
            existing.id
        }
        None => {
            let row_id = crate::share::new_share_id();
            if policy.commit(|| {
                state.db.acquire_new_org_share_for_source(
                    &row_id,
                    &org.org_id,
                    meeting_id.as_deref(),
                    document_id.as_deref(),
                    kind.as_str(),
                    Some(&title),
                    rev,
                    generation,
                    &content_sha,
                    scrub,
                    &now,
                )
            })? != Some(true)
            {
                return Err(AppError::Unavailable(
                    "this source already has a live or pending share in the organization".into(),
                ));
            }
            row_id
        }
    };
    let doc_id = state
        .db
        .get_org_share(&row_id)?
        .and_then(|row| row.doc_id)
        .unwrap_or_else(|| {
            if kind == crate::share::org_envelope::OrgItemKind::Task {
                document_id
                    .clone()
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
            } else {
                uuid::Uuid::new_v4().to_string()
            }
        });
    state
        .db
        .set_org_share_document_metadata(&row_id, &doc_id, access.as_str())?;
    // Record the placement on the journal row BEFORE dispatch, so the republish path — which reads
    // the row, not the caller — rebuilds the identical envelope on every later revision.
    if let Some(placement) = placement.as_ref() {
        state.db.set_org_share_placement(
            &row_id,
            Some(&placement.parent_container_id),
            placement.position,
            placement.explicit,
        )?;
    }
    let (initial_row_version, initial_dirty_counter) =
        state.db.org_share_source_counters(&row_id)?;

    // (5) Seal under the OCK + LOCAL OPEN-VERIFY (the egress verify-before-destroy — publish only a
    // blob we just proved we can decrypt back).
    //
    // AAD ITEM NONCE = hex(content_sha256_of_plaintext), NOT the local row_id: the server assigns its
    // OWN item_id on publish (the client never controls it), so a per-publish LOCAL id would be
    // unknowable to any OTHER member syncing the feed → they could never open the cell. The content
    // hash is deterministic + rides the feed (`OrgItemEntry.content_sha256`), so every member
    // reconstructs the SAME AAD. (2026-07-10 cross-slice fix — see org_sync_now's open side.)
    let ock = acquire_org_ock_with_policy(state, &org.org_id, generation, policy).await?;
    if !policy.is_current() {
        return Err(AppError::Unavailable(
            "background org publish deferred for recording".into(),
        ));
    }
    let item_nonce = org_item_nonce(&content_sha);
    let (ciphertext, _sha) =
        match crate::share::org_envelope::seal_org_envelope(&ock, &env, &org.org_id, &item_nonce) {
            Ok(v) => v,
            Err(e) => {
                let _ = policy.commit(|| {
                    fail_initial_org_publish_pre_dispatch_if_current(
                        state,
                        &row_id,
                        "seal_failed",
                        &org.org_id,
                        meeting_id.as_deref(),
                        document_id.as_deref(),
                        &doc_id,
                        access,
                        rev,
                        generation,
                        &content_sha,
                        scrub,
                        source_version.content_version,
                        initial_row_version,
                        initial_dirty_counter,
                        &now,
                    )
                })?;
                return Err(e);
            }
        };

    // SIZE PRE-CHECK (Brain v3): the server hard-caps an org item blob at
    // `MAX_ORG_ITEM_BLOB_BYTES` — an oversized ciphertext would 413 on EVERY attempt, and the
    // launch sweep would requeue it forever (the poison loop). Fail CLIENT-SIDE, before any
    // egress, with the TERMINAL `too_large` reason the sweep excludes from retry. Sizes only —
    // never content.
    if ciphertext.len() > murmur_protocol::caps::MAX_ORG_ITEM_BLOB_BYTES {
        let _ = policy.commit(|| {
            fail_initial_org_publish_pre_dispatch_if_current(
                state,
                &row_id,
                ORG_SHARE_ERR_TOO_LARGE,
                &org.org_id,
                meeting_id.as_deref(),
                document_id.as_deref(),
                &doc_id,
                access,
                rev,
                generation,
                &content_sha,
                scrub,
                source_version.content_version,
                initial_row_version,
                initial_dirty_counter,
                &now,
            )
        })?;
        return Err(AppError::InvalidArg(format!(
            "this item is too large to share ({} bytes sealed; the org limit is {} bytes) — shorten it and share again",
            ciphertext.len(),
            murmur_protocol::caps::MAX_ORG_ITEM_BLOB_BYTES
        )));
    }

    // (6) publish the ciphertext inline. The server stores blob + item atomically, so a failed
    // request cannot leave an unbounded anonymous staged blob behind.
    require_current_org_share_snapshot(
        state,
        meeting_id.as_deref(),
        document_id.as_deref(),
        &source_version,
    )?;
    let (dispatch_permit, initial_dispatch_id) = if let Some(attempt) = policy.commit(|| {
        persist_initial_org_publish_intent(
            state,
            &row_id,
            &org.org_id,
            meeting_id.as_deref(),
            document_id.as_deref(),
            &doc_id,
            access,
            rev,
            generation,
            &content_sha,
            scrub,
            &publisher_user_id,
            &now,
            &client.host(),
            ciphertext.len(),
            org_dispatch_cell_sha256(&ciphertext),
            source_version.content_version,
            initial_row_version,
            initial_dirty_counter,
        )
    })? {
        attempt
    } else {
        return Err(AppError::Unavailable(
            "background org publish deferred for recording".into(),
        ));
    };
    let publish_result = client
        .org_publish_item(
            &access_token,
            &org.org_id,
            crate::share::org_dto::PublishItemRequest {
                // The currently deployed relay predates mutation receipts and rejects this new
                // field under `deny_unknown_fields`. Keep the durable local dispatch UUID, but omit
                // it on the wire until a deployed capability can be authenticated explicitly.
                mutation_id: None,
                doc_id: Some(doc_id.clone()),
                access: Some(access),
                blob_id: None,
                content_cell: Some(ciphertext),
                content_sha256: content_sha.clone(),
                rev,
                generation,
            },
            dispatch_permit,
        )
        .await;
    let published = match publish_result {
        Ok(published)
            if published.doc_id.as_deref() == Some(doc_id.as_str())
                && published.access == access
                && published.document_owner_user_id.as_deref()
                    == Some(publisher_user_id.as_str()) =>
        {
            published
        }
        Err(e) if matches!(e, AppError::InvalidArg(_)) && !is_org_edit_conflict(&e) => {
            let _ = policy.commit(|| {
                transition_initial_org_publish_intent(
                    state,
                    &row_id,
                    ORG_SHARE_PUBLISH_REJECTED,
                    &publisher_user_id,
                    &publisher_user_id,
                    &doc_id,
                    access,
                    rev,
                    generation,
                    &content_sha,
                    scrub,
                    &initial_dispatch_id,
                    &now,
                )
            })?;
            return Err(e);
        }
        Ok(_) | Err(AppError::Unavailable(_)) | Err(AppError::InvalidArg(_)) => {
            // A malformed/wrong 2xx, lost response, 5xx, or stable-document 409 may all follow a
            // committed POST. Accept only the authenticated exact head. If the proof is unavailable
            // or differs in any semantic/authority dimension, retain the row as a terminal conflict;
            // generic retry must never redispatch an ambiguous create.
            let reconciled = authoritative_org_document_head(
                state,
                &client,
                &access_token,
                &org.org_id,
                &doc_id,
            )
            .await;
            match reconciled {
                Ok(Some(head))
                    if head.rev == rev
                        && head.generation == generation
                        && head.content_sha256.as_deref() == Some(content_sha.as_slice())
                        && head.access == access
                        && head.author_user_id == publisher_user_id
                        && head.document_owner_user_id.as_deref()
                            == Some(publisher_user_id.as_str()) =>
                {
                    publish_response_from_authoritative_head(head)
                }
                Ok(Some(head)) if head.rev == rev => {
                    let _ = policy.commit(|| {
                        transition_initial_org_publish_intent(
                            state,
                            &row_id,
                            ORG_SHARE_ERR_EDIT_CONFLICT,
                            &publisher_user_id,
                            &publisher_user_id,
                            &doc_id,
                            access,
                            rev,
                            generation,
                            &content_sha,
                            scrub,
                            &initial_dispatch_id,
                            &now,
                        )
                    })?;
                    return Err(AppError::InvalidArg(crate::errcode::tag(
                        crate::errcode::ORG_EDIT_CONFLICT,
                        "the published document could not be proven to match this exact share",
                    )));
                }
                Ok(Some(_)) => {
                    // A later current revision does not disprove that this exact create committed:
                    // the deployed relay redacts retired content hashes. Until mutation receipts
                    // are explicitly deployed/capability-advertised, keep the immutable attempt
                    // pending rather than manufacturing either a replay or a conflict.
                    return Err(AppError::Unavailable(
                        "org publish outcome is pending authenticated reconciliation".into(),
                    ));
                }
                Ok(None) | Err(_) => {
                    return Err(AppError::Unavailable(
                        "org publish outcome is pending authenticated reconciliation".into(),
                    ));
                }
            }
        }
        Err(e) => {
            let _ = policy.commit(|| {
                transition_initial_org_publish_intent(
                    state,
                    &row_id,
                    ORG_SHARE_PUBLISH_REJECTED,
                    &publisher_user_id,
                    &publisher_user_id,
                    &doc_id,
                    access,
                    rev,
                    generation,
                    &content_sha,
                    scrub,
                    &initial_dispatch_id,
                    &now,
                )
            })?;
            return Err(e);
        }
    };
    if !policy.is_current() {
        return Err(AppError::Unavailable(
            "background org publish deferred for recording".into(),
        ));
    }
    let server_doc_id = published.doc_id.as_deref().ok_or_else(|| {
        AppError::Unavailable("stable org publish omitted its document id".into())
    })?;
    let initial_pending = state.db.get_org_share(&row_id)?.ok_or_else(|| {
        AppError::Storage("initial org publish attempt disappeared before projection".into())
    })?;
    let Some(_) = policy.commit(|| {
        confirm_org_mutation_for_projection(
            state,
            &initial_pending,
            &published,
            ORG_SHARE_INITIAL_POST_PENDING,
            &initial_dispatch_id,
            &now,
        )
    })?
    else {
        return Err(AppError::Unavailable(
            "background org publish deferred before projection".into(),
        ));
    };
    let (local_markdown, local_attachments) =
        prepare_incoming_attachment_bundle(&env.markdown, &env.attachments)?;
    let prepared = crate::storage::Db::prepare_org_item_index_for_kind(
        env.kind,
        &env.title,
        &env.created_at,
        &local_markdown,
        None,
    )?;
    let projected = policy.commit(|| {
        commit_org_metadata_mutation(
            state,
            app.map(|app| app as &dyn AskHistoryInvalidationNotifier),
            || {
                state.db.commit_org_republish_projection_if_current(
                    &row_id,
                    &initial_dispatch_id,
                    &org.org_id,
                    server_doc_id,
                    published.access.as_str(),
                    rev,
                    generation,
                    &content_sha,
                    &publisher_user_id,
                    &publisher_user_id,
                    Some(&published.item_id),
                    Some(ORG_SHARE_PROJECTION_PENDING),
                    false,
                    &crate::storage::org_store::OrgRepublishProjection {
                        item_id: &published.item_id,
                        seq: published.seq,
                        author_hint: &env.author_hint,
                        title: &env.title,
                        markdown: &local_markdown,
                        created_at: &env.created_at,
                        source_kind: env
                            .source_kind
                            .map(crate::share::org_envelope::OrgSourceKind::as_str),
                        author_user_id: Some(&publisher_user_id),
                        prepared: &prepared,
                        attachments: &local_attachments,
                    },
                )
            },
        )
    })?;
    if !projected.is_some_and(|outcome| outcome.changed) {
        return Err(AppError::Unavailable(
            "org publish is awaiting durable local projection".into(),
        ));
    }

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
///  - SCRUB INTENT: `org_shares.scrub` persists the original choice. Legacy rows migrate to fail-safe
///    scrub ON, while retries/edits preserve an explicit opt-out so semantic replay keeps the same
///    canonical content hash even though each ciphertext seal uses a fresh random nonce.
#[cfg(test)]
pub(crate) async fn republish_org_shares_for_source(
    state: &AppState,
    meeting_id: Option<&str>,
    document_id: Option<&str>,
) -> Result<u32, AppError> {
    let _mutation = state.lock_org_mutation().await;
    republish_org_shares_for_source_with_policy(
        state,
        meeting_id,
        document_id,
        OrgWorkPolicy::manual(),
        None,
    )
    .await
}

pub(crate) async fn republish_org_shares_for_source_notifying(
    state: &AppState,
    meeting_id: Option<&str>,
    document_id: Option<&str>,
    app: &AppHandle,
) -> Result<u32, AppError> {
    let _mutation = state.lock_org_mutation().await;
    republish_org_shares_for_source_with_policy(
        state,
        meeting_id,
        document_id,
        OrgWorkPolicy::manual(),
        Some(app),
    )
    .await
}

async fn republish_org_shares_for_source_with_policy(
    state: &AppState,
    meeting_id: Option<&str>,
    document_id: Option<&str>,
    policy: OrgWorkPolicy,
    app: Option<&AppHandle>,
) -> Result<u32, AppError> {
    if !policy.is_current() {
        return Ok(0);
    }
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
        if !policy.is_current() {
            return Ok(0);
        }
        let _ =
            collapse_org_share_dups_for_source(state, org_id, meeting_id, document_id, policy, app)
                .await;
        if !policy.is_current() {
            return Ok(0);
        }
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
    for mut row in rows {
        if !policy.is_current() {
            return Ok(republished);
        }
        let mut recovered_put = None;
        let mut recovered_actor = None;
        let mut recovered_dispatch_id = None;
        let mut durable_republish_owner = row.expected_owner_user_id.clone();
        // A direct stable-document PUT has already persisted an exact, query-only recovery witness.
        // A later local save must not overwrite that witness or dispatch a second mutation while the
        // first outcome is unknown; authenticated reconciliation is the only path that may advance it.
        if row.last_error.as_deref() == Some(ORG_SHARE_DIRECT_PUT_PENDING) {
            continue;
        }
        if row.last_error.as_deref() == Some(ORG_SHARE_ERR_EDIT_CONFLICT) {
            continue;
        }
        if row.last_error.as_deref() == Some("recovery_witness_missing") {
            continue;
        }
        if row.last_error.as_deref() == Some(ORG_SHARE_PROJECTION_PENDING) {
            // Remote success was already proven and mutation replay is forbidden. The normal
            // authenticated feed/reconcile transaction installs the complete projection and closes
            // this journal; it preserves republish_dirty so a newer source B becomes one later PUT.
            continue;
        }
        // A stable automatic republish PUT is exactly-once. Once dispatched, its durable row is a
        // query-only recovery journal: never re-read/re-seal the source and never send a second PUT.
        // Authenticated history either proves the exact successor, proves an incompatible successor,
        // or leaves the attempt pending for a later GET-only pass.
        if row.last_error.as_deref() == Some(ORG_SHARE_REPUBLISH_PUT_PENDING) {
            let Some(existing_dispatch_id) = state.db.org_share_dispatch_id(&row.id)? else {
                continue;
            };
            let (Some(expected_actor), Some(expected_owner), Some(doc_id), Some(content_sha256)) = (
                row.expected_actor_user_id.as_deref(),
                row.expected_owner_user_id.as_deref(),
                row.doc_id.as_deref(),
                row.content_sha256.as_deref(),
            ) else {
                continue;
            };
            let dispatch_id = existing_dispatch_id;
            let Ok((access_token, current_actor)) = authenticated_org_actor(state).await else {
                continue;
            };
            if current_actor != expected_actor {
                continue;
            }
            let Some(access) = crate::share::org_dto::OrgItemAccess::parse(&row.access) else {
                continue;
            };
            let Ok(base) = share_base_url(state) else {
                continue;
            };
            let Ok(client) = crate::share::client::ShareClient::new(&base) else {
                continue;
            };
            let resolution = reconcile_direct_org_update_attempt(
                state,
                &client,
                &access_token,
                &row,
                expected_actor,
            )
            .await;
            if !policy.is_current() {
                return Ok(republished);
            }
            match resolution {
                Ok(DirectOrgUpdateResolution::Exact(published)) => {
                    recovered_actor = Some(expected_actor.to_string());
                    recovered_dispatch_id = Some(dispatch_id.clone());
                    durable_republish_owner = Some(expected_owner.to_string());
                    if policy
                        .commit(|| {
                            confirm_org_mutation_for_projection(
                                state,
                                &row,
                                &published,
                                ORG_SHARE_REPUBLISH_PUT_PENDING,
                                &dispatch_id,
                                &now,
                            )
                        })?
                        .is_none()
                    {
                        return Ok(republished);
                    }
                    let Some(confirmed) = state.db.get_org_share(&row.id)? else {
                        continue;
                    };
                    recovered_put = Some(published);
                    row = confirmed;
                }
                Ok(DirectOrgUpdateResolution::Conflict) => {
                    let _ = policy.commit(|| {
                        conflict_republish_put_intent(
                            state,
                            &row.id,
                            doc_id,
                            access,
                            expected_actor,
                            expected_owner,
                            row.item_id.as_deref().ok_or_else(|| {
                                AppError::Storage(
                                    "org republish PUT lost its predecessor witness".into(),
                                )
                            })?,
                            row.rev,
                            row.generation,
                            content_sha256,
                            &dispatch_id,
                            row.last_error
                                .as_deref()
                                .unwrap_or(ORG_SHARE_REPUBLISH_PUT_PENDING),
                            &now,
                        )
                    })?;
                    continue;
                }
                Ok(DirectOrgUpdateResolution::Inconclusive) | Err(_) => continue,
            }
        }
        // Re-read the CURRENT plaintext THROUGH the read-gate. Automatic future edits are always
        // scrubbed: an initial explicit scrub=false publish/retry remains byte-stable in the queued
        // retry path, but silently carrying that opt-out onto later edits is unsafe without a durable
        // warning surface on every editor.
        let republish_scrub = true;
        let (observed_source_version, observed_dirty_counter) =
            (row.source_version, row.republish_dirty);
        let source = match build_org_share_snapshot(
            state,
            row.meeting_id.as_deref(),
            row.document_id.as_deref(),
            republish_scrub,
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
        let OrgShareBodySnapshot {
            title,
            markdown,
            created_at,
            counts: _,
            kind,
            attachment_owner,
            source_version,
        } = source;
        // `org_shares_for_source` rows always anchor exactly one of meeting_id/document_id (mirrors the
        // exclusivity `build_org_share_body` already enforced when this row was first published).
        let source_kind = match kind {
            crate::share::org_envelope::OrgItemKind::Task => {
                crate::share::org_envelope::OrgSourceKind::Task
            }
            _ if row.meeting_id.is_some() => crate::share::org_envelope::OrgSourceKind::Meeting,
            _ => crate::share::org_envelope::OrgSourceKind::Document,
        };

        if kind == crate::share::org_envelope::OrgItemKind::Task
            && crate::share::task_envelope::TaskEnvelope::from_json(&markdown, &row.org_id).is_err()
        {
            tracing::warn!(target: "org", "republish: task image manifest is incomplete; skipped");
            continue;
        }
        let bundle = if kind == crate::share::org_envelope::OrgItemKind::Task {
            task_attachment_bundle_for_markdown(state, &attachment_owner, &row.org_id, &markdown)
        } else {
            attachment_bundle_for_markdown(state, &attachment_owner, &markdown)
        };
        let (markdown, attachments) = match bundle {
            Ok(bundle) => bundle,
            Err(e) => {
                tracing::warn!(target: "org", error = %e, "republish: could not bundle source images; skipped");
                continue;
            }
        };
        if state.db.org_share_source_counters(&row.id)?
            != (observed_source_version, observed_dirty_counter)
        {
            continue;
        }

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

        let pending_post = row.last_error.as_deref() == Some(ORG_SHARE_REPUBLISH_POST_PENDING);
        let initial_too_large =
            row.item_id.is_none() && row.last_error.as_deref() == Some(ORG_SHARE_ERR_TOO_LARGE);
        if initial_too_large && row.republish_dirty == 0 {
            continue;
        }
        let pending_replay = pending_post || initial_too_large;

        // Short-circuit: unchanged content ⇒ NO new item, NO egress. `content_sha256` folds
        // `source_rev` into the canonical bytes, so the stored hash (computed at `row.rev`) must be
        // compared against a hash at the SAME rev — else every republish would look "changed" purely
        // because the rev bumped. Build the comparison envelope at `row.rev`; only the PUBLISH envelope
        // uses `new_rev`.
        // The row's OWN placement, not the caller's: a republish must rebuild the envelope the
        // last publish produced, or the comparison hash would differ for a reason that is not a
        // content change and every save would mint a needless revision.
        let row_placement =
            row.parent_container_id
                .as_ref()
                .map(|parent| crate::share::org_envelope::OrgPlacement {
                    parent_container_id: parent.clone(),
                    position: row.position,
                });
        let cmp_env = crate::share::org_envelope::OrgEnvelope::new(
            kind,
            title.clone(),
            markdown.clone(),
            author_hint.clone(),
            created_at.clone(),
            row.rev,
            source_kind,
        )
        .with_attachments(attachments.clone())
        .with_placement(row_placement.clone());
        if recovered_put.is_some()
            && row.content_sha256.as_deref() != Some(cmp_env.content_sha256().as_slice())
        {
            // The exact remote result is known, but the original plaintext payload is no longer
            // reconstructable from this changed source. Keep the durable pending witness for feed
            // repair; never complete it and dispatch a newer mutation across that projection gap.
            continue;
        }
        if !pending_replay
            && row.content_sha256.as_deref() == Some(cmp_env.content_sha256().as_slice())
        {
            let recovering_projection = recovered_put.is_some();
            let mut projection_safe = recovered_put.is_none();
            if let Some(published) = recovered_put {
                let (local_markdown, local_attachments) = match prepare_incoming_attachment_bundle(
                    &cmp_env.markdown,
                    &cmp_env.attachments,
                ) {
                    Ok(bundle) => bundle,
                    Err(error) => {
                        tracing::warn!(target: "org", error = %error, "republish recovery: local attachment projection failed");
                        continue;
                    }
                };
                let prepared = match crate::storage::Db::prepare_org_item_index_for_kind(
                    cmp_env.kind,
                    &cmp_env.title,
                    &cmp_env.created_at,
                    &local_markdown,
                    None,
                ) {
                    Ok(prepared) => prepared,
                    Err(error) => {
                        tracing::warn!(target: "org", error = %error, "republish recovery: local index projection failed");
                        continue;
                    }
                };
                if !org_share_snapshot_is_current(
                    state,
                    row.meeting_id.as_deref(),
                    row.document_id.as_deref(),
                    &source_version,
                )? {
                    continue;
                }
                let projected = policy.commit(|| {
                    commit_org_metadata_mutation(
                        state,
                        app.map(|app| app as &dyn AskHistoryInvalidationNotifier),
                        || {
                            state.db.commit_org_republish_projection_if_current(
                                &row.id,
                                recovered_dispatch_id.as_deref().ok_or_else(|| {
                                    AppError::Storage(
                                        "recovered org republish lost its dispatch id".into(),
                                    )
                                })?,
                                &row.org_id,
                                published.doc_id.as_deref().ok_or_else(|| {
                                    AppError::Storage(
                                        "recovered org republish lost its document id".into(),
                                    )
                                })?,
                                published.access.as_str(),
                                row.rev,
                                row.generation,
                                row.content_sha256.as_deref().unwrap_or_default(),
                                recovered_actor.as_deref().ok_or_else(|| {
                                    AppError::Storage(
                                        "recovered org republish lost its actor".into(),
                                    )
                                })?,
                                published.document_owner_user_id.as_deref().ok_or_else(|| {
                                    AppError::Storage(
                                        "recovered org republish lost its owner".into(),
                                    )
                                })?,
                                row.item_id.as_deref(),
                                Some(ORG_SHARE_PROJECTION_PENDING),
                                false,
                                &crate::storage::org_store::OrgRepublishProjection {
                                    item_id: &published.item_id,
                                    seq: published.seq,
                                    author_hint: &cmp_env.author_hint,
                                    title: &cmp_env.title,
                                    markdown: &local_markdown,
                                    created_at: &cmp_env.created_at,
                                    source_kind: cmp_env
                                        .source_kind
                                        .map(crate::share::org_envelope::OrgSourceKind::as_str),
                                    author_user_id: recovered_actor.as_deref(),
                                    prepared: &prepared,
                                    attachments: &local_attachments,
                                },
                            )
                        },
                    )
                })?;
                if projected.is_none() {
                    return Ok(republished);
                }
                projection_safe = projected.is_some_and(|outcome| outcome.changed);
            }
            if projection_safe
                && org_share_snapshot_is_current(
                    state,
                    row.meeting_id.as_deref(),
                    row.document_id.as_deref(),
                    &source_version,
                )?
            {
                let _ = policy.commit(|| {
                    state
                        .db
                        .clear_org_share_dirty_if_epoch(
                            &row.id,
                            observed_source_version,
                            observed_dirty_counter,
                            row.item_id.as_deref().ok_or_else(|| {
                                AppError::Storage("org republish lost its item id".into())
                            })?,
                            row.rev,
                            row.content_sha256.as_deref().unwrap_or_default(),
                        )
                        .map(|_| ())
                })?;
            }
            if recovering_projection && projection_safe {
                republished = republished.saturating_add(1);
            }
            continue;
        }

        let new_rev = if pending_replay {
            row.rev
        } else {
            row.rev.saturating_add(1)
        };
        let env = crate::share::org_envelope::OrgEnvelope::new(
            kind,
            title,
            markdown,
            author_hint,
            created_at,
            new_rev,
            source_kind,
        )
        .with_attachments(attachments)
        .with_placement(row_placement);
        let content_sha = env.content_sha256();
        if pending_post && row.content_sha256.as_deref() != Some(content_sha.as_slice()) {
            // Preserve the already-dispatched immutable witness. A newer local source is
            // represented by the row's dirty counter and must not rewrite the pending attempt.
            continue;
        }

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
        let (access_token, publisher_user_id) = match authenticated_org_actor(state).await {
            Ok(snapshot) => snapshot,
            Err(_) => {
                // No live session → leave the row uploaded (stale); a later save with a session
                // republishes. NOT a failure (never blocks the save).
                continue;
            }
        };
        if !policy.is_current() {
            return Ok(republished);
        }
        let client = match crate::share::client::ShareClient::new(&base) {
            Ok(c) => c,
            Err(_) => continue,
        };

        // (5) Seal under the OCK + LOCAL OPEN-VERIFY (egress verify-before-destroy). AAD nonce =
        // hex(content_sha256) — deterministic + rides the feed so every member reconstructs it.
        let ock = match acquire_org_ock_with_policy(state, &org.org_id, generation, policy).await {
            Ok(k) => k,
            Err(_) => continue,
        };
        if !policy.is_current() {
            return Ok(republished);
        }
        let item_nonce = org_item_nonce(&content_sha);
        let ciphertext = match crate::share::org_envelope::seal_org_envelope(
            &ock,
            &env,
            &org.org_id,
            &item_nonce,
        ) {
            Ok((ct, _)) => ct,
            Err(_) => continue,
        };

        // SIZE PRE-CHECK (Brain v3): oversized ciphertext would 413 forever — mark the row with
        // the TERMINAL `too_large` reason (excluded from the launch sweep) instead of egressing.
        // The OLD item stays live on the server (never tombstone-into-nothing); a later edit-save
        // that shrinks the source re-enters via `org_shares_for_source` and heals the row.
        if ciphertext.len() > murmur_protocol::caps::MAX_ORG_ITEM_BLOB_BYTES {
            let _ = policy.commit(|| {
                fail_republish_pre_dispatch_if_current(
                    state,
                    &row,
                    ORG_SHARE_ERR_TOO_LARGE,
                    observed_source_version,
                    observed_dirty_counter,
                    &now,
                )
            })?;
            tracing::warn!(
                target: "org",
                sealed_bytes = ciphertext.len(),
                cap_bytes = murmur_protocol::caps::MAX_ORG_ITEM_BLOB_BYTES,
                "republish skipped: sealed item exceeds the org blob cap (terminal too_large)"
            );
            continue;
        }

        // (6) publish the NEW rev inline and atomically. On failure, do NOT tombstone: the old copy
        // stays live, and no anonymous staged blob is orphaned.
        if !org_share_snapshot_is_current(
            state,
            row.meeting_id.as_deref(),
            row.document_id.as_deref(),
            &source_version,
        )? {
            tracing::info!(
                target: "org",
                org_id = %row.org_id,
                "republish deferred: source moved or lock lifecycle changed before upload"
            );
            continue;
        }
        let stable_doc_id = row.doc_id.clone();
        let use_legacy_post =
            pending_post || initial_too_large || (!pending_replay && stable_doc_id.is_none());
        let post_doc_id = if use_legacy_post {
            Some(
                stable_doc_id
                    .clone()
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
            )
        } else {
            None
        };
        let pending_reason = if use_legacy_post {
            ORG_SHARE_REPUBLISH_POST_PENDING
        } else {
            ORG_SHARE_REPUBLISH_PUT_PENDING
        };
        let expected_rev = if pending_replay {
            new_rev.saturating_sub(1)
        } else {
            row.rev
        };
        let requested_access = crate::share::org_dto::OrgItemAccess::parse(&row.access)
            .unwrap_or(crate::share::org_dto::OrgItemAccess::View);
        let expected_owner = if use_legacy_post {
            Some(publisher_user_id.clone())
        } else {
            row.item_id
                .as_deref()
                .and_then(|item_id| state.db.org_item_edit_ctx(item_id).ok().flatten())
                .and_then(|ctx| ctx.document_owner_user_id)
                .or_else(|| {
                    recovered_put
                        .as_ref()
                        .and_then(|published| published.document_owner_user_id.clone())
                })
                .or_else(|| durable_republish_owner.clone())
        };
        let put_dispatch_id: Option<String>;
        let publish_result = if !use_legacy_post {
            let Some(doc_id) = stable_doc_id.as_deref() else {
                return Err(AppError::Storage(
                    "stable org republish lost its document id".into(),
                ));
            };
            let request = crate::share::org_dto::UpdateOrgItemRequest {
                mutation_id: None,
                expected_rev,
                content_cell: ciphertext,
                content_sha256: content_sha.clone(),
                generation,
            };
            let owner_user_id = expected_owner.as_deref().ok_or_else(|| {
                AppError::Storage("stable org republish omitted its owner".into())
            })?;
            let operation = OrgDispatchOperation::Update {
                org_id: org.org_id.clone(),
                doc_id: doc_id.to_string(),
                expected_rev: request.expected_rev,
                generation: request.generation,
                content_sha256: request.content_sha256.clone(),
                cell_len: request.content_cell.len(),
                cell_sha256: org_dispatch_cell_sha256(&request.content_cell),
                access: requested_access,
                owner_user_id: owner_user_id.to_string(),
            };
            let Some(attempt) = policy.commit(|| {
                persist_org_republish_intent(
                    state,
                    &row,
                    new_rev,
                    generation,
                    &content_sha,
                    None,
                    pending_reason,
                    &now,
                    &client.host(),
                    request.content_cell.len(),
                    operation,
                    &publisher_user_id,
                    owner_user_id,
                    observed_source_version,
                    observed_dirty_counter,
                )
            })?
            else {
                return Ok(republished);
            };
            put_dispatch_id = Some(attempt.dispatch_id);
            client
                .org_update_item(&access_token, &org.org_id, doc_id, request, attempt.permit)
                .await
        } else {
            let request = crate::share::org_dto::PublishItemRequest {
                mutation_id: None,
                doc_id: post_doc_id.clone(),
                access: Some(requested_access),
                blob_id: None,
                content_cell: Some(ciphertext),
                content_sha256: content_sha.clone(),
                rev: new_rev,
                generation,
            };
            let operation = OrgDispatchOperation::Publish {
                org_id: org.org_id.clone(),
                doc_id: request.doc_id.clone(),
                access: request.access,
                rev: request.rev,
                generation: request.generation,
                content_sha256: request.content_sha256.clone(),
                cell_len: request.content_cell.as_ref().map(Vec::len).unwrap_or(0),
                cell_sha256: org_dispatch_cell_sha256(
                    request.content_cell.as_deref().unwrap_or_default(),
                ),
                owner_user_id: Some(publisher_user_id.clone()),
            };
            let sealed_bytes = request.content_cell.as_ref().map(Vec::len).unwrap_or(0);
            let Some(attempt) = policy.commit(|| {
                persist_org_republish_intent(
                    state,
                    &row,
                    new_rev,
                    generation,
                    &content_sha,
                    post_doc_id.as_deref(),
                    pending_reason,
                    &now,
                    &client.host(),
                    sealed_bytes,
                    operation,
                    &publisher_user_id,
                    expected_owner.as_deref().ok_or_else(|| {
                        AppError::Storage("org republish POST omitted its owner".into())
                    })?,
                    observed_source_version,
                    observed_dirty_counter,
                )
            })?
            else {
                return Ok(republished);
            };
            put_dispatch_id = Some(attempt.dispatch_id.clone());
            client
                .org_publish_item(&access_token, &org.org_id, request, attempt.permit)
                .await
        };
        let expected_doc_id = if use_legacy_post {
            post_doc_id.as_deref()
        } else {
            stable_doc_id.as_deref()
        };
        let expected_owner_user_id = expected_owner.as_deref();
        let published = match publish_result {
            Ok(p)
                if p.doc_id.as_deref() == expected_doc_id
                    && p.access == requested_access
                    && p.document_owner_user_id.as_deref() == expected_owner_user_id =>
            {
                p
            }
            Ok(_) => {
                let Some(doc_id) = expected_doc_id else {
                    continue;
                };
                match authoritative_org_document_head(
                    state,
                    &client,
                    &access_token,
                    &org.org_id,
                    doc_id,
                )
                .await
                {
                    Ok(Some(head))
                        if head.rev == new_rev
                            && head.generation == generation
                            && head.content_sha256.as_deref() == Some(content_sha.as_slice())
                            && head.access == requested_access
                            && head.author_user_id == publisher_user_id
                            && head.document_owner_user_id.as_deref() == expected_owner_user_id =>
                    {
                        publish_response_from_authoritative_head(head)
                    }
                    Ok(Some(_)) => {
                        let _ = policy.commit(|| {
                            conflict_republish_put_intent(
                                state,
                                &row.id,
                                doc_id,
                                requested_access,
                                &publisher_user_id,
                                expected_owner_user_id.ok_or_else(|| {
                                    AppError::Storage("stable org republish lost its owner".into())
                                })?,
                                row.item_id.as_deref().ok_or_else(|| {
                                    AppError::Storage(
                                        "stable org republish lost its predecessor".into(),
                                    )
                                })?,
                                new_rev,
                                generation,
                                &content_sha,
                                put_dispatch_id.as_deref().ok_or_else(|| {
                                    AppError::Storage(
                                        "stable org republish lost its dispatch id".into(),
                                    )
                                })?,
                                pending_reason,
                                &now,
                            )
                        })?;
                        continue;
                    }
                    Ok(None) | Err(_) => continue,
                }
            }
            Err(error) if is_org_edit_conflict(&error) && use_legacy_post => {
                let Some(doc_id) = post_doc_id.as_deref() else {
                    return Err(AppError::Storage(
                        "legacy org republish lost its pending document id".into(),
                    ));
                };
                match authoritative_org_document_head(
                    state,
                    &client,
                    &access_token,
                    &org.org_id,
                    doc_id,
                )
                .await
                {
                    Ok(Some(head))
                        if head.rev == new_rev
                            && head.generation == generation
                            && head.content_sha256.as_deref() == Some(content_sha.as_slice())
                            && head.access == requested_access
                            && head.author_user_id == publisher_user_id
                            && head.document_owner_user_id.as_deref() == expected_owner_user_id =>
                    {
                        publish_response_from_authoritative_head(head)
                    }
                    Ok(Some(_)) => {
                        // Best-effort current-head resolution gives the next UI refresh authoritative
                        // context. The 409 itself is already terminal for this exact expectedRev and
                        // payload, even if the corroborating scan is temporarily unavailable.
                        let _ = policy.commit(|| {
                            conflict_republish_put_intent(
                                state,
                                &row.id,
                                doc_id,
                                requested_access,
                                &publisher_user_id,
                                expected_owner_user_id.ok_or_else(|| {
                                    AppError::Storage("stable org republish lost its owner".into())
                                })?,
                                row.item_id.as_deref().ok_or_else(|| {
                                    AppError::Storage(
                                        "stable org republish lost its predecessor".into(),
                                    )
                                })?,
                                new_rev,
                                generation,
                                &content_sha,
                                put_dispatch_id.as_deref().ok_or_else(|| {
                                    AppError::Storage(
                                        "stable org republish lost its dispatch id".into(),
                                    )
                                })?,
                                pending_reason,
                                &now,
                            )
                        })?;
                        continue;
                    }
                    Ok(None) | Err(_) => continue,
                }
            }
            Err(error) => {
                if is_org_edit_conflict(&error) {
                    let _ = policy.commit(|| {
                        conflict_republish_put_intent(
                            state,
                            &row.id,
                            expected_doc_id.ok_or_else(|| {
                                AppError::Storage(
                                    "stable org republish lost its document id".into(),
                                )
                            })?,
                            requested_access,
                            &publisher_user_id,
                            expected_owner_user_id.ok_or_else(|| {
                                AppError::Storage("stable org republish lost its owner".into())
                            })?,
                            row.item_id.as_deref().ok_or_else(|| {
                                AppError::Storage(
                                    "stable org republish lost its predecessor".into(),
                                )
                            })?,
                            new_rev,
                            generation,
                            &content_sha,
                            put_dispatch_id.as_deref().ok_or_else(|| {
                                AppError::Storage(
                                    "stable org republish lost its dispatch id".into(),
                                )
                            })?,
                            pending_reason,
                            &now,
                        )
                    })?;
                }
                // Transient/lost responses deliberately retain the durable pending marker, revision,
                // hash and scrub intent written before dispatch. The sweep replays that exact attempt.
                continue;
            }
        };
        if !policy.is_current() {
            return Ok(republished);
        }
        let projection_row = state.db.get_org_share(&row.id)?.ok_or_else(|| {
            AppError::Storage("org republish attempt disappeared before projection".into())
        })?;
        let projection_dispatch_id = put_dispatch_id
            .as_deref()
            .ok_or_else(|| AppError::Storage("stable org republish lost its dispatch id".into()))?;
        if projection_row.last_error.as_deref() != Some(ORG_SHARE_PROJECTION_PENDING) {
            let Some(_) = policy.commit(|| {
                confirm_org_mutation_for_projection(
                    state,
                    &projection_row,
                    &published,
                    pending_reason,
                    projection_dispatch_id,
                    &now,
                )
            })?
            else {
                return Ok(republished);
            };
        }

        // LOCAL REPLICA CONSISTENCY (F-org-editable): the author's OWN `org_items` replica would
        // otherwise stay frozen on the OLD item_id until the next feed pull — so the just-repointed
        // `org_shares` row no longer matches the replica the Notes list renders (`item_id` drift →
        // the card falls back to a stale, read-only viewer). Upsert the NEW item + tombstone the OLD
        // one LOCALLY now, so `list_org_items` immediately resolves this as an owned/editable card with
        // the fresh title. The FTS-only material is prepared outside the epoch lease, then one storage
        // transaction preserves an identical/newer feed-ingested vector index while atomically evicting
        // the old local item. Best-effort: a local-replica error must never fail the save (the server
        // copy is already live + correct).
        //
        // AUTHOR (root-cause fix, 2026-07-15): this row's caller is the SAME session that just
        // successfully republished it (`require_session_mk` above already proved a live session),
        // so stamp its own server user id directly — the repointed row is correct from the moment
        // it's written, never dependent on a later backfill.
        let my_author_id = session_server_user_id(state).ok();
        let replica = match prepare_incoming_attachment_bundle(&env.markdown, &env.attachments) {
            Ok((local_markdown, local_attachments)) => {
                match crate::storage::Db::prepare_org_item_index_for_kind(
                    env.kind,
                    &env.title,
                    &env.created_at,
                    &local_markdown,
                    None,
                ) {
                    Ok(prepared) => {
                        if !org_share_snapshot_is_current(
                            state,
                            row.meeting_id.as_deref(),
                            row.document_id.as_deref(),
                            &source_version,
                        )? {
                            continue;
                        }
                        policy.commit(|| {
                            let outcome = commit_org_metadata_mutation(
                                state,
                                app.map(|app| app as &dyn AskHistoryInvalidationNotifier),
                                || {
                                    state.db.commit_org_republish_projection_if_current(
                                        &row.id,
                                        put_dispatch_id.as_deref().ok_or_else(|| {
                                            AppError::Storage(
                                                "stable org republish lost its dispatch id".into(),
                                            )
                                        })?,
                                        &org.org_id,
                                        published.doc_id.as_deref().ok_or_else(|| {
                                            AppError::Storage(
                                                "stable org republish lost its document id".into(),
                                            )
                                        })?,
                                        published.access.as_str(),
                                        new_rev,
                                        generation,
                                        &content_sha,
                                        &publisher_user_id,
                                        published.document_owner_user_id.as_deref().ok_or_else(
                                            || {
                                                AppError::Storage(
                                                    "stable org republish lost its owner".into(),
                                                )
                                            },
                                        )?,
                                        Some(published.item_id.as_str()),
                                        Some(ORG_SHARE_PROJECTION_PENDING),
                                        false,
                                        &crate::storage::org_store::OrgRepublishProjection {
                                            item_id: &published.item_id,
                                            seq: published.seq,
                                            author_hint: &env.author_hint,
                                            title: &env.title,
                                            markdown: &local_markdown,
                                            created_at: &env.created_at,
                                            source_kind: env.source_kind.map(
                                                crate::share::org_envelope::OrgSourceKind::as_str,
                                            ),
                                            author_user_id: my_author_id.as_deref(),
                                            prepared: &prepared,
                                            attachments: &local_attachments,
                                        },
                                    )
                                },
                            )?;
                            Ok(outcome)
                        })
                    }
                    Err(e) => Err(e),
                }
            }
            Err(e) => Err(e),
        };
        match replica {
            Ok(Some(outcome)) if outcome.changed => republished += 1,
            Ok(Some(_)) => continue,
            Ok(None) => return Ok(republished),
            Err(e) => {
                tracing::warn!(target: "org", error = %e, "republish: local replica upsert failed (server copy live)");
            }
        }

        // THEN tombstone the OLD item so members evict the stale copy. Publish-BEFORE-tombstone: a
        // crash here leaves a transient dup (recoverable), never a window with no org copy. A tombstone
        // failure is non-fatal — the new copy is already live; the stale one lingers until a revoke.
        if use_legacy_post {
            if let Some(old_item) = row.item_id.as_deref() {
                if !policy.is_current() {
                    return Ok(republished);
                }
                let permit = permit_simple_org_dispatch(
                    state,
                    &client.host(),
                    "org_share_revoke",
                    OrgDispatchOperation::Tombstone {
                        org_id: org.org_id.clone(),
                        item_id: old_item.to_string(),
                    },
                )?;
                match delete_legacy_org_item(
                    state,
                    &client,
                    &access_token,
                    &org.org_id,
                    old_item,
                    permit,
                )
                .await
                {
                    Ok(()) => {
                        if !policy.is_current() {
                            return Ok(republished);
                        }
                    }
                    Err(e) => {
                        if !policy.is_current() {
                            return Ok(republished);
                        }
                        tracing::warn!(
                            target: "org",
                            error = %e,
                            org_id = %org.org_id,
                            "republish: superseded item published but old-item tombstone failed (transient dup)"
                        );
                    }
                }
            }
        }
    }
    Ok(republished)
}

/// `list_org_shares(org_id)` — the caller's outbound shares INTO ONE org (local rows; titles render
/// only to the local owner). Content-free enough for the FE list.
#[tauri::command]
pub fn list_org_shares(
    state: State<'_, AppState>,
    org_id: String,
) -> Result<Vec<OrgShareEntry>, AppError> {
    list_org_shares_inner(state.inner(), &org_id)
}

/// The multi-org fix (2026-07-26): this read used to ignore its caller entirely and return the FIRST
/// locally-joined org's shares (`list_org_states().next()`) in a shipped MULTI-org app — so a member
/// of two orgs saw the wrong org's share list. The org id is now the caller's explicit choice.
/// An unknown/unjoined id returns an EMPTY list rather than silently falling back to another org.
pub(crate) fn list_org_shares_inner(
    state: &AppState,
    org_id: &str,
) -> Result<Vec<OrgShareEntry>, AppError> {
    // Keep the source gate and the identifying title/status read in one seal-lifecycle interval.
    // Otherwise a concurrent lock could land after the visibility decision but before return.
    let _lifecycle = lifecycle_guard(state);
    let org_id = org_id.trim();
    if org_id.is_empty() || state.db.get_org_state(org_id)?.is_none() {
        return Ok(Vec::new());
    }
    let rows = state.db.list_org_shares_for_org(org_id)?;
    let mut out = Vec::new();
    for r in rows {
        if !org_share_source_is_visible(state, &r)? {
            continue;
        }
        out.push(OrgShareEntry {
            item_id: r.item_id,
            kind: r.kind,
            title: r.title,
            shared_at: r.created_at,
            rev: r.rev,
            state: r.state,
        });
    }
    Ok(out)
}

/// Gate an outbound share's identifying metadata through its canonical local source read gate.
/// Missing/corrupt anchors fail closed. This helper never reads content, but uses the same visibility
/// decision as content readers so a sealed source cannot leak its title/share relationship via IPC.
fn org_share_source_is_visible(
    state: &AppState,
    row: &crate::storage::OrgShareRow,
) -> Result<bool, AppError> {
    if let Some(meeting_id) = row.meeting_id.as_deref() {
        return meeting_is_unlocked(state, meeting_id);
    }
    let Some(document_id) = row.document_id.as_deref() else {
        return Ok(false);
    };
    let Some((folder_id, _created_at, _updated_at)) = state.db.note_gate_anchor(document_id)?
    else {
        return Ok(false);
    };
    folder_is_unlocked(state, &folder_id)
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
    let _lifecycle = lifecycle_guard(st);
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
    let _lifecycle = lifecycle_guard(st);
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
    org_live_shares_for_source_inner(state.inner(), meeting_id.as_deref(), document_id.as_deref())
}

pub(crate) fn org_live_shares_for_source_inner(
    st: &AppState,
    meeting_id: Option<&str>,
    document_id: Option<&str>,
) -> Result<Vec<OrgSourceShareStatus>, AppError> {
    // The source visibility decision and returned local identifiers share one seal lifecycle.
    let _lifecycle = lifecycle_guard(st);
    let rows = st.db.org_shares_for_source(meeting_id, document_id)?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        if !org_share_source_is_visible(st, &r)? {
            continue;
        }
        // A collaborator may have advanced the stable document while this origin row deliberately
        // retains its last-published CAS baseline. Surface the current live head/access for
        // management only; never mutate `r.rev`/hash/item_id here.
        let current = match r.doc_id.as_deref() {
            Some(doc_id) => st.db.current_org_document_status(&r.org_id, doc_id)?,
            None => None,
        };
        let (item_id, rev, access) = current
            .map(|(item_id, rev, access)| (Some(item_id), rev, access))
            .unwrap_or((r.item_id, r.rev, r.access));
        out.push(OrgSourceShareStatus {
            org_id: r.org_id,
            item_id,
            rev,
            access,
            conflicted: r.last_error.as_deref() == Some(ORG_SHARE_ERR_EDIT_CONFLICT),
        });
    }
    Ok(out)
}

/// `revoke_org_share(item_id)` — tombstone a published org item (destroys its server ciphertext), evict
/// this device's own decrypted replica of it, and mark the local row revoked.
///
/// CRASH-SAFE ORDER, each step idempotent so an interruption is always re-drivable:
/// `revoke_pending` → server DELETE → local eviction → `revoked`. Marking `revoke_pending` FIRST means
/// the launch sweep completes a tombstone that didn't land; evicting BEFORE the `revoked` flip means an
/// interruption can never leave withdrawn content live locally under a row no queue re-drives (see the
/// ordering note inside `revoke_org_share_inner_with_policy`).
///
/// Emits [`crate::events::EVENT_ORG_FEED_UPDATED`] on success — like every other org-item-mutating
/// command here — so the org roster, the Settings organization section and an open org-item viewer
/// re-fetch immediately instead of showing the withdrawn row until some later productive sync tick.
#[tauri::command]
pub async fn revoke_org_share(
    app: AppHandle,
    state: State<'_, AppState>,
    item_id: String,
) -> Result<(), AppError> {
    let _mutation = state.lock_org_mutation().await;
    revoke_org_share_inner_with_policy(state.inner(), item_id, OrgWorkPolicy::manual(), Some(&app))
        .await?;
    crate::events::emit_org_feed_updated(&app, 1);
    Ok(())
}

/// Inner of [`revoke_org_share`] with the FE notice expressed as the testable
/// [`crate::events::OrgFeedNotifier`] seam instead of a concrete `AppHandle`. The notice fires ONLY
/// after the revoke fully succeeded — a failed revoke changed nothing worth re-fetching, and the row
/// the FE already shows is still the truth.
#[cfg(test)]
pub(crate) async fn revoke_org_share_notifying(
    state: &AppState,
    item_id: String,
    notifier: &dyn OrgFeedNotifier,
) -> Result<(), AppError> {
    let _mutation = state.lock_org_mutation().await;
    revoke_org_share_inner_with_policy(state, item_id, OrgWorkPolicy::manual(), None).await?;
    notifier.org_feed_updated(1);
    Ok(())
}

#[cfg(test)]
pub(crate) async fn revoke_org_share_inner(
    state: &AppState,
    item_id: String,
) -> Result<(), AppError> {
    let _mutation = state.lock_org_mutation().await;
    revoke_org_share_inner_with_policy(state, item_id, OrgWorkPolicy::manual(), None).await
}

pub(crate) async fn revoke_org_share_inner_with_policy(
    state: &AppState,
    item_id: String,
    policy: OrgWorkPolicy,
    app: Option<&AppHandle>,
) -> Result<(), AppError> {
    if !policy.is_current() {
        return Err(AppError::Unavailable(
            "background org revoke deferred for recording".into(),
        ));
    }
    let item_id = item_id.trim().to_string();
    let Some(row) = state.db.org_share_for_revoke_target(&item_id)? else {
        return Err(AppError::InvalidArg(
            "no local org share for that item".into(),
        ));
    };
    revoke_org_share_row_with_policy(state, row, policy, app).await
}

/// Revoke one already-selected durable journal row. Internal source/folder/sweep callers must use
/// this seam instead of feeding a local `org_shares.id` through the public server-item resolver:
/// those identifiers are separate namespaces and may collide adversarially.
async fn revoke_org_share_row_with_policy(
    state: &AppState,
    row: crate::storage::OrgShareRow,
    policy: OrgWorkPolicy,
    app: Option<&AppHandle>,
) -> Result<(), AppError> {
    if !policy.is_current() {
        return Err(AppError::Unavailable(
            "background org revoke deferred for recording".into(),
        ));
    }
    let item_id = row.item_id.clone().unwrap_or_else(|| row.id.clone());
    if let Some(proof) = proven_never_landed_org_share(state, &row)? {
        let now = chrono::Utc::now().to_rfc3339();
        let conn = state.db.lock();
        let (state_name, last_error) = match proof {
            NeverLandedOrgShare::InitialReplayable => {
                ("failed", Some(ORG_SHARE_INITIAL_POST_REPLAYABLE))
            }
            NeverLandedOrgShare::PreDispatchFailure(error) => ("failed", Some(error)),
        };
        let changed = conn.execute(
            "UPDATE org_shares SET state='revoked',last_error=NULL,dispatch_id=NULL,updated_at=?2
              WHERE id=?1 AND state=?3 AND last_error IS ?4 AND item_id IS NULL
                AND (?5=1 OR dispatch_id IS NULL)",
            rusqlite::params![row.id,now,state_name,last_error,
                matches!(proof,NeverLandedOrgShare::InitialReplayable) as i64],
        ).map_err(|_| AppError::Storage("cancel proven-absent org publish".into()))?;
        return if changed == 1 {
            Ok(())
        } else {
            Err(AppError::Unavailable(
                "org share changed before cancellation".into(),
            ))
        };
    }
    if row.item_id.is_none() && row.doc_id.is_none() {
        return Err(AppError::Unavailable(
            "org share lacks proof that its remote publish never landed".into(),
        ));
    }
    // DELETE and its possible authenticated feed corroboration are org egress too. Refuse before
    // `revoke_pending`, network dispatch, local eviction, or a dispatch ledger row.
    require_org_egress_consent(state)?;
    let now = chrono::Utc::now().to_rfc3339();
    // Make the withdrawal durable before resolving network prerequisites. An offline delete must
    // never leave the share `uploaded`, because the launch sweep would then have no revoke intent to
    // re-drive and could resurrect the locally deleted source. This transition has no dispatch id or
    // ledger row: those are committed together only after a real host/session-backed request is ready.
    if policy
        .commit(|| persist_org_revoke_intent(state, &row.id, &now))?
        .is_none()
    {
        return Err(AppError::Unavailable(
            "background org revoke deferred for recording".into(),
        ));
    }
    let base = share_base_url(state)?;
    let access = valid_access_token(state).await?;
    if !policy.is_current() {
        return Err(AppError::Unavailable(
            "background org revoke deferred for recording".into(),
        ));
    }
    let client = crate::share::client::ShareClient::new(&base)?;
    let operation = match row.doc_id.as_deref() {
        Some(doc_id) => OrgDispatchOperation::DeleteDocument {
            org_id: row.org_id.clone(),
            doc_id: doc_id.to_string(),
        },
        None => OrgDispatchOperation::Tombstone {
            org_id: row.org_id.clone(),
            item_id: item_id.clone(),
        },
    };
    let Some(dispatch_permit) = policy
        .commit(|| persist_org_revoke_dispatch(state, &row.id, &now, &client.host(), operation))?
    else {
        return Err(AppError::Unavailable(
            "background org revoke deferred for recording".into(),
        ));
    };
    if let Some(doc_id) = row.doc_id.as_deref() {
        delete_stable_org_document(
            state,
            &client,
            &access,
            &row.org_id,
            doc_id,
            dispatch_permit,
        )
        .await?;
    } else {
        delete_legacy_org_item(
            state,
            &client,
            &access,
            &row.org_id,
            &item_id,
            dispatch_permit,
        )
        .await?;
    }
    if !policy.is_current() {
        return Err(AppError::Unavailable(
            "background org revoke deferred for recording".into(),
        ));
    }
    // EVICT THE LOCAL REPLICA ON THE PUBLISHING DEVICE — *BEFORE* the `revoked` state flip. Withdrawing
    // a share used to mutate ONLY the `org_shares` state machine, so the device that revoked kept its
    // own `org_items` row — markdown, chunks, FTS tokens, int8 vectors and the image BLOBs — live and
    // searchable through org search / Ask / MCP `org_search` forever. Routed through the ONE eviction
    // primitive so this path gets exactly the same cleanup as a feed tombstone or the reconcile sweep.
    // AFTER the server DELETE succeeded (an eviction before a failed tombstone would blind this device
    // to still-shared content), and idempotent — a device with no local replica row is simply a no-op.
    //
    // ORDER IS LOAD-BEARING (eviction THEN state, never state THEN eviction). These are two separate
    // commits, so a crash / quit / background-epoch flip lands between them:
    //   - state-then-eviction (the pre-2026-07-26 order) leaves a row marked `revoked` whose decrypted
    //     replica is still live and searchable. `org_sweep_pending` only re-drives `revoke_pending`, so
    //     that row NEVER re-enters the crash-safe recovery path and the leak persists until the slow
    //     anti-entropy sweep happens to walk that seq.
    //   - eviction-then-state (this order) leaves a `revoke_pending` row whose replica is ALREADY gone.
    //     The server tombstone and the eviction are both idempotent, so the launch sweep re-drives the
    //     row harmlessly and completes the flip. Strictly safer: the interrupted state is a stale
    //     bookkeeping row, never live withdrawn content.
    let Some(_evicted) = policy.commit(|| {
        commit_org_visibility_reduction(
            state,
            app.map(|app| app as &dyn AskHistoryInvalidationNotifier),
            || match row.doc_id.as_deref() {
                Some(doc_id) => {
                    state
                        .db
                        .terminalize_and_evict_org_document(&row.org_id, doc_id, &now)
                }
                None => state.db.evict_org_item(&item_id),
            },
        )
    })?
    else {
        return Err(AppError::Unavailable(
            "background org revoke deferred for recording".into(),
        ));
    };
    if row.doc_id.is_none()
        && policy
            .commit(|| state.db.set_org_share_state(&row.id, "revoked", &now))?
            .is_none()
    {
        return Err(AppError::Unavailable(
            "background org revoke deferred for recording".into(),
        ));
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum NeverLandedOrgShare<'a> {
    InitialReplayable,
    PreDispatchFailure(&'a str),
}

/// `item_id = NULL` is not proof of absence: a stable initial POST can commit while its response is
/// lost. Only states with a durable no-dispatch witness, or a completed authenticated absence scan,
/// may be cancelled locally. Every other stable row must use document DELETE + 404 corroboration.
fn proven_never_landed_org_share<'a>(
    state: &AppState,
    row: &'a crate::storage::OrgShareRow,
) -> Result<Option<NeverLandedOrgShare<'a>>, AppError> {
    if row.item_id.is_some() {
        return Ok(None);
    }
    if row.last_error.as_deref() == Some(ORG_SHARE_INITIAL_POST_REPLAYABLE) {
        return Ok(Some(NeverLandedOrgShare::InitialReplayable));
    }
    if state.db.org_share_dispatch_id(&row.id)?.is_some() {
        return Ok(None);
    }
    match row.last_error.as_deref() {
        Some(error @ (ORG_SHARE_ERR_TOO_LARGE | "seal_failed")) => {
            Ok(Some(NeverLandedOrgShare::PreDispatchFailure(error)))
        }
        _ => Ok(None),
    }
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
    let _lifecycle = lifecycle_guard(state);
    let item_id = item_id.trim();
    let Some(row) = state.db.org_share_by_item(item_id)? else {
        return Ok(None);
    };
    let Some(org) = state.db.get_org_state(&row.org_id)? else {
        return Ok(None);
    };
    if !org.context_enabled {
        return Ok(None);
    }
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
        let Some((folder_id, _, _)) = state.db.note_gate_anchor(&document_id)? else {
            return Ok(None);
        };
        if !folder_is_unlocked(state, &folder_id)? {
            return Ok(None);
        }
        return Ok(Some(OrgSourceRef {
            kind: "document".to_string(),
            source_id: document_id,
        }));
    }
    if let Some(meeting_id) = row.meeting_id {
        if !meeting_is_unlocked(state, &meeting_id)? {
            return Ok(None);
        }
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
    let _lifecycle = lifecycle_guard(st);
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
    app: AppHandle,
    state: State<'_, AppState>,
    folder_id: String,
) -> Result<(), AppError> {
    let _mutation = state.lock_org_mutation().await;
    state.db.begin_org_folder_closure(&folder_id)?;
    revoke_shares_for_folder_inner(state.inner(), &folder_id, Some(&app)).await
}

pub(crate) async fn revoke_shares_for_folder_inner(
    st: &AppState,
    folder_id: &str,
    app: Option<&AppHandle>,
) -> Result<(), AppError> {
    let link_user = st.db.active_link_user_shares_for_folder(folder_id)?;
    let org = st.db.active_org_share_ids_for_folder(folder_id)?;
    let mut first_err: Option<AppError> = None;
    let mut revoked_documents = std::collections::HashSet::new();

    for (share_id, _mode) in link_user {
        if let Err(e) = revoke_share_inner(st, share_id).await {
            first_err.get_or_insert(e);
        }
    }
    for (row_id, _item_id, _title) in org {
        let Some(row) = st.db.get_org_share(&row_id)? else {
            first_err.get_or_insert_with(|| {
                AppError::Unavailable("org share changed during folder revoke".into())
            });
            continue;
        };
        if let Some(doc_id) = row.doc_id.as_ref() {
            let org_id = row.org_id.clone();
            let doc_id = doc_id.clone();
            if !revoked_documents.insert((org_id, doc_id)) {
                continue;
            }
        }
        // A NULL item id can still name a stable document whose POST response was lost. Passing the
        // row id lets the central proof classifier either cancel a proven pre-dispatch row or route
        // the ambiguous stable document through DELETE and authenticated 404 corroboration.
        let res = revoke_org_share_row_with_policy(st, row, OrgWorkPolicy::manual(), app).await;
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
pub(crate) async fn revoke_org_shares_for_source_notifying(
    state: &AppState,
    meeting_id: Option<&str>,
    document_id: Option<&str>,
    app: Option<&AppHandle>,
) -> Result<(), AppError> {
    // Link and directed-user shares are remote ciphertext too. Revoke every known local share id
    // under the same durable source closure before any org-document work or local destruction.
    let outbound = state
        .db
        .active_outbound_shares_for_source(meeting_id, document_id)?;
    let rows = state
        .db
        .org_shares_for_source_revoke(meeting_id, document_id)?;
    let mut first_err: Option<AppError> = None;
    for share_id in outbound {
        if let Err(error) = revoke_share_inner(state, share_id).await {
            tracing::warn!(
                target: "share",
                error = %brief_err(&error),
                "delete-cascade: failed to revoke a live link/user share"
            );
            first_err.get_or_insert(error);
        }
    }
    let mut revoked_documents = std::collections::HashSet::new();
    for row in rows {
        let org_id = row.org_id.clone();
        if let Some(doc_id) = row.doc_id.as_deref() {
            if !revoked_documents.insert((org_id.clone(), doc_id.to_string())) {
                continue;
            }
        }
        let res = revoke_org_share_row_with_policy(state, row, OrgWorkPolicy::manual(), app).await;
        if let Err(e) = res {
            tracing::warn!(
                target: "org",
                org_id = %org_id,
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

/// One background org-sync tick: advance at most one outbound queue action, then pull + ingest one
/// bounded inbound-feed page for one round-robin org into the local int8 partition. This is what
/// makes the org brain a REPLICATED brain — every
/// member's app stays fresh for Ask/MCP WITHOUT anyone opening Settings. Best-effort: each half
/// warns-and-continues (a transient failure never kills the loop), and both inners gate to an early
/// `Ok` when logged out / no org joined, so this is a no-op until a session is live. Logs only
/// non-PII counts on a productive tick.
///
/// When the live pull finds NOTHING to pull, the tick spends its budget on ONE bounded step of the
/// ANTI-ENTROPY RECONCILE SWEEP instead (`org_reconcile_tick_with_policy`) — the slow second cursor
/// that re-walks the whole feed from 0 so a tombstone sitting BELOW the live cursor (the server never
/// re-seqs a withdrawal) is still eventually applied. The live pull always wins the budget while it
/// has work, so the sweep can never starve it.
///
/// Returns `true` when the local replica actually CHANGED this tick (≥1 ingest or tombstone from the
/// live pull, or ≥1 row converged by the sweep) — the caller (`lib.rs` loop) uses this to fire a
/// content-free [`crate::events::EVENT_ORG_FEED_UPDATED`] so an open FE view re-fetches WITHOUT
/// polling. Returns `false` on a no-op / error tick. An optional `AppHandle` is threaded through
/// production callers so productive visibility reductions can emit the history invalidation while
/// the lifecycle guard is still held.
pub(crate) async fn org_background_sync_tick(state: &AppState, app: Option<AppHandle>) -> bool {
    // The org mutex is taken PER PHASE below, not once around the whole tick.
    //
    // This tick runs four phases and every one of them makes network round trips. Holding
    // `org_share_mutation_lock` across all four meant a user action that needs it — sharing,
    // revoking, "sync now" — waited behind up to four consecutive HTTP timeouts on a tick that
    // fires every 60 s. Per phase, the worst case is one.
    //
    // Safe because per-phase acquisition is, for EVERY phase, at least as strong as that phase's
    // existing manual path: `org_sweep_pending` and `org_sync_now` are commands that already take
    // this lock around exactly one phase, and `sync_container_shares` /
    // `org_reconcile_memberships_notifying` already run their phase with no acquisition at all. So
    // this introduces no interleaving the app does not already perform. The `policy.is_current()`
    // checks between phases already assumed the tick is interruptible at these boundaries.
    let policy = OrgWorkPolicy::background(crate::perf::background_epoch());
    if !policy.is_current() {
        return false;
    }
    // Reconcile membership FIRST so a newly-invited org is present (and synced this same tick) and a
    // departed org is dropped before we pull its feed. Best-effort — a failure never blocks the sync.
    {
        let _mutation = state.lock_org_mutation().await;
        if let Err(e) = org_reconcile_memberships_with_policy(state, policy, app.as_ref()).await {
            tracing::warn!(target: "org", error = %brief_err(&e), "org membership reconcile tick failed");
        }
    }
    if !policy.is_current() {
        return false;
    }
    {
        let _mutation = state.lock_org_mutation().await;
        if let Err(e) = org_sweep_pending_with_policy(state, policy, app.as_ref()).await {
            tracing::warn!(target: "org", error = %e, "org outbound sweep tick failed");
        }
    }
    if !policy.is_current() {
        return false;
    }
    // Keep every shared container in step with the local tree. Best-effort for the same reason the
    // outbound sweep is: a container that cannot reconcile right now is retried next tick, and must
    // never stop the feed pull that follows.
    // NO acquisition around this phase — taking it here DEADLOCKS.
    //
    // `reconcile_container_shares` -> `reconcile_one_container_root` -> `share_to_org_placed_notifying`,
    // whose FIRST statement is `org_share_mutation_lock.lock().await`. `tokio::sync::Mutex` is not
    // reentrant, so a caller that already holds the guard awaits a second acquisition of the same
    // mutex on the same task and never returns — the guard is never dropped either, so the mutex is
    // held for the rest of the process and every later org command, `unlock_folder` included, blocks
    // forever. The tick has no timeout around it, so this does not recover.
    //
    // This is why `sync_container_shares` (the FE's explicit "sync now") takes NO lock before calling
    // the same function: the serialization is already inside `share_to_org_placed_notifying`. The
    // pre-2026-09-03 tick held one lock from its top across all four phases and so had exactly the
    // same latent deadlock; per-phase acquisition did not introduce it, but it does not excuse it,
    // and my "per phase is at least as strong as the manual path" argument was wrong HERE — for this
    // phase the manual path is lock-free because it must be.
    if let Err(e) =
        crate::commands::org_containers::reconcile_container_shares(state, app.as_ref()).await
    {
        tracing::warn!(target: "org", error = %brief_err(&e), "container share reconcile tick failed");
    }
    if !policy.is_current() {
        return false;
    }
    let report = {
        let _mutation = state.lock_org_mutation().await;
        match org_sync_now_inner_with_policy(state, policy, app.clone()).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(target: "org", error = %e, "org feed sync tick failed");
                return false;
            }
        }
    };
    let live_changed = report.ingested > 0 || report.tombstoned > 0;
    if live_changed {
        tracing::info!(
            target: "org",
            ingested = report.ingested,
            tombstoned = report.tombstoned,
            "org feed synced"
        );
    }
    // ANTI-ENTROPY: the slow reconcile sweep runs ONLY on a tick where the live pull found nothing to
    // pull. That is what makes "the live pull keeps priority" structural rather than a hope — while
    // any org is genuinely behind, every page budget goes to catching it up; the sweep only spends a
    // budget once the live cursor is idle. Best-effort: a failure never kills the loop.
    if !live_changed && report.pulled == 0 && policy.is_current() {
        match org_reconcile_tick_with_policy(state, policy, app).await {
            Ok(changed) => return changed > 0,
            Err(e) => {
                tracing::warn!(target: "org", error = %brief_err(&e), "org reconcile tick failed");
                return false;
            }
        }
    }
    live_changed
}

/// `org_sweep_pending()` — the on-launch org queue sweep (extends the mode-B `share_rewrap_pending`
/// launch pattern). Idempotent + OFFLINE-TOLERANT: logged out / no server / a per-row failure leaves
/// the row where it is for the next pass (never an error). One local repair pass plus three queues:
///   - `revoked` rows whose LOCAL replica is somehow still live → evicted through the ONE eviction
///     primitive, NO network (the server tombstone already landed for a `revoked` row). Defence in
///     depth behind `revoke_org_share_inner_with_policy`'s evict-before-flip ordering, and the one
///     step that runs even while logged out, since it needs neither a server nor a session;
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
pub async fn org_sweep_pending(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<u32, AppError> {
    let _mutation = state.lock_org_mutation().await;
    org_sweep_pending_with_policy(state.inner(), OrgWorkPolicy::manual(), Some(&app)).await
}

#[cfg(test)]
pub(crate) async fn org_sweep_pending_inner(state: &AppState) -> Result<u32, AppError> {
    let _mutation = state.lock_org_mutation().await;
    org_sweep_pending_with_policy(state, OrgWorkPolicy::manual(), None).await
}

#[cfg(test)]
pub(crate) async fn org_sweep_pending_background_once_inner(
    state: &AppState,
) -> Result<u32, AppError> {
    let _mutation = state.lock_org_mutation().await;
    org_sweep_pending_with_policy(
        state,
        OrgWorkPolicy::background(crate::perf::background_epoch()),
        None,
    )
    .await
}

/// Snapshot the bearer and stable actor from one live session after refresh. If the account changes
/// between refresh and this lock, fail closed rather than pairing one account's bearer with another
/// account's durable recovery witness.
pub(crate) async fn authenticated_org_actor(state: &AppState) -> Result<(String, String), AppError> {
    valid_access_token(state).await?;
    let session = state
        .account_session
        .lock()
        .map_err(|_| AppError::Storage("account-session mutex poisoned".into()))?;
    let session = crate::share::require_login(&session)?;
    let access_token = session.access_token.clone();
    let actor_user_id = session.server_user_id.clone().ok_or_else(|| {
        AppError::Unavailable("sign out and sign back in to recover organization sharing".into())
    })?;
    Ok((access_token, actor_user_id))
}

async fn org_sweep_pending_with_policy(
    state: &AppState,
    policy: OrgWorkPolicy,
    app: Option<&AppHandle>,
) -> Result<u32, AppError> {
    if !policy.is_current() {
        return Ok(0);
    }
    let mut advanced = 0u32;
    let mut attempted = 0usize;
    let background_limit = policy.background_epoch.map(|_| 1usize);

    // Link/user cleanup has two crash phases. `create_pending` first proves capability + reserves
    // the same owner-bound id, then becomes `revoke_pending`; the latter retries only DELETE. No
    // recovery path redispatches ciphertext. Resume both before any new share mutation so persisted
    // source/folder closures can converge after restart without reopening admission.
    let mut outbound_cleanup = state.db.outbound_shares_in_state("create_pending")?;
    outbound_cleanup.extend(state.db.outbound_shares_in_state("revoke_pending")?);
    for share_id in outbound_cleanup {
        if background_limit.is_some_and(|limit| attempted >= limit) || !policy.is_current() {
            return Ok(advanced);
        }
        attempted += 1;
        if revoke_share_inner(state, share_id).await.is_ok() {
            advanced = advanced.saturating_add(1);
        }
    }

    // Access PATCH is single-flight per stable document. A crash or ambiguous response leaves one
    // durable pending lease; recovery is GET-only and projects the authenticated current access.
    for access_attempt in pending_org_access_attempts(state)? {
        if background_limit.is_some_and(|limit| attempted >= limit) || !policy.is_current() {
            return Ok(advanced);
        }
        attempted += 1;
        if reconcile_pending_org_access_attempt(state, &access_attempt)
            .await
            .unwrap_or(false)
        {
            advanced = advanced.saturating_add(1);
        }
    }

    // 0a) LOCAL-ONLY ORPHAN REPAIR (defence in depth behind the eviction-before-`revoked` ordering in
    //     `revoke_org_share_inner_with_policy`). A device interrupted in the OLD order — or by any
    //     future bug that flips a row to `revoked` without evicting — keeps a fully decrypted, fully
    //     searchable replica of content it already withdrew from the server, and no queue re-drives a
    //     `revoked` row. This pass converges exactly those: for each `revoked` row that still names a
    //     server item, repair the exact retained deletion witness. A stable document repair evicts
    //     ALL locally held revisions and installs its terminal relation marker even when the named
    //     predecessor is already dead; a legacy item keeps the indexed live-state probe. NO NETWORK
    //     — the server tombstone already
    //     happened for a `revoked` row, so re-issuing it would only re-DELETE an already-gone item on
    //     every launch. Runs BEFORE the logged-out / no-server early returns because it needs neither,
    //     and is a pure read (zero write transactions) once every replica has converged. It does NOT
    //     spend the per-tick `attempted` budget — that budget bounds NETWORK/model work, and starving
    //     an outbound publish for a minute behind a purely local tombstone would be the wrong trade —
    //     but it IS bounded by `ORG_REVOKED_ORPHAN_REPAIR_PER_SWEEP` and by the background epoch.
    let mut repaired = 0usize;
    for row in state.db.list_org_shares_in_state("revoked")? {
        if repaired >= ORG_REVOKED_ORPHAN_REPAIR_PER_SWEEP || !policy.is_current() {
            break;
        }
        let Some(item_id) = row.item_id.as_deref() else {
            continue;
        };
        let repaired_now = if let Some(doc_id) = row.doc_id.as_deref() {
            // A stable revoked journal with a retained server item id is durable proof that the
            // old crash ordering completed its server DELETE but not local document
            // terminalization. Repair the WHOLE stable resource even when this named predecessor
            // is already tombstoned and a different revision remains live.
            policy
                .commit(|| {
                    commit_org_visibility_reduction(
                        state,
                        app.map(|app| app as &dyn AskHistoryInvalidationNotifier),
                        || {
                            state.db.repair_revoked_org_share_terminal_state(
                                &row.id,
                                &row.org_id,
                                doc_id,
                            )
                        },
                    )
                })?
                .unwrap_or(false)
        } else {
            if !state
                .db
                .org_replica_state(item_id)?
                .is_some_and(|held| !held.tombstoned)
            {
                continue;
            }
            policy
                .commit(|| {
                    commit_org_visibility_reduction(
                        state,
                        app.map(|app| app as &dyn AskHistoryInvalidationNotifier),
                        || state.db.evict_org_item(item_id),
                    )
                })?
                .unwrap_or(false)
        };
        if repaired_now {
            repaired += 1;
            advanced += 1;
            tracing::info!(
                target: "org",
                "evicted an orphaned local replica of an already-revoked org share"
            );
        }
    }
    for row in state.db.list_org_shares_in_state("failed")? {
        if row.last_error.as_deref() == Some(ORG_SHARE_PROJECTION_PENDING)
            && row.meeting_id.is_none()
            && row.document_id.is_none()
            && policy.commit(|| state.db.complete_source_less_projection_if_present(&row.id))?
                == Some(true)
        {
            advanced += 1;
        }
    }

    let base = share_base_url(state)?;
    if base.trim().is_empty() {
        return Ok(advanced);
    }
    // Logged out ⇒ nothing more to do (best-effort launch sweep, not an error).
    let (access_token, authenticated_actor_user_id) = match authenticated_org_actor(state).await {
        Ok(snapshot) => snapshot,
        Err(_) => return Ok(advanced),
    };
    if !policy.is_current() {
        return Ok(advanced);
    }
    let client = crate::share::client::ShareClient::new(&base)?;
    let direct_actor_user_id = Some(authenticated_actor_user_id);

    // 0b-rot) OWED KEY ROTATIONS, first of the network passes. A member was removed and the new
    // generation never committed, so until this settles anything published into that org is still
    // readable with the key the removed member holds. That makes it the most time-critical debt in
    // the sweep, ahead of any publish or access reconcile — a late publish is an inconvenience, a
    // late rotation is the removal not having happened in the way the user was told it had.
    // Failures leave the row in place (with its reason recorded) for the next sweep.
    if !state.db.list_org_rotations_due()?.is_empty() {
        advanced = advanced.saturating_add(drive_pending_org_rotations(state).await?);
        if !policy.is_current() {
            return Ok(advanced);
        }
    }

    // 0c) Initial stable POST ambiguity is resolved from its pre-dispatch witness before any source
    // reread. Never overwrite the committed hash with a newer local edit and never redispatch while
    // the authenticated head is unavailable. An exact head maps the remote item locally; a proven
    // different head is terminal. A proven absent head remains pending for explicit recovery.
    for row in state.db.list_org_shares_in_state("failed")? {
        if row.last_error.as_deref() != Some(ORG_SHARE_INITIAL_POST_PENDING) {
            continue;
        }
        if background_limit.is_some_and(|limit| attempted >= limit) || !policy.is_current() {
            return Ok(advanced);
        }
        let (
            Some(current_actor),
            Some(expected_actor),
            Some(expected_owner),
            Some(doc_id),
            Some(sha),
        ) = (
            direct_actor_user_id.as_deref(),
            row.expected_actor_user_id.as_deref(),
            row.expected_owner_user_id.as_deref(),
            row.doc_id.as_deref(),
            row.content_sha256.as_deref(),
        )
        else {
            continue;
        };
        let Some(expected_dispatch_id) = state.db.org_share_dispatch_id(&row.id)? else {
            continue;
        };
        if current_actor != expected_actor || current_actor != expected_owner {
            continue;
        }
        let Some(access) = crate::share::org_dto::OrgItemAccess::parse(&row.access) else {
            continue;
        };
        attempted += 1;
        let head =
            authoritative_org_document_head(state, &client, &access_token, &row.org_id, doc_id)
                .await;
        let now = chrono::Utc::now().to_rfc3339();
        match head {
            Ok(Some(head))
                if head.rev == row.rev
                    && head.generation == row.generation
                    && head.content_sha256.as_deref() == Some(sha)
                    && head.access == access
                    && head.author_user_id == expected_actor
                    && head.document_owner_user_id.as_deref() == Some(expected_owner) =>
            {
                let published = publish_response_from_authoritative_head(head);
                if policy
                    .commit(|| {
                        confirm_org_mutation_for_projection(
                            state,
                            &row,
                            &published,
                            ORG_SHARE_INITIAL_POST_PENDING,
                            &expected_dispatch_id,
                            &now,
                        )
                    })?
                    .is_some()
                {
                    advanced += 1;
                }
            }
            Ok(Some(head)) if head.rev == row.rev => {
                let _ = policy.commit(|| {
                    transition_initial_org_publish_intent(
                        state,
                        &row.id,
                        ORG_SHARE_ERR_EDIT_CONFLICT,
                        expected_actor,
                        expected_owner,
                        doc_id,
                        access,
                        row.rev,
                        row.generation,
                        sha,
                        row.scrub,
                        &expected_dispatch_id,
                        &now,
                    )
                })?;
            }
            Ok(Some(_)) => {}
            Ok(None) => {
                let _ = policy.commit(|| {
                    transition_initial_org_publish_intent(
                        state,
                        &row.id,
                        ORG_SHARE_INITIAL_POST_REPLAYABLE,
                        expected_actor,
                        expected_owner,
                        doc_id,
                        access,
                        row.rev,
                        row.generation,
                        sha,
                        row.scrub,
                        &expected_dispatch_id,
                        &now,
                    )
                })?;
            }
            Err(_) => {}
        }
    }

    // 0b) A direct stable-document PUT is NEVER replayed automatically. Its exact target revision,
    // generation, semantic hash, permission and expected old head were committed before dispatch.
    // After a crash/lost response, authenticate the relay's authoritative head: exact equality proves
    // the one attempt landed; a proven mismatch becomes a fixed terminal conflict. An unavailable
    // head remains pending for a later query-only sweep and is never re-dispatched.
    for row in state.db.list_org_shares_in_state("failed")? {
        if row.last_error.as_deref() != Some(ORG_SHARE_DIRECT_PUT_PENDING) {
            continue;
        }
        if background_limit.is_some_and(|limit| attempted >= limit) || !policy.is_current() {
            return Ok(advanced);
        }
        attempted += 1;
        let Some(expected_actor) = row.expected_actor_user_id.as_deref() else {
            continue;
        };
        let reconciled = match direct_actor_user_id.as_deref() {
            Some(current) if current == expected_actor => {
                reconcile_direct_org_update_attempt(
                    state,
                    &client,
                    &access_token,
                    &row,
                    expected_actor,
                )
                .await
            }
            _ => continue,
        };
        if !policy.is_current() {
            return Ok(advanced);
        }
        let now = chrono::Utc::now().to_rfc3339();
        match reconciled {
            Ok(DirectOrgUpdateResolution::Exact(published)) => {
                let Some(dispatch_id) = state.db.org_share_dispatch_id(&row.id)? else {
                    continue;
                };
                if policy
                    .commit(|| {
                        confirm_org_mutation_for_projection(
                            state,
                            &row,
                            &published,
                            ORG_SHARE_DIRECT_PUT_PENDING,
                            &dispatch_id,
                            &now,
                        )
                    })?
                    .is_some()
                {
                    advanced += 1;
                }
            }
            Ok(DirectOrgUpdateResolution::Conflict) => {
                let (
                    Some(doc_id),
                    Some(content_sha256),
                    Some(expected_owner),
                    Some(predecessor),
                    Some(access),
                    Some(dispatch_id),
                ) = (
                    row.doc_id.as_deref(),
                    row.content_sha256.as_deref(),
                    row.expected_owner_user_id.as_deref(),
                    row.item_id.as_deref(),
                    crate::share::org_dto::OrgItemAccess::parse(&row.access),
                    state.db.org_share_dispatch_id(&row.id)?,
                )
                else {
                    continue;
                };
                let _ = policy.commit(|| {
                    conflict_direct_org_update_intent(
                        state,
                        &row.id,
                        doc_id,
                        access,
                        expected_actor,
                        expected_owner,
                        predecessor,
                        row.rev,
                        row.generation,
                        content_sha256,
                        &dispatch_id,
                        &now,
                    )
                })?;
            }
            Ok(DirectOrgUpdateResolution::Inconclusive) => {}
            Err(_) => {}
        }
    }

    // A canonical source commit can land and crash before its command reaches the notifier. The
    // row-local monotonic counter makes that work durable; sweep each dirty uploaded source through
    // the normal exact-CAS republish path without manufacturing another source mutation.
    for row in state.db.list_dirty_uploaded_org_shares()? {
        if background_limit.is_some_and(|limit| attempted >= limit) || !policy.is_current() {
            return Ok(advanced);
        }
        attempted += 1;
        let changed = republish_org_shares_for_source_with_policy(
            state,
            row.meeting_id.as_deref(),
            row.document_id.as_deref(),
            policy,
            app,
        )
        .await
        .map(|count| count > 0)
        .unwrap_or(false);
        if changed {
            advanced += 1;
        }
    }

    // 0) DEDUP (auto-clean, user-opted-in): collapse accidental DUPLICATE live items — same org + same
    //    source — down to the earliest, tombstoning the extras. Fixes duplicates created BEFORE the
    //    idempotency guard existed (e.g. a double-click on Share), which self-healing needs a proactive
    //    pass to reach (the FE now blocks re-share, so the share-time collapse never re-fires for them).
    //    `duplicate_uploaded_org_shares` returns exactly the extras (never a keeper); each is torn down
    //    via the crash-safe revoke path (marks `revoke_pending` first, so an interrupted tombstone is
    //    completed by step 1 on the next pass). Best-effort — a network failure just retries next launch.
    for extra in state.db.duplicate_uploaded_org_shares()? {
        if background_limit.is_some_and(|limit| attempted >= limit) || !policy.is_current() {
            return Ok(advanced);
        }
        if let Some(item_id) = extra.item_id.clone() {
            attempted += 1;
            if revoke_org_share_inner_with_policy(state, item_id, policy, app)
                .await
                .is_ok()
            {
                advanced += 1;
            }
            if !policy.is_current() {
                return Ok(advanced);
            }
        }
    }

    // 1) Finish any pending revokes (a tombstone that didn't land before a crash).
    for row in state.db.list_org_shares_in_state("revoke_pending")? {
        if background_limit.is_some_and(|limit| attempted >= limit) || !policy.is_current() {
            return Ok(advanced);
        }
        if row.item_id.is_none() && row.doc_id.is_none() {
            // Missing both remote identities is not proof of absence. Preserve the retryable row for
            // explicit repair/quarantine rather than falsely declaring remote ciphertext revoked.
            continue;
        }
        attempted += 1;
        if revoke_org_share_row_with_policy(state, row, policy, app)
            .await
            .is_ok()
        {
            advanced += 1;
        }
        if !policy.is_current() {
            return Ok(advanced);
        }
    }

    // 2) Re-attempt any queued/failed publishes. Re-run the full gated share so a source sealed since
    //    queueing NEVER egresses (the read-gate refuses → the row stays `failed`).
    for state_label in ["queued", "failed"] {
        for row in state.db.list_org_shares_in_state(state_label)? {
            if background_limit.is_some_and(|limit| attempted >= limit) || !policy.is_current() {
                return Ok(advanced);
            }
            if row.last_error.as_deref() == Some(ORG_SHARE_DIRECT_PUT_PENDING) {
                continue;
            }
            if row.last_error.as_deref() == Some(ORG_SHARE_ERR_EDIT_CONFLICT) {
                continue;
            }
            if row.last_error.as_deref() == Some(ORG_SHARE_INITIAL_POST_PENDING) {
                continue;
            }
            if row.last_error.as_deref() == Some(ORG_SHARE_INITIAL_POST_REPLAYABLE)
                && !matches!(
                    (
                        direct_actor_user_id.as_deref(),
                        row.expected_actor_user_id.as_deref(),
                        row.expected_owner_user_id.as_deref(),
                    ),
                    (Some(current), Some(actor), Some(owner))
                        if current == actor && current == owner
                )
            {
                continue;
            }
            // Brain v3 size pre-check: `too_large` is TERMINAL for the sweep — retrying cannot
            // shrink the content, so requeueing it every launch is exactly the poison loop the
            // client-side cap check exists to kill. Recovery is content-driven (a manual re-share /
            // an edit-save republish re-reads the possibly-trimmed source and re-arms the row).
            if matches!(
                row.last_error.as_deref(),
                Some(ORG_SHARE_ERR_TOO_LARGE | ORG_SHARE_ERR_EDIT_CONFLICT)
                    | Some(
                        ORG_SHARE_PUBLISH_REJECTED
                            | "recovery_witness_missing"
                            | ORG_SHARE_PROJECTION_PENDING
                    )
            ) {
                continue;
            }
            if row.item_id.is_some() {
                attempted += 1;
                // Was live before: this row's LAST attempt was a REPUBLISH (not the initial publish)
                // and it failed — `set_org_share_failed` deliberately retains the OLD, still-server-live
                // `item_id` (only the success path's `reset_org_share_for_retry` clears it). The correct
                // retry here is `republish_org_shares_for_source` (bumps `rev`, tombstones the OLD item
                // only AFTER the new one lands) — `share_to_org_inner` would wrongly restart at `rev = 1`
                // and mint a genuine DUPLICATE item since the old one is still live on the server.
                // `org_shares_for_source` (the enumerator it reads through) now surfaces exactly this
                // shape (`failed` + non-null `item_id`), so this retry can actually find the row.
                let advanced_this_source = republish_org_shares_for_source_with_policy(
                    state,
                    row.meeting_id.as_deref(),
                    row.document_id.as_deref(),
                    policy,
                    app,
                )
                .await
                .map(|n| n > 0)
                .unwrap_or(false);
                if advanced_this_source {
                    advanced += 1;
                }
                if !policy.is_current() {
                    return Ok(advanced);
                }
                continue;
            }
            attempted += 1;
            let res = share_to_org_inner_with_policy(
                state,
                // Re-publish targets the SAME org the row was queued under (never the first via
                // `.next()`) — a multi-org account's sweep must re-share into the right org.
                &row.org_id,
                row.meeting_id.clone(),
                row.document_id.clone(),
                // Preserve the exact original choice. Existing pre-column rows migrated fail-safe
                // to `true`, while an explicit opt-out must replay the same canonical plaintext.
                row.scrub,
                crate::share::org_dto::OrgItemAccess::parse(&row.access)
                    .unwrap_or(crate::share::org_dto::OrgItemAccess::View),
                // Replay the row's OWN placement. A retry that dropped it would publish the note
                // outside the shared folder it was queued for, and the container sweep would then
                // see a document it thought it had filed sitting loose in the org.
                row.parent_container_id
                    .clone()
                    .map(|parent_container_id| ContainerPlacement {
                        parent_container_id,
                        position: row.position,
                        explicit: row.explicit,
                    }),
                policy,
                app,
            )
            .await;
            if res.is_ok() {
                // SB-3: `share_to_org_inner` REUSED this same row (dedup on the logical key) and
                // flipped it to `uploaded` on success — so there is NO stale row to revoke here (the
                // pre-fix code minted a fresh row per attempt and revoked the old one, which is
                // exactly the amplification we removed). Just count the advance.
                advanced += 1;
            }
            if !policy.is_current() {
                return Ok(advanced);
            }
        }
    }

    Ok(advanced)
}

/// `org_sync_now(org_id?)` — pull one bounded org-feed page, OPEN each ciphertext blob
/// with the (RAM-cached / grant-unwrapped) OCK, and INGEST it into the local decrypted replica + int8
/// retrieval partition. A TOMBSTONE evicts the item's chunks/vectors/FTS. Returns a content-free
/// [`OrgSyncReport`] (counts + `fts_only` + per-item error strings). Best-effort per item: a single
/// item whose OCK is unavailable / whose blob won't open is SKIPPED (recorded in `errors`), never
/// crashing the whole sync — the cursor still advances past a tombstone but STOPS at the first
/// un-openable LIVE item so a transient key gap is retried next sync (no silent skip-forward).
///
/// `org_id`: `Some(id)` syncs ONLY that (FE-picked, membership-checked) org; `None` selects one joined
/// org per call by process-local round-robin. This makes the page/RAM bound global to the invocation,
/// while the normal background cadence still services every joined org fairly.
#[tauri::command]
pub async fn org_sync_now(
    app: AppHandle,
    state: State<'_, AppState>,
    org_id: Option<String>,
) -> Result<crate::storage::models::OrgSyncReport, AppError> {
    let _mutation = state.lock_org_mutation().await;
    // The FE passes a SPECIFIC org id (→ sync only that org); the background tick / internal callers
    // pass `None` (→ sync the next round-robin org). This is the command-boundary
    // dispatch of the multi-org fix: a user-triggered "Sync now" from a picked org must not sync (or
    // report against) the wrong org.
    let mut report = match org_id {
        Some(id) => org_sync_one_now_with_app(state.inner(), &id, Some(app.clone())).await,
        None => {
            org_sync_now_inner_with_policy(
                state.inner(),
                OrgWorkPolicy::manual(),
                Some(app.clone()),
            )
            .await
        }
    }?;
    // A manual "Sync now" should converge the containers too, not just the item feed — otherwise a
    // user who just renamed a shared folder presses Sync and nothing happens. Best-effort: a
    // container failure never turns a successful feed sync into an error.
    if let Err(e) =
        crate::commands::org_containers::reconcile_container_shares(state.inner(), Some(&app)).await
    {
        tracing::warn!(target: "org", error = %brief_err(&e), "container share reconcile after manual sync failed");
        note_container_failure(&mut report, &e);
    }
    Ok(report)
}

/// One deliberately small feed page per org-sync invocation. A protocol-valid org blob may be up to
/// 16 MiB; keeping this at four bounds the decrypted/prepared page far below the old 200-item shape.
const ORG_FEED_PAGE: u32 = 4;

/// How many orphaned local replicas of ALREADY-`revoked` shares one `org_sweep_pending` pass may
/// evict (step 0a). Bounded for the same reason every other sweep step is: a launch/background pass
/// must stay a short, predictable amount of local work. The steady state is zero — the scan itself is
/// one indexed read per revoked row and writes nothing once every replica has converged — so this cap
/// only ever bites on a device with a genuine backlog, which the next pass continues.
const ORG_REVOKED_ORPHAN_REPAIR_PER_SWEEP: usize = 8;

/// One deliberately small ANTI-ENTROPY page per reconcile tick — same order of magnitude as
/// [`ORG_FEED_PAGE`], so the slow sweep can never starve the live pull nor hammer the server. The
/// sweep only runs on a tick where the live pull found NOTHING (see `org_background_sync_tick`), so
/// the live cursor always keeps priority.
const ORG_RECONCILE_PAGE: u32 = 4;

/// Multi-org fairness for the reconcile sweep, mirroring [`ORG_SYNC_ROUND_ROBIN`]. Process-local
/// scheduling only — the durable per-org `reconcile_seq` cursor is the authoritative position, so a
/// restart can repeat work but never skip a record.
static ORG_RECONCILE_ROUND_ROBIN: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// How long to wait after a COMPLETED full pass before starting the next one. Anti-entropy is a
/// safety net, not a poll: without this a member of a small org would re-walk its whole feed every
/// few idle minutes forever. Six hours keeps the repair well inside "the same day" while making the
/// steady-state cost of the sweep negligible. Only gates the START of a pass — a pass already in
/// flight always runs to completion.
const ORG_RECONCILE_PASS_COOLDOWN_SECS: i64 = 6 * 3600;

/// The un-targeted/background command consumes ONE global page budget, not one page per joined org.
/// This process-local cursor is sufficient because cursors themselves remain durable per org; after
/// restart beginning again at the oldest joined org can duplicate work, but can never skip feed data.
static ORG_SYNC_ROUND_ROBIN: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// A permanently unresolvable legacy author row must not monopolize every invocation's sole feed
/// page. While such rows remain, alternate the one-page budget between author repair and the live
/// cursor. This is scheduling only; both durable cursors/replica state remain in SQLite.
static ORG_AUTHOR_BACKFILL_TURN: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(true);

/// Rebuild the complete local org vector partition after a global embed-model switch without
/// touching the canonical decrypted replica, feed cursors, chunks or FTS. The caller MUST pass the
/// same REAL persistence handle already pinned for the surrounding global reindex and keep that
/// handle alive through this function's final DB commit.
///
/// Old vectors are purged first, making every observable intermediate state either vector-empty or
/// new-model-only. The keyset reader loads exactly one item's existing chunks at a time; embedding is
/// outside SQLite, and the short vector-only transaction commits only if the canonical item
/// seq/rev/generation/hash plus ordered chunk ids are still current. A concurrent feed
/// replace/tombstone is therefore never overwritten with stale vectors even if SQLite reuses rowids.
pub(crate) fn reindex_org_embeddings(
    db: &crate::storage::Db,
    embedder: &dyn crate::embed::Embedder,
) -> Result<usize, AppError> {
    db.purge_all_org_vectors()?;

    let mut after_item_id: Option<String> = None;
    let mut indexed = 0usize;
    loop {
        let Some(batch) = db.next_org_item_vector_batch(after_item_id.as_deref())? else {
            break;
        };
        let next_item_id = batch.item_id.clone();
        let vector_blobs = crate::storage::Db::prepare_org_vector_blobs(&batch.texts, embedder)?;
        if db.commit_org_item_vectors_if_unchanged(&batch, &vector_blobs)? {
            indexed += 1;
        }
        after_item_id = Some(next_item_id);
    }
    Ok(indexed)
}

/// Repair at most `max_items` vectorless org items without purging the partition. This is the
/// startup/background continuation for an interrupted explicit reindex and for items previously
/// ingested FTS-only. The global keyset reader materializes one item at a time.
///
/// For scheduled work, the caller must pass BOTH the tick's epoch here and a
/// [`crate::embed::background_persistence_embedder`] created from that same epoch. The background
/// embedder guards model work; `OrgWorkPolicy` independently gates each short vector CAS commit.
pub(crate) fn repair_missing_org_embeddings(
    db: &crate::storage::Db,
    embedder: &dyn crate::embed::Embedder,
    max_items: usize,
    background_epoch: Option<u64>,
) -> Result<usize, AppError> {
    let policy = match background_epoch {
        Some(epoch) => OrgWorkPolicy::background(epoch),
        None => OrgWorkPolicy::manual(),
    };
    let mut after_item_id: Option<String> = None;
    let mut repaired = 0usize;
    for _ in 0..max_items {
        if !policy.is_current() {
            break;
        }
        let Some(batch) = db.next_missing_org_item_vector_batch(after_item_id.as_deref())? else {
            break;
        };
        let next_item_id = batch.item_id.clone();
        let vector_blobs = crate::storage::Db::prepare_org_vector_blobs(&batch.texts, embedder)?;
        match policy.commit(|| db.commit_org_item_vectors_if_unchanged(&batch, &vector_blobs))? {
            Some(true) => repaired += 1,
            Some(false) => {}
            None => break,
        }
        after_item_id = Some(next_item_id);
    }
    Ok(repaired)
}

/// Sync exactly ONE (FE-targeted) org's feed — the single-org boundary of [`org_sync_now`]. Resolves
/// the org (membership-checked, never `.next()`), then runs the same per-org pull/ingest via
/// `org_sync_one` used by the round-robin scheduler. Offline / logged-out ⇒ an empty report (no-op),
/// matching the untargeted path.
#[cfg(test)]
pub(crate) async fn org_sync_one_now_inner(
    state: &AppState,
    org_id: &str,
) -> Result<crate::storage::models::OrgSyncReport, AppError> {
    let _mutation = state.lock_org_mutation().await;
    org_sync_one_now_with_app(state, org_id, None).await
}

async fn org_sync_one_now_with_app(
    state: &AppState,
    org_id: &str,
    app: Option<AppHandle>,
) -> Result<crate::storage::models::OrgSyncReport, AppError> {
    org_sync_one_now_with_app_and_policy(state, org_id, app, OrgWorkPolicy::manual()).await
}

#[cfg(test)]
pub(crate) async fn org_sync_one_now_with_pre_task_reader(
    state: &AppState,
    org_id: &str,
) -> Result<crate::storage::models::OrgSyncReport, AppError> {
    let _mutation = state.lock_org_mutation().await;
    org_sync_one_now_with_app_and_policy(state, org_id, None, OrgWorkPolicy::pre_task_reader()).await
}

async fn org_sync_one_now_with_app_and_policy(
    state: &AppState,
    org_id: &str,
    app: Option<AppHandle>,
    policy: OrgWorkPolicy,
) -> Result<crate::storage::models::OrgSyncReport, AppError> {
    let mut report = crate::storage::models::OrgSyncReport::default();
    let org = resolve_org(state, org_id)?;
    let base = share_base_url(state)?;
    if base.trim().is_empty() {
        return Ok(report);
    }
    let access = match reconcile_token_outcome(valid_access_token(state).await) {
        TokenOutcome::Proceed(a) => a,
        // A dead session must not masquerade as a clean sync: an empty report renders as
        // "Synced — up to date.", which is the most misleading thing the panel can say.
        TokenOutcome::Fatal(e) => return Err(e),
        TokenOutcome::SkipQuietly => return Ok(report),
    };
    let client = crate::share::client::ShareClient::new(&base)?;
    org_sync_one(
        state,
        &client,
        &access,
        &org,
        &mut report,
        policy,
        app,
    )
    .await?;
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

#[cfg(test)]
pub(crate) async fn org_sync_now_inner(
    state: &AppState,
) -> Result<crate::storage::models::OrgSyncReport, AppError> {
    let _mutation = state.lock_org_mutation().await;
    org_sync_now_inner_with_policy(state, OrgWorkPolicy::manual(), None).await
}

async fn org_sync_now_inner_with_policy(
    state: &AppState,
    policy: OrgWorkPolicy,
    app: Option<AppHandle>,
) -> Result<crate::storage::models::OrgSyncReport, AppError> {
    let mut report = crate::storage::models::OrgSyncReport::default();

    if !policy.is_current() {
        return Ok(report);
    }

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
    let access = match reconcile_token_outcome(valid_access_token(state).await) {
        TokenOutcome::Proceed(a) => a,
        // A dead session must not masquerade as a clean sync: an empty report renders as
        // "Synced — up to date.", which is the most misleading thing the panel can say.
        TokenOutcome::Fatal(e) => return Err(e),
        TokenOutcome::SkipQuietly => return Ok(report),
    };
    if !policy.is_current() {
        return Ok(report);
    }
    let client = crate::share::client::ShareClient::new(&base)?;

    // MULTI-ORG FAIRNESS under a GLOBAL one-page budget: choose one locally-joined org per call.
    // Each org keeps its own durable feed cursor, so this in-memory round-robin only schedules work;
    // it is never authoritative state and a restart cannot skip anything.
    let index =
        ORG_SYNC_ROUND_ROBIN.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % orgs.len();
    let org = &orgs[index];
    if let Err(e) = org_sync_one(
        state,
        &client,
        &access,
        org,
        &mut report,
        policy,
        app,
    )
    .await
    {
        if policy.is_current() {
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
        "org feed sync (round-robin org)"
    );
    Ok(report)
}

/// Sync ONE org's append-only feed from its own cursor into the local decrypted replica + retrieval
/// partition. Used by both the targeted command and the one-org round-robin scheduler; aggregates
/// counts into the shared `report`.
async fn org_sync_one(
    state: &AppState,
    client: &crate::share::client::ShareClient,
    access: &str,
    org: &crate::storage::OrgState,
    report: &mut crate::storage::models::OrgSyncReport,
    policy: OrgWorkPolicy,
    app: Option<AppHandle>,
) -> Result<(), AppError> {
    if !policy.is_current() {
        return Ok(());
    }

    // Legacy author repair consumes this invocation's SAME single-page budget instead of issuing a
    // second side request after the main feed. It is rare (new local/feed rows stamp the author at
    // insertion). If an old row cannot be matched server-side, alternate repair and live-cursor turns
    // so that row can never stall newer feed entries forever. No blob/model work is needed here.
    let has_missing_author = !state
        .db
        .org_items_with_null_author_seq(&org.org_id, ORG_FEED_PAGE as i64)?
        .is_empty();
    let take_backfill_turn = has_missing_author
        && ORG_AUTHOR_BACKFILL_TURN.fetch_xor(true, std::sync::atomic::Ordering::Relaxed);
    if take_backfill_turn {
        backfill_null_org_item_authors(state, client, access, &org.org_id, report, policy).await;
        report.last_seq = state.db.org_last_seq_for(&org.org_id)?;
        return Ok(());
    }

    // ── ASYNC PULL PHASE — fetch one page, opening each cell; buffer only that bounded page ───────
    // The embedder (`dyn Embedder`, !Send) is deliberately NOT constructed here: the whole async
    // section is Send-safe, and the INGEST phase below owns the embedder entirely INSIDE its
    // `perf::run_heavy` blocking closure (never held across an `.await`). A tombstone applies
    // immediately (no key/blob needed).
    //
    // FIX D — per-item failures are classified TRANSIENT vs TERMINAL so one poison item never stalls
    // every later page forever (pre-fix: EVERY failure stopped page draining, so one un-openable cell
    // blocked all newer items indefinitely):
    //   • TRANSIENT (OCK unavailable / blob fetch network error) → stop this page, cursor NOT advanced:
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
            doc_id: Option<String>,
            is_current: bool,
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
            env: Box<crate::share::org_envelope::OrgEnvelope>,
            attachments: Vec<crate::storage::IncomingAttachment>,
            sha: Vec<u8>,
            /// The author's server account id, off the feed entry — stored on the local replica so the
            /// author's OTHER machines can recognise + edit their own item (2026-07-14).
            author_user_id: String,
            doc_id: Option<String>,
            access: crate::share::org_dto::OrgItemAccess,
            document_owner_user_id: Option<String>,
            is_current: bool,
        },
        /// A CONTAINER manifest → `org_containers`, never `org_items`.
        ///
        /// This arm exists because the feed has TWO ingest paths — this live pull and the
        /// anti-entropy reconcile sweep — and 2.1.0 shipped the container branch on only the
        /// sweep. A manifest arriving through the live pull was therefore written as an ordinary
        /// note, so a shared Space showed up in Shared Brains as a note named after the folder
        /// instead of appearing in the sidebar as a Space.
        IngestContainer {
            item_id: String,
            seq: u64,
            rev: u32,
            generation: u32,
            manifest: Box<crate::share::container_envelope::ContainerEnvelope>,
            author_hint: String,
            author_user_id: String,
            access: crate::share::org_dto::OrgItemAccess,
            document_owner_user_id: Option<String>,
            created_at: String,
        },
    }
    let mut actions: Vec<FeedAction> = Vec::new();
    let cursor = state.db.org_last_seq_for(&org.org_id)?;
    let feed = client
        .org_feed(access, &org.org_id, cursor, ORG_FEED_PAGE)
        .await?;
    if !policy.is_current() {
        return Ok(());
    }
    if feed.items.len() > ORG_FEED_PAGE as usize {
        return Err(AppError::Unavailable(
            "org server exceeded the requested bounded feed page".into(),
        ));
    }
    validate_org_feed_page_metadata(&feed)?;

    let mut last_feed_seq = cursor;
    'items: for item in &feed.items {
        if !policy.is_current() {
            return Ok(());
        }
        if item.seq <= last_feed_seq {
            return Err(AppError::Unavailable(
                "org server returned a non-increasing feed sequence".into(),
            ));
        }
        last_feed_seq = item.seq;
        report.pulled += 1;

        if item.tombstoned {
            actions.push(FeedAction::Tombstone {
                item_id: item.item_id.clone(),
                seq: item.seq,
                doc_id: item.doc_id.clone(),
                is_current: resolved_org_item_is_current(item),
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
            report.errors.push(format!(
                "item {}: live entry missing content hash",
                item.item_id
            ));
            actions.push(FeedAction::SkipTerminal { seq: item.seq });
            continue;
        };
        let item_nonce = org_item_nonce(&sha);

        // Resolve the OCK for THIS item's generation (RAM cache / grant unwrap; gated on MK
        // session). Unavailable ⇒ TRANSIENT key gap → record + STOP (retried next sync).
        let ock =
            match acquire_org_ock_with_policy(state, &org.org_id, item.generation, policy).await {
                Ok(k) => k,
                Err(e) => {
                    if !policy.is_current() {
                        return Ok(());
                    }
                    report.errors.push(format!(
                        "item {}: key unavailable ({})",
                        item.item_id,
                        brief_err(&e)
                    ));
                    break 'items;
                }
            };
        if !policy.is_current() {
            return Ok(());
        }
        let ciphertext = match client.get_blob(access, &blob_id).await {
            Ok(c) => c,
            Err(e) => {
                if !policy.is_current() {
                    return Ok(());
                }
                // TRANSIENT: a network blob-fetch failure may succeed next sync → STOP, don't skip.
                report.errors.push(format!(
                    "item {}: blob fetch failed ({})",
                    item.item_id,
                    brief_err(&e)
                ));
                break 'items;
            }
        };
        if !policy.is_current() {
            return Ok(());
        }
        if ciphertext.len() > murmur_protocol::caps::MAX_ORG_ITEM_BLOB_BYTES {
            report.errors.push(format!(
                "item {}: blob exceeds the protocol size cap",
                item.item_id
            ));
            actions.push(FeedAction::SkipTerminal { seq: item.seq });
            continue;
        }
        // OPEN (verify-before-trust: fails closed on wrong OCK / tampered cell / wrong AAD).
        let env = match policy
            .reader
            .open(&ock, &ciphertext, &org.org_id, &item_nonce)
        {
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
        if env.kind == crate::share::org_envelope::OrgItemKind::Container {
            // Structure, not prose: a manifest skips the attachment bundle, the chunker and the
            // embedder entirely. A payload this device cannot parse is terminal for that seq —
            // never half-written, so a malformed manifest cannot leave a nameless container behind.
            match crate::share::container_envelope::ContainerEnvelope::from_json(&env.markdown) {
                Ok(manifest) => actions.push(FeedAction::IngestContainer {
                    item_id: item.item_id.clone(),
                    seq: item.seq,
                    rev: item.rev,
                    generation: item.generation,
                    manifest: Box::new(manifest),
                    author_hint: env.author_hint.clone(),
                    author_user_id: item.author_user_id.clone(),
                    access: item.access,
                    document_owner_user_id: item.document_owner_user_id.clone(),
                    created_at: item.created_at.clone(),
                }),
                Err(_) => {
                    report
                        .errors
                        .push(format!("item {}: container manifest invalid", item.item_id));
                    actions.push(FeedAction::SkipTerminal { seq: item.seq });
                }
            }
            continue;
        }
        // Validate the complete authenticated bundle before any local write, then replace wire ids
        // with fresh local UUIDs. A malformed bundle is terminal for this ciphertext.
        let (local_markdown, incoming_attachments) =
            match prepare_incoming_attachment_bundle(&env.markdown, &env.attachments) {
                Ok(bundle) => bundle,
                Err(_) => {
                    report
                        .errors
                        .push(format!("item {}: attachment bundle invalid", item.item_id));
                    actions.push(FeedAction::SkipTerminal { seq: item.seq });
                    continue;
                }
            };
        let mut env = env;
        env.markdown = local_markdown;
        env.attachments.clear();
        actions.push(FeedAction::Ingest {
            item_id: item.item_id.clone(),
            seq: item.seq,
            rev: item.rev,
            generation: item.generation,
            env: Box::new(env),
            attachments: incoming_attachments,
            sha,
            author_user_id: item.author_user_id.clone(),
            doc_id: item.doc_id.clone(),
            access: item.access,
            document_owner_user_id: item.document_owner_user_id.clone(),
            is_current: resolved_org_item_is_current(item),
        });
    }

    // ── INGEST PHASE — on the blocking pool, through the ONE global heavy-inference gate ──────────
    // Ingesting an item embeds it via Candle/Metal (`upsert_org_item` → `embed_passage`), which used
    // to run INLINE on this async command's Tokio worker AND outside `perf::run_heavy` — a large feed
    // pull could run an ungated Metal forward pass concurrently with transcription/diarization. Route
    // the whole apply loop through the shared gate like every other heavy native call site. The same
    // pinned REAL persistence handle also repairs at most one older FTS-only item after the page, so a
    // model installed after an offline/default ingest eventually fills the backlog without mixing
    // vector spaces. The handle lives through every exact DB commit and drops before the next await.
    let has_missing_vector = !state.db.org_items_needing_embed(&org.org_id, 1)?.is_empty();
    if !actions.is_empty() || has_missing_vector {
        let db = state.db.clone();
        let org_id = org.org_id.clone();
        let background_epoch = policy.background_epoch;
        let app_for_apply = app;
        let (tombstoned, ingested, fts_only) =
            crate::perf::run_heavy(
                &state.heavy_inference,
                move || -> Result<(u32, u32, bool), AppError> {
                    // Resolve once, inside the blocking closure: one immutable real model snapshot for
                    // every live item in this page. Missing model is honest FTS-only; init/forward errors
                    // still fail loud and never degrade to persisted stub vectors.
                    let embedder: Option<Box<dyn crate::embed::Embedder>> = match background_epoch {
                        Some(epoch) => crate::embed::background_persistence_embedder(epoch).ok(),
                        None => crate::embed::active_persistence_embedder_if_available(),
                    };
                    let mut fts_only = embedder.is_none();
                    let embedder_ref: Option<&dyn crate::embed::Embedder> = embedder.as_deref();
                    let mut tombstoned = 0u32;
                    let mut ingested = 0u32;
                    for action in actions {
                        if !policy.is_current() {
                            break;
                        }
                        match action {
                            FeedAction::Tombstone {
                                item_id,
                                seq,
                                doc_id,
                                is_current,
                            } => {
                                let Some((applied, _evicted)) = policy.commit(|| {
                                    if let Some(handle) = app_for_apply.as_ref() {
                                        let app_state = handle.state::<AppState>();
                                        let mut outcome = (false, false);
                                        commit_org_visibility_reduction(
                                            app_state.inner(),
                                            Some(handle),
                                            || {
                                                outcome = db
                                                    .commit_org_feed_tombstone_with_metadata_outcome(
                                                        &org_id,
                                                        &item_id,
                                                        seq,
                                                        doc_id.as_deref(),
                                                        is_current,
                                                    )?;
                                                Ok(outcome.1)
                                            },
                                        )?;
                                        Ok(outcome)
                                    } else {
                                        db.commit_org_feed_tombstone_with_metadata_outcome(
                                            &org_id,
                                            &item_id,
                                            seq,
                                            doc_id.as_deref(),
                                            is_current,
                                        )
                                    }
                                })?
                                else {
                                    break;
                                };
                                if applied {
                                    tombstoned += 1;
                                }
                            }
                            // FIX D: a permanently un-ingestable item advances the cursor past its seq (no DB
                            // write), so it never stalls the feed — the good item behind it ingests on the SAME sync.
                            FeedAction::SkipTerminal { seq } => {
                                let Some(_applied) = policy
                                    .commit(|| db.commit_org_feed_terminal_skip(&org_id, seq))?
                                else {
                                    break;
                                };
                            }
                            FeedAction::Ingest {
                                item_id,
                                seq,
                                rev,
                                generation,
                                env,
                                attachments,
                                sha,
                                author_user_id,
                                doc_id,
                                access,
                                document_owner_user_id,
                                is_current,
                            } => {
                                let env = *env;
                                let prepared =
                                    match crate::storage::Db::prepare_org_item_index_for_kind(
                                        env.kind,
                                        &env.title,
                                        &env.created_at,
                                        &env.markdown,
                                        embedder_ref,
                                    ) {
                                        Ok(prepared) => prepared,
                                        Err(AppError::Unavailable(_))
                                            if background_epoch.is_none() =>
                                        {
                                            // A manual sync remains usable during capture, but never runs an
                                            // unscoped model there. Preserve chunk/FTS ingestion only.
                                            fts_only = true;
                                            crate::storage::Db::prepare_org_item_index_for_kind(
                                                env.kind,
                                                &env.title,
                                                &env.created_at,
                                                &env.markdown,
                                                None,
                                            )?
                                        }
                                        Err(e) => return Err(e),
                                    };
                                let author_ref = if author_user_id.is_empty() {
                                    None
                                } else {
                                    Some(author_user_id.as_str())
                                };
                                let Some(applied) = policy.commit(|| {
                                    let commit = || {
                                        db.commit_org_feed_item_with_metadata_and_attachments(
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
                                            env.source_kind.map(
                                                crate::share::org_envelope::OrgSourceKind::as_str,
                                            ),
                                            author_ref,
                                            &prepared,
                                            doc_id.as_deref(),
                                            access.as_str(),
                                            document_owner_user_id.as_deref(),
                                            is_current,
                                            &attachments,
                                        )
                                    };
                                    let outcome = if let Some(handle) = app_for_apply.as_ref() {
                                        let app_state = handle.state::<AppState>();
                                        commit_org_metadata_mutation(
                                            app_state.inner(),
                                            Some(handle),
                                            commit,
                                        )?
                                    } else {
                                        commit()?
                                    };
                                    Ok(outcome.changed)
                                })?
                                else {
                                    break;
                                };
                                if applied {
                                    ingested += 1;
                                }
                                // WHERE the sender filed this document. Written after the item row
                                // exists, and unconditionally: a document that LEAVES a shared
                                // folder arrives with no placement, and clearing it is what makes
                                // it move. 2.1.0 wrote this only on the reconcile-sweep path, so a
                                // document arriving through the live pull was never filed at all.
                                let placement = env.placement.as_ref();
                                let _ = db.set_org_item_placement(
                                    &item_id,
                                    placement.map(|p| p.parent_container_id.as_str()),
                                    placement.map(|p| p.position).unwrap_or(0),
                                );
                            }
                            FeedAction::IngestContainer {
                                item_id,
                                seq,
                                rev,
                                generation,
                                manifest,
                                author_hint,
                                author_user_id,
                                access,
                                document_owner_user_id,
                                created_at,
                            } => {
                                let manifest = *manifest;
                                let row = crate::storage::models::OrgContainerRow {
                                    org_id: org_id.clone(),
                                    container_id: manifest.container_id.clone(),
                                    item_id: item_id.clone(),
                                    level: manifest.level.as_str().to_string(),
                                    name: manifest.name.clone(),
                                    emoji: manifest.emoji.clone(),
                                    tint: manifest.tint.clone(),
                                    parent_container_id: manifest.parent_container_id.clone(),
                                    position: manifest.position,
                                    access: access.as_str().to_string(),
                                    author_hint,
                                    author_user_id: (!author_user_id.is_empty())
                                        .then_some(author_user_id),
                                    document_owner_user_id,
                                    seq,
                                    rev,
                                    generation,
                                    created_at,
                                };
                                // The cursor must advance with the write, or the same manifest is
                                // replayed on every pull.
                                let Some(()) = policy.commit(|| {
                                    db.upsert_org_container(&row)?;
                                    db.set_org_last_seq(&org_id, seq as i64)
                                })?
                                else {
                                    break;
                                };
                                ingested += 1;
                            }
                        }
                    }

                    // One bounded backlog repair under this page's SAME pinned real handle. Re-read the
                    // candidate after applying the page because the action loop may itself have filled it.
                    // Model work is outside the epoch closure; only the vector CAS transaction is gated.
                    if policy.is_current() {
                        if let Some(embedder) = embedder_ref {
                            if let Some(item_id) =
                                db.org_items_needing_embed(&org_id, 1)?.into_iter().next()
                            {
                                if let Some(batch) = db.org_item_vector_batch(&item_id)? {
                                    match crate::storage::Db::prepare_org_vector_blobs(
                                        &batch.texts,
                                        embedder,
                                    ) {
                                        Ok(vector_blobs) => {
                                            if policy
                                                .commit(|| {
                                                    db.commit_org_item_vectors_if_unchanged(
                                                        &batch,
                                                        &vector_blobs,
                                                    )
                                                })?
                                                .is_none()
                                            {
                                                return Ok((tombstoned, ingested, fts_only));
                                            }
                                        }
                                        Err(AppError::Unavailable(_))
                                            if background_epoch.is_none() =>
                                        {
                                            // Manual sync stays usable during capture; the FTS index is
                                            // already valid and this one vector repair remains queued.
                                            fts_only = true;
                                        }
                                        Err(_) if !policy.is_current() => {}
                                        Err(e) => return Err(e),
                                    }
                                }
                            }
                        }
                    }
                    Ok((tombstoned, ingested, fts_only))
                },
            )
            .await?;
        report.fts_only = fts_only;
        report.tombstoned += tombstoned;
        report.ingested += ingested;
    }

    // `report.last_seq` reflects the LAST org synced (per-org field on an aggregate report).
    report.last_seq = state.db.org_last_seq_for(&org.org_id)?;
    Ok(())
}

// ── ANTI-ENTROPY RECONCILE SWEEP (2026-07-26) ─────────────────────────────────────────────────────
//
// THE BUG THIS EXISTS FOR. The server withdraws an org item with
// `UPDATE org_items SET tombstoned_at = now()` and NEVER changes that item's `seq`; the feed is
// `WHERE org_id = ? AND seq > ?`. So a member whose live cursor (`org_state.last_seq`) is ALREADY
// past that seq can never be told the item is gone: the decrypted local replica (`org_items.markdown`
// + `org_chunks` + `fts_org_chunks` + `org_vec_chunks` + `note_attachments`) survives forever and
// stays searchable through org search, Ask and MCP `org_search`. Because every edit publishes rev+1
// as a NEW item and tombstones the old one, recipients also accumulate every superseded revision.
//
// THE FIX. A SECOND, deliberately slow cursor (`org_state.reconcile_seq`) that restarts at 0 and
// walks the entire feed in small bounded steps, applying whatever the feed currently says. It NEVER
// writes `last_seq`. A server-side fix (assigning a fresh `seq` on tombstone) lands separately; this
// sweep is what repairs replicas ALREADY orphaned on real machines, and remains a correct backstop
// afterwards.

/// Run ONE anti-entropy reconcile step immediately (manual policy — never epoch-deferred). Returns
/// the number of local replica rows this step changed. The scheduled path is
/// `org_background_sync_tick`; this is the direct entry point for internal callers and for the
/// regression tests that drive the sweep against a mock feed.
pub async fn org_reconcile_now_inner(state: &AppState) -> Result<u32, AppError> {
    let _mutation = state.lock_org_mutation().await;
    org_reconcile_tick_with_policy(state, OrgWorkPolicy::manual(), None).await
}

/// One anti-entropy reconcile tick: pick one joined org (round-robin) and walk one bounded
/// [`ORG_RECONCILE_PAGE`] of its feed from the SLOW cursor. Returns how many local replica rows this
/// tick actually changed (evicted + re-ingested), so the caller can fire `org-feed-updated`.
///
/// CONFIGURATION-tolerant, NOT network-tolerant — the distinction matters, so state it exactly. No
/// joined org, no configured server, or no valid session ⇒ `Ok(0)`: nothing to reconcile is not a
/// failure. But once a request is actually made, a failing `client.org_feed(..)` PROPAGATES as `Err`
/// from here (unlike the per-record failures inside `org_reconcile_one`, which are deliberately
/// swallowed so the slow cursor can never stall short of a tombstone). The scheduled caller
/// `org_background_sync_tick` is what makes an offline tick harmless: it logs a non-PII warning and
/// returns `false`. Kept as an `Err` rather than folded into `Ok(0)` because the manual entry point
/// (`org_reconcile_now_inner`, used by internal callers and the regression tests) needs to be able to
/// tell "the feed is genuinely converged" from "the server could not be reached" — collapsing them
/// would make an unreachable server indistinguishable from a healthy no-op.
///
/// Honors the background epoch through `policy` at every await boundary and every DB commit.
async fn org_reconcile_tick_with_policy(
    state: &AppState,
    policy: OrgWorkPolicy,
    app: Option<AppHandle>,
) -> Result<u32, AppError> {
    if !policy.is_current() {
        return Ok(0);
    }
    let orgs = state.db.list_org_states()?;
    if orgs.is_empty() {
        return Ok(0);
    }
    let base = share_base_url(state)?;
    if base.trim().is_empty() {
        return Ok(0);
    }
    let access = match valid_access_token(state).await {
        Ok(a) => a,
        Err(_) => return Ok(0),
    };
    if !policy.is_current() {
        return Ok(0);
    }
    let client = crate::share::client::ShareClient::new(&base)?;
    let index =
        ORG_RECONCILE_ROUND_ROBIN.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % orgs.len();
    let org = &orgs[index];
    org_reconcile_one(state, &client, &access, org, policy, app).await
}

/// Is the last COMPLETED reconcile pass still inside [`ORG_RECONCILE_PASS_COOLDOWN_SECS`]?
/// `None`/unparseable ⇒ `false` (never joined, never completed, or a corrupt stamp ⇒ sweep now:
/// failing OPEN here just costs one bounded page, while failing closed would silently disable the
/// whole repair). A stamp in the future (a clock that jumped backwards) also reads as "recent",
/// which self-corrects the moment the clock does.
fn recently_reconciled(pass_at: Option<&str>) -> bool {
    let Some(at) = pass_at else {
        return false;
    };
    let Ok(at) = chrono::DateTime::parse_from_rfc3339(at) else {
        return false;
    };
    let age = chrono::Utc::now()
        .signed_duration_since(at.with_timezone(&chrono::Utc))
        .num_seconds();
    age < ORG_RECONCILE_PASS_COOLDOWN_SECS
}

/// Reconcile ONE bounded page of one org's feed against the local replica, from the slow cursor.
///
/// Per record, in feed order:
///   - a TOMBSTONE record → evict the local replica through the ONE eviction primitive
///     (`Db::evict_org_item`), idempotently;
///   - a LIVE record whose `content_sha256` already matches what is stored → repair stable
///     document/access/owner metadata with NO blob fetch;
///   - a LIVE record already tombstoned locally → SKIP (an append-only tombstone is permanent; the
///     sweep must never resurrect withdrawn plaintext);
///   - a LIVE record that is missing locally or diverged → open + ingest it.
///
/// UNLIKE the live pull, the sweep NEVER stalls: any per-record failure (key gap, blob fetch error,
/// broken cell) just advances the slow cursor past it, because stalling is precisely the failure mode
/// that would stop the sweep from ever reaching the tombstones behind it. The record comes around
/// again on the next pass.
///
/// Walking off the end of the feed COMPLETES the pass: stamp `reconcile_pass_at` and rewind
/// `reconcile_seq` to 0 so the next pass re-observes every record. `last_seq` is never written here.
async fn org_reconcile_one(
    state: &AppState,
    client: &crate::share::client::ShareClient,
    access: &str,
    org: &crate::storage::OrgState,
    policy: OrgWorkPolicy,
    app: Option<AppHandle>,
) -> Result<u32, AppError> {
    if !policy.is_current() {
        return Ok(0);
    }
    /// One decided reconcile action. Every feed record in the page gets exactly one, so the slow
    /// cursor can advance strictly through what has actually been handled.
    enum ReconcileAction {
        /// Already converged (or deliberately left alone) — nothing to write.
        Skip { seq: u64 },
        /// Content hash already converged, but stable permission/link metadata still needs repair.
        RepairMetadata {
            item_id: String,
            seq: u64,
            generation: u32,
            doc_id: Option<String>,
            access: crate::share::org_dto::OrgItemAccess,
            document_owner_user_id: Option<String>,
            is_current: bool,
        },
        /// The feed says this item is withdrawn → evict the local replica. Stable-document
        /// metadata distinguishes a predecessor transition from an authoritative resource delete.
        Evict {
            item_id: String,
            seq: u64,
            doc_id: Option<String>,
            is_current: bool,
        },
        /// A CONTAINER manifest → write it into `org_containers`, not `org_items`.
        ///
        /// Kept a distinct action rather than a flag on `Ingest` so the apply arm cannot
        /// accidentally run the note path's embed + chunk pipeline over a JSON manifest, which
        /// would put container names into the retrieval index as if they were note text.
        IngestContainer {
            item_id: String,
            seq: u64,
            rev: u32,
            generation: u32,
            manifest: Box<crate::share::container_envelope::ContainerEnvelope>,
            author_hint: String,
            author_user_id: String,
            access: crate::share::org_dto::OrgItemAccess,
            document_owner_user_id: Option<String>,
            created_at: String,
        },
        /// Divergent live record → (re)write the opened envelope into the replica.
        Ingest {
            item_id: String,
            seq: u64,
            rev: u32,
            generation: u32,
            env: Box<crate::share::org_envelope::OrgEnvelope>,
            attachments: Vec<crate::storage::IncomingAttachment>,
            sha: Vec<u8>,
            author_user_id: String,
            doc_id: Option<String>,
            access: crate::share::org_dto::OrgItemAccess,
            document_owner_user_id: Option<String>,
            is_current: bool,
        },
    }

    let cursor = state.db.org_reconcile_seq_for(&org.org_id)?;
    // At the START of a pass only: hold off if the previous full pass finished recently. A pass
    // already under way (cursor > 0) always runs to completion, so a repair in flight is never
    // abandoned half-done.
    if cursor == 0 && recently_reconciled(state.db.org_reconcile_pass_at(&org.org_id)?.as_deref()) {
        return Ok(0);
    }
    let feed = client
        .org_feed(access, &org.org_id, cursor, ORG_RECONCILE_PAGE)
        .await?;
    if !policy.is_current() {
        return Ok(0);
    }
    if feed.items.len() > ORG_RECONCILE_PAGE as usize {
        return Err(AppError::Unavailable(
            "org server exceeded the requested bounded reconcile page".into(),
        ));
    }
    validate_org_feed_page_metadata(&feed)?;
    if feed.items.is_empty() {
        let now = chrono::Utc::now().to_rfc3339();
        if policy
            .commit(|| state.db.complete_org_reconcile_pass(&org.org_id, &now))?
            .is_some()
        {
            tracing::debug!(target: "org", "org reconcile pass complete");
        }
        return Ok(0);
    }

    let mut actions: Vec<ReconcileAction> = Vec::new();
    let mut walked = cursor;
    for item in &feed.items {
        if !policy.is_current() {
            return Ok(0);
        }
        if item.seq <= walked {
            return Err(AppError::Unavailable(
                "org server returned a non-increasing feed sequence".into(),
            ));
        }
        walked = item.seq;

        if item.tombstoned {
            actions.push(ReconcileAction::Evict {
                item_id: item.item_id.clone(),
                seq: item.seq,
                doc_id: item.doc_id.clone(),
                is_current: resolved_org_item_is_current(item),
            });
            continue;
        }

        let held = state.db.org_replica_state(&item.item_id)?;
        if held.as_ref().is_some_and(|h| h.tombstoned) {
            // Permanent: an append-only tombstone is never undone by a later live record.
            actions.push(ReconcileAction::Skip { seq: item.seq });
            continue;
        }
        // CONVERGED: the stored plaintext hash equals the feed's ⇒ nothing to do, and — critically —
        // no blob is fetched. This is what keeps a full pass cheap on a large, healthy replica.
        if let (Some(held), Some(sha)) = (held.as_ref(), item.content_sha256.as_ref()) {
            if held.content_sha256.as_deref() == Some(sha.as_slice())
                && held.projection_sha256.as_deref() == Some(sha.as_slice())
            {
                actions.push(ReconcileAction::RepairMetadata {
                    item_id: item.item_id.clone(),
                    seq: item.seq,
                    generation: item.generation,
                    doc_id: item.doc_id.clone(),
                    access: item.access,
                    document_owner_user_id: item.document_owner_user_id.clone(),
                    is_current: resolved_org_item_is_current(item),
                });
                continue;
            }
        }

        let (Some(blob_id), Some(sha)) = (item.blob_id.clone(), item.content_sha256.clone()) else {
            // Structurally unopenable (no blob / no hash ⇒ no AAD nonce). Advance; never stall.
            actions.push(ReconcileAction::Skip { seq: item.seq });
            continue;
        };
        let item_nonce = org_item_nonce(&sha);
        let ock =
            match acquire_org_ock_with_policy(state, &org.org_id, item.generation, policy).await {
                Ok(k) => k,
                Err(_) => {
                    if !policy.is_current() {
                        return Ok(0);
                    }
                    actions.push(ReconcileAction::Skip { seq: item.seq });
                    continue;
                }
            };
        if !policy.is_current() {
            return Ok(0);
        }
        let ciphertext = match client.get_blob(access, &blob_id).await {
            Ok(c) => c,
            Err(_) => {
                if !policy.is_current() {
                    return Ok(0);
                }
                actions.push(ReconcileAction::Skip { seq: item.seq });
                continue;
            }
        };
        if !policy.is_current() {
            return Ok(0);
        }
        if ciphertext.len() > murmur_protocol::caps::MAX_ORG_ITEM_BLOB_BYTES {
            actions.push(ReconcileAction::Skip { seq: item.seq });
            continue;
        }
        let env = match crate::share::org_envelope::open_org_envelope(
            &ock,
            &ciphertext,
            &org.org_id,
            &item_nonce,
        ) {
            Ok(e) => e,
            Err(_) => {
                actions.push(ReconcileAction::Skip { seq: item.seq });
                continue;
            }
        };
        if env.content_sha256() != sha {
            actions.push(ReconcileAction::Skip { seq: item.seq });
            continue;
        }
        if env.kind == crate::share::org_envelope::OrgItemKind::Container {
            // A manifest is structure, not prose: it skips the attachment bundle, the chunker and
            // the embedder entirely. A payload this device cannot parse is SKIPPED whole — never
            // half-written — so a malformed or hostile manifest cannot leave a container row with
            // no name behind.
            match crate::share::container_envelope::ContainerEnvelope::from_json(&env.markdown) {
                Ok(manifest) => actions.push(ReconcileAction::IngestContainer {
                    item_id: item.item_id.clone(),
                    seq: item.seq,
                    rev: item.rev,
                    generation: item.generation,
                    manifest: Box::new(manifest),
                    author_hint: env.author_hint.clone(),
                    author_user_id: item.author_user_id.clone(),
                    access: item.access,
                    document_owner_user_id: item.document_owner_user_id.clone(),
                    created_at: item.created_at.clone(),
                }),
                Err(_) => actions.push(ReconcileAction::Skip { seq: item.seq }),
            }
            continue;
        }
        let (local_markdown, incoming_attachments) =
            match prepare_incoming_attachment_bundle(&env.markdown, &env.attachments) {
                Ok(bundle) => bundle,
                Err(_) => {
                    actions.push(ReconcileAction::Skip { seq: item.seq });
                    continue;
                }
            };
        let mut env = env;
        env.markdown = local_markdown;
        env.attachments.clear();
        actions.push(ReconcileAction::Ingest {
            item_id: item.item_id.clone(),
            seq: item.seq,
            rev: item.rev,
            generation: item.generation,
            env: Box::new(env),
            attachments: incoming_attachments,
            sha,
            author_user_id: item.author_user_id.clone(),
            doc_id: item.doc_id.clone(),
            access: item.access,
            document_owner_user_id: item.document_owner_user_id.clone(),
            is_current: resolved_org_item_is_current(item),
        });
    }

    // ── APPLY — on the blocking pool, through the ONE global heavy-inference gate (same discipline
    // as the live ingest: an embed is a Metal forward pass and must never run ungated inline).
    let db = state.db.clone();
    let org_id = org.org_id.clone();
    let background_epoch = policy.background_epoch;
    let app_for_apply = app;
    let (changed, progress) = crate::perf::run_heavy(
        &state.heavy_inference,
        move || -> Result<(u32, u64), AppError> {
            let embedder: Option<Box<dyn crate::embed::Embedder>> = match background_epoch {
                Some(epoch) => crate::embed::background_persistence_embedder(epoch).ok(),
                None => crate::embed::active_persistence_embedder_if_available(),
            };
            let embedder_ref: Option<&dyn crate::embed::Embedder> = embedder.as_deref();
            let mut changed = 0u32;
            let mut progress = cursor;
            for action in actions {
                if !policy.is_current() {
                    break;
                }
                match action {
                    ReconcileAction::Skip { seq } => progress = seq,
                    ReconcileAction::RepairMetadata {
                        item_id,
                        seq,
                        generation,
                        doc_id,
                        access,
                        document_owner_user_id,
                        is_current,
                    } => {
                        let Some(repaired) = policy.commit(|| {
                            let commit = || {
                                db.repair_org_reconcile_metadata(
                                    &item_id,
                                    &org_id,
                                    generation,
                                    doc_id.as_deref(),
                                    access.as_str(),
                                    document_owner_user_id.as_deref(),
                                    is_current,
                                )
                            };
                            if let Some(handle) = app_for_apply.as_ref() {
                                let app_state = handle.state::<AppState>();
                                commit_org_metadata_mutation(
                                    app_state.inner(),
                                    Some(handle),
                                    commit,
                                )
                            } else {
                                commit()
                            }
                        })?
                        else {
                            break;
                        };
                        if repaired.changed {
                            changed += 1;
                        }
                        progress = seq;
                    }
                    ReconcileAction::Evict {
                        item_id,
                        seq,
                        doc_id,
                        is_current,
                    } => {
                        let Some(evicted) = policy.commit(|| {
                            if let Some(handle) = app_for_apply.as_ref() {
                                let app_state = handle.state::<AppState>();
                                commit_org_visibility_reduction(
                                    app_state.inner(),
                                    Some(handle),
                                    || {
                                        db.evict_org_reconcile_tombstone_with_metadata(
                                            &org_id,
                                            &item_id,
                                            doc_id.as_deref(),
                                            is_current,
                                        )
                                    },
                                )
                            } else {
                                db.evict_org_reconcile_tombstone_with_metadata(
                                    &org_id,
                                    &item_id,
                                    doc_id.as_deref(),
                                    is_current,
                                )
                            }
                        })?
                        else {
                            break;
                        };
                        // The same feed entry withdraws a CONTAINER, and only this device knows
                        // which item ids were containers — the relay sees opaque documents.
                        let container_evicted = policy
                            .commit(|| db.tombstone_org_container_by_item(&item_id))?
                            .unwrap_or(false);
                        if evicted || container_evicted {
                            changed += 1;
                        }
                        progress = seq;
                    }
                    ReconcileAction::IngestContainer {
                        item_id,
                        seq,
                        rev,
                        generation,
                        manifest,
                        author_hint,
                        author_user_id,
                        access,
                        document_owner_user_id,
                        created_at,
                    } => {
                        let manifest = *manifest;
                        let row = crate::storage::models::OrgContainerRow {
                            org_id: org_id.clone(),
                            container_id: manifest.container_id.clone(),
                            item_id: item_id.clone(),
                            level: manifest.level.as_str().to_string(),
                            name: manifest.name.clone(),
                            emoji: manifest.emoji.clone(),
                            tint: manifest.tint.clone(),
                            parent_container_id: manifest.parent_container_id.clone(),
                            position: manifest.position,
                            access: access.as_str().to_string(),
                            author_hint,
                            author_user_id: (!author_user_id.is_empty()).then_some(author_user_id),
                            document_owner_user_id,
                            seq,
                            rev,
                            generation,
                            created_at,
                        };
                        let Some(()) = policy.commit(|| db.upsert_org_container(&row))? else {
                            break;
                        };
                        changed += 1;
                        progress = seq;
                    }
                    ReconcileAction::Ingest {
                        item_id,
                        seq,
                        rev,
                        generation,
                        env,
                        attachments,
                        sha,
                        author_user_id,
                        doc_id,
                        access,
                        document_owner_user_id,
                        is_current,
                    } => {
                        let env = *env;
                        let prepared = match crate::storage::Db::prepare_org_item_index_for_kind(
                            env.kind,
                            &env.title,
                            &env.created_at,
                            &env.markdown,
                            embedder_ref,
                        ) {
                            Ok(prepared) => prepared,
                            // The sweep is a background repair, never the user's critical path: a
                            // model that refuses right now degrades to an honest FTS-only write
                            // rather than stalling the walk. The normal embed backlog repair in the
                            // live pull fills the vectors in later.
                            Err(AppError::Unavailable(_)) => {
                                crate::storage::Db::prepare_org_item_index_for_kind(
                                    env.kind,
                                    &env.title,
                                    &env.created_at,
                                    &env.markdown,
                                    None,
                                )?
                            }
                            Err(e) => return Err(e),
                        };
                        let author_ref = if author_user_id.is_empty() {
                            None
                        } else {
                            Some(author_user_id.as_str())
                        };
                        let Some(applied) = policy.commit(|| {
                            let commit = || {
                                db.commit_org_reconcile_item_with_metadata_and_attachments(
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
                                    env.source_kind
                                        .map(crate::share::org_envelope::OrgSourceKind::as_str),
                                    author_ref,
                                    &prepared,
                                    doc_id.as_deref(),
                                    access.as_str(),
                                    document_owner_user_id.as_deref(),
                                    is_current,
                                    &attachments,
                                )
                            };
                            let outcome = if let Some(handle) = app_for_apply.as_ref() {
                                let app_state = handle.state::<AppState>();
                                commit_org_metadata_mutation(
                                    app_state.inner(),
                                    Some(handle),
                                    commit,
                                )?
                            } else {
                                commit()?
                            };
                            Ok(outcome.changed)
                        })?
                        else {
                            break;
                        };
                        if applied {
                            changed += 1;
                        }
                        // Record WHERE the sender filed this document. Written after the item row
                        // exists, and unconditionally: a document that LEAVES a shared folder
                        // arrives with no placement, and clearing it is what makes it move.
                        let placement = env.placement.as_ref();
                        let _ = db.set_org_item_placement(
                            &item_id,
                            placement.map(|p| p.parent_container_id.as_str()),
                            placement.map(|p| p.position).unwrap_or(0),
                        );
                        progress = seq;
                    }
                }
            }
            Ok((changed, progress))
        },
    )
    .await?;

    if progress > cursor
        && policy
            .commit(|| state.db.set_org_reconcile_seq(&org.org_id, progress))?
            .is_none()
    {
        return Ok(changed);
    }
    if changed > 0 {
        tracing::info!(target: "org", changed, "org reconcile sweep converged local replica");
    }
    Ok(changed)
}

/// Re-derive `author_user_id` for the oldest bounded batch of locally-held LIVE items still missing
/// it. The one metadata-only page starts immediately before the oldest missing item's stored seq and
/// consumes this invocation's feed-page budget instead of running beside the main pull. It never
/// touches the real sync cursor. Best-effort: errors are swallowed into `report.errors` because a
/// missing author id degrades edit-in-place but never blocks reading the item.
async fn backfill_null_org_item_authors(
    state: &AppState,
    client: &crate::share::client::ShareClient,
    access: &str,
    org_id: &str,
    report: &mut crate::storage::models::OrgSyncReport,
    policy: OrgWorkPolicy,
) {
    if !policy.is_current() {
        return;
    }
    let missing = match state
        .db
        .org_items_with_null_author_seq(org_id, ORG_FEED_PAGE as i64)
    {
        Ok(rows) => rows,
        Err(e) => {
            report.errors.push(format!(
                "author backfill: local lookup failed ({})",
                brief_err(&e)
            ));
            return;
        }
    };
    if missing.is_empty() {
        return; // the common case — nothing to do, no extra network round-trip.
    }
    let cursor = missing
        .first()
        .map(|(_, seq)| seq.saturating_sub(1))
        .unwrap_or(0);
    let mut remaining: std::collections::HashSet<String> =
        missing.into_iter().map(|(item_id, _)| item_id).collect();
    let feed = match client.org_feed(access, org_id, cursor, ORG_FEED_PAGE).await {
        Ok(feed) => feed,
        Err(e) => {
            if policy.is_current() {
                report.errors.push(format!(
                    "author backfill: feed re-pull failed ({})",
                    brief_err(&e)
                ));
            }
            return;
        }
    };
    if !policy.is_current() {
        return;
    }
    if feed.items.len() > ORG_FEED_PAGE as usize {
        report
            .errors
            .push("author backfill: server exceeded the requested bounded feed page".into());
        return;
    }
    if let Err(error) = validate_org_feed_page_metadata(&feed) {
        report.errors.push(format!(
            "author backfill: malformed document metadata ({})",
            brief_err(&error)
        ));
        return;
    }
    for item in &feed.items {
        if !policy.is_current() {
            return;
        }
        if item.author_user_id.is_empty() || !remaining.remove(&item.item_id) {
            continue;
        }
        match policy.commit(|| {
            state
                .db
                .set_org_item_author(&item.item_id, &item.author_user_id)
        }) {
            Ok(Some(())) => report.authors_backfilled += 1,
            Ok(None) => return,
            Err(e) => report
                .errors
                .push(format!("author backfill: stamp failed ({})", brief_err(&e))),
        }
    }
}

/// A short, PII-free rendering of an error for a sync report string (never note content — AppError
/// Display here carries only stage/status labels the client controls).
pub(crate) fn brief_err(e: &AppError) -> String {
    match e {
        AppError::Locked(_) => "locked".to_string(),
        AppError::Auth(_) => "auth".to_string(),
        // Keep the `errcode::tag` code when there is one. A failure that repeats every 60 s
        // ("container share reconcile tick failed error=unavailable") is unactionable without it —
        // that exact line cost a full field investigation before anyone could say WHICH failure it
        // was. The tagged prefix is safe to log by construction: `share/client.rs` maps HTTP
        // failures to a fixed label plus the numeric status and never surfaces the reqwest
        // `Display` (which can echo the URL). An UNTAGGED message has no such guarantee, so it
        // stays collapsed — no rule promises an arbitrary `Unavailable` string is PII-free.
        AppError::Unavailable(msg) => match tagged_code(msg) {
            Some(code) => format!("unavailable[{code}]"),
            None => "unavailable".to_string(),
        },
        _ => "error".to_string(),
    }
}

/// Record a best-effort container-reconcile failure ON the sync report, so a manual "Sync now"
/// cannot answer a clean report while shared-folder publishing is failing.
///
/// This was the second half of the 2026-09-01 field report: the tick failed every 60 s, the manual
/// sync swallowed the same failure into a `tracing::warn!`, and the panel — which renders an empty
/// report as **"Synced — up to date."** — told the user everything was fine. Content-free by
/// construction: [`brief_err`] emits a stage label and, at most, a client-chosen `[code]`.
pub(crate) fn note_container_failure(
    report: &mut crate::storage::models::OrgSyncReport,
    e: &AppError,
) {
    report.errors.push(format!("shared folders: {}", brief_err(e)));
}

/// The `[code]` an [`crate::errcode::tag`] message starts with, if any.
fn tagged_code(msg: &str) -> Option<&str> {
    let rest = msg.strip_prefix('[')?;
    let (code, _) = rest.split_once(']')?;
    // A tag is a short kebab-case label the client itself chose; anything else is not a tag.
    let plausible = !code.is_empty()
        && code.len() <= 40
        && code
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    plausible.then_some(code)
}

/// `org_get_item(item_id)` — the full decrypted org item for the read-only FE viewer. Org items are
/// deliberately org-disclosed content (no folder lock gate applies). Returns `None` for an unknown or
/// tombstoned item.
#[tauri::command]
pub fn org_get_item(
    state: State<'_, AppState>,
    item_id: String,
) -> Result<Option<crate::storage::models::OrgItemDetail>, AppError> {
    org_get_item_inner(state.inner(), &item_id)
}

pub(crate) fn org_get_item_inner(
    st: &AppState,
    item_or_link_id: &str,
) -> Result<Option<crate::storage::models::OrgItemDetail>, AppError> {
    let _lifecycle = lifecycle_guard(st);
    // Conflict recovery may address the resource by its revision-stable `orgId:docId` link id after
    // a feed sync. Resolve ONLY through the existing membership/context/current-head gate; an invalid,
    // withdrawn, non-current, left or disabled composite is indistinguishable from an unknown item.
    // Exact legacy/current item ids retain their historical behavior.
    let item_id = if item_or_link_id.contains(':') {
        let Some((current_item_id, _title)) = st.db.org_link_target_visible(item_or_link_id)?
        else {
            return Ok(None);
        };
        current_item_id
    } else {
        item_or_link_id.to_string()
    };
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
        let (can_edit, can_manage) = org_item_permissions(st, &item_id)?;
        detail.can_manage = can_manage;
        detail.can_edit = can_edit;
        detail.editable = can_edit;
        if let Some(doc_id) = ctx.doc_id.as_deref() {
            if crate::share::org_dto::parse_stable_uuid(&ctx.org_id).is_some()
                && crate::share::org_dto::parse_stable_uuid(doc_id).is_some()
            {
                detail.link_id = Some(format!("{}:{doc_id}", ctx.org_id));
            }
        }
    }
    Ok(Some(detail))
}

/// Copy a RECEIVED Shared Brain snapshot into an OPEN user Space/folder while retaining the org
/// replica. Documents become authored notes; meetings become local meeting shells with the shared
/// snapshot as their provider note. No recording/audio is copied or invented.
#[tauri::command]
pub async fn add_org_item_to_container(
    state: State<'_, AppState>,
    item_id: String,
    container_id: String,
) -> Result<crate::storage::models::OrgItemImportResult, AppError> {
    let _org_mutation = state.lock_org_mutation().await;
    add_org_item_to_container_inner(state.inner(), &item_id, &container_id)
}

/// Synchronous production core for [`add_org_item_to_container`]. Keeping every
/// membership/current/context/destination check in this unit gives the command
/// one auditable, deterministic gate chain; the async wrapper owns only mutation
/// serialization.
pub(crate) fn add_org_item_to_container_inner(
    state: &AppState,
    item_id: &str,
    container_id: &str,
) -> Result<crate::storage::models::OrgItemImportResult, AppError> {
    let lifecycle = lifecycle_guard(state);
    let ctx = state
        .db
        .org_item_edit_ctx(item_id)?
        .ok_or_else(|| AppError::InvalidArg("no current Shared Brain item".into()))?;
    resolve_org(state, &ctx.org_id)?;
    ensure_meeting_folder_target(&state.db, Some(container_id))?;
    let target = state
        .db
        .folder_by_id(container_id)?
        .ok_or_else(|| AppError::InvalidArg("no such destination".into()))?;
    if target.locked {
        return Err(AppError::Locked(
            "unlock the destination before adding shared content".into(),
        ));
    }
    let imported = state
        .db
        .import_received_org_item_atomic(item_id, container_id)?;

    drop(lifecycle);
    finalize_imported_org_item_best_effort(state, &imported);
    Ok(imported)
}

/// Publish the ordinary local derived projections after the atomic received-item import. This is
/// deliberately best-effort after the canonical commit: a vault/index failure must not turn a
/// successful copy into a false IPC failure. Every plaintext read is re-gated in a fresh lifecycle
/// interval, so a lock racing the post-commit work only defers projections and never leaks content.
pub(crate) fn finalize_imported_org_item_best_effort(
    state: &AppState,
    imported: &crate::storage::models::OrgItemImportResult,
) {
    if imported.kind != "note" {
        // Meetings and their provider notes are inserted in one transaction; the existing
        // meetings/notes FTS triggers make the shell lexically searchable immediately, and the
        // ordinary gated `export_note` command can export its shared provider note. No audio row or
        // path is synthesized.
        return;
    }
    let note_projection = (|| -> Result<Option<(String, String)>, AppError> {
        let _lifecycle = lifecycle_guard(state);
        let Some((folder_id, _, _)) = state.db.note_gate_anchor(&imported.id)? else {
            return Ok(None);
        };
        if !folder_is_unlocked(state, &folder_id)? {
            return Ok(None);
        }
        let Some(row) = state.db.get_note_row(&imported.id)? else {
            return Ok(None);
        };
        if let Err(error) = export_note_to_vault_under_lifecycle_authorized(state, &row.id) {
            tracing::warn!(target: "org", error = %error, "received note vault projection deferred");
        }
        Ok(Some((note_display_title(&row), row.text)))
    })();
    let Some((title, markdown)) = note_projection.unwrap_or_else(|error| {
        tracing::warn!(target: "org", error = %error, "received note projection deferred");
        None
    }) else {
        return;
    };
    // Chunk-only indexing keeps keyword retrieval available without loading an embedding model;
    // semantic vectors are repaired by the normal model-aware reindex path when available.
    refresh_note_doc_derived_best_effort(state, &imported.id, &title, &markdown, None);
}

pub(crate) fn org_item_permissions(st: &AppState, item_id: &str) -> Result<(bool, bool), AppError> {
    let Some(ctx) = st.db.org_item_edit_ctx(item_id)? else {
        return Ok((false, false));
    };
    let Ok(me) = session_server_user_id(st) else {
        return Ok((false, false));
    };
    let org_owner = resolve_org(st, &ctx.org_id)
        .map(|org| org.role == "owner")
        .unwrap_or(false);
    let document_owner = ctx.document_owner_user_id.as_deref() == Some(me.as_str());
    let legacy_author =
        ctx.document_owner_user_id.is_none() && ctx.author_user_id.as_deref() == Some(me.as_str());
    let can_manage = document_owner || org_owner;
    Ok((
        can_manage || legacy_author || ctx.access == "edit",
        can_manage,
    ))
}

#[tauri::command]
pub async fn org_set_item_access(
    app: AppHandle,
    state: State<'_, AppState>,
    item_id: String,
    access: crate::share::org_dto::OrgItemAccess,
) -> Result<(), AppError> {
    org_set_item_access_inner(state.inner(), &item_id, access).await?;
    crate::events::emit_org_feed_updated(&app, 0);
    Ok(())
}

pub(crate) async fn org_set_item_access_inner(
    st: &AppState,
    item_id: &str,
    access: crate::share::org_dto::OrgItemAccess,
) -> Result<(), AppError> {
    let _mutation = st.lock_org_mutation().await;
    let ctx = match st.db.org_item_edit_ctx(item_id)? {
        Some(ctx) => ctx,
        None => {
            // The origin share may still name its last-published (now tombstoned) revision after a
            // collaborator advanced the stable head. Resolve management through `(org, doc)` while
            // leaving the persisted share CAS baseline untouched.
            let row = st.db.org_share_by_item(item_id)?.ok_or_else(|| {
                AppError::InvalidArg("no such org item (or it was removed)".into())
            })?;
            let doc_id = row.doc_id.as_deref().ok_or_else(|| {
                AppError::InvalidArg("this legacy shared item has no permission resource".into())
            })?;
            st.db
                .org_item_edit_ctx_by_document(&row.org_id, doc_id)?
                .ok_or_else(|| AppError::InvalidArg("shared document is no longer live".into()))?
        }
    };
    let (_access_token, me) = authenticated_org_actor(st).await?;
    let org = resolve_org(st, &ctx.org_id)?;
    let can_manage =
        ctx.document_owner_user_id.as_deref() == Some(me.as_str()) || org.role == "owner";
    if !can_manage {
        return Err(AppError::Auth(
            "only the document owner or organization owner can change access".into(),
        ));
    }
    let expected_owner_user_id = ctx
        .document_owner_user_id
        .clone()
        .ok_or_else(|| AppError::InvalidArg("stable shared document omitted its owner".into()))?;
    let doc_id = ctx.doc_id.ok_or_else(|| {
        AppError::InvalidArg("this legacy shared item has no permission resource".into())
    })?;
    if st
        .db
        .org_document_has_blocked_republish(&org.org_id, &doc_id)?
    {
        return Err(AppError::Unavailable(
            "shared document has an unresolved edit attempt".into(),
        ));
    }
    // PATCH changes what future collaborators may do with shared content and therefore remains part
    // of the same explicit org-egress boundary as publish/update. Fail closed after local authority
    // checks but before any network request.
    let consented = st
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?
        .org_egress_consented;
    if !consented {
        return Err(AppError::Unavailable(crate::errcode::tag(
            crate::errcode::ORG_CONSENT,
            "confirm the one-time upload notice first",
        )));
    }
    let base = share_base_url(st)?;
    let (access_token, actor_user_id) = authenticated_org_actor(st).await?;
    if actor_user_id != me {
        return Err(AppError::Unavailable(
            "account changed before access dispatch".into(),
        ));
    }
    let client = crate::share::client::ShareClient::new(&base)?;
    let old_access = crate::share::org_dto::OrgItemAccess::parse(&ctx.access).ok_or_else(|| {
        AppError::InvalidArg("shared document has invalid access metadata".into())
    })?;
    let (dispatch_permit, dispatch_id) = permit_org_access(
        st,
        &client.host(),
        &org.org_id,
        &doc_id,
        access,
        old_access,
        &actor_user_id,
        &expected_owner_user_id,
    )?;
    let response = client
        .org_set_item_access(
            &access_token,
            &org.org_id,
            &doc_id,
            crate::share::org_dto::SetOrgItemAccessRequest { access },
            dispatch_permit,
        )
        .await;
    let response = match response {
        Ok(response) => response,
        Err(error @ AppError::InvalidArg(_)) => {
            fail_org_access_attempt(
                st,
                &dispatch_id,
                &org.org_id,
                &doc_id,
                old_access,
                access,
                &actor_user_id,
                &expected_owner_user_id,
            )?;
            return Err(error);
        }
        Err(error) => {
            let attempt = pending_org_access_attempts(st)?
                .into_iter()
                .find(|attempt| attempt.dispatch_id == dispatch_id)
                .ok_or_else(|| AppError::Storage("org access attempt disappeared".into()))?;
            if reconcile_pending_org_access_attempt(st, &attempt)
                .await
                .unwrap_or(false)
            {
                return Ok(());
            }
            return Err(error);
        }
    };
    if response.doc_id != doc_id
        || response.access != access
        || response.document_owner_user_id != expected_owner_user_id
    {
        let attempt = pending_org_access_attempts(st)?
            .into_iter()
            .find(|attempt| attempt.dispatch_id == dispatch_id)
            .ok_or_else(|| AppError::Storage("org access attempt disappeared".into()))?;
        let _ = reconcile_pending_org_access_attempt(st, &attempt).await;
        return Err(AppError::Unavailable(
            "org-set-item-access: server returned inconsistent document metadata".into(),
        ));
    }
    if !apply_org_access_attempt(
        st,
        &dispatch_id,
        &org.org_id,
        &response.doc_id,
        old_access,
        response.access,
        &actor_user_id,
        &response.document_owner_user_id,
    )? {
        return Err(AppError::Unavailable(
            "shared document changed while applying access".into(),
        ));
    }
    Ok(())
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
pub async fn org_update_item(
    app: AppHandle,
    state: State<'_, AppState>,
    item_id: String,
    title: String,
    markdown: String,
) -> Result<String, AppError> {
    let new_item_id =
        org_update_own_item_notifying(state.inner(), &item_id, &title, &markdown, Some(&app))
            .await?;
    // Ping every open org view (Notes list + Settings shared-brain) to re-fetch — the edit superseded
    // the item (new id) + tombstoned the old. Content-free; best-effort (never affects the result).
    crate::events::emit_org_feed_updated(&app, 1);
    Ok(new_item_id)
}

#[cfg(test)]
pub(crate) async fn org_update_own_item_inner(
    state: &AppState,
    item_id: &str,
    title: &str,
    markdown: &str,
) -> Result<String, AppError> {
    org_update_own_item_notifying(state, item_id, title, markdown, None).await
}

pub(crate) async fn org_update_own_item_notifying(
    state: &AppState,
    item_id: &str,
    title: &str,
    markdown: &str,
    app: Option<&AppHandle>,
) -> Result<String, AppError> {
    let _mutation = state.lock_org_mutation().await;
    // Resolve the item's edit context (org, current rev, original created_at/source_kind, author id).
    let ctx = state
        .db
        .org_item_edit_ctx(item_id)?
        .ok_or_else(|| AppError::InvalidArg("no such org item (or it was removed)".into()))?;

    // Local preflight mirrors the relay's authoritative per-request check. A manager may always edit;
    // another active member only when the document is `edit`. Missing stable metadata fails closed.
    let me = session_server_user_id(state)?;
    let org = resolve_org(state, &ctx.org_id)?;
    let legacy_author =
        ctx.document_owner_user_id.is_none() && ctx.author_user_id.as_deref() == Some(me.as_str());
    let can_manage =
        ctx.document_owner_user_id.as_deref() == Some(me.as_str()) || org.role == "owner";
    if !can_manage && !legacy_author && ctx.access != "edit" {
        return Err(AppError::Auth("this shared document is view only".into()));
    }
    let doc_id = ctx.doc_id.clone();

    // (2) CONSENT fail-closed (the same global one-time org-egress consent the share path checks).
    {
        let cfg = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
        if !cfg.org_egress_consented {
            return Err(AppError::Unavailable(crate::errcode::tag(
                crate::errcode::ORG_CONSENT,
                "confirm the one-time upload notice first",
            )));
        }
    }

    // Task free text is scrubbed field-by-field so stable ids and JSON structure remain intact.
    // Notes retain the leak-safe clean+scrub transform.
    let is_task = ctx.source_kind.as_deref() == Some("task");
    let (wire_title, final_md, _counts) = if is_task {
        let task = crate::share::task_envelope::TaskEnvelope::from_json(markdown, &ctx.org_id)?;
        if task.title.trim() != title.trim() {
            return Err(AppError::InvalidArg(
                "task title does not match its structured payload".into(),
            ));
        }
        super::tasks::validate_task_org_refs(state, &task)?;
        let (task, canonical, counts) = scrub_task_envelope_json(markdown, &ctx.org_id)?;
        (task.title, canonical, counts)
    } else {
        let cleaned = crate::share::envelope::clean_note_body(markdown);
        let (markdown, counts) = scrub_org_markdown(&cleaned);
        (title.to_string(), markdown, counts)
    };
    let (final_md, attachments) = attachment_bundle_for_markdown(
        state,
        &crate::storage::AttachmentOwner::OrgItem {
            item_id: item_id.to_string(),
        },
        &final_md,
    )?;
    if is_task {
        crate::share::task_envelope::TaskEnvelope::from_json(&final_md, &ctx.org_id)?;
    }

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
    let generation = org.generation;
    let base = share_base_url(state)?;
    let (_account_id, _gen_id, _mk, access_token) = require_session_mk(state).await?;
    let editor_hint = {
        let session = state
            .account_session
            .lock()
            .map_err(|_| AppError::Storage("account-session mutex poisoned".into()))?;
        org_author_hint(&crate::share::require_login(&session)?.email)
    };
    let client = crate::share::client::ShareClient::new(&base)?;

    let source_kind = match ctx.source_kind.as_deref() {
        Some("meeting") => crate::share::org_envelope::OrgSourceKind::Meeting,
        Some("task") => crate::share::org_envelope::OrgSourceKind::Task,
        _ => crate::share::org_envelope::OrgSourceKind::Document,
    };

    let new_rev = ctx.rev.checked_add(1).ok_or_else(|| {
        AppError::InvalidArg("this shared document exhausted its revision range".into())
    })?;
    let env = crate::share::org_envelope::OrgEnvelope::new(
        if is_task {
            crate::share::org_envelope::OrgItemKind::Task
        } else {
            crate::share::org_envelope::OrgItemKind::Note
        },
        wire_title,
        final_md,
        editor_hint,
        ctx.created_at.clone(),
        new_rev,
        source_kind,
    )
    .with_attachments(attachments)
    // PRESERVE the placement this document already carries. An editor is changing the TEXT, not
    // where the document lives — dropping the placement here would silently evict the note from
    // its shared folder for every member the moment somebody else edited it.
    .with_placement(state.db.org_item_placement(item_id)?);
    let content_sha = env.content_sha256();

    // (5) SEAL under the OCK + LOCAL OPEN-VERIFY (verify-before-egress). AAD nonce = hex(content_sha256),
    // deterministic + rides the feed so every member reconstructs it.
    let ock = acquire_org_ock(state, &org.org_id, generation).await?;
    let item_nonce = org_item_nonce(&content_sha);
    let (ciphertext, _sha) =
        crate::share::org_envelope::seal_org_envelope(&ock, &env, &org.org_id, &item_nonce)?;
    if ciphertext.len() > murmur_protocol::caps::MAX_ORG_ITEM_BLOB_BYTES {
        return Err(AppError::InvalidArg(format!(
            "this item is too large to share ({} bytes sealed; the org limit is {} bytes)",
            ciphertext.len(),
            murmur_protocol::caps::MAX_ORG_ITEM_BLOB_BYTES
        )));
    }

    // (6) atomic inline publish of the NEXT rev. A failure leaves the OLD item live and cannot leave
    // an orphan staging blob.
    let now = chrono::Utc::now().to_rfc3339();
    let mut stable_projection_witness = None;
    let published = if let Some(doc_id) = doc_id.as_deref() {
        let requested_access = crate::share::org_dto::OrgItemAccess::parse(&ctx.access)
            .ok_or_else(|| AppError::Storage("stored org document access is invalid".into()))?;
        let expected_owner = ctx.document_owner_user_id.as_deref().ok_or_else(|| {
            AppError::Storage("stable org document omitted its durable owner".into())
        })?;
        let existing_anchor = state.db.org_share_by_item(item_id)?;
        let (attempt_row_id, dispatch_permit, dispatch_id) = persist_direct_org_update_intent(
            state,
            existing_anchor.as_ref(),
            &org.org_id,
            doc_id,
            item_id,
            ctx.rev,
            new_rev,
            generation,
            &content_sha,
            requested_access,
            &me,
            expected_owner,
            &env.title,
            &now,
            &client.host(),
            ciphertext.len(),
            org_dispatch_cell_sha256(&ciphertext),
        )?;
        let publish_result = client
            .org_update_item(
                &access_token,
                &org.org_id,
                doc_id,
                crate::share::org_dto::UpdateOrgItemRequest {
                    mutation_id: None,
                    expected_rev: ctx.rev,
                    content_cell: ciphertext,
                    content_sha256: content_sha.clone(),
                    generation,
                },
                dispatch_permit,
            )
            .await;
        let published = match publish_result {
            Ok(published)
                if published.doc_id.as_deref() == Some(doc_id)
                    && published.access == requested_access
                    && published.document_owner_user_id.as_deref() == Some(expected_owner) =>
            {
                published
            }
            Err(error) if is_org_edit_conflict(&error) => {
                conflict_direct_org_update_intent(
                    state,
                    &attempt_row_id,
                    doc_id,
                    requested_access,
                    &me,
                    expected_owner,
                    item_id,
                    new_rev,
                    generation,
                    &content_sha,
                    &dispatch_id,
                    &now,
                )?;
                return Err(error);
            }
            Ok(_) | Err(AppError::Unavailable(_)) => {
                let attempt = state.db.get_org_share(&attempt_row_id)?.ok_or_else(|| {
                    AppError::Storage("direct org update attempt disappeared".into())
                })?;
                match reconcile_direct_org_update_attempt(
                    state,
                    &client,
                    &access_token,
                    &attempt,
                    &me,
                )
                .await
                {
                    Ok(DirectOrgUpdateResolution::Exact(published)) => published,
                    Ok(DirectOrgUpdateResolution::Conflict) => {
                        conflict_direct_org_update_intent(
                            state,
                            &attempt_row_id,
                            doc_id,
                            requested_access,
                            &me,
                            expected_owner,
                            item_id,
                            new_rev,
                            generation,
                            &content_sha,
                            &dispatch_id,
                            &now,
                        )?;
                        return Err(AppError::InvalidArg(crate::errcode::tag(
                            crate::errcode::ORG_EDIT_CONFLICT,
                            "the shared document head did not exactly match this saved draft",
                        )));
                    }
                    Ok(DirectOrgUpdateResolution::Inconclusive) => {
                        return Err(AppError::Unavailable(
                            "direct org update outcome is pending authenticated reconciliation"
                                .into(),
                        ));
                    }
                    Err(error) => return Err(error),
                }
            }
            Err(error) => {
                return Err(error);
            }
        };
        let pending = state.db.get_org_share(&attempt_row_id)?.ok_or_else(|| {
            AppError::Storage("direct org update attempt disappeared before projection".into())
        })?;
        confirm_org_mutation_for_projection(
            state,
            &pending,
            &published,
            ORG_SHARE_DIRECT_PUT_PENDING,
            &dispatch_id,
            &now,
        )?;
        stable_projection_witness = Some((attempt_row_id, dispatch_id, existing_anchor.is_none()));
        published
    } else {
        let request = crate::share::org_dto::PublishItemRequest {
            mutation_id: None,
            doc_id: None,
            access: None,
            blob_id: None,
            content_cell: Some(ciphertext),
            content_sha256: content_sha.clone(),
            rev: new_rev,
            generation,
        };
        let permit = permit_org_publish(state, &client.host(), &org.org_id, &request, None)?;
        let published = client
            .org_publish_item(&access_token, &org.org_id, request, permit)
            .await?;
        published
    };

    // LOCAL REPLICA CONSISTENCY: upsert the NEW item (so the Notes list resolves it immediately) +
    // stamp our authorship on it directly (root-cause fix, 2026-07-15 — `me` was already proven to be
    // this item's author by the ownership gate above, so pass it straight into the upsert rather than
    // a separate follow-up `set_org_item_author` call; the row is correct the instant it's written) +
    // tombstone the OLD replica. The FTS-only material is prepared first, then the atomic local
    // replica helper preserves any identical/newer feed-ingested real vectors and evicts the old item
    // in the same transaction. Best-effort — a local-replica error must never fail the save (the server
    // copy is already live).
    let local_replica = prepare_incoming_attachment_bundle(&env.markdown, &env.attachments)
        .and_then(|(local_markdown, local_attachments)| {
            crate::storage::Db::prepare_org_item_index_for_kind(
                env.kind,
                &env.title,
                &env.created_at,
                &local_markdown,
                None,
            )
            .and_then(|prepared| {
                commit_org_metadata_mutation(
                    state,
                    app.map(|app| app as &dyn AskHistoryInvalidationNotifier),
                    || {
                        if let Some((row_id, dispatch_id, discard_source_less_anchor)) =
                            stable_projection_witness.as_ref()
                        {
                            state.db.commit_org_republish_projection_if_current(
                                row_id,
                                dispatch_id,
                                &org.org_id,
                                published.doc_id.as_deref().ok_or_else(|| {
                                    AppError::Storage(
                                        "direct org update lost its document id".into(),
                                    )
                                })?,
                                published.access.as_str(),
                                new_rev,
                                generation,
                                &content_sha,
                                &me,
                                published.document_owner_user_id.as_deref().ok_or_else(|| {
                                    AppError::Storage("direct org update lost its owner".into())
                                })?,
                                Some(&published.item_id),
                                Some(ORG_SHARE_PROJECTION_PENDING),
                                *discard_source_less_anchor,
                                &crate::storage::org_store::OrgRepublishProjection {
                                    item_id: &published.item_id,
                                    seq: published.seq,
                                    author_hint: &env.author_hint,
                                    title: &env.title,
                                    markdown: &local_markdown,
                                    created_at: &env.created_at,
                                    source_kind: env
                                        .source_kind
                                        .map(crate::share::org_envelope::OrgSourceKind::as_str),
                                    author_user_id: Some(me.as_str()),
                                    prepared: &prepared,
                                    attachments: &local_attachments,
                                },
                            )
                        } else {
                            state.db.commit_local_org_replica_with_metadata(
                                &published.item_id,
                                &org.org_id,
                                published.seq,
                                &env.author_hint,
                                &env.title,
                                &local_markdown,
                                &env.created_at,
                                new_rev,
                                generation,
                                &content_sha,
                                env.source_kind
                                    .map(crate::share::org_envelope::OrgSourceKind::as_str),
                                Some(me.as_str()),
                                &prepared,
                                Some(item_id),
                                published.doc_id.as_deref(),
                                published.access.as_str(),
                                published.document_owner_user_id.as_deref(),
                                true,
                            )
                        }
                    },
                )?;
                if stable_projection_witness.is_none() {
                    state.db.replace_org_item_attachment_bundle(
                        &published.item_id,
                        &local_attachments,
                    )?;
                }
                Ok(())
            })
        });
    if let Err(e) = local_replica {
        tracing::warn!(target: "org", error = %e, "org edit: local replica refresh failed (server copy live)");
    }
    // Repoint any local `org_shares` anchor for the OLD id (usually none on a non-origin machine, but if
    // this IS the origin machine keep the anchor pointing at the live item so the vault-note republish
    // path stays consistent).
    if doc_id.is_none() {
        if let Ok(Some(row)) = state.db.org_share_by_item(item_id) {
            let _ = state.db.reset_org_share_for_retry(
                &row.id,
                row.title.as_deref(),
                new_rev,
                generation,
                &content_sha,
                row.scrub,
                &now,
            );
            let _ = state
                .db
                .set_org_share_uploaded(&row.id, &published.item_id, &now);
        }
    }
    if doc_id.is_none() && published.item_id != item_id {
        let permit = permit_simple_org_dispatch(
            state,
            &client.host(),
            "org_share_revoke",
            OrgDispatchOperation::Tombstone {
                org_id: org.org_id.clone(),
                item_id: item_id.to_string(),
            },
        )?;
        delete_legacy_org_item(state, &client, &access_token, &org.org_id, item_id, permit).await?;
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
    delete_org_item_as_author_notifying(state.inner(), &item_id, Some(&app)).await?;
    crate::events::emit_org_feed_updated(&app, 1);
    Ok(())
}

/// Inner of [`delete_org_item_as_author`] taking `&AppState` (unit-testable ownership gate + the
/// fail-closed server-then-local tombstone ordering).
#[cfg(test)]
pub(crate) async fn delete_org_item_as_author_inner(
    state: &AppState,
    item_id: &str,
) -> Result<(), AppError> {
    delete_org_item_as_author_notifying(state, item_id, None).await
}

pub(crate) async fn delete_org_item_as_author_notifying(
    state: &AppState,
    item_id: &str,
    app: Option<&AppHandle>,
) -> Result<(), AppError> {
    let _mutation = state.lock_org_mutation().await;
    let item_id = item_id.trim();
    if item_id.is_empty() {
        return Err(AppError::InvalidArg("item id required".into()));
    }

    // (1) MANAGEMENT GATE — stable document owner or org owner. Legacy rows without stable owner
    // retain their historical author-only behavior; an editor revision actor never gains withdraw.
    let ctx = state.db.org_item_edit_ctx(item_id)?.ok_or_else(|| {
        AppError::InvalidArg("no such org item (or it was already removed)".into())
    })?;
    let me = session_server_user_id(state)?;
    let org = resolve_org(state, &ctx.org_id)?;
    let legacy_author =
        ctx.document_owner_user_id.is_none() && ctx.author_user_id.as_deref() == Some(me.as_str());
    let can_manage = ctx.document_owner_user_id.as_deref() == Some(me.as_str())
        || org.role == "owner"
        || legacy_author;
    if !can_manage {
        return Err(AppError::Auth(
            "only the document owner or organization owner can remove this shared document".into(),
        ));
    }

    // Covers both the primary DELETE and `org_delete_document`'s authenticated feed corroboration
    // after an ambiguous 404. No consent means no request, local mutation, or success ledger.
    require_org_egress_consent(state)?;

    // (2) SERVER TOMBSTONE FIRST — fail loud, no local mutation yet. `org_tombstone_item` is
    // idempotent (a 404 — already gone — is treated as success), so a repeat call after a prior
    // partial failure (server succeeded, local step below didn't run yet) is safe to retry.
    let base = share_base_url(state)?;
    let access_token = valid_access_token(state).await?;
    let client = crate::share::client::ShareClient::new(&base)?;
    if let Some(doc_id) = ctx.doc_id.as_deref() {
        let permit = permit_simple_org_dispatch(
            state,
            &client.host(),
            "org_share_revoke",
            OrgDispatchOperation::DeleteDocument {
                org_id: org.org_id.clone(),
                doc_id: doc_id.to_string(),
            },
        )?;
        delete_stable_org_document(state, &client, &access_token, &org.org_id, doc_id, permit)
            .await?;
    } else {
        let permit = permit_simple_org_dispatch(
            state,
            &client.host(),
            "org_share_revoke",
            OrgDispatchOperation::Tombstone {
                org_id: org.org_id.clone(),
                item_id: item_id.to_string(),
            },
        )?;
        delete_legacy_org_item(state, &client, &access_token, &org.org_id, item_id, permit).await?;
    }

    // (3) ONLY NOW tombstone the local replica — the server confirmed gone, so dropping this
    // device's own copy can never leave a dangling "removed here but still live on the server"
    // state. Reuses the SAME local tombstone primitive the feed's own `FeedAction::Tombstone` arm
    // uses (`Db::tombstone_org_item`) — not a bespoke delete.
    {
        let _lifecycle = lifecycle_guard(state);
        let evicted = match ctx.doc_id.as_deref() {
            Some(doc_id) => state.db.terminalize_and_evict_org_document(
                &org.org_id,
                doc_id,
                &chrono::Utc::now().to_rfc3339(),
            )?,
            None => state.db.evict_org_item(item_id)?,
        };
        if evicted {
            bump_seal_epoch(state);
            if let Some(app) = app {
                emit_ask_history_invalidated_fail_closed(app);
            }
        }
    }

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
    let _lifecycle = lifecycle_guard(st);
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
        let Some((folder_id, _created_at, _updated_at)) = st.db.note_gate_anchor(&document_id)?
        else {
            return Ok(None);
        };
        // GATE: a sealed-not-unlocked note's title must not leak into the org list.
        if !folder_is_unlocked(st, &folder_id)? {
            return Ok(None);
        }
        let Some(note) = st.db.get_note_row(&document_id)? else {
            return Ok(None);
        };
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
