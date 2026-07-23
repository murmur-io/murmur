use super::*;

/// `set_gateway_key("")` must return `InvalidArg`, not silently succeed.
#[test]
fn set_gateway_key_empty_is_invalid_arg() {
    let err = set_gateway_key(String::new()).unwrap_err();
    assert!(
        matches!(err, AppError::InvalidArg(_)),
        "empty gateway key must be InvalidArg, got: {err:?}"
    );
}

/// `set_gateway_key("   ")` (whitespace-only) is also invalid.
#[test]
fn set_gateway_key_whitespace_is_invalid_arg() {
    let err = set_gateway_key("   ".to_string()).unwrap_err();
    assert!(
        matches!(err, AppError::InvalidArg(_)),
        "whitespace-only gateway key must be InvalidArg"
    );
}

// ── FIX 6: gateway URL validated at save time ──────────────────────────────────────────────

/// `save_config` rejects a gateway URL that embeds credentials — the validation used by the
/// save path (`validate_gateway_url`) refuses `https://key:@host/v1` before it reaches the DB.
/// Empty URL (no gateway configured) and a valid https URL are both accepted.
#[test]
fn save_config_gateway_url_with_credentials_is_rejected() {
    // Credential-bearing URL → InvalidArg (never stored).
    let err = crate::summarize::gateway::validate_gateway_url("https://key:@host/v1").unwrap_err();
    assert!(
        matches!(err, AppError::InvalidArg(_)),
        "URL with credentials must be InvalidArg, got: {err:?}"
    );
}

#[test]
fn save_config_valid_gateway_url_is_accepted() {
    // Valid https URL → Ok.
    assert!(
        crate::summarize::gateway::validate_gateway_url("https://gw.example.com/v1").is_ok(),
        "valid https gateway URL must be accepted"
    );
    // Localhost http → Ok.
    assert!(
        crate::summarize::gateway::validate_gateway_url("http://127.0.0.1:4000/v1").is_ok(),
        "loopback http gateway URL must be accepted"
    );
}
