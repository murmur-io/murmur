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

/// Parse one server-issued stable identity. Stable organization/document ids are UUIDs on the
/// relay; keeping this as the single parser authority prevents storage and link identities from
/// accepting different syntax. Callers choose the domain-specific error surface.
pub(crate) fn parse_stable_uuid(value: &str) -> Option<uuid::Uuid> {
    uuid::Uuid::parse_str(value).ok()
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

/// `POST /v1/orgs/{id}/generation {generation}` — bump the org's live OCK generation.
///
/// The body is NOT optional. The server extracts it with axum's `Json`, which rejects a request
/// carrying no `Content-Type: application/json` with **415 before the handler runs** — so a
/// body-less POST here never reached the owner check, let alone the rotation. That was the whole
/// of the dead-rotation defect: every member removal ended in a refusal the user could not act on,
/// and the org stayed on the generation the removed member still held a key for.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct BumpGenerationRequest {
    /// MUST equal the server's `current_generation + 1`; anything else is a content-free 409.
    pub generation: u32,
}

/// Response to a successful bump: the org's new live generation, as the server recorded it.
#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct BumpGenerationResponse {
    pub current_generation: u32,
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
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum OrgItemAccess {
    #[default]
    View,
    Edit,
}

impl OrgItemAccess {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::View => "view",
            Self::Edit => "edit",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "view" => Some(Self::View),
            "edit" => Some(Self::Edit),
            _ => None,
        }
    }
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PublishItemRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mutation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<OrgItemAccess>,
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
    pub doc_id: Option<String>,
    #[serde(default)]
    pub access: OrgItemAccess,
    pub document_owner_user_id: Option<String>,
}

impl PublishItemResponse {
    /// Validate the internally-consistent compatibility shape before durable metadata is admitted.
    /// Historical servers may omit both stable-document fields. Once `docId` is present, however,
    /// the stable owner is authorization metadata and MUST be present and non-blank; accepting a
    /// partial shape would let command code fall back to legacy author-based management.
    pub fn validate_document_metadata(&self) -> std::result::Result<(), &'static str> {
        match (
            self.doc_id.as_deref(),
            self.document_owner_user_id.as_deref(),
        ) {
            (None, None) => Ok(()),
            (Some(doc_id), _) if parse_stable_uuid(doc_id).is_none() => {
                Err("durable org document response carried invalid docId")
            }
            (Some(_), Some(owner)) if !owner.trim().is_empty() => Ok(()),
            (Some(_), _) => Err("durable org document response omitted its owner"),
            (None, Some(_)) => Err("legacy org item response carried an owner without a document"),
        }
    }
}

/// CAS replacement of the current encrypted document head.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct UpdateOrgItemRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mutation_id: Option<String>,
    pub expected_rev: u32,
    #[serde(with = "b64")]
    pub content_cell: Vec<u8>,
    #[serde(with = "b64")]
    pub content_sha256: Vec<u8>,
    pub generation: u32,
}

#[derive(Deserialize,Clone,Debug)]
#[serde(rename_all="camelCase")]
pub struct OrgDocumentMutationReceipt {
    pub mutation_id: String,
    pub org_id: String,
    pub doc_id: String,
    pub predecessor_item_id: Option<String>,
    pub predecessor_rev: Option<u32>,
    pub target_item_id: String,
    pub target_seq: u64,
    pub actor_user_id: String,
    pub document_owner_user_id: String,
    #[serde(default)]
    pub access: OrgItemAccess,
    pub rev: u32,
    pub generation: u32,
    #[serde(with="b64")]
    pub content_sha256: Vec<u8>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SetOrgItemAccessRequest {
    pub access: OrgItemAccess,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct OrgDocumentAccessResponse {
    pub doc_id: String,
    pub access: OrgItemAccess,
    pub document_owner_user_id: String,
}

impl OrgDocumentAccessResponse {
    pub fn validate_document_metadata(&self) -> std::result::Result<(), &'static str> {
        if parse_stable_uuid(&self.doc_id).is_none() {
            return Err("org document access response carried invalid docId");
        }
        if self.document_owner_user_id.trim().is_empty() {
            return Err("org document access response omitted its owner");
        }
        Ok(())
    }
}

/// One feed entry. A LIVE item carries `blobId` + `contentSha256`; a TOMBSTONE carries
/// `tombstoned:true` and no blob.
#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct OrgItemEntry {
    pub item_id: String,
    pub seq: u64,
    pub author_user_id: String,
    #[serde(default)]
    pub doc_id: Option<String>,
    #[serde(default)]
    pub access: OrgItemAccess,
    #[serde(default)]
    pub document_owner_user_id: Option<String>,
    /// Authoritative durable-resource head marker. `None` means an older/incompatible server omitted
    /// the field; it is deliberately distinct from `Some(false)`, which is a real demotion. Feed
    /// admission must validate the whole page before mutating a cursor or local head.
    #[serde(default)]
    pub is_current: Option<bool>,
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

impl OrgItemsResponse {
    /// Validate server-owned authorization/head metadata for the complete page. The caller must run
    /// this before any feed action or cursor mutation, so an older/partial server response preserves
    /// the last known-good local replica instead of silently demoting or re-authorizing it.
    pub fn validate_authoritative_metadata(&self) -> std::result::Result<(), &'static str> {
        self.items.iter().try_for_each(|item| {
            let Some(doc_id) = item.doc_id.as_deref() else {
                return Ok(());
            };
            if parse_stable_uuid(doc_id).is_none() {
                return Err("durable org feed item carried invalid docId");
            }
            if item.is_current.is_none() {
                return Err("durable org feed item omitted isCurrent");
            }
            match item.document_owner_user_id.as_deref() {
                Some(owner) if !owner.trim().is_empty() => Ok(()),
                _ => Err("durable org feed item omitted its owner"),
            }
        })
    }
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

