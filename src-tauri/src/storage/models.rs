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
    /// A USER-created link (`links.edge_type = 'manual'`, written by `upsert_manual_link` for the
    /// note↔meeting↔document "Related" chip). Deterministic + always `active` — the most intentional
    /// link there is, so it MUST render in the full-brain graph like wikilink/companion. Omitting it
    /// from the edge taxonomy is what made a manually-linked document show "0 connections" (2026-07-20).
    Manual,
}

impl FullGraphEdgeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FullGraphEdgeKind::CoOccurrence => "co_occurrence",
            FullGraphEdgeKind::Mention => "mention",
            FullGraphEdgeKind::Wikilink => "wikilink",
            FullGraphEdgeKind::Companion => "companion",
            FullGraphEdgeKind::Semantic => "semantic",
            FullGraphEdgeKind::Manual => "manual",
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
    /// Canonical user-container placement. Legacy rows are conservatively backfilled from notes.
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

/// One visibility-gated FTS transcript match before presentation-channel projection.
///
/// The tool layer joins this stable stored segment id against the canonical rendered projection.
/// That is what lets the final hit carry a character offset in the exact same coordinate system as
/// `get_meeting`, while a `merged` projection may safely omit an echoed raw segment.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptSegmentHit {
    pub meeting_id: String,
    pub meeting_title: String,
    pub seg_idx: i64,
}

/// One persisted raw capture-lane segment plus explicit presentation-only echo provenance.
///
/// `echo_suppressed` is set only by the ingest path after measured acoustic leak evidence. Legacy
/// rows and deserialized payloads that predate the field default to visible, so read-time rendering
/// never guesses from text/timestamps and never hides old data.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredTranscriptSegment {
    #[serde(flatten)]
    pub segment: crate::transcribe::types::Segment,
    #[serde(default)]
    pub echo_suppressed: bool,
}

/// A visibility-gated enrolled speaker label without the biometric embedding.
///
/// Transcript rendering needs only the cluster-to-name mapping. Keeping this DTO separate from the
/// full voiceprint row prevents a read-only text response from loading CAM++ biometric vectors.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VisibleSpeakerLabel {
    pub cluster_index: i64,
    pub label: String,
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
    pub created_at: i64,
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
    pub doc_id: Option<String>,
    pub link_id: Option<String>,
    pub author_hint: String,
    pub title: String,
    pub created_at: String,
    pub rev: u32,
    pub markdown: String,
    /// Compatibility mirror of `can_edit` for older frontends. Permission-aware clients use
    /// `can_edit`/`can_manage`, computed from stable ownership, org role, and document access.
    pub editable: bool,
    pub access: String,
    pub can_edit: bool,
    pub can_manage: bool,
}

/// Result of copying a received Shared Brain snapshot into the user's local hierarchy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OrgItemImportResult {
    /// Corresponding local kind: `note` for an org document, `meeting` for an org meeting shell.
    pub kind: String,
    /// Local authored-note id or local meeting id.
    pub id: String,
}

/// Internal edit/management context for a live org item. It preserves immutable envelope provenance
/// and separates the revision actor (`author_user_id`) from the stable document manager.
#[derive(Debug, Clone)]
pub struct OrgItemEditCtx {
    pub org_id: String,
    pub doc_id: Option<String>,
    pub rev: u32,
    pub created_at: String,
    pub author_hint: String,
    pub source_kind: Option<String>,
    pub author_user_id: Option<String>,
    pub document_owner_user_id: Option<String>,
    pub access: String,
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
    pub doc_id: Option<String>,
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
    /// `high` | `medium` | `low`; informational only because every move is review-first.
    pub confidence: String,
    pub reason: String,
}

/// One existing, open direct-child destination offered by the note organizer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OrganizeTarget {
    pub id: String,
    pub label: String,
}

/// The auto-organize plan (WP5) — the reviewable set of proposed moves plus honest coverage.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrganizePlan {
    /// `None` keeps the legacy Notes-home all-visible scope; `Some` is one selected container.
    pub scope_folder_id: Option<String>,
    pub moves: Vec<OrganizeMove>,
    pub targets: Vec<OrganizeTarget>,
    pub total_scanned: u32,
    pub already_organized: u32,
    pub deferred: u32,
}

/// One note that could not be applied after the plan was reviewed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OrganizeFailure {
    pub note_id: String,
    pub reason: String,
    #[serde(default)]
    pub retryable: bool,
}

/// Honest best-effort apply receipt. `appliedIds` and `failures.noteId` are disjoint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OrganizeApplyResult {
    pub applied_ids: Vec<String>,
    pub failures: Vec<OrganizeFailure>,
}

/// One turn in a meeting chat conversation. `role` is "user" | "assistant".
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatTurn {
    pub role: String,
    pub content: String,
}

/// Durable Ask Brain scope. `refId` is absent for vault and required for note/meeting.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum AskConversationScope {
    Vault,
    Note { ref_id: String },
    Meeting { ref_id: String },
}

impl AskConversationScope {
    pub(crate) fn storage_parts(&self) -> (&'static str, Option<&str>) {
        match self {
            Self::Vault => ("vault", None),
            Self::Note { ref_id } => ("note", Some(ref_id.as_str())),
            Self::Meeting { ref_id } => ("meeting", Some(ref_id.as_str())),
        }
    }

