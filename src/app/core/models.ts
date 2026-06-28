// TS mirrors of Rust DTOs (camelCase-serialized). Keep in sync with PHASE0-PLAN §6.

export type Stage =
  | "idle"
  | "recording"
  | "transcribing"
  | "summarizing"
  | "exporting"
  | "done"
  | "error";

export interface StatusPayload {
  stage: Stage;
  message: string;
  meetingId: string | null;
}

export type Availability =
  | { Available: true }
  | { Unavailable: { reason: string } };

export interface ProviderStatus {
  id: string;
  available: boolean;
  reason?: string;
}

export interface AppConfigDto {
  providerId: string;
  vaultPath: string | null;
  vaultSubfolder: string | null;
  whisperModelPath: string | null;
  language: string | null;
  anthropicModel: string;
  /**
   * Brain/AI MODEL override for the active cloud provider (settable, mirrors Rust
   * `AppConfigDto.provider_model`). Empty `""` = provider default.
   */
  providerModel: string;
  /**
   * Brain/AI reasoning EFFORT (`""`/`low`/`medium`/`high`, mirrors Rust
   * `AppConfigDto.provider_effort`). Honored ONLY by the `anthropic` provider.
   */
  providerEffort: string;
  ollamaBaseUrl: string;
  ollamaModel: string;
  claudeBinary: string;
  inputDevice: string | null;
  captureSystemAudio: boolean;
  vadEnabled: boolean;
  keepHiresMasters: boolean;
  diarizeOthers: boolean;
  aecEnabled: boolean;
  modelSize: string;
  voiceTrigger: boolean;
  onboarded: boolean;
  noteStyle: string;
  autoOrganize: boolean;
  noteLanguage: string;
  /**
   * Stage E security flags. These are part of the `get_config` / `save_config`
   * round-trip (Rust `AppConfigDto`), so the FE MUST read the current values and
   * send them back unchanged on every save — otherwise the backend's serde
   * defaults clobber them (`mcpRequireToken` → false, `cloudEgressConsented` →
   * false). Never drop these from a `saveConfig` payload.
   */
  /** Require a bearer token on every MCP method (E3). Default true. */
  mcpRequireToken: boolean;
  /** Require biometric (Touch ID) before unlocking a sealed folder. Default true. */
  lockRequireBiometric: boolean;
  /** Auto-relock + zeroize the cached KEK when screen sharing starts. Default true. */
  relockOnScreenshare: boolean;
  /**
   * E10 — one-time cloud-egress consent. Granted ONLY via the dedicated
   * `consent_to_cloud_egress` command (the auditable, explicit user act);
   * round-tripped on every `save_config` so a normal settings save PRESERVES it
   * (never silently clears it). Default false (fail-closed): no meeting content
   * leaves the device for a cloud LLM until the user has consented once.
   */
  cloudEgressConsented: boolean;
  /**
   * Phase H — the "brain" (AI assistant) backend that answers questions /
   * reacts in-meeting. `"cloud"` = Claude (recommended for live latency),
   * `"local"` = an on-device GGUF model (`brainModelId`), `"off"` = no brain.
   * Round-tripped on every `save_config` like the other flags.
   */
  brainBackend: BrainBackend;
  /**
   * Phase H — the in-meeting voice assistant ("realtime reactions"): listen for
   * a wake phrase during a recording and answer grounded questions live.
   * Default false (off) — it's a power feature with cost/latency tradeoffs.
   */
  realtimeReactions: boolean;
  /**
   * Phase H — the selected local brain model id (a registry id like
   * `"bielik-11b"`, or a custom GGUF file path). Only meaningful when
   * `brainBackend === "local"`. Null = none selected yet.
   */
  brainModelId: string | null;
  /**
   * brain2 RAG — the semantic-search master flag. When on, Ask-My-Vault retrieves
   * candidates by HYBRID (FTS ∪ vector-KNN) retrieval over the on-device embedding
   * model; off falls back to lexical-only. Flipping it on does NOT auto-index — the
   * user runs `reindexEmbeddings()` to backfill. Round-tripped on every `save_config`
   * like the other flags. Default false. Mirrors Rust `AppConfigDto.semantic_search_enabled`.
   */
  semanticSearchEnabled: boolean;
  /**
   * brain2 connectors — the web-search connector MASTER toggle. NEW CLOUD EGRESS:
   * when on (AND `webSearchConsented` AND a Brave key is stored), the brain/Ask
   * answer may send a REDACTED query off-device to the search provider. A settable
   * flag: round-tripped on every `save_config` like the other flags. Default false.
   * Mirrors Rust `AppConfigDto.web_search_enabled`.
   */
  webSearchEnabled: boolean;
  /**
   * brain2 connectors — one-time consent for the web-search egress. Like
   * `cloudEgressConsented`, this is PRESERVE-ONLY on `save_config` (a normal save
   * carries the current value back, never flips it) and is granted SOLELY by the
   * dedicated `consent_to_web_search` command. Default false (fail-closed): no
   * query leaves the device until the user has consented once. Mirrors Rust
   * `AppConfigDto.web_search_consented`.
   */
  webSearchConsented: boolean;
}

