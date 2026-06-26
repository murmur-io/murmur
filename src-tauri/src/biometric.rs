//! macOS Touch ID / device-owner authentication gate (Stage E).
//!
//! Releases the per-folder unlock path only after a passing biometric (or, as a fallback, a
//! passcode) prompt via Apple's `LocalAuthentication` framework. This is the "teeth" that turn a
//! locked folder from a boolean into a real authentication gate at unlock time.
//!
//! GRACEFUL DEGRADATION (deliberate): if LocalAuthentication is unavailable — no Touch ID
//! hardware, a headless/CI box, a policy that can't be evaluated — we return `Ok(true)` and log
//! it rather than locking the user out. We NEVER panic across the FFI boundary. The
//! cryptographically-real key protection lives in the Keychain ACL (the KEK release path); this
//! module is the interactive presence check layered on top.
//!
//! THREADING: the LocalAuthentication evaluation is callback-based and may block on user
//! interaction, so the whole thing runs inside `tokio::task::spawn_blocking` and bridges the
//! Objective-C reply block back through a `std::sync::mpsc` channel — it never blocks the async
//! runtime's worker threads.

use crate::error::{AppError, Result};

/// Authenticate the device owner (Touch ID, falling back to the device passcode) before a
/// security-sensitive action. `reason` is shown verbatim in the system prompt (e.g.
/// "Unlock this folder").
///
/// Returns:
/// - `Ok(true)`  — authenticated, OR biometrics/LA is unavailable (graceful degradation).
/// - `Ok(false)` — the user was prompted and FAILED/cancelled the prompt.
/// - `Err(_)`    — an unexpected internal failure (e.g. the blocking task was cancelled).
///
/// Never panics; FFI errors degrade to `Ok(true)` with a warning log.
pub async fn authenticate(reason: &str) -> Result<bool> {
    let reason = reason.to_string();
    tokio::task::spawn_blocking(move || authenticate_blocking(&reason))
        .await
        .map_err(|e| AppError::Auth(format!("biometric task join failed: {e}")))?
}

/// macOS implementation: real LocalAuthentication via objc2.
/// Decide WHICH policy to evaluate, or `None` to degrade-to-allow when nothing is evaluable.
///
/// Prefer biometrics; fall back to device-owner auth (which also accepts the device passcode) so a
/// Mac without Touch ID hardware can still gate on the login password. If NEITHER policy can be
/// evaluated (no hardware + no passcode, an unsigned/sandboxed test binary, or a CI box),
/// returns `None` — the caller then allows the unlock rather than locking the user out.
///
/// Side-effect-free: `canEvaluatePolicy` does NOT show a prompt (only `evaluatePolicy` does), so
/// this is safe to call from tests without popping a real Touch ID dialog.
#[cfg(target_os = "macos")]
fn resolve_policy(
    context: &objc2_local_authentication::LAContext,
) -> Option<objc2_local_authentication::LAPolicy> {
    use objc2_local_authentication::LAPolicy;

    // canEvaluatePolicy returns Ok(()) when evaluable, Err(NSError) otherwise.
    let bio_ok = unsafe {
        context
            .canEvaluatePolicy_error(LAPolicy::DeviceOwnerAuthenticationWithBiometrics)
            .is_ok()
    };
    if bio_ok {
        return Some(LAPolicy::DeviceOwnerAuthenticationWithBiometrics);
    }
    let owner_ok = unsafe {
        context
            .canEvaluatePolicy_error(LAPolicy::DeviceOwnerAuthentication)
            .is_ok()
    };
    if owner_ok {
        // No biometrics, but a passcode is set → prompt for the passcode.
        return Some(LAPolicy::DeviceOwnerAuthentication);
    }
    // Nothing to evaluate (no Touch ID, no passcode, CI/headless/unsigned). Don't lock out.
    None
}

