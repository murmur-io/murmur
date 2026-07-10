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

/// Brain v2 L2.1 — one memory-consolidation rollup row (`memory_rollups`): cross-meeting synthesis
/// for one reflection scope (`entity:<id>` or `weekly:<YYYY-WNN>`). Lock model: ALL rollup rows are
/// purged inside every seal transaction (and their exported `.md`s deleted by the caller), and the
/// hourly pass re-reflects/GCs any rollup whose VISIBLE fact set changed (`fact_set_hash`) — see
/// `crate::memory`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryRollup {
    pub id: String,
    pub scope: String,
    pub content: String,
    pub created_at: String,
    pub updated_at: String,
    /// Absolute vault `.md` path this rollup was last exported to (`None` until exported).
    pub exported_path: Option<String>,
    /// Deterministic hash of the SORTED visible-open-fact id set the content was synthesized from
    /// (`crate::memory::fact_set_hash`); `None` on rows written before the hash existed — treated
    /// as "changed" so they re-reflect on the next pass.
    pub fact_set_hash: Option<String>,
}

/// Brain v2 L2.1 — one memory score row (`memory_scores`): the deterministic components + the
/// composite for one OPEN user fact. CONTENT-FREE (ids + floats); cascades off `user_facts`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryScore {
    pub fact_id: String,
    pub scope: String,
    pub recency: f64,
    pub importance: f64,
    pub relevance: f64,
    pub composite: f64,
    pub scored_at: String,
}

/// Brain v2 L5 — one SCHEDULED-BRIEF definition (`brief_schedules`): a structured local-time
/// schedule (NO cron syntax / crate), a lookback window, and an optional user prompt hint. The
/// runner (`crate::brief_runner`) fires it at most ONCE per local day.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BriefSchedule {
    pub id: String,
    /// User-authored display label (e.g. "Monday kickoff"). Config data, not meeting content.
    pub label: String,
    /// ISO weekday the brief fires on: 0 = Monday … 6 = Sunday
    /// (`chrono::Weekday::num_days_from_monday`). `None` = daily.
    pub day_of_week: Option<i64>,
    /// Local wall-clock hour (0–23) the brief becomes due.
    pub hour_local: i64,
    /// Local wall-clock minute (0–59) the brief becomes due.
    pub minute_local: i64,
    /// How many days back the brief's corpus window reaches (default 7).
    pub scope_days: i64,
    /// Optional user focus hint appended to the synthesis prompt (user-authored config).
    pub prompt_hint: Option<String>,
    pub enabled: bool,
    /// The LOCAL date (`YYYY-MM-DD`) this schedule last ran — the once-per-day guard.
    pub last_run_at: Option<String>,
    pub created_at: String,
}

/// Brain v2 L5 — one PROPOSED brief run (`brief_runs`, the propose-accept staging row). `note_md`
/// is synthesized from VISIBLE-ONLY content (the runner reads with the EMPTY unlock set, like the
/// memory consolidation job), so it cannot contain sealed content by construction; it is CONSUMED
/// (blanked) on accept. `meeting_ids` are opaque ids only.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BriefRun {
    pub id: String,
    pub schedule_id: String,
    /// "pending" | "accepted" (dismissed rows are DELETED).
    pub status: String,
    /// The proposed brief markdown; blanked once accepted (the vault `.md` becomes the copy).
    pub note_md: String,
    /// The source meeting ids the corpus was built from (ids only — never content).
    pub meeting_ids: Vec<String>,
    pub proposed_at: String,
    pub accepted_at: Option<String>,
}

/// Brain v2 L5 — one configured external MCP server (`mcp_servers`). `consented` is the
/// per-server egress consent flag (preserve-only, flipped solely by `consent_to_mcp_server` /
/// `revoke_mcp_consent`); an unconsented or disabled server is fail-closed ABSENT from the
/// connector registry and the brain's tool list. Carries connection config only — never results.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServer {
    /// Opaque id (hyphen-free so it embeds in the `mcp_<id>_query` tool name).
    pub id: String,
    /// User-authored display label.
    pub label: String,
    /// "http" (JSON-RPC over HTTP) or "stdio" (a local process — CODE EXECUTION; absolute path only).
    pub transport: String,
    /// The HTTP endpoint URL, or the ABSOLUTE stdio command path.
    pub endpoint: String,
    /// stdio command arguments (empty for http).
    pub args: Vec<String>,
    pub enabled: bool,
    /// One-time per-server egress consent — default FALSE, fail-closed.
    pub consented: bool,
    pub created_at: String,
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

