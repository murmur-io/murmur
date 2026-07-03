//! `CallMeta` — per-call metadata returned by providers alongside their text output.
//!
//! Providers fill this from the API response headers/body (token counts, model served).
//! The default is all-`None`; callers that do not override `*_with_meta` get an empty struct
//! — the absence of a field means "not reported by this provider", not "zero".

/// Token-usage and model metadata captured from a single provider call.
///
/// All fields are `Option` so a partial response (e.g. a provider that does not return
/// `usage`) parses cleanly and degrades to `None` rather than erroring.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CallMeta {
    /// Model id as reported by the provider in the response body (may differ from the id the
    /// caller requested if the gateway re-routed the call).
    pub model_served: Option<String>,
    /// Tokens consumed by the prompt/system + user messages.
    pub prompt_tokens: Option<u32>,
    /// Tokens generated in the completion.
    pub completion_tokens: Option<u32>,
    /// Total tokens (prompt + completion). Populated when the API returns it directly; for
    /// providers that do not return a total, callers should sum `prompt_tokens + completion_tokens`.
    pub total_tokens: Option<u32>,
    /// Prompt tokens served from the provider's prompt cache (KV-cache or equivalent). `None`
    /// means the provider did not report a cache hit count — not that there were zero.
    pub cached_tokens: Option<u32>,
    /// PII the redaction firewall scrubbed from THIS call's content before egress, bucketed by
    /// kind. `Some(..)` ONLY when a [`RedactingProvider`](crate::summarize::redact::RedactingProvider)
    /// wrapped the call — i.e. a cloud provider. A LOCAL provider (loopback ollama / on-device
    /// reasoner) returns unwrapped, so its `redactions` stays `None` (no firewall ran). Counts
    /// only, never the scrubbed values. Read by the per-note privacy-receipt self-report.
    pub redactions: Option<RedactionCounts>,
}

/// Counts of PII placeholders injected by the redaction firewall for a single cloud call.
///
/// Populated by `RedactingProvider` and passed to the egress ledger (Phase 2.5).
/// The ledger stores ONLY these counts — never the scrubbed values themselves.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RedactionCounts {
    pub email: u32,
    pub card: u32,
    pub phone: u32,
    pub name: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Task 2.1 — `CallMeta` and `RedactionCounts` default to all-None / all-zero.
    #[test]
    fn call_meta_default_is_all_none() {
        let m = CallMeta::default();
        assert_eq!(m.model_served, None);
        assert_eq!(m.prompt_tokens, None);
        assert_eq!(m.completion_tokens, None);
        assert_eq!(m.total_tokens, None);
        assert_eq!(m.cached_tokens, None);
        assert_eq!(m.redactions, None);
    }

    #[test]
    fn redaction_counts_default_is_all_zero() {
        let r = RedactionCounts::default();
        assert_eq!(r.email, 0);
        assert_eq!(r.card, 0);
        assert_eq!(r.phone, 0);
        assert_eq!(r.name, 0);
    }

    #[test]
    fn call_meta_equality_works() {
        let a = CallMeta {
            model_served: Some("claude-opus-4-8".to_string()),
            prompt_tokens: Some(11),
            completion_tokens: Some(22),
            total_tokens: Some(33),
            cached_tokens: None,
            redactions: None,
        };
        let b = a.clone();
        assert_eq!(a, b);

        let c = CallMeta {
            model_served: None,
            ..Default::default()
        };
        assert_ne!(a, c);
    }
}
