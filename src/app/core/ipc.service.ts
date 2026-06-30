import { Injectable } from "@angular/core";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  ActionItem,
  Analytics,
  AppConfigDto,
  BrainDownloadProgress,
  BrainModelDto,
  EmbedDownloadProgress,
  GatewayHealth,
  GatewayModel,
  ReindexProgress,
  ReindexResult,
  InputDeviceInfo,
  AskVaultResult,
  BriefResult,
  BuiltinRecipe,
  CalendarEvent,
  CalendarEventFull,
  CalendarContext,
  ChatTurn,
  DigestResult,
  DocumentInfo,
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
  VoiceCommandListeningPayload,
  VoiceCommandProcessingPayload,
  AssistantToolPayload,
  ChatMsg,
  WakeDetectedPayload,
} from "./models";

export const EVENT_STATUS = "meetnotes://status";
export const EVENT_VOICE_START = "murmur://voice-start";
export const EVENT_TOGGLE_RECORD = "murmur://toggle-record";
export const EVENT_LIVE_CAPTION = "murmur://live-caption";
// Phase H — the brain / in-meeting voice assistant event stream.
export const EVENT_WAKE_DETECTED = "murmur://wake-detected";
export const EVENT_VOICE_ACTION_RESULT = "murmur://voice-action-result";
export const EVENT_VOICE_COMMAND_LISTENING = "murmur://voice-command-listening";
export const EVENT_VOICE_COMMAND_PROCESSING =
  "murmur://voice-command-processing";