/// Re-Truth (the vault heals itself) — one SUPERSESSION row: a fact asserted in `superseding_meeting_id`
/// INVALIDATED an older fact sourced in `source_meeting_id`. Surfaced for REVIEW; the user one-taps to
/// APPEND an Obsidian callout to the source note (never mangling prose). Append-only is the safe
/// verify-before-destroy shape: no existing bytes are touched, and the exact pre-image bytes of each
/// stamped note are captured (`*_pre_image`) so `undo_supersessions` restores them BYTE-IDENTICAL —
/// the owned-files promise. `applied_at` NULL ⇒ pending; `Some` ⇒ stamped. This is DERIVED,
/// meeting-anchored state: `delete_meeting` purges every row referencing either meeting
/// (`purge_supersessions_tx`), and the read command is folder-lock + `meeting_is_unlocked` gated.
#[derive(Debug, Clone, PartialEq)]
pub struct SupersessionRow {
    pub id: String,
    /// The NEWER meeting whose fact superseded the older one (the review anchor).
    pub superseding_meeting_id: String,
    /// The OLDER meeting whose note sourced the now-invalidated fact (the note we stamp).
    pub source_meeting_id: String,
    /// The fact's subject (entity name), e.g. "Project Atlas".
    pub entity: String,
    pub predicate: String,
    /// The value that WAS true (now closed).
    pub old_value: String,
    /// The value that IS true (the superseding assertion).
    pub new_value: String,
    pub created_at: String,
    /// `None` while pending; the stamp instant once applied.
    pub applied_at: Option<String>,
    /// Exact source-note bytes captured at apply, for byte-identical undo. `None` until applied.
    pub source_pre_image: Option<Vec<u8>>,
    /// Exact superseding-note bytes captured at apply (when its backlink was stamped). `None`
    /// until applied, or when the superseding note's folder was locked at apply (skipped).
    pub superseding_pre_image: Option<Vec<u8>>,
}

/// A DURABLE resume record for a mode-B share whose server row was flipped to `accepted` but whose
/// local verify+ingest has NOT yet committed (spec §7 accept invariant). Persisted between the server
/// flip and the vault write so a post-flip failure (network / crash) is RECOVERABLE: the server no
/// longer lists an accepted share in the inbox and a re-accept 404s, so without this the share would
/// be stranded (gone from the inbox, un-re-acceptable). Carries only what `finalize_accepted_share`
/// needs to re-fetch (the blob stays fetchable while `accepted`) + re-verify + ingest. Dropped the
/// instant the ingest commits. `wrapped_key`/`grant_sig` are the same opaque server-relayed bytes the
/// inbox already carried (no new secret at rest); the whole row is SQLCipher-encrypted like the rest.
#[derive(Debug, Clone)]
pub struct PendingShareAccept {
    pub share_id: String,
    /// The content-cell blob id returned by the server `accept` (fetch is authorized while `accepted`).
    pub blob_id: String,
    /// The write-gated target folder the note lands in (re-gated on resume in case it was sealed since).
    pub target_folder_id: String,
    /// The sender's STABLE server account id (TOFU namespace).
    pub sender_user_id: String,
    /// The sender's attested safety-word fingerprint (re-attested on resume).
    pub sender_fingerprint: String,
    /// HPKE-wrapped NK to us + packed sender identity (opaque; re-unpacked + §4.8-verified on resume).
    pub wrapped_key: Vec<u8>,
    /// The sender's detached Ed25519 grant signature (verified CLIENT-side on ingest).
    pub grant_sig: Vec<u8>,
    pub rev: u32,
    pub key_generation: u32,
    pub created_at: String,
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

/// A standalone authored note — the LIST-row DTO (leak-free: no body for a sealed note). A note is a
/// `documents(kind='note')` row. `title` falls back to `name` when the `title` column is NULL;
/// `updatedAt` falls back to `createdAt` when `updated_at` is NULL. When the owning folder is
/// sealed-and-not-session-unlocked the COMMAND layer returns a MASKED value (`locked: true`,
/// `title: "🔒 Locked"`, empty `snippet`/`tags`) — the title/topic never leaks.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteSummary {
    pub id: String,
    pub title: String,
    pub folder_id: String,
    /// First ~180 chars of the BODY (front-matter stripped); "" when locked.
    pub snippet: String,
    /// Parsed from the note's YAML front-matter `tags:` list; [] when locked.
    pub tags: Vec<String>,
    pub updated_at: i64,
    pub created_at: i64,
    /// Sealed AND not session-unlocked.
    pub locked: bool,
    /// Has an active outbound share (WP6 wires this; false until then).
    pub shared: bool,
}

