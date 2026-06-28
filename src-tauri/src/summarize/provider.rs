use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::Result;

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
}
