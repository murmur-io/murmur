//! Organization Content Key (OCK) — the per-org, generation-numbered symmetric key that seals every
//! org-feed item (spec §"Trust model"). This is the org analogue of a per-folder content key, but it
//! is org-wide and E2EE-distributed: the OCK is wrapped PER MEMBER with the same HPKE + signed-grant
//! machinery as mode-B user shares ([`crate::e2ee::wrap`]), so the zero-knowledge server relays only
//! opaque wrapped OCKs and never sees the key.
//!
//! ## Lifecycle
//! - `generate_ock` → a fresh random 32-byte key (on org create, and on each rotation).
//! - `wrap_ock_for_member` → seal the OCK to one member's published identity key + a detached
//!   signature (the granter proves authorship; the recipient pins the granter). Reuses
//!   [`crate::e2ee::wrap::seal_to_recipient`] with an ORG-SCOPED grant id so a grant can't be lifted
//!   to another org or rolled back to an older generation.
//! - `open_own_grant` → unwrap the caller's OCK from their grant, verifying the granter's signature
//!   against the pinned granter key BEFORE any HPKE open (fail-closed on unsigned/forged/mismatched).
//! - Rotation (owner side, on member-remove/leave): `generate_ock` for gen N+1, wrap it to every
//!   REMAINING member, PUT the grants, then bump the server generation. Handled in `commands.rs`.
//!
//! The OCK lives in RAM ONLY ([`crate::state::AppState::org_ock_cache`]), keyed by `(org_id,
//! generation)`; it is NEVER persisted to SQLite/Keychain and NEVER logged (spec §8 + §"Trust model").

use crate::e2ee::keys::IdentityKeypair;
use crate::e2ee::wrap::{open_from_sender, pack_wrapped_key, SealedGrant};
use crate::e2ee::Key32;
use crate::error::Result;

/// A member's wrapped OCK grant ready to PUT to the server: the opaque `wrapped_key` blob (framed
/// sender-pubkeys + HPKE-sealed OCK) + the detached grant signature. Mirrors the mode-B
/// [`crate::e2ee::wrap`] framing so the recipient can verify + open with the identical accept path.
pub struct OckGrant {
    pub wrapped_key: Vec<u8>,
    pub grant_sig: Vec<u8>,
}

/// The org-scoped "grant id" that stands in for a mode-B `share_id` in the HPKE info + signed grant.
/// Binds the wrapped OCK to `{org_id, generation}` so a grant is neither replayable across orgs nor
/// rollback-able to an older generation (the recipient reconstructs the SAME id from the org + the
/// generation it believes is current, and the signature covers it).
pub fn ock_grant_id(org_id: &str, generation: u32) -> String {
    format!("org:{org_id}:gen:{generation}")
}

// KNOWN v1 LIMITATION (was "FIX F", REVERTED in 0.9.3 — it broke backward compat):
// the signed [`ShareGrantSignedView`] binds a `sender_generation`. `wrap_ock_for_member` binds the
// GRANTER's (owner's) identity generation; `acquire_org_ock` unwraps with the MEMBER's OWN identity
// generation. Today identity generation is hard-wired to 1 everywhere, so owner-gen == member-gen == 1
// and every grant verifies. 0.9.2 tried to pin BOTH sides to a fixed constant (0) so a future member
// gen (2+) wouldn't have to match the owner's — but that made 0.9.2 reconstruct the signed view with
// sender_generation=0 for grants ALREADY SIGNED under gen=1 (every org created pre-0.9.2), so
// `verify_strict` rejected them ("share grant signature is invalid") and org sharing broke. The
// constant is reverted; the grant stays bound to `granter_generation` (matching all existing grants).
// The latent owner-vs-member-generation mismatch only fires once identity-key ROTATION ships (not
// shipped); the correct fix THEN is to resolve the OWNER's identity generation at unwrap (from the
// member directory) + a grant-version marker for migration — NOT a constant that breaks old grants.