    const DOC_ID: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

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
            mutation_id: None,
            doc_id: Some(DOC_ID.into()),
            access: Some(OrgItemAccess::Edit),
            blob_id: None,
            content_cell: Some(vec![0xAB, 0xCD]),
            content_sha256: vec![1u8; 32],
            rev: 1,
            generation: 3,
        };
        let v: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert!(v.get("blobId").is_none());
        assert_eq!(
            v["contentCell"],
            murmur_protocol::b64::encode(&[0xAB, 0xCD])
        );
        assert_eq!(v["rev"], 1);
        assert_eq!(v["generation"], 3);
        assert_eq!(v["docId"], DOC_ID);
        assert_eq!(v["access"], "edit");
        assert!(v.get("mutationId").is_none());
        assert_eq!(v["contentSha256"], murmur_protocol::b64::encode(&[1u8; 32]));
    }

    /// Omitting the new permission key is the historical wire shape. The pinned relay interprets
    /// it as the enum default (`view`), so old callers never gain edit access.
    #[test]
    fn publish_item_request_missing_access_is_least_privilege() {
        let request = PublishItemRequest {
            mutation_id: None,
            doc_id: Some(DOC_ID.into()),
            access: None,
            blob_id: None,
            content_cell: Some(vec![0xAB]),
            content_sha256: vec![1; 32],
            rev: 1,
            generation: 1,
        };
        let value = serde_json::to_value(request).unwrap();
        assert!(value.get("access").is_none());
        assert_eq!(OrgItemAccess::default(), OrgItemAccess::View);
    }

    #[test]
    fn publish_response_rejects_partial_stable_owner_metadata() {
        let legacy: PublishItemResponse =
            serde_json::from_str(r#"{"itemId":"i1","seq":1,"access":"view"}"#).unwrap();
        assert!(legacy.validate_document_metadata().is_ok());

        let complete: PublishItemResponse = serde_json::from_str(
            r#"{"itemId":"i2","seq":2,"docId":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","access":"edit",
                "documentOwnerUserId":"owner"}"#,
        )
        .unwrap();
        assert!(complete.validate_document_metadata().is_ok());

        let missing_owner: PublishItemResponse =
            serde_json::from_str(r#"{"itemId":"i3","seq":3,"docId":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","access":"view"}"#)
                .unwrap();
        assert_eq!(
            missing_owner.validate_document_metadata(),
            Err("durable org document response omitted its owner")
        );

        let malformed: PublishItemResponse = serde_json::from_str(
            r#"{"itemId":"i4","seq":4,"docId":"not-a-uuid","access":"view",
                "documentOwnerUserId":"owner"}"#,
        )
        .unwrap();
        assert_eq!(
            malformed.validate_document_metadata(),
            Err("durable org document response carried invalid docId")
        );
    }

    #[test]
    fn update_and_access_requests_match_stable_document_wire() {
        let update = UpdateOrgItemRequest {
            mutation_id: None,
            expected_rev: 7,
            content_cell: vec![0xAB],
            content_sha256: vec![3; 32],
            generation: 2,
        };
        let value = serde_json::to_value(update).unwrap();
        let keys: std::collections::BTreeSet<_> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            std::collections::BTreeSet::from([
                "contentCell",
                "contentSha256",
                "expectedRev",
                "generation",
            ])
        );
        assert_eq!(value["expectedRev"], 7);
        assert_eq!(value["contentCell"], murmur_protocol::b64::encode(&[0xAB]));
        assert_eq!(
            value["contentSha256"],
            murmur_protocol::b64::encode(&[3; 32])
        );
        assert_eq!(value["generation"], 2);
        assert!(value.get("mutationId").is_none());
        let access = serde_json::to_value(SetOrgItemAccessRequest {
            access: OrgItemAccess::View,
        })
        .unwrap();
        assert_eq!(access, serde_json::json!({"access": "view"}));
        assert!(serde_json::from_str::<OrgItemAccess>(r#""owner""#).is_err());

        let valid_access: OrgDocumentAccessResponse = serde_json::from_str(
            r#"{"docId":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","access":"edit",
                "documentOwnerUserId":"owner"}"#,
        )
        .unwrap();
        assert!(valid_access.validate_document_metadata().is_ok());
        let invalid_access: OrgDocumentAccessResponse = serde_json::from_str(
            r#"{"docId":"not-a-uuid","access":"view","documentOwnerUserId":"owner"}"#,
        )
        .unwrap();
        assert_eq!(
            invalid_access.validate_document_metadata(),
            Err("org document access response carried invalid docId")
        );
    }

    /// A feed response deserializes a LIVE item + a TOMBSTONE with the right optionality.
    #[test]
    fn feed_response_deserializes_live_and_tombstone() {
        let sha = murmur_protocol::b64::encode(&[2u8; 32]);
        let json = format!(
            r#"{{"items":[
                {{"itemId":"i1","seq":1,"authorUserId":"editor","docId":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
                  "access":"edit","documentOwnerUserId":"owner","isCurrent":true,
                  "rev":1,"generation":1,
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
        assert_eq!(resp.items[0].doc_id.as_deref(), Some(DOC_ID));
        assert_eq!(resp.items[0].access, OrgItemAccess::Edit);
        assert_eq!(
            resp.items[0].document_owner_user_id.as_deref(),
            Some("owner")
        );
        assert_eq!(resp.items[0].is_current, Some(true));
        assert!(resp.items[1].tombstoned);
        assert_eq!(resp.items[1].access, OrgItemAccess::View);
        assert_eq!(resp.items[1].is_current, None);
        assert!(resp.items[1].blob_id.is_none());
        assert!(resp.items[1].content_sha256.is_none());
        assert!(resp.validate_authoritative_metadata().is_ok());
    }

    /// Presence is security-significant: omission is unknown, while an explicit false is an
    /// authoritative demotion and must not be rewritten to a compatibility default.
    #[test]
    fn stable_feed_head_marker_distinguishes_absent_true_and_false() {
        fn item(marker: &str, owner: &str) -> OrgItemEntry {
            serde_json::from_str(&format!(
                r#"{{"itemId":"i1","seq":1,"authorUserId":"author","docId":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
                    "access":"view"{owner},"rev":1,"generation":1,
                    "createdAt":"2026-08-12T00:00:00Z","tombstoned":false{marker}}}"#
            ))
            .unwrap()
        }

        let absent = item("", r#","documentOwnerUserId":"owner""#);
        let current = item(r#","isCurrent":true"#, r#","documentOwnerUserId":"owner""#);
        let demoted = item(r#","isCurrent":false"#, r#","documentOwnerUserId":"owner""#);
        assert_eq!(absent.is_current, None);
        assert_eq!(current.is_current, Some(true));
        assert_eq!(demoted.is_current, Some(false));

        let absent_page = OrgItemsResponse {
            items: vec![absent],
            next_seq: 1,
        };
        assert_eq!(
            absent_page.validate_authoritative_metadata(),
            Err("durable org feed item omitted isCurrent")
        );

        for entry in [current, demoted] {
            let page = OrgItemsResponse {
                items: vec![entry],
                next_seq: 1,
            };
            assert!(page.validate_authoritative_metadata().is_ok());
        }
    }

    #[test]
    fn stable_feed_missing_owner_fails_page_but_legacy_item_remains_compatible() {
        let stable_without_owner: OrgItemsResponse = serde_json::from_str(
            r#"{"items":[{"itemId":"i1","seq":1,"authorUserId":"author","docId":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
                "access":"view","isCurrent":true,"rev":1,"generation":1,
                "createdAt":"2026-08-12T00:00:00Z","tombstoned":false}],"nextSeq":1}"#,
        )
        .unwrap();
        assert_eq!(
            stable_without_owner.validate_authoritative_metadata(),
            Err("durable org feed item omitted its owner")
        );

        let legacy: OrgItemsResponse = serde_json::from_str(
            r#"{"items":[{"itemId":"i0","seq":1,"authorUserId":"author","rev":1,
                "generation":1,"createdAt":"2026-08-12T00:00:00Z","tombstoned":false}],
                "nextSeq":1}"#,
        )
        .unwrap();
        assert_eq!(legacy.items[0].access, OrgItemAccess::View);
        assert_eq!(legacy.items[0].is_current, None);
        assert!(legacy.validate_authoritative_metadata().is_ok());

        let malformed: OrgItemsResponse = serde_json::from_str(
            r#"{"items":[{"itemId":"i2","seq":2,"authorUserId":"author",
                "docId":"not-a-uuid","access":"view","documentOwnerUserId":"owner",
                "isCurrent":true,"rev":1,"generation":1,
                "createdAt":"2026-08-12T00:00:00Z","tombstoned":false}],"nextSeq":2}"#,
        )
        .unwrap();
        assert_eq!(
            malformed.validate_authoritative_metadata(),
            Err("durable org feed item carried invalid docId")
        );
    }

    #[test]
    fn unknown_access_rejects_the_whole_feed_response() {
        let json = r#"{"items":[{"itemId":"i1","seq":1,"authorUserId":"author","docId":"d1",
            "access":"full","documentOwnerUserId":"owner","isCurrent":true,"rev":1,
            "generation":1,"createdAt":"2026-08-12T00:00:00Z","tombstoned":false}],
            "nextSeq":1}"#;
        assert!(serde_json::from_str::<OrgItemsResponse>(json).is_err());
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
