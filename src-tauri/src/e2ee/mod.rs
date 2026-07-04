//! Client-side E2EE crypto core for Murmur's sharing feature (Milestone M2, spec §4).
//!
//! This module is the PURE crypto layer for account-scoped, zero-knowledge sharing. It owns nothing
//! about Tauri commands, the DB, or the network — those land in later steps (`share/`, `commands.rs`)
//! once the server endpoints exist. Here we build ONLY:
//!
//! - the account key hierarchy (`keys`): a random master key `MK`, its three wraps (local KEK,
//!   password-derived `KEK_pw`, recovery key `RK`), and the X25519+Ed25519 identity keypair with a
//!   self-signed [`IdentityBundle`];
//! - the recovery layer (`recovery`): a 24-word BIP39 phrase encoding `RK`, and the mutual
//!   `MK↔RK` wrapping so the phrase can be re-shown while logged in;
//! - mode A link-shares (`link`): `KEK_link` derivation (optionally password-hardened with Argon2id),
//!   the `L`-derived server fetch gate secret, and seal/open of a link share;
//! - mode B user↔user shares (`wrap`): HPKE-Base wrap of the note key `NK` to a recipient plus a
//!   detached Ed25519 signature over the canonical share-grant, with a fail-closed accept path.
//!
//! ALL wire formats (AES-256-GCM `nonce||ct||tag` cells, the per-slot AAD strings, canonical
//! serialization, the inner [`ShareEnvelope`]) are reused from the shared `murmur-protocol` crate —
//! this module never re-implements a format. Every fallible fn returns [`crate::error::Result`]; no
//! `unwrap`/`expect` in non-test code; no key/plaintext ever logged (rules §1/§8).
//!
//! ## Algorithms (spec §4.6, fixed)
//! AES-256-GCM (content + wraps), HKDF-SHA256 (`KEK_pw`/`KEK_link`/gate), Argon2id m=64 MiB/t=3/p=1
//! (link password), HPKE RFC 9180 Base DHKEM(X25519,HKDF-SHA256)+HKDF-SHA256+AES-256-GCM + detached
//! Ed25519, BIP39/24 words. Every GCM nonce is a fresh 96-bit `getrandom` value (in `cell::seal`).

pub mod keys;
pub mod link;
pub mod recovery;
pub mod wrap;

use crate::error::{AppError, Result};
use murmur_protocol::envelope::ShareEnvelope;
use zeroize::Zeroizing;

/// A 32-byte secret key held in memory that is zeroized on drop (MK, KEKs, NK, RK, L, sk_*).
pub type Key32 = Zeroizing<[u8; 32]>;

/// Generate a fresh random 32-byte key (zeroizes on drop).
pub fn random_key32() -> Result<Key32> {
    let mut k: Key32 = Zeroizing::new([0u8; 32]);
    getrandom::getrandom(&mut *k).map_err(|e| AppError::Secrets(format!("E2EE key RNG: {e}")))?;
    Ok(k)
}

/// Map a `murmur_protocol` codec error into the app error domain. A decrypt/AEAD-auth failure
/// (wrong key, tampered blob, wrong AAD-context) fails CLOSED as [`AppError::Locked`], mirroring the
/// per-folder `crypto.rs` semantics; malformed input is [`AppError::InvalidArg`]. Carries no
/// plaintext or key material.
pub(crate) fn map_proto(e: murmur_protocol::ProtocolError) -> AppError {
    use murmur_protocol::ProtocolError as P;
    match e {
        P::Decrypt => AppError::Locked("E2EE decrypt/authentication failed".into()),
        P::Malformed => AppError::InvalidArg("malformed E2EE ciphertext".into()),
        P::Encrypt => AppError::Secrets("E2EE encrypt failed".into()),
        P::Rng => AppError::Secrets("E2EE RNG failed".into()),
    }
}

/// Map an HPKE error without leaking detail. An open/decap failure (the recipient can't unwrap, or a
/// forged/tampered envelope reaches the AEAD) fails CLOSED as [`AppError::Locked`]; a seal-side
/// failure is an internal [`AppError::Secrets`].
pub(crate) fn map_hpke(e: hpke::HpkeError) -> AppError {
    use hpke::HpkeError as H;
    match e {
        H::OpenError | H::DecapError | H::ValidationError | H::IncorrectInputLength(_, _) => {
            AppError::Locked("HPKE unwrap failed (wrong recipient key or tampered envelope)".into())
        }
        _ => AppError::Secrets("HPKE seal failed".into()),
    }
}

/// Convert a byte slice into a fixed 32-byte array, erroring (not panicking) on a length mismatch.
pub(crate) fn to_arr32(b: &[u8]) -> Result<[u8; 32]> {
    b.try_into()
        .map_err(|_| AppError::InvalidArg("expected a 32-byte key".into()))
}

