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
    /// The TRUE count of VISIBLE entities, BEFORE the `MAX_VISIBLE_ENTITIES` (500) render cap
    /// trims `nodes`. `total_visible_entities > nodes.len()` means the cap silently dropped rows —
    /// distinct from `has_hidden` (which only reflects LOCKED folders, not the render cap). The FE
    /// uses this to show an honest "showing N of TOTAL" caption instead of presenting the
    /// capped `nodes.len()` as the whole graph.
    pub total_visible_entities: i64,
}

// ── Brain v3 PR-4 — the FULL-BRAIN graph (typed, multi-kind) ─────────────────────────────────
//
// `get_graph` (above) is the ENTITY-ONLY graph and stays byte-compatible for its FE consumers.
// The full-brain graph is a SEPARATE, additive payload that unifies entities + meetings + notes +
// documents as TYPED nodes and every relation (co-occurrence + entity→meeting mentions + `links`
// rows) as TYPED edges. It is a PURE READ — no writes, no new storage. Every node is emitted only
// via its existing *_visible gate, and every edge requires BOTH endpoints to be in the visible-node
// set (an edge to a sealed node is dropped). A sealed-and-not-session-unlocked meeting/note/document
// contributes NOTHING — no node, and no edge that touches it.

/// The kind of a full-brain graph NODE. `entity` = a person/project from the self-assembling graph;
/// `meeting`/`note`/`document` = an owned-content item. Serialized lowercase for the FE lens toggles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FullGraphNodeKind {
    Entity,
    Meeting,
    Note,
    Document,
}

impl FullGraphNodeKind {
    /// Stable lowercase discriminant (used in the visible-node-set key + deterministic ordering).
    pub fn as_str(self) -> &'static str {
        match self {
            FullGraphNodeKind::Entity => "entity",
            FullGraphNodeKind::Meeting => "meeting",
            FullGraphNodeKind::Note => "note",
            FullGraphNodeKind::Document => "document",
        }
    }
}

/// One TYPED node in the full-brain graph. `label` is the display title (already resolved through
/// the visibility gate — a sealed item never produces a node, so a label is never a leak). `date` is
/// an ISO-8601 / epoch-derived timestamp string when the source row carries one (`None` for
/// entities). `degree` is the node's edge count WITHIN the returned (gated + capped) graph — a
/// layout hint, never the true corpus degree.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FullGraphNode {
    pub id: String,
    pub kind: FullGraphNodeKind,
    pub label: String,
    pub date: Option<String>,
    pub degree: i64,
}

/// The relation a full-brain edge encodes. `co_occurrence` = entity↔entity (shared visible meeting);
/// `mention` = entity→meeting (`entity_mentions`); `wikilink`/`companion`/`semantic` = a `links` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FullGraphEdgeKind {
    CoOccurrence,
    Mention,
    Wikilink,
    Companion,
    Semantic,
}

impl FullGraphEdgeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FullGraphEdgeKind::CoOccurrence => "co_occurrence",
            FullGraphEdgeKind::Mention => "mention",
            FullGraphEdgeKind::Wikilink => "wikilink",
            FullGraphEdgeKind::Companion => "companion",
            FullGraphEdgeKind::Semantic => "semantic",
        }
    }
}

/// One TYPED edge in the full-brain graph. `src`/`dst` are node ids that MUST both be present in
/// the returned `nodes` (BOTH-endpoint-gated — an edge to a sealed node is never emitted).
/// `src_kind`/`dst_kind` carry the ENDPOINT node kinds the backend gated on (PR-9 F4): a links edge
/// can connect `meeting↔note`, so the endpoint kinds are NOT derivable from `kind` alone, and the FE
/// must match endpoints by `(kind, id)` — not bare `id` — to be safe against a cross-kind id
/// collision. `status` is `active` for deterministic edges (co-occurrence/mention/wikilink/companion
/// + accepted semantic) and `suggested` for un-accepted semantic edges (only when the opts flag is on).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FullGraphEdge {
    pub src: String,
    pub dst: String,
    pub src_kind: FullGraphNodeKind,
    pub dst_kind: FullGraphNodeKind,
    pub kind: FullGraphEdgeKind,
    pub score: f64,
    pub status: String,
}

