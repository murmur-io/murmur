//! `OrgEnvelope v1` — the canonical plaintext that gets sealed under the org content key (OCK) and
//! published to the org ciphertext feed (spec §"Trust model", "Envelope `OrgEnvelope v1`").
//!
//! This is the egress analogue of the per-folder seal: an org share is only published AFTER a local
//! `open()` of the freshly-sealed envelope round-trips byte-identical (the "verify-before-destroy"
//! discipline applied to egress — never upload a blob we can't decrypt back). The envelope is sealed
//! with AES-256-GCM (`crypto::encrypt`/`decrypt`) under the 32-byte OCK, bound to an explicit AAD
//! DOMAIN string (`org-item|<org_id>|<item_nonce>`) so a ciphertext can't be lifted between orgs or
//! replayed as a different item. `content_sha256` is the SHA-256 of the CANONICAL PLAINTEXT (used for
//! self-share dedup + the server's integrity check over the CIPHERTEXT is computed separately).
//!
//! Canonical serialization: a fixed field order, length-prefixed, versioned — so two devices produce
//! byte-identical bytes for the same logical envelope (required for a stable `content_sha256`). NO
//! PII in any error; the plaintext never reaches a log.

use crate::crypto;
use crate::error::{AppError, Result};
use murmur_protocol::envelope::ShareAttachment;
use sha2::{Digest, Sha256};

/// The AAD domain prefix binding a sealed org item to `{org_id, item_nonce}`. Domain-separated from
/// every other AES-GCM slot in the app (per-folder `folder|meeting|...`, share cells, etc.).
const ORG_ITEM_AAD_V1: &str = "murmur-org/v1|org-item";

/// The envelope wire version. Bump only for a breaking canonical-format change (readers reject an
/// unknown version fail-closed).
///
/// v1 → v2 appends `source_kind`; v2 → v3 appends a canonical attachment manifest. Readers retain
/// byte-identical parsing/re-serialization of already-published v1 and v2 envelopes.
pub const ORG_ENVELOPE_VERSION: u16 = 3;

/// The PREVIOUS wire version, still accepted on read for backward compatibility with envelopes
/// already published to org feeds before this device upgraded. See `ORG_ENVELOPE_VERSION` doc.
const ORG_ENVELOPE_VERSION_V1: u16 = 1;
const ORG_ENVELOPE_VERSION_V2: u16 = 2;

/// The kind of authored content an org item carries. Serialized as a single tag byte in the canonical
/// form (stable across versions).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrgItemKind {
    /// A meeting/note summary.
    Note,
    /// An explicit standalone summary (reserved for a future "summary" flow; kept distinct on the wire).
    Summary,
}

impl OrgItemKind {
    fn tag(self) -> u8 {
        match self {
            OrgItemKind::Note => 1,
            OrgItemKind::Summary => 2,
        }
    }
    fn from_tag(t: u8) -> Result<Self> {
        match t {
            1 => Ok(OrgItemKind::Note),
            2 => Ok(OrgItemKind::Summary),
            _ => Err(AppError::InvalidArg("unknown org item kind tag".into())),
        }
    }
    /// The lowercase wire label used in the local `org_shares.kind` column + FE DTOs.
    pub fn as_str(self) -> &'static str {
        match self {
            OrgItemKind::Note => "note",
            OrgItemKind::Summary => "summary",
        }
    }
}

/// The SOURCE type an org item was published from — a DIFFERENT axis from [`OrgItemKind`] (which is
/// content SHAPE, not source type; both `share_meeting_to_org` and `share_document_to_org` currently
/// stamp every envelope `OrgItemKind::Note` regardless of source). Serialized as a single tag byte,
/// present ONLY in a v2 envelope (v1 envelopes carry no source-type signal on the wire and parse to
/// `OrgEnvelope.source_kind: None`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrgSourceKind {
    /// Published from an authored standalone note/document.
    Document,
    /// Published from a meeting's note/summary.
    Meeting,
}

