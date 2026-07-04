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

/// One row of the `/people` personal-CRM list (`list_people`): a Person entity rolled up over
/// the EXISTING gated graph + facts + commitments readers. EVERY count here is VISIBLE-only —
/// a Person whose mentions/facts/commitments live solely in sealed-and-not-session-unlocked
/// meetings never surfaces (dropped by `list_entities_visible`'s `HAVING`), and its counts
/// reflect only visible sources. No new/ungated query feeds this DTO.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonCard {
    pub id: String,
    pub name: String,
    /// Number of VISIBLE meetings mentioning this person (sealed meetings drop out).
    pub meeting_count: i64,
    /// ISO 8601 start of the most-recent VISIBLE meeting that mentioned this person, or `None`
    /// when there is no visible mention (should not happen — a card is only built for a visible
    /// person, but kept fail-soft).
    pub last_talked: Option<String>,
    /// Open (`- [ ]`) action items across VISIBLE meetings owned by this person (name match).
    pub open_commitment_count: i64,
    /// Currently-valid (open) facts about this person from VISIBLE meetings.
    pub current_fact_count: i64,
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteRecord {
    pub meeting_id: String,
    pub provider_id: String,
    pub markdown: String,
    pub created_at: String,
    pub exported_path: Option<String>,
    /// Phase 5 provenance — the model id the pipeline REQUESTED (e.g. `"gpt-4o"`, `"claude-opus-4-8"`).
    /// `None` for notes created before this column was added (additive migration; legacy rows read back
    /// as `None`).
    pub model_requested: Option<String>,
    /// Phase 5 provenance — the model id the gateway/API ACTUALLY served (from `CallMeta.model_served`).
    /// May differ from `model_requested` when the gateway aliases, falls back, or load-balances.
    /// `None` when the provider did not return this in the response.
    pub model_served: Option<String>,
    /// Phase 5 provenance — the HOST portion of the gateway base URL, present only for the `gateway`
    /// provider (e.g. `"gw.example.com"`, `"127.0.0.1:4000"`). `None` for all other providers.
    pub gateway_host: Option<String>,
}

/// One persisted in-meeting voice-assistant interaction (Q&A): the user's spoken command, the
/// assistant's answer, the grounding citations, and the dispatch status. PERSISTED so the meeting
/// note can surface the assistant exchange that was previously ephemeral (only the live card). It is
/// DERIVED convenience data — purged (not sealed) when the meeting's folder is sealed, exactly like
/// `correction_log` / `note_chunks`; the underlying transcript is still sealed + restorable.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantInteraction {
    /// The HEARD command — the user's own dictated words ("Klaudku, sprawdź pogodę").
    pub command: String,
    /// The assistant's answer (the dispatch `summary`): research/recall result or a status line.
    pub answer: String,
    /// `[[Title]]` wikilink / "(web)" citations the answer was grounded on (VISIBLE meetings only).
    pub citations: Vec<String>,
    /// Dispatch status: `ok` | `unavailable` | `unrecognized` | `needs_consent` | `error` |
    /// `nothing_heard`.
    pub status: String,
    /// Coarse source label for the FE card style (the intent kind), e.g. `research` / `recall`.
    pub source_label: Option<String>,
    /// RFC3339 timestamp the interaction was recorded.
    pub created_at: String,
}

/// One persisted @brain THREAD exchange — the durable substrate the FE rebuilds its thread panels
/// from across meeting switches / restarts. Rows come from `assistant_interactions` and are
/// returned ONLY when they carry a `thread_id` (legacy voice rows are excluded). Like
/// [`AssistantInteraction`], it is DERIVED convenience data — purged (not sealed) when the
/// meeting's folder is sealed, and the read is visibility-gated (sealed-not-unlocked ⇒ empty).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantThreadRow {
    /// Opaque thread id: the FE-supplied @brain thread id, or a backend-generated UUID for the
    /// voice/wake path. Groups the exchanges of one conversation.
    pub thread_id: String,
    /// The note text the @brain thread was ANCHORED to (the ✨ ask-brain seed), when any.
    pub anchor_text: Option<String>,
    /// The user's LATEST message of that exchange (never the rendered conversation history).
    pub command: String,
    /// The assistant's answer for that exchange.
    pub answer: String,
    /// `[[Title]]` wikilink / "(web)" citations the answer was grounded on (VISIBLE meetings only).
    pub citations: Vec<String>,
    /// Dispatch status: `ok` | `unavailable` | `unrecognized` | `needs_consent` | `error` |
    /// `nothing_heard`.
    pub status: String,
    /// RFC3339 timestamp the exchange was recorded.
    pub created_at: String,
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

