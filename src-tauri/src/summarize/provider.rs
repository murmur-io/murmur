use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Result;
use crate::summarize::meta::CallMeta;

/// The curated Claude model ids offered for the `claude_code` and `anthropic` connections.
///
/// Single source of truth for the FE model dropdowns, served through the `list_models` command —
/// this list previously lived hardcoded in the Settings template. Order is display order (most
/// capable first). Static compile-time data — no I/O, no egress.
pub const CLAUDE_MODELS: &[&str] = &["claude-opus-4-8", "claude-sonnet-5", "claude-haiku-4-5"];

/// Curated Codex model ids offered for the `codex_cli` connection.
///
/// These are the current Sol/Terra/Luna roles exposed by Codex CLI: highest-quality, balanced,
/// and fast respectively. Static compile-time data; selecting one passes it verbatim to
/// `codex exec --model`.
pub const CODEX_MODELS: &[&str] = &["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingMeta {
    /// e.g. "2026-06-24"
    pub date_iso: String,
    pub title_hint: Option<String>,
    pub duration_s: i64,
    pub language: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SummarizeRequest {
    pub transcript: String,
    pub meta: MeetingMeta,
    /// note-format prompt (summarize/template.rs)
    pub template: String,
    /// existing note titles → [[link]] targets
    pub vault_titles: Vec<String>,
    /// brain2 RAG Phase 4 — a small, GATED corpus of related PRIOR notes (each headed by a
    /// `### [[Title]] · date · id:` citation) so the model can ground the new note in past
    /// decisions/owed items. `None` (the default + flag-OFF case) ⇒ `render_user_content` is
    /// byte-identical to before this field existed. SECURITY: this string EGRESSES to the cloud
    /// provider in the summarization prompt, so it MUST be assembled only from VISIBLE
    /// (not sealed-not-unlocked) prior notes — see `summarize::related_context::build_related_context`.
    /// It is redacted by `RedactingProvider` alongside the transcript before egress.
    pub related_context: Option<String>,
    /// ENHANCE-MY-NOTES: the user's own typed in-meeting notes (the `manual_notes` buffer —
    /// raw `\n`-joined lines, NOT markdown bullets), present ONLY when `notes_mode == "enhance"`
    /// AND the buffer is non-blank. `None` ⇒ `render_user_content` is byte-identical to before
    /// this field existed (same contract as `related_context`). SECURITY: this string EGRESSES
    /// to the provider in the prompt — `RedactingProvider` MUST scrub it alongside the
    /// transcript (summarize/redact.rs) before egress. Today's append mode never egresses it.
    pub user_notes: Option<String>,
    /// Brain v2 L4 — the RUNNING LIVE BULLETS captured during the recording
    /// (`transcribe::bullets`), rendered by `render_user_content` as the "LIVE NOTES (auto)"
    /// section BEFORE the transcript. `None`/blank (no recording bullets — the default, the
    /// flag-off case, and every resummarize after the Stop-time consume) ⇒ the rendered prompt is
    /// byte-identical to before this field existed (same contract as `user_notes`). SECURITY:
    /// this string EGRESSES to the provider in the prompt — `RedactingProvider` MUST scrub it
    /// alongside the transcript (summarize/redact.rs) before egress.
    pub live_bullets: Option<String>,
    /// Bounded, structured workspace glossary rendered locally from Settings. `None` means the
    /// user prompt is byte-identical to the pre-glossary prompt. When present this EGRESSES in the
    /// note-generation prompt and MUST pass the same shared regex + name-redaction batch as the
    /// transcript. It is never included in the separate graph-extraction call.
    pub glossary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Availability {
    Available,
    Unavailable { reason: String },
}

/// Canonical system prompt used by the trait's free-text JSON fallback. Kept as a helper so the
/// egress ledger can measure the exact bytes the default path sends without reimplementing it.
pub(crate) fn default_json_system_prompt(system: &str, schema: &Value) -> String {
    format!(
        "{system}\n\nRespond with ONLY a single JSON object matching this schema (no prose, no code fences):\n{}",
        serde_json::to_string(schema).unwrap_or_default()
    )
}

#[async_trait]
pub trait SummarizerProvider: Send + Sync {
    /// Stable id: "claude_code" | "codex_cli" | "anthropic" | "ollama" | "gateway".
    fn id(&self) -> &str;

    /// Cheap, non-failing readiness probe (key set? ollama up? claude in PATH?).
    async fn availability(&self) -> Availability;

    /// Produce finished Obsidian-ready Markdown from the request.
    async fn summarize(&self, req: &SummarizeRequest) -> Result<String>;

    /// Raw completion: run a system + user prompt and return the model's text verbatim
    /// (no formatting/validation). Used for structured side-tasks like the timeline.
    async fn complete(&self, system: &str, user: &str) -> Result<String>;

    /// Like [`summarize`] but also returns token-usage and model metadata from the provider.
    ///
    /// Default implementation delegates to [`summarize`] and returns an empty [`CallMeta`].
    /// Providers that capture `usage` + `model` from their API response override this
    /// (and delegate [`summarize`] to this, stripping the meta) so the HTTP call is not
    /// duplicated.
    async fn summarize_with_meta(&self, req: &SummarizeRequest) -> Result<(String, CallMeta)> {
        Ok((self.summarize(req).await?, CallMeta::default()))
    }

    /// Like [`complete`] but also returns token-usage and model metadata from the provider.
    ///
    /// Default implementation delegates to [`complete`] and returns an empty [`CallMeta`].
    /// Providers that capture `usage` + `model` override this.
    async fn complete_with_meta(&self, system: &str, user: &str) -> Result<(String, CallMeta)> {
        Ok((self.complete(system, user).await?, CallMeta::default()))
    }

    /// Like [`complete_with_meta`] but carries [`crate::reason::GenOptions`] — a per-call token cap /
    /// sampler override for the on-device path (the note-edit runaway guard).
    ///
    /// DEFAULT = IGNORE the options and delegate to [`complete_with_meta`]. The cloud
    /// ([`super::redact::RedactingProvider`]) and other non-local providers have no local sampler to
    /// bound and keep their own limits + redaction — so ignoring the opts is correct and non-breaking.
    /// [`super::local::LocalSummarizerProvider`] OVERRIDES this to honor the cap on the GGUF path.
    async fn complete_with_meta_opts(
        &self,
        system: &str,
        user: &str,
        _opts: crate::reason::GenOptions,
    ) -> Result<(String, CallMeta)> {
        self.complete_with_meta(system, user).await
    }

    /// Structured-output completion returning the JSON value AND the provider's `CallMeta`
    /// (token usage + served model).
    ///
    /// DEFAULT = schema-in-prompt + `parse_first_json`, with meta from `complete_with_meta`.
    /// A provider that supports native constrained decoding (the OpenAI-compatible gateway)
    /// OVERRIDES this to send `response_format: {"type":"json_schema", …}` and returns both
    /// the parsed value and real token/model metadata directly.
    async fn complete_json_with_meta(
        &self,
        system: &str,
        user: &str,
        schema: &Value,
    ) -> Result<(Value, CallMeta)> {
        // Default: embed the schema as a system-prompt instruction and call complete_with_meta
        // so token usage is captured even on the free-text path.
        let sys = default_json_system_prompt(system, schema);
        let (reply, meta) = self.complete_with_meta(&sys, user).await?;
        let v = crate::reason::parse_first_json::<Value>(&reply)?;
        Ok((v, meta))
    }

    /// Structured-output completion: return a JSON value adhering to `schema`.
    ///
    /// Delegates to [`complete_json_with_meta`] and drops the `CallMeta` so callers that
    /// only need the value are unchanged. Providers that support native constrained decoding
    /// override [`complete_json_with_meta`] instead; this default is then correct automatically.
    async fn complete_json(&self, system: &str, user: &str, schema: &Value) -> Result<Value> {
        Ok(self.complete_json_with_meta(system, user, schema).await?.0)
    }

    /// Does this provider enforce valid JSON NATIVELY (constrained decoding — e.g. the
    /// OpenAI-compatible `response_format: {"type":"json_schema", …}`) rather than
    /// schema-in-prompt + [`crate::reason::parse_first_json`] recovery?
    ///
    /// Brain v2 L3 structured-output hardening: a CAPABILITY SEAM ONLY for now — nothing
    /// dispatches on it yet (per spec §L3, `CloudReasoner` keeps its current path until the
    /// shadow data justifies a cutover). Default `false`; the gateway provider (which already
    /// sends `response_format` json_schema in `complete_json_with_meta`) overrides to `true`.
    fn supports_native_json(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Task 2.2 — a provider that does NOT override `*_with_meta` gets an empty `CallMeta`
    /// from the default implementations. This proves the additive default is wired correctly
    /// without touching any existing provider.
    struct FixedProvider(&'static str);

    #[async_trait]
    impl SummarizerProvider for FixedProvider {
        fn id(&self) -> &str {
            "fixed"
        }
        async fn availability(&self) -> Availability {
            Availability::Available
        }
        async fn summarize(&self, _req: &SummarizeRequest) -> Result<String> {
            Ok(self.0.to_string())
        }
        async fn complete(&self, _system: &str, _user: &str) -> Result<String> {
            Ok(self.0.to_string())
        }
    }

    /// Brain v2 L3 — `supports_native_json` DEFAULTS to false: a provider that does not override
    /// it is on the schema-in-prompt + `parse_first_json` recovery path (only the gateway, which
    /// sends `response_format: json_schema`, overrides to true — tested in `gateway.rs`).
    #[test]
    fn supports_native_json_defaults_to_false() {
        assert!(!FixedProvider("x").supports_native_json());
    }

    /// `complete_with_meta` default returns the plain `complete` output paired with empty `CallMeta`.
    #[tokio::test]
    async fn default_complete_with_meta_returns_empty_call_meta() {
        let p = FixedProvider("hello");
        let (text, meta) = p.complete_with_meta("sys", "usr").await.unwrap();
        assert_eq!(text, "hello");
        assert_eq!(
            meta,
            CallMeta::default(),
            "default impl must return empty CallMeta"
        );
    }

    /// Task 8.1 — `complete_json` default extracts the first JSON object from a prose-wrapped /
    /// fenced reply (the free-text path). A provider that does NOT override `complete_json` gets
    /// the schema embedded in the system prompt and the JSON extracted via `parse_first_json`.
    #[tokio::test]
    async fn default_complete_json_extracts_first_json_from_free_text() {
        // FixedProvider returns whatever string was given as its inner &str — simulate a model
        // that wraps the JSON in prose and a code fence (the worst-case free-text reply).
        struct JsonFencedProvider;
        #[async_trait]
        impl SummarizerProvider for JsonFencedProvider {
            fn id(&self) -> &str {
                "json-fenced"
            }
            async fn availability(&self) -> Availability {
                Availability::Available
            }
            async fn summarize(&self, _req: &SummarizeRequest) -> Result<String> {
                Ok(String::new())
            }
            async fn complete(&self, _system: &str, _user: &str) -> Result<String> {
                // Simulate a chatty model that wraps output in prose + a code fence.
                Ok("Sure! Here is the JSON:\n```json\n{\"people\":[\"Alice\"],\"projects\":[]}\n```\nLet me know if you need changes.".to_string())
            }
        }
        let schema = serde_json::json!({"type":"object","properties":{"people":{"type":"array"},"projects":{"type":"array"}}});
        let v = JsonFencedProvider
            .complete_json("SYS", "USER", &schema)
            .await
            .unwrap();
        assert_eq!(
            v["people"][0], "Alice",
            "default impl must extract JSON from fenced/prose reply"
        );
        assert!(v["projects"].as_array().unwrap().is_empty());
    }

    /// Task 8.1 — `complete_json` default returns an error when the reply contains no JSON object.
    #[tokio::test]
    async fn default_complete_json_errors_on_no_json() {
        struct NoJsonProvider;
        #[async_trait]
        impl SummarizerProvider for NoJsonProvider {
            fn id(&self) -> &str {
                "no-json"
            }
            async fn availability(&self) -> Availability {
                Availability::Available
            }
            async fn summarize(&self, _req: &SummarizeRequest) -> Result<String> {
                Ok(String::new())
            }
            async fn complete(&self, _system: &str, _user: &str) -> Result<String> {
                Ok("I'm sorry, I cannot produce JSON right now.".to_string())
            }
        }
        let schema = serde_json::json!({});
        let err = NoJsonProvider.complete_json("SYS", "USER", &schema).await;
        assert!(
            err.is_err(),
            "no JSON in reply must yield an error from the default impl"
        );
    }

    /// `summarize_with_meta` default returns the plain `summarize` output paired with empty `CallMeta`.
    #[tokio::test]
    async fn default_summarize_with_meta_returns_empty_call_meta() {
        let p = FixedProvider("note body");
        let req = SummarizeRequest {
            transcript: "t".into(),
            meta: MeetingMeta {
                date_iso: "2026-06-30".into(),
                title_hint: None,
                duration_s: 60,
                language: None,
            },
            template: String::new(),
            vault_titles: vec![],
            related_context: None,
            user_notes: None,
            live_bullets: None,
            glossary: None,
        };
        let (text, meta) = p.summarize_with_meta(&req).await.unwrap();
        assert_eq!(text, "note body");
        assert_eq!(
            meta,
            CallMeta::default(),
            "default impl must return empty CallMeta"
        );
    }
}