    pub(crate) fn validate(&self) -> crate::error::Result<()> {
        match self {
            Self::Vault => Ok(()),
            Self::Note { ref_id } | Self::Meeting { ref_id } if ref_id.trim().is_empty() => Err(
                crate::error::AppError::InvalidArg("conversation scope reference is empty".into()),
            ),
            Self::Note { .. } | Self::Meeting { .. } => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AskConversationSummary {
    pub id: String,
    pub scope: AskConversationScope,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    pub message_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AskConversationMessage {
    pub id: String,
    pub ordinal: u32,
    pub role: String,
    pub content: String,
    #[serde(default)]
    pub sources: Vec<VaultSource>,
    #[serde(default)]
    pub citations: Vec<String>,
    pub created_at: String,
}

/// A source reference returned by durable Ask history. Identity is persisted as `kind + id` only;
/// `title` is resolved at load time through the live visibility gate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AskConversationSourceRef {
    pub kind: crate::links::LinkKind,
    pub id: String,
    pub title: String,
}

/// Live dashboard chrome for a durable composite scope. Only `dashboard_id` is persisted on the
/// conversation; title/emoji are resolved from the current board row on every load.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DashboardScopeRef {
    pub id: String,
    pub title: String,
    pub emoji: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AskConversation {
    pub id: String,
    pub scope: AskConversationScope,
    pub title: String,
    pub selected_sources: Vec<AskConversationSourceRef>,
    pub dashboard: Option<DashboardScopeRef>,
    pub messages: Vec<AskConversationMessage>,
    pub created_at: String,
    pub updated_at: String,
}

/// Successful durable send. Every canonical identity is minted by the backend: `conversationId`
/// names the SQLite thread, while the message IDs identify the atomic pair just committed.
/// `askTraceId` remains a separate ephemeral value used only to route live tool-trace events.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AskConversationSendResult {
    pub conversation_id: String,
    pub user_message_id: String,
    pub assistant_message_id: String,
    pub answer: String,
    #[serde(default)]
    pub sources: Vec<VaultSource>,
    #[serde(default)]
    pub citations: Vec<String>,
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

/// One ordered `## {heading}` block of a user-authored note template. `instruction` is the plain,
/// DECLARATIVE guidance the model gets for that section — it is DATA, never code (see
/// `summarize::template::validate_note_template`, which rejects scripting tokens at save).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteTemplateSection {
    pub heading: String,
    pub instruction: String,
}

/// A user-authored NOTE TEMPLATE (Granola-style named sections). CONTENT-FREE, single-user metadata
/// (exactly like `saved_recipes` / `saved_views`): it stores only a note SHAPE — a tone line, an
/// ordered list of `{heading, instruction}` sections, and any extra front-matter keys — never meeting
/// content, so its read/write path is NOT visibility-gated. Selected by id via the note-style
/// selector; rendered into the summarizer system prompt by `summarize::template::build_template`
/// (the same `SummarizeRequest.template` seam the built-in styles use). `sections` and
/// `extra_frontmatter_keys` persist as JSON TEXT columns.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteTemplate {
    pub id: String,
    pub name: String,
    /// A short style/tone directive appended to the prompt preamble (may be empty).
    pub tone: String,
    pub sections: Vec<NoteTemplateSection>,
    /// Additional YAML front-matter keys to request beyond the fixed 5 (may be empty).
    pub extra_frontmatter_keys: Vec<String>,
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

// ── Murmur Reminders ────────────────────────────────────────────────────────────────────────────

/// A reminder's recurrence cadence. Stored as a stable lowercase string and surfaced unchanged
/// over IPC.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ReminderRepeatUnit {
    Days,
    Weeks,
    Months,
    Years,
}

impl ReminderRepeatUnit {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Days => "days",
            Self::Weeks => "weeks",
            Self::Months => "months",
            Self::Years => "years",
        }
    }

    pub(crate) fn parse(value: &str) -> crate::error::Result<Self> {
        match value {
            "days" => Ok(Self::Days),
            "weeks" => Ok(Self::Weeks),
            "months" => Ok(Self::Months),
            "years" => Ok(Self::Years),
            other => Err(crate::error::AppError::Storage(format!(
                "unknown reminder repeat unit: {other}"
            ))),
        }
    }
}

/// Reminder provenance. `Smart` means the user explicitly promoted a reviewed local suggestion;
/// it never means that an audit created a reminder automatically.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ReminderOrigin {
    Manual,
    Smart,
}

impl ReminderOrigin {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Smart => "smart",
        }
    }

    pub(crate) fn parse(value: &str) -> crate::error::Result<Self> {
        match value {
            "manual" => Ok(Self::Manual),
            "smart" => Ok(Self::Smart),
            other => Err(crate::error::AppError::Storage(format!(
                "unknown reminder origin: {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ReminderState {
    Active,
    Completed,
}

impl ReminderState {
    pub(crate) fn parse(value: &str) -> crate::error::Result<Self> {
        match value {
            "active" => Ok(Self::Active),
            "completed" => Ok(Self::Completed),
            other => Err(crate::error::AppError::Storage(format!(
                "unknown reminder state: {other}"
            ))),
        }
    }
}

/// An opaque anchor to a Murmur-authored note or meeting. Titles are deliberately NOT stored here:
/// the command layer resolves them only through the source's current visibility gate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReminderSourceAnchor {
    pub kind: String,
    pub id: String,
}

/// User-editable input shared by create/update and Smart-suggestion promotion.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReminderDraft {
    pub title: String,
    pub details: Option<String>,
    /// UTC epoch milliseconds. The composer derives this from a calendar-valid local date/time.
    pub due_at: i64,
    pub repeat_every: Option<u32>,
    pub repeat_unit: Option<ReminderRepeatUnit>,
    #[serde(default)]
    pub sources: Vec<ReminderSourceAnchor>,
}

/// Canonical SQLCipher-backed reminder row plus its opaque source anchors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredReminder {
    pub id: String,
    pub title: String,
    pub details: Option<String>,
    pub due_at: i64,
    pub repeat_every: Option<u32>,
    pub repeat_unit: Option<ReminderRepeatUnit>,
    pub state: ReminderState,
    pub origin: ReminderOrigin,
    pub created_at: i64,
    pub updated_at: i64,
    pub completed_at: Option<i64>,
    pub sources: Vec<ReminderSourceAnchor>,
}

/// Live-gated source metadata. A sealed/deleted source contributes no entry; the reminder itself is
/// independent user data and remains usable.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReminderSourceView {
    pub kind: String,
    pub id: String,
    pub title: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReminderView {
    pub id: String,
    pub title: String,
    pub details: Option<String>,
    pub due_at: i64,
    pub repeat_every: Option<u32>,
    pub repeat_unit: Option<ReminderRepeatUnit>,
    pub state: ReminderState,
    pub origin: ReminderOrigin,
    pub created_at: i64,
    pub updated_at: i64,
    pub completed_at: Option<i64>,
    pub sources: Vec<ReminderSourceView>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReminderInboxItem {
    pub occurrence_id: String,
    pub due_at: i64,
    pub reminder: ReminderView,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RemindersSnapshot {
    pub inbox: Vec<ReminderInboxItem>,
    pub upcoming: Vec<ReminderView>,
    pub completed: Vec<ReminderView>,
    pub due_inbox_count: u64,
}

/// Content-free shell badge projection. Fetching it never resolves reminder/source titles.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReminderSummary {
    pub due_inbox_count: u64,
}

/// Storage projection for one unread due occurrence. The command layer joins the referenced
/// reminder to its live-gated source metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredReminderOccurrence {
    pub id: String,
    pub reminder_id: String,
    pub due_at: i64,
}

/// One disposable Smart-audit suggestion row. The title is source-derived plaintext, so storage
/// purges this domain on every source edit/seal/relock; it is never treated as user-owned reminder
/// data until an explicit atomic promotion succeeds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredReminderSuggestion {
    pub id: String,
    pub source_kind: String,
    pub source_id: String,
    pub content_hash: String,
    pub engine_id: String,
    pub candidate_key: String,
    pub title: String,
    pub suggested_due_at: Option<i64>,
    pub created_at: i64,
}

