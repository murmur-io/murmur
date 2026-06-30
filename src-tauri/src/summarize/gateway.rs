//! OpenAI-compatible "AI Gateway" provider + URL guardrails.
use crate::error::{AppError, Result};

/// True iff the URL host is loopback — the ONLY case where plain `http://` is allowed and (for
/// `ollama`) the only case treated as non-cloud. A loopback gateway can still FORWARD to the cloud,
/// so loopback is NOT a redaction exemption for the future `gateway` provider — only for `ollama`.
pub fn host_is_loopback(url: &reqwest::Url) -> bool {
    match url.host_str() {
        None => false,
        Some(h) => {
            if h.eq_ignore_ascii_case("localhost") {
                return true;
            }
            let h = h.strip_prefix('[').and_then(|s| s.strip_suffix(']')).unwrap_or(h);
            h.parse::<std::net::IpAddr>().map(|ip| ip.is_loopback()).unwrap_or(false)
        }
    }
}

/// Validate a user-supplied gateway base URL (guardrails R1/R4): https required, except http on
/// loopback; reject every other scheme (no file:/ftp:/gopher: SSRF surface).
pub fn validate_gateway_url(raw: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(raw.trim())
        .map_err(|_| AppError::InvalidArg("gateway URL is not a valid URL".into()))?;
    match url.scheme() {
        "https" => Ok(url),
        "http" if host_is_loopback(&url) => Ok(url),
        "http" => Err(AppError::InvalidArg(
            "gateway URL must use https:// (http:// is only allowed for localhost)".into(),
        )),
        other => Err(AppError::InvalidArg(format!("unsupported gateway URL scheme: {other}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_detection() {
        assert!(host_is_loopback(&reqwest::Url::parse("http://localhost:11434").unwrap()));
        assert!(host_is_loopback(&reqwest::Url::parse("http://127.0.0.1:4000/v1").unwrap()));
        assert!(host_is_loopback(&reqwest::Url::parse("http://[::1]:8000").unwrap()));
        assert!(!host_is_loopback(&reqwest::Url::parse("https://api.example.com/v1").unwrap()));
    }

    #[test]
    fn url_validation_rejects_plain_http_remote_and_bad_scheme() {
        assert!(validate_gateway_url("https://gw.example.com/v1").is_ok());
        assert!(validate_gateway_url("http://localhost:4000/v1").is_ok());
        assert!(validate_gateway_url("http://evil.example.com/v1").is_err()); // remote http rejected
        assert!(validate_gateway_url("file:///etc/passwd").is_err()); // scheme rejected
        assert!(validate_gateway_url("not a url").is_err());
    }
}