impl OrgSourceKind {
    fn tag(self) -> u8 {
        match self {
            OrgSourceKind::Document => 1,
            OrgSourceKind::Meeting => 2,
        }
    }
    fn from_tag(t: u8) -> Result<Self> {
        match t {
            1 => Ok(OrgSourceKind::Document),
            2 => Ok(OrgSourceKind::Meeting),
            _ => Err(AppError::InvalidArg("unknown org source kind tag".into())),
        }
    }
    /// The lowercase wire label used by storage + FE DTOs (mirrors `OrgItemHeader.kind`'s existing
    /// `"document"` / `"meeting"` strings from the owned-item resolver).
    pub fn as_str(self) -> &'static str {
        match self {
            OrgSourceKind::Document => "document",
            OrgSourceKind::Meeting => "meeting",
        }
    }
}

/// The plaintext an org item carries (spec `OrgEnvelope v1`/`v2`). `author_hint` is a display label only
/// (e.g. the author's email local-part / account short id) — it is deliberately NOT a note-content
/// string, and is what the connector formatter renders as `[org · <author_hint>]` provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgEnvelope {
    pub version: u16,
    pub kind: OrgItemKind,
    pub title: String,
    pub markdown: String,
    pub author_hint: String,
    pub created_at: String,
    /// The source revision of the local note/document at publish time (for later "update share").
    pub source_rev: u32,
    /// The SOURCE type (document vs meeting) this envelope was published from. `Some(..)` for every
    /// envelope THIS device constructs (`new()` always stamps v2 + a required source kind); `None`
    /// ONLY for an envelope PARSED from already-published v1 wire bytes, which carry no such signal.
    pub source_kind: Option<OrgSourceKind>,
    /// Image bytes authenticated inside the OCK-sealed v3 envelope. Empty for parsed v1/v2 data.
    pub attachments: Vec<ShareAttachment>,
}

impl OrgEnvelope {
    /// Build a text-only envelope in the v2 wire shape. v3 is selected only by
    /// [`Self::with_attachments`] when real image bytes exist, preserving mixed-version org sync for
    /// older clients that can still consume every text-only item. `source_kind: None` remains reserved
    /// for parsed v1 wire data. Callers pass already-cleaned + scrubbed markdown.
    pub fn new(
        kind: OrgItemKind,
        title: impl Into<String>,
        markdown: impl Into<String>,
        author_hint: impl Into<String>,
        created_at: impl Into<String>,
        source_rev: u32,
        source_kind: OrgSourceKind,
    ) -> Self {
        Self {
            version: ORG_ENVELOPE_VERSION_V2,
            kind,
            title: title.into(),
            markdown: markdown.into(),
            author_hint: author_hint.into(),
            created_at: created_at.into(),
            source_rev,
            source_kind: Some(source_kind),
            attachments: Vec::new(),
        }
    }

    /// Attach a deterministic manifest and upgrade to v3 only when it is non-empty. Calling this with
    /// an empty vector deliberately preserves v2 compatibility for text-only publish/republish paths.
    pub fn with_attachments(mut self, mut attachments: Vec<ShareAttachment>) -> Self {
        if attachments.is_empty() {
            self.attachments.clear();
            return self;
        }
        attachments.sort_by(|a, b| a.id.cmp(&b.id));
        self.version = ORG_ENVELOPE_VERSION;
        self.attachments = attachments;
        self
    }