/// A standalone authored note — the FULL DTO for the editor. Same masking contract as
/// [`NoteSummary`]: when locked the COMMAND layer returns `markdown: ""`, `title: "🔒 Locked"`,
/// empty `tags`/`properties`, so the sealed body never crosses the IPC boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteDoc {
    pub id: String,
    pub title: String,
    pub folder_id: String,
    /// FULL markdown INCLUDING YAML front-matter; "" when masked.
    pub markdown: String,
    /// Parsed from the front-matter `tags:` list; [] when masked.
    pub tags: Vec<String>,
    /// Other scalar front-matter keys (excl. `tags`); {} when masked.
    pub properties: std::collections::BTreeMap<String, String>,
    pub updated_at: i64,
    pub created_at: i64,
    /// The vault `.md` path, or null when never exported / sealed.
    pub exported_path: Option<String>,
    /// Masked (no markdown) when true.
    pub locked: bool,
    pub shared: bool,
}

/// A note folder — reuses the [`Folder`] shape with the `kind` discriminator surfaced (always
/// `"note"` for a note folder). Note folders are `folders` rows with `kind='note'`; the Notes
/// section shows only these, the Meetings section shows `kind != 'note'`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteFolder {
    pub id: String,
    pub name: String,
    pub path: String,
    pub parent_id: Option<String>,
    pub locked: bool,
    pub kind: String,
}

/// The selection Brain-assistant request (WP4): the selected text + its surrounding context +
/// which action. `before`/`after` are up to ~500 chars each around the selection.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteAssistRequest {
    pub note_id: String,
    /// `"refine"` | `"shorten"` | `"enhance"`.
    pub action: String,
    pub selection: String,
    #[serde(default)]
    pub before: Option<String>,
    #[serde(default)]
    pub after: Option<String>,
}

/// One enhance-context provenance citation — the source note/meeting the additive passage drew on.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteCitation {
    /// `"meeting"` | `"note"`.
    pub kind: String,
    pub id: String,
    pub title: String,
    pub snippet: String,
}

/// The selection Brain-assistant result (WP4). `suggestion` is the replacement for
/// refine/shorten, or the ADDITIVE passage for enhance. `citations` is populated for enhance only.
/// `modelLabel`/`mode`/`redacted` are DISPLAY metadata derived from the resolved provider target +
/// `CallMeta` — the popover shows them ("via Claude" / "via Qwen local").
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteAssistResult {
    pub action: String,
    pub suggestion: String,
    pub citations: Vec<NoteCitation>,
    pub model_label: String,
    /// `"local"` | `"cloud"`.
    pub mode: String,
    pub redacted: bool,
}

/// One proposed auto-organize move (WP5): a note and its proposed target note-folder. `toFolderId`
/// is the existing note-folder id when the name matches an existing `kind='note'` folder, else null
/// (⇒ a new folder to create on apply). Non-destructive: the FE reviews before `apply_organize_plan`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrganizeMove {
    pub note_id: String,
    pub title: String,
    pub from_folder_id: String,
    pub from_folder: String,
    pub to_folder: String,
    pub to_folder_id: Option<String>,
    pub reason: String,
}

/// The auto-organize plan (WP5) — the reviewable set of proposed moves.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrganizePlan {
    pub moves: Vec<OrganizeMove>,
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