/// Content-free lookup anchor for accept/dismiss authorization. Command code may read this before
/// taking the lifecycle mutex because it contains no suggestion title, due time, or source title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReminderSuggestionGateAnchor {
    pub id: String,
    pub source_kind: String,
    pub source_id: String,
}

/// IPC-safe Smart suggestion. Source metadata is resolved through the current visibility gate by
/// the command layer; storage never persists a source title in the derived audit tables.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReminderSuggestionView {
    pub id: String,
    pub title: String,
    pub suggested_due_at: Option<i64>,
    pub source: ReminderSourceView,
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

/// One exact, directed user-created row in the `links` table. A displayed [`LinkEdge`] may collapse
/// several rows for the same neighbour (including both `A -> B` and `B -> A`), so removability must
/// carry the stored tuples instead of reconstructing one from the display representative.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ManualLinkEdge {
    pub src_kind: String,
    pub src_id: String,
    pub dst_kind: String,
    pub dst_id: String,
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
/// `manual` (note↔meeting-links PR-1) is the backward-compatible DISPLAY-DEDUPE flag: when a
/// user-initiated `manual` edge
/// AND a derived `wikilink` (or another deterministic edge) exist for the SAME `(other_kind,
/// other_id)` pair, `links_for_visible` collapses them to ONE row (preferring the deterministic
/// `edge_type` for its stable id) but sets `manual = true` so the FE knows the chip is user-created +
/// REMOVABLE (renders the `×` → `unlink_items`). A pair with no `manual` edge has `manual = false`
/// (an auto wikilink/semantic chip — not user-removable). `manual_edges` is the authoritative exact
/// directed set behind that flag (one or both directions); it lets unlink remove every collapsed row
/// atomically even when the representative points the other way. Both fields default empty/false.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LinkEdge {
    pub id: i64,
    /// "out" (queried item is `src`) | "in" (queried item is `dst`).
    pub direction: String,
    /// The neighbour endpoint kind: `meeting|note|document|org`.
    pub other_kind: String,
    pub other_id: String,
    /// Current navigation id when the stable endpoint id differs from the routed id. Present for an
    /// `org` edge (`other_id = org_id:doc_id`, `navigation_id = current item_id`) and omitted locally.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub navigation_id: Option<String>,
    /// The neighbour's current display title, resolved through the visibility gate.
    pub other_title: String,
    /// For an `other_kind == "container"` neighbour ONLY: `"project"` (a Space) or `"folder"`.
    ///
    /// NON-CONTENT metadata — the same `folders.level` the sidebar already renders — carried so the
    /// chip can pick its glyph and its noun ("Space" vs "folder") without a second IPC round-trip.
    /// Space-vs-folder is deliberately a FIELD rather than a second `LinkKind`: one endpoint kind
    /// keeps the write path, the purge path and every exhaustive match single-branched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub other_container_level: Option<String>,
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
    /// Every exact directed `manual` row folded into this displayed chip. Empty for derived-only
    /// chips. The unlink command removes this whole set in one transaction; `manual` remains as the
    /// backward-compatible display flag.
    #[serde(default)]
    pub manual_edges: Vec<ManualLinkEdge>,
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
    /// Revision-stable endpoint id used by the private link graph. For org targets this is the
    /// strict `org_id:doc_id` composite; `id` remains the current `item_id` for navigation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stable_id: Option<String>,
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

/// One org member's PUBLIC identity key, as this device learned it (`org_member_keys`).
///
/// Public material only — the same bytes `POST /v1/keys/lookup` publishes to any authenticated
/// caller, plus the fingerprint the safety-word check already shows. Cached because a key rotation
/// has to wrap the new OCK for every remaining member in one pass, while the only key directory is
/// the email-keyed lookup capped at 20 calls per day for orgs of up to 50 members.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgMemberKey {
    pub org_id: String,
    /// The member's STABLE server user id — the same identity `org_members.user_id` and a key grant
    /// are keyed on. Never the email.
    pub user_id: String,
    /// The address the key was learned from, when one was known. Diagnostic only; the lookup path
    /// prefers this over asking the server again.
    pub email: Option<String>,
    pub pk_enc: Vec<u8>,
    pub pk_sig: Vec<u8>,
    /// `key_fingerprint(pk_enc, pk_sig)` — the value a grant binds as `recipient_acct_id`.
    pub fingerprint: String,
    pub updated_at: String,
}