/// Lightweight metadata for one uploaded document — the FE list DTO. Carries NO text (the text is
/// gated content surfaced only by `get_document`, never in the list). `created_at` is epoch millis.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentInfo {
    pub id: String,
    pub name: String,
    /// `"document"` (uploaded file) or `"note"` (typed brain note) — lets the Brain page split the
    /// two source kinds. Both ride the same seal/gating; this is presentation only.
    pub kind: String,
    pub created_at: i64,
}

/// Headline counts + flags for the Brain page ("what's in my brain"). All counts are over
/// VISIBLE/unlocked content only (a sealed-not-unlocked folder's items are never counted). Carries
/// NO text — counts + the two semantic flags, so it is leak-free.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrainOverview {
    pub meeting_count: i64,
    pub document_count: i64,
    pub note_count: i64,
    pub indexed_chunk_count: i64,
    pub semantic_enabled: bool,
    pub embed_model_present: bool,
}

/// One gated document-chunk retrieval hit (the document analogue of [`SearchHit`], minus the
/// meeting): the nearest chunk's snippet + the source document name + its (visible) folder id.
/// Returned by `search_doc_chunks_visible` and folded into the brain/Ask grounding corpus.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocChunkHit {
    pub document_id: String,
    pub name: String,
    pub folder_id: String,
    pub snippet: String,
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
    /// The meeting this example was derived from (`None` for legacy/unattributed rows). LOCK-SAFETY:
    /// the gated reader (`Db::list_corrections`) joins this to `meetings`/`notes`/`folders` and only
    /// returns rows whose meeting is currently VISIBLE; a `None` here is treated as NOT visible
    /// (fail-closed). The seal/delete paths purge a meeting's rows, so a sealed meeting never
    /// contributes to the flywheel. `folder_id` is DERIVED via the join, never stored here.
    pub meeting_id: Option<String>,
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

/// One OPEN action item ("commitment") rolled up across the whole library, carrying its meeting
/// context. Produced by the deterministic `Db::list_open_commitments` aggregation: only OPEN
/// (`- [ ]`, not `- [x]`) items from VISIBLE meetings contribute — a sealed-and-not-unlocked
/// meeting yields nothing (excluded by both `list_meetings_visible` and `get_note_if_visible`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Commitment {
    pub meeting_id: String,
    pub meeting_title: String,
    /// ISO 8601 meeting start (used for recency ordering + the [[Title]] context).
    pub started_at: String,
    pub owner: Option<String>,
    pub due_date: Option<String>,
    pub text: String,
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
    /// ADDITIVE (PR G, ask-unify): the agentic loop's gated citation strings verbatim —
    /// `[[Title]]` vault wikilinks plus loud `(web)` / `(calendar)` attributions the structured
    /// `sources` chips can't carry. Empty on the corpus-floor path; FE may ignore it.
    #[serde(default)]
    pub citations: Vec<String>,
}

/// Result of generating a vault digest: the markdown + the path written into the vault.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DigestResult {
    pub markdown: String,
    pub exported_path: Option<String>,
}

/// An upcoming Calendar event (best-effort; absent if Calendar access is denied).
/// Minimal shape used by the legacy AppleScript `next_calendar_event` probe.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarEvent {
    pub title: String,
    pub start: Option<String>,
}

/// A full Calendar event surfaced by the bundled EventKit sidecar (`meetnotes-calendar`):
/// title + attendees + agenda/notes, so the brain / pre-meeting brief can use "who's in this
/// meeting + the agenda". On-device only — reading the local calendar adds no network egress.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CalendarEventFull {
    /// EventKit `eventIdentifier` — stable handle to fetch this event again (`calendar_context_for`).
    pub id: String,
    pub title: String,
    /// ISO-8601 start, or `None` if EventKit had no start date.
    pub start: Option<String>,
    /// ISO-8601 end, or `None`.
    pub end: Option<String>,
    /// Attendee display names (or email when there's no name). May be empty.
    pub attendees: Vec<String>,
    /// The event's agenda / notes body. May be empty.
    pub notes: String,
}

/// The envelope the `meetnotes-calendar` sidecar prints on stdout. `status` is always one of
/// `ok` / `denied` / `empty` / `error`; `events` is empty for everything but `ok`.
#[derive(Debug, Clone, Deserialize)]
pub struct CalendarSidecarEnvelope {
    pub status: String,
    #[serde(default)]
    pub events: Vec<CalendarEventFull>,
}

