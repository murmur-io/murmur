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
use murmur_protocol::dto::{
    AcceptShareResponse, AttachKeyRequest, CreateShareRequest, CreateShareResponse, InboxResponse,
    KeyLookupRequest, KeyLookupResponse, LoginFinishRequest, LoginFinishResponse,
    LoginStartRequest, LoginStartResponse, ProvisionFinishRequest, ProvisionFinishResponse,
    ProvisionRequest, ProvisionResponse, RefreshRequest, RefreshResponse, SharesResponse,
    SignupRequest, VerifyEmailRequest, VerifyEmailResponse,
};
use reqwest::StatusCode;

/// A validated murmur-server base URL + a reqwest client. Constructed per-operation from the current
/// Settings value (cheap: reqwest pools connections process-wide).
pub struct ShareClient {
    base: reqwest::Url,
    http: reqwest::Client,
}

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
            StatusCode::UNAUTHORIZED => AppError::Auth(format!("{ctx}: not authenticated")),
            StatusCode::TOO_MANY_REQUESTS => {
                AppError::InvalidArg(format!("{ctx}: too many attempts — wait a bit and try again"))
            }
            s if s.is_client_error() => AppError::InvalidArg(format!("{ctx}: rejected ({})", s.as_u16())),
            s => AppError::Unavailable(format!("{ctx}: server returned {}", s.as_u16())),
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
        let resp = req
            .send()
            .await
            .map_err(|_| AppError::Unavailable(format!("{ctx}: could not reach the server")))?;
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
            .map_err(|_| AppError::Unavailable(format!("{ctx}: could not reach the server")))?;
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

    /// `POST /v1/shares` (bearer) — create a link share. Returns `{shareId, shareBaseUrl}`. The link
    /// key `L` is NOT in the request (the caller keeps it) — enforced by the `CreateShareRequest`
    /// field set (there is no `l` field).
    pub async fn create_share(
        &self,
        access_token: &str,
        req: CreateShareRequest,
    ) -> Result<CreateShareResponse> {
        self.post_json("/v1/shares", Some(access_token), &req, "create-share")
            .await
    }

    /// `GET /v1/shares` (bearer) — the caller's shares (content-free; no titles server-side).
    pub async fn list_shares(&self, access_token: &str) -> Result<SharesResponse> {
        self.get_json("/v1/shares", access_token, "list-shares")
            .await
    }

    /// `DELETE /v1/shares/{id}` (bearer, owner-only) — revoke. A non-owner/missing share is the same
    /// uniform 404; we treat 404 as "already gone" (idempotent revoke).
    pub async fn revoke_share(&self, access_token: &str, share_id: &str) -> Result<()> {
        // share_id is a UUID we minted/stored; path-safe. Still, guard against a stray slash.
        if share_id.contains('/') || share_id.contains('?') {
            return Err(AppError::InvalidArg("invalid share id".into()));
        }
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
        if resp.status().is_success() || resp.status() == StatusCode::NOT_FOUND {
            Ok(())
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

    /// `POST /v1/shares` for mode='user' (bearer). Same endpoint as a link share; the request carries
    /// the sealed content cell + per-recipient wrapped keys (opaque). Returns `{shareId, ...}`.
    pub async fn create_user_share(
        &self,
        access_token: &str,
        req: CreateShareRequest,
    ) -> Result<CreateShareResponse> {
        self.post_json("/v1/shares", Some(access_token), &req, "create-user-share")
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