/// One `supersessions` row, in the shape the sealed ledger round-trips.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SealedSupersession {
    pub id: String,
    pub superseding_meeting_id: String,
    pub source_meeting_id: String,
    pub entity: String,
    pub predicate: String,
    pub old_value: String,
    pub new_value: String,
    pub created_at: String,
    pub applied_at: Option<String>,
    pub source_pre_image: Option<Vec<u8>>,
    pub superseding_pre_image: Option<Vec<u8>>,
}

/// One meeting's whole fact ledger — what a seal encrypts and an unlock puts back.
///
/// Serialized as JSON and sealed under the folder content key, exactly like the note markdown and
/// the timeline. The rows themselves still leave the database on seal, so the at-rest guarantee is
/// unchanged; what changes is that the seal is now reversible, as every other piece of user content
/// in a locked folder already was.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SealedFactLedger {
    pub facts: Vec<crate::facts::Fact>,
    /// `facts.importance` per fact id. It lives on the ROW but not on `crate::facts::Fact`, and
    /// leaving it out would have made "restored identical" quietly false for the one column the
    /// reflection pass reads to decide what is worth revisiting — it would silently re-assess every
    /// restored fact through the reasoner.
    #[serde(default)]
    pub fact_importance: Vec<(String, f64)>,
    /// User-scoped facts live in their own table with no entity; `Fact::entity_id` is empty for
    /// these and is never written back into `facts`.
    pub user_facts: Vec<crate::facts::Fact>,
    pub supersessions: Vec<SealedSupersession>,
}

impl SealedFactLedger {
    /// Nothing to seal — used to skip a meeting that never had a ledger rather than storing an
    /// empty ciphertext for it.
    pub fn is_empty(&self) -> bool {
        self.facts.is_empty() && self.user_facts.is_empty() && self.supersessions.is_empty()
    }

    /// Carry forward everything `prev` holds that `self` does not, keyed by row id.
    ///
    /// A re-seal reads the LIVE rows, and some rows are deliberately absent from them: a restore
    /// skips a supersession whose other anchor is still sealed. Sealing the live rows alone would
    /// therefore replace the ciphertext with a strict subset of itself and lose those rows for
    /// good. `self` always wins on a shared id — the live row is the current truth — and `prev`
    /// only fills the gaps.
    pub fn merge_missing_from(&mut self, prev: Self) {
        let have_facts: std::collections::HashSet<&str> =
            self.facts.iter().map(|f| f.id.as_str()).collect();
        let carried: Vec<_> = prev
            .facts
            .iter()
            .filter(|f| !have_facts.contains(f.id.as_str()))
            .cloned()
            .collect();
        self.facts.extend(carried);

        let have_user: std::collections::HashSet<&str> =
            self.user_facts.iter().map(|f| f.id.as_str()).collect();
        let carried: Vec<_> = prev
            .user_facts
            .iter()
            .filter(|f| !have_user.contains(f.id.as_str()))
            .cloned()
            .collect();
        self.user_facts.extend(carried);

        let have_sup: std::collections::HashSet<&str> =
            self.supersessions.iter().map(|s| s.id.as_str()).collect();
        let carried: Vec<_> = prev
            .supersessions
            .iter()
            .filter(|s| !have_sup.contains(s.id.as_str()))
            .cloned()
            .collect();
        self.supersessions.extend(carried);

        let have_importance: std::collections::HashSet<&str> = self
            .fact_importance
            .iter()
            .map(|(id, _)| id.as_str())
            .collect();
        let carried: Vec<_> = prev
            .fact_importance
            .iter()
            .filter(|(id, _)| !have_importance.contains(id.as_str()))
            .cloned()
            .collect();
        self.fact_importance.extend(carried);
    }
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
    pub doc_id: Option<String>,
    pub access: String,
    /// The caller's durable PII-scrub choice. Legacy rows migrate to fail-safe `true`.
    pub scrub: bool,
    pub state: String,
    pub last_error: Option<String>,
    pub expected_actor_user_id: Option<String>,
    pub expected_owner_user_id: Option<String>,
    pub source_version: u64,
    pub republish_dirty: u64,
    /// The shared container this document was published under, when the container sweep owns it.
    /// `None` for a standalone share — never guessed.
    pub parent_container_id: Option<String>,
    /// Ordering inside that container.
    pub position: i64,
    /// True when the user shared this document THEMSELVES; false when it exists only because its
    /// container is shared. This is what makes unsharing a container safe: only `explicit == false`
    /// rows are withdrawn with it.
    pub explicit: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// Durable lifecycle of one physical recording attempt. Only verified-empty PREPARED attempts may
/// take the exceptional direct PREPARED -> RETIRED abandonment edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecordingGenerationState {
    Prepared,
    Capturing,
    Finalized,
    Archived,
    Retired,
}

impl RecordingGenerationState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "PREPARED",
            Self::Capturing => "CAPTURING",
            Self::Finalized => "FINALIZED",
            Self::Archived => "ARCHIVED",
            Self::Retired => "RETIRED",
        }
    }

    pub(crate) fn parse(value: &str) -> crate::error::Result<Self> {
        match value {
            "PREPARED" => Ok(Self::Prepared),
            "CAPTURING" => Ok(Self::Capturing),
            "FINALIZED" => Ok(Self::Finalized),
            "ARCHIVED" => Ok(Self::Archived),
            "RETIRED" => Ok(Self::Retired),
            _ => Err(crate::error::AppError::Storage(
                "invalid recording generation state".into(),
            )),
        }
    }
}