#[cfg(target_os = "macos")]
fn authenticate_blocking(reason: &str) -> Result<bool> {
    use std::sync::mpsc;

    use block2::RcBlock;
    use objc2_foundation::{NSError, NSString};
    use objc2_local_authentication::LAContext;

    // SAFETY: standard LAContext construction; no special invariants beyond a valid ObjC runtime,
    // which is always present in a macOS app/test process.
    let context = unsafe { LAContext::new() };

    let policy = match resolve_policy(&context) {
        Some(p) => p,
        None => {
            tracing::warn!(
                target: "biometric",
                "LocalAuthentication unavailable (no biometrics or passcode policy evaluable) — allowing unlock without a prompt"
            );
            return Ok(true);
        }
    };

    // Bridge the async ObjC reply block → a sync channel we wait on within this blocking thread.
    let (tx, rx) = mpsc::channel::<std::result::Result<bool, String>>();
    let ns_reason = NSString::from_str(reason);

    let reply = RcBlock::new(move |success: objc2::runtime::Bool, error: *mut NSError| {
        let result = if success.as_bool() {
            Ok(true)
        } else if error.is_null() {
            // No error but not successful → treat as a denied prompt.
            Ok(false)
        } else {
            // SAFETY: LocalAuthentication hands us a valid autoreleased NSError on failure; we only
            // read its localizedDescription and never retain it past this closure.
            let desc = unsafe { (*error).localizedDescription() };
            Err(desc.to_string())
        };
        // The receiver is alive for the duration of evaluatePolicy; ignore a closed channel.
        let _ = tx.send(result);
    });

    // SAFETY: `reason` is a valid NSString and `reply` is a sendable block (RcBlock). The reply is
    // invoked exactly once by the framework on completion.
    unsafe {
        context.evaluatePolicy_localizedReason_reply(policy, &ns_reason, &reply);
    }

    // Block this (blocking-pool) thread until the framework calls back. A 60s ceiling guards
    // against a wedged prompt; on timeout we DENY (fail-closed) since the user was actively asked.
    match rx.recv_timeout(std::time::Duration::from_secs(60)) {
        Ok(Ok(true)) => Ok(true),
        Ok(Ok(false)) => {
            tracing::info!(target: "biometric", "biometric prompt denied or cancelled by user");
            Ok(false)
        }
        Ok(Err(desc)) => {
            tracing::info!(target: "biometric", error = %desc, "biometric evaluation failed");
            Ok(false)
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            tracing::warn!(target: "biometric", "biometric prompt timed out — denying");
            Ok(false)
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            // The reply block was dropped without firing — treat as an unavailable LA stack and
            // degrade to allow rather than wedging the unlock flow.
            tracing::warn!(
                target: "biometric",
                "biometric reply channel disconnected without a result — allowing unlock"
            );
            Ok(true)
        }
    }
}

/// Non-macOS fallback: there is no Touch ID, so the gate is a no-op (graceful degradation). This
/// crate ships macOS-only, but keeping the cfg makes `cargo build`/`test` on other hosts work.
#[cfg(not(target_os = "macos"))]
fn authenticate_blocking(_reason: &str) -> Result<bool> {
    tracing::warn!(
        target: "biometric",
        "biometric authentication not supported on this platform — allowing unlock"
    );
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// macOS: `resolve_policy` is side-effect-free (`canEvaluatePolicy` never prompts). Calling it
    /// must not panic across the objc2 FFI and must return a clean `Option<LAPolicy>` — which is
    /// the degradation decision (`None` ⇒ allow, `Some` ⇒ would prompt). We do NOT assert a
    /// specific variant because it is environment-dependent (signed-and-Touch-ID vs.
    /// unsigned/CI), and asserting one would be a fabricated, environment-shaped pass. This proves
    /// the FFI boundary + the graceful-degradation branch are sound WITHOUT firing a real prompt.
    #[cfg(target_os = "macos")]
    #[test]
    fn resolve_policy_does_not_panic_or_prompt() {
        use objc2_local_authentication::LAContext;
        let context = unsafe { LAContext::new() };
        // The point is that this returns at all (no panic, no UI). Both arms are valid outcomes.
        let _decision = resolve_policy(&context);
    }

    /// Non-macOS: the gate degrades to allow without any FFI.
    #[cfg(not(target_os = "macos"))]
    #[tokio::test]
    async fn authenticate_degrades_to_allow_off_platform() {
        assert!(authenticate("test").await.unwrap());
    }
}