/// Parse a 64-byte detached Ed25519 signature.
pub(crate) fn ed25519_signature(sig: &[u8]) -> Result<ed25519_dalek::Signature> {
    let arr: [u8; 64] = sig
        .try_into()
        .map_err(|_| AppError::Auth("Ed25519 signature must be 64 bytes".into()))?;
    Ok(ed25519_dalek::Signature::from_bytes(&arr))
}

/// HKDF-SHA256 expand to a fresh 32-byte key. `salt = None` uses the RFC-5869 all-zero salt (which
/// HMAC key-padding makes byte-identical to WebCrypto's empty-salt derivation — proven by the Node
/// interop vector's `gateSecret` cross-check).
pub(crate) fn hkdf_expand32(ikm: &[u8], salt: Option<&[u8]>, info: &[u8]) -> Result<Key32> {
    use hkdf::Hkdf;
    use sha2::Sha256;
    let hk = Hkdf::<Sha256>::new(salt, ikm);
    let mut out: Key32 = Zeroizing::new([0u8; 32]);
    hk.expand(info, &mut *out)
        .map_err(|_| AppError::Secrets("HKDF expand failed (invalid output length)".into()))?;
    Ok(out)
}

/// Open a cell known to wrap a 32-byte key, returning it in zeroizing memory. The intermediate
/// plaintext `Vec` is itself zeroized. Fails closed on any AEAD/AAD mismatch.
pub(crate) fn unwrap_key32(key: &[u8; 32], cell_bytes: &[u8], aad: &[u8]) -> Result<Key32> {
    let pt = Zeroizing::new(murmur_protocol::cell::open(key, cell_bytes, aad).map_err(map_proto)?);
    if pt.len() != 32 {
        return Err(AppError::Locked(
            "unwrapped key has the wrong length".into(),
        ));
    }
    let mut out: Key32 = Zeroizing::new([0u8; 32]);
    out.copy_from_slice(pt.as_slice());
    Ok(out)
}

/// Seal the inner plaintext envelope (title travels INSIDE) under the note key `NK`, bound to
/// `aad::share_content(share_id, rev)`. Returns the content cell `C` = `nonce(12)||ct||tag(16)`.
pub fn seal_content(
    nk: &[u8; 32],
    env: &ShareEnvelope,
    share_id: &str,
    rev: u32,
) -> Result<Vec<u8>> {
    let aad = murmur_protocol::aad::share_content(share_id, rev);
    murmur_protocol::cell::seal(nk, &env.to_bytes(), aad.as_bytes()).map_err(map_proto)
}

/// Open a content cell `C` under `NK`, returning the parsed inner [`ShareEnvelope`]. Fails closed on
/// a wrong key / tampered cell / wrong share_id|rev AAD.
pub fn open_content(nk: &[u8; 32], cell: &[u8], share_id: &str, rev: u32) -> Result<ShareEnvelope> {
    let aad = murmur_protocol::aad::share_content(share_id, rev);
    let pt =
        Zeroizing::new(murmur_protocol::cell::open(nk, cell, aad.as_bytes()).map_err(map_proto)?);
    ShareEnvelope::from_bytes(&pt).map_err(map_proto)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inner_envelope_round_trips_and_binds_share_context() {
        let nk = random_key32().unwrap();
        let env = ShareEnvelope::new("Weekly sync", "- did stuff", "2026-07-04T10:00:00Z");
        let cell = seal_content(&nk, &env, "share-1", 1).unwrap();
        // Correct context → identical envelope back.
        assert_eq!(open_content(&nk, &cell, "share-1", 1).unwrap(), env);
        // Wrong rev (stale-NK reuse across "Update share") fails closed.
        assert!(matches!(
            open_content(&nk, &cell, "share-1", 2),
            Err(AppError::Locked(_))
        ));
        // Wrong share_id fails closed.
        assert!(open_content(&nk, &cell, "share-2", 1).is_err());
        // Wrong key fails closed.
        assert!(open_content(&random_key32().unwrap(), &cell, "share-1", 1).is_err());
    }

    #[test]
    fn content_cell_never_contains_the_plaintext() {
        let nk = random_key32().unwrap();
        let secret = "TOP SECRET MARKDOWN BODY";
        let env = ShareEnvelope::new("t", secret, "2026-07-04T10:00:00Z");
        let cell = seal_content(&nk, &env, "s", 1).unwrap();
        let needle = secret.as_bytes();
        assert!(
            !cell.windows(needle.len()).any(|w| w == needle),
            "ciphertext must not leak the plaintext markdown"
        );
    }
}