/// A compact calendar-context block attachable to a meeting so the existing pre-meeting brief /
/// note pre-analysis can consume it (the brain already takes context). Plain text + the source
/// event id; if this text reaches a cloud provider it MUST ride the existing make_provider
/// redaction firewall + consent — it is NEVER a new egress path.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CalendarContext {
    /// Source EventKit event id (empty if assembled from a non-EventKit event).
    pub event_id: String,
    pub title: String,
    pub attendees: Vec<String>,
    /// A short, human-readable context block: title + attendees + agenda. This is what the brain
    /// consumes; keep it bounded.
    pub text: String,
}

impl CalendarContext {
    /// Assemble a bounded context block from a full calendar event. Pure + deterministic so it's
    /// unit-testable headless (no EventKit needed).
    pub fn from_event(e: &CalendarEventFull) -> Self {
        let mut text = String::new();
        text.push_str("Meeting: ");
        text.push_str(if e.title.is_empty() {
            "(untitled)"
        } else {
            &e.title
        });
        if let Some(start) = &e.start {
            text.push_str("\nWhen: ");
            text.push_str(start);
            if let Some(end) = &e.end {
                text.push_str(" – ");
                text.push_str(end);
            }
        }
        if !e.attendees.is_empty() {
            text.push_str("\nAttendees: ");
            text.push_str(&e.attendees.join(", "));
        }
        let agenda = e.notes.trim();
        if !agenda.is_empty() {
            // Bound the agenda so a giant notes field can't bloat the prompt / leak surface.
            const MAX_AGENDA: usize = 2000;
            text.push_str("\nAgenda:\n");
            if agenda.len() > MAX_AGENDA {
                text.push_str(&agenda.chars().take(MAX_AGENDA).collect::<String>());
                text.push('…');
            } else {
                text.push_str(agenda);
            }
        }
        CalendarContext {
            event_id: e.id.clone(),
            title: e.title.clone(),
            attendees: e.attendees.clone(),
            text,
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn full_event() -> CalendarEventFull {
        CalendarEventFull {
            id: "E1".into(),
            title: "Sprint Planning".into(),
            start: Some("2026-06-28T10:00:00Z".into()),
            end: Some("2026-06-28T11:00:00Z".into()),
            attendees: vec!["Alice".into(), "bob@example.com".into()],
            notes: "Agenda:\n- velocity\n- scope".into(),
        }
    }

    #[test]
    fn calendar_context_assembles_full_block() {
        let ctx = CalendarContext::from_event(&full_event());
        assert_eq!(ctx.event_id, "E1");
        assert_eq!(ctx.title, "Sprint Planning");
        assert_eq!(ctx.attendees, vec!["Alice", "bob@example.com"]);
        assert!(ctx.text.contains("Meeting: Sprint Planning"));
        assert!(ctx
            .text
            .contains("When: 2026-06-28T10:00:00Z – 2026-06-28T11:00:00Z"));
        assert!(ctx.text.contains("Attendees: Alice, bob@example.com"));
        assert!(ctx.text.contains("Agenda:"));
        assert!(ctx.text.contains("velocity"));
    }

    #[test]
    fn calendar_context_handles_sparse_event() {
        let e = CalendarEventFull {
            id: String::new(),
            title: String::new(),
            start: None,
            end: None,
            attendees: vec![],
            notes: String::new(),
        };
        let ctx = CalendarContext::from_event(&e);
        // No panic; untitled placeholder; no When/Attendees/Agenda sections.
        assert!(ctx.text.contains("Meeting: (untitled)"));
        assert!(!ctx.text.contains("When:"));
        assert!(!ctx.text.contains("Attendees:"));
        assert!(!ctx.text.contains("Agenda:"));
    }

    #[test]
    fn calendar_context_bounds_giant_agenda() {
        let mut e = full_event();
        e.notes = "x".repeat(5000);
        let ctx = CalendarContext::from_event(&e);
        // Bounded to MAX_AGENDA (2000) + an ellipsis marker; never the full 5000.
        assert!(ctx.text.contains('…'));
        assert!(ctx.text.len() < 2200);
    }

    #[test]
    fn calendar_context_start_without_end() {
        let mut e = full_event();
        e.end = None;
        let ctx = CalendarContext::from_event(&e);
        assert!(ctx.text.contains("When: 2026-06-28T10:00:00Z"));
        assert!(!ctx.text.contains(" – "));
    }
}