    /// Serialize to the CANONICAL byte form. v1 layout: `version(u16 LE) | kind_tag(u8) |
    /// source_rev(u32 LE) | [len(u32 LE) || utf8]{title, markdown, author_hint, created_at}`. v2 layout
    /// APPENDS one `source_kind_tag(u8)` after `created_at` — nothing before it moves, so a v1-shaped
    /// value serialized here (i.e. `self.version == 1`, from a PARSED old envelope) reproduces the
    /// EXACT v1 bytes, never silently upgrading the wire form (that would shift `content_sha256` for
    /// existing dedup rows). Fixed field order + explicit length prefixes ⇒ two devices on the same
    /// version produce byte-identical bytes ⇒ a stable `content_sha256`.
    pub fn to_canonical_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(
            2 + 1
                + 4
                + 16
                + self.title.len()
                + self.markdown.len()
                + self.author_hint.len()
                + self.created_at.len()
                + 1
                + self
                    .attachments
                    .iter()
                    .map(|a| 28 + a.id.len() + a.mime_type.len() + a.sha256.len() + a.data.len())
                    .sum::<usize>(),
        );
        out.extend_from_slice(&self.version.to_le_bytes());
        out.push(self.kind.tag());
        out.extend_from_slice(&self.source_rev.to_le_bytes());
        for field in [
            self.title.as_str(),
            self.markdown.as_str(),
            self.author_hint.as_str(),
            self.created_at.as_str(),
        ] {
            out.extend_from_slice(&(field.len() as u32).to_le_bytes());
            out.extend_from_slice(field.as_bytes());
        }
        if self.version >= ORG_ENVELOPE_VERSION_V2 {
            // v2 and later append the source-kind tag. `new()`
            // guarantees `Some` here; a `None` on a v2-stamped value is an internal invariant violation
            // (should never happen — nothing constructs one), so fail closed rather than write garbage.
            let tag = self.source_kind.map(OrgSourceKind::tag).unwrap_or(0);
            out.push(tag);
        }
        if self.version >= ORG_ENVELOPE_VERSION {
            out.extend_from_slice(&(self.attachments.len() as u32).to_le_bytes());
            for attachment in &self.attachments {
                push_bytes(&mut out, attachment.id.as_bytes());
                push_bytes(&mut out, attachment.mime_type.as_bytes());
                out.extend_from_slice(&attachment.width.to_le_bytes());
                out.extend_from_slice(&attachment.height.to_le_bytes());
                push_bytes(&mut out, &attachment.sha256);
                push_bytes(&mut out, &attachment.data);
            }
        }
        out
    }

    /// Parse the canonical byte form. Fails closed (`InvalidArg`) on a truncated/oversized/malformed
    /// buffer or an unknown version — never panics, never reads out of bounds.
    ///
    /// BACKWARD COMPAT (binding): accepts BOTH `ORG_ENVELOPE_VERSION_V1` (already-published envelopes —
    /// parsed exactly as the original v1 layout, `source_kind: None`) and `ORG_ENVELOPE_VERSION` (v2 —
    /// the same 4 fields, then one extra source-kind tag byte). Any OTHER version fails closed exactly
    /// as before. Never reject a v1 envelope — real users have v1 bytes live in org feeds today.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self> {
        let mut cur = Cursor { b: bytes, i: 0 };
        let version = cur.take_u16()?;
        if version != ORG_ENVELOPE_VERSION_V1
            && version != ORG_ENVELOPE_VERSION_V2
            && version != ORG_ENVELOPE_VERSION
        {
            return Err(AppError::InvalidArg(format!(
                "unsupported org envelope version {version}"
            )));
        }
        let kind = OrgItemKind::from_tag(cur.take_u8()?)?;
        let source_rev = cur.take_u32()?;
        let title = cur.take_str()?;
        let markdown = cur.take_str()?;
        let author_hint = cur.take_str()?;
        let created_at = cur.take_str()?;
        let source_kind = if version >= ORG_ENVELOPE_VERSION_V2 {
            Some(OrgSourceKind::from_tag(cur.take_u8()?)?)
        } else {
            None
        };
        let attachments = if version >= ORG_ENVELOPE_VERSION {
            let count = cur.take_u32()? as usize;
            if count > murmur_protocol::caps::MAX_NOTE_ATTACHMENTS {
                return Err(AppError::InvalidArg(
                    "too many org envelope attachments".into(),
                ));
            }
            let mut attachments = Vec::with_capacity(count);
            let mut prior_id: Option<String> = None;
            for _ in 0..count {
                let id = cur.take_str()?;
                if prior_id.as_ref().is_some_and(|prior| prior >= &id) {
                    return Err(AppError::InvalidArg(
                        "org envelope attachment ids are not canonical".into(),
                    ));
                }
                let mime_type = cur.take_str()?;
                let width = cur.take_u32()?;
                let height = cur.take_u32()?;
                let sha256 = cur.take_vec()?;
                let data = cur.take_vec()?;
                prior_id = Some(id.clone());
                attachments.push(ShareAttachment {
                    id,
                    mime_type,
                    width,
                    height,
                    sha256,
                    data,
                });
            }
            attachments
        } else {
            Vec::new()
        };
        if cur.i != bytes.len() {
            return Err(AppError::InvalidArg(
                "trailing bytes after org envelope".into(),
            ));
        }
        Ok(Self {
            version,
            kind,
            title,
            markdown,
            author_hint,
            created_at,
            source_rev,
            source_kind,
            attachments,
        })
    }

    /// SHA-256 over the CANONICAL PLAINTEXT — the content-free dedup key stored in `org_shares` +
    /// used to relabel "I shared this myself" org feed hits. Stable across devices.
    pub fn content_sha256(&self) -> Vec<u8> {
        Sha256::digest(self.to_canonical_bytes()).to_vec()
    }
}