/// Options for `get_full_graph` / `build_full_graph`. Additive + all-default so the FE can call it
/// with no args. `include_suggested` (default `false`) admits un-accepted (`status='suggested'`)
/// semantic `links` rows — OFF by default so the graph shows only confirmed relations.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FullGraphOpts {
    #[serde(default)]
    pub include_suggested: bool,
}

/// The full-brain graph payload (`get_full_graph`): TYPED nodes + TYPED edges + the same honest
/// disclosure the entity graph makes. `has_hidden` is true when ≥1 folder is sealed-and-not-unlocked
/// (some nodes/edges may be hidden). `total_visible_nodes` is the TRUE count of visible nodes BEFORE
/// the per-kind render caps trimmed `nodes` — `total_visible_nodes > nodes.len()` means a cap
/// dropped rows (distinct from `has_hidden`, which only reflects LOCKED folders). `edges_truncated`
/// (PR-9 F2) is true when an EDGE-leg cap (the mention or links LIMIT) trimmed edges, so the FE can
/// disclose "some links are hidden" — distinct from `total_visible_nodes` (a node-leg cap) and
/// `has_hidden` (a locked folder).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FullGraphData {
    pub nodes: Vec<FullGraphNode>,
    pub edges: Vec<FullGraphEdge>,
    pub has_hidden: bool,
    pub total_visible_nodes: i64,
    pub edges_truncated: bool,
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

/// The payload returned by `list_people`: the (possibly render-capped) roster of `PersonCard`s
/// plus the TRUE count of VISIBLE people. `list_people` derives its candidate set from
/// `list_entities_visible`, which applies a `MAX_VISIBLE_ENTITIES` (500) cap ordered by mention
/// count — on a vault with >500 visible entities, `people` can be a strict subset of every
/// visible Person. `total_visible_people > people.len()` is the ONLY signal of that truncation;
/// without it the FE's "Show all N people" expander silently understates completeness (added
/// 2026-07-13, mirrors `GraphData::total_visible_entities`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeopleList {
    pub people: Vec<PersonCard>,
    pub total_visible_people: i64,
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
    /// `"meeting"` or `"note"` — `list_folders` returns EVERY folder (both namespaces share the
    /// `folders` table, so lock-reactive consumers see all of them), so the FE uses `kind` to render
    /// ONLY meeting folders in the Meetings tree — a note folder must never leak into it (2026-07-14).
    pub kind: String,
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
/// Backend-internal: this struct never crosses IPC to the FE (no `models.ts` counterpart).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocChunkHit {
    pub document_id: String,
    pub name: String,
    pub folder_id: String,
    pub snippet: String,
    /// The source document's `documents.kind` — `"document"` (uploaded file) or `"note"` (typed
    /// brain note). ADDITIVE (Feature D): lets a search-result renderer tag a document hit with its
    /// concrete kind (`[document:note:<id>]` vs `[document:document:<id>]`) so a model knows which
    /// `get_*` tool to call. Populated straight off the existing `documents.kind` column.
    pub kind: String,
    /// Brain v3 audit Fix 1 — the WINNING (post-dedup) chunk's `doc_chunks.id`, so parent expansion
    /// can align to the chunk that was actually retrieved instead of the document's dominant section.
    pub chunk_id: i64,
    /// The winning chunk's L1 section-parent row id (`doc_chunks.parent_id`); `None` for an L1/L2
    /// winner and for legacy flat rows.
    pub parent_id: Option<i64>,
    /// The heading trail the winning chunk sits under; `None` = flat/heading-less content, for
    /// which parent expansion NEVER fires (the flat L1 is the doc head, not a real section).
    pub section_path: Option<String>,
    /// The winning chunk's own 1-based page/slide; `None` for flow formats.
    pub page_no: Option<u32>,
    /// The winning chunk's `doc_chunks.level` (0 leaf / 1 section-parent / 2 doc-summary).
    pub level: i64,
    /// Distinct L0 leaves under the winning chunk's parent that appeared in the PRE-dedup candidate
    /// set of the SAME query (the winner itself included). `>= 2` is the auto-merging trigger:
    /// only a section corroborated by a second sibling hit is expanded to its L1 parent.
    pub sibling_hits: u32,
}

