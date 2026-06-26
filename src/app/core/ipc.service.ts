import { Injectable } from "@angular/core";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  ActionItem,
  Analytics,
  AppConfigDto,
  AskVaultResult,
  BriefResult,
  BuiltinRecipe,
  CalendarEvent,
  ChatTurn,
  DigestResult,
  GraphPayload,
  Meeting,
  MeetingDetail,
  MeetingTimeline,
  NoteDto,
  PinResult,
  ProviderStatus,
  SavedRecipe,
  SearchHit,
  StartResult,
  StatusPayload,
  StopResult,
  TopicThread,
} from "./models";

export const EVENT_STATUS = "meetnotes://status";
export const EVENT_VOICE_START = "murmur://voice-start";
export const EVENT_TOGGLE_RECORD = "murmur://toggle-record";
export const EVENT_LIVE_CAPTION = "murmur://live-caption";

/**
 * Thin wrapper over @tauri-apps/api invoke/listen. One method per Tauri command
 * (PHASE0-PLAN §7) plus onStatus() for the EVENT_STATUS event stream.
 */
@Injectable({ providedIn: "root" })
export class IpcService {
  startRecording(): Promise<StartResult> {
    return invoke<StartResult>("start_recording");
  }

  stopRecording(): Promise<StopResult> {
    return invoke<StopResult>("stop_recording");
  }

  recordingLevel(): Promise<number> {
    return invoke<number>("recording_level");
  }

  getLastNote(): Promise<NoteDto | null> {
    return invoke<NoteDto | null>("get_last_note");
  }

  /** Replace a meeting's note markdown (in-app edit) + re-write the vault file in place. */
  updateNote(meetingId: string, markdown: string): Promise<NoteDto> {
    return invoke<NoteDto>("update_note", { meetingId, markdown });
  }

  getConfig(): Promise<AppConfigDto> {
    return invoke<AppConfigDto>("get_config");
  }

  saveConfig(config: AppConfigDto): Promise<void> {
    return invoke<void>("save_config", { config });
  }

  setAnthropicKey(key: string): Promise<void> {
    return invoke<void>("set_anthropic_key", { key });
  }

  hasAnthropicKey(): Promise<boolean> {
    return invoke<boolean>("has_anthropic_key");
  }

  providerStatuses(): Promise<ProviderStatus[]> {
    return invoke<ProviderStatus[]>("provider_statuses");
  }

  resummarize(meetingId: string): Promise<StopResult> {
    return invoke<StopResult>("resummarize", { meetingId });
  }

  listMeetings(): Promise<Meeting[]> {
    return invoke<Meeting[]>("list_meetings");
  }

  /** Search meetings (title + transcript + note) for the Library search box. */
  searchMeetings(query: string): Promise<SearchHit[]> {
    return invoke<SearchHit[]>("search_meetings", { query });
  }

  /** Permanently delete a meeting (audio + vault note + all DB rows). Irreversible. */
  deleteMeeting(meetingId: string): Promise<void> {
    return invoke<void>("delete_meeting", { meetingId });
  }

  /** Rename a meeting's title. */
  renameMeeting(meetingId: string, title: string): Promise<void> {
    return invoke<void>("rename_meeting", { meetingId, title });
  }

  /** Ask a grounded question about a meeting's transcript (chat with the meeting). */
  chatMeeting(
    meetingId: string,
    question: string,
    history: ChatTurn[],
  ): Promise<string> {
    return invoke<string>("chat_meeting", { meetingId, question, history });
  }

  /** Built-in recipe templates (quick chips). */
  listBuiltinRecipes(): Promise<BuiltinRecipe[]> {
    return invoke<BuiltinRecipe[]>("list_builtin_recipes");
  }

  /** User-saved recipe templates. */
  listSavedRecipes(): Promise<SavedRecipe[]> {
    return invoke<SavedRecipe[]>("list_saved_recipes");
  }

  /** Save a recipe template. */
  saveRecipe(title: string, prompt: string): Promise<SavedRecipe> {
    return invoke<SavedRecipe>("save_recipe", { title, prompt });
  }

  /** Delete a saved recipe. */
  deleteRecipe(id: string): Promise<void> {
    return invoke<void>("delete_recipe", { id });
  }

  /** Run a recipe prompt over a meeting's transcript (grounded). */
  runRecipe(meetingId: string, prompt: string): Promise<string> {
    return invoke<string>("run_recipe", { meetingId, prompt });
  }

  /** Parse a meeting note's action-item checklist into structured items. */
  getActionItems(meetingId: string): Promise<ActionItem[]> {
    return invoke<ActionItem[]>("get_action_items", { meetingId });
  }

  /** Rewrite the note's action items into Obsidian Tasks format + re-write the vault file. */
  patchNoteTasks(meetingId: string): Promise<NoteDto> {
    return invoke<NoteDto>("patch_note_tasks", { meetingId });
  }

  /** Add a macOS Reminder for an action item (best-effort, TCC-gated). */
  addReminder(text: string, dueDate: string | null): Promise<void> {
    return invoke<void>("add_reminder", { text, dueDate });
  }

  /** Pin a meeting moment → ^block-ref in the note + an obsidian:// deep link. */
  pinMoment(
    meetingId: string,
    seconds: number,
    label: string,
  ): Promise<PinResult> {
    return invoke<PinResult>("pin_moment", { meetingId, seconds, label });
  }

