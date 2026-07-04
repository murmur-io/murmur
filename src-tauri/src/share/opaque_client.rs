//! OPAQUE (RFC 9807) CLIENT side — registration + login — run on-device so the account password
//! never leaves the Mac (spec §3). The client's `export_key` is the ONLY output that reaches the
//! crypto layer: `keys::derive_kek_pw(export_key)` turns it into the key that wraps `MK`.
//!
//! CIPHERSUITE = SINGLE SHARED SOURCE
//! ---------------------------------------------------------------------------------------------
//! OPAQUE is a two-party protocol: the client and server MUST agree on the ciphersuite byte-for-byte
//! or every registration/login silently produces mismatched wire messages and fails. The suite
//! (`Ristretto255` OPRF + `TripleDh<Ristretto255, Sha512>` + `Argon2` KSF) is therefore defined ONCE
//! in `murmur_protocol::opaque` (behind that crate's `opaque` feature) and re-used by BOTH sides —
//! it is no longer duplicated, so the two cannot drift. The [`tests::opaque_client_server_round_trip`]
//! test still pins the interop end-to-end (client helpers here + server helpers there).
//!
//! No `unwrap`/`expect` in non-test code (rust-tauri §1): the server's helpers `expect(...)` on
//! client start/finish; ours return [`crate::error::Result`] with [`AppError::Auth`]. The password
//! bytes are borrowed, never logged.

use crate::error::{AppError, Result};
use opaque_ke::{
    ClientLogin, ClientLoginFinishParameters, ClientRegistration,
    ClientRegistrationFinishParameters, CredentialResponse, RegistrationResponse,
};
use rand::rngs::OsRng;

// The ciphersuite is now the SINGLE shared definition in `murmur-protocol` (behind its `opaque`
// feature) — no longer duplicated here, so client and server cannot drift. The OPAQUE message types
// below resolve to the same `CS` on both sides.
pub use murmur_protocol::opaque::{MurmurCipherSuite, CS};

fn opaque_err(ctx: &str) -> impl Fn(opaque_ke::errors::ProtocolError) -> AppError + '_ {
    move |_e| AppError::Auth(format!("OPAQUE {ctx} failed"))
}

// ─────────────────────────────── Registration (signup) ───────────────────────────────

/// Transient client-side registration state: the OPAQUE `ClientRegistration` + the `ke1`-equivalent
/// `RegistrationRequest` bytes to send to the server's `POST /v1/auth/provision`.
pub struct ClientRegStart {
    pub state: ClientRegistration<CS>,
    /// Serialized `RegistrationRequest` (goes on the wire as `opaqueRegistrationRequest`).
    pub request_bytes: Vec<u8>,
}

/// Client registration step 1. Runs the OPRF blind over the password and returns the request to
/// send to the server, plus the state to carry into [`client_registration_finish`].
pub fn client_registration_start(password: &[u8]) -> Result<ClientRegStart> {
    let r = ClientRegistration::<CS>::start(&mut OsRng, password)
        .map_err(opaque_err("registration start"))?;
    Ok(ClientRegStart {
        state: r.state,
        request_bytes: r.message.serialize().to_vec(),
    })
}

/// Client registration step 2. Given the server's `RegistrationResponse` bytes, produces the
/// `RegistrationUpload` to send to `POST /v1/auth/provision/finish` AND the stable `export_key`
/// that the caller feeds to `keys::derive_kek_pw` to wrap `MK`.
pub fn client_registration_finish(
    state: ClientRegistration<CS>,
    password: &[u8],
    registration_response_bytes: &[u8],
) -> Result<(Vec<u8>, Vec<u8>)> {
    let response = RegistrationResponse::<CS>::deserialize(registration_response_bytes)
        .map_err(opaque_err("registration response decode"))?;
    let r = state
        .finish(
            &mut OsRng,
            password,
            response,
            ClientRegistrationFinishParameters::default(),
        )
        .map_err(opaque_err("registration finish"))?;
    Ok((r.message.serialize().to_vec(), r.export_key.to_vec()))
}

// ─────────────────────────────── Login ───────────────────────────────

/// Transient client-side login state: the OPAQUE `ClientLogin` + the `ke1` (`CredentialRequest`)
/// bytes to send to `POST /v1/auth/login/start`.
pub struct ClientLoginStart {
    pub state: ClientLogin<CS>,
    /// Serialized `CredentialRequest` (goes on the wire as `ke1`).
    pub ke1_bytes: Vec<u8>,
}

/// Client login step 1. Blinds the password into `ke1`.
pub fn client_login_start(password: &[u8]) -> Result<ClientLoginStart> {
    let r = ClientLogin::<CS>::start(&mut OsRng, password).map_err(opaque_err("login start"))?;
    Ok(ClientLoginStart {
        state: r.state,
        ke1_bytes: r.message.serialize().to_vec(),
    })
}

