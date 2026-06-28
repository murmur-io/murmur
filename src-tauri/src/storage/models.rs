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

/// The two entity kinds the self-assembling graph resolves from meeting notes.
/// Stored as a stable lowercase string in `entities.kind` (mirrors `MeetingStatus`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub enum EntityKind {
    Person,
    Project,
}

/// A graph entity row (a person or project) — internal/DB-shaped, with its
/// first-seen casing preserved in `name`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphEntity {
    pub id: String,
    pub name: String,
    pub kind: EntityKind,
    pub created_at: String,
}

/// A graph node = an entity plus its VISIBLE mention count (sealed-and-not-unlocked
/// meetings contribute zero). The directory + neighborhood views render these.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphNode {
    pub id: String,
    pub name: String,
    pub kind: EntityKind,
    /// VISIBLE mention count — never the true count; sealed meetings drop out.
    pub mention_count: i64,
}

/// An undirected co-occurrence edge between two entities sharing ≥1 VISIBLE meeting.
/// `source`/`target` are entity ids with `source < target` (dedup), `weight` = shared count.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphEdge {
    pub source: String,
    pub target: String,
    pub weight: i64,
}

/// The full graph payload returned by `get_graph`: every visible node + every visible edge.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphData {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    /// True when ≥1 folder is sealed-and-not-unlocked → some entities/mentions may be hidden.
    /// The FE renders one honest disclosure banner; the count itself is never leaked.
    pub has_hidden: bool,
}

/// A co-occurring neighbor of a selected entity (the neighborhood satellites), with the
/// number of VISIBLE meetings the two share.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntityNeighbor {
    pub id: String,
    pub name: String,
    pub kind: EntityKind,
    pub shared_meetings: i64,
}

/// The detail payload for one entity: the entity, its visible backlinked meetings
/// (reusing the `VaultSource` chip shape), and its top co-occurring neighbors.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntityDetail {
    pub entity: GraphEntity,
    /// Visible meetings mentioning this entity (sealed-not-unlocked meetings excluded).
    pub meetings: Vec<VaultSource>,
    pub neighbors: Vec<EntityNeighbor>,
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
    /// Owning folder id (from the meeting's note rows), or `None` when at the vault root.
    /// Derived from `notes.folder_id` — a meeting's folder = its note's folder.
    pub folder_id: Option<String>,
}

/// A vault folder Murmur tracks for organization + per-folder locking.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Folder {
    pub id: String,
    pub name: String,
    pub path: String,
    pub parent_id: Option<String>,
    pub locked: bool,
    pub created_at: String,
}

/// A folder node for the tree UI: note count + current session lock state + children.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderNode {
    pub id: String,
    pub name: String,
    pub parent_id: Option<String>,
    pub note_count: usize,
    /// Folder is sealed (encrypted) on disk.
    pub locked: bool,
    /// Sealed AND unlocked in the current session (decrypted for view + MCP until relock).
    pub unlocked: bool,
    pub children: Vec<FolderNode>,
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

/// One row of the local correction-log "flywheel" (`correction_log`): a single
/// model-output→human-correction example captured for later on-device fine-tuning (LoRA). Local +
/// SQLCipher-encrypted like the rest of the DB; never egresses. `final_output` is `None` until the
/// user edits the model output; `accepted` records whether the model output was kept as-is.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CorrectionRecord {
    pub id: i64,
    /// Task discriminator, e.g. "ner" | "timeline" | "summary" — groups examples per model head.
    pub kind: String,
    /// The model's input (prompt / source text).
    pub input: String,
    /// What the model produced.
    pub model_output: String,
    /// The human-corrected output, if the user edited it (else `None`).
    pub final_output: Option<String>,
    /// True iff the model output was accepted unchanged.
    pub accepted: bool,
    /// Owner scope; "local" for the single-user on-device dataset.
    pub owner_id: String,
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