  /** Build the self-assembling graph: write [[Person]]/[[Project]] stub notes + backlinks. */
  linkMeetingEntities(meetingId: string): Promise<GraphPayload> {
    return invoke<GraphPayload>("link_meeting_entities", { meetingId });
  }

  /** Ask-My-Vault: grounded Q&A across ALL meetings, with source meetings. */
  askVault(question: string, history: ChatTurn[]): Promise<AskVaultResult> {
    return invoke<AskVaultResult>("ask_vault", { question, history });
  }

  /** Generate a Weekly Vault Digest over the last `days` days (writes to vault Digests/). */
  generateDigest(days: number): Promise<DigestResult> {
    return invoke<DigestResult>("generate_digest", { days });
  }

  /** Topic Threads: cross-meeting topic clusters from cached timelines. */
  topicThreads(): Promise<TopicThread[]> {
    return invoke<TopicThread[]>("topic_threads");
  }

  /** Export a meeting as an Obsidian Canvas (.canvas) board. Returns the written path. */
  exportCanvas(meetingId: string): Promise<string> {
    return invoke<string>("export_canvas", { meetingId });
  }

  /** Pre-Meeting Brief: grounded prep card for an upcoming meeting subject, from history. */
  preMeetingBrief(subject: string): Promise<BriefResult> {
    return invoke<BriefResult>("pre_meeting_brief", { subject });
  }

  /** Best-effort next macOS Calendar event (title) in the next hour, or null. */
  nextCalendarEvent(): Promise<CalendarEvent | null> {
    return invoke<CalendarEvent | null>("next_calendar_event");
  }

  /** Copy a meeting's recording (WAV) to a chosen path. */
  exportAudio(meetingId: string, destPath: string): Promise<void> {
    return invoke<void>("export_audio", { meetingId, destPath });
  }

  /** Write a meeting's note markdown to a chosen path. */
  exportNote(meetingId: string, destPath: string): Promise<void> {
    return invoke<void>("export_note", { meetingId, destPath });
  }

  /** Best-effort detection of a running meeting app (Zoom/Teams/Webex), else null. */
  detectMeetingApp(): Promise<string | null> {
    return invoke<string | null>("detect_meeting_app");
  }

  /** Replace a meeting's tags. */
  setMeetingTags(meetingId: string, tags: string[]): Promise<void> {
    return invoke<void>("set_meeting_tags", { meetingId, tags });
  }

  /** A meeting's tags (sorted). */
  getMeetingTags(meetingId: string): Promise<string[]> {
    return invoke<string[]>("get_meeting_tags", { meetingId });
  }

  /** All distinct tags across meetings (Library filter). */
  listAllTags(): Promise<string[]> {
    return invoke<string[]>("list_all_tags");
  }

  /** Meetings carrying a given tag, newest first. */
  listMeetingsByTag(tag: string): Promise<Meeting[]> {
    return invoke<Meeting[]>("list_meetings_by_tag", { tag });
  }

  /** Aggregate analytics for the dashboard + Analytics tab. */
  getAnalytics(): Promise<Analytics> {
    return invoke<Analytics>("get_analytics");
  }

  getMeetingDetail(meetingId: string): Promise<MeetingDetail | null> {
    return invoke<MeetingDetail | null>("get_meeting_detail", { meetingId });
  }

  /** AI-derived speaker + topic timeline for a meeting (generated + cached on first call). */
  getTimeline(meetingId: string): Promise<MeetingTimeline> {
    return invoke<MeetingTimeline>("get_timeline", { meetingId });
  }

  /** Rename a speaker across a meeting's timeline (e.g. "User 1" → "Sarah"). */
  renameSpeaker(
    meetingId: string,
    oldLabel: string,
    newLabel: string,
  ): Promise<MeetingTimeline> {
    return invoke<MeetingTimeline>("rename_speaker", {
      meetingId,
      oldLabel,
      newLabel,
    });
  }

  /** Whether a usable Whisper model is present (configured path or default models dir). */
  modelPresent(): Promise<boolean> {
    return invoke<boolean>("model_present");
  }

  /** Download the default Whisper model (~150 MB) if missing; resolves with its path. */
  downloadModel(): Promise<string> {
    return invoke<string>("download_model");
  }

  /** Show/hide the floating always-on-top recorder bar window. */
  toggleBar(): Promise<void> {
    return invoke<void>("toggle_bar");
  }

  onStatus(cb: (payload: StatusPayload) => void): Promise<UnlistenFn> {
    return listen<StatusPayload>(EVENT_STATUS, (event) => cb(event.payload));
  }

  /** Fires when the backend voice listener hears the wake phrase. */
  onVoiceStart(cb: () => void): Promise<UnlistenFn> {
    return listen(EVENT_VOICE_START, () => cb());
  }

  /** Fires when the menu-bar (tray) "Start / Stop recording" item is chosen. */
  onToggleRecord(cb: () => void): Promise<UnlistenFn> {
    return listen(EVENT_TOGGLE_RECORD, () => cb());
  }

  /** Fires with the latest live-transcription caption during recording. */
  onLiveCaption(cb: (text: string) => void): Promise<UnlistenFn> {
    return listen<{ text: string }>(EVENT_LIVE_CAPTION, (e) =>
      cb(e.payload.text),
    );
  }
}