/// Allowlisted, content-free reason code. OS error strings and paths never enter the ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecordingCaptureFault {
    MicIo,
    SystemIo,
    DiskFull,
    DeviceLost,
    Interrupted,
}

impl RecordingCaptureFault {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::MicIo => "MIC_IO",
            Self::SystemIo => "SYSTEM_IO",
            Self::DiskFull => "DISK_FULL",
            Self::DeviceLost => "DEVICE_LOST",
            Self::Interrupted => "INTERRUPTED",
        }
    }

    pub(crate) fn parse(value: &str) -> crate::error::Result<Self> {
        match value {
            "MIC_IO" => Ok(Self::MicIo),
            "SYSTEM_IO" => Ok(Self::SystemIo),
            "DISK_FULL" => Ok(Self::DiskFull),
            "DEVICE_LOST" => Ok(Self::DeviceLost),
            "INTERRUPTED" => Ok(Self::Interrupted),
            _ => Err(crate::error::AppError::Storage(
                "invalid recording capture fault code".into(),
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecordingRetirementReason {
    Archived,
    EmptyAbandoned,
}

impl RecordingRetirementReason {
    pub(crate) fn parse(value: &str) -> crate::error::Result<Self> {
        match value {
            "ARCHIVED" => Ok(Self::Archived),
            "EMPTY_ABANDONED" => Ok(Self::EmptyAbandoned),
            _ => Err(crate::error::AppError::Storage(
                "invalid recording retirement reason".into(),
            )),
        }
    }
}

fn canonical_uuid(value: &str, label: &str) -> crate::error::Result<String> {
    let parsed = uuid::Uuid::parse_str(value)
        .map_err(|_| crate::error::AppError::InvalidArg(format!("invalid {label}")))?;
    if parsed.is_nil() || parsed.hyphenated().to_string() != value {
        return Err(crate::error::AppError::InvalidArg(format!(
            "invalid {label}"
        )));
    }
    Ok(value.to_owned())
}

fn canonical_uuid_v4(value: &str, label: &str) -> crate::error::Result<String> {
    let canonical = canonical_uuid(value, label)?;
    let parsed = uuid::Uuid::parse_str(&canonical)
        .map_err(|_| crate::error::AppError::InvalidArg(format!("invalid {label}")))?;
    if parsed.get_version() != Some(uuid::Version::Random) {
        return Err(crate::error::AppError::InvalidArg(format!(
            "invalid {label}"
        )));
    }
    Ok(canonical)
}

fn safe_recording_basename(value: &str) -> crate::error::Result<String> {
    let safe = !value.is_empty()
        && value.len() <= 255
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if !safe {
        return Err(crate::error::AppError::InvalidArg(
            "invalid recording artifact basename".into(),
        ));
    }
    Ok(value.to_owned())
}

fn sha256_hex(value: &str) -> crate::error::Result<String> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(crate::error::AppError::InvalidArg(
            "invalid recording artifact SHA-256".into(),
        ));
    }
    Ok(value.to_owned())
}

fn checked_identity(device: u64, inode: u64) -> crate::error::Result<()> {
    if device == 0 || inode == 0 || device > i64::MAX as u64 || inode > i64::MAX as u64 {
        return Err(crate::error::AppError::InvalidArg(
            "invalid recording artifact identity".into(),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecordingGenerationKey {
    meeting_id: String,
    generation_id: String,
}

impl RecordingGenerationKey {
    pub(crate) fn new(meeting_id: &str, generation_id: &str) -> crate::error::Result<Self> {
        Ok(Self {
            meeting_id: canonical_uuid(meeting_id, "meeting UUID")?,
            generation_id: canonical_uuid_v4(generation_id, "recording generation UUID")?,
        })
    }

    pub(crate) fn fresh(meeting_id: &str) -> crate::error::Result<Self> {
        Self::new(meeting_id, &uuid::Uuid::new_v4().hyphenated().to_string())
    }

    pub(crate) fn meeting_id(&self) -> &str {
        &self.meeting_id
    }

    pub(crate) fn generation_id(&self) -> &str {
        &self.generation_id
    }
}

/// Caller-asserted identity recorded at PREPARED time. This is deliberately not named or treated
/// as verified evidence; only the private capability types in `recording_store` carry that meaning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecordingMicAssertion {
    basename: String,
    sample_rate: u32,
    device: u64,
    inode: u64,
}

impl RecordingMicAssertion {
    pub(crate) fn for_generation(
        key: &RecordingGenerationKey,
        sample_rate: u32,
        device: u64,
        inode: u64,
    ) -> crate::error::Result<Self> {
        checked_identity(device, inode)?;
        if !(8_000..=384_000).contains(&sample_rate) {
            return Err(crate::error::AppError::InvalidArg(
                "invalid recording sample rate".into(),
            ));
        }
        Ok(Self {
            basename: safe_recording_basename(&format!("{}.mic.f32", key.generation_id()))?,
            sample_rate,
            device,
            inode,
        })
    }

    pub(crate) fn basename(&self) -> &str {
        &self.basename
    }

    pub(crate) fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub(crate) fn device(&self) -> u64 {
        self.device
    }

    pub(crate) fn inode(&self) -> u64 {
        self.inode
    }
}

/// Canonical artifact namespace. The role is part of the assertion, so system evidence cannot be
/// replayed as archive evidence even if every numeric identity field were copied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecordingArtifactRole {
    System,
    Archive,
}

impl RecordingArtifactRole {
    fn suffix(self) -> &'static str {
        match self {
            Self::System => "system.wav",
            Self::Archive => "archive.wav",
        }
    }
}

/// Stored checkpoint assertion. Creation validates shape only; state transitions accept a separate
/// private verified-evidence capability, never this freely constructible row shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecordingCheckpointAssertion {
    durable_frames: u64,
    byte_len: u64,
    sha256_prefix: String,
}

impl RecordingCheckpointAssertion {
    pub(crate) fn new(
        durable_frames: u64,
        byte_len: u64,
        sha256_prefix: &str,
    ) -> crate::error::Result<Self> {
        if durable_frames > (i64::MAX as u64) / 4 || byte_len > i64::MAX as u64 {
            return Err(crate::error::AppError::InvalidArg(
                "recording checkpoint size is too large".into(),
            ));
        }
        let expected_len = durable_frames.checked_mul(4).ok_or_else(|| {
            crate::error::AppError::InvalidArg("recording checkpoint size overflow".into())
        })?;
        let sha256_prefix = sha256_hex(sha256_prefix)?;
        if byte_len != expected_len
            || (durable_frames == 0
                && sha256_prefix
                    != "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
        {
            return Err(crate::error::AppError::InvalidArg(
                "invalid recording checkpoint assertion".into(),
            ));
        }
        Ok(Self {
            durable_frames,
            byte_len,
            sha256_prefix,
        })
    }

    pub(crate) fn durable_frames(&self) -> u64 {
        self.durable_frames
    }

    pub(crate) fn byte_len(&self) -> u64 {
        self.byte_len
    }

    pub(crate) fn sha256_prefix(&self) -> &str {
        &self.sha256_prefix
    }
}

/// Stored system/archive metadata. Shape-valid but explicitly not proof of a filesystem read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecordingArtifactAssertion {
    role: RecordingArtifactRole,
    basename: String,
    device: u64,
    inode: u64,
    byte_len: u64,
    sha256: String,
}

impl RecordingArtifactAssertion {
    pub(crate) fn for_generation(
        key: &RecordingGenerationKey,
        role: RecordingArtifactRole,
        device: u64,
        inode: u64,
        byte_len: u64,
        sha256: &str,
    ) -> crate::error::Result<Self> {
        checked_identity(device, inode)?;
        if byte_len == 0 || byte_len > i64::MAX as u64 {
            return Err(crate::error::AppError::InvalidArg(
                "invalid recording artifact length".into(),
            ));
        }
        Ok(Self {
            role,
            basename: safe_recording_basename(&format!(
                "{}.{}",
                key.generation_id(),
                role.suffix()
            ))?,
            device,
            inode,
            byte_len,
            sha256: sha256_hex(sha256)?,
        })
    }

    pub(crate) fn role(&self) -> RecordingArtifactRole {
        self.role
    }

    pub(crate) fn basename(&self) -> &str {
        &self.basename
    }

    pub(crate) fn device(&self) -> u64 {
        self.device
    }

    pub(crate) fn inode(&self) -> u64 {
        self.inode
    }

    pub(crate) fn byte_len(&self) -> u64 {
        self.byte_len
    }

    pub(crate) fn sha256(&self) -> &str {
        &self.sha256
    }
}

#[allow(dead_code)] // Read by the recording coordinator in the next bounded harness slice.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct RecordingGenerationSnapshot {
    pub(crate) key: RecordingGenerationKey,
    pub(crate) state: RecordingGenerationState,
    pub(crate) lease_expires_at_ms: i64,
    pub(crate) mic: RecordingMicAssertion,
    pub(crate) checkpoint: RecordingCheckpointAssertion,
    pub(crate) system_artifact: Option<RecordingArtifactAssertion>,
    pub(crate) capture_fault: Option<RecordingCaptureFault>,
    pub(crate) archive: Option<RecordingArtifactAssertion>,
    pub(crate) retirement_reason: Option<RecordingRetirementReason>,
    pub(crate) created_at_ms: i64,
    pub(crate) updated_at_ms: i64,
    pub(crate) finalized_at_ms: Option<i64>,
    pub(crate) archived_at_ms: Option<i64>,
    pub(crate) retired_at_ms: Option<i64>,
    /// Signed system-first-frame offset from mic capture start. Persisted independently of the
    /// system artifact so CAPTURING crash recovery can reconstruct wall-clock stream alignment.
    pub(crate) system_start_offset_micros: Option<i64>,
    /// Durable post-note cleanup progress. Bits are owned by the recording pipeline and are
    /// advanced only after an exact unlink plus parent-directory fsync.
    pub(crate) cleanup_mask: u8,
}

// ── Dashboards (2026-08-03) ────────────────────────────────────────────────────────────────────
//
// A board is LAYOUT + POINTERS. Neither struct carries meeting content: the tile holds a `kind`
// and an optional `ref_id`, and the command layer resolves that reference through the gated
// readers on every read, so a sealed source surfaces a masked tile rather than a title.

/// One user-composed board.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Dashboard {
    pub id: String,
    pub title: String,
    /// Cosmetic leading emoji (a single grapheme, validated at the command layer).
    pub emoji: Option<String>,
    /// Cosmetic accent key (a design-token NAME such as `indigo`, never a raw colour).
    pub tint: Option<String>,
    pub pinned: bool,
    pub position: i64,
    /// The container this board is filed in; `None` means unfiled, which every board that
    /// predates the hierarchy is. It is also the board's LOCK anchor: a board with no folder
    /// cannot be sealed, because there is no folder whose key would seal it.
    pub folder_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    /// True when this board's folder is sealed and NOT unlocked for this session. The title
    /// and tiles are then masked rather than returned — see `Db::list_dashboards_visible`.
    pub locked: bool,
}