// The live tool-trace of an in-meeting agentic turn (one event per tool call).
export const EVENT_ASSISTANT_TOOL = "murmur://assistant-tool";
// The chat panel's own tool-trace stream (kept separate from the assistant card's).
export const EVENT_CHAT_TOOL = "murmur://chat-tool";
export const EVENT_BRAIN_DOWNLOAD = "murmur://brain-download";
// brain2 RAG — semantic-search model download + reindex backfill event streams.
export const EVENT_EMBED_DOWNLOAD = "murmur://embed-download";
export const EVENT_REINDEX = "murmur://reindex-embeddings";

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

  /**
   * AI Gateway (Phase 1) — store/replace the gateway API key in Keychain.
   * An empty/blank key is rejected — call `clearGatewayKey` to remove an existing key.
   * The key is NEVER logged and NEVER returned to the FE — only `hasGatewayKey` reports presence.
   * Mirrors {@link setAnthropicKey}.
   */
  setGatewayKey(key: string): Promise<void> {
    return invoke<void>("set_gateway_key", { key });
  }

  /** Whether a gateway API key is currently stored. Never the value. Mirrors {@link hasAnthropicKey}. */
  hasGatewayKey(): Promise<boolean> {
    return invoke<boolean>("has_gateway_key");
  }

  /**
   * AI Gateway (Phase 1) — remove the stored gateway API key from Keychain (if any).
   * No-op when no key is stored. Mirrors how one might remove the Anthropic key.
   */
  clearGatewayKey(): Promise<void> {
    return invoke<void>("clear_gateway_key");
  }

  /**
   * AI Gateway (Phase 3) — fetch the model catalog from the configured gateway's
   * `/v1/models` endpoint. Returns an array of `{ id }` objects (one per model).
   * Rejects when the gateway is unreachable, the key is wrong, or the endpoint does
   * not exist (not all gateways expose `/v1/models`). The FE falls back to a plain
   * text input when the list is empty or the call rejects.
   */
  listGatewayModels(): Promise<GatewayModel[]> {
    return invoke<GatewayModel[]>("list_gateway_models");
  }

  /**
   * AI Gateway (Phase 4) — probe whether the configured gateway is reachable and
   * return the number of models in its catalog. The backend never errors on this
   * command (unreachable → `{ reachable: false, modelCount: 0 }`). The FE still
   * catches for safety.
   */
  gatewayHealth(): Promise<GatewayHealth> {
    return invoke<GatewayHealth>("gateway_health");
  }

  /**
   * brain2 connectors — grant the one-time consent for WEB-SEARCH egress. This is
   * the ONLY supported way to flip `webSearchConsented` true: the backend persists
   * the flag AND updates its in-memory config cache, so the next brain/Ask answer
   * may expose the web connector (provided web search is enabled AND a key is
   * stored). Idempotent. Until granted, the redacted query never leaves the device.
   * Mirrors {@link consentToCloudEgress}.
   */
  consentToWebSearch(): Promise<void> {
    return invoke<void>("consent_to_web_search");
  }

  /**
   * brain2 connectors — store/replace the BYO web-search (Brave) API key in the
   * Keychain. An empty string clears it. The key is NEVER logged and NEVER returned
   * to the FE — only {@link hasWebSearchKey} reports presence. Mirrors
   * {@link setAnthropicKey}.
   */
  setWebSearchApiKey(key: string): Promise<void> {
    return invoke<void>("set_web_search_api_key", { key });
  }

  /** Whether a web-search (Brave) API key is currently stored. Never the value. */
  hasWebSearchKey(): Promise<boolean> {
    return invoke<boolean>("has_web_search_key");
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

  /**
   * Semantic neighbors of a meeting ("Powiązane wg znaczenia"). Returns `[]`
   * when `semantic_search_enabled` is off or the meeting has no neighbors —
   * the FE simply renders nothing in that case. Gated server-side.
   */
  relatedMeetings(meetingId: string): Promise<SearchHit[]> {
    return invoke<SearchHit[]>("related_meetings", { meetingId });
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

  // ── brain2 documents — expand the brain with imported .md/.txt files ────

  /**
   * Import a `.md`/`.txt` document into `folderId` to expand the brain: the
   * backend reads it as UTF-8 text, stores it, and (when the on-device embedding
   * model is present) indexes it into the vector layer. Resolves with the new
   * document id. Rejects with `AppError::Locked` when the folder is
   * sealed-and-NOT-session-unlocked (never resurrect plaintext behind a lock),
   * and with `AppError::InvalidArg` for a non-`.md`/`.txt` or unreadable file —
   * surface both as friendly messages. The path comes from the native file
   * dialog (`@tauri-apps/plugin-dialog` `open`); only `.md`/`.txt` are accepted.
   */
  importDocument(path: string, folderId: string): Promise<string> {
    return invoke<string>("import_document", { path, folderId });
  }

  /**
   * A folder's documents (metadata only — name + created_at; NO text). GATED
   * server-side: a sealed-and-NOT-session-unlocked folder returns an EMPTY array
   * (masked — never even a document name behind the lock), so re-fetch on a
   * FoldersService lock-state change to drop masked docs live.
   */
  listDocuments(folderId: string): Promise<DocumentInfo[]> {
    return invoke<DocumentInfo[]>("list_documents", { folderId });
  }

  /**
   * Read ONE document's full text. GATED: a sealed-and-NOT-session-unlocked
   * folder returns "" (masked — never the stored text), like `getManualNotes`.
   */
  getDocument(id: string): Promise<string> {
    return invoke<string>("get_document", { id });
  }

  /**
   * Permanently delete a document (cascade-deletes its chunks + vectors). GATED:
   * a sealed-and-NOT-session-unlocked folder is refused (`AppError::Locked`);
   * an unknown id is an idempotent no-op.
   */
  deleteDocument(id: string): Promise<void> {
    return invoke<void>("delete_document", { id });
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

  // ── brain2 RAG — semantic search (embedding model + reindex backfill) ───

  /**
   * Whether the on-device embedding model (multilingual-e5-small) is present —
   * i.e. the REAL embedder would load. Cheap existence check; semantic search /
   * reindex are no-ops (stub) without it.
   */
  embedModelPresent(): Promise<boolean> {
    return invoke<boolean>("embed_model_present");
  }

  /**
   * Download the on-device embedding model (multilingual-e5-small, 3 small files).
   * Progress streams over {@link onEmbedDownload} (EVENT_EMBED_DOWNLOAD); the
   * promise resolves with the model dir when the download finishes. Sends NO
   * meeting content (inbound-only, no egress).
   */
  downloadEmbedModel(): Promise<string> {
    return invoke<string>("download_embed_model");
  }

  /**
   * brain2 RAG — backfill the semantic vector index for ALL VISIBLE meetings (the
   * one-shot run after turning semantic search on, or after installing the e5
   * model). Progress streams over {@link onReindex} (EVENT_REINDEX). Resolves with
   * a {@link ReindexResult}: `status: "model_missing"` when the e5 model is absent
   * (nothing indexed → the FE nudges the user to download it first), else
   * `"indexed"`. Sealed-not-unlocked meetings are never indexed (gated).
   */
  reindexEmbeddings(): Promise<ReindexResult> {
    return invoke<ReindexResult>("reindex_embeddings");
  }

  /** Show/hide the floating always-on-top recorder bar window. */
  toggleBar(): Promise<void> {
    return invoke<void>("toggle_bar");
  }

  /**
   * Phase H — manually trigger the in-meeting voice assistant to listen for a
   * spoken command (the "Ask AI" button in the recording bar), bypassing the
   * wake phrase. The backend emits `EVENT_VOICE_COMMAND_LISTENING {active:true}`
   * while the mic is open, then `{active:false}` once it captures the utterance;
   * the answer arrives via `EVENT_VOICE_ACTION_RESULT`. The promise resolves once
   * listening has STARTED (not when the answer is ready).
   */
  beginVoiceCommand(): Promise<void> {
    return invoke<void>("begin_voice_command");
  }

  /**
   * Phase H — CLICK-TO-STOP: stop the in-meeting voice-command listener the user
   * started with {@link beginVoiceCommand}, so the FULL accumulated post-click
   * utterance is dispatched (over the same gated `handle_voice_action` path). The
   * backend emits `EVENT_VOICE_COMMAND_PROCESSING {active:true}` while the
   * dispatch is in flight, then the answer arrives via `EVENT_VOICE_ACTION_RESULT`.
   * A no-op when nothing is armed (double-click / already auto-stopped) — never throws.
   */
  endVoiceCommand(): Promise<void> {
    return invoke<void>("end_voice_command");
  }

  /**
   * Ask the in-meeting assistant a TYPED question (the text composer — the twin of
   * the voice trigger). Routes the typed command through the SAME gated agentic
   * brain as voice: the model decides which gated tools to call, the live tool
   * trace streams via {@link onAssistantTool}, and the answer arrives via
   * {@link onVoiceActionResult}. Resolves once the turn is dispatched (not when
   * the answer is ready). Rejects (InvalidArg) on an empty question.
   */
  askAssistantText(text: string): Promise<void> {
    return invoke<void>("ask_assistant_text", { text });
  }

  /**
   * Ask the in-meeting assistant a CHAT message (the dedicated multi-turn panel).
   * Sends the FULL conversation (incl. the new user message as the last item) so
   * the brain has multi-turn memory, and RESOLVES with the reply (summary +
   * citations + status) so the panel can resolve the in-flight assistant bubble.
   * The live tool-trace streams via {@link onChatTool}.
   */
  askAssistantChat(messages: ChatMsg[]): Promise<VoiceActionResultPayload> {
    return invoke<VoiceActionResultPayload>("ask_assistant_chat", { messages });
  }

  // ── brain2 realtime typed @brain notes (record screen "My notes") ──────

  /**
   * Persist a meeting's live typed-notes buffer (debounced autosave from the
   * record screen "My notes" editor). GATED server-side: a
   * sealed-and-not-session-unlocked meeting is refused with `AppError::Locked`
   * (never resurrect typed plaintext behind a lock) — the FE swallows that and
   * keeps the local draft. The text is the user's OWN words (no new egress).
   */
  saveManualNotes(meetingId: string, text: string): Promise<void> {
    return invoke<void>("save_manual_notes", { meetingId, text });
  }

  /**
   * Read a meeting's live typed-notes buffer (the editor rehydrates from this on
   * mount / when the active meeting changes). GATED server-side: a
   * sealed-and-not-session-unlocked meeting returns "" (masked, never the buffer).
   */
  getManualNotes(meetingId: string): Promise<string> {
    return invoke<string>("get_manual_notes", { meetingId });
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

  /**
   * Fires when the manual "Ask AI" voice-command listener opens (`active:true`)
   * and closes (`active:false`). Drives the inline "🎙 Słucham…" listening
   * indicator + the pulsing Ask-AI button in the recording bar.
   */
  onVoiceCommandListening(
    cb: (p: VoiceCommandListeningPayload) => void,
  ): Promise<UnlistenFn> {
    return listen<VoiceCommandListeningPayload>(
      EVENT_VOICE_COMMAND_LISTENING,
      (e) => cb(e.payload),
    );
  }

  /**
   * Fires when a manual voice command has STOPPED listening and is being
   * DISPATCHED (`active:true`) — the gap between capture-stop and the answer
   * landing via {@link onVoiceActionResult}. Drives the "🧠 Przetwarzam…"
   * processing state of the assistant orb.
   */
  onVoiceCommandProcessing(
    cb: (p: VoiceCommandProcessingPayload) => void,
  ): Promise<UnlistenFn> {
    return listen<VoiceCommandProcessingPayload>(
      EVENT_VOICE_COMMAND_PROCESSING,
      (e) => cb(e.payload),
    );
  }

  /**
   * Fires once per TOOL CALL the in-meeting brain makes during an agentic turn,
   * so the card can render the live tool-trace chips ("Searching notes… ✓").
   * NO PII — tool name + a coarse count only.
   */
  onAssistantTool(
    cb: (p: AssistantToolPayload) => void,
  ): Promise<UnlistenFn> {
    return listen<AssistantToolPayload>(EVENT_ASSISTANT_TOOL, (e) =>
      cb(e.payload),
    );
  }

  /** The CHAT panel's own live tool-trace (separate from the assistant card's). */
  onChatTool(cb: (p: AssistantToolPayload) => void): Promise<UnlistenFn> {
    return listen<AssistantToolPayload>(EVENT_CHAT_TOOL, (e) => cb(e.payload));
  }

  /** Fires with progress for an in-flight local brain-model download. */
  onBrainDownload(
    cb: (p: BrainDownloadProgress) => void,
  ): Promise<UnlistenFn> {
    return listen<BrainDownloadProgress>(EVENT_BRAIN_DOWNLOAD, (e) =>
      cb(e.payload),
    );
  }

  /** Fires with per-file progress for the in-flight embedding-model download. */
  onEmbedDownload(
    cb: (p: EmbedDownloadProgress) => void,
  ): Promise<UnlistenFn> {
    return listen<EmbedDownloadProgress>(EVENT_EMBED_DOWNLOAD, (e) =>
      cb(e.payload),
    );
  }

  /** Fires with COUNT-only progress for the in-flight semantic reindex backfill. */
  onReindex(cb: (p: ReindexProgress) => void): Promise<UnlistenFn> {
    return listen<ReindexProgress>(EVENT_REINDEX, (e) => cb(e.payload));
  }
}