/// The full body of ONE standalone note OR imported/uploaded document, by id — the transport DTO
/// for the gated `get_document` tool (Feature D). Returned ONLY by [`Db::get_document_if_visible`],
/// which visibility-gates on the owning folder's lock (a sealed-and-not-session-unlocked document
/// resolves to `None`, never a masked partial). `markdown` is the plaintext `documents.text` body;
/// `title`/`updated_at` are the nullable authoring columns (a `kind='document'` upload leaves them
/// NULL — the caller falls back to `name`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentSummary {
    pub id: String,
    pub folder_id: String,
    pub kind: String,
    pub name: String,
    pub title: Option<String>,
    pub markdown: String,
    pub updated_at: Option<i64>,
}

/// Brain v3 audit Fix 3(b) — ONE entry in a document's structural OUTLINE (the heading/section tree
/// persisted in `doc_chunks`): a section-parent (L1) or the doc summary (L2). Deterministic, derived
/// PROVENANCE over already-derived plaintext — carries the section trail + page, NOT the section body
/// text, so an outline is a cheap MAP the agent reads to plan targeted `get_document(offset,maxChars)`
/// reads instead of blind char paging. Returned ONLY by [`Db::get_document_outline_if_visible`], which
/// visibility-gates on the owning folder's lock (a sealed-and-not-session-unlocked document → empty).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocOutlineEntry {
    /// `doc_chunks.level` — 1 = section-parent (a heading), 2 = doc summary/outline node.
    pub level: i64,
    /// The heading trail this node sits under (`"A › B"`); `None` for a flat/heading-less node.
    pub section_path: Option<String>,
    /// 1-based page/slide (PDF/PPTX); `None` for flow formats.
    pub page_no: Option<u32>,
}

/// Shared Brain v1 — one ORG-partition retrieval hit: the nearest/best chunk's snippet + the
/// source org item's id, `author_hint`, and title. The parallel of [`DocChunkHit`] for the org
/// leg. The `org_search` / `org_brain_search` TOOL renders each hit as a LOUD `[org · <author>]`
/// text line (`crate::tools::format_org_hits`); the structured `VaultSource` provenance chip on the
/// Ask surface is NOT wired to org hits (`origin` is always `None`). `content_sha256` rides along
/// so the self-share dedup (drop a hit whose plaintext hash matches a local `org_shares` row) is
/// applied without a second DB read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgChunkHit {
    pub item_id: String,
    pub author_hint: String,
    pub title: String,
    pub snippet: String,
    /// SHA-256 of the item's canonical plaintext envelope (self-share dedup key). May be empty for a
    /// legacy row that predates the hash.
    pub content_sha256: Vec<u8>,
}

/// Shared Brain v1 — the full decrypted org item for the read-only FE viewer (`org_get_item`).
/// `markdown` is the plaintext envelope body — deliberately-disclosed org content (no lock gate
/// applies to org items). Mirrors the FE `OrgItemDetail` (camelCase).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OrgItemDetail {
    pub item_id: String,
    pub author_hint: String,
    pub title: String,
    pub created_at: String,
    pub rev: u32,
    pub markdown: String,
    /// True when the CALLER authored this item (their `server_user_id` matches the item's stored
    /// `author_user_id`) — so the viewer can offer edit-in-place on ANY of the author's machines, not
    /// just the one that first shared it (the origin machine redirects to the local source instead;
    /// a second machine has no local `org_shares` row, so this server-authoritative author check is
    /// what unlocks editing there). Computed by the `org_get_item` command (needs the session);
    /// `Db::get_org_item` always sets it `false`. 2026-07-14.
    pub editable: bool,
}

