//! §4 key hierarchy: the account master key `MK`, its wraps (local KEK / `KEK_pw` / — in `recovery` —
//! `RK`), and the identity keypair (X25519 enc + Ed25519 sig) with a self-signed [`IdentityBundle`].
//!
//! `MK` is a RANDOM 32-byte key (Ente/Standard-Notes shape), NOT password-derived, so a password
//! change only re-wraps it (spec §4). Each wrap uses `murmur_protocol::cell` (AES-256-GCM) with the
//! UNIQUE per-slot AAD from `murmur_protocol::aad`, so a malicious server can't swap two ciphertexts
//! (e.g. `sk_enc`↔`sk_sig`) or roll the identity generation back — decrypt fails closed on the wrong
//! slot. The X25519 keypair is HPKE's own KEM keypair (so `pk_enc` is the standard 32-byte X25519
//! public key that the browser viewer / a future client agrees on); the Ed25519 keypair signs the
//! bundle and (in `wrap`) the share-grant.

use super::{ed25519_signature, map_hpke, map_proto, random_key32, to_arr32, unwrap_key32, Key32};
use crate::error::{AppError, Result};
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use hpke::{kem::X25519HkdfSha256, Deserializable, Kem as KemTrait, Serializable};
use murmur_protocol::{aad, cell, identity::IdentityBundle};
use zeroize::Zeroizing;

/// Generate a random 32-byte account master key `MK`.
pub fn generate_master_key() -> Result<Key32> {
    random_key32()
}

/// Derive `KEK_pw = HKDF-SHA256(export_key, info = "murmur:v1:mk-wrap")` from the OPAQUE
/// `export_key` (spec §3/§4.6). `KEK_pw` never leaves the device; it wraps `MK` for the server.
pub fn derive_kek_pw(export_key: &[u8]) -> Result<Key32> {
    super::hkdf_expand32(export_key, None, aad::HKDF_MK_WRAP.as_bytes())
}

// ─────────────────────────────── MK wraps (local + password) ───────────────────────────────
// (MK_wrap_rk / RK_wrap_mk live in `recovery`, keyed by the BIP39 recovery key.)

/// `MK_wrap_local`: seal `MK` under the Touch-ID-gated account KEK (AAD `mk-local|<acct_id>`).
pub fn wrap_mk_local(mk: &[u8; 32], kek_local: &[u8; 32], acct_id: &str) -> Result<Vec<u8>> {
    cell::seal(kek_local, mk, aad::mk_local(acct_id).as_bytes()).map_err(map_proto)
}

/// Unwrap `MK` from its local-KEK wrap.
pub fn unwrap_mk_local(wrapped: &[u8], kek_local: &[u8; 32], acct_id: &str) -> Result<Key32> {
    unwrap_key32(kek_local, wrapped, aad::mk_local(acct_id).as_bytes())
}

/// `MK_wrap_pw`: seal `MK` under `KEK_pw` (AAD `mk-pw|<acct_id>`) — stored server-side; a second Mac
/// re-derives `KEK_pw` from its password and unwraps this (spec §4.3).
pub fn wrap_mk_pw(mk: &[u8; 32], kek_pw: &[u8; 32], acct_id: &str) -> Result<Vec<u8>> {
    cell::seal(kek_pw, mk, aad::mk_pw(acct_id).as_bytes()).map_err(map_proto)
}

/// Unwrap `MK` from its `KEK_pw` wrap.
pub fn unwrap_mk_pw(wrapped: &[u8], kek_pw: &[u8; 32], acct_id: &str) -> Result<Key32> {
    unwrap_key32(kek_pw, wrapped, aad::mk_pw(acct_id).as_bytes())
}

// ─────────────────────────────── Identity keypair + bundle ───────────────────────────────

/// An account identity keypair for one generation: X25519 (HPKE enc) + Ed25519 (sig). Private key
/// bytes are zeroized on drop; public keys are the 32-byte wire encodings that go into the bundle.
pub struct IdentityKeypair {
    /// X25519 private key (HPKE KEM serialization, 32 bytes).
    pub(crate) sk_enc: Key32,
    /// X25519 public key (32 bytes).
    pub pk_enc: [u8; 32],
    /// Ed25519 signing-key seed (32 bytes).
    pub(crate) sk_sig: Key32,
    /// Ed25519 verifying key (32 bytes).
    pub pk_sig: [u8; 32],
}

