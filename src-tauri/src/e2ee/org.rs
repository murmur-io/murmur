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

/// The FIXED sender/granter IDENTITY generation an OCK grant is signed under, on BOTH wrap and unwrap.
///
/// LOCK-SECURITY (FIX F): the underlying [`ShareGrantSignedView`] signs a `sender_generation` field.
/// For an OCK grant that field is AMBIGUOUS and adds nothing — the grant is ALREADY bound to the org +
/// the ORG generation via [`ock_grant_id`] (the `share_id`/`rev`). The two sides disagreed on which
/// generation to bind: `wrap_ock_for_member` bound the GRANTER's (owner's) identity generation, while
/// `acquire_org_ock` unwrapped with the MEMBER's OWN identity generation. Today both are hard-wired to
/// 1, so the signature verifies — but the moment identity generation rotation ships, an invited
/// member's identity gen (2+) would no longer match the owner's (1) and EVERY invited-member unwrap
/// would fail closed. Pinning BOTH sides to this constant removes the ambiguous field's mismatch (org
/// authenticity still rests on the org-scoped grant id + the granter-key TOFU pin) while keeping the
/// public signatures unchanged. The `*_generation` parameters below are therefore IGNORED for OCK
/// grants.
const OCK_GRANT_SENDER_GENERATION: u32 = 0;

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
/// `granter_generation` is IGNORED (FIX F): the signed grant's `sender_generation` is pinned to the
/// fixed [`OCK_GRANT_SENDER_GENERATION`] on BOTH wrap and unwrap, so an invited member's own identity
/// generation never has to match the granter's for the unwrap to verify.
#[allow(clippy::too_many_arguments)]
pub fn wrap_ock_for_member(
    ock: &[u8; 32],
    org_id: &str,
    generation: u32,
    recipient_pk_enc: &[u8],
    recipient_acct_id: &str,
    granter: &IdentityKeypair,
    granter_acct_id: &str,
    _granter_generation: u32,
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
        // FIX F: pin the sender identity generation to a fixed constant (org+gen binding lives in the
        // grant id; the identity generation is ambiguous across owner-vs-member and adds nothing).
        OCK_GRANT_SENDER_GENERATION,
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
/// `granter_generation` is IGNORED (FIX F): the reconstructed `sender_generation` is the fixed
/// [`OCK_GRANT_SENDER_GENERATION`], matching the wrap side, so a member whose OWN identity generation
/// differs from the granter's (post-rotation) still opens the grant.
#[allow(clippy::too_many_arguments)]
pub fn open_own_grant(
    wrapped_key: &[u8],
    grant_sig: &[u8],
    recipient: &IdentityKeypair,
    recipient_acct_id: &str,
    self_acct_id: &str,
    granter_acct_id: &str,
    _granter_generation: u32,
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
        // FIX F: pin the sender identity generation to the SAME fixed constant the wrap side used.
        OCK_GRANT_SENDER_GENERATION,
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
    /// The grant is bound to the ORG + org generation via the grant id, so the ambiguous IDENTITY
    /// generation must NOT be part of what has to match: the unwrap still opens the exact OCK. RED on the
    /// pre-fix code (wrap bound owner-gen=1, unwrap reconstructed member-gen=2 → signature mismatch →
    /// Auth error); GREEN after both sides pin the fixed OCK sender generation.
    #[test]
    fn ock_open_succeeds_across_identity_generation_mismatch() {
        let owner = generate_identity().unwrap();
        let member = generate_identity().unwrap();
        let ock = generate_ock().unwrap();

        // Owner wraps under identity generation 1.
        let grant = wrap_ock_for_member(
            &ock,
            ORG,
            GEN,
            &member.pk_enc,
            MEMBER,
            &owner,
            OWNER,
            1, // owner's identity generation at wrap
        )
        .unwrap();

        // Member unwraps under identity generation 2 (post-rotation) — must STILL open.
        let opened = open_own_grant(
            &grant.wrapped_key,
            &grant.grant_sig,
            &member,
            MEMBER,
            MEMBER,
            OWNER,
            2, // member's OWN identity generation — DIFFERENT from the owner's
            OWNER,
            &owner.pk_sig,
            ORG,
            GEN,
        )
        .unwrap();
        assert_eq!(
            *opened, *ock,
            "an invited member opens the OCK even when its identity generation differs from the granter's"
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
