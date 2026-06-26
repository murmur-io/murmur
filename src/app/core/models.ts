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
  ollamaBaseUrl: string;
  ollamaModel: string;
  claudeBinary: string;
  captureSystemAudio: boolean;
  modelSize: string;
  voiceTrigger: boolean;
  onboarded: boolean;
  noteStyle: string;
  autoOrganize: boolean;
  noteLanguage: string;
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
  exportedPath: string;
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

export interface MeetingDetail {
  meeting: Meeting;
  note: NoteDto | null;
  segments: Segment[];
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

export interface BriefResult {
  markdown: string;
  sources: VaultSource[];
}