fn push_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(bytes);
}

/// The AAD domain string binding a sealed item to its org + a per-item nonce (spec: `org-item` +
/// org_id + item nonce). The `item_nonce` is any per-publish unique string the caller controls (we
/// use the local `org_shares.id` UUID so a re-publish of the same content under a new row gets a
/// fresh binding).
pub fn org_item_aad(org_id: &str, item_nonce: &str) -> String {
    format!("{ORG_ITEM_AAD_V1}|{org_id}|{item_nonce}")
}

/// Seal an [`OrgEnvelope`] under the 32-byte OCK, bound to `org_item_aad(org_id, item_nonce)`, and
/// VERIFY it opens back byte-identical BEFORE returning the ciphertext (the egress verify-before-
/// destroy: the caller uploads ONLY a blob it just proved it can decrypt). Returns
/// `(ciphertext, content_sha256_of_plaintext)`.
pub fn seal_org_envelope(
    ock: &[u8; 32],
    env: &OrgEnvelope,
    org_id: &str,
    item_nonce: &str,
) -> Result<(Vec<u8>, Vec<u8>)> {
    let plaintext = env.to_canonical_bytes();
    let aad = org_item_aad(org_id, item_nonce);
    let ciphertext = crypto::encrypt(ock, &plaintext, aad.as_bytes())?;
    // Verify-before-egress: decrypt back under the SAME AAD and require byte-identical plaintext,
    // then re-parse the envelope (so a corrupt/short cell is caught before it ever leaves the device).
    let check = crypto::decrypt(ock, &ciphertext, aad.as_bytes())?;
    if check != plaintext {
        return Err(AppError::Storage(
            "org envelope seal verification failed (decrypted mismatch)".into(),
        ));
    }
    let reparsed = OrgEnvelope::from_canonical_bytes(&check)?;
    if &reparsed != env {
        return Err(AppError::Storage(
            "org envelope seal verification failed (reparsed mismatch)".into(),
        ));
    }
    let sha = env.content_sha256();
    Ok((ciphertext, sha))
}