impl IdentityKeypair {
    /// Reconstruct the Ed25519 signing key from the stored seed.
    pub(crate) fn signing_key(&self) -> SigningKey {
        SigningKey::from_bytes(&self.sk_sig)
    }
}

/// Generate a fresh identity keypair. The X25519 keypair is derived (RFC 9180 DeriveKeyPair) from
/// fresh random IKM; the Ed25519 keypair from a fresh random seed. Both use `getrandom` via
/// [`random_key32`], so no external RNG-trait plumbing is needed here.
pub fn generate_identity() -> Result<IdentityKeypair> {
    // X25519 (HPKE KEM): derive a keypair from random IKM, then serialize both halves to 32 bytes.
    let ikm = random_key32()?;
    let (sk, pk) = X25519HkdfSha256::derive_keypair(&*ikm);
    let sk_enc = Zeroizing::new(to_arr32(&sk.to_bytes())?);
    let pk_enc = to_arr32(&pk.to_bytes())?;

    // Ed25519: build a signing key from a random 32-byte seed and take its verifying key.
    let seed = random_key32()?;
    let signing = SigningKey::from_bytes(&seed);
    let pk_sig = signing.verifying_key().to_bytes();

    Ok(IdentityKeypair {
        sk_enc,
        pk_enc,
        sk_sig: seed,
        pk_sig,
    })
}

/// DETERMINISTICALLY derive the account identity keypair from `MK` (M5 enablement). The X25519 KEM
/// IKM and the Ed25519 seed are each `HKDF-SHA256(MK, info = "murmur:v1:id-{enc,sig}|<acct>|<gen>")`,
/// so the SAME identity keypair is reproducible from `MK` on any device / any login WITHOUT storing or
/// uploading the private keys (the server never sees them; only the self-signed public bundle is
/// published). This is what makes mode-B send (needs `sk_sig`) and accept (needs `sk_enc`) work after
/// a fresh login: `MK` is unwrapped from `mk_wrap_pw` and the identity is re-derived from it.
///
/// SECURITY: an `MK` compromise compromises the identity either way (the wraps are also under `MK`),
/// so deriving is no weaker than wrapping-and-storing — it just removes a storage/round-trip. The
/// per-`(acct, generation)` info string domain-separates generations (a future rotation bumps `gen`).
pub fn derive_identity(mk: &[u8; 32], acct_id: &str, generation: u32) -> Result<IdentityKeypair> {
    // X25519 (HPKE KEM): HKDF a 32-byte IKM from MK, then RFC 9180 DeriveKeyPair.
    let enc_info = format!("murmur:v1:id-enc|{acct_id}|{generation}");
    let ikm = super::hkdf_expand32(mk, None, enc_info.as_bytes())?;
    let (sk, pk) = X25519HkdfSha256::derive_keypair(&*ikm);
    let sk_enc = Zeroizing::new(to_arr32(&sk.to_bytes())?);
    let pk_enc = to_arr32(&pk.to_bytes())?;

    // Ed25519: HKDF a 32-byte seed from MK.
    let sig_info = format!("murmur:v1:id-sig|{acct_id}|{generation}");
    let seed = super::hkdf_expand32(mk, None, sig_info.as_bytes())?;
    let signing = SigningKey::from_bytes(&seed);
    let pk_sig = signing.verifying_key().to_bytes();

    Ok(IdentityKeypair {
        sk_enc,
        pk_enc,
        sk_sig: seed,
        pk_sig,
    })
}

/// Wrap BOTH identity private keys under `MK`, each with its generation-bound AAD
/// (`sk-enc|<acct>|<gen>` and `sk-sig|<acct>|<gen>`). Returns `(sk_enc_wrap, sk_sig_wrap)`.
pub fn wrap_identity(
    id: &IdentityKeypair,
    mk: &[u8; 32],
    acct_id: &str,
    generation: u32,
) -> Result<(Vec<u8>, Vec<u8>)> {
    let enc_wrap = cell::seal(mk, &*id.sk_enc, aad::sk_enc(acct_id, generation).as_bytes())
        .map_err(map_proto)?;
    let sig_wrap = cell::seal(mk, &*id.sk_sig, aad::sk_sig(acct_id, generation).as_bytes())
        .map_err(map_proto)?;
    Ok((enc_wrap, sig_wrap))
}