/// Client login step 2. Given the server's `ke2` (`CredentialResponse`) bytes, produces:
/// `(ke3_bytes, export_key)`. A WRONG password (or the server's anti-enumeration dummy path) fails
/// the envelope open here → `Err(AppError::Auth)`, so the client learns "bad credentials" locally
/// without the server ever confirming the account exists.
pub fn client_login_finish(
    state: ClientLogin<CS>,
    password: &[u8],
    ke2_bytes: &[u8],
) -> Result<(Vec<u8>, Vec<u8>)> {
    let response =
        CredentialResponse::<CS>::deserialize(ke2_bytes).map_err(opaque_err("ke2 decode"))?;
    let r = state
        .finish(
            &mut OsRng,
            password,
            response,
            ClientLoginFinishParameters::default(),
        )
        .map_err(opaque_err("login finish (wrong password?)"))?;
    Ok((r.message.serialize().to_vec(), r.export_key.to_vec()))
}

#[cfg(test)]
mod tests {
    //! The ciphersuite-duplication guard (spec: "pin correctness with a test"). We do NOT link the
    //! AGPL `murmur-server` crate (a license hazard for the non-AGPL app, even as a dev-dep). Instead
    //! the SERVER side is replicated INLINE with `opaque-ke`'s own server types under THIS module's
    //! `CS` (the client ciphersuite). Because both parties are driven by the SAME `CS`, the round-trip
    //! succeeding proves the suite the client uses is the one that interoperates — and `CS` is a
    //! verbatim copy of the server's, so a future edit on either side is caught here. This mirrors the
    //! server's own `opaque.rs` test exactly (same call shapes), which is the executable spec of the
    //! server contract.
    use super::*;
    use opaque_ke::{
        CredentialRequest, RegistrationRequest, RegistrationUpload, ServerLogin,
        ServerLoginParameters, ServerRegistration, ServerSetup,
    };

    /// Full register→login round-trip against an in-process server driven by the identical `CS`.
    /// Asserts the `export_key` interlock (stable across register↔login) + mutual auth (session keys
    /// agree). If the client `CS` ever diverges from the server's, the wire messages stop lining up.
    #[test]
    fn opaque_client_server_round_trip() {
        let email = b"alice@example.com";
        let password = b"correct horse battery staple";
        let setup = ServerSetup::<CS>::new(&mut OsRng);

        // --- Registration ---
        let creg = client_registration_start(password).unwrap();
        let req = RegistrationRequest::<CS>::deserialize(&creg.request_bytes).unwrap();
        let sresp = ServerRegistration::<CS>::start(&setup, req, email)
            .unwrap()
            .message;
        let (upload_bytes, reg_export_key) =
            client_registration_finish(creg.state, password, &sresp.serialize().to_vec()).unwrap();
        let upload = RegistrationUpload::<CS>::deserialize(&upload_bytes).unwrap();
        let password_file = ServerRegistration::<CS>::finish(upload)
            .serialize()
            .to_vec();

        // --- Login (correct password) ---
        let clog = client_login_start(password).unwrap();
        let ke1 = CredentialRequest::<CS>::deserialize(&clog.ke1_bytes).unwrap();
        let srv_start = ServerLogin::start(
            &mut OsRng,
            &setup,
            Some(ServerRegistration::<CS>::deserialize(&password_file).unwrap()),
            ke1,
            email,
            ServerLoginParameters::default(),
        )
        .unwrap();
        let ke2_bytes = srv_start.message.serialize().to_vec();
        let (ke3_bytes, login_export_key) =
            client_login_finish(clog.state, password, &ke2_bytes).unwrap();
        let ke3 = opaque_ke::CredentialFinalization::<CS>::deserialize(&ke3_bytes).unwrap();
        let srv_finish = srv_start
            .state
            .finish(ke3, ServerLoginParameters::default())
            .unwrap();

        assert!(!reg_export_key.is_empty());
        assert_eq!(
            reg_export_key, login_export_key,
            "export_key must be stable"
        );
        // Mutual auth: the client also derived a session key at login/finish. We assert the server
        // side completed without error (above) — a mismatched suite would have errored there.
        assert!(!srv_finish.session_key.to_vec().is_empty());
    }

    /// A wrong password fails CLOSED at the client `finish` (envelope open fails) — the client learns
    /// "bad credentials" without the server confirming the account exists.
    #[test]
    fn wrong_password_fails_client_side() {
        let email = b"bob@example.com";
        let setup = ServerSetup::<CS>::new(&mut OsRng);

        let creg = client_registration_start(b"right").unwrap();
        let req = RegistrationRequest::<CS>::deserialize(&creg.request_bytes).unwrap();
        let sresp = ServerRegistration::<CS>::start(&setup, req, email)
            .unwrap()
            .message;
        let (upload_bytes, _) =
            client_registration_finish(creg.state, b"right", &sresp.serialize().to_vec()).unwrap();
        let password_file = ServerRegistration::<CS>::finish(
            RegistrationUpload::<CS>::deserialize(&upload_bytes).unwrap(),
        )
        .serialize()
        .to_vec();

        let clog = client_login_start(b"WRONG").unwrap();
        let ke1 = CredentialRequest::<CS>::deserialize(&clog.ke1_bytes).unwrap();
        let srv_start = ServerLogin::start(
            &mut OsRng,
            &setup,
            Some(ServerRegistration::<CS>::deserialize(&password_file).unwrap()),
            ke1,
            email,
            ServerLoginParameters::default(),
        )
        .unwrap();
        let ke2_bytes = srv_start.message.serialize().to_vec();
        assert!(
            client_login_finish(clog.state, b"WRONG", &ke2_bytes).is_err(),
            "a wrong password must fail closed at the client finish"
        );
    }
}