/** Phase H — which backend powers the brain / in-meeting voice assistant. */
export type BrainBackend = "cloud" | "local" | "off";

/**
 * Phase H — a selectable local brain model from the registry (`list_brain_models`).
 * Mirrors the Rust `BrainModelDto` (camelCase). RAM-fit / download / selected
 * state are computed by the backend against this Mac.
 */
export interface BrainModelDto {
  id: string;
  name: string;
  /** Human size label (e.g. "6.7 GB") for display. */
  sizeLabel: string;
  /** Exact on-disk size in bytes (for progress math / precise display). */
  bytes: number;
  /** Minimum recommended RAM in GB to run this model. */
  minRamGb: number;
  /** Languages the model handles well (e.g. ["pl", "en"]). */
  languages: string[];
  /** Already downloaded on this Mac. */
  downloaded: boolean;
  /** Fits in this Mac's RAM (false → warn / discourage). */
  fitsRam: boolean;
  /** Currently the selected brain model. */
  selected: boolean;
}

/**
 * Phase H — progress payload for a local brain-model download
 * (`EVENT_BRAIN_DOWNLOAD`). Matches the backend `BrainDownloadPayload`:
 * `downloaded`/`total` (bytes; `total` is null when the server omits
 * Content-Length) drive the progress bar; `done` ends the in-flight state.
 * The backend downloads one model at a time, so the component tracks WHICH
 * model it started locally (errors surface via the download command's promise).
 */
export interface BrainDownloadProgress {
  downloaded: number;
  total: number | null;
  done: boolean;
}

/**
 * brain2 RAG — progress for the on-device embedding-model (multilingual-e5-small)
 * download (`EVENT_EMBED_DOWNLOAD`). Mirrors the backend `EmbedDownloadPayload`:
 * the e5 model is 3 small files, so progress is reported per-file (`fileIndex` /
 * `fileCount`). `total` is null when the server omits Content-Length; `done` fires
 * once all files are written + renamed into place.
 */
export interface EmbedDownloadProgress {
  fileIndex: number;
  fileCount: number;
  downloaded: number;
  total: number | null;
  done: boolean;
}

/**
 * brain2 RAG — progress for the semantic-search backfill (`EVENT_REINDEX`). Counts
 * only, NO PII. Mirrors the backend `ReindexPayload`: visible meetings indexed so
 * far (`done`) out of the run total (`total`).
 */
export interface ReindexProgress {
  done: number;
  total: number;
}

/**
 * brain2 RAG — result of `reindexEmbeddings()`. Mirrors the backend `ReindexResult`
 * (camelCase). `status` is `"model_missing"` when the real e5 model is absent (no
 * indexing was attempted — the FE nudges the user to download it first), else
 * `"indexed"`; `indexed` is the count of VISIBLE meetings whose chunks were rebuilt.
 */
export interface ReindexResult {
  status: string;
  indexed: number;
  total: number;
}

/**
 * Phase H — fired when the in-meeting voice assistant hears its wake phrase
 * (`EVENT_WAKE_DETECTED`). The FE shows a pending "heard: {command}" row.
 */
export interface WakeDetectedPayload {
  matchedPhrase: string;
  command: string;
  intent: string;
}

/**
 * Phase H — status of a completed voice action. Mirrors the backend
 * `VoiceActionResult.status` strings (`voice_action.rs`): `ok` (done) |
 * `needs_consent` (cloud brain refused, fail-closed) | `unavailable` (deferred
 * capability, e.g. Slack) | `unrecognized` (nothing actionable parsed) |
 * `nothing_heard` (a manual capture's budget expired with NOTHING spoken) |
 * `error` (best-effort failure, message non-PII).
 */
export type VoiceActionStatus =
  | "ok"
  | "needs_consent"
  | "unavailable"
  | "unrecognized"
  | "nothing_heard"
  | "error";

/**
 * Phase H — the result of a voice action (`EVENT_VOICE_ACTION_RESULT`): the
 * HEARD command (the user's own dictated words, so the card can show
 * "usłyszano: {command}"; empty when nothing was heard), a short summary +
 * grounding citations (meeting titles → [[wikilink]] chips) + a status pill.
 */
export interface VoiceActionResultPayload {
  intentKind: string;
  status: VoiceActionStatus;
  summary: string;
  /** What the user actually said (their OWN dictation). Empty when nothing was heard. */
  command: string;
  citations: string[];
}

