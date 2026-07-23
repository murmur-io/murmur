//! Wire DTOs for the murmur-server Organizations / Shared Brain API (spec §"App: local schema" +
//! server `routes/orgs.rs`).
//!
//! WHY THESE LIVE HERE (and not in `murmur_protocol::dto`): the app links the `murmur-protocol`
//! crate from the server's `main` worktree, while the org endpoints ship on the server's `feat/orgs`
//! branch. To keep the app build decoupled from the server branch's in-flight protocol edits (and to
//! follow the same "the wire contract is JSON, define the client's own request/response types"
//! discipline a zero-knowledge relay client wants), these mirror the server's `orgs` DTOs EXACTLY —
//! camelCase keys, base64 for opaque byte fields via [`murmur_protocol::b64`]. If/when the org DTOs
//! land on the linked protocol crate, these can be swapped for the shared types with no behavior
//! change (the JSON is identical). NO content-derived string ever appears here: every field is an
//! opaque id, a role label, a byte count, ciphertext, a wrapped key, a signature, or a hash.

use serde::{Deserialize, Serialize};

/// Reuse the shared base64url (url-unpadded) serde from `murmur_protocol` so the byte-field wire
/// encoding is byte-identical to the server's `#[serde(with = "b64")]` DTOs.
use murmur_protocol::b64;

fn one() -> u32 {
    1
}

/// `POST /v1/orgs {name}`.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CreateOrgRequest {
    pub name: String,
}

/// Response to create/status: content-free org metadata + the caller's role + the live OCK generation.
#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct OrgResponse {
    pub org_id: String,
    pub name: String,
    /// `owner` | `member`.
    pub role: String,
    pub created_at: String,
    /// The org's live OCK generation (defaults to 1 for older servers that omit it).
    #[serde(default = "one")]
    pub current_generation: u32,
}

/// One org the caller actively belongs to, in `GET /v1/orgs` — the membership-discovery pull that
/// makes an org you were INVITED to (not one you created) visible + syncable. Content-free: an opaque
/// id, a role label, a timestamp, and the live generation. Mirrors [`OrgResponse`]'s field set (the
/// server returns the same per-org shape in a list), tolerant of a server that omits `currentGeneration`.
#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct OrgSummary {
    pub org_id: String,
    pub name: String,
    /// `owner` | `member`.
    pub role: String,
    pub created_at: String,
    /// The org's live OCK generation (defaults to 1 for older servers that omit it).
    #[serde(default = "one")]
    pub current_generation: u32,
}

/// Response to `GET /v1/orgs` — every org the caller actively belongs to (owned OR invited-and-active).
#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct OrgListResponse {
    pub orgs: Vec<OrgSummary>,
}

/// `POST /v1/orgs/{id}/members {email}` (owner-only).
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AddMemberRequest {
    pub email: String,
}

/// Response: the resolved member account id (so the caller can wrap the OCK + issue a grant).
#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AddMemberResponse {
    pub user_id: String,
}

/// One member row (active members only; content-free).
#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct OrgMemberEntry {
    pub user_id: String,
    pub role: String,
    pub created_at: String,
    /// The member's email, when the server discloses it (2026-07-14). `#[serde(default)]` keeps an
    /// older server (which omits the field) deserializing cleanly — the FE then falls back to the id.
    #[serde(default)]
    pub email: Option<String>,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct OrgMembersResponse {
    pub members: Vec<OrgMemberEntry>,
}

/// `PUT /v1/orgs/{id}/key-grants` — upload/replace opaque wrapped-OCK grants for a generation.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PutKeyGrantsRequest {
    pub grants: Vec<KeyGrantInput>,
}

/// One opaque wrapped-OCK grant for a member at a generation.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct KeyGrantInput {
    pub user_id: String,
    pub generation: u32,
    /// The OCK sealed to the member's identity key (HPKE); opaque to the server.
    #[serde(with = "b64")]
    pub wrapped_key: Vec<u8>,
    /// The granter's signature over the canonical grant; relayed opaquely, NEVER verified.
    #[serde(with = "b64")]
    pub grant_sig: Vec<u8>,
}

