import { Injectable } from "@angular/core";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  ActionItem,
  Analytics,
  AppConfigDto,
  BrainDownloadProgress,
  BrainModelDto,
  InputDeviceInfo,
  AskVaultResult,
  BriefResult,
  BuiltinRecipe,
  CalendarEvent,
  CalendarEventFull,
  CalendarContext,
  ChatTurn,
  DigestResult,
  EntityDetail,
  Folder,
  FolderNode,
  GraphData,
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
  VoiceActionResultPayload,
  WakeDetectedPayload,
} from "./models";

export const EVENT_STATUS = "meetnotes://status";
export const EVENT_VOICE_START = "murmur://voice-start";
export const EVENT_TOGGLE_RECORD = "murmur://toggle-record";
export const EVENT_LIVE_CAPTION = "murmur://live-caption";
// Phase H — the brain / in-meeting voice assistant event stream.
export const EVENT_WAKE_DETECTED = "murmur://wake-detected";
export const EVENT_VOICE_ACTION_RESULT = "murmur://voice-action-result";
export const EVENT_BRAIN_DOWNLOAD = "murmur://brain-download";

/**
 * Thin wrapper over @tauri-apps/api invoke/listen. One method per Tauri command
 * (PHASE0-PLAN §7) plus onStatus() for the EVENT_STATUS event stream.
 *
 * SECURITY TODO (D9 — Rust-side, NOT fixed here; this layer must not be trusted as a
 * validation boundary). With `withGlobalTauri: false` the IPC surface is not exposed on
 * `window.__TAURI__`, but the following commands still take UNBOUNDED, user-controlled
 * string inputs that reach the backend (DB / LLM-prompt / filesystem) with no length or
 * content cap on the FE. The Rust command handlers MUST enforce the real bound (reject /
 * truncate over a max length, reject control chars / NUL, normalize) — a malicious or
 * buggy renderer can send arbitrarily large or hostile strings:
 *   - search_meetings(query)            — free-text search term
 *   - rename_meeting(title)             — note title
 *   - chat_meeting(question, history)   — chat prompt + full history array
 *   - ask_vault(question, history)      — vault-wide chat prompt + history
 *   - save_recipe(title, prompt)        — recipe title + prompt body
 *   - run_recipe(prompt)                — ad-hoc prompt body
 *   - add_reminder(text)                — reminder text
 *   - create_folder(name)              — folder name (also a path component → reject `/`, `..`, NUL)
 *   - rename_speaker(...)               — speaker label
 * Do NOT rely on this TS wrapper for any of the above limits — enforce in the #[tauri::command].
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

  /**
   * Mute / unmute the local microphone on the LIVE recorder. Muting silences
   * only the mic — captured system audio ("others") keeps recording. No-op when
   * not recording.
   */
  setMicMuted(muted: boolean): Promise<void> {
    return invoke<void>("set_mic_muted", { muted });
  }

  /** Whether the live recorder's mic is currently muted (false when not recording). */
  isMicMuted(): Promise<boolean> {
    return invoke<boolean>("is_mic_muted");
  }

  /** Available microphone input devices for the picker (name + system-default flag). */
  listInputDevices(): Promise<InputDeviceInfo[]> {
    return invoke<InputDeviceInfo[]>("list_input_devices");
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

  /**
   * E10 — grant the one-time cloud-egress consent. This is the ONLY supported way
   * to flip `cloudEgressConsented` true: the backend persists the flag AND updates
   * its in-memory config cache, so the next cloud summarize/chat/brief
   * (claude_code / anthropic) is allowed to build. Idempotent.
   *
   * Until this is called, every cloud provider rejects with
   * "cloud egress not consented …" — the FE surfaces that as a consent prompt
   * rather than a silent failure.
   */
  consentToCloudEgress(): Promise<void> {
    return invoke<void>("consent_to_cloud_egress");
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

  /**
   * The self-assembling graph: all VISIBLE entity nodes (with visible mention counts) + all
   * VISIBLE co-occurrence edges + a `hasHidden` flag. Sealed-not-unlocked meetings contribute
   * nothing; re-fetch on a FoldersService lock-state change to drop sealed entities live.
   */
  getGraph(): Promise<GraphData> {
    return invoke<GraphData>("get_graph");
  }

  /** Detail for one entity: the entity + its visible backlinked meetings + top neighbors. */
  getEntityDetail(entityId: string): Promise<EntityDetail> {
    return invoke<EntityDetail>("get_entity_detail", { entityId });
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

  /**
   * Local Calendar events (title + attendees + agenda) in a window around now, via the on-device
   * EventKit sidecar. Empty array if Calendar access is denied or nothing's scheduled — never throws.
   */
  listCalendarEvents(): Promise<CalendarEventFull[]> {
    return invoke<CalendarEventFull[]>("list_calendar_events");
  }

  /** Compact calendar context (title + attendees + agenda) for one event, or null if not found. */
  calendarContextFor(eventId: string): Promise<CalendarContext | null> {
    return invoke<CalendarContext | null>("calendar_context_for", { eventId });
  }

  /** Copy a meeting's recording (WAV) to a chosen path. */
  exportAudio(meetingId: string, destPath: string): Promise<void> {
    return invoke<void>("export_audio", { meetingId, destPath });
  }

  /**
   * Copy a meeting's MIC hi-res master archive (the faithful native-rate float32
   * WAV kept when "Keep high-fidelity masters" was on) to a chosen path. Rejects
   * with InvalidArg ("…has no master for that stream") when the meeting was
   * recorded without that archive, and fails closed with Locked for a
   * sealed-and-not-session-unlocked folder — surface both as friendly messages.
   */
  exportMicMaster(meetingId: string, destPath: string): Promise<void> {
    return invoke<void>("export_mic_master", { meetingId, destPath });
  }

  /**
   * Copy a meeting's SYSTEM hi-res master archive (the faithful 48 kHz float32
   * WAV of the captured "others" stream) to a chosen path. Same failure modes as
   * {@link exportMicMaster}: InvalidArg when no system master exists, Locked when
   * the meeting's folder is sealed and not session-unlocked.
   */
  exportSysMaster(meetingId: string, destPath: string): Promise<void> {
    return invoke<void>("export_sys_master", { meetingId, destPath });
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

  /**
   * Resolve this meeting's owning folder and run the biometric unlock_folder
   * path (Touch ID). Returns the updated `FolderNode` for the folder, or null
   * when the meeting is at the vault root / in an open folder. After a success,
   * re-fetch `getMeetingDetail` to get the full unmasked content. Rejects when
   * the biometric prompt is denied / cancelled.
   */
  unlockMeeting(meetingId: string): Promise<FolderNode | null> {
    return invoke<FolderNode | null>("unlock_meeting", { meetingId });
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

  // ── Phase H — brain (AI assistant) model registry ──────────────────────

  /**
   * The selectable local brain models (Bielik / Qwen3-14B / Qwen2.5-3B …) with
   * per-Mac `downloaded` / `fitsRam` / `selected` state computed by the backend.
   */
  listBrainModels(): Promise<BrainModelDto[]> {
    return invoke<BrainModelDto[]>("list_brain_models");
  }

  /** Make `modelId` the active local brain model (also persisted to config). */
  selectBrainModel(modelId: string): Promise<void> {
    return invoke<void>("select_brain_model", { modelId });
  }

  /**
   * Download a local brain model by id. Progress streams over
   * {@link onBrainDownload} (EVENT_BRAIN_DOWNLOAD); the promise resolves when
   * the download finishes (or rejects on failure).
   */
  downloadBrainModel(modelId: string): Promise<void> {
    return invoke<void>("download_brain_model", { modelId });
  }

  /** Show/hide the floating always-on-top recorder bar window. */
  toggleBar(): Promise<void> {
    return invoke<void>("toggle_bar");
  }

  // ── folders + per-folder lock lifecycle (PHASE0-PLAN Stage C) ──

  /** The folder tree (roots → children) with per-folder note counts + session lock state. */
  listFolders(): Promise<FolderNode[]> {
    return invoke<FolderNode[]>("list_folders");
  }

  /** Create a folder under an optional parent; creates the matching vault subdirectory. */
  createFolder(name: string, parentId: string | null): Promise<Folder> {
    return invoke<Folder>("create_folder", { name, parentId });
  }

  /**
   * Rename a folder: change its display name + the matching vault subdirectory + every governed path,
   * without ever touching sealed content (a LOCKED folder rename is metadata-only). Returns the
   * updated `Folder`. Rejects with InvalidArg for an empty/invalid name.
   */
  renameFolder(folderId: string, newName: string): Promise<Folder> {
    return invoke<Folder>("rename_folder", { folderId, newName });
  }

  /**
   * Delete a folder, NEVER losing a note. Its notes move to the vault root ("All notes"); the folder
   * row + (now-empty) vault subdir are removed. Rejects with `Locked` when the folder is sealed and
   * NOT session-unlocked (unlock it first), and with InvalidArg when it still has subfolders.
   */
  deleteFolder(folderId: string): Promise<void> {
    return invoke<void>("delete_folder", { folderId });
  }

  /** Move a note into a folder (or to the vault root with `folderId = null`). */
  moveNote(meetingId: string, folderId: string | null): Promise<void> {
    return invoke<void>("move_note", { meetingId, folderId });
  }

  /** Seal a folder: encrypt its notes into content blobs, blank markdown, remove vault .md. */
  lockFolder(folderId: string): Promise<void> {
    return invoke<void>("lock_folder", { folderId });
  }

  /** Session-unlock a sealed folder (decrypt into markdown for this session; no re-export). */
  unlockFolder(folderId: string): Promise<FolderNode> {
    return invoke<FolderNode>("unlock_folder", { folderId });
  }

  /** Re-seal a single session-unlocked folder (re-blank markdown; folder stays locked on disk). */
  relockFolder(folderId: string): Promise<void> {
    return invoke<void>("relock_folder", { folderId });
  }

  /** Re-seal ALL session-unlocked folders + zeroize the cached KEK (e.g. on screen-share). */
  relockAll(): Promise<void> {
    return invoke<void>("relock_all");
  }

  /** Permanently remove a folder's lock: decrypt to plaintext + re-export to the vault. */
  removeLock(folderId: string): Promise<void> {
    return invoke<void>("remove_lock", { folderId });
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

  // ── Phase H — brain / in-meeting voice assistant event streams ─────────

  /**
   * Fires when the in-meeting voice assistant hears its wake phrase. The FE
   * shows a pending "heard: {command}" row until the matching
   * {@link onVoiceActionResult} arrives.
   */
  onWakeDetected(cb: (p: WakeDetectedPayload) => void): Promise<UnlistenFn> {
    return listen<WakeDetectedPayload>(EVENT_WAKE_DETECTED, (e) =>
      cb(e.payload),
    );
  }

  /** Fires with the result of a voice action (summary + citations + status). */
  onVoiceActionResult(
    cb: (p: VoiceActionResultPayload) => void,
  ): Promise<UnlistenFn> {
    return listen<VoiceActionResultPayload>(EVENT_VOICE_ACTION_RESULT, (e) =>
      cb(e.payload),
    );
  }

  /** Fires with progress for an in-flight local brain-model download. */
  onBrainDownload(
    cb: (p: BrainDownloadProgress) => void,
  ): Promise<UnlistenFn> {
    return listen<BrainDownloadProgress>(EVENT_BRAIN_DOWNLOAD, (e) =>
      cb(e.payload),
    );
  }
}