/**
 * Phase H — fired when the manual "Ask AI" voice-command trigger toggles the
 * assistant listener (`EVENT_VOICE_COMMAND_LISTENING`). `active` flips true the
 * moment the backend opens the mic for a spoken command and false once it has
 * captured the utterance and dispatched it (the answer then arrives via
 * `EVENT_VOICE_ACTION_RESULT`). Drives the inline "🎙 Słucham…" indicator.
 */
export interface VoiceCommandListeningPayload {
  active: boolean;
}

/**
 * Fired when a MANUAL voice-command capture has STOPPED listening and the
 * accumulated utterance is being DISPATCHED (`EVENT_VOICE_COMMAND_PROCESSING`).
 * `active` flips true the instant the backend stops capturing and the gated
 * `handle_voice_action` round-trip (RAG + brain) begins, and is cleared (false
 * is implied) when the answer lands via `EVENT_VOICE_ACTION_RESULT`. Drives the
 * "🧠 Przetwarzam…" processing state in the gap between stop and answer.
 */
export interface VoiceCommandProcessingPayload {
  active: boolean;
}

/** A selectable microphone input device (from `list_input_devices`). */
export interface InputDeviceInfo {
  name: string;
  isDefault: boolean;
}

export interface NoteDto {
  meetingId: string;
  providerId: string;
  markdown: string;
  exportedPath: string | null;
}

export interface StartResult {
  meetingId: string;
}

export interface StopResult {
  meetingId: string;
  markdown: string;
  /** Path of the exported Obsidian `.md`, or `null` when no vault is configured (the note is
   *  still saved to Murmur — the vault is export-only). */
  exportedPath: string | null;
}

export type MeetingStatus =
  | "DRAFT"
  | "RECORDING"
  | "TRANSCRIBED"
  | "SUMMARIZED"
  | "EXPORTED"
  | "ERROR";

export interface Meeting {
  id: string;
  startedAt: string;
  endedAt: string | null;
  title: string | null;
  durationS: number;
  audioPath: string | null;
  status: MeetingStatus;
  /** Owning folder id, or null when at the vault root. */
  folderId?: string | null;
}

/** A folder row as returned by createFolder (mirrors the Rust `Folder` DTO). */
export interface Folder {
  id: string;
  name: string;
  /** Vault-relative folder path. */
  path: string;
  parentId: string | null;
  locked: boolean;
  createdAt: string;
}

/**
 * A folder node for the tree UI. `locked` = sealed (encrypted) on disk; `unlocked` = sealed AND
 * decrypted for this session (visible in-app + MCP until relock). An open folder is
 * `locked=false, unlocked=false`.
 */
export interface FolderNode {
  id: string;
  name: string;
  parentId: string | null;
  noteCount: number;
  locked: boolean;
  unlocked: boolean;
  children: FolderNode[];
}

export interface Segment {
  idx: number;
  startS: number;
  endS: number;
  text: string;
  /**
   * Who produced this segment: `"me"` (the local mic / recorder), `"others"`
   * (captured system audio), or `null` for legacy / mic-only recordings made
   * before speaker attribution existed. A bare `string` is tolerated so a future
   * backend can carry richer labels without a model break.
   */
  speaker?: "me" | "others" | string | null;
}

/**
 * One persisted in-meeting voice-assistant interaction (Q&A): the user's spoken
 * command, the assistant's answer, the grounding citations, and the dispatch
 * status. Surfaced in the meeting detail's "Asystent — Q&A" section. EMPTY when
 * the meeting is locked-and-not-session-unlocked (gated, like note/segments) and
 * also empty for a sealed meeting at rest (the rows are PURGED on seal). Mirrors
 * the Rust `AssistantInteraction` (serde camelCase).
 */
export interface AssistantInteraction {
  /** The HEARD command — the user's own dictated words. */
  command: string;
  /** The assistant's answer (research/recall result or a status line). */
  answer: string;
  /** `[[Title]]` wikilink / "(web)" citations the answer was grounded on. */
  citations: string[];
  /**
   * Dispatch status: `ok` | `unavailable` | `unrecognized` | `needs_consent` |
   * `error` | `nothing_heard`.
   */
  status: string;
  /** Coarse source label / intent kind, e.g. `research` / `recall`. */
  sourceLabel: string | null;
  /** RFC3339 timestamp the interaction was recorded. */
  createdAt: string;
}

