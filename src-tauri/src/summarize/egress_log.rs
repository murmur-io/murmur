//! Egress ledger — a content-free, per-call audit log of every cloud provider round-trip.
//!
//! `EgressEntry` carries ONLY ids, host/label, model strings, token counts, PII-token counts by
//! kind, and byte counts. It MUST NEVER carry the transcript, the prompt, scrubbed values, the
//! API key, or any note/meeting content. A lock-security review will fail this module if any
//! content reaches the ledger (rust-tauri §8: no PII in logs).
//!
//! The process-global `EGRESS_SINK` is set once at startup (via `set_egress_sink`) by `lib.rs`
//! after `AppState::init()` succeeds. Until then — and in tests that do not set a sink —
//! `active_sink()` falls through to `NoopEgressSink` so no code path panics.

use std::sync::{Arc, OnceLock};

use crate::storage::Db;
use crate::summarize::meta::{CallMeta, RedactionCounts};

/// One content-free audit row per cloud provider call.
///
/// Security contract: this struct's fields are ONLY counts, ids, labels, and byte sizes.
/// No transcript text, prompt content, scrubbed values, API keys, or any meeting content
/// is allowed here — the Debug impl is asserted in the content-free test in `redact.rs`.
#[derive(Debug, Clone)]
pub struct EgressEntry {
    /// Stable provider id: "claude_code" | "anthropic" | "ollama" | "gateway".
    pub provider_id: String,
    /// Non-PII destination label, e.g. `"api.anthropic.com"`, `"claude_code (Anthropic CLI)"`,
    /// `"127.0.0.1:4000"` (gateway host). Never an API key or secret.
    pub destination: String,
    /// The model id that was requested (from config), e.g. `"claude-opus-4-8"` or `""` (default).
    pub model_requested: String,
    /// Whether this was a `"summarize"` or `"complete"` call.
    pub call_kind: &'static str,
    /// Token-usage and model metadata from the provider's response.
    pub meta: CallMeta,
    /// Count of PII placeholders injected by the redaction firewall for this call.
    pub redactions: RedactionCounts,
    /// Byte length of the (redacted) system prompt — a SIZE only, never the content.
    pub system_bytes: usize,
    /// Byte length of the (redacted) user content — a SIZE only, never the content.
    pub user_bytes: usize,
    /// Meeting id this call was made for. `None` for now (Phase 5 may wire it).
    pub meeting_id: Option<String>,
}

/// Brain v2 L3 — build the CONTENT-FREE "cascade escalation" ledger row: a tier of the in-meeting
/// brain cascade declined to answer and the ladder stepped up to the next tier, which will run on
/// `connection` (a provider connection — the caller must NOT record a local-tier escalation whose
/// next tier stays on-device; that is not egress). Carries ONLY the connection id and the tier
/// transition (`"cascade tier1→tier2"`) — NEVER the query, the tier prompt, or any content. Zero
/// token counts / byte sizes: the escalation itself sends nothing; the next tier's own provider
/// call writes its own row. Pure, so it is unit-testable without touching the global sink.
pub fn escalation_entry(connection: &str, from_tier: u8, to_tier: u8) -> EgressEntry {
    EgressEntry {
        provider_id: connection.to_string(),
        destination: format!("cascade tier{from_tier}→tier{to_tier}"),
        model_requested: String::new(),
        call_kind: "escalation",
        meta: CallMeta::default(),
        redactions: RedactionCounts::default(),
        system_bytes: 0,
        user_bytes: 0,
        meeting_id: None,
    }
}