/// Open a sealed org-item ciphertext under the OCK, expecting `org_item_aad(org_id, item_nonce)`.
/// Fails closed (`AppError::Locked`) on a wrong OCK / tampered cell / wrong org|item AAD.
pub fn open_org_envelope(
    ock: &[u8; 32],
    ciphertext: &[u8],
    org_id: &str,
    item_nonce: &str,
) -> Result<OrgEnvelope> {
    let aad = org_item_aad(org_id, item_nonce);
    let plaintext = crypto::decrypt(ock, ciphertext, aad.as_bytes())?;
    OrgEnvelope::from_canonical_bytes(&plaintext)
}

/// A tiny bounds-checked reader for [`OrgEnvelope::from_canonical_bytes`] — never panics.
struct Cursor<'a> {
    b: &'a [u8],
    i: usize,
}

impl Cursor<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8]> {
        let end = self
            .i
            .checked_add(n)
            .ok_or_else(|| AppError::InvalidArg("org envelope length overflow".into()))?;
        if end > self.b.len() {
            return Err(AppError::InvalidArg("org envelope truncated".into()));
        }
        let s = &self.b[self.i..end];
        self.i = end;
        Ok(s)
    }
    fn take_u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn take_u16(&mut self) -> Result<u16> {
        let s = self.take(2)?;
        Ok(u16::from_le_bytes([s[0], s[1]]))
    }
    fn take_u32(&mut self) -> Result<u32> {
        let s = self.take(4)?;
        Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn take_str(&mut self) -> Result<String> {
        let len = self.take_u32()? as usize;
        // Bound a single field to a sane ceiling so a malformed length can't request a huge alloc.
        const MAX_FIELD: usize = 8 * 1024 * 1024;
        if len > MAX_FIELD {
            return Err(AppError::InvalidArg("org envelope field too large".into()));
        }
        let s = self.take(len)?;
        String::from_utf8(s.to_vec())
            .map_err(|_| AppError::InvalidArg("org envelope field is not valid UTF-8".into()))
    }
    fn take_vec(&mut self) -> Result<Vec<u8>> {
        let len = self.take_u32()? as usize;
        if len > murmur_protocol::caps::MAX_NOTE_BUNDLE_BYTES {
            return Err(AppError::InvalidArg(
                "org envelope byte field too large".into(),
            ));
        }
        Ok(self.take(len)?.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> OrgEnvelope {
        OrgEnvelope::new(
            OrgItemKind::Note,
            "Weekly Sync",
            "- decided on the roadmap\n- Anna owns follow-up",
            "anna",
            "2026-07-10T10:00:00Z",
            3,
            OrgSourceKind::Meeting,
        )
    }

    /// Hand-build a byte buffer in the OLD v1 wire layout (no trailing `source_kind` byte) — the exact
    /// shape of an envelope already published to a real org feed before this device shipped v2. Mirrors
    /// the manual byte-construction style of `malformed_canonical_bytes_fail_closed_no_panic`.
    fn v1_bytes(
        title: &str,
        markdown: &str,
        author_hint: &str,
        created_at: &str,
        source_rev: u32,
    ) -> Vec<u8> {
        let mut out = ORG_ENVELOPE_VERSION_V1.to_le_bytes().to_vec();
        out.push(OrgItemKind::Note.tag());
        out.extend_from_slice(&source_rev.to_le_bytes());
        for field in [title, markdown, author_hint, created_at] {
            out.extend_from_slice(&(field.len() as u32).to_le_bytes());
            out.extend_from_slice(field.as_bytes());
        }
        out
    }

    fn v2_bytes(
        title: &str,
        markdown: &str,
        author_hint: &str,
        created_at: &str,
        source_rev: u32,
        source_kind: OrgSourceKind,
    ) -> Vec<u8> {
        let mut out = v1_bytes(title, markdown, author_hint, created_at, source_rev);
        out[0..2].copy_from_slice(&ORG_ENVELOPE_VERSION_V2.to_le_bytes());
        out.push(source_kind.tag());
        out
    }

    #[test]
    fn canonical_round_trips_byte_identical() {
        let e = env();
        let bytes = e.to_canonical_bytes();
        let back = OrgEnvelope::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(back, e);
        // Re-serializing the parsed value yields identical bytes (stable canonical form).
        assert_eq!(back.to_canonical_bytes(), bytes);
    }

    /// RED-BEFORE-GREEN (the load-bearing backward-compat regression): a byte buffer in the OLD v1
    /// wire layout — no trailing `source_kind` tag byte, exactly what is sitting in real users' org
    /// feeds today — MUST still parse successfully, yield `source_kind: None` (unclassified, not a
    /// guess), and re-serialize back to the EXACT SAME v1 bytes (so `content_sha256`/dedup for
    /// already-published v1 items never shifts). A naive "just require the new field always" fix fails
    /// this test (either by rejecting the version-1 buffer outright, or by upgrading it to v2 bytes on
    /// re-serialize); only correct version-branching in both `from_canonical_bytes` and
    /// `to_canonical_bytes` passes.
    #[test]
    fn v1_wire_bytes_still_parse_and_reserialize_byte_identical() {
        let old = v1_bytes(
            "Weekly Sync",
            "- decided on the roadmap\n- Anna owns follow-up",
            "anna",
            "2026-07-10T10:00:00Z",
            3,
        );
        let parsed = OrgEnvelope::from_canonical_bytes(&old)
            .expect("an already-published v1 envelope must remain parseable forever");
        assert_eq!(parsed.version, ORG_ENVELOPE_VERSION_V1);
        assert_eq!(
            parsed.source_kind, None,
            "a v1 envelope carries no source-type signal on the wire — unclassified, never guessed"
        );
        assert_eq!(parsed.title, "Weekly Sync");
        assert_eq!(parsed.kind, OrgItemKind::Note);
        // Re-serializing a PARSED v1 value must reproduce the EXACT v1 bytes — no silent upgrade to
        // the v2 layout (that would shift content_sha256 for existing dedup rows).
        assert_eq!(
            parsed.to_canonical_bytes(),
            old,
            "re-serializing a parsed v1 envelope must not mutate the wire format of old data"
        );
    }

    #[test]
    fn v2_wire_bytes_still_parse_and_reserialize_byte_identical() {
        let old = v2_bytes(
            "Doc",
            "body",
            "anna",
            "2026-07-11T09:00:00Z",
            7,
            OrgSourceKind::Document,
        );
        let parsed = OrgEnvelope::from_canonical_bytes(&old).unwrap();
        assert_eq!(parsed.version, ORG_ENVELOPE_VERSION_V2);
        assert_eq!(parsed.source_kind, Some(OrgSourceKind::Document));
        assert!(parsed.attachments.is_empty());
        assert_eq!(parsed.to_canonical_bytes(), old);
    }

    /// A fresh text-only v2 envelope carries `source_kind` and round-trips both source kinds.
    #[test]
    fn text_only_v2_canonical_round_trips_both_source_kinds() {
        for sk in [OrgSourceKind::Meeting, OrgSourceKind::Document] {
            let e = OrgEnvelope::new(
                OrgItemKind::Note,
                "Standup",
                "body",
                "anna",
                "2026-07-11T09:00:00Z",
                1,
                sk,
            );
            assert_eq!(e.version, ORG_ENVELOPE_VERSION_V2);
            let bytes = e.to_canonical_bytes();
            let back = OrgEnvelope::from_canonical_bytes(&bytes).unwrap();
            assert_eq!(back, e);
            assert_eq!(back.source_kind, Some(sk));
            // Stable re-serialization for v2 too.
            assert_eq!(back.to_canonical_bytes(), bytes);
        }
    }

    #[test]
    fn empty_attachment_builder_keeps_text_only_v2_compatible() {
        let e = env().with_attachments(Vec::new());
        assert_eq!(e.version, ORG_ENVELOPE_VERSION_V2);
        assert!(e.attachments.is_empty());
        let back = OrgEnvelope::from_canonical_bytes(&e.to_canonical_bytes()).unwrap();
        assert_eq!(back, e);
    }

    #[test]
    fn v3_attachment_manifest_is_sorted_and_round_trips() {
        let attachment = |id: &str, byte: u8| ShareAttachment {
            id: id.into(),
            mime_type: "image/webp".into(),
            width: 640,
            height: 480,
            sha256: vec![byte; 32],
            data: vec![byte; 16],
        };
        let e = env().with_attachments(vec![
            attachment("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb", 2),
            attachment("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", 1),
        ]);
        assert_eq!(e.version, ORG_ENVELOPE_VERSION);
        assert_eq!(e.attachments[0].id, "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa");
        let bytes = e.to_canonical_bytes();
        let back = OrgEnvelope::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(back, e);
        assert_eq!(back.to_canonical_bytes(), bytes);
    }

    #[test]
    fn content_sha256_is_stable_and_plaintext_derived() {
        let e = env();
        let a = e.content_sha256();
        let b = OrgEnvelope::from_canonical_bytes(&e.to_canonical_bytes())
            .unwrap()
            .content_sha256();
        assert_eq!(a, b, "same logical envelope ⇒ same content hash");
        assert_eq!(a.len(), 32);
    }

    #[test]
    fn seal_open_round_trips_and_verifies_before_egress() {
        let ock = crypto::random_key().unwrap();
        let e = env();
        let (ct, sha) = seal_org_envelope(&ock, &e, "org-1", "item-1").unwrap();
        // The ciphertext must NOT contain the plaintext markdown (real seal, not a copy).
        let needle = e.markdown.as_bytes();
        assert!(
            !ct.windows(needle.len()).any(|w| w == needle),
            "ciphertext leaks the plaintext markdown"
        );
        // The returned hash is over the PLAINTEXT (matches a direct compute).
        assert_eq!(sha, e.content_sha256());
        // Opening under the same org+item recovers the exact envelope.
        let opened = open_org_envelope(&ock, &ct, "org-1", "item-1").unwrap();
        assert_eq!(opened, e);
    }

    #[test]
    fn wrong_ock_fails_closed() {
        let ock = crypto::random_key().unwrap();
        let (ct, _) = seal_org_envelope(&ock, &env(), "org-1", "item-1").unwrap();
        let other = crypto::random_key().unwrap();
        assert!(matches!(
            open_org_envelope(&other, &ct, "org-1", "item-1"),
            Err(AppError::Locked(_))
        ));
    }

    #[test]
    fn wrong_org_or_item_aad_fails_closed() {
        let ock = crypto::random_key().unwrap();
        let (ct, _) = seal_org_envelope(&ock, &env(), "org-1", "item-1").unwrap();
        // A ciphertext sealed for org-1 must not open as org-2 (cross-org lift blocked).
        assert!(open_org_envelope(&ock, &ct, "org-2", "item-1").is_err());
        // …nor as a different item within the same org (replay blocked).
        assert!(open_org_envelope(&ock, &ct, "org-1", "item-9").is_err());
    }

    #[test]
    fn tampered_ciphertext_fails_closed() {
        let ock = crypto::random_key().unwrap();
        let (mut ct, _) = seal_org_envelope(&ock, &env(), "org-1", "item-1").unwrap();
        let last = ct.len() - 1;
        ct[last] ^= 0xff;
        assert!(open_org_envelope(&ock, &ct, "org-1", "item-1").is_err());
    }

    #[test]
    fn malformed_canonical_bytes_fail_closed_no_panic() {
        // Empty, truncated, and unknown-version buffers all error rather than panic.
        assert!(OrgEnvelope::from_canonical_bytes(&[]).is_err());
        assert!(OrgEnvelope::from_canonical_bytes(&[1, 0, 1]).is_err()); // version ok, then truncated
        assert!(OrgEnvelope::from_canonical_bytes(&[9, 9, 1]).is_err()); // unknown version
        // A length prefix that overruns the buffer.
        let mut bad = 1u16.to_le_bytes().to_vec();
        bad.push(OrgItemKind::Note.tag());
        bad.extend_from_slice(&0u32.to_le_bytes()); // source_rev
        bad.extend_from_slice(&(9999u32).to_le_bytes()); // title len way past EOF
        assert!(OrgEnvelope::from_canonical_bytes(&bad).is_err());

        // A v2 buffer missing its trailing source_kind tag byte (truncated exactly at the v1/v2
        // boundary) must fail closed, not silently parse as v1.
        let v2_missing_tag = v1_bytes("t", "m", "a", "c", 1); // v1-shaped bytes...
        let mut v2_claimed = v2_missing_tag.clone();
        v2_claimed[0] = ORG_ENVELOPE_VERSION_V2.to_le_bytes()[0]; // ...but claims version 2
        v2_claimed[1] = ORG_ENVELOPE_VERSION_V2.to_le_bytes()[1];
        assert!(OrgEnvelope::from_canonical_bytes(&v2_claimed).is_err());

        // A v2 buffer with an unknown source_kind tag byte fails closed.
        let mut v2_bad_tag = v1_bytes("t", "m", "a", "c", 1);
        v2_bad_tag[0] = ORG_ENVELOPE_VERSION_V2.to_le_bytes()[0];
        v2_bad_tag[1] = ORG_ENVELOPE_VERSION_V2.to_le_bytes()[1];
        v2_bad_tag.push(0); // 0 is not a valid OrgSourceKind tag
        assert!(OrgEnvelope::from_canonical_bytes(&v2_bad_tag).is_err());
    }

    #[test]
    fn kind_tag_round_trips_and_labels() {
        assert_eq!(OrgItemKind::from_tag(OrgItemKind::Note.tag()).unwrap(), OrgItemKind::Note);
        assert_eq!(
            OrgItemKind::from_tag(OrgItemKind::Summary.tag()).unwrap(),
            OrgItemKind::Summary
        );
        assert!(OrgItemKind::from_tag(0).is_err());
        assert_eq!(OrgItemKind::Note.as_str(), "note");
        assert_eq!(OrgItemKind::Summary.as_str(), "summary");
    }

    #[test]
    fn source_kind_tag_round_trips_and_labels() {
        assert_eq!(
            OrgSourceKind::from_tag(OrgSourceKind::Document.tag()).unwrap(),
            OrgSourceKind::Document
        );
        assert_eq!(
            OrgSourceKind::from_tag(OrgSourceKind::Meeting.tag()).unwrap(),
            OrgSourceKind::Meeting
        );
        assert!(OrgSourceKind::from_tag(0).is_err());
        assert_eq!(OrgSourceKind::Document.as_str(), "document");
        assert_eq!(OrgSourceKind::Meeting.as_str(), "meeting");
    }

    /// The seal/open AEAD layer round-trips a text-only v2 envelope end to end (models
    /// `seal_open_round_trips_and_verifies_before_egress` for the source-kind field).
    #[test]
    fn seal_open_round_trips_text_only_v2_envelope_with_source_kind() {
        let ock = crypto::random_key().unwrap();
        let e = OrgEnvelope::new(
            OrgItemKind::Note,
            "Doc share",
            "authored note body",
            "bob",
            "2026-07-11T12:00:00Z",
            1,
            OrgSourceKind::Document,
        );
        let (ct, sha) = seal_org_envelope(&ock, &e, "org-1", "item-2").unwrap();
        assert_eq!(sha, e.content_sha256());
        let opened = open_org_envelope(&ock, &ct, "org-1", "item-2").unwrap();
        assert_eq!(opened, e);
        assert_eq!(opened.source_kind, Some(OrgSourceKind::Document));
    }
}