/// Unwrap both identity private keys from their `MK` wraps and recompute the matching public keys
/// (X25519 via HPKE `sk_to_pk`, Ed25519 via the seed's verifying key). Fails closed if either wrap
/// was sealed under a different generation/account (AAD mismatch).
pub fn unwrap_identity(
    sk_enc_wrap: &[u8],
    sk_sig_wrap: &[u8],
    mk: &[u8; 32],
    acct_id: &str,
    generation: u32,
) -> Result<IdentityKeypair> {
    let sk_enc = unwrap_key32(mk, sk_enc_wrap, aad::sk_enc(acct_id, generation).as_bytes())?;
    let sk_sig = unwrap_key32(mk, sk_sig_wrap, aad::sk_sig(acct_id, generation).as_bytes())?;

    let sk = <X25519HkdfSha256 as KemTrait>::PrivateKey::from_bytes(&*sk_enc).map_err(map_hpke)?;
    let pk_enc = to_arr32(&X25519HkdfSha256::sk_to_pk(&sk).to_bytes())?;
    let pk_sig = SigningKey::from_bytes(&sk_sig).verifying_key().to_bytes();

    Ok(IdentityKeypair {
        sk_enc,
        pk_enc,
        sk_sig,
        pk_sig,
    })
}

/// Build the self-signed [`IdentityBundle`] for `generation`: fill in the public keys, then sign the
/// bundle's canonical bytes with `sk_sig`. Returns `(bundle, self_signature)` (the 64-byte detached
/// Ed25519 sig, uploaded as `bundle_sig` in `PUT /keys`).
pub fn build_identity_bundle(
    id: &IdentityKeypair,
    acct_id: &str,
    generation: u32,
    created_at: &str,
) -> Result<(IdentityBundle, Vec<u8>)> {
    let bundle = IdentityBundle {
        acct_id: acct_id.to_string(),
        generation,
        pk_enc: id.pk_enc.to_vec(),
        pk_sig: id.pk_sig.to_vec(),
        created_at: created_at.to_string(),
    };
    let sig = id.signing_key().sign(&bundle.canonical());
    Ok((bundle, sig.to_bytes().to_vec()))
}