/// Internal (not FE-facing) context the `org_update_own_item` egress command needs to re-publish an
/// edited org item the caller authored: which org to publish into, the current rev (→ rev+1 supersede),
/// the original `created_at` + `source_kind` to preserve on the wire, and the stored `author_user_id`
/// for the ownership gate. Resolved by [`Db::org_item_edit_ctx`] for a LIVE (non-tombstoned) item.
#[derive(Debug, Clone)]
pub struct OrgItemEditCtx {
    pub org_id: String,
    pub rev: u32,
    pub created_at: String,
    pub source_kind: Option<String>,
    pub author_user_id: Option<String>,
}

/// Shared Brain v1 — a LIST-row header for one live org item (the browsable org-items list, so a
/// member can SEE what colleagues shared into the org instead of only search-hitting it). Headers
/// ONLY — the `markdown` body is deliberately NOT here (that's `org_get_item`); org items are
/// disclosed content so no per-note lock gate applies, but keeping the list content-min avoids
/// shipping every body on a list read. Mirrors the FE `OrgItemHeader` (camelCase).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OrgItemHeader {
    pub item_id: String,
    pub title: String,
    pub author_hint: String,
    pub created_at: String,
    pub seq: u64,
    /// The item's source kind — `"document"` (a shared authored note) or `"meeting"` (a shared
    /// meeting note) — so the FE can filter a per-org list into "shared meetings" vs "shared notes"
    /// (Library/Meetings vs Notes view). The `OrgEnvelope` wire format now carries this natively as of
    /// `ORG_ENVELOPE_VERSION = 2` (`share::org_envelope::OrgSourceKind`), stored straight off the
    /// opened envelope into `org_items.source_kind` at ingest — so a COLLEAGUE'S item now classifies
    /// too, not just items THIS device published. For items THIS device published, an UNGATED local
    /// `org_shares`-anchored resolver (`meeting_id` XOR `document_id`) can still override/fall back
    /// (metadata only, never gated on unlock state, unlike `owned_source` below which also carries the
    /// live title). `None` means genuinely unclassified: an item ingested off an old v1 envelope
    /// (published before the peer/this device upgraded, or before this column existed) carries no
    /// source-type signal on the wire — the FE MUST treat `None` as "unclassified", never
    /// assume/default it to one bucket.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// The caller's OWN editable local source for this item, when THIS device published it (resolved
    /// via `org_share_by_item` → the anchored note/meeting) AND that source is currently readable
    /// (unlock-gated — a locked source resolves to `None`, never leaking its title). `None` for an
    /// item shared by someone else (a read-only replica) or a locked own source. When present, the FE
    /// links the row straight to the editable original (`/notes/:id` | `/meeting/:id`) and the header's
    /// `title` is overridden to the source's CURRENT title — so the author's own card never shows a
    /// stale publish-time snapshot and never routes through the read-only viewer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owned_source: Option<OrgOwnedSource>,
}

/// The caller's local editable source behind an org item they authored (see `OrgItemHeader.owned_source`).
/// `kind` is `"document"` (a note → `/notes/:id`) or `"meeting"` (a recording → `/meeting/:id`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OrgOwnedSource {
    pub kind: String,
    pub id: String,
}

/// Shared Brain v1 — the content-free result of a manual `org_sync_now()` (counts + errors only).
/// `fts_only` is true when the local member has no real embedder (StubEmbedder ⇒ the org partition
/// is indexed FTS-only until a model appears + a re-embed runs). Mirrors the FE `OrgSyncReport`
/// (camelCase: `pulled, ingested, tombstoned, lastSeq, ftsOnly, errors, authorsBackfilled`).
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OrgSyncReport {
    pub pulled: u32,
    pub ingested: u32,
    pub tombstoned: u32,
    pub last_seq: u64,
    pub fts_only: bool,
    /// Content-free error strings (per-item OPEN/ingest failures that were SKIPPED, not fatal).
    pub errors: Vec<String>,
    /// Count of local `org_items` rows whose `author_user_id` was NULL (stale-ingest, from before the
    /// column/stamping existed, or a cursor already past the item) and got backfilled THIS sync from a
    /// full-feed re-pull (`org_sync_one` → the null-author backfill pass). Content-free — an id/author
    /// count only. (2026-07-15.)
    pub authors_backfilled: u32,
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
    /// Sealed (encrypted) on disk — the DB `locked` column.
    pub locked: bool,
    /// Sealed on disk BUT session-unlocked (decrypted for this session only). Mirrors
    /// [`FolderNode::unlocked`] — the DB knows nothing about the session, so `row_to_note_folder`
    /// always sets this `false`; the `list_note_folders` command overwrites it by joining the live
    /// `AppState::unlocked_folders` set (a sealed folder that is NOT session-unlocked stays `false`).
    pub unlocked: bool,
    /// The reserved always-open note-root that backs the "Notes" section (2026-07-14). Exactly one
    /// note-folder has this set; the FE HIDES it from the folder tree (it IS the section root, where
    /// unfiled notes live) and it can never be locked. Everything else is `false`.
    pub is_root: bool,
    pub kind: String,
}