/// Generate a fresh random 32-byte OCK (org create + each rotation). Zeroizes on drop.
pub fn generate_ock() -> Result<Key32> {
    crate::e2ee::random_key32()
}

/// Wrap `ock` to ONE member: HPKE-seal it to their published `recipient_pk_enc` + sign the canonical
/// grant with the granter's identity. The grant is bound to `ock_grant_id(org_id, generation)` (as
/// the `share_id`) and to `generation` (as the `rev`), so the recipient's reconstruction under a
/// different org/generation fails the signature check.
///
/// `granter` is the owner's identity keypair (derived from their MK); `granter_acct_id` /
/// `recipient_acct_id` are the stable account ids the two sides pin on (the caller passes the server
/// user ids / fingerprints, exactly as mode-B does).
///
/// `granter_generation` binds the granter's identity generation into the signed grant (must match the
/// unwrap side — today always 1; see the KNOWN v1 LIMITATION note above).
#[allow(clippy::too_many_arguments)]
pub fn wrap_ock_for_member(
    ock: &[u8; 32],
    org_id: &str,
    generation: u32,
    recipient_pk_enc: &[u8],
    recipient_acct_id: &str,
    granter: &IdentityKeypair,
    granter_acct_id: &str,
    granter_generation: u32,
) -> Result<OckGrant> {
    let grant_id = ock_grant_id(org_id, generation);
    // The signed grant binds a hash of the "content cell". For an OCK grant there is no separate
    // content cell (the OCK IS the payload), so we bind the org grant id itself — a stable, per-org-
    // per-generation string — as the hashed context. This makes the signature cover {granter,
    // recipient, org, generation} without introducing a spurious content dependency.
    let grant: SealedGrant = crate::e2ee::wrap::seal_to_recipient(
        ock,
        grant_id.as_bytes(),
        recipient_pk_enc,
        recipient_acct_id,
        granter,
        granter_acct_id,
        granter_generation,
        &grant_id,
        generation,
    )?;
    let wrapped_key = pack_wrapped_key(&granter.pk_enc, &granter.pk_sig, &grant)?;
    Ok(OckGrant {
        wrapped_key,
        grant_sig: grant.signature,
    })
}