/// One grant row in `GET /v1/orgs/{id}/key-grants` (the caller's own wrapped OCKs).
#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct KeyGrantEntry {
    pub user_id: String,
    pub generation: u32,
    #[serde(with = "b64")]
    pub wrapped_key: Vec<u8>,
    #[serde(with = "b64")]
    pub grant_sig: Vec<u8>,
    pub created_at: String,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct KeyGrantsResponse {
    pub grants: Vec<KeyGrantEntry>,
}

/// `POST /v1/orgs/{id}/items` — exactly one of legacy `blobId` or atomic inline `contentCell`.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PublishItemRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blob_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", with = "b64::opt")]
    pub content_cell: Option<Vec<u8>>,
    /// SHA-256 of the canonical PLAINTEXT envelope — NOT the ciphertext (see
    /// `OrgEnvelope::content_sha256`, `share/org_envelope.rs`). It is the content-free self-share
    /// dedup key AND the per-item AAD nonce source (`org_item_nonce(content_sha256)`): the publisher
    /// seals under `hex(plaintext-sha)` and every consumer re-derives the SAME value from the feed,
    /// so a cross-member open succeeds. Do NOT "fix" this to a ciphertext hash — that would break the
    /// deterministic AAD and make every shared item undecryptable.
    #[serde(with = "b64")]
    pub content_sha256: Vec<u8>,
    pub rev: u32,
    /// The OCK generation the ciphertext was sealed under.
    pub generation: u32,
}

/// Response: the item id + its assigned monotonic feed cursor `seq`.
#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PublishItemResponse {
    pub item_id: String,
    pub seq: u64,
}

/// One feed entry. A LIVE item carries `blobId` + `contentSha256`; a TOMBSTONE carries
/// `tombstoned:true` and no blob.
#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct OrgItemEntry {
    pub item_id: String,
    pub seq: u64,
    pub author_user_id: String,
    pub rev: u32,
    #[serde(default = "one")]
    pub generation: u32,
    pub created_at: String,
    pub tombstoned: bool,
    #[serde(default)]
    pub blob_id: Option<String>,
    #[serde(default, with = "b64::opt")]
    pub content_sha256: Option<Vec<u8>>,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct OrgItemsResponse {
    pub items: Vec<OrgItemEntry>,
    pub next_seq: u64,
}