/// One tile on a board — a pointer plus its layout, never content.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardTile {
    pub id: String,
    pub dashboard_id: String,
    /// One of `storage::dashboards_store::TILE_KINDS`.
    pub kind: String,
    /// The anchor into an existing row (meeting / document / entity id). `None` for kinds that
    /// derive from the whole board (e.g. `reminders`).
    pub ref_id: Option<String>,
    /// A user-supplied tile heading. `None` ⇒ the resolver supplies one from the source.
    pub title: Option<String>,
    /// Grid columns spanned, 3–12.
    pub span: i64,
    pub position: i64,
    /// Small per-kind JSON options bag (e.g. the Living-answer question).
    pub config: Option<String>,
    pub created_at: String,
}

// ── RELATED PICKER — the gated hierarchy the "Add related" modal walks ───────────────────────────
//
// A DELIBERATELY SEPARATE type family from `ItemKind`/`ContainerNode`. The global `ItemKind` carries
// `task` and `dashboard`; the picker must never offer either, and "the enum has four variants but
// two of them are filtered out at every call site" is precisely the shape that leaks one back in
// the first time somebody adds a fifth. Three variants, no filtering.

/// The three LINKABLE leaf kinds the picker offers: a recording, an authored note, and an imported
/// document. Never a task, never a dashboard.
///
/// A unit enum, so `rename_all = "camelCase"` is sufficient and there are no variant FIELDS to
/// rename — the distinction that broke `TileData` (#566/#568).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PickerItemKind {
    Meeting,
    Note,
    Document,
}