// ── Feature C — TYPED note front-matter properties + folder Table/Board substrate ────────────────
//
// A note-folder can declare a SCHEMA: an ordered list of typed property columns (Text/Select/Date/
// Checkbox/Number) that overlay the note's YAML front-matter. The schema is content-free metadata
// (like `saved_views`) stored in `note_folder_schemas`. Typing is a READ-TIME COERCION layer over
// the SAME `Record<String,String>` YAML scalars `parse_front_matter` already yields — the owned-.md
// byte round-trip and the `text_blob` seal path are UNAWARE of it (load-bearing: the front-matter
// parsers are never touched). A `Select` value that is not in the declared `options` is PRESERVED as
// `Text`, never dropped.

/// The type of a note-folder schema property column. `snake_case` on the wire so the FE reads
/// `"text"`/`"select"`/`"date"`/`"checkbox"`/`"number"`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PropertyKind {
    Text,
    Select,
    Date,
    Checkbox,
    Number,
}

/// One declared property column in a note-folder's schema. `options` is meaningful ONLY for
/// `Select` (the allowed values); empty for the other kinds.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PropertySchemaField {
    pub key: String,
    pub kind: PropertyKind,
    #[serde(default)]
    pub options: Vec<String>,
}

/// A COERCED front-matter value — the typed reading of a raw YAML scalar against a schema column.
/// ADJACENTLY-tagged: serialized as `{ "kind": "...", "value": ... }` so the FE gets both the type
/// and the concrete value. A raw scalar that fails to coerce to the declared kind is preserved as
/// `Text` (the value is never lost).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum PropertyValue {
    Text(String),
    Select(String),
    Date(String),
    Checkbox(bool),
    Number(f64),
}

/// A note row projected through a folder's typed schema (the Table/Board substrate). `values` maps a
/// schema `key` → its coerced [`PropertyValue`] (only keys present in BOTH the schema and the note's
/// front-matter appear; a key not declared in the schema is not projected). `tags` is the raw
/// front-matter tag list. Built ONLY from gated readers — a sealed-not-unlocked folder yields NO
/// rows (never a masked row), so a typed row can never carry sealed content.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TypedNoteRow {
    pub id: String,
    pub title: String,
    pub folder_id: String,
    pub values: std::collections::BTreeMap<String, PropertyValue>,
    pub tags: Vec<String>,
    pub updated_at: i64,
}

/// The selection Brain-assistant request (WP4): the selected text + its surrounding context +
/// which action. `before`/`after` are up to ~500 chars each around the selection.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteAssistRequest {
    pub note_id: String,
    /// One of the note-assistant action ids (see the seam contract): the EDIT actions
    /// (`refine`/`grammar`/`shorten`/`expand`/`simplify`/`tone`/`translate`), STRUCTURE
    /// (`bullets`/`table`/`keypoints`), FROM YOUR BRAIN (`enhance`/`find_related`/`link_entities`/
    /// `fact_check`/`ask`), EXTRACT (`action_items`/`decisions`), CREATE (`draft_followup`/
    /// `spinoff_note`), or `custom`.
    pub action: String,
    pub selection: String,
    #[serde(default)]
    pub before: Option<String>,
    #[serde(default)]
    pub after: Option<String>,
    /// Variant selector for the variant-heavy actions: `tone` name (Professional/Casual/…) or
    /// `translate` target language. `None` for actions that take no variant.
    #[serde(default)]
    pub variant: Option<String>,
    /// Free-text instruction: the `custom` action's instruction, or the `ask` action's question
    /// about the selection. `None` for actions that need no instruction.
    #[serde(default)]
    pub instruction: Option<String>,
}

