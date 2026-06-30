use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::summarize::meta::CallMeta;

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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Availability {
    Available,
    Unavailable { reason: String },
}

#[async_trait]
pub trait SummarizerProvider: Send + Sync {
    /// Stable id: "claude_code" | "anthropic" | "ollama".
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
    async fn summarize_with_meta(
        &self,
        req: &SummarizeRequest,
    ) -> Result<(String, CallMeta)> {
        Ok((self.summarize(req).await?, CallMeta::default()))
    }

    /// Like [`complete`] but also returns token-usage and model metadata from the provider.
    ///
    /// Default implementation delegates to [`complete`] and returns an empty [`CallMeta`].
    /// Providers that capture `usage` + `model` override this.
    async fn complete_with_meta(
        &self,
        system: &str,
        user: &str,
    ) -> Result<(String, CallMeta)> {
        Ok((self.complete(system, user).await?, CallMeta::default()))
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

    /// `complete_with_meta` default returns the plain `complete` output paired with empty `CallMeta`.
    #[tokio::test]
    async fn default_complete_with_meta_returns_empty_call_meta() {
        let p = FixedProvider("hello");
        let (text, meta) = p.complete_with_meta("sys", "usr").await.unwrap();
        assert_eq!(text, "hello");
        assert_eq!(meta, CallMeta::default(), "default impl must return empty CallMeta");
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
        };
        let (text, meta) = p.summarize_with_meta(&req).await.unwrap();
        assert_eq!(text, "note body");
        assert_eq!(meta, CallMeta::default(), "default impl must return empty CallMeta");
    }
}
