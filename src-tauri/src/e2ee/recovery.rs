//! §4.3/§4.9 recovery layer: a 24-word BIP39 phrase encoding the 256-bit recovery key `RK`, and the
//! mutual `MK↔RK` wrapping.
//!
//! `RK` is a random 256-bit value; the 24-word BIP39 mnemonic is simply its checksummed human
//! encoding (`RK` == the mnemonic's entropy), so the phrase round-trips losslessly and can be re-shown
//! while logged in. Two wraps exist (spec §4):
//! - `MK_wrap_rk` = `MK` sealed UNDER `RK` → server. A forgotten-password reset unwraps `MK` here with
//!   the phrase (proof-of-possession) and re-wraps under the new `KEK_pw`.
//! - `RK_wrap_mk` = `RK` sealed UNDER `MK` → server. While logged in (holding `MK`) this recovers `RK`
//!   to re-display the phrase without asking the user to re-enter it.
//!
//! The phrase is SKIPPABLE at signup (§4.3): when skipped, neither wrap is created (no `MK_wrap_rk`
//! row), which keeps the "reset-without-kit loses server shares" copy honest.

use super::{unwrap_key32, Key32};
use crate::error::{AppError, Result};
use bip39::Mnemonic;
use murmur_protocol::{aad, cell};
use zeroize::Zeroizing;

use super::map_proto;

/// Generate a fresh 24-word BIP39 recovery phrase and the 256-bit recovery key `RK` it encodes.
/// Returns `(phrase, RK)` — the phrase is shown to the user once; `RK` is used to wrap `MK`.
pub fn generate_recovery_phrase() -> Result<(String, Key32)> {
    let rk = super::random_key32()?;
    let mnemonic = Mnemonic::from_entropy(&*rk)
        .map_err(|e| AppError::Secrets(format!("BIP39 encode failed: {e}")))?;
    Ok((mnemonic.to_string(), rk))
}

/// Recover the 256-bit `RK` from a user-entered 24-word phrase (whitespace-trimmed). Rejects an
/// invalid phrase (bad checksum / unknown word) as [`AppError::InvalidArg`] and any non-256-bit
/// mnemonic (must be 24 words).
pub fn recovery_key_from_phrase(phrase: &str) -> Result<Key32> {
    let mnemonic = Mnemonic::parse(phrase.trim())
        .map_err(|_| AppError::InvalidArg("invalid recovery phrase".into()))?;
    let entropy = Zeroizing::new(mnemonic.to_entropy());
    if entropy.len() != 32 {
        return Err(AppError::InvalidArg(
            "recovery phrase must be 24 words (256-bit)".into(),
        ));
    }
    let mut rk: Key32 = Zeroizing::new([0u8; 32]);
    rk.copy_from_slice(entropy.as_slice());
    Ok(rk)
}

/// Re-encode an in-hand `RK` back to its 24-word phrase (the "show recovery key while logged in" path,
/// after [`unwrap_rk_mk`]).
pub fn phrase_from_recovery_key(rk: &[u8; 32]) -> Result<String> {
    let mnemonic = Mnemonic::from_entropy(rk)
        .map_err(|e| AppError::Secrets(format!("BIP39 encode failed: {e}")))?;
    Ok(mnemonic.to_string())
}

/// `MK_wrap_rk`: seal `MK` UNDER `RK` (AAD `mk-rk|<acct_id>`). Server-stored; unwrapped on a
/// forgotten-password recovery.
pub fn wrap_mk_rk(mk: &[u8; 32], rk: &[u8; 32], acct_id: &str) -> Result<Vec<u8>> {
    cell::seal(rk, mk, aad::mk_rk(acct_id).as_bytes()).map_err(map_proto)
}

/// Unwrap `MK` from `MK_wrap_rk` using the recovery key. A successful decrypt is itself the
/// proof-of-possession that authorizes the reset (spec §3.6).
pub fn unwrap_mk_rk(wrapped: &[u8], rk: &[u8; 32], acct_id: &str) -> Result<Key32> {
    unwrap_key32(rk, wrapped, aad::mk_rk(acct_id).as_bytes())
}

