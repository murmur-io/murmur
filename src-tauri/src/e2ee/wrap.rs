//! §4.8 mode B (Murmur↔Murmur) crypto: HPKE-Base wrap of the note key `NK` to a recipient, plus a
//! detached Ed25519 signature over the canonical share-grant.
//!
//! HPKE Base mode has NO sender authentication, so the signature is load-bearing: the sender signs a
//! [`ShareGrantSignedView`] binding `{sender acct+gen, recipient acct+pk_enc, share_id, rev, hpke_enc,
//! ciphertext_hash}`, and the recipient MUST (spec §4.8, BINDING):
//!   1. reject any envelope WITHOUT a valid Ed25519 signature from the PINNED sender key — no unsigned
//!      path;
//!   2. verify the signature covers the exact grant fields above (so a valid sig can't be spliced onto
//!      swapped ciphertext or replayed A→B as A→C);
//!   3. check `recipient_acct == self` AND `sender_acct == locally-pinned sender` BEFORE any HPKE open.
//!
//! [`open_from_sender`] performs all three checks and fails closed (`AppError::Auth`) before it ever
//! decapsulates — an unsigned or mismatched envelope never reaches the AEAD.

use super::{ed25519_signature, map_hpke, to_arr32, Key32};
use crate::e2ee::keys::IdentityKeypair;
use crate::error::{AppError, Result};
use ed25519_dalek::{Signer, VerifyingKey};
use hpke::{
    aead::AesGcm256, kdf::HkdfSha256, kem::X25519HkdfSha256, Deserializable, Kem as KemTrait,
    OpModeR, OpModeS, Serializable,
};
use murmur_protocol::{aad, identity::ShareGrantSignedView};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

/// The HPKE suite fixed by spec §4.6: DHKEM(X25519, HKDF-SHA256) + HKDF-SHA256 + AES-256-GCM.
type SuiteKem = X25519HkdfSha256;
type SuiteKdf = HkdfSha256;
type SuiteAead = AesGcm256;

/// The output of sealing `NK` to a mode-B recipient.
pub struct SealedGrant {
    /// The HPKE encapsulated key (32 bytes for X25519).
    pub hpke_enc: Vec<u8>,
    /// `NK` HPKE-sealed to the recipient (the AEAD ciphertext).
    pub wrapped_nk: Vec<u8>,
    /// The sender's 64-byte detached Ed25519 signature over the canonical share-grant.
    pub signature: Vec<u8>,
}

/// The X25519 HPKE encapsulated-key length (fixed by DHKEM(X25519)). The opaque `wrapped_key` blob
/// frames a fixed 32-byte `pk_enc || pk_sig || hpke_enc` prefix; the remainder is `wrapped_nk`.
pub const HPKE_ENC_LEN: usize = 32;

/// Pack the sender's public identity + the sealed grant into the ONE opaque `wrapped_key` blob the
/// server relays (spec: the server treats it as an inert byte string, never inspected). Framing:
/// `[sender_pk_enc(32) || sender_pk_sig(32) || hpke_enc(32) || wrapped_nk]`.
///
/// The sender's public keys travel INSIDE so the recipient — whose inbox item carries only the
/// server-attested `sender_fingerprint`, not the raw keys — can (1) recompute the fingerprint and
/// require it EQUALS the server's attested value (anti-substitution binding) and (2) verify the
/// Ed25519 grant signature against `sender_pk_sig`. The `grant_sig` rides beside this blob.
pub fn pack_wrapped_key(
    sender_pk_enc: &[u8],
    sender_pk_sig: &[u8],
    grant: &SealedGrant,
) -> Result<Vec<u8>> {
    if sender_pk_enc.len() != 32
        || sender_pk_sig.len() != 32
        || grant.hpke_enc.len() != HPKE_ENC_LEN
    {
        return Err(AppError::InvalidArg(
            "wrapped-key framing expects 32-byte pk_enc/pk_sig/hpke_enc".into(),
        ));
    }
    let mut v = Vec::with_capacity(96 + grant.wrapped_nk.len());
    v.extend_from_slice(sender_pk_enc);
    v.extend_from_slice(sender_pk_sig);
    v.extend_from_slice(&grant.hpke_enc);
    v.extend_from_slice(&grant.wrapped_nk);
    Ok(v)
}

/// The recipient side of [`pack_wrapped_key`]: `sender_pk_enc`, `sender_pk_sig`, and a [`SealedGrant`]
/// reconstructed from the framed blob + the detached `grant_sig`. Malformed framing → `InvalidArg`
/// (fail closed, write NOTHING upstream).
pub struct UnpackedGrant {
    pub sender_pk_enc: [u8; 32],
    pub sender_pk_sig: [u8; 32],
    pub grant: SealedGrant,
}

