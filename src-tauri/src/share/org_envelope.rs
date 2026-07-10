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
use sha2::{Digest, Sha256};

/// The AAD domain prefix binding a sealed org item to `{org_id, item_nonce}`. Domain-separated from
/// every other AES-GCM slot in the app (per-folder `folder|meeting|...`, share cells, etc.).
const ORG_ITEM_AAD_V1: &str = "murmur-org/v1|org-item";

/// The envelope wire version. Bump only for a breaking canonical-format change (readers reject an
/// unknown version fail-closed).
pub const ORG_ENVELOPE_VERSION: u16 = 1;

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

/// The plaintext an org item carries (spec `OrgEnvelope v1`). `author_hint` is a display label only
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
}

impl OrgEnvelope {
    /// Build a v1 envelope. Callers pass the ALREADY-CLEANED + scrubbed markdown (the seal layer does
    /// no sanitization — that is the command's job, upstream, so the leak-safety transform is a
    /// single well-tested seam).
    pub fn new(
        kind: OrgItemKind,
        title: impl Into<String>,
        markdown: impl Into<String>,
        author_hint: impl Into<String>,
        created_at: impl Into<String>,
        source_rev: u32,
    ) -> Self {
        Self {
            version: ORG_ENVELOPE_VERSION,
            kind,
            title: title.into(),
            markdown: markdown.into(),
            author_hint: author_hint.into(),
            created_at: created_at.into(),
            source_rev,
        }
    }

    /// Serialize to the CANONICAL byte form: `version(u16 LE) | kind_tag(u8) | source_rev(u32 LE) |
    /// [len(u32 LE) || utf8]{title, markdown, author_hint, created_at}`. Fixed field order + explicit
    /// length prefixes ⇒ two devices produce byte-identical bytes ⇒ a stable `content_sha256`.
    pub fn to_canonical_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(
            2 + 1
                + 4
                + 16
                + self.title.len()
                + self.markdown.len()
                + self.author_hint.len()
                + self.created_at.len(),
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
        out
    }

    /// Parse the canonical byte form. Fails closed (`InvalidArg`) on a truncated/oversized/malformed
    /// buffer or an unknown version — never panics, never reads out of bounds.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self> {
        let mut cur = Cursor { b: bytes, i: 0 };
        let version = cur.take_u16()?;
        if version != ORG_ENVELOPE_VERSION {
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
        })
    }

    /// SHA-256 over the CANONICAL PLAINTEXT — the content-free dedup key stored in `org_shares` +
    /// used to relabel "I shared this myself" org feed hits. Stable across devices.
    pub fn content_sha256(&self) -> Vec<u8> {
        Sha256::digest(self.to_canonical_bytes()).to_vec()
    }
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
        )
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
}