impl PickerItemKind {
    /// The fixed presentation order of a container's groups.
    pub const ORDER: [PickerItemKind; 3] = [
        PickerItemKind::Meeting,
        PickerItemKind::Note,
        PickerItemKind::Document,
    ];

    /// The `documents.kind` discriminator for the two `documents`-backed kinds. A meeting is not a
    /// `documents` row at all, so it maps to the sentinel that matches nothing — the leaf builder
    /// never asks, and a future caller that does gets an empty result rather than a note.
    pub fn document_kind(self) -> &'static str {
        match self {
            PickerItemKind::Note => "note",
            PickerItemKind::Document => "document",
            PickerItemKind::Meeting => "",
        }
    }

    /// The placeholder shown for a row with no usable title. Resolved in the READER (not the FE) so
    /// the modal, its search results and its keyboard traversal all agree on one label.
    pub fn untitled_label(self) -> &'static str {
        match self {
            PickerItemKind::Meeting => "Untitled recording",
            PickerItemKind::Note => "Untitled note",
            PickerItemKind::Document => "Untitled document",
        }
    }

    /// The [`crate::links::LinkKind`] a picked row links AS.
    pub fn link_kind(self) -> crate::links::LinkKind {
        match self {
            PickerItemKind::Meeting => crate::links::LinkKind::Meeting,
            PickerItemKind::Note => crate::links::LinkKind::Note,
            PickerItemKind::Document => crate::links::LinkKind::Document,
        }
    }
}

/// Which set of leaves a page/index query is scoped to.
///
/// Storage-internal: the wire carries `containerId: string | null`, and `null` means
/// [`PickerScope::Unclassified`]. Modelling it as an enum here rather than an `Option<&str>` is what
/// makes the two unclassified SOURCES explicit — an unfiled RECORDING has no container at all,
/// while an unfiled NOTE lives in the reserved always-open note root — so neither leg can silently
/// inherit the other's predicate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerScope {
    /// The synthetic "Not classified" node: unfiled recordings + reserved-root notes/documents.
    Unclassified,
    /// A real, renderable container (a Space or a folder), by `folders.id`.
    Container(String),
}

/// One linkable LEAF row. Carries NO on-disk path and NO snippet — a title and an id are everything
/// a picker row needs, and everything else is content this read has no business disclosing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PickerRow {
    pub kind: PickerItemKind,
    pub id: String,
    /// Always present — the reader substitutes the per-kind placeholder rather than shipping a null
    /// the FE would have to re-invent a label for.
    pub title: String,
}

/// One SEARCH hit. Identical to a [`PickerRow`] plus the container it lives in, which the command
/// layer turns into a full `Space / folder` breadcrumb from the hierarchy it already resolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PickerSearchRow {
    pub kind: PickerItemKind,
    pub id: String,
    pub title: String,
    /// `None` ⇒ the hit is unclassified (an unfiled recording, or a reserved-root note).
    pub container_id: Option<String>,
}

/// The LIVE identity of a `container` link endpoint, resolved through the visibility gate.
///
/// Storage-internal (never crosses IPC on its own): the name feeds `LinkEdge::other_title` and the
/// level feeds `LinkEdge::other_container_level`, so both come from ONE gated read rather than two
/// that could disagree across a relock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerEndpoint {
    /// The container's CURRENT visible name — resolved at read time, which is what makes a
    /// container chip survive a rename.
    pub name: String,
    /// `"project"` (a Space) or `"folder"`.
    pub level: String,
}

// ── WORKSPACE HIERARCHY — Projects › Folders › items ─────────────────────────────────────────────
//
// A Project and a Folder are BOTH `folders` rows, discriminated by `folders.level`. That is the
// load-bearing decision of the hierarchy design: the seal machinery in `commands/lock.rs` is
// folder-id-keyed and carries no predicate on `folders.kind`, so a project row inherits it whole,
// and a project lock cascades by locking each child folder in its own right — which is why
// `visibility_clause`, every `*_visible` reader and every `folder=<id>` AAD binding are untouched.
// See `docs/superpowers/specs/2026-08-22-workspace-hierarchy-design.md` §2.

/// The four item kinds a container can hold.
///
/// A unit enum, so `rename_all = "camelCase"` is sufficient and there are no variant FIELDS to
/// rename — the distinction that broke `TileData` (#566/#568), where `rename_all` renamed the
/// variants while their snake_case fields reached the FE as `undefined`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ItemKind {
    Meeting,
    Note,
    Task,
    Dashboard,
}

impl ItemKind {
    /// The fixed presentation order of the type groups. An EMPTY group is omitted entirely rather
    /// than rendered with a zero count, so this is an order, not a guarantee of presence.
    pub const ORDER: [ItemKind; 4] = [
        ItemKind::Meeting,
        ItemKind::Note,
        ItemKind::Task,
        ItemKind::Dashboard,
    ];
}

