use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MeetingStatus {
    Draft,
    Recording,
    Transcribed,
    Summarized,
    Exported,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Meeting {
    /// uuid
    pub id: String,
    /// ISO 8601
    pub started_at: String,
    pub ended_at: Option<String>,
    pub title: Option<String>,
    pub duration_s: i64,
    pub audio_path: Option<String>,
    pub status: MeetingStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteRecord {
    pub meeting_id: String,
    pub provider_id: String,
    pub markdown: String,
    pub created_at: String,
    pub exported_path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusCount {
    pub status: String,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DayCount {
    /// "YYYY-MM-DD"
    pub date: String,
    pub count: i64,
    pub duration_s: i64,
}

/// Aggregate stats for the dashboard + Analytics tab.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Analytics {
    pub total_meetings: i64,
    pub total_duration_s: i64,
    pub avg_duration_s: i64,
    pub longest_duration_s: i64,
    pub meetings_7d: i64,
    pub duration_7d_s: i64,
    pub notes_count: i64,
    pub first_meeting_at: Option<String>,
    pub by_status: Vec<StatusCount>,
    /// Per-day activity for the last ~30 days (only days with meetings).
    pub per_day: Vec<DayCount>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeakerTurn {
    pub speaker: String,
    #[serde(alias = "start", alias = "start_s")]
    pub start_s: f64,
    #[serde(alias = "end", alias = "end_s")]
    pub end_s: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TopicSpan {
    pub label: String,
    #[serde(alias = "start", alias = "start_s")]
    pub start_s: f64,
    #[serde(alias = "end", alias = "end_s")]
    pub end_s: f64,
}

/// Speaker turns + topic spans for the interactive meeting timeline (AI-derived, since
/// Whisper doesn't diarize). Cached per meeting once generated.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingTimeline {
    #[serde(default)]
    pub speakers: Vec<SpeakerTurn>,
    #[serde(default)]
    pub topics: Vec<TopicSpan>,
}

/// One search result: the matched meeting + a snippet and which field matched.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub meeting: Meeting,
    pub snippet: String,
    /// "title" | "transcript" | "note"
    pub matched_in: String,
}

/// One turn in a meeting chat conversation. `role` is "user" | "assistant".
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatTurn {
    pub role: String,
    pub content: String,
}

/// A built-in recipe (prompt template) shown as a quick chip.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuiltinRecipe {
    pub id: String,
    pub label: String,
    pub prompt: String,
}

/// A user-saved recipe (prompt template) persisted in the DB.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecipeRecord {
    pub id: String,
    pub title: String,
    pub prompt: String,
    pub created_at: String,
}

/// One parsed action-item checklist line from a note's "## Action items" section.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionItem {
    pub idx: usize,
    pub done: bool,
    pub text: String,
    pub owner: Option<String>,
    pub due_date: Option<String>,
}

/// Result of pinning a meeting moment: the ^block-ref id + an obsidian:// deep link.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PinResult {
    pub url: String,
    pub block_id: String,
    pub mmss: String,
}

/// A meeting referenced as a source in an Ask-My-Vault answer.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultSource {
    pub meeting_id: String,
    pub title: String,
    pub started_at: String,
}

/// Result of an Ask-My-Vault query: the grounded answer + the source meetings used.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AskVaultResult {
    pub answer: String,
    pub sources: Vec<VaultSource>,
}

/// Result of generating a vault digest: the markdown + the path written into the vault.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DigestResult {
    pub markdown: String,
    pub exported_path: Option<String>,
}

/// An upcoming Calendar event (best-effort; absent if Calendar access is denied).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarEvent {
    pub title: String,
    pub start: Option<String>,
}

/// A pre-meeting brief: grounded markdown + the source meetings used.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BriefResult {
    pub markdown: String,
    pub sources: Vec<VaultSource>,
}

/// One occurrence of a topic in a meeting (a node in a Topic Thread).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TopicMention {
    pub meeting_id: String,
    pub title: String,
    pub started_at: String,
    pub start_s: f64,
    pub end_s: f64,
}

/// A cross-meeting topic thread: every mention of a topic across the whole library.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TopicThread {
    pub label: String,
    pub count: usize,
    pub mentions: Vec<TopicMention>,
}
