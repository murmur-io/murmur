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