/// One enhance-context provenance citation — the source note/meeting/org-item the additive passage
/// drew on.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteCitation {
    /// `"meeting"` | `"note"` | `"org"`.
    pub kind: String,
    /// For `kind == "org"` this is the org item id (`OrgChunkHit::item_id`), routed by the FE to
    /// `/org-item/:id` — never a local meeting/note id.
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
    /// How the FE renders + applies the result: `"replace"` (struck original vs suggestion →
    /// Accept replaces the selection) | `"insert"` (append the suggestion after the selection) |
    /// `"info"` (read-only answer + citations; Copy / Insert-as-note; NO destructive replace) |
    /// `"artifact"` (a drafted email/note preview: `title` + `suggestion`). The FE renders
    /// generically off THIS field — it does NOT re-derive the shape from the action.
    pub shape: String,
    /// The artifact title — an email subject (`draft_followup`) or note title (`spinoff_note`).
    /// `None` for every non-artifact shape.
    #[serde(default)]
    pub title: Option<String>,
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

/// A user-saved VIEW over a list surface (Feature B — "Saved views over the meetings list").
///
/// LOCK MODEL: this is CONTENT-FREE user metadata (like `saved_recipes`) — a single-user,
/// non-shared row that stores only a VIEW DEFINITION, never meeting content. `config` is an OPAQUE
/// JSON TEXT blob owned by the FE (filters / sort / groupBy / columns); the backend never parses it
/// and it MUST never carry note/transcript/title text. Because it holds no meeting content, its
/// read/write path is NOT visibility-gated (mirrors `list_saved_recipes` / `insert_recipe`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedView {
    pub id: String,
    /// Which list surface this view targets — `"meetings"` or `"notes"` (added 2026-07-14 when
    /// Saved Views were ported to the Notes surface). The `scope` column partitions the roster so
    /// each surface lists only its own views.
    pub scope: String,
    pub name: String,
    /// Presentation mode chosen for the view (e.g. `"list"` | `"board"` | `"table"`) — FE-owned.
    pub layout: String,
    /// OPAQUE FE-owned JSON view definition (filters/sort/groupBy/columns). NEVER meeting content.
    pub config: String,
    pub sort_order: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// Per-meeting open/done action-item counts, rolled up across the VISIBLE library for the saved-views
/// meetings surface. Produced by the deterministic `Db::list_meeting_action_summaries` aggregation:
/// only VISIBLE meetings contribute — a sealed-and-not-session-unlocked meeting yields NO row
/// (aggregate posture, NOT a masked row), gated exactly like `Db::list_open_commitments`
/// (`list_meetings_visible` + `get_note_if_visible`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingActionSummary {
    pub meeting_id: String,
    pub open_count: i64,
    pub done_count: i64,
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

/// Shared Brain v1 — where a retrieval hit came from. `kind:"local"` (owned meeting/note the user
/// recorded/authored) or `kind:"org"` (an org-brain item synced from a colleague's share). For an
/// org hit, `author` is the item's `author_hint` label and `org_item_id` is the id the read-only
/// org-item viewer loads via `org_get_item`. Mirrors the FE `SourceOrigin` (camelCase). CONTENT-FREE
/// beyond the author label the local user is already allowed to see.
///
/// NOTE (2026-07-11 SB-2): the STRUCTURED chip is not yet wired — every [`VaultSource`] currently
/// sets `origin: None`; org retrieval provenance is surfaced only as the LOUD `[org · <author>]`
/// TEXT line the `org_search` / `org_brain_search` tool emits (`crate::tools::format_org_hits`).
/// This DTO exists for the (unimplemented) chip surface; do not document a fusion that does not run.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceOrigin {
    /// `"local"` | `"org"`.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org_item_id: Option<String>,
}