/// Parse the opaque `wrapped_key` blob + the detached `grant_sig` back into the sender's public keys
/// and a [`SealedGrant`]. Requires at least the 96-byte fixed prefix.
pub fn unpack_wrapped_key(wrapped_key: &[u8], grant_sig: &[u8]) -> Result<UnpackedGrant> {
    if wrapped_key.len() < 96 {
        return Err(AppError::InvalidArg(
            "wrapped-key blob is too short (need ≥96-byte framed prefix)".into(),
        ));
    }
    let sender_pk_enc = to_arr32(&wrapped_key[0..32])?;
    let sender_pk_sig = to_arr32(&wrapped_key[32..64])?;
    let hpke_enc = wrapped_key[64..96].to_vec();
    let wrapped_nk = wrapped_key[96..].to_vec();
    Ok(UnpackedGrant {
        sender_pk_enc,
        sender_pk_sig,
        grant: SealedGrant {
            hpke_enc,
            wrapped_nk,
            signature: grant_sig.to_vec(),
        },
    })
}

/// Seal `NK` to `recipient_pk_enc` under HPKE-Base with `info = hpke_info(share_id)`, then sign the
/// canonical share-grant (binding the SHA-256 of the content cell `C`) with the sender's `sk_sig`.
///
/// `content_cell` is `C` (the note sealed under `NK`); only its hash is signed, so the grant binds the
/// exact ciphertext without copying it. HPKE per-message AAD is empty — the info string binds the
/// share_id and the Ed25519 signature binds everything else (`hpke_enc`, ciphertext hash, both party
/// identities, rev).
#[allow(clippy::too_many_arguments)]
pub fn seal_to_recipient(
    nk: &[u8; 32],
    content_cell: &[u8],
    recipient_pk_enc: &[u8],
    recipient_acct_id: &str,
    sender: &IdentityKeypair,
    sender_acct_id: &str,
    sender_generation: u32,
    share_id: &str,
    rev: u32,
) -> Result<SealedGrant> {
    let ct_hash = Sha256::digest(content_cell);
    seal_to_recipient_with_hash(
        nk,
        ct_hash.as_slice(),
        recipient_pk_enc,
        recipient_acct_id,
        sender,
        sender_acct_id,
        sender_generation,
        share_id,
        rev,
    )
}

/// Like [`seal_to_recipient`] but signs a PRECOMPUTED `content_hash` (`SHA-256(C)`) instead of
/// re-hashing the cell. Used by the on-launch re-wrap (`share_rewrap_pending`): the sender retains
/// only `NK` + `content_hash` locally, so it can produce a fresh grant to a newly-registered
/// recipient WITHOUT reading the meeting content again (no gate needed — only key material).
#[allow(clippy::too_many_arguments)]
pub fn seal_to_recipient_with_hash(
    nk: &[u8; 32],
    content_hash: &[u8],
    recipient_pk_enc: &[u8],
    recipient_acct_id: &str,
    sender: &IdentityKeypair,
    sender_acct_id: &str,
    sender_generation: u32,
    share_id: &str,
    rev: u32,
) -> Result<SealedGrant> {
    let pk = <SuiteKem as KemTrait>::PublicKey::from_bytes(recipient_pk_enc).map_err(map_hpke)?;
    let info = aad::hpke_info(share_id);
    let mut rng = rand::rngs::OsRng;
    let (encapped, ct) = hpke::single_shot_seal::<SuiteAead, SuiteKdf, SuiteKem, _>(
        &OpModeS::Base,
        &pk,
        info.as_bytes(),
        nk,
        b"",
        &mut rng,
    )
    .map_err(map_hpke)?;

    let hpke_enc = encapped.to_bytes().to_vec();
    let view = ShareGrantSignedView {
        sender_acct_id,
        sender_generation,
        recipient_acct_id,
        recipient_pk_enc,
        share_id,
        rev,
        hpke_enc: &hpke_enc,
        ciphertext_hash: content_hash,
    };
    let sig = sender.signing_key().sign(&view.canonical());

    Ok(SealedGrant {
        hpke_enc,
        wrapped_nk: ct,
        signature: sig.to_bytes().to_vec(),
    })
}

