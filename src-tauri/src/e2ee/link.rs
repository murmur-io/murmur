//! §4.7 mode A (link-share) crypto.
//!
//! A link share hands the note out via a URL fragment `#<share_id>.<b64url(L)>` where `L` is a random
//! 32-byte key. The note key `NK` (which seals the content cell `C`) is wrapped under
//! `KEK_link = HKDF(ikm, salt_A, info = "murmur-link/v1|<share_id>|<rev>")` where `ikm = L`
//! (no password) or `ikm = L || Argon2id(password, salt_p)` (an OPTIONAL password strengthens the
//! encryption — a leaked URL alone can't decrypt). Separately, the server FETCH gate secret is
//! derived from `L` ONLY — `gate_secret = HKDF(L, "murmur-link/v1:gate")` — never from the password
//! (a password-only verifier would be offline-crackable by the very server the password defends
//! against).
//!
//! The whole flow uses only WebCrypto-reachable primitives (AES-256-GCM + HKDF-SHA256, plus Argon2id
//! via `hash-wasm` in the browser) so the dependency-free JS viewer can decrypt an identical cell —
//! proven by the [`tests::link_share_interop_vector`] fixture that the Node `verify_vector.mjs`
//! cross-checks.

use super::{hkdf_expand32, open_content, random_key32, seal_content, unwrap_key32, Key32};
use crate::error::{AppError, Result};
use argon2::{Algorithm, Argon2, Params, Version};
use murmur_protocol::{aad, cell, envelope::ShareEnvelope};
use zeroize::Zeroizing;

use super::map_proto;

/// Length of the random salts (`salt_A` for the `KEK_link` HKDF; `salt_p` for the password Argon2id).
pub const LINK_SALT_LEN: usize = 16;

/// Argon2id parameters (spec §4.6: m=64 MiB, t=3, p=1). Versioned so they travel in the blob header
/// and can be re-tuned after a real-device benchmark (§14.4) without breaking old shares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArgonParams {
    /// Memory cost in KiB.
    pub m_cost_kib: u32,
    /// Iterations.
    pub t_cost: u32,
    /// Parallelism.
    pub p_cost: u32,
}

impl ArgonParams {
    /// The v1 defaults (64 MiB / t=3 / p=1).
    pub const fn default_v1() -> Self {
        Self {
            m_cost_kib: 64 * 1024,
            t_cost: 3,
            p_cost: 1,
        }
    }
}

/// The output of sealing a mode-A link share.
pub struct LinkShareSealed {
    /// The content cell `C` = `seal(NK, envelope, aad = share_content)`.
    pub ciphertext_cell: Vec<u8>,
    /// `NK` wrapped under `KEK_link` (`aad = link_nk`).
    pub wrapped_nk: Vec<u8>,
    /// The high-entropy fragment key `L` (goes in the URL fragment; NEVER logged/persisted server-side).
    pub l: Key32,
    /// The server fetch gate secret `HKDF(L, "…:gate")` (challenge-response, not a stored verifier).
    pub gate_secret: Key32,
    /// `salt_A` for the `KEK_link` HKDF (uploaded as the share's `gate_salt`).
    pub gate_salt: [u8; LINK_SALT_LEN],
    /// `salt_p` for the password Argon2id, present only when the share is password-protected.
    pub argon_salt: Option<[u8; LINK_SALT_LEN]>,
    /// The Argon2id params used (only meaningful when `argon_salt` is `Some`).
    pub argon_params: ArgonParams,
}

fn random_salt() -> Result<[u8; LINK_SALT_LEN]> {
    let mut s = [0u8; LINK_SALT_LEN];
    getrandom::getrandom(&mut s).map_err(|e| AppError::Secrets(format!("link salt RNG: {e}")))?;
    Ok(s)
}