/// A meeting referenced as a source in an Ask-My-Vault answer.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultSource {
    pub meeting_id: String,
    pub title: String,
    pub started_at: String,
    /// Shared Brain v1 — always `None` today (SB-2): the structured org-origin chip is NOT wired, so
    /// every source is a plain LOCAL owned-content reference. Kept as an ADDITIVE, FE-optional field
    /// (`skip_serializing_if`) for the eventual chip surface; org provenance rides the tool's text
    /// `[org · <author>]` line instead. See [`SourceOrigin`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<SourceOrigin>,
}

/// Which kind of owned-content row a backlink SOURCE (or target) is — a meeting (its AI note in
/// `notes.markdown`) or a standalone note (`documents.text` where `kind='note'`). Serialized as a
/// stable lowercase string so the FE can branch a chip icon/route without a second field.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Meeting,
    Note,
}

/// A row whose body contains a `[[Title]]` wikilink pointing AT the queried target — a "what links
/// here" backlink chip. `timestamp` is unified so the FE needs no kind-branch: the meeting's
/// `started_at` for a meeting, the note's `updated_at` (rendered as a string) for a note. Only ever
/// built from VISIBLE (session-unlocked) rows — a sealed source can never appear here, and a sealed
/// target yields an empty list (never reveals it HAS backlinks).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BacklinkSource {
    pub id: String,
    pub kind: SourceKind,
    pub title: String,
    pub timestamp: String,
}

/// Brain v3 PR-3 — one persisted `links` row surfaced to the FE, with the OTHER endpoint's display
/// title resolved through the SAME visibility gate as the queried endpoint (both-endpoint gating).
/// `direction` says whether the queried item is this edge's `src` ("out") or `dst` ("in") so the FE
/// can render an arrow; `other_kind`/`other_id` are the neighbour to navigate to. `edge_type` ∈
/// `wikilink|companion|semantic|manual`, `created_by` ∈ `user|auto|accepted`, `status` ∈
/// `active|suggested|dismissed` (dismissed rows are never returned). `score` is the semantic cosine
/// (1.0 for the deterministic wikilink/companion/manual edges). Only ever built when BOTH endpoints
/// are VISIBLE — a sealed neighbour can never appear, and a sealed queried item yields an empty list.
///
/// `manual` (note↔meeting-links PR-1) is the DISPLAY-DEDUPE flag: when a user-initiated `manual` edge
/// AND a derived `wikilink` (or another deterministic edge) exist for the SAME `(other_kind,
/// other_id)` pair, `links_for_visible` collapses them to ONE row (preferring the deterministic
/// `edge_type` for its stable id) but sets `manual = true` so the FE knows the chip is user-created +
/// REMOVABLE (renders the `×` → `unlink_items`). A pair with no `manual` edge has `manual = false`
/// (an auto wikilink/semantic chip — not user-removable). Always present, defaults `false` for any
/// non-manual edge, so an old FE reads it as absent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LinkEdge {
    pub id: i64,
    /// "out" (queried item is `src`) | "in" (queried item is `dst`).
    pub direction: String,
    /// The neighbour endpoint kind: `meeting|note|document`.
    pub other_kind: String,
    pub other_id: String,
    /// The neighbour's current display title, resolved through the visibility gate.
    pub other_title: String,
    pub edge_type: String,
    pub created_by: String,
    pub status: String,
    pub score: f64,
    pub created_at: i64,
    /// note↔meeting-links PR-1 — `true` iff a user-initiated `manual` edge exists for this
    /// `(other_kind, other_id)` pair (whether this collapsed row's `edge_type` is `manual` itself or a
    /// deterministic edge the manual link was deduped into). The FE renders the removable `×` only on
    /// a `manual` chip. Defaults `false` (serde default) for every non-manual pair.
    #[serde(default)]
    pub manual: bool,
}

