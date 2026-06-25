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