/// Response to `{blobId}` from `POST /v1/blobs`.
#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct BlobCreatedResponse {
    pub blob_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A create request serializes with the exact camelCase key the server's `deny_unknown_fields`
    /// DTO expects.
    #[test]
    fn create_org_request_wire_shape() {
        let j = serde_json::to_string(&CreateOrgRequest {
            name: "Acme".into(),
        })
        .unwrap();
        assert_eq!(j, r#"{"name":"Acme"}"#);
    }

    /// A key-grant input's opaque bytes serialize as base64 under the camelCase keys the server reads.
    #[test]
    fn key_grant_input_wire_shape_is_base64_camelcase() {
        let g = KeyGrantInput {
            user_id: "u-1".into(),
            generation: 2,
            wrapped_key: vec![0xDE, 0xAD],
            grant_sig: vec![0xBE, 0xEF],
        };
        let v: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&g).unwrap()).unwrap();
        assert_eq!(v["userId"], "u-1");
        assert_eq!(v["generation"], 2);
        // base64url-unpadded of DE AD / BE EF.
        assert_eq!(v["wrappedKey"], murmur_protocol::b64::encode(&[0xDE, 0xAD]));
        assert_eq!(v["grantSig"], murmur_protocol::b64::encode(&[0xBE, 0xEF]));
    }

    /// A publish request round-trips its content_sha256 as base64.
    #[test]
    fn publish_item_request_wire_shape() {
        let r = PublishItemRequest {
            blob_id: None,
            content_cell: Some(vec![0xAB, 0xCD]),
            content_sha256: vec![1u8; 32],
            rev: 1,
            generation: 3,
        };
        let v: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert!(v.get("blobId").is_none());
        assert_eq!(v["contentCell"], murmur_protocol::b64::encode(&[0xAB, 0xCD]));
        assert_eq!(v["rev"], 1);
        assert_eq!(v["generation"], 3);
        assert_eq!(v["contentSha256"], murmur_protocol::b64::encode(&[1u8; 32]));
    }

    /// A feed response deserializes a LIVE item + a TOMBSTONE with the right optionality.
    #[test]
    fn feed_response_deserializes_live_and_tombstone() {
        let sha = murmur_protocol::b64::encode(&[2u8; 32]);
        let json = format!(
            r#"{{"items":[
                {{"itemId":"i1","seq":1,"authorUserId":"a","rev":1,"generation":1,
                  "createdAt":"2026-07-10T00:00:00Z","tombstoned":false,
                  "blobId":"b1","contentSha256":"{sha}"}},
                {{"itemId":"i2","seq":2,"authorUserId":"a","rev":1,"generation":1,
                  "createdAt":"2026-07-10T00:01:00Z","tombstoned":true}}
            ],"nextSeq":2}}"#
        );
        let resp: OrgItemsResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(resp.next_seq, 2);
        assert_eq!(resp.items.len(), 2);
        assert!(!resp.items[0].tombstoned);
        assert_eq!(resp.items[0].blob_id.as_deref(), Some("b1"));
        assert_eq!(resp.items[0].content_sha256, Some(vec![2u8; 32]));
        assert!(resp.items[1].tombstoned);
        assert!(resp.items[1].blob_id.is_none());
        assert!(resp.items[1].content_sha256.is_none());
    }

    /// A response DTO tolerates an UNKNOWN field (forward-compat: no `deny_unknown_fields`).
    #[test]
    fn org_response_tolerates_unknown_fields() {
        let json = r#"{"orgId":"o1","name":"Acme","role":"owner",
            "createdAt":"2026-07-10T00:00:00Z","currentGeneration":2,"futureField":123}"#;
        let r: OrgResponse = serde_json::from_str(json).unwrap();
        assert_eq!(r.org_id, "o1");
        assert_eq!(r.current_generation, 2);
    }

    /// An older server omitting `currentGeneration` defaults it to 1.
    #[test]
    fn org_response_defaults_generation_to_one() {
        let json =
            r#"{"orgId":"o1","name":"Acme","role":"member","createdAt":"2026-07-10T00:00:00Z"}"#;
        let r: OrgResponse = serde_json::from_str(json).unwrap();
        assert_eq!(r.current_generation, 1);
    }

    /// `GET /v1/orgs` deserializes a MIXED list — an org I own + an org I was invited to (member),
    /// one carrying a generation, one relying on the default. This is the membership-discovery shape.
    #[test]
    fn org_list_response_deserializes_owned_and_invited() {
        let json = r#"{"orgs":[
            {"orgId":"o-own","name":"Acme","role":"owner",
             "createdAt":"2026-07-10T00:00:00Z","currentGeneration":3},
            {"orgId":"o-inv","name":"Partner Co","role":"member",
             "createdAt":"2026-07-11T00:00:00Z"}
        ]}"#;
        let r: OrgListResponse = serde_json::from_str(json).unwrap();
        assert_eq!(r.orgs.len(), 2);
        assert_eq!(r.orgs[0].org_id, "o-own");
        assert_eq!(r.orgs[0].role, "owner");
        assert_eq!(r.orgs[0].current_generation, 3);
        assert_eq!(r.orgs[1].org_id, "o-inv");
        assert_eq!(r.orgs[1].role, "member");
        // Default generation for the invited org whose row omitted it.
        assert_eq!(r.orgs[1].current_generation, 1);
    }

    /// An empty membership list (a fresh account belonging to nothing) round-trips to an empty vec.
    #[test]
    fn org_list_response_deserializes_empty() {
        let r: OrgListResponse = serde_json::from_str(r#"{"orgs":[]}"#).unwrap();
        assert!(r.orgs.is_empty());
    }
}