/// Verify a bundle's self-signature: the bundle's declared `pk_sig` must validate its canonical bytes
/// against `signature`. This is the TOFU-pin check a peer runs on a fetched bundle (spec §4.8) and
/// the integrity check on a re-read local bundle. Uses strict verification (rejects the malleable /
/// small-order edge cases).
pub fn verify_identity_bundle(bundle: &IdentityBundle, signature: &[u8]) -> Result<()> {
    let pk = to_arr32(&bundle.pk_sig)?;
    let vk = VerifyingKey::from_bytes(&pk)
        .map_err(|_| AppError::Auth("identity bundle has an invalid pk_sig".into()))?;
    let sig = ed25519_signature(signature)?;
    vk.verify_strict(&bundle.canonical(), &sig)
        .map_err(|_| AppError::Auth("identity bundle self-signature is invalid".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACCT: &str = "acct-123";

    #[test]
    fn mk_local_wrap_round_trips_and_is_context_bound() {
        let mk = generate_master_key().unwrap();
        let kek = crate::crypto::random_key().unwrap();
        let wrapped = wrap_mk_local(&mk, &kek, ACCT).unwrap();
        // Correct KEK + acct → identical MK back.
        assert_eq!(*unwrap_mk_local(&wrapped, &kek, ACCT).unwrap(), *mk);
        // Wrong account (AAD) fails closed.
        assert!(unwrap_mk_local(&wrapped, &kek, "other").is_err());
        // Wrong KEK fails closed.
        assert!(unwrap_mk_local(&wrapped, &crate::crypto::random_key().unwrap(), ACCT).is_err());
    }

    #[test]
    fn kek_pw_derivation_is_deterministic_and_wrap_round_trips() {
        let export_key = b"opaque-export-key-secret-material";
        let kek_pw_a = derive_kek_pw(export_key).unwrap();
        let kek_pw_b = derive_kek_pw(export_key).unwrap();
        assert_eq!(
            *kek_pw_a, *kek_pw_b,
            "KEK_pw is a pure function of export_key"
        );
        assert_ne!(
            *derive_kek_pw(b"different").unwrap(),
            *kek_pw_a,
            "a different export_key yields a different KEK_pw"
        );

        let mk = generate_master_key().unwrap();
        let wrapped = wrap_mk_pw(&mk, &kek_pw_a, ACCT).unwrap();
        assert_eq!(*unwrap_mk_pw(&wrapped, &kek_pw_a, ACCT).unwrap(), *mk);
    }

    #[test]
    fn cross_slot_wrong_aad_fails_closed() {
        // A blob sealed for the mk-pw slot must NOT decrypt as an mk-local slot even under the same
        // key (the server-swap defense).
        let mk = generate_master_key().unwrap();
        let key = crate::crypto::random_key().unwrap();
        let pw_wrap = wrap_mk_pw(&mk, &key, ACCT).unwrap();
        assert!(
            unwrap_mk_local(&pw_wrap, &key, ACCT).is_err(),
            "a mk-pw ciphertext must fail to open under the mk-local AAD"
        );
    }

    #[test]
    fn identity_keys_wrap_unwrap_and_derive_matching_pubkeys() {
        let mk = generate_master_key().unwrap();
        let id = generate_identity().unwrap();
        assert_eq!(id.pk_enc.len(), 32);
        assert_eq!(id.pk_sig.len(), 32);

        let (enc_wrap, sig_wrap) = wrap_identity(&id, &mk, ACCT, 1).unwrap();
        let back = unwrap_identity(&enc_wrap, &sig_wrap, &mk, ACCT, 1).unwrap();
        // Public keys recomputed from the unwrapped private keys match the originals.
        assert_eq!(back.pk_enc, id.pk_enc);
        assert_eq!(back.pk_sig, id.pk_sig);
        assert_eq!(*back.sk_enc, *id.sk_enc);
        assert_eq!(*back.sk_sig, *id.sk_sig);
    }

    #[test]
    fn identity_generation_rollback_fails_closed() {
        // Keys wrapped at generation 2 must NOT unwrap as generation 1 (rollback defense, §4.1).
        let mk = generate_master_key().unwrap();
        let id = generate_identity().unwrap();
        let (enc_wrap, sig_wrap) = wrap_identity(&id, &mk, ACCT, 2).unwrap();
        assert!(unwrap_identity(&enc_wrap, &sig_wrap, &mk, ACCT, 1).is_err());
    }

    #[test]
    fn derive_identity_is_deterministic_and_mk_bound() {
        // The SAME (MK, acct, gen) reproduces byte-identical private + public keys — this is what
        // lets a fresh login re-derive `sk_enc`/`sk_sig` to accept/send mode-B shares.
        let mk = generate_master_key().unwrap();
        let a = derive_identity(&mk, ACCT, 1).unwrap();
        let b = derive_identity(&mk, ACCT, 1).unwrap();
        assert_eq!(a.pk_enc, b.pk_enc);
        assert_eq!(a.pk_sig, b.pk_sig);
        assert_eq!(*a.sk_enc, *b.sk_enc);
        assert_eq!(*a.sk_sig, *b.sk_sig);
        // The derived X25519 public key matches sk_to_pk(sk_enc) (a valid HPKE keypair).
        let sk =
            <X25519HkdfSha256 as KemTrait>::PrivateKey::from_bytes(&*a.sk_enc).unwrap();
        assert_eq!(a.pk_enc, to_arr32(&X25519HkdfSha256::sk_to_pk(&sk).to_bytes()).unwrap());
        // A different generation, account, or MK yields a different identity (domain separation).
        assert_ne!(a.pk_sig, derive_identity(&mk, ACCT, 2).unwrap().pk_sig);
        assert_ne!(a.pk_sig, derive_identity(&mk, "other", 1).unwrap().pk_sig);
        assert_ne!(a.pk_sig, derive_identity(&generate_master_key().unwrap(), ACCT, 1).unwrap().pk_sig);
    }

    #[test]
    fn self_signed_bundle_verifies_and_rejects_tampering() {
        let id = generate_identity().unwrap();
        let (bundle, sig) = build_identity_bundle(&id, ACCT, 1, "2026-07-04T00:00:00Z").unwrap();
        assert_eq!(bundle.pk_enc, id.pk_enc.to_vec());
        // Valid self-signature verifies.
        verify_identity_bundle(&bundle, &sig).unwrap();

        // Tamper the generation → canonical bytes change → signature no longer valid.
        let mut forged = bundle.clone();
        forged.generation = 2;
        assert!(verify_identity_bundle(&forged, &sig).is_err());

        // A different identity's signature over the same bundle is rejected.
        let other = generate_identity().unwrap();
        let bad_sig = other
            .signing_key()
            .sign(&bundle.canonical())
            .to_bytes()
            .to_vec();
        assert!(verify_identity_bundle(&bundle, &bad_sig).is_err());
    }
}