/// `RK_wrap_mk`: seal `RK` UNDER `MK` (AAD `rk-mk|<acct_id>`). Server-stored; lets a logged-in user
/// recover `RK` (and re-show the phrase) without re-entering it.
pub fn wrap_rk_mk(rk: &[u8; 32], mk: &[u8; 32], acct_id: &str) -> Result<Vec<u8>> {
    cell::seal(mk, rk, aad::rk_mk(acct_id).as_bytes()).map_err(map_proto)
}

/// Unwrap `RK` from `RK_wrap_mk` using `MK`.
pub fn unwrap_rk_mk(wrapped: &[u8], mk: &[u8; 32], acct_id: &str) -> Result<Key32> {
    unwrap_key32(mk, wrapped, aad::rk_mk(acct_id).as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::e2ee::keys::generate_master_key;

    const ACCT: &str = "acct-xyz";

    #[test]
    fn phrase_is_24_words_and_round_trips_to_the_same_rk() {
        let (phrase, rk) = generate_recovery_phrase().unwrap();
        assert_eq!(
            phrase.split_whitespace().count(),
            24,
            "256-bit BIP39 = 24 words"
        );
        // Phrase → RK recovers the identical key.
        assert_eq!(*recovery_key_from_phrase(&phrase).unwrap(), *rk);
        // RK → phrase re-encodes identically (re-show while logged in).
        assert_eq!(phrase_from_recovery_key(&rk).unwrap(), phrase);
    }

    #[test]
    fn invalid_phrase_is_rejected() {
        assert!(matches!(
            recovery_key_from_phrase("not a valid bip39 mnemonic at all"),
            Err(AppError::InvalidArg(_))
        ));
        // A 24-word phrase with a broken checksum (all "abandon") is rejected.
        let bad = vec!["abandon"; 24].join(" ");
        assert!(recovery_key_from_phrase(&bad).is_err());
    }

    #[test]
    fn mk_wrap_rk_round_trips_and_is_context_bound() {
        let mk = generate_master_key().unwrap();
        let (_phrase, rk) = generate_recovery_phrase().unwrap();
        let wrapped = wrap_mk_rk(&mk, &rk, ACCT).unwrap();
        // Correct RK + acct → identical MK back (the forgotten-password recovery path).
        assert_eq!(*unwrap_mk_rk(&wrapped, &rk, ACCT).unwrap(), *mk);
        // Wrong RK fails closed.
        let (_p2, rk2) = generate_recovery_phrase().unwrap();
        assert!(unwrap_mk_rk(&wrapped, &rk2, ACCT).is_err());
        // Wrong account (AAD) fails closed.
        assert!(unwrap_mk_rk(&wrapped, &rk, "other").is_err());
    }

    #[test]
    fn rk_wrap_mk_round_trips_for_reshow() {
        let mk = generate_master_key().unwrap();
        let (phrase, rk) = generate_recovery_phrase().unwrap();
        let wrapped = wrap_rk_mk(&rk, &mk, ACCT).unwrap();
        let recovered = unwrap_rk_mk(&wrapped, &mk, ACCT).unwrap();
        assert_eq!(*recovered, *rk);
        // And the recovered RK re-shows the original phrase.
        assert_eq!(phrase_from_recovery_key(&recovered).unwrap(), phrase);
    }

    #[test]
    fn mk_recoverable_from_all_three_wraps() {
        // The full §4 promise: MK unwraps from local, pw, and rk wraps to the same 32 bytes.
        let mk = generate_master_key().unwrap();
        let kek_local = crate::crypto::random_key().unwrap();
        let kek_pw = crate::e2ee::keys::derive_kek_pw(b"export").unwrap();
        let (_phrase, rk) = generate_recovery_phrase().unwrap();

        let w_local = crate::e2ee::keys::wrap_mk_local(&mk, &kek_local, ACCT).unwrap();
        let w_pw = crate::e2ee::keys::wrap_mk_pw(&mk, &kek_pw, ACCT).unwrap();
        let w_rk = wrap_mk_rk(&mk, &rk, ACCT).unwrap();

        assert_eq!(
            *crate::e2ee::keys::unwrap_mk_local(&w_local, &kek_local, ACCT).unwrap(),
            *mk
        );
        assert_eq!(
            *crate::e2ee::keys::unwrap_mk_pw(&w_pw, &kek_pw, ACCT).unwrap(),
            *mk
        );
        assert_eq!(*unwrap_mk_rk(&w_rk, &rk, ACCT).unwrap(), *mk);
    }
}