/// Open the caller's own OCK grant: verify the granter's signature against the PINNED granter key +
/// the org/generation binding BEFORE any HPKE open, then decapsulate the OCK. Fails closed
/// (`AppError::Auth`) on an unsigned/forged/mismatched grant, or (`AppError::Locked`) on an HPKE open
/// failure. `wrapped_key` + `grant_sig` are the opaque bytes the server relayed.
///
/// `pinned_granter_acct_id` / `pinned_granter_pk_sig` are the TOFU-pinned granter identity the
/// recipient stored on first contact (the org owner). `self_acct_id` is the caller's own account id.
///
/// `granter_generation` must match the generation the grant was signed under at wrap time (today 1;
/// see the KNOWN v1 LIMITATION note above — the caller passes the recipient's own generation, which
/// equals the owner's at gen 1).
#[allow(clippy::too_many_arguments)]
pub fn open_own_grant(
    wrapped_key: &[u8],
    grant_sig: &[u8],
    recipient: &IdentityKeypair,
    recipient_acct_id: &str,
    self_acct_id: &str,
    granter_acct_id: &str,
    granter_generation: u32,
    pinned_granter_acct_id: &str,
    pinned_granter_pk_sig: &[u8],
    org_id: &str,
    generation: u32,
) -> Result<Key32> {
    let grant_id = ock_grant_id(org_id, generation);
    let unpacked = crate::e2ee::wrap::unpack_wrapped_key(wrapped_key, grant_sig)?;
    // The "content cell" the signature was computed over is the org grant id (see `wrap_ock_for_member`).
    open_from_sender(
        &unpacked.grant,
        grant_id.as_bytes(),
        recipient,
        recipient_acct_id,
        self_acct_id,
        granter_acct_id,
        granter_generation,
        pinned_granter_acct_id,
        pinned_granter_pk_sig,
        &grant_id,
        generation,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::e2ee::keys::generate_identity;

    const OWNER: &str = "acct-owner";
    const MEMBER: &str = "acct-member";
    const OTHER: &str = "acct-other";
    const ORG: &str = "org-1";
    const GEN: u32 = 1;

    /// The owner wraps the OCK to a member; the member opens it back byte-identical.
    #[test]
    fn ock_wrap_open_round_trips() {
        let owner = generate_identity().unwrap();
        let member = generate_identity().unwrap();
        let ock = generate_ock().unwrap();

        let grant = wrap_ock_for_member(
            &ock,
            ORG,
            GEN,
            &member.pk_enc,
            MEMBER,
            &owner,
            OWNER,
            1,
        )
        .unwrap();

        let opened = open_own_grant(
            &grant.wrapped_key,
            &grant.grant_sig,
            &member,
            MEMBER,
            MEMBER,
            OWNER,
            1,
            OWNER,
            &owner.pk_sig,
            ORG,
            GEN,
        )
        .unwrap();
        assert_eq!(*opened, *ock, "member recovers the exact OCK");
    }

    /// FIX F (identity-generation mismatch, RED→GREEN): the OWNER wraps the OCK under their own
    /// identity generation (1), and the invited MEMBER unwraps under a DIFFERENT identity generation (2)
    /// — as happens the moment identity rotation ships (the owner is gen 1, an invited member gen 2+).
    /// 0.9.3 BACKWARD-COMPAT REGRESSION GUARD: an OCK grant is bound to the granter identity
    /// generation it was SIGNED under. A grant wrapped at gen 1 (every org created pre-0.9.2) MUST
    /// open when unwrapped at gen 1, and must be REJECTED at gen 0 — which is exactly what 0.9.2's
    /// pin-sender_generation-to-0 did to those existing grants ("share grant signature is invalid").
    /// RED on the 0.9.2 constant-0 code (gen-1 grant opened at gen 0 / failed at gen 1); GREEN after
    /// the revert. (The latent owner-vs-member generation mismatch only fires once identity rotation
    /// ships — see the KNOWN v1 LIMITATION note above; not exercised here.)
    #[test]
    fn ock_grant_is_generation_bound_gen1_roundtrips_and_gen0_rejects() {
        let owner = generate_identity().unwrap();
        let member = generate_identity().unwrap();
        let ock = generate_ock().unwrap();

        // Owner wraps under identity generation 1 (the only generation that exists today).
        let grant =
            wrap_ock_for_member(&ock, ORG, GEN, &member.pk_enc, MEMBER, &owner, OWNER, 1).unwrap();

        // gen 1 unwrap → opens (existing pre-0.9.2 grants keep working).
        let opened = open_own_grant(
            &grant.wrapped_key,
            &grant.grant_sig,
            &member,
            MEMBER,
            MEMBER,
            OWNER,
            1,
            OWNER,
            &owner.pk_sig,
            ORG,
            GEN,
        )
        .unwrap();
        assert_eq!(*opened, *ock, "a gen-1 grant opens when unwrapped at gen 1");

        // gen 0 unwrap (what 0.9.2's constant did) → REJECTED: this is the reported regression.
        let res = open_own_grant(
            &grant.wrapped_key,
            &grant.grant_sig,
            &member,
            MEMBER,
            MEMBER,
            OWNER,
            0,
            OWNER,
            &owner.pk_sig,
            ORG,
            GEN,
        );
        assert!(
            matches!(res, Err(crate::error::AppError::Auth(_))),
            "a gen-1 grant must NOT open under gen 0 (the 0.9.2 constant-0 regression)"
        );
    }

    /// A grant wrapped for org-1 must NOT open as org-2 (cross-org replay blocked).
    #[test]
    fn grant_bound_to_org_rejects_other_org() {
        let owner = generate_identity().unwrap();
        let member = generate_identity().unwrap();
        let ock = generate_ock().unwrap();
        let grant =
            wrap_ock_for_member(&ock, ORG, GEN, &member.pk_enc, MEMBER, &owner, OWNER, 1).unwrap();

        let res = open_own_grant(
            &grant.wrapped_key,
            &grant.grant_sig,
            &member,
            MEMBER,
            MEMBER,
            OWNER,
            1,
            OWNER,
            &owner.pk_sig,
            "org-2",
            GEN,
        );
        assert!(res.is_err(), "a grant for org-1 must not open as org-2");
    }

    /// A generation-rollback (grant is gen 2 but recipient checks gen 1) fails the binding.
    #[test]
    fn grant_bound_to_generation_rejects_rollback() {
        let owner = generate_identity().unwrap();
        let member = generate_identity().unwrap();
        let ock = generate_ock().unwrap();
        let grant =
            wrap_ock_for_member(&ock, ORG, 2, &member.pk_enc, MEMBER, &owner, OWNER, 1).unwrap();

        // Recipient believes the org is at generation 1 → reconstructed grant id differs → reject.
        let res = open_own_grant(
            &grant.wrapped_key,
            &grant.grant_sig,
            &member,
            MEMBER,
            MEMBER,
            OWNER,
            1,
            OWNER,
            &owner.pk_sig,
            ORG,
            1,
        );
        assert!(res.is_err(), "a gen-2 grant must not open as gen-1");
    }

    /// A grant signed by an UNPINNED key (a malicious relay substituting the granter) is rejected
    /// before any HPKE open.
    #[test]
    fn grant_from_unpinned_granter_rejected() {
        let owner = generate_identity().unwrap();
        let attacker = generate_identity().unwrap();
        let member = generate_identity().unwrap();
        let ock = generate_ock().unwrap();

        // The ATTACKER wraps a perfectly-valid grant (signed with THEIR key), claiming to be OWNER.
        let grant =
            wrap_ock_for_member(&ock, ORG, GEN, &member.pk_enc, MEMBER, &attacker, OWNER, 1)
                .unwrap();

        // The member pinned the REAL owner's key → verification fails closed.
        let res = open_own_grant(
            &grant.wrapped_key,
            &grant.grant_sig,
            &member,
            MEMBER,
            MEMBER,
            OWNER,
            1,
            OWNER,
            &owner.pk_sig,
            ORG,
            GEN,
        );
        assert!(res.is_err(), "a grant signed by an unpinned key must be rejected");
    }

    /// A grant addressed to MEMBER cannot be opened by OTHER (recipient binding).
    #[test]
    fn grant_bound_to_recipient() {
        let owner = generate_identity().unwrap();
        let member = generate_identity().unwrap();
        let other = generate_identity().unwrap();
        let ock = generate_ock().unwrap();
        let grant =
            wrap_ock_for_member(&ock, ORG, GEN, &member.pk_enc, MEMBER, &owner, OWNER, 1).unwrap();

        let res = open_own_grant(
            &grant.wrapped_key,
            &grant.grant_sig,
            &other,
            OTHER,
            OTHER,
            OWNER,
            1,
            OWNER,
            &owner.pk_sig,
            ORG,
            GEN,
        );
        assert!(res.is_err(), "a grant for MEMBER must not open for OTHER");
    }

    #[test]
    fn ock_grant_id_binds_org_and_generation() {
        assert_eq!(ock_grant_id("o", 3), "org:o:gen:3");
        assert_ne!(ock_grant_id("o", 1), ock_grant_id("o", 2));
        assert_ne!(ock_grant_id("a", 1), ock_grant_id("b", 1));
    }
}