export interface MeetingDetail {
  meeting: Meeting;
  note: NoteDto | null;
  segments: Segment[];
  /**
   * Persisted in-meeting assistant Q&A for this meeting. Present (gated) only
   * when the meeting is unlocked; empty otherwise. The FE renders these in the
   * "🎙 Asystent — Q&A" detail section.
   */
  assistantInteractions: AssistantInteraction[];
  /**
   * True when this meeting lives in a sealed-and-NOT-session-unlocked folder.
   * The backend MASKS the payload in that case (title "🔒 Locked", note null,
   * segments [], audioPath null) — the FE renders a lock gate instead of the
   * note/transcript/audio/timeline. Re-fetch after `unlockMeeting` to get the
   * full unmasked content (`locked` then absent/false).
   */
  locked?: boolean;
}

export interface StatusCount {
  status: string;
  count: number;
}

export interface DayCount {
  date: string;
  count: number;
  durationS: number;
}

export interface Analytics {
  totalMeetings: number;
  totalDurationS: number;
  avgDurationS: number;
  longestDurationS: number;
  meetings7d: number;
  duration7dS: number;
  notesCount: number;
  firstMeetingAt: string | null;
  byStatus: StatusCount[];
  perDay: DayCount[];
}

export interface SpeakerTurn {
  speaker: string;
  startS: number;
  endS: number;
}

export interface TopicSpan {
  label: string;
  startS: number;
  endS: number;
}

export interface MeetingTimeline {
  speakers: SpeakerTurn[];
  topics: TopicSpan[];
}

export interface SearchHit {
  meeting: Meeting;
  snippet: string;
  matchedIn: string;
}

export interface ChatTurn {
  role: "user" | "assistant";
  content: string;
}

export interface BuiltinRecipe {
  id: string;
  label: string;
  prompt: string;
}

export interface SavedRecipe {
  id: string;
  title: string;
  prompt: string;
  createdAt: string;
}

export interface ActionItem {
  idx: number;
  done: boolean;
  text: string;
  owner: string | null;
  dueDate: string | null;
}

export interface PinResult {
  url: string;
  blockId: string;
  mmss: string;
}

export interface GraphPayload {
  people: string[];
  projects: string[];
}

export interface VaultSource {
  meetingId: string;
  title: string;
  startedAt: string;
}

/** The two entity kinds the self-assembling graph resolves (Rust `EntityKind`, camelCase). */
export type EntityKind = "person" | "project";

/** A graph entity row (person or project), first-seen casing preserved in `name`. */
export interface GraphEntity {
  id: string;
  name: string;
  kind: EntityKind;
  createdAt: string;
}

/** A graph node = an entity + its VISIBLE mention count (sealed meetings contribute zero). */
export interface GraphNode {
  id: string;
  name: string;
  kind: EntityKind;
  /** VISIBLE mention count — never the true count; sealed-not-unlocked meetings drop out. */
  mentionCount: number;
}

/**
 * An undirected co-occurrence edge between two entities sharing ≥1 VISIBLE meeting.
 * `source` < `target` (deduped); `weight` = number of shared visible meetings.
 */
export interface GraphEdge {
  source: string;
  target: string;
  weight: number;
}

/** The full graph payload from `getGraph()`: visible nodes + visible edges + a hidden flag. */
export interface GraphData {
  nodes: GraphNode[];
  edges: GraphEdge[];
  /** True when ≥1 folder is sealed-and-not-unlocked → render one honest disclosure banner. */
  hasHidden: boolean;
}

/** A co-occurring neighbor of a selected entity (a neighborhood satellite). */
export interface EntityNeighbor {
  id: string;
  name: string;
  kind: EntityKind;
  /** Number of VISIBLE meetings the two entities share. */
  sharedMeetings: number;
}

/** Detail for one entity: the entity, its visible backlinked meetings, top neighbors. */
export interface EntityDetail {
  entity: GraphEntity;
  /** Visible meetings mentioning this entity (reuses the `VaultSource` backlink chip shape). */
  meetings: VaultSource[];
  neighbors: EntityNeighbor[];
}

export interface AskVaultResult {
  answer: string;
  sources: VaultSource[];
}

export interface DigestResult {
  markdown: string;
  exportedPath: string | null;
}

export interface TopicMention {
  meetingId: string;
  title: string;
  startedAt: string;
  startS: number;
  endS: number;
}

export interface TopicThread {
  label: string;
  count: number;
  mentions: TopicMention[];
}

export interface CalendarEvent {
  title: string;
  start: string | null;
}

/** A full local Calendar event from the EventKit sidecar — title + attendees + agenda. */
export interface CalendarEventFull {
  id: string;
  title: string;
  start: string | null;
  end: string | null;
  attendees: string[];
  notes: string;
}

/** A compact calendar-context block (title + attendees + agenda) for a meeting. */
export interface CalendarContext {
  eventId: string;
  title: string;
  attendees: string[];
  text: string;
}

export interface BriefResult {
  markdown: string;
  sources: VaultSource[];
}