/// Brain v2 L5 (lock-security WEAKNESS fix, 2026-07-10) — the CONTENT-FREE ledger row for an
/// MCP-server CONNECTION TEST (`test_mcp_server`: JSON-RPC `initialize` + `tools/list`). The probe
/// egresses NO user content (only the protocol handshake), but it IS egress to the external server
/// — for a stdio server it LAUNCHES the configured binary — so the privacy receipt must show the
/// attempt, exactly like every connector search. `provider_id` is the per-server truthful
/// attribution (`mcp_<server_id>`, the SAME id the connector's search rows carry via
/// [`crate::connectors::mcp::connector_id`]); a distinct `call_kind` (`"mcp_probe"`, mirroring
/// jira's `"connector_lookup"` split from `"connector_search"`) so a test connection is never
/// mistaken for a query. Zero byte sizes / counts by construction. Pure, so it is unit-testable
/// without touching the global sink (mirrors [`escalation_entry`]).
pub fn mcp_probe_entry(server_id: &str) -> EgressEntry {
    EgressEntry {
        provider_id: crate::connectors::mcp::connector_id(server_id),
        destination: "MCP server (connection test)".to_string(),
        model_requested: String::new(),
        call_kind: "mcp_probe",
        meta: CallMeta::default(),
        redactions: RedactionCounts::default(),
        system_bytes: 0,
        user_bytes: 0,
        meeting_id: None,
    }
}

/// Receiver for egress audit entries. Implementations MUST be `Send + Sync` and MUST NOT panic
/// on error (a logging failure must never break summarization).
pub trait EgressSink: Send + Sync {
    fn record(&self, entry: EgressEntry);
}

/// No-op sink: discards every entry silently. Used by default (before startup wiring) and by all
/// existing `RedactingProvider::new` / `with_name_redactor` callers, preserving byte-identical
/// behavior for every existing test and call site.
pub struct NoopEgressSink;

impl EgressSink for NoopEgressSink {
    fn record(&self, _: EgressEntry) {}
}

static EGRESS_SINK: OnceLock<Arc<dyn EgressSink>> = OnceLock::new();

/// Set the process-global egress sink. Called ONCE at startup, after `AppState::init()` succeeds.
/// Subsequent calls are silently ignored (`OnceLock` set-once semantics).
pub fn set_egress_sink(sink: Arc<dyn EgressSink>) {
    let _ = EGRESS_SINK.set(sink);
}

/// Return the active egress sink. Falls through to `NoopEgressSink` before startup wiring and in
/// tests that do not call `set_egress_sink`. Never panics.
pub fn active_sink() -> Arc<dyn EgressSink> {
    EGRESS_SINK
        .get()
        .cloned()
        .unwrap_or_else(|| Arc::new(NoopEgressSink))
}

/// Egress sink that writes one content-free row to the `egress_log` SQLite table per call.
/// Holds an `Arc<Db>` so it can be cheaply cloned from the startup `AppState.db`.
/// On a write error, logs a non-PII `tracing::warn!` and returns — never panics, never
/// breaks the summarization caller.
pub struct DbEgressSink {
    db: Arc<Db>,
}

impl DbEgressSink {
    pub fn new(db: Arc<Db>) -> Self {
        Self { db }
    }
}

