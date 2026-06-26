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
}

export interface MeetingDetail {
  meeting: Meeting;
  note: NoteDto | null;
  segments: Segment[];
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