/// Argon2id(password, salt) → 32 bytes, with the given params.
fn argon2id(password: &[u8], salt: &[u8], p: &ArgonParams) -> Result<Key32> {
    let params = Params::new(p.m_cost_kib, p.t_cost, p.p_cost, Some(32))
        .map_err(|e| AppError::Secrets(format!("argon2 params: {e}")))?;
    let hasher = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut out: Key32 = Zeroizing::new([0u8; 32]);
    hasher
        .hash_password_into(password, salt, &mut *out)
        .map_err(|e| AppError::Secrets(format!("argon2 hash: {e}")))?;
    Ok(out)
}

/// Derive `KEK_link = HKDF(ikm, salt_A, info = link_nk(share_id, rev))`, where `ikm = L` (no
/// password) or `ikm = L || Argon2id(password, salt_p)`.
fn derive_kek_link(
    l: &[u8; 32],
    salt_a: &[u8],
    password: Option<&str>,
    argon_salt: Option<&[u8]>,
    argon: &ArgonParams,
    share_id: &str,
    rev: u32,
) -> Result<Key32> {
    let info = aad::link_nk(share_id, rev);
    let ikm: Zeroizing<Vec<u8>> = match (password, argon_salt) {
        (Some(pw), Some(psalt)) => {
            let ph = argon2id(pw.as_bytes(), psalt, argon)?;
            let mut v = Zeroizing::new(Vec::with_capacity(64));
            v.extend_from_slice(l);
            v.extend_from_slice(&*ph);
            v
        }
        (Some(_), None) => {
            return Err(AppError::InvalidArg(
                "a password-protected link share needs its argon salt".into(),
            ))
        }
        _ => {
            let mut v = Zeroizing::new(Vec::with_capacity(32));
            v.extend_from_slice(l);
            v
        }
    };
    hkdf_expand32(&ikm, Some(salt_a), info.as_bytes())
}

/// Derive the server fetch gate secret from `L` only (spec §4.7). Kept public so the share client can
/// prove possession of `L` in the challenge-response fetch.
pub fn derive_gate_secret(l: &[u8; 32]) -> Result<Key32> {
    hkdf_expand32(l, None, aad::HKDF_LINK_GATE.as_bytes())
}

/// Seal a note as a mode-A link share. Generates a fresh `NK` and `L`, seals the inner envelope under
/// `NK`, wraps `NK` under `KEK_link`, and derives the `L`-based gate secret. If `password` is set the
/// wrap key is additionally hardened with Argon2id over a fresh `salt_p`.
pub fn seal_link_share(
    env: &ShareEnvelope,
    share_id: &str,
    rev: u32,
    password: Option<&str>,
) -> Result<LinkShareSealed> {
    let nk = random_key32()?;
    let ciphertext_cell = seal_content(&nk, env, share_id, rev)?;

    let l = random_key32()?;
    let gate_salt = random_salt()?;
    let argon_params = ArgonParams::default_v1();
    let argon_salt = if password.is_some() {
        Some(random_salt()?)
    } else {
        None
    };

    let kek_link = derive_kek_link(
        &l,
        &gate_salt,
        password,
        argon_salt.as_ref().map(|s| s.as_slice()),
        &argon_params,
        share_id,
        rev,
    )?;
    let wrapped_nk =
        cell::seal(&kek_link, &*nk, aad::link_nk(share_id, rev).as_bytes()).map_err(map_proto)?;
    let gate_secret = derive_gate_secret(&l)?;

    Ok(LinkShareSealed {
        ciphertext_cell,
        wrapped_nk,
        l,
        gate_secret,
        gate_salt,
        argon_salt,
        argon_params,
    })
}