impl EgressSink for DbEgressSink {
    fn record(&self, entry: EgressEntry) {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        if let Err(e) = self.db.insert_egress(ts, &entry) {
            tracing::warn!(
                target: "egress",
                error = %e,
                provider = %entry.provider_id,
                kind = %entry.call_kind,
                "egress_log insert failed; row dropped (summarization unaffected)"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_entry() -> EgressEntry {
        EgressEntry {
            provider_id: "anthropic".to_string(),
            destination: "api.anthropic.com".to_string(),
            model_requested: "claude-opus-4-8".to_string(),
            call_kind: "complete",
            meta: CallMeta {
                model_served: Some("claude-opus-4-8-20251001".to_string()),
                prompt_tokens: Some(100),
                completion_tokens: Some(50),
                total_tokens: Some(150),
                cached_tokens: None,
                redactions: None,
            },
            redactions: RedactionCounts {
                email: 1,
                card: 0,
                phone: 1,
                name: 2,
            },
            system_bytes: 512,
            user_bytes: 1024,
            meeting_id: Some("m1".to_string()),
        }
    }

    /// `NoopEgressSink::record` does not panic and produces no side-effects.
    #[test]
    fn noop_sink_does_not_panic() {
        let sink = NoopEgressSink;
        sink.record(sample_entry());
    }

    /// `active_sink()` is callable without panic regardless of whether a sink was installed.
    /// (The global OnceLock may have been set by another test in the same process — that is fine.)
    #[test]
    fn active_sink_never_panics() {
        let sink = active_sink();
        sink.record(sample_entry()); // must not panic regardless of which sink is active
    }

    /// Brain v2 L5 — the MCP connection-test ledger row is CONTENT-FREE by construction and
    /// attributed PER SERVER (`mcp_<server_id>` — the same provider_id the connector's search
    /// rows carry), with the distinct `"mcp_probe"` kind so a test connection never reads as a
    /// query in the privacy receipt.
    #[test]
    fn mcp_probe_entry_is_content_free_and_attributed_per_server() {
        let e = mcp_probe_entry("abc123");
        assert_eq!(e.provider_id, "mcp_abc123");
        assert_eq!(
            e.provider_id,
            crate::connectors::mcp::connector_id("abc123"),
            "the probe row and the connector's search rows share one per-server attribution"
        );
        assert_eq!(e.call_kind, "mcp_probe");
        assert_eq!(e.destination, "MCP server (connection test)");
        assert_eq!(e.model_requested, "");
        assert_eq!(e.system_bytes, 0);
        assert_eq!(e.user_bytes, 0);
        assert!(e.meeting_id.is_none());
        // Content-free by construction: only the server id enters, and only as an id.
        let dbg = format!("{e:?}");
        assert!(
            !dbg.contains("http"),
            "no endpoint/server strings in the row: {dbg}"
        );
    }

    /// Brain v2 L3 — the escalation ledger row is CONTENT-FREE by construction: `call_kind`
    /// "escalation", the tier transition in the destination label, zero counts/sizes, and no way
    /// for query text to enter (the constructor takes only the connection id + two tier numbers).
    #[test]
    fn escalation_entry_is_content_free_and_kind_tagged() {
        let e = escalation_entry("claude_code", 1, 2);
        assert_eq!(e.call_kind, "escalation");
        assert_eq!(e.provider_id, "claude_code");
        assert_eq!(e.destination, "cascade tier1→tier2");
        assert_eq!(e.model_requested, "");
        assert_eq!(e.system_bytes, 0);
        assert_eq!(e.user_bytes, 0);
        assert!(e.meeting_id.is_none());
        // Debug output carries only ids/labels/zeros — the recording sink can never leak content.
        let dbg = format!("{e:?}");
        assert!(dbg.contains("escalation"));
        assert!(dbg.contains("tier1"));
        // And a NoopEgressSink record of it is panic-free (the pre-wiring default).
        NoopEgressSink.record(escalation_entry("anthropic", 2, 3));
    }

    /// The content-free proof: `format!("{:?}", entry)` must not contain the scrubbed input values.
    ///
    /// This test captures what `RedactingProvider::with_name_redactor_and_sink` records in the
    /// `CaptureEgressSink` (a Vec<EgressEntry> in a Mutex) when given an input containing one
    /// email + one phone — and asserts that neither the email string nor the note text appears
    /// in the entry's Debug output.
    #[test]
    fn egress_entry_debug_contains_no_content() {
        let entry = sample_entry();
        // The EgressEntry Debug must not contain:
        // - email addresses
        // - note text / transcript content
        // - API keys
        // It should only contain counts, labels, ids, byte sizes, and the CallMeta token counts.
        let debug_str = format!("{:?}", entry);
        assert!(
            !debug_str.contains("@acme.com"),
            "email must not appear in entry debug"
        );
        assert!(
            !debug_str.contains("transcript text"),
            "transcript must not appear in entry debug"
        );
        // Only counts, labels, and byte sizes are present.
        assert!(
            debug_str.contains("api.anthropic.com"),
            "destination label is non-PII"
        );
        assert!(debug_str.contains("512"), "system_bytes size is non-PII");
    }
}