/// Recipient side (spec §4.8 accept rules): verify the grant's signature against the PINNED sender key
/// and check the account bindings BEFORE any HPKE open, then decapsulate `NK`. Rejects — hard fail,
/// `AppError::Auth` — any unsigned/forged/mismatched envelope. Returns `NK` on success.
///
/// `content_cell` is the recipient's copy of `C`; its hash MUST match the signed `ciphertext_hash`
/// (so a server can't hand the signed grant with a different content blob). `pinned_sender_pk_sig` /
/// `pinned_sender_acct_id` are the TOFU-pinned values the recipient stored on first contact.
#[allow(clippy::too_many_arguments)]
pub fn open_from_sender(
    grant: &SealedGrant,
    content_cell: &[u8],
    recipient: &IdentityKeypair,
    recipient_acct_id: &str,
    self_acct_id: &str,
    sender_acct_id: &str,
    sender_generation: u32,
    pinned_sender_acct_id: &str,
    pinned_sender_pk_sig: &[u8],
    share_id: &str,
    rev: u32,
) -> Result<Key32> {
    // (3) identity binding — the grant must be addressed to US, from the contact we pinned.
    if recipient_acct_id != self_acct_id {
        return Err(AppError::Auth(
            "share grant is addressed to a different account".into(),
        ));
    }
    if sender_acct_id != pinned_sender_acct_id {
        return Err(AppError::Auth(
            "share grant sender does not match the pinned contact".into(),
        ));
    }

    // (1)+(2) reconstruct the EXACT signed view (from our own pk_enc + our copy of C's hash) and
    // require a valid signature from the pinned sender key. No unsigned path exists.
    let ct_hash = Sha256::digest(content_cell);
    let view = ShareGrantSignedView {
        sender_acct_id,
        sender_generation,
        recipient_acct_id,
        recipient_pk_enc: &recipient.pk_enc,
        share_id,
        rev,
        hpke_enc: &grant.hpke_enc,
        ciphertext_hash: ct_hash.as_slice(),
    };
    let pinned = to_arr32(pinned_sender_pk_sig)?;
    let vk = VerifyingKey::from_bytes(&pinned)
        .map_err(|_| AppError::Auth("pinned sender key is invalid".into()))?;
    let sig = ed25519_signature(&grant.signature)?;
    vk.verify_strict(&view.canonical(), &sig).map_err(|_| {
        AppError::Auth(
            "share grant signature is invalid — rejecting unsigned/forged/tampered envelope".into(),
        )
    })?;

    // Only after every check passes do we HPKE-open.
    let sk =
        <SuiteKem as KemTrait>::PrivateKey::from_bytes(&*recipient.sk_enc).map_err(map_hpke)?;
    let enc = <SuiteKem as KemTrait>::EncappedKey::from_bytes(&grant.hpke_enc).map_err(map_hpke)?;
    let info = aad::hpke_info(share_id);
    let nk_pt = Zeroizing::new(
        hpke::single_shot_open::<SuiteAead, SuiteKdf, SuiteKem>(
            &OpModeR::Base,
            &sk,
            &enc,
            info.as_bytes(),
            &grant.wrapped_nk,
            b"",
        )
        .map_err(map_hpke)?,
    );
    if nk_pt.len() != 32 {
        return Err(AppError::Locked("unwrapped NK has the wrong length".into()));
    }
    let mut nk: Key32 = Zeroizing::new([0u8; 32]);
    nk.copy_from_slice(nk_pt.as_slice());
    Ok(nk)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::e2ee::keys::generate_identity;
    use crate::e2ee::{random_key32, seal_content};
    use murmur_protocol::envelope::ShareEnvelope;

    const SHARE_ID: &str = "share-B-1";
    const REV: u32 = 1;
    const ALICE: &str = "acct-alice";
    const BOB: &str = "acct-bob";
    const CAROL: &str = "acct-carol";

    /// Alice seals NK+content to Bob and Bob opens it, recovering NK and the note.
    #[test]
    fn mode_b_seal_open_round_trips() {
        let alice = generate_identity().unwrap();
        let bob = generate_identity().unwrap();
        let nk = random_key32().unwrap();
        let env = ShareEnvelope::new("Secret", "- for bob only", "2026-07-04T10:00:00Z");
        let content = seal_content(&nk, &env, SHARE_ID, REV).unwrap();

        let grant = seal_to_recipient(
            &nk,
            &content,
            &bob.pk_enc,
            BOB,
            &alice,
            ALICE,
            1,
            SHARE_ID,
            REV,
        )
        .unwrap();

        let opened_nk = open_from_sender(
            &grant,
            &content,
            &bob,
            BOB,
            BOB,
            ALICE,
            1,
            ALICE,
            &alice.pk_sig,
            SHARE_ID,
            REV,
        )
        .unwrap();
        assert_eq!(*opened_nk, *nk);
        // And NK actually decrypts the note.
        assert_eq!(
            crate::e2ee::open_content(&opened_nk, &content, SHARE_ID, REV).unwrap(),
            env
        );
    }

    /// The opaque `wrapped_key` blob round-trips: pack the sender's public keys + the sealed grant,
    /// unpack them, and the recipient opens NK exactly as from an unframed grant. Proves the framing
    /// carries the sender's identity so the recipient (who only gets a fingerprint from the server) can
    /// verify + open.
    #[test]
    fn wrapped_key_blob_round_trips_through_pack_unpack() {
        let alice = generate_identity().unwrap();
        let bob = generate_identity().unwrap();
        let nk = random_key32().unwrap();
        let env = ShareEnvelope::new("Secret", "- body", "2026-07-04T10:00:00Z");
        let content = seal_content(&nk, &env, SHARE_ID, REV).unwrap();
        let grant = seal_to_recipient(
            &nk,
            &content,
            &bob.pk_enc,
            BOB,
            &alice,
            ALICE,
            1,
            SHARE_ID,
            REV,
        )
        .unwrap();

        // Sender packs → server relays an opaque blob + grant_sig.
        let blob = pack_wrapped_key(&alice.pk_enc, &alice.pk_sig, &grant).unwrap();
        let up = unpack_wrapped_key(&blob, &grant.signature).unwrap();
        assert_eq!(up.sender_pk_enc, alice.pk_enc);
        assert_eq!(up.sender_pk_sig, alice.pk_sig);

        // The recipient opens NK from the UNPACKED grant, verifying against the unpacked sender key.
        let opened = open_from_sender(
            &up.grant,
            &content,
            &bob,
            BOB,
            BOB,
            ALICE,
            1,
            ALICE,
            &up.sender_pk_sig,
            SHARE_ID,
            REV,
        )
        .unwrap();
        assert_eq!(*opened, *nk);
        // A too-short blob fails closed.
        assert!(matches!(
            unpack_wrapped_key(&[0u8; 10], &grant.signature),
            Err(AppError::InvalidArg(_))
        ));
    }

    /// The retained-NK re-wrap path (`share_rewrap_pending`): a grant signed over a precomputed
    /// content hash opens identically to one signed over the cell — so the sender can re-wrap to a
    /// newly-registered recipient WITHOUT re-reading the meeting content.
    #[test]
    fn seal_with_hash_matches_seal_over_cell() {
        use sha2::{Digest, Sha256};
        let alice = generate_identity().unwrap();
        let bob = generate_identity().unwrap();
        let nk = random_key32().unwrap();
        let content = seal_content(
            &nk,
            &ShareEnvelope::new("t", "m", "2026-07-04T10:00:00Z"),
            SHARE_ID,
            REV,
        )
        .unwrap();
        let hash = Sha256::digest(&content);
        let grant = seal_to_recipient_with_hash(
            &nk,
            hash.as_slice(),
            &bob.pk_enc,
            BOB,
            &alice,
            ALICE,
            1,
            SHARE_ID,
            REV,
        )
        .unwrap();
        let opened = open_from_sender(
            &grant,
            &content,
            &bob,
            BOB,
            BOB,
            ALICE,
            1,
            ALICE,
            &alice.pk_sig,
            SHARE_ID,
            REV,
        )
        .unwrap();
        assert_eq!(*opened, *nk);
    }

    /// BINDING: a valid signature from an UNPINNED (wrong) key is rejected before any HPKE open.
    #[test]
    fn rejects_signature_from_unpinned_key() {
        let alice = generate_identity().unwrap();
        let attacker = generate_identity().unwrap();
        let bob = generate_identity().unwrap();
        let nk = random_key32().unwrap();
        let content = seal_content(
            &nk,
            &ShareEnvelope::new("t", "m", "2026-07-04T10:00:00Z"),
            SHARE_ID,
            REV,
        )
        .unwrap();

        // The attacker seals a perfectly-valid grant (signed with THEIR key) to Bob…
        let grant = seal_to_recipient(
            &nk,
            &content,
            &bob.pk_enc,
            BOB,
            &attacker,
            ALICE,
            1,
            SHARE_ID,
            REV,
        )
        .unwrap();
        // …but Bob has pinned ALICE's key. Verification against the pinned key fails closed.
        let res = open_from_sender(
            &grant,
            &content,
            &bob,
            BOB,
            BOB,
            ALICE,
            1,
            ALICE,
            &alice.pk_sig,
            SHARE_ID,
            REV,
        );
        assert!(matches!(res, Err(AppError::Auth(_))));
    }

    /// BINDING: an unsigned / zero-signature envelope is rejected (no unsigned path).
    #[test]
    fn rejects_unsigned_envelope() {
        let alice = generate_identity().unwrap();
        let bob = generate_identity().unwrap();
        let nk = random_key32().unwrap();
        let content = seal_content(
            &nk,
            &ShareEnvelope::new("t", "m", "2026-07-04T10:00:00Z"),
            SHARE_ID,
            REV,
        )
        .unwrap();
        let mut grant = seal_to_recipient(
            &nk,
            &content,
            &bob.pk_enc,
            BOB,
            &alice,
            ALICE,
            1,
            SHARE_ID,
            REV,
        )
        .unwrap();
        grant.signature = vec![0u8; 64]; // strip to an all-zero "unsigned" sig
        let res = open_from_sender(
            &grant,
            &content,
            &bob,
            BOB,
            BOB,
            ALICE,
            1,
            ALICE,
            &alice.pk_sig,
            SHARE_ID,
            REV,
        );
        assert!(matches!(res, Err(AppError::Auth(_))));
    }

    /// BINDING: a grant addressed to Bob cannot be replayed to Carol (recipient binding).
    #[test]
    fn rejects_replay_to_other_recipient() {
        let alice = generate_identity().unwrap();
        let bob = generate_identity().unwrap();
        let carol = generate_identity().unwrap();
        let nk = random_key32().unwrap();
        let content = seal_content(
            &nk,
            &ShareEnvelope::new("t", "m", "2026-07-04T10:00:00Z"),
            SHARE_ID,
            REV,
        )
        .unwrap();
        // Sealed to Bob's pk_enc, grant says recipient = Bob.
        let grant = seal_to_recipient(
            &nk,
            &content,
            &bob.pk_enc,
            BOB,
            &alice,
            ALICE,
            1,
            SHARE_ID,
            REV,
        )
        .unwrap();
        // Carol tries to accept it as herself: recipient_acct in the signed view was BOB, and Carol's
        // pk_enc differs → signature over her reconstructed view fails.
        let res = open_from_sender(
            &grant,
            &content,
            &carol,
            CAROL,
            CAROL,
            ALICE,
            1,
            ALICE,
            &alice.pk_sig,
            SHARE_ID,
            REV,
        );
        assert!(matches!(res, Err(AppError::Auth(_))));
    }

    /// BINDING: swapping the content cell for a different one breaks the ciphertext-hash binding.
    #[test]
    fn rejects_swapped_content_cell() {
        let alice = generate_identity().unwrap();
        let bob = generate_identity().unwrap();
        let nk = random_key32().unwrap();
        let real = seal_content(
            &nk,
            &ShareEnvelope::new("t", "real", "2026-07-04T10:00:00Z"),
            SHARE_ID,
            REV,
        )
        .unwrap();
        let grant = seal_to_recipient(
            &nk,
            &real,
            &bob.pk_enc,
            BOB,
            &alice,
            ALICE,
            1,
            SHARE_ID,
            REV,
        )
        .unwrap();
        // A different content cell (attacker-substituted) → hash mismatch → signature invalid.
        let evil = seal_content(
            &nk,
            &ShareEnvelope::new("t", "EVIL", "2026-07-04T10:00:00Z"),
            SHARE_ID,
            REV,
        )
        .unwrap();
        let res = open_from_sender(
            &grant,
            &evil,
            &bob,
            BOB,
            BOB,
            ALICE,
            1,
            ALICE,
            &alice.pk_sig,
            SHARE_ID,
            REV,
        );
        assert!(matches!(res, Err(AppError::Auth(_))));
    }

    /// BINDING: a generation-rollback (grant claims gen 1 but recipient checks gen 2) fails.
    #[test]
    fn rejects_generation_mismatch() {
        let alice = generate_identity().unwrap();
        let bob = generate_identity().unwrap();
        let nk = random_key32().unwrap();
        let content = seal_content(
            &nk,
            &ShareEnvelope::new("t", "m", "2026-07-04T10:00:00Z"),
            SHARE_ID,
            REV,
        )
        .unwrap();
        let grant = seal_to_recipient(
            &nk,
            &content,
            &bob.pk_enc,
            BOB,
            &alice,
            ALICE,
            1,
            SHARE_ID,
            REV,
        )
        .unwrap();
        // Recipient believes the sender is at generation 2 → reconstructed view differs → reject.
        let res = open_from_sender(
            &grant,
            &content,
            &bob,
            BOB,
            BOB,
            ALICE,
            2,
            ALICE,
            &alice.pk_sig,
            SHARE_ID,
            REV,
        );
        assert!(matches!(res, Err(AppError::Auth(_))));
    }
}