/// The inverse of [`seal_link_share`]: re-derive `KEK_link` from `L` (+ optional password), unwrap
/// `NK`, and open the content cell. Fails closed on a wrong `L`, wrong password, or tampered cell.
#[allow(clippy::too_many_arguments)]
pub fn open_link_share(
    ciphertext_cell: &[u8],
    wrapped_nk: &[u8],
    l: &[u8; 32],
    gate_salt: &[u8],
    password: Option<&str>,
    argon_salt: Option<&[u8]>,
    argon_params: &ArgonParams,
    share_id: &str,
    rev: u32,
) -> Result<ShareEnvelope> {
    let kek_link = derive_kek_link(
        l,
        gate_salt,
        password,
        argon_salt,
        argon_params,
        share_id,
        rev,
    )?;
    let nk = unwrap_key32(
        &kek_link,
        wrapped_nk,
        aad::link_nk(share_id, rev).as_bytes(),
    )?;
    open_content(&nk, ciphertext_cell, share_id, rev)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHARE_ID: &str = "11111111-1111-4111-8111-111111111111";
    const REV: u32 = 1;
    const TITLE: &str = "Weekly sync";
    const MARKDOWN: &str = "- shipped the E2EE core\n- next: wire the server endpoints";
    const CREATED_AT: &str = "2026-07-04T10:00:00Z";

    fn envelope() -> ShareEnvelope {
        ShareEnvelope::new(TITLE, MARKDOWN, CREATED_AT)
    }

    #[test]
    fn no_password_link_share_round_trips() {
        let env = envelope();
        let sealed = seal_link_share(&env, SHARE_ID, REV, None).unwrap();
        assert!(sealed.argon_salt.is_none());
        let back = open_link_share(
            &sealed.ciphertext_cell,
            &sealed.wrapped_nk,
            &sealed.l,
            &sealed.gate_salt,
            None,
            None,
            &ArgonParams::default_v1(),
            SHARE_ID,
            REV,
        )
        .unwrap();
        assert_eq!(back, env);
    }

    #[test]
    fn password_link_share_round_trips_and_needs_the_password() {
        let env = envelope();
        let pw = "correct horse battery staple";
        let sealed = seal_link_share(&env, SHARE_ID, REV, Some(pw)).unwrap();
        let argon_salt = sealed
            .argon_salt
            .expect("password share carries an argon salt");

        // Right L + right password → decrypts.
        let back = open_link_share(
            &sealed.ciphertext_cell,
            &sealed.wrapped_nk,
            &sealed.l,
            &sealed.gate_salt,
            Some(pw),
            Some(&argon_salt),
            &sealed.argon_params,
            SHARE_ID,
            REV,
        )
        .unwrap();
        assert_eq!(back, env);

        // Right L but WRONG password → fails closed (the password strengthens the encryption).
        assert!(open_link_share(
            &sealed.ciphertext_cell,
            &sealed.wrapped_nk,
            &sealed.l,
            &sealed.gate_salt,
            Some("wrong password"),
            Some(&argon_salt),
            &sealed.argon_params,
            SHARE_ID,
            REV,
        )
        .is_err());

        // Right password but no L (empty) → fails closed.
        assert!(open_link_share(
            &sealed.ciphertext_cell,
            &sealed.wrapped_nk,
            &[0u8; 32],
            &sealed.gate_salt,
            Some(pw),
            Some(&argon_salt),
            &sealed.argon_params,
            SHARE_ID,
            REV,
        )
        .is_err());
    }

    #[test]
    fn wrong_l_fails_closed() {
        let sealed = seal_link_share(&envelope(), SHARE_ID, REV, None).unwrap();
        assert!(open_link_share(
            &sealed.ciphertext_cell,
            &sealed.wrapped_nk,
            &[7u8; 32],
            &sealed.gate_salt,
            None,
            None,
            &ArgonParams::default_v1(),
            SHARE_ID,
            REV,
        )
        .is_err());
    }

    #[test]
    fn gate_secret_is_l_derived_only() {
        // The gate secret depends ONLY on L — a password does not change it (spec §4.7).
        let l = random_key32().unwrap();
        let a = derive_gate_secret(&l).unwrap();
        let b = derive_gate_secret(&l).unwrap();
        assert_eq!(*a, *b);
        assert_ne!(*a, *derive_gate_secret(&random_key32().unwrap()).unwrap());
    }

    /// Spec T2.3 — the critical Rust↔browser interop vector. Always round-trips in Rust; when
    /// `MURMUR_EMIT_VECTORS=1`, writes `testdata/link_share_vector.json` for the Node WebCrypto
    /// cross-check (`verify_vector.mjs`). The `L`, `NK`, and every GCM nonce are random, so the
    /// fixture is regenerated (not asserted byte-for-byte); its value is the CROSS-LANGUAGE proof.
    #[test]
    fn link_share_interop_vector() {
        use murmur_protocol::b64;

        let env = envelope();

        // No-password vector (Node verifies this fully with pure WebCrypto).
        let np = seal_link_share(&env, SHARE_ID, REV, None).unwrap();
        // Rust self-check proves correctness regardless of whether we emit.
        let back = open_link_share(
            &np.ciphertext_cell,
            &np.wrapped_nk,
            &np.l,
            &np.gate_salt,
            None,
            None,
            &ArgonParams::default_v1(),
            SHARE_ID,
            REV,
        )
        .unwrap();
        assert_eq!(back, env);

        if std::env::var("MURMUR_EMIT_VECTORS").is_err() {
            return;
        }

        // Password vector too (Node documents this as argon2-needed; the no-password case is the
        // mandatory cross-check).
        let pw = "correct horse battery staple";
        let wp = seal_link_share(&env, SHARE_ID, REV, Some(pw)).unwrap();
        let wp_salt = wp.argon_salt.unwrap();
        let wp_back = open_link_share(
            &wp.ciphertext_cell,
            &wp.wrapped_nk,
            &wp.l,
            &wp.gate_salt,
            Some(pw),
            Some(&wp_salt),
            &wp.argon_params,
            SHARE_ID,
            REV,
        )
        .unwrap();
        assert_eq!(wp_back, env);

        let expected = serde_json::json!({
            "v": ShareEnvelope::VERSION,
            "title": TITLE,
            "markdown": MARKDOWN,
            "createdAt": CREATED_AT,
        });
        let vector = serde_json::json!({
            "note": "murmur link-share interop vector (spec T2.3). Cell = AES-256-GCM nonce(12)||ct||tag(16); \
                     KEK_link = HKDF-SHA256(ikm, salt=gateSalt, info=linkNkAad); ikm = L (no password) or \
                     L || Argon2id(password, argonSalt). All byte fields are base64url (no padding).",
            "aad": {
                "content": aad::share_content(SHARE_ID, REV),
                "linkNk": aad::link_nk(SHARE_ID, REV),
                "gate": aad::HKDF_LINK_GATE,
            },
            "hkdf": { "hash": "SHA-256", "outputBytes": 32 },
            "noPassword": {
                "shareId": SHARE_ID,
                "rev": REV,
                "lB64": b64::encode(&*np.l),
                "gateSaltB64": b64::encode(&np.gate_salt),
                "gateSecretB64": b64::encode(&*np.gate_secret),
                "wrappedNkB64": b64::encode(&np.wrapped_nk),
                "ciphertextCellB64": b64::encode(&np.ciphertext_cell),
                "expected": expected,
            },
            "withPassword": {
                "shareId": SHARE_ID,
                "rev": REV,
                "password": pw,
                "lB64": b64::encode(&*wp.l),
                "gateSaltB64": b64::encode(&wp.gate_salt),
                "argonSaltB64": b64::encode(&wp_salt),
                "argon": {
                    "type": "argon2id",
                    "version": 19,
                    "mCostKib": wp.argon_params.m_cost_kib,
                    "tCost": wp.argon_params.t_cost,
                    "pCost": wp.argon_params.p_cost,
                    "outputBytes": 32,
                    "ikm": "L || argon2id(password, argonSalt)",
                },
                "wrappedNkB64": b64::encode(&wp.wrapped_nk),
                "ciphertextCellB64": b64::encode(&wp.ciphertext_cell),
                "expected": expected,
            },
        });

        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/e2ee/testdata");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("link_share_vector.json");
        std::fs::write(&path, serde_json::to_vec_pretty(&vector).unwrap()).unwrap();
        eprintln!("emitted interop vector → {}", path.display());
    }
}