/// A resolved `[[Title]]` wikilink navigation target — the VISIBLE note, meeting, document, or
/// (2026-07-15) org (Shared Brain) item whose exact title the link names, so the FE can route
/// `/notes/:id`, `/meeting/:id`, the document viewer, or `/org-item/:id`. Resolution is
/// visibility-gated: a sealed-and-not-session-unlocked note/meeting/document with that title
/// resolves to `None`, so clicking a wikilink can never reveal or navigate to locked content.
/// `kind` is a raw string (`"meeting"` | `"note"` | `"document"` | `"org"`), NOT [`SourceKind`] —
/// mirrors the convention [`NoteCitation`] already established for
/// the identical tri-state (`SourceKind` stays a strict local meeting/note enum, used only for
/// backlinks, which have no org leg). For `kind == "org"`, `id` is the org item id
/// (`org_items.item_id`), never a local id — the FE routes it through
/// `TabsService.openOrgItem`, exactly like `NoteCitation`'s org rows.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WikiTarget {
    /// `"meeting"` | `"note"` | `"document"` | `"org"`.
    pub kind: String,
    pub id: String,
}

/// note↔meeting-links PR-2 — one EXPLICIT source the user pinned in the Ask source picker: a
/// `meeting|note|document` reference that SCOPES a Brain answer to exactly the listed items (plus
/// their capped link-expansion) instead of a whole-vault search. Serialized camelCase `{kind, id}`
/// so the FE can build it straight from a `LinkEdge`/`LinkKind`.
///
/// NO `deny_unknown_fields`: the FE picker sends an extra `title` field (for chip display) that the
/// backend must harmlessly IGNORE. Every read of a pinned source stays visibility-gated in
/// `build_vault_context_pinned_visible` — a sealed-and-not-session-unlocked source contributes
/// nothing (E9), so pinning a locked item can never leak it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SourceRef {
    pub kind: crate::links::LinkKind,
    pub id: String,
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

/// Result of the CLOUD-synthesized `entity_dossier` command (B2, Shared Brain). `has_org_context`
/// is an HONEST signal: `true` iff the synthesis prompt included any `[org · author]`-attributed
/// colleague content ALONGSIDE the user's own verified facts — so the FE/response never silently
/// blends org-sourced claims into the dossier without a distinguishing signal. READ-ONLY: org
/// content that contributes here is NEVER written into `entities`/`entity_mentions`/`facts`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntityDossierResult {
    pub markdown: String,
    #[serde(default)]
    pub has_org_context: bool,
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

/// M6 Shared Brain — a locally-joined org (row of `org_state`). Membership metadata only; no content.
///
/// `context_enabled` (per-instance org toggle): whether this JOINED org contributes content on THIS
/// Murmur install — browsing (`list_org_items`) AND brain/assistant context
/// (`search_org_chunks_knn`/`_fts`). Default `true` (every existing/new membership stays active).
/// Distinct from `consented` (org EGRESS consent — sharing OUT); this gates READING IN. A disabled
/// org's rows are NEVER deleted/purged — only excluded from every read path — so re-enabling is
/// instant with no re-sync.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgState {
    pub org_id: String,
    pub name: String,
    pub role: String,
    pub joined_at: String,
    pub consented: bool,
    pub last_seq: i64,
    pub generation: u32,
    pub context_enabled: bool,
}

/// M6 Shared Brain — one row of the outbound org-share state machine (`org_shares`). Anchors on a
/// local `meeting_id` XOR `document_id`; carries the item kind, the content-hash dedup key, the
/// server `item_id` once published, and the current `state`. NO note title/body/OCK.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgShareRow {
    pub id: String,
    pub org_id: String,
    pub meeting_id: Option<String>,
    pub document_id: Option<String>,
    pub kind: String,
    /// A local display title for the owner's own share list (renders only to the local owner who can
    /// already read it). NOT sent to the server.
    pub title: Option<String>,
    pub rev: u32,
    pub generation: u32,
    pub content_sha256: Option<Vec<u8>>,
    pub item_id: Option<String>,
    pub state: String,
    pub last_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
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