/// One `folders` row as the hierarchy reads it. Storage-internal: it carries `kind`/`is_root`, which
/// the wire DTO does not need in full.
#[derive(Debug, Clone)]
pub struct ContainerRow {
    pub id: String,
    pub name: String,
    pub parent_id: Option<String>,
    /// `"project"` or `"folder"`.
    pub level: String,
    pub emoji: Option<String>,
    pub tint: Option<String>,
    pub position: i64,
    /// The row's own `locked` COLUMN — the disk truth, independent of the session unlock set.
    pub locked: bool,
    pub is_root: bool,
    pub kind: String,
}

/// One row in a container's type group.
///
/// Deliberately a FLAT struct carrying its `kind` as a field rather than a data-carrying enum: the
/// enum shape is exactly what shipped `started_at`/`duration_s`/`has_audio` against a camelCase FE,
/// where every field read `undefined`, the tile threw while rendering and took the whole dashboard
/// board down. A flat struct cannot have that bug, and the serialized-key test pins it anyway.
///
/// It carries NO on-disk path of any kind — see the `convertFileSrc` note on
/// `crate::storage::workspace_store`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemRow {
    pub kind: ItemKind,
    pub id: String,
    /// `None` for an untitled item; the FE supplies its own placeholder.
    pub title: Option<String>,
    /// Meetings only; `None` for every other kind.
    pub duration_s: Option<i64>,
    /// Newest-first sort key, normalised to epoch MILLISECONDS for every kind (meetings store
    /// RFC3339 TEXT, authored notes store epoch-ms INTEGER).
    pub sort_at: i64,
}

/// A container's items of ONE kind: the newest few plus the true total.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeGroup {
    pub kind: ItemKind,
    /// The container's FULL visible count for this kind — `items` is only the first page.
    pub total: u32,
    pub items: Vec<ItemRow>,
}

/// A container in the tree: a Project (with child folders) or a Folder.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerNode {
    pub id: String,
    pub name: String,
    /// Canonical container namespace (`"meeting"` or `"note"`). The frontend uses this only to
    /// hide creation/move affordances that the command layer would refuse; backend gates remain
    /// authoritative for every write.
    pub kind: String,
    /// `"project"` or `"folder"`.
    pub level: String,
    pub emoji: Option<String>,
    pub tint: Option<String>,
    /// Sealed (encrypted) on disk — the `folders.locked` column.
    pub locked: bool,
    /// Sealed AND session-unlocked (decrypted for this session only). Mirrors `FolderNode.unlocked`.
    pub unlocked: bool,
    /// The reserved always-open note root. It can never be sealed, so the FE disables its lock
    /// affordance rather than offering an action that is refused.
    pub is_root: bool,
    /// Child folders (a Project's; empty for a Folder while sub-folders remain out of scope, but the
    /// reader preserves and renders any depth that already exists in a user's database).
    pub folders: Vec<ContainerNode>,
    /// Per-kind groups in [`ItemKind::ORDER`]. An empty group is ABSENT. A sealed-and-not-unlocked
    /// container carries NO groups at all — not even totals.
    pub groups: Vec<TypeGroup>,
}

/// One page of a single container's items of a single kind.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemPage {
    pub kind: ItemKind,
    pub items: Vec<ItemRow>,
    /// The full visible count, so the caller can render "N of M" and know when to stop paging.
    pub total: u32,
}

/// A container resolved on its own, for a breadcrumb or header.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerDto {
    pub id: String,
    pub name: String,
    pub level: String,
    pub emoji: Option<String>,
    pub tint: Option<String>,
    pub locked: bool,
    pub unlocked: bool,
    pub is_root: bool,
    /// The owning project's id and name when this is a Folder; `None` for a Project.
    pub parent_id: Option<String>,
    pub parent_name: Option<String>,
}

/// One row of the OUTBOUND container-share journal (`org_container_shares`) — a Space or Folder
/// this device publishes to an org.
///
/// Not an IPC DTO: the frontend never sees this shape, it sees `ContainerShareStatus`. Keeping the
/// journal row and the wire row distinct is what lets the journal carry crash-recovery fields
/// (`content_sha256`, `last_error`) that have no business crossing to the renderer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerShareRow {
    pub id: String,
    pub org_id: String,
    /// The LOCAL `folders.id`. Never leaves the device.
    pub folder_id: String,
    /// The stable, client-generated manifest identity published as the org `docId`.
    pub container_id: String,
    pub access: String,
    pub scrub: bool,
    /// True for the container the user actually picked; false for a descendant folder that is
    /// shared only because its root is.
    pub is_root: bool,
    pub state: String,
    pub item_id: Option<String>,
    pub rev: u32,
    pub generation: u32,
    pub content_sha256: Option<Vec<u8>>,
    pub position: i64,
    pub last_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// One decrypted container manifest received from an org feed (`org_containers`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgContainerRow {
    pub org_id: String,
    pub container_id: String,
    pub item_id: String,
    /// `"space"` or `"folder"` — the wire level, already validated by `ContainerLevel::parse`.
    pub level: String,
    pub name: String,
    pub emoji: Option<String>,
    pub tint: Option<String>,
    pub parent_container_id: Option<String>,
    pub position: i64,
    pub access: String,
    pub author_hint: String,
    pub author_user_id: Option<String>,
    pub document_owner_user_id: Option<String>,
    pub seq: u64,
    pub rev: u32,
    pub generation: u32,
    pub created_at: String,
}

/// One private, device-local placement of a received org object (`org_local_placements`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalPlacementRow {
    pub org_id: String,
    /// `"container"` or `"doc"`.
    pub target_kind: String,
    pub target_id: String,
    /// The local `folders.id` the user filed it under; `None` means the Shared Brains root.
    pub local_parent_id: Option<String>,
    pub position: i64,
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
