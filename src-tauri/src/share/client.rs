//! HTTP client for the murmur-server sharing API (spec §13). Thin, typed reqwest wrappers around the
//! auth + share endpoints; every request/response body is a `murmur_protocol::dto` type so wire drift
//! is a compile error. No content-derived string ever reaches a field or a log here (§7 inv. 8): the
//! bodies carry only ciphertext, wrapped keys, salts, and the `gate_secret` (a one-way branch of `L`).
//! The link key `L` is NEVER sent — the caller keeps it and assembles the fragment locally.
//!
//! Base-URL discipline (§7 inv. 9): the server URL is validated by
//! [`summarize::gateway::validate_gateway_url`] (https required except loopback; embedded creds
//! rejected). Errors map to [`AppError::Unavailable`] (network/HTTP) or [`AppError::Auth`] (401) so
//! the FE can branch cleanly; the reqwest error Display is never surfaced verbatim (it can echo the
//! URL) — we log/return a fixed message + the numeric status only.

use crate::error::{AppError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevokeShareResult {
    Deleted,
    NotFound,
}
use murmur_protocol::dto::{
    AcceptShareResponse, AttachKeyRequest, CreateShareRequest, CreateShareResponse, Health,
    InboxResponse, KeyLookupRequest, KeyLookupResponse, LoginFinishRequest, LoginFinishResponse,
    LoginStartRequest, LoginStartResponse, ProvisionFinishRequest, ProvisionFinishResponse,
    ProvisionRequest, ProvisionResponse, RefreshRequest, RefreshResponse, ReserveShareIdRequest,
    ShareMode, SharesResponse, SignupRequest, VerifyEmailRequest, VerifyEmailResponse,
};
use reqwest::StatusCode;

/// A validated murmur-server base URL + a reqwest client. Constructed per-operation from the current
/// Settings value (cheap: reqwest pools connections process-wide).
pub struct ShareClient {
    base: reqwest::Url,
    http: reqwest::Client,
}

pub(crate) enum OrgDeleteDocumentResult {
    Deleted,
    NotFound,
}

pub(crate) enum OrgTombstoneItemResult {
    Deleted,
    NotFound,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PublishItemWireResponse {
    item_id: String,
    seq: u64,
    doc_id: Option<String>,
    access: Option<super::org_dto::OrgItemAccess>,
    document_owner_user_id: Option<String>,
}

impl PublishItemWireResponse {
    fn into_response(self) -> super::org_dto::PublishItemResponse {
        super::org_dto::PublishItemResponse {
            item_id: self.item_id,
            seq: self.seq,
            doc_id: self.doc_id,
            access: self.access.unwrap_or_default(),
            document_owner_user_id: self.document_owner_user_id,
        }
    }
}

const ORG_PUBLISH_AMBIGUOUS: &str = "[org-publish-ambiguous]";

impl ShareClient {
    /// Build a client from a user-configured base URL. Validates the URL exactly like the AI gateway
    /// (reject embedded creds; http only for loopback). Fails closed if the URL is empty/invalid.
    pub fn new(base_url: &str) -> Result<Self> {
        let trimmed = base_url.trim();
        if trimmed.is_empty() {
            return Err(AppError::Unavailable(
                "no sharing server configured — set it in Settings → Account".into(),
            ));
        }
        // Same guardrails as the AI gateway: https required (http loopback-only), no embedded creds.
        let base = crate::summarize::gateway::validate_gateway_url(trimmed)?;
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|_| AppError::Unavailable("failed to build HTTP client".into()))?;
        Ok(Self { base, http })
    }

    /// The host:port of the configured server — the ONLY server-identifying string safe for the
    /// egress ledger (never a path, never a share id, never `L`).
    pub fn host(&self) -> String {
        match (self.base.host_str(), self.base.port()) {
            (Some(h), Some(p)) => format!("{h}:{p}"),
            (Some(h), None) => h.to_string(),
            _ => "unknown".to_string(),
        }
    }

    fn url(&self, path: &str) -> Result<reqwest::Url> {
        self.base
            .join(path)
            .map_err(|_| AppError::Unavailable("invalid server URL path".into()))
    }

    /// Map a response status to a typed error WITHOUT surfacing the reqwest Display (which can echo
    /// the URL). A 4xx is a REQUEST problem (bad/expired code, used token) → `InvalidArg`, so the UI
    /// never renders it as "can't reach the server" — only a genuine connection failure (the `.send()`
    /// map_err → `Unavailable`) or a 5xx is unavailability. 401 → Auth (re-login).
    fn status_err(ctx: &str, status: StatusCode) -> AppError {
        match status {
            StatusCode::UNAUTHORIZED => AppError::Auth(crate::errcode::tag(
                crate::errcode::SHARING_SIGNIN_REQUIRED,
                format!("{ctx}: not authenticated"),
            )),
            StatusCode::TOO_MANY_REQUESTS => AppError::InvalidArg(crate::errcode::tag(
                crate::errcode::SHARING_RATE_LIMITED,
                format!("{ctx}: too many attempts"),
            )),
            s if s.is_client_error() => AppError::InvalidArg(crate::errcode::tag(
                crate::errcode::SHARING_REJECTED,
                format!("{ctx}: rejected ({})", s.as_u16()),
            )),
            s => AppError::Unavailable(crate::errcode::tag(
                crate::errcode::SHARING_UNREACHABLE,
                format!("{ctx}: server returned {}", s.as_u16()),
            )),
        }
    }

    async fn post_json<B: serde::Serialize, R: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        bearer: Option<&str>,
        body: &B,
        ctx: &'static str,
    ) -> Result<R> {
        let url = self.url(path)?;
        let mut req = self.http.post(url).json(body);
        if let Some(tok) = bearer {
            req = req.bearer_auth(tok);
        }
        let resp = req.send().await.map_err(|_| {
            AppError::Unavailable(crate::errcode::tag(
                crate::errcode::SHARING_UNREACHABLE,
                format!("{ctx}: could not reach the server"),
            ))
        })?;
        let status = resp.status();
        if !status.is_success() {
            return Err(Self::status_err(ctx, status));
        }
        resp.json::<R>()
            .await
            .map_err(|_| AppError::Unavailable(format!("{ctx}: malformed server response")))
    }

    async fn get_json<R: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        bearer: &str,
        ctx: &'static str,
    ) -> Result<R> {
        let url = self.url(path)?;
        let resp = self
            .http
            .get(url)
            .bearer_auth(bearer)
            .send()
            .await
            .map_err(|_| {
                AppError::Unavailable(crate::errcode::tag(
                    crate::errcode::SHARING_UNREACHABLE,
                    format!("{ctx}: could not reach the server"),
                ))
            })?;
        let status = resp.status();
        if !status.is_success() {
            return Err(Self::status_err(ctx, status));
        }
        resp.json::<R>()
            .await
            .map_err(|_| AppError::Unavailable(format!("{ctx}: malformed server response")))
    }

    // ─────────────────────────────── auth ───────────────────────────────

    /// `POST /v1/auth/signup {email}` → always 202 (anti-enumeration). Body is discarded.
    pub async fn signup(&self, email: &str) -> Result<()> {
        let _: murmur_protocol::dto::Accepted = self
            .post_json(
                "/v1/auth/signup",
                None,
                &SignupRequest {
                    email: email.to_string(),
                },
                "signup",
            )
            .await?;
        Ok(())
    }

    /// `POST /v1/auth/verify-email {email, code}` → single-use signup token.
    pub async fn verify_email(&self, email: &str, code: &str) -> Result<String> {
        let r: VerifyEmailResponse = self
            .post_json(
                "/v1/auth/verify-email",
                None,
                &VerifyEmailRequest {
                    email: email.to_string(),
                    code: code.to_string(),
                },
                "verify-email",
            )
            .await?;
        Ok(r.signup_token)
    }

    /// `POST /v1/auth/provision` — OPAQUE registration step 1; returns `registrationResponse` bytes.
    pub async fn provision(
        &self,
        signup_token: &str,
        opaque_registration_request: Vec<u8>,
    ) -> Result<Vec<u8>> {
        let r: ProvisionResponse = self
            .post_json(
                "/v1/auth/provision",
                None,
                &ProvisionRequest {
                    signup_token: signup_token.to_string(),
                    opaque_registration_request,
                },
                "provision",
            )
            .await?;
        Ok(r.registration_response)
    }

    /// `POST /v1/auth/provision/finish` — atomic account activation (§3.1a).
    #[allow(clippy::too_many_arguments)]
    pub async fn provision_finish(&self, req: ProvisionFinishRequest) -> Result<String> {
        let r: ProvisionFinishResponse = self
            .post_json("/v1/auth/provision/finish", None, &req, "provision-finish")
            .await?;
        Ok(r.user_id)
    }

    /// `POST /v1/auth/login/start {email, ke1}` → `{loginId, ke2}`.
    pub async fn login_start(&self, email: &str, ke1: Vec<u8>) -> Result<LoginStartResponse> {
        self.post_json(
            "/v1/auth/login/start",
            None,
            &LoginStartRequest {
                email: email.to_string(),
                ke1,
            },
            "login-start",
        )
        .await
    }

    /// `POST /v1/auth/login/finish` → session tokens + the server-stored key material.
    pub async fn login_finish(
        &self,
        login_id: &str,
        ke3: Vec<u8>,
        platform: &str,
    ) -> Result<LoginFinishResponse> {
        self.post_json(
            "/v1/auth/login/finish",
            None,
            &LoginFinishRequest {
                login_id: login_id.to_string(),
                ke3,
                device: murmur_protocol::dto::DeviceInfo {
                    platform: platform.to_string(),
                },
            },
            "login-finish",
        )
        .await
    }

    /// `POST /v1/auth/refresh {refreshToken}` → a ROTATED session pair (a fresh access token + a fresh
    /// refresh token, same family). The presented refresh token is SINGLE-USE: the caller MUST persist
    /// the returned pair and never re-present the old token — re-presenting it triggers server-side
    /// family revocation (§3.3 reuse detection). A 401 here means the refresh token itself is
    /// expired/revoked/reused ⇒ the session is unrecoverable and the user must sign in again.
    pub async fn refresh(&self, refresh_token: &str) -> Result<RefreshResponse> {
        self.post_json(
            "/v1/auth/refresh",
            None,
            &RefreshRequest {
                refresh_token: refresh_token.to_string(),
            },
            "refresh-session",
        )
        .await
    }

    /// `POST /v1/auth/logout` (bearer). Best-effort; a 401 is treated as already-logged-out.
    pub async fn logout(&self, access_token: &str) -> Result<()> {
        let url = self.url("/v1/auth/logout")?;
        let resp = self
            .http
            .post(url)
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|_| AppError::Unavailable("logout: could not reach the server".into()))?;
        // NO_CONTENT (204) on success; a 401 means the token was already invalid — fine.
        if resp.status().is_success() || resp.status() == StatusCode::UNAUTHORIZED {
            Ok(())
        } else {
            Err(Self::status_err("logout", resp.status()))
        }
    }

    // ─────────────────────────────── shares ───────────────────────────────

    /// `GET /healthz` — content-free capability discovery. Old relays deserialize with an empty
    /// capability set; callers MUST fail closed before a content POST when the owner-claim contract
    /// is absent. This request intentionally carries no bearer token.
    pub(crate) async fn health(
        &self,
        permit: crate::commands::ShareCapabilityReadPermit,
    ) -> Result<Health> {
        permit.authorize(&self.host(), "/healthz")?;
        let url = self.url("/healthz")?;
        let resp = self.http.get(url).send().await.map_err(|_| {
            AppError::Unavailable(crate::errcode::tag(
                crate::errcode::SHARING_UNREACHABLE,
                "sharing capability check could not reach the server",
            ))
        })?;
        let status = resp.status();
        if !status.is_success() {
            return Err(Self::status_err("sharing capability check", status));
        }
        resp.json::<Health>().await.map_err(|_| {
            AppError::Unavailable("sharing capability check returned malformed metadata".into())
        })
    }

    /// `PUT /v1/shares/{id}/reservation` — bind a client-minted id to this authenticated owner and
    /// immutable mode BEFORE any ciphertext POST. Only exact 204 is terminal; every other outcome
    /// leaves the caller's durable local attempt unresolved and forbids content egress.
    pub(crate) async fn reserve_share_id(
        &self,
        access_token: &str,
        share_id: &str,
        owner_user_id: &str,
        mode: ShareMode,
        permit: crate::commands::ShareReservationPermit,
    ) -> Result<()> {
        if uuid::Uuid::parse_str(share_id).is_err() {
            return Err(AppError::InvalidArg("invalid share id".into()));
        }
        permit.authorize(&self.host(), share_id, owner_user_id, mode)?;
        let url = self.url(&format!("/v1/shares/{share_id}/reservation"))?;
        let resp = self
            .http
            .put(url)
            .bearer_auth(access_token)
            .json(&ReserveShareIdRequest { mode })
            .send()
            .await
            .map_err(|_| {
                AppError::Unavailable(crate::errcode::tag(
                    crate::errcode::SHARING_UNREACHABLE,
                    "share reservation could not reach the server",
                ))
            })?;
        if resp.status() == StatusCode::NO_CONTENT {
            Ok(())
        } else {
            Err(Self::status_err("share reservation", resp.status()))
        }
    }

    async fn create_share_exact(
        &self,
        access_token: &str,
        owner_user_id: &str,
        source_commitment: [u8; 32],
        req: CreateShareRequest,
        permit: crate::commands::ShareContentDispatchPermit,
        ctx: &'static str,
    ) -> Result<CreateShareResponse> {
        permit.authorize(&self.host(), owner_user_id, source_commitment, &req)?;
        let expected_share_id = req.share_id.clone();
        let url = self.url("/v1/shares")?;
        let resp = self
            .http
            .post(url)
            .bearer_auth(access_token)
            .json(&req)
            .send()
            .await
            .map_err(|_| {
                AppError::Unavailable(crate::errcode::tag(
                    crate::errcode::SHARING_UNREACHABLE,
                    format!("{ctx}: could not reach the server"),
                ))
            })?;
        if resp.status() != StatusCode::CREATED {
            return Err(Self::status_err(ctx, resp.status()));
        }
        let created = resp.json::<CreateShareResponse>().await.map_err(|_| {
            AppError::Unavailable(format!("{ctx}: malformed server response"))
        })?;
        if created.share_id != expected_share_id {
            return Err(AppError::Unavailable(format!(
                "{ctx}: server returned a different share id"
            )));
        }
        Ok(created)
    }

    /// `POST /v1/shares` (bearer) — create a link share. Returns `{shareId, shareBaseUrl}`. The link
    /// key `L` is NOT in the request (the caller keeps it) — enforced by the `CreateShareRequest`
    /// field set (there is no `l` field).
    pub(crate) async fn create_share(
        &self,
        access_token: &str,
        owner_user_id: &str,
        source_commitment: [u8; 32],
        req: CreateShareRequest,
        permit: crate::commands::ShareContentDispatchPermit,
    ) -> Result<CreateShareResponse> {
        self.create_share_exact(
            access_token,
            owner_user_id,
            source_commitment,
            req,
            permit,
            "create-share",
        )
            .await
    }

    /// `GET /v1/shares` (bearer) — the caller's shares (content-free; no titles server-side).
    pub async fn list_shares(&self, access_token: &str) -> Result<SharesResponse> {
        self.get_json("/v1/shares", access_token, "list-shares")
            .await
    }

    /// `DELETE /v1/shares/{id}` (bearer, owner-only) — revoke. A non-owner/missing share is the same
    /// uniform 404, so 404 is NOT deletion proof: treating it as success could seal/destroy a local
    /// source while ciphertext owned by a different session remains remotely readable.
    pub(crate) async fn revoke_share(
        &self,
        access_token: &str,
        share_id: &str,
        owner_user_id: &str,
        mode: ShareMode,
        rev: u32,
        permit: crate::commands::ShareDeleteDispatchPermit,
    ) -> Result<RevokeShareResult> {
        // share_id is a UUID we minted/stored; path-safe. Still, guard against a stray slash.
        if share_id.contains('/') || share_id.contains('?') {
            return Err(AppError::InvalidArg("invalid share id".into()));
        }
        permit.authorize(&self.host(), share_id, owner_user_id, mode, rev)?;
        let url = self.url(&format!("/v1/shares/{share_id}"))?;
        let resp = self
            .http
            .delete(url)
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|_| {
                AppError::Unavailable("revoke-share: could not reach the server".into())
            })?;
        if resp.status() == StatusCode::NO_CONTENT {
            Ok(RevokeShareResult::Deleted)
        } else if resp.status() == StatusCode::NOT_FOUND {
            Ok(RevokeShareResult::NotFound)
        } else {
            Err(Self::status_err("revoke-share", resp.status()))
        }
    }

    // ─────────────────────── M5: Murmur↔Murmur (mode B) ───────────────────────

    /// `POST /v1/keys/lookup {email}` (bearer, 20/day, audited) — is the address a registered account,
    /// and if so its current identity key + safety-word fingerprint (spec §13/§6). The request carries
    /// ONLY the recipient's email (never any note content).
    pub async fn lookup_key(&self, access_token: &str, email: &str) -> Result<KeyLookupResponse> {
        self.post_json(
            "/v1/keys/lookup",
            Some(access_token),
            &KeyLookupRequest {
                email: email.to_string(),
            },
            "keys-lookup",
        )
        .await
    }

    /// `PUT /v1/shares/{id}/keys` (bearer, owner) — attach a re-wrapped key after an invitee
    /// registered (`awaiting_key`) or a key rotated (`stale_key`). Opaque bytes; server never verifies.
    pub async fn attach_key(
        &self,
        access_token: &str,
        share_id: &str,
        req: AttachKeyRequest,
    ) -> Result<()> {
        if share_id.contains('/') || share_id.contains('?') {
            return Err(AppError::InvalidArg("invalid share id".into()));
        }
        let url = self.url(&format!("/v1/shares/{share_id}/keys"))?;
        let resp = self
            .http
            .put(url)
            .bearer_auth(access_token)
            .json(&req)
            .send()
            .await
            .map_err(|_| AppError::Unavailable("attach-key: could not reach the server".into()))?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(Self::status_err("attach-key", resp.status()))
        }
    }

    /// `GET /v1/inbox` (bearer) — the caller's incoming `pending_accept` shares (content-free).
    pub async fn list_inbox(&self, access_token: &str) -> Result<InboxResponse> {
        self.get_json("/v1/inbox", access_token, "inbox").await
    }

    /// `POST /v1/shares/{id}/accept` (bearer, recipient) — flip the recipient row to `accepted` and
    /// return the content blob id the recipient is now authorized to GET.
    pub async fn accept_share_server(
        &self,
        access_token: &str,
        share_id: &str,
    ) -> Result<AcceptShareResponse> {
        if share_id.contains('/') || share_id.contains('?') {
            return Err(AppError::InvalidArg("invalid share id".into()));
        }
        self.post_json(
            &format!("/v1/shares/{share_id}/accept"),
            Some(access_token),
            &serde_json::json!({}),
            "accept-share",
        )
        .await
    }

    /// `POST /v1/shares/{id}/decline` (bearer, recipient) — drop the wrapped key server-side. 204.
    pub async fn decline_share_server(&self, access_token: &str, share_id: &str) -> Result<()> {
        if share_id.contains('/') || share_id.contains('?') {
            return Err(AppError::InvalidArg("invalid share id".into()));
        }
        let url = self.url(&format!("/v1/shares/{share_id}/decline"))?;
        let resp = self
            .http
            .post(url)
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|_| {
                AppError::Unavailable("decline-share: could not reach the server".into())
            })?;
        if resp.status().is_success() || resp.status() == StatusCode::NOT_FOUND {
            Ok(())
        } else {
            Err(Self::status_err("decline-share", resp.status()))
        }
    }

    /// `GET /v1/blobs/{id}` (bearer) — fetch the raw ciphertext content cell `C` (octet-stream). The
    /// server authorizes by recipiency (only after `accept` flips the row to `accepted`).
    pub async fn get_blob(&self, access_token: &str, blob_id: &str) -> Result<Vec<u8>> {
        if blob_id.contains('/') || blob_id.contains('?') {
            return Err(AppError::InvalidArg("invalid blob id".into()));
        }
        let url = self.url(&format!("/v1/blobs/{blob_id}"))?;
        let resp = self
            .http
            .get(url)
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|_| AppError::Unavailable("get-blob: could not reach the server".into()))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(Self::status_err("get-blob", status));
        }
        // Size cap (adversarial finding): a malicious server could stream a multi-GB body → OOM.
        // A legit content cell is <= the server's blob cap; reject anything larger, checking both the
        // advertised Content-Length AND the actual streamed bytes (a chunked body can lie/omit it).
        let cap = murmur_protocol::caps::MAX_BLOB_BYTES + 4096;
        if resp.content_length().is_some_and(|n| n as usize > cap) {
            return Err(AppError::Unavailable("get-blob: response too large".into()));
        }
        let mut resp = resp;
        let mut buf: Vec<u8> = Vec::new();
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|_| AppError::Unavailable("get-blob: malformed server response".into()))?
        {
            if buf.len() + chunk.len() > cap {
                return Err(AppError::Unavailable("get-blob: response too large".into()));
            }
            buf.extend_from_slice(&chunk);
        }
        Ok(buf)
    }

    // ─────────────────────── Organizations / Shared Brain (M6) ───────────────────────
    //
    // The server is a zero-knowledge relay for orgs too: every body is an opaque id, a role, a
    // wrapped OCK grant, ciphertext, or a hash — NEVER a note title / body / OCK / plaintext. The
    // OCK is wrapped/unwrapped ONLY client-side (`e2ee::org`); it never appears in any request here.

    /// `POST /v1/orgs {name}` (bearer) — create an org; caller becomes sole `owner`.
    pub async fn org_create(
        &self,
        access_token: &str,
        name: &str,
    ) -> Result<super::org_dto::OrgResponse> {
        self.post_json(
            "/v1/orgs",
            Some(access_token),
            &super::org_dto::CreateOrgRequest {
                name: name.to_string(),
            },
            "org-create",
        )
        .await
    }

    /// `GET /v1/orgs` (bearer) — EVERY org the caller actively belongs to (owned OR invited-and-active).
    /// This is the membership-discovery pull: without it an org you were INVITED to (never one you
    /// CREATED) is invisible locally and its feed never syncs. Content-free (ids/roles/timestamps).
    pub async fn org_list(&self, access_token: &str) -> Result<Vec<super::org_dto::OrgSummary>> {
        let resp: super::org_dto::OrgListResponse =
            self.get_json("/v1/orgs", access_token, "org-list").await?;
        Ok(resp.orgs)
    }

    /// `GET /v1/orgs/{id}` (member-gated) — the caller's view of an org (name + role + generation).
    pub async fn org_status(
        &self,
        access_token: &str,
        org_id: &str,
    ) -> Result<super::org_dto::OrgResponse> {
        Self::guard_id(org_id)?;
        self.get_json(&format!("/v1/orgs/{org_id}"), access_token, "org-status")
            .await
    }

    /// Exact membership corroboration used only when the aggregate org list is unexpectedly empty.
    /// A 404 on this member-gated resource is authoritative absence; other failures stay errors.
    pub(crate) async fn org_status_optional(
        &self,
        access_token: &str,
        org_id: &str,
        permit: crate::commands::OrgDispatchPermit,
    ) -> Result<Option<super::org_dto::OrgResponse>> {
        Self::guard_id(org_id)?;
        permit.authorize_membership_corroborate(&self.host(), org_id)?;
        let url = self.url(&format!("/v1/orgs/{org_id}"))?;
        let resp = self
            .http
            .get(url)
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|_| AppError::Unavailable("org-status: could not reach the server".into()))?;
        if resp.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !resp.status().is_success() {
            return Err(Self::status_err("org-status", resp.status()));
        }
        let status: super::org_dto::OrgResponse = resp
            .json()
            .await
            .map_err(|_| AppError::Unavailable("org-status: malformed server response".into()))?;
        if status.org_id != org_id || status.current_generation == 0 {
            return Err(AppError::Unavailable(
                "org-status: inconsistent membership metadata".into(),
            ));
        }
        Ok(Some(status))
    }

    /// `POST /v1/orgs/{id}/members {email}` (owner-only) — add a registered account by email.
    /// Returns the resolved member's account id.
    pub async fn org_add_member(
        &self,
        access_token: &str,
        org_id: &str,
        email: &str,
    ) -> Result<super::org_dto::AddMemberResponse> {
        Self::guard_id(org_id)?;
        self.post_json(
            &format!("/v1/orgs/{org_id}/members"),
            Some(access_token),
            &super::org_dto::AddMemberRequest {
                email: email.to_string(),
            },
            "org-add-member",
        )
        .await
    }

    /// `DELETE /v1/orgs/{id}/members/{userId}` (owner-only) — soft-remove a member. A missing/owner/
    /// non-member target is a uniform 404, treated as "already gone" (idempotent).
    pub async fn org_remove_member(
        &self,
        access_token: &str,
        org_id: &str,
        member_id: &str,
    ) -> Result<()> {
        Self::guard_id(org_id)?;
        Self::guard_id(member_id)?;
        let url = self.url(&format!("/v1/orgs/{org_id}/members/{member_id}"))?;
        let resp = self
            .http
            .delete(url)
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|_| {
                AppError::Unavailable("org-remove-member: could not reach the server".into())
            })?;
        if resp.status().is_success() || resp.status() == StatusCode::NOT_FOUND {
            Ok(())
        } else {
            Err(Self::status_err("org-remove-member", resp.status()))
        }
    }

    /// `GET /v1/orgs/{id}/members` (member-gated) — the org's active members (content-free).
    pub async fn org_list_members(
        &self,
        access_token: &str,
        org_id: &str,
    ) -> Result<super::org_dto::OrgMembersResponse> {
        Self::guard_id(org_id)?;
        self.get_json(
            &format!("/v1/orgs/{org_id}/members"),
            access_token,
            "org-list-members",
        )
        .await
    }

    /// `POST /v1/orgs/{id}/leave` (member self-removal). 204 on success; a 404 (already gone) is
    /// idempotent-OK.
    pub async fn org_leave(&self, access_token: &str, org_id: &str) -> Result<()> {
        Self::guard_id(org_id)?;
        let url = self.url(&format!("/v1/orgs/{org_id}/leave"))?;
        let resp = self
            .http
            .post(url)
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|_| AppError::Unavailable("org-leave: could not reach the server".into()))?;
        if resp.status().is_success() || resp.status() == StatusCode::NOT_FOUND {
            Ok(())
        } else {
            Err(Self::status_err("org-leave", resp.status()))
        }
    }

    /// `PUT /v1/orgs/{id}/key-grants` (member-gated) — upload opaque wrapped-OCK grants for a
    /// generation. The bytes are stored as-is, never verified server-side (§4.8).
    pub async fn org_put_key_grants(
        &self,
        access_token: &str,
        org_id: &str,
        grants: Vec<super::org_dto::KeyGrantInput>,
    ) -> Result<()> {
        Self::guard_id(org_id)?;
        let url = self.url(&format!("/v1/orgs/{org_id}/key-grants"))?;
        let resp = self
            .http
            .put(url)
            .bearer_auth(access_token)
            .json(&super::org_dto::PutKeyGrantsRequest { grants })
            .send()
            .await
            .map_err(|_| {
                AppError::Unavailable("org-put-key-grants: could not reach the server".into())
            })?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(Self::status_err("org-put-key-grants", resp.status()))
        }
    }

    /// `GET /v1/orgs/{id}/key-grants` (member-gated) — the CALLER's own wrapped-OCK grants.
    pub async fn org_get_key_grants(
        &self,
        access_token: &str,
        org_id: &str,
    ) -> Result<super::org_dto::KeyGrantsResponse> {
        Self::guard_id(org_id)?;
        self.get_json(
            &format!("/v1/orgs/{org_id}/key-grants"),
            access_token,
            "org-get-key-grants",
        )
        .await
    }

    /// `POST /v1/orgs/{id}/items` (member-gated) — publish a staged ciphertext blob as an org item.
    pub(crate) async fn org_publish_item(
        &self,
        access_token: &str,
        org_id: &str,
        req: super::org_dto::PublishItemRequest,
        permit: crate::commands::OrgDispatchPermit,
    ) -> Result<super::org_dto::PublishItemResponse> {
        Self::guard_id(org_id)?;
        let expected_owner_user_id = permit.authorize_publish(&self.host(), org_id, &req)?;
        let stable_expectation = match req.doc_id.as_deref() {
            Some(doc_id) => {
                let access = req.access.ok_or_else(|| {
                    AppError::Storage(
                        "stable org publish permit omitted explicit document access".into(),
                    )
                })?;
                let owner_user_id = expected_owner_user_id.ok_or_else(|| {
                    AppError::Storage("stable org publish permit omitted its owner".into())
                })?;
                Some((doc_id.to_string(), access, owner_user_id))
            }
            None if expected_owner_user_id.is_none() => None,
            None => {
                return Err(AppError::Storage(
                    "legacy org publish permit unexpectedly carried an owner".into(),
                ));
            }
        };
        let url = self.url(&format!("/v1/orgs/{org_id}/items"))?;
        let resp = self
            .http
            .post(url)
            .bearer_auth(access_token)
            .json(&req)
            .send()
            .await
            .map_err(|_| {
                AppError::Unavailable("org-publish-item: could not reach the server".into())
            })?;
        if resp.status() == StatusCode::CONFLICT {
            return Err(AppError::InvalidArg(crate::errcode::tag(
                crate::errcode::ORG_EDIT_CONFLICT,
                "the durable shared document already exists",
            )));
        }
        if !resp.status().is_success() {
            return Err(Self::status_err("org-publish-item", resp.status()));
        }
        let stable_response_expected = stable_expectation.is_some();
        let wire: PublishItemWireResponse = resp.json().await.map_err(|_| {
            if stable_response_expected {
                Self::ambiguous_org_publish_response("malformed stable document response")
            } else {
                AppError::Unavailable("org-publish-item: malformed server response".into())
            }
        })?;
        if stable_response_expected && wire.access.is_none() {
            return Err(Self::ambiguous_org_publish_response(
                "stable document response omitted access",
            ));
        }
        let published = wire.into_response();
        if published.validate_document_metadata().is_err() {
            return Err(Self::ambiguous_org_publish_response(
                "partial stable document metadata",
            ));
        }
        match stable_expectation {
            Some((expected_doc_id, expected_access, expected_owner_user_id))
                if published.doc_id.as_deref() == Some(expected_doc_id.as_str())
                    && published.access == expected_access
                    && published.document_owner_user_id.as_deref()
                        == Some(expected_owner_user_id.as_str()) => {}
            Some(_) => {
                return Err(Self::ambiguous_org_publish_response(
                    "stable document metadata did not match the request",
                ));
            }
            None if published.doc_id.is_none() && published.document_owner_user_id.is_none() => {}
            None => {
                return Err(Self::ambiguous_org_publish_response(
                    "legacy request returned stable document metadata",
                ));
            }
        }
        Ok(published)
    }

    fn ambiguous_org_publish_response(reason: &'static str) -> AppError {
        AppError::Unavailable(format!(
            "{ORG_PUBLISH_AMBIGUOUS} org-publish-item: {reason}"
        ))
    }

    /// CAS-replace one stable Shared Brain document head. A 409 receives its own stable IPC code so
    /// the editor retains the draft and can offer reload instead of flattening it into generic 4xx.
    pub(crate) async fn org_update_item(
        &self,
        access_token: &str,
        org_id: &str,
        doc_id: &str,
        req: super::org_dto::UpdateOrgItemRequest,
        permit: crate::commands::OrgDispatchPermit,
    ) -> Result<super::org_dto::PublishItemResponse> {
        Self::guard_id(org_id)?;
        Self::guard_id(doc_id)?;
        let (expected_access, expected_owner_user_id) =
            permit.authorize_update(&self.host(), org_id, doc_id, &req)?;
        let url = self.url(&format!("/v1/orgs/{org_id}/documents/{doc_id}"))?;
        let resp = self
            .http
            .put(url)
            .bearer_auth(access_token)
            .json(&req)
            .send()
            .await
            .map_err(|_| {
                AppError::Unavailable("org-update-item: could not reach the server".into())
            })?;
        if resp.status() == StatusCode::CONFLICT {
            return Err(AppError::InvalidArg(crate::errcode::tag(
                crate::errcode::ORG_EDIT_CONFLICT,
                "the shared document changed before this draft was saved",
            )));
        }
        if !resp.status().is_success() {
            return Err(Self::status_err("org-update-item", resp.status()));
        }
        let published: super::org_dto::PublishItemResponse = resp.json().await.map_err(|_| {
            AppError::Unavailable("org-update-item: malformed server response".into())
        })?;
        published.validate_document_metadata().map_err(|_| {
            AppError::Unavailable("org-update-item: inconsistent document metadata".into())
        })?;
        if published.doc_id.as_deref() != Some(doc_id)
            || published.access != expected_access
            || published.document_owner_user_id.as_deref()
                != Some(expected_owner_user_id.as_str())
        {
            return Err(AppError::Unavailable(
                "org-update-item: inconsistent document metadata".into(),
            ));
        }
        Ok(published)
    }

    pub(crate) async fn org_set_item_access(
        &self,
        access_token: &str,
        org_id: &str,
        doc_id: &str,
        req: super::org_dto::SetOrgItemAccessRequest,
        permit: crate::commands::OrgDispatchPermit,
    ) -> Result<super::org_dto::OrgDocumentAccessResponse> {
        Self::guard_id(org_id)?;
        Self::guard_id(doc_id)?;
        let expected_owner_user_id =
            permit.authorize_access(&self.host(), org_id, doc_id, &req)?;
        let url = self.url(&format!("/v1/orgs/{org_id}/documents/{doc_id}"))?;
        let resp = self
            .http
            .patch(url)
            .bearer_auth(access_token)
            .json(&req)
            .send()
            .await
            .map_err(|_| {
                AppError::Unavailable("org-set-item-access: could not reach the server".into())
            })?;
        if !resp.status().is_success() {
            return Err(Self::status_err("org-set-item-access", resp.status()));
        }
        let response: super::org_dto::OrgDocumentAccessResponse =
            resp.json().await.map_err(|_| {
                AppError::Unavailable("org-set-item-access: malformed server response".into())
            })?;
        if response.doc_id != doc_id
            || response.access != req.access
            || response.document_owner_user_id != expected_owner_user_id
        {
            return Err(AppError::Unavailable(
                "org-set-item-access: inconsistent document metadata".into(),
            ));
        }
        Ok(response)
    }

    /// Withdraw the stable Shared Brain document (all revisions). A 404 is accepted as idempotent
    /// success only after the authenticated feed proves the known document has no live head;
    /// authorization remains relay-authoritative (stable document owner or org owner).
    pub(crate) async fn org_delete_document(
        &self,
        access_token: &str,
        org_id: &str,
        doc_id: &str,
        permit: crate::commands::OrgDispatchPermit,
    ) -> Result<OrgDeleteDocumentResult> {
        Self::guard_id(org_id)?;
        Self::guard_id(doc_id)?;
        permit.authorize_delete_document(&self.host(), org_id, doc_id)?;
        let url = self.url(&format!("/v1/orgs/{org_id}/documents/{doc_id}"))?;
        let resp = self
            .http
            .delete(url)
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|_| {
                AppError::Unavailable("org-delete-document: could not reach the server".into())
            })?;
        if resp.status().is_success() {
            return Ok(OrgDeleteDocumentResult::Deleted);
        }
        if resp.status() == StatusCode::NOT_FOUND {
            return Ok(OrgDeleteDocumentResult::NotFound);
        }
        Err(Self::status_err("org-delete-document", resp.status()))
    }

    /// Fetch exactly one authenticated feed page for stable-document recovery. The caller owns
    /// pagination and must supply one freshly ledgered permit for every actual GET attempt.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn org_document_recovery_page(
        &self,
        access_token: &str,
        org_id: &str,
        doc_id: &str,
        purpose: crate::commands::OrgReadPurpose,
        since_seq: u64,
        limit: u32,
        permit: crate::commands::OrgReadPermit,
    ) -> Result<super::org_dto::OrgItemsResponse> {
        Self::guard_id(org_id)?;
        Self::guard_id(doc_id)?;
        permit.authorize_page(&self.host(), org_id, doc_id, purpose, since_seq, limit)?;
        self.org_feed(access_token, org_id, since_seq, limit).await
    }

    /// `GET /v1/orgs/{id}/items?sinceSeq=&limit=` (member-gated) — the append-only feed from a cursor.
    pub async fn org_feed(
        &self,
        access_token: &str,
        org_id: &str,
        since_seq: u64,
        limit: u32,
    ) -> Result<super::org_dto::OrgItemsResponse> {
        Self::guard_id(org_id)?;
        let path = format!("/v1/orgs/{org_id}/items?sinceSeq={since_seq}&limit={limit}");
        self.get_json(&path, access_token, "org-feed").await
    }

    /// `DELETE /v1/orgs/{id}/items/{itemId}` (author OR owner) — tombstone an item + destroy its
    /// ciphertext. A 404 (already gone / not authorized) is idempotent-OK for a revoke.
    pub(crate) async fn org_tombstone_item(
        &self,
        access_token: &str,
        org_id: &str,
        item_id: &str,
        permit: crate::commands::OrgDispatchPermit,
    ) -> Result<OrgTombstoneItemResult> {
        Self::guard_id(org_id)?;
        Self::guard_id(item_id)?;
        permit.authorize_tombstone(&self.host(), org_id, item_id)?;
        let url = self.url(&format!("/v1/orgs/{org_id}/items/{item_id}"))?;
        let resp = self
            .http
            .delete(url)
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|_| {
                AppError::Unavailable("org-tombstone-item: could not reach the server".into())
            })?;
        if resp.status().is_success() {
            return Ok(OrgTombstoneItemResult::Deleted);
        }
        if resp.status() == StatusCode::NOT_FOUND {
            return Ok(OrgTombstoneItemResult::NotFound);
        }
        Err(Self::status_err("org-tombstone-item", resp.status()))
    }

    /// `POST /v1/orgs/{id}/generation {generation}` (owner) — bump the OCK generation (monotonic
    /// +1). The owner must have already PUT grants for gen `generation` for EVERY active member;
    /// the server checks that coverage in the same transaction and answers 409 otherwise.
    ///
    /// The JSON body is REQUIRED. This call used to send none, and the server extracts it with
    /// axum's `Json<BumpGenerationRequest>` — an extractor that rejects a body-less POST with 415
    /// *before* the handler runs. So the rotation that follows every member removal always failed,
    /// the org never left the generation the removed member held a key for, and the user saw only
    /// an untagged refusal. Returns the server's new live generation so the caller records what the
    /// relay actually committed rather than what it hoped for.
    pub async fn org_bump_generation(
        &self,
        access_token: &str,
        org_id: &str,
        generation: u32,
    ) -> Result<u32> {
        Self::guard_id(org_id)?;
        let url = self.url(&format!("/v1/orgs/{org_id}/generation"))?;
        let resp = self
            .http
            .post(url)
            .bearer_auth(access_token)
            .json(&super::org_dto::BumpGenerationRequest { generation })
            .send()
            .await
            .map_err(|_| {
                AppError::Unavailable("org-bump-generation: could not reach the server".into())
            })?;
        if !resp.status().is_success() {
            return Err(Self::status_err("org-bump-generation", resp.status()));
        }
        let body: super::org_dto::BumpGenerationResponse = resp.json().await.map_err(|_| {
            AppError::Unavailable("org-bump-generation: malformed server response".into())
        })?;
        Ok(body.current_generation)
    }

    /// Guard a path segment that we mint (a UUID) against traversal / query injection. A stray `/`
    /// or `?` is a client bug, refused before the request is built.
    fn guard_id(id: &str) -> Result<()> {
        if id.contains('/') || id.contains('?') || id.contains('#') || id.is_empty() {
            return Err(AppError::InvalidArg("invalid org/member/item id".into()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC_ID: &str = "11111111-1111-4111-8111-111111111111";
    const ORG_ID: &str = "33333333-3333-4333-8333-333333333333";
    const OWNER_ID: &str = "55555555-5555-4555-8555-555555555555";

    fn serve_json_once(body: &'static str) -> (String, std::thread::JoinHandle<()>) {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            let mut buf = [0u8; 1024];
            loop {
                match stream.read(&mut buf) {
                    Ok(0) => break,
                    Ok(read) => {
                        request.extend_from_slice(&buf[..read]);
                        if request.windows(4).any(|window| window == b"\r\n\r\n") {
                            break;
                        }
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) =>
                    {
                        break;
                    }
                    Err(error) => panic!("failed to read request: {error}"),
                }
            }
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
        });
        (format!("http://{addr}"), handle)
    }

    fn serve_status_once(status: &'static str) -> (String, std::thread::JoinHandle<()>) {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            let mut buf = [0u8; 1024];
            loop {
                match stream.read(&mut buf) {
                    Ok(0) => break,
                    Ok(read) => {
                        request.extend_from_slice(&buf[..read]);
                        if request.windows(4).any(|window| window == b"\r\n\r\n") {
                            break;
                        }
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) =>
                    {
                        break;
                    }
                    Err(error) => panic!("failed to read request: {error}"),
                }
            }
            assert!(String::from_utf8_lossy(&request).starts_with(&format!(
                "DELETE /v1/shares/{DOC_ID} HTTP/1.1"
            )));
            write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
        });
        (format!("http://{addr}"), handle)
    }

    fn stable_publish_request() -> super::super::org_dto::PublishItemRequest {
        super::super::org_dto::PublishItemRequest {
            mutation_id: None,
            doc_id: Some(DOC_ID.into()),
            access: Some(super::super::org_dto::OrgItemAccess::Edit),
            blob_id: None,
            content_cell: Some(vec![1, 2, 3]),
            content_sha256: vec![7; 32],
            rev: 1,
            generation: 1,
        }
    }

    async fn stable_publish_with_response(
        body: &'static str,
    ) -> Result<super::super::org_dto::PublishItemResponse> {
        let (base, server) = serve_json_once(body);
        let client = ShareClient::new(&base).unwrap();
        let request = stable_publish_request();
        let permit = crate::commands::test_org_publish_dispatch_permit(
            &client.host(),
            ORG_ID,
            &request,
            Some(OWNER_ID),
        );
        let result = client
            .org_publish_item("token", ORG_ID, request, permit)
            .await;
        server.join().unwrap();
        result
    }

    fn assert_ambiguous(result: Result<super::super::org_dto::PublishItemResponse>) {
        match result {
            Err(AppError::Unavailable(message)) => assert!(
                message.starts_with(ORG_PUBLISH_AMBIGUOUS),
                "wrong stable publish failure: {message}"
            ),
            other => panic!("expected an explicit ambiguous response error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn owner_claim_reservation_binds_exact_user_mode_and_requires_204() {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            let mut buf = [0u8; 1024];
            let mut expected = None;
            loop {
                let read = stream.read(&mut buf).unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..read]);
                if expected.is_none() {
                    if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&request[..end]);
                        let content_len = headers
                            .lines()
                            .find_map(|line| {
                                line.split_once(':').and_then(|(name, value)| {
                                    name.eq_ignore_ascii_case("content-length")
                                        .then(|| value.trim().parse::<usize>().ok())
                                        .flatten()
                                })
                            })
                            .unwrap_or(0);
                        expected = Some(end + 4 + content_len);
                    }
                }
                if expected.is_some_and(|length| request.len() >= length) {
                    break;
                }
            }
            let text = String::from_utf8_lossy(&request);
            assert!(text.starts_with(&format!(
                "PUT /v1/shares/{DOC_ID}/reservation HTTP/1.1"
            )));
            assert!(text.to_ascii_lowercase().contains("authorization: bearer token"));
            let body_at = request.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
            let json: serde_json::Value = serde_json::from_slice(&request[body_at..]).unwrap();
            assert_eq!(json, serde_json::json!({"mode":"user"}));
            stream
                .write_all(
                    b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
        });
        let client = ShareClient::new(&format!("http://{addr}")).unwrap();
        let permit = crate::commands::test_share_reservation_permit(
            &client.host(),
            DOC_ID,
            OWNER_ID,
            ShareMode::User,
        );
        let reused = permit.clone();
        client
            .reserve_share_id("token", DOC_ID, OWNER_ID, ShareMode::User, permit)
            .await
            .unwrap();
        server.join().unwrap();

        let error = client
            .reserve_share_id("token", DOC_ID, OWNER_ID, ShareMode::User, reused)
            .await
            .unwrap_err();
        assert!(matches!(error, AppError::Storage(_)));

        let mismatches = [
            crate::commands::test_share_reservation_permit(
                "different.invalid",
                DOC_ID,
                OWNER_ID,
                ShareMode::User,
            ),
            crate::commands::test_share_reservation_permit(
                &client.host(),
                "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
                OWNER_ID,
                ShareMode::User,
            ),
            crate::commands::test_share_reservation_permit(
                &client.host(),
                DOC_ID,
                "different-owner",
                ShareMode::User,
            ),
            crate::commands::test_share_reservation_permit(
                &client.host(),
                DOC_ID,
                OWNER_ID,
                ShareMode::Link,
            ),
        ];
        for mismatched in mismatches {
            let error = client
                .reserve_share_id("token", DOC_ID, OWNER_ID, ShareMode::User, mismatched)
                .await
                .unwrap_err();
            assert!(matches!(error, AppError::Storage(_)));
        }
    }

    #[tokio::test]
    async fn share_capability_read_permit_rejects_mismatch_and_reuse_before_socket() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let client =
            ShareClient::new(&format!("http://{}", listener.local_addr().unwrap())).unwrap();

        let mismatched = crate::commands::test_share_capability_read_permit("different.invalid");
        let error = client.health(mismatched).await.unwrap_err();
        assert!(matches!(error, AppError::Storage(_)));

        let permit = crate::commands::test_share_capability_read_permit(&client.host());
        let reused = permit.clone();
        permit.authorize(&client.host(), "/healthz").unwrap();
        let error = client.health(reused).await.unwrap_err();
        assert!(matches!(error, AppError::Storage(_)));

        assert!(matches!(
            listener.accept(),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
        ));
    }

    #[tokio::test]
    async fn share_content_and_delete_permits_reject_reuse_and_mismatch_before_socket() {
        let client = ShareClient::new("http://127.0.0.1:9").unwrap();
        let source_commitment = [9; 32];
        let request = CreateShareRequest {
            share_id: DOC_ID.into(),
            mode: ShareMode::Link,
            content_cell: vec![1, 2, 3],
            wrapped_nk: vec![4],
            gate_salt: vec![5],
            gate_secret: vec![6],
            rev: 1,
            password_required: false,
            argon: None,
            expires_at: None,
            max_downloads: None,
            recipients: None,
        };

        let content = crate::commands::test_share_content_dispatch_permit(
            &client.host(),
            OWNER_ID,
            source_commitment,
            &request,
        );
        let reused_content = content.clone();
        content
            .authorize(&client.host(), OWNER_ID, source_commitment, &request)
            .unwrap();
        let error = client
            .create_share(
                "token",
                OWNER_ID,
                source_commitment,
                request.clone(),
                reused_content,
            )
            .await
            .unwrap_err();
        assert!(matches!(error, AppError::Storage(_)));

        let wrong_owner = crate::commands::test_share_content_dispatch_permit(
            &client.host(),
            OWNER_ID,
            source_commitment,
            &request,
        );
        let error = client
            .create_share(
                "token",
                "different-owner",
                source_commitment,
                request,
                wrong_owner,
            )
            .await
            .unwrap_err();
        assert!(matches!(error, AppError::Storage(_)));

        let delete = crate::commands::test_share_delete_dispatch_permit(
            &client.host(),
            DOC_ID,
            OWNER_ID,
            ShareMode::Link,
            1,
        );
        let reused_delete = delete.clone();
        delete
            .authorize(&client.host(), DOC_ID, OWNER_ID, ShareMode::Link, 1)
            .unwrap();
        let error = client
            .revoke_share(
                "token",
                DOC_ID,
                OWNER_ID,
                ShareMode::Link,
                1,
                reused_delete,
            )
            .await
            .unwrap_err();
        assert!(matches!(error, AppError::Storage(_)));

        let wrong_mode = crate::commands::test_share_delete_dispatch_permit(
            &client.host(),
            DOC_ID,
            OWNER_ID,
            ShareMode::User,
            1,
        );
        let error = client
            .revoke_share(
                "token",
                DOC_ID,
                OWNER_ID,
                ShareMode::Link,
                1,
                wrong_mode,
            )
            .await
            .unwrap_err();
        assert!(matches!(error, AppError::Storage(_)));
    }

    #[tokio::test]
    async fn share_delete_requires_exact_204_not_other_success_statuses() {
        for status in ["200 OK", "202 Accepted"] {
            let (base, server) = serve_status_once(status);
            let client = ShareClient::new(&base).unwrap();
            let permit = crate::commands::test_share_delete_dispatch_permit(
                &client.host(),
                DOC_ID,
                OWNER_ID,
                ShareMode::Link,
                1,
            );
            let error = client
                .revoke_share(
                    "token",
                    DOC_ID,
                    OWNER_ID,
                    ShareMode::Link,
                    1,
                    permit,
                )
                .await
                .expect_err("only exact 204 proves remote deletion");
            server.join().unwrap();
            assert!(matches!(error, AppError::Unavailable(_)));
        }
    }

    #[tokio::test]
    async fn owner_claim_create_rejects_non_201_even_with_a_valid_body() {
        let body = r#"{"shareId":"11111111-1111-4111-8111-111111111111","shareBaseUrl":"https://share.invalid"}"#;
        let (base, server) = serve_json_once(body);
        let client = ShareClient::new(&base).unwrap();
        let source_commitment = [9; 32];
        let request = CreateShareRequest {
            share_id: DOC_ID.into(),
            mode: ShareMode::Link,
            content_cell: vec![1, 2, 3],
            wrapped_nk: vec![4],
            gate_salt: vec![5],
            gate_secret: vec![6],
            rev: 1,
            password_required: false,
            argon: None,
            expires_at: None,
            max_downloads: None,
            recipients: None,
        };
        let permit = crate::commands::test_share_content_dispatch_permit(
            &client.host(),
            OWNER_ID,
            source_commitment,
            &request,
        );
        let error = client
            .create_share(
                "token",
                OWNER_ID,
                source_commitment,
                request,
                permit,
            )
            .await
            .expect_err("owner-claim content create requires exact 201");
        server.join().unwrap();
        assert!(matches!(error, AppError::Unavailable(_)));
    }

    #[test]
    fn rejects_empty_and_credentialed_urls() {
        assert!(ShareClient::new("").is_err());
        assert!(ShareClient::new("   ").is_err());
        // Embedded credentials are rejected (validate_gateway_url).
        assert!(ShareClient::new("https://tok:@share.example.com").is_err());
        // Plain http on a non-loopback host is rejected.
        assert!(ShareClient::new("http://share.example.com").is_err());
    }

    #[test]
    fn accepts_https_and_loopback_http_and_reports_host() {
        let c = ShareClient::new("https://share.example.com/").unwrap();
        assert_eq!(c.host(), "share.example.com");
        let loop_c = ShareClient::new("http://127.0.0.1:8080").unwrap();
        assert_eq!(loop_c.host(), "127.0.0.1:8080");
    }

    #[test]
    fn host_is_never_a_full_url_or_path() {
        let c = ShareClient::new("https://api.murmur.example/v1/base").unwrap();
        let h = c.host();
        assert!(!h.contains('/'), "host must not carry a path: {h:?}");
        assert!(
            !h.starts_with("http"),
            "host must not carry a scheme: {h:?}"
        );
    }

    #[tokio::test]
    async fn stable_publish_rejects_success_missing_doc_id() {
        assert_ambiguous(
            stable_publish_with_response(
                r#"{"itemId":"22222222-2222-4222-8222-222222222222","seq":1,"access":"edit"}"#,
            )
            .await,
        );
    }

    #[tokio::test]
    async fn stable_publish_rejects_success_mismatched_doc_id() {
        assert_ambiguous(stable_publish_with_response(
            r#"{"itemId":"22222222-2222-4222-8222-222222222222","seq":1,"docId":"44444444-4444-4444-8444-444444444444","access":"edit","documentOwnerUserId":"55555555-5555-4555-8555-555555555555"}"#,
        )
        .await);
    }

    #[tokio::test]
    async fn stable_publish_rejects_success_missing_owner() {
        assert_ambiguous(stable_publish_with_response(
            r#"{"itemId":"22222222-2222-4222-8222-222222222222","seq":1,"docId":"11111111-1111-4111-8111-111111111111","access":"edit"}"#,
        )
        .await);
    }

    #[tokio::test]
    async fn stable_publish_rejects_success_mismatched_access() {
        assert_ambiguous(stable_publish_with_response(
            r#"{"itemId":"22222222-2222-4222-8222-222222222222","seq":1,"docId":"11111111-1111-4111-8111-111111111111","access":"view","documentOwnerUserId":"55555555-5555-4555-8555-555555555555"}"#,
        )
        .await);
    }

    #[tokio::test]
    async fn stable_publish_rejects_success_missing_access() {
        assert_ambiguous(stable_publish_with_response(
            r#"{"itemId":"22222222-2222-4222-8222-222222222222","seq":1,"docId":"11111111-1111-4111-8111-111111111111","documentOwnerUserId":"55555555-5555-4555-8555-555555555555"}"#,
        )
        .await);
    }

    #[tokio::test]
    async fn stable_publish_rejects_success_mismatched_owner() {
        assert_ambiguous(stable_publish_with_response(
            r#"{"itemId":"22222222-2222-4222-8222-222222222222","seq":1,"docId":"11111111-1111-4111-8111-111111111111","access":"edit","documentOwnerUserId":"66666666-6666-4666-8666-666666666666"}"#,
        )
        .await);
    }

    #[tokio::test]
    async fn stable_publish_tags_malformed_success_as_ambiguous() {
        assert_ambiguous(stable_publish_with_response(r#"{"itemId":null}"#).await);
    }

    #[tokio::test]
    async fn stable_publish_accepts_exact_document_metadata() {
        let published = stable_publish_with_response(
            r#"{"itemId":"22222222-2222-4222-8222-222222222222","seq":1,"docId":"11111111-1111-4111-8111-111111111111","access":"edit","documentOwnerUserId":"55555555-5555-4555-8555-555555555555"}"#,
        )
        .await
        .unwrap();
        assert_eq!(published.doc_id.as_deref(), Some(DOC_ID));
        assert_eq!(published.document_owner_user_id.as_deref(), Some(OWNER_ID));
        assert_eq!(published.access, super::super::org_dto::OrgItemAccess::Edit);
    }

    #[tokio::test]
    async fn legacy_publish_still_accepts_item_only_success() {
        let (base, server) = serve_json_once(
            r#"{"itemId":"22222222-2222-4222-8222-222222222222","seq":1,"access":"view"}"#,
        );
        let client = ShareClient::new(&base).unwrap();
        let mut req = stable_publish_request();
        req.doc_id = None;
        req.access = None;
        let permit = crate::commands::test_org_publish_dispatch_permit(
            &client.host(),
            ORG_ID,
            &req,
            None,
        );
        let published = client
            .org_publish_item("token", ORG_ID, req, permit)
            .await
            .unwrap();
        server.join().unwrap();
        assert!(published.doc_id.is_none());
        assert!(published.document_owner_user_id.is_none());
    }
}
