import { Injectable, signal } from "@angular/core";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  AcceptedShare,
  AccountStatus,
  ActionItem,
  ActiveSharesReport,
  AiMapRow,
  Analytics,
  AppConfigDto,
  AppInfo,
  ApplyResult,
  AskConversation,
  AskConversationScope,
  ContainerSharePreview,
  ContainerShareResult,
  ContainerShareStatus,
  OrgShareTargetRow,
  AskConversationSendResult,
  AskConversationSummary,
  AskVaultResult,
  AssistantThreadRow,
  AssistantToolPayload,
  AuditExplanation,
  AuditFinding,
  AuditRunSummary,
  AuditSchedule,
  BacklinkSource,
  BrainDownloadProgress,
  BrainModelDto,
  BrainOverview,
  BriefProposedPayload,
  BriefRun,
  BriefSchedule,
  BuiltinRecipe,
  CalendarContext,
  CalendarEvent,
  CalendarEventFull,
  ChatMsg,
  ChatTurn,
  ClaimAlignment,
  CompanionAppendResult,
  ContainerNode,
  ContentDeletedPayload,
  ContextHit,
  Dashboard,
  DashboardDetail,
  DashboardSummary,
  DashboardTint,
  DigestResult,
  BulkImportProgress,
  DocImportProgress,
  ImportReport,
  ImportScanReport,
  ImportSourceId,
  DocumentInfo,
  DossierData,
  EchoSuppressedPayload,
  EgressLedger,
  EmbedDownloadProgress,
  EntityDetail,
  EntityKnowledgeDiff,
  Folder,
  FolderNode,
  FullGraphData,
  FullGraphOpts,
  GatewayHealth,
  GatewayModel,
  GraphData,
  GraphPayload,
  InputDeviceInfo,
  ItemKind,
  ItemPage,
  LinkEdge,
  LinkKind,
  LivingAnswerTileData,
  MachineChangeNudge,
  ManualLinkEdge,
  Meeting,
  MeetingActionSummary,
  MeetingDetail,
  MeetingOrgShareInfo,
  MeetingOrgShareRow,
  MeetingTimeline,
  ModelCatalog,
  ModelDownloadOutcome,
  ModelDownloadProgress,
  MyShareEntry,
  NerDownloadProgress,
  NoteAssistRequest,
  NoteAssistResult,
  NoteAttachmentDto,
  NoteAttachmentOwnerKind,
  NoteCitation,
  NoteDoc,
  NoteDto,
  NoteFolder,
  NoteRecipe,
  NoteSummary,
  NoteTemplate,
  NoteTemplateSection,
  OrgAccess,
  OrganizePlan,
  OrganizeApplyResult,
  FilingRecoveryStatus,
  WorkspaceOrganizeApplyResult,
  WorkspaceOrganizeMove,
  WorkspaceOrganizePlan,
  OrgFeedUpdatedPayload,
  OrgItemDetail,
  OrgItemHeader,
  OrgItemImportResult,
  OrgMember,
  OrgShareEntry,
  OrgSharePreview,
  OrgSourceRef,
  OrgSourceShareStatus,
  OrgStatus,
  OrgSyncReport,
  OrgTask,
  PeopleList,
  PinResult,
  Posture,
  ProactiveHintPayload,
  PropertySchemaField,
  ProviderStatus,
  PruneSummary,
  RecipientPreview,
  RecordingCappedPayload,
  RecordingCaptureFaultPayload,
  RecordingStatus,
  ReindexProgress,
  ReindexResult,
  ReminderDraft,
  ReminderSourceAnchor,
  ReminderSourceUpdatedPayload,
  RemindersSnapshot,
  ReminderSuggestionView,
  ReminderSummary,
  RemindersUpdatedPayload,
  ReminderView,
  RetiredModelNudge,
  SavedRecipe,
  SavedView,
  SearchHit,
  Segment,
  ShareInboxItem,
  SharedPlacementTarget,
  SharedWorkspace,
  ShareToUserResult,
  SourceKind,
  SourceRef,
  SpeakerSuggestion,
  StartResult,
  StatusPayload,
  StopResult,
  StorageReport,
  SupersessionDto,
  TaskAssignee,
  TaskDraft,
  TaskLocalRef,
  TileKind,
  TopicThread,
  TypedNoteRow,
  UpdateInfo,
  UserMemory,
  VerifyFindingDto,
  VoiceActionResultPayload,
  VoiceCommandListeningPayload,
  VoiceCommandProcessingPayload,
  VoiceprintInfo,
  WakeDetectedPayload,
  WhisperCard,
  WhisperRecommendationDto,
  WikiTarget,
  McpStatus,
} from "./models";

export const EVENT_STATUS = "meetnotes://status";
export const EVENT_VOICE_START = "murmur://voice-start";
export const EVENT_TOGGLE_RECORD = "murmur://toggle-record";
export const EVENT_LIVE_CAPTION = "murmur://live-caption";
export const EVENT_ECHO_SUPPRESSED = "murmur://echo-suppressed";
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
// The Ask page's own tool-trace stream (separate from the record-screen streams).
export const EVENT_ASK_TOOL = "murmur://ask-tool";
// Whisper transcribe-model download progress stream.
export const EVENT_MODEL_DOWNLOAD = "murmur://model-download";
// Proactive brain (P2) — one zero-egress recall hint from the live-loop matcher.
export const EVENT_PROACTIVE_HINT = "murmur://proactive-hint";
export const EVENT_BRAIN_DOWNLOAD = "murmur://brain-download";
// Realtime Reactions (Murmur Brain Live) — one on-device "whisper" contradiction card.
export const EVENT_WHISPER_CARD = "murmur://whisper-card";
// brain2 RAG — semantic-search model download + reindex backfill event streams.
export const EVENT_EMBED_DOWNLOAD = "murmur://embed-download";
export const EVENT_REINDEX = "murmur://reindex-embeddings";
// Brain v3 PR-2 — extract→chunk→embed progress for an in-flight document import (counts + stage, NO PII).
export const EVENT_DOC_IMPORT = "murmur://doc-import";
export const EVENT_BULK_IMPORT = "murmur://bulk-import";
// Phase D — on-device PERSON-name NER (redaction) model download progress stream.
export const EVENT_NER_DOWNLOAD = "murmur://ner-download";
// Recording-storage: an AUTO-prune freed ≥1 old recording's audio to stay under the cap.
export const EVENT_STORAGE_PRUNED = "murmur://storage-pruned";
// Recording hit the 4h hard TIME cap and self-stopped (distinct from the byte-based prune).
export const EVENT_RECORDING_CAPPED = "murmur://recording-capped";
export const EVENT_RECORDING_CAPTURE_FAULT = "murmur://recording-capture-fault";
export const EVENT_MIC_AUTO_UNMUTED = "murmur://mic-auto-unmuted";
// Brain v2 L5 — a scheduled brief was STAGED (propose-accept; run id + label + size only).
export const EVENT_BRIEF_PROPOSED = "murmur://brief-proposed";
// Vault Audit — the audit state changed (a run finished / findings purged by a seal or delete).
// Payload shape is deliberately untrusted — listeners refetch via list_audit_findings.
export const EVENT_AUDIT_UPDATED = "murmur://audit-updated";
// Shared Brain — the background org-sync loop INGESTED/TOMBSTONED ≥1 org item this tick
// (content-free "something changed, re-fetch" ping; drives the Notes org picker live refresh).
export const EVENT_ORG_FEED_UPDATED = "murmur://org-feed-updated";
/** Progress of one container share. Counts only — no folder name, no item id. */
export const EVENT_CONTAINER_SHARE_PROGRESS =
  "murmur://container-share-progress";
// Delete fan-out fix — a note/meeting delete FULLY succeeded (local rows gone + any org shares
// revoked); lets OTHER open surfaces (the tab-strip) prune themselves. Content-free (id + kind only).
export const EVENT_CONTENT_DELETED = "murmur://content-deleted";
/** Count-only invalidation for the first-class Murmur reminder inbox. */
export const EVENT_REMINDERS_UPDATED = "murmur://reminders-updated";
/** Kind + opaque source id only; Smart cards re-audit through the gated command. */
export const EVENT_REMINDER_SOURCE_UPDATED = "murmur://reminder-source-updated";
/** No-payload privacy barrier: every cached reminder source title must be discarded immediately. */
export const EVENT_REMINDER_VISIBILITY_INVALIDATED =
  "murmur://reminder-visibility-invalidated";
/** No-payload privacy barrier: every durable Ask history cache must be purged immediately. */
export const EVENT_ASK_HISTORY_INVALIDATED = "murmur://ask-history-invalidated";

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
  private readonly _workspaceMutationRevision = signal(0);
  /**
   * Content writes that can change the mixed Workspace hierarchy without going through
   * `WorkspaceService` bump this revision after backend confirmation. Consumers refetch
   * canonical SQLite state; no content is mirrored in the signal itself.
   */
  readonly workspaceMutationRevision =
    this._workspaceMutationRevision.asReadonly();
  startRecording(folderId: string | null = null): Promise<StartResult> {
    return invoke<StartResult>("start_recording", { folderId });
  }

  stopRecording(companionFlushCompleted?: boolean): Promise<StopResult> {
    return invoke<StopResult>("stop_recording", { companionFlushCompleted });
  }

  recordingLevel(): Promise<number> {
    return invoke<number>("recording_level");
  }

  /**
   * The backend's truth for "is a recording in flight RIGHT NOW" (+ the
   * in-progress meeting id / start time). `RecorderStore.init()` calls this once
   * per webview load to reconcile a stale optimistic stage: a webview reload (or
   * a pipeline task that died before this webview session even loaded) leaves
   * the FE stage disagreeing with the long-lived Rust process.
   */
  recordingStatus(): Promise<RecordingStatus> {
    return invoke<RecordingStatus>("recording_status");
  }

  /**
   * Mute / unmute the local microphone on the LIVE recorder. Muting silences
   * only the mic — captured system audio ("others") keeps recording. Muting is
   * rejected until the backend has observed a real system-audio frame, so a Mac
   * that degraded to mic-only can never be silenced completely. No-op when not
   * recording.
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

  /** Is the default audio output the built-in speakers (echo risk)? null = undeterminable. */
  outputIsBuiltinSpeakers(): Promise<boolean | null> {
    return invoke<boolean | null>("output_is_builtin_speakers");
  }

  getLastNote(): Promise<NoteDto | null> {
    return invoke<NoteDto | null>("get_last_note");
  }

  /** Replace a meeting's note markdown (in-app edit) + re-write the vault file in place. */
  updateNote(meetingId: string, markdown: string): Promise<NoteDto> {
    return invoke<NoteDto>("update_note", { meetingId, markdown });
  }

  /**
   * VERIFY PASS: check the note's Jira ticket claims against LIVE Jira (deterministic — the LLM is
   * never the judge). On-demand only; rides the Jira connector's enable+consent gate and refuses a
   * locked meeting. Returns one finding per referenced ticket.
   */
  verifyNoteSources(meetingId: string): Promise<VerifyFindingDto[]> {
    return invoke<VerifyFindingDto[]>("verify_note_sources", { meetingId });
  }

  /** Persist the reviewed verify findings as non-destructive inline `> ` markers in the note. */
  applyNoteVerifyMarkers(
    meetingId: string,
    findings: VerifyFindingDto[],
  ): Promise<NoteDto> {
    return invoke<NoteDto>("apply_note_verify_markers", {
      meetingId,
      findings,
    });
  }

  /**
   * ENRICH: preview live context to fold into the note, from EVERY consented connector (Jira via
   * precise issue-key lookup; other connectors via a title search). This is the egress moment
   * (explicit, like verify); refuses a locked meeting; returns `[]` when there is nothing to add.
   */
  enrichNoteContext(meetingId: string): Promise<ContextHit[]> {
    return invoke<ContextHit[]>("enrich_note_context", { meetingId });
  }

  /**
   * Persist the reviewed context hits as ONE consolidated, dated `> [!context]-` callout appended to
   * the note (byte-exact undo: pass `[]` to strip it). No egress — the hits were already fetched.
   */
  applyNoteEnrichment(meetingId: string, hits: ContextHit[]): Promise<NoteDto> {
    return invoke<NoteDto>("apply_note_enrichment", { meetingId, hits });
  }

  /**
   * STAGE 2 / LANE A — MANUAL re-link / backfill of cross-meeting `[[links]]` over a FINISHED note.
   * ZERO egress: the pass retrieves over the user's OWN visible notes (double visibility-gated,
   * self-excluding) and appends an additive, idempotent, byte-exact-undo `> [!related]-` block —
   * nothing leaves the device. The AUTO pipeline runs the same pass as a deferred post-`Exported`
   * step; this is the on-demand refresh so the panel can re-link after later meetings land. Lock-
   * gated + seal-safe (a sealed meeting is a silent no-op). Re-fetch the note (getMeetingDetail)
   * after it resolves to render the refreshed links.
   */
  linkRelatedNotes(meetingId: string): Promise<void> {
    return invoke<void>("link_related_notes", { meetingId });
  }

  /**
   * Re-Truth — preview the facts a just-finished meeting SUPERSEDES from earlier
   * notes (pending only; `applied` always false). Returns `[]` when nothing moved on.
   */
  previewSupersessions(meetingId: string): Promise<SupersessionDto[]> {
    return invoke<SupersessionDto[]>("preview_supersessions", { meetingId });
  }

  /**
   * Heal the vault: APPEND an Obsidian `[!superseded]` callout to each selected
   * supersession's source note (append-only, reversible). Sealed sources are skipped.
   */
  applySupersessions(ids: string[]): Promise<ApplyResult> {
    return invoke<ApplyResult>("apply_supersessions", { ids });
  }

  /** Undo a heal — remove the appended callouts, restoring the originals. */
  undoSupersessions(ids: string[]): Promise<void> {
    return invoke<void>("undo_supersessions", { ids });
  }

  getConfig(): Promise<AppConfigDto> {
    return invoke<AppConfigDto>("get_config");
  }

  /**
   * The ready-to-paste Claude Code MCP config block (pretty JSON, `type: "http"`),
   * carrying the localhost URL and — when `mcp_require_token` is on (default) — the
   * `Authorization: Bearer <token>` header so the handshake authenticates. When the
   * token flag is off the config has no headers block.
   */
  getMcpConfig(): Promise<string> {
    return invoke<string>("get_mcp_config");
  }

  /**
   * Whether the local server for Claude actually came up.
   *
   * Settings used to assert it was running and hand over a config regardless; a bind failure was
   * a log line nobody reads. This is the read that lets the screen tell the truth.
   */
  getMcpStatus(): Promise<McpStatus> {
    return invoke<McpStatus>("get_mcp_status");
  }

  saveConfig(config: AppConfigDto): Promise<void> {
    return invoke<void>("save_config", { config });
  }

  /** Recording-storage usage report (on-disk path, byte totals, cap, auto-prune flag). */
  getStorageReport(): Promise<StorageReport> {
    return invoke<StorageReport>("get_storage_report");
  }

  /** Prune oldest recordings to the cap NOW (no-op with no cap set). Never touches notes/locked audio. */
  freeUpSpace(): Promise<PruneSummary> {
    return invoke<PruneSummary>("free_up_space");
  }

  /** Reveal the recordings folder in Finder. */
  revealAudioDir(): Promise<void> {
    return invoke<void>("reveal_audio_dir");
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

  /**
   * Revoke the cloud-egress consent granted via {@link consentToCloudEgress}.
   * The backend persists `cloudEgressConsented = false` AND updates its
   * in-memory config cache, so every cloud-classified provider fails closed
   * again until the user re-allows. Idempotent. Mirrors
   * {@link consentToCloudEgress} (same unit-return command shape).
   */
  revokeCloudEgress(): Promise<void> {
    return invoke<void>("revoke_cloud_egress");
  }

  // ── M3-CLIENT: sharing account + zero-knowledge link shares (mode A) ──

  /** Current sharing-account session state (logged in? unlocked? consented? server set?). */
  accountStatus(): Promise<AccountStatus> {
    return invoke<AccountStatus>("account_status");
  }

  /**
   * THE missing send-code step of the sign-up flow. Builds a `ShareClient` from
   * the configured `shareBaseUrl` and calls `POST /v1/auth/signup {email}` → 202,
   * which triggers the server to EMAIL the 6-digit verification code. Resolves
   * regardless of whether the email exists (the server always 202s for
   * anti-enumeration), so a resolved promise means "the code was sent if the
   * address is valid" — surface a neutral "check your inbox" notice, never a
   * "user exists" signal. Rejects (`InvalidArg`) on an empty email, and
   * (`Unavailable`) when the server is unreachable. Do NOT call
   * {@link accountSignup} to send the code — that command consumes the
   * single-use signup token. Mirrors {@link accountLogin} in shape.
   */
  accountSendCode(email: string): Promise<void> {
    return invoke<void>("account_send_code", { email });
  }

  /**
   * Persist that the user resolved the first-run sharing decision (local-only OR
   * the account door), so the init gateway (`/welcome`) never shows again. The
   * SOLE mutator that flips `sharingChoiceMade` true (a one-way latch — a normal
   * `saveConfig` can never set/clear it, PRESERVE-ONLY, exactly like the consent
   * flags). Idempotent, unit return. Mirrors {@link consentToShareEgress}.
   */
  markSharingChoiceMade(): Promise<void> {
    return invoke<void>("mark_sharing_choice_made");
  }

  /**
   * Create a sharing account. Runs the OPAQUE client registration on-device (the password never
   * leaves the Mac), generates the account key material + (skippable) recovery phrase, and uploads
   * it. `code` is the 6-digit email verification code. Returns the 24-word recovery phrase ONLY when
   * `saveRecovery` is true (else `null` — skipped).
   */
  accountSignup(
    email: string,
    code: string,
    password: string,
    saveRecovery: boolean,
  ): Promise<string | null> {
    return invoke<string | null>("account_signup", {
      email,
      code,
      password,
      saveRecovery,
    });
  }

  /** Log in (OPAQUE); unwraps MK for the session and stores the tokens in the Keychain. */
  accountLogin(email: string, password: string): Promise<AccountStatus> {
    return invoke<AccountStatus>("account_login", { email, password });
  }

  /**
   * Re-unlock the session for sharing with a SINGLE Touch ID sheet — no password.
   * Requires being logged in with a cached account key on this device (mirror of
   * {@link AccountStatus.biometricUnlockAvailable}); presents one biometric prompt,
   * restores the session MK, and returns the fresh {@link AccountStatus} (now
   * `unlockedForSharing: true`). Fails closed with an `AppError`: `Unavailable`
   * ("not signed in" / "no cached account key") or `BiometricFailed` on
   * cancel/failure — callers fall back to the password sign-in flow.
   */
  unlockSharingWithBiometric(): Promise<AccountStatus> {
    return invoke<AccountStatus>("unlock_sharing_with_biometric");
  }

  /** Log out: server family-revoke (best-effort) + clear Keychain tokens + drop the session MK. */
  accountLogout(): Promise<void> {
    return invoke<void>("account_logout");
  }

  /** Grant the one-time share-egress consent (mirrors {@link consentToCloudEgress}). */
  consentToShareEgress(): Promise<void> {
    return invoke<void>("consent_to_share_egress");
  }

  /** Revoke the share-egress consent (the next share is refused fail-closed). */
  revokeShareEgress(): Promise<void> {
    return invoke<void>("revoke_share_egress");
  }

  /**
   * Create a zero-knowledge link share of a note and return the share URL. The note is cleaned
   * (frontmatter/wikilinks/obsidian:// stripped) and sealed on-device; only ciphertext + wrapped
   * keys leave. The URL's `#…` fragment carries the decryption key `L` and is assembled locally —
   * `L` never reaches the server (never log the returned URL). Refuses (`Locked`) a sealed meeting;
   * requires login + consent.
   *
   * `opts` (all optional): `expiresDays` (link auto-expiry), `password` (mixed into the link key
   * on-device — the server never sees it), and `maxDownloads` (the server-enforced open cap; a
   * nonsensical 0 is clamped to 1 backend-side). Tauri maps the camelCase named args →
   * the snake_case Rust params.
   */
  shareNoteToLink(
    meetingId: string,
    opts?: { expiresDays?: number; password?: string; maxDownloads?: number },
  ): Promise<string> {
    return invoke<string>("share_note_to_link", {
      meetingId,
      expiresDays: opts?.expiresDays,
      password: opts?.password,
      maxDownloads: opts?.maxDownloads,
    });
  }

  /**
   * Create a zero-knowledge link share of an AUTHORED NOTE (`documents(kind='note')`),
   * returning the share URL. The distinct command name `share_note_to_link_doc` avoids
   * the meeting `share_note_to_link` collision (the note is anchored on its
   * `document_id`, not a meeting). Same E2EE contract as the meeting path: the note is
   * cleaned + sealed on-device, only ciphertext + wrapped keys leave, and the URL's
   * `#…` fragment (decryption key `L`) is assembled locally and NEVER reaches the server
   * (never log the returned URL). Refuses (`Locked`) a sealed-not-unlocked note's folder;
   * requires login + share consent. `expiresDays`/`password`/`maxDownloads` are all
   * optional (null ⇒ omit); a note's shares are the {@link listMyShares} rows whose
   * `documentId` matches this note id.
   */
  shareNoteToLinkDoc(
    id: string,
    expiresDays: number | null,
    password: string | null,
    maxDownloads: number | null,
  ): Promise<string> {
    return invoke<string>("share_note_to_link_doc", {
      id,
      expiresDays,
      password,
      maxDownloads,
    });
  }

  /** The user's shares (a sealed meeting's title is masked). */
  listMyShares(): Promise<MyShareEntry[]> {
    return invoke<MyShareEntry[]>("list_my_shares");
  }

  /** Revoke a share: DELETE the server ciphertext + flip the local state. Idempotent. */
  revokeShare(shareId: string): Promise<void> {
    return invoke<void>("revoke_share", { shareId });
  }

  // ── M5-CLIENT: Murmur↔Murmur (mode B) — invite a colleague + accept into the vault ──

  /**
   * Read-only preview of a recipient email: is it a registered Murmur account, its safety-word
   * fingerprint, and the TOFU state (first contact → show + confirm; key changed → BLOCK). Mutates
   * no pin. The FE uses this to DEFAULT to a protected link when the recipient isn't registered.
   */
  previewShareRecipient(email: string): Promise<RecipientPreview> {
    return invoke<RecipientPreview>("preview_share_recipient", { email });
  }

  /**
   * Share a note to a colleague by email (mode B). Registered → wrapped + `"sent"`; unregistered →
   * a pending invite + `"invited"`. Refuses (`Locked`) a sealed meeting; BLOCKS on a changed key.
   */
  shareNoteToUser(
    meetingId: string,
    recipientEmail: string,
    expiresDays?: number,
  ): Promise<ShareToUserResult> {
    return invoke<ShareToUserResult>("share_note_to_user", {
      meetingId,
      recipientEmail,
      expiresDays,
    });
  }

  /** Re-wrap any pending invites whose recipient has since registered. Returns the count advanced. */
  shareRewrapPending(): Promise<number> {
    return invoke<number>("share_rewrap_pending");
  }

  /** The incoming (pending-accept) share inbox (content-free; titles only appear on accept). */
  listShareInbox(): Promise<ShareInboxItem[]> {
    return invoke<ShareInboxItem[]>("list_share_inbox");
  }

  /**
   * Accept an incoming share into the vault. Verifies the sender's signature + binding, then writes a
   * new meeting + note into `folderId` (default: an auto-created unsealed "Shared" folder). Refuses a
   * sealed target (`Locked`); rejects a tampered/unsigned grant; idempotent on `shareId`.
   */
  acceptShare(shareId: string, folderId?: string): Promise<AcceptedShare> {
    return invoke<AcceptedShare>("accept_share", { shareId, folderId });
  }

  /** Decline an incoming share: drop the wrapped key server-side. Idempotent. */
  declineShare(shareId: string): Promise<void> {
    return invoke<void>("decline_share", { shareId });
  }

  // ── Shared Brain v1 — org-wide E2EE replicated brain ──
  // One typed method per spec command. Org items are deliberately-disclosed
  // content OUTSIDE the folder-lock domain; egress from THIS user is gated
  // (`meeting_is_unlocked` + consent) backend-side. Never log a returned envelope.

  /** Create a new org (this user becomes owner) → the fresh {@link OrgStatus}. */
  orgCreate(name: string): Promise<OrgStatus> {
    return invoke<OrgStatus>("org_create", { name });
  }

  /** The current org membership + sync state, or `null` when this user is in no org. */
  orgStatus(): Promise<OrgStatus | null> {
    return invoke<OrgStatus | null>("org_status");
  }

  /**
   * EVERY org this user actively belongs to (created OR invited-into) — one
   * {@link OrgStatus} per org. Supersedes the single-org {@link orgStatus} for
   * the multi-org Settings surface. Call {@link orgRefresh} first so a freshly
   * invited-into org (never created locally) is discovered before this reads the
   * local replica. Content-free per org (counts + role only, no member emails).
   */
  orgListStatuses(): Promise<OrgStatus[]> {
    return invoke<OrgStatus[]>("org_list_statuses");
  }

  /**
   * Read every already-admitted org from the local SQLCipher replica only.
   * Unlike {@link orgListStatuses}, this never refreshes an access token or
   * contacts the relay, so passive Shared Brains navigation uses this method.
   */
  orgListCachedStatuses(): Promise<OrgStatus[]> {
    return invoke<OrgStatus[]>("org_list_cached_statuses");
  }

  /**
   * PER-INSTANCE org toggle (Settings → Organization): flip whether a JOINED org
   * contributes content — browsing + brain/assistant context — on THIS Murmur
   * install. Membership-checked server-side; purely local, no egress. Disabling
   * never deletes the local replica, so re-enabling is instant.
   */
  orgSetContextEnabled(orgId: string, enabled: boolean): Promise<void> {
    return invoke<void>("org_set_context_enabled", { orgId, enabled });
  }

  /**
   * Pull the user's org MEMBERSHIP list from the server (`GET /v1/orgs`) and
   * reconcile the local `org_state` — this is what makes an org you were INVITED
   * to (and never created locally) appear. Best-effort: resolves even if the
   * server is unreachable (the last-known local state stands). Run it before
   * {@link orgListStatuses} on open.
   */
  orgRefresh(): Promise<void> {
    return invoke<void>("org_refresh");
  }

  // ── Org Tasks ───────────────────────────────────────────────────────────

  listTasks(orgId?: string): Promise<OrgTask[]> {
    return invoke<OrgTask[]>("list_tasks", { orgId });
  }

  getTask(id: string): Promise<OrgTask | null> {
    return invoke<OrgTask | null>("get_task", { id });
  }

  createTask(draft: TaskDraft): Promise<OrgTask> {
    return invoke<OrgTask>("create_task", { draft });
  }

  updateTask(id: string, draft: TaskDraft): Promise<OrgTask> {
    return invoke<OrgTask>("update_task", { id, draft });
  }

  deleteTask(id: string): Promise<void> {
    return invoke<void>("delete_task", { id });
  }

  setTaskLocalRefs(id: string, refs: TaskLocalRef[]): Promise<TaskLocalRef[]> {
    return invoke<TaskLocalRef[]>("set_task_local_refs", { id, refs });
  }

  taskListAssignees(orgId: string): Promise<TaskAssignee[]> {
    return invoke<TaskAssignee[]>("task_list_assignees", { orgId });
  }

  /**
   * Invite a member by email into a SPECIFIC org (owner only). Idempotent on an
   * already-invited address. `orgId` targets the org in a multi-org world.
   */
  orgInviteMember(orgId: string, email: string): Promise<void> {
    return invoke<void>("org_invite_member", { orgId, email });
  }

  /** List one org's members (owner sees emails to manage; drives the member list). */
  orgListMembers(orgId: string): Promise<OrgMember[]> {
    return invoke<OrgMember[]>("org_list_members", { orgId });
  }

  /** Remove a member from a SPECIFIC org (owner only) — drives the OCK generation rotation. */
  orgRemoveMember(orgId: string, userId: string): Promise<void> {
    return invoke<void>("org_remove_member", { orgId, userId });
  }

  /** Leave a SPECIFIC org (self-removal). The local org replica for that org is dropped. */
  orgLeave(orgId: string): Promise<void> {
    return invoke<void>("org_leave", { orgId });
  }

  /** Grant the one-time org-egress consent (mirrors `consentToShareEgress`). PRESERVE-ONLY config key. */
  consentToOrgEgress(): Promise<void> {
    return invoke<void>("consent_to_org_egress");
  }

  /** Revoke the org-egress consent (the next org share is refused fail-closed). */
  revokeOrgEgress(): Promise<void> {
    return invoke<void>("revoke_org_egress");
  }

  /**
   * Preview the EXACT outgoing org-share envelope for a meeting OR a note, at the
   * given `scrub` setting — the OPAQUE preview sheet renders this verbatim before
   * the user confirms. Pass exactly one of `meetingId` / `documentId`. Re-fetch
   * whenever the scrub toggle flips (the markdown + counts change). Refuses
   * (`Locked`) a sealed source. Mirrors the spec `preview_org_share`.
   */
  previewOrgShare(args: {
    meetingId?: string;
    documentId?: string;
    scrub: boolean;
  }): Promise<OrgSharePreview> {
    return invoke<OrgSharePreview>("preview_org_share", {
      meetingId: args.meetingId ?? null,
      documentId: args.documentId ?? null,
      scrub: args.scrub,
    });
  }

  /**
   * List a SPECIFIC org's browsable shared-brain items (`list_org_items`) — the
   * synced+decrypted item headers (title + author hint + date), newest first.
   * This is what makes org content BROWSABLE (previously it was search-only, so a
   * member had nowhere to see a colleague's share). Content-lean per the org
   * "no content-derived strings" discipline; the full body is fetched by
   * {@link orgGetItem} in the viewer. `orgId` targets the org in a multi-org world.
   */
  listOrgItems(orgId: string): Promise<OrgItemHeader[]> {
    return invoke<OrgItemHeader[]>("list_org_items", { orgId });
  }

  /** Import a received org replica into an open local user container. */
  addOrgItemToContainer(
    itemId: string,
    containerId: string,
  ): Promise<OrgItemImportResult> {
    return invoke<OrgItemImportResult>("add_org_item_to_container", {
      itemId,
      containerId,
    });
  }

  /**
   * Publish a MEETING to the org brain (seal under the OCK + upload). Gate order
   * backend-side: unlocked → consent → clean → scrub → seal → upload → ledger.
   * Refuses (`Locked`) a sealed meeting; requires org membership + consent.
   */
  shareMeetingToOrg(
    meetingId: string,
    orgId: string,
    scrub: boolean,
    access: OrgAccess,
  ): Promise<void> {
    return invoke<void>("share_meeting_to_org", {
      meetingId,
      orgId,
      scrub,
      access,
    });
  }

  /**
   * Publish an authored NOTE to the org brain. Same gated pipeline as the meeting
   * path. `orgId` targets the CHOSEN org (previously the backend shared to the
   * FIRST org via `.next()`, which is why a share landed in the wrong org).
   */
  shareDocumentToOrg(
    documentId: string,
    orgId: string,
    scrub: boolean,
    access: OrgAccess,
  ): Promise<void> {
    return invoke<void>("share_document_to_org", {
      documentId,
      orgId,
      scrub,
      access,
    });
  }

  /**
   * This user's outgoing shares INTO ONE org (drives the "In Org Brain" state + per-item revoke).
   *
   * `orgId` is REQUIRED (2026-07-26): the backend used to ignore the caller entirely and answer with
   * the FIRST locally-joined org's shares, so in a multi-org account this read silently described the
   * wrong org. An unjoined/unknown id resolves to `[]`, never another org's rows.
   */
  listOrgShares(orgId: string): Promise<OrgShareEntry[]> {
    return invoke<OrgShareEntry[]>("list_org_shares", { orgId });
  }

  /**
   * Every org THIS meeting is actively shared into (org id + display name) — drives the
   * "Shared with [org]" badge on the Library row + the Detail view. Gated exactly like
   * `getMeetingDetail`: a sealed-and-not-session-unlocked meeting returns `[]`, never leaking
   * its share status. Only the text note/summary is ever shared — the caller renders this
   * alongside an explicit "audio never leaves the device" caption, never implying otherwise.
   */
  meetingOrgShares(meetingId: string): Promise<MeetingOrgShareInfo[]> {
    return invoke<MeetingOrgShareInfo[]>("meeting_org_shares", { meetingId });
  }

  /**
   * The BULK Library-row variant: every active meeting→org share pairing across ALL of the
   * caller's meetings in one call — avoids fetching {@link meetingOrgShares} per row. Same gate
   * (a sealed-and-not-session-unlocked meeting contributes no rows).
   */
  listMeetingOrgShares(): Promise<MeetingOrgShareRow[]> {
    return invoke<MeetingOrgShareRow[]>("list_meeting_org_shares");
  }

  /**
   * Which orgs already hold a LIVE (`uploaded`) share of THIS local source
   * (meeting XOR note) — so the share sheet can mark those orgs "Already added ✓"
   * and BLOCK a re-share (the double-click duplicate fix). Read-only, no egress.
   */
  orgLiveSharesForSource(args: {
    meetingId?: string;
    documentId?: string;
  }): Promise<OrgSourceShareStatus[]> {
    return invoke<OrgSourceShareStatus[]>("org_live_shares_for_source", {
      meetingId: args.meetingId ?? null,
      documentId: args.documentId ?? null,
    });
  }

  /** Revoke an org share: tombstone the feed item + drop the local ciphertext. Idempotent. */
  revokeOrgShare(itemId: string): Promise<void> {
    return invoke<void>("revoke_org_share", { itemId });
  }

  /** Manually pull + ingest a SPECIFIC org's feed now → the {@link OrgSyncReport} (counts + errors only). */
  orgSyncNow(orgId: string): Promise<OrgSyncReport> {
    return invoke<OrgSyncReport>("org_sync_now", { orgId });
  }

  /**
   * The full decrypted org item for the read-only viewer route.
   *
   * `null` when the item is unknown, TOMBSTONED (withdrawn from the org), or its org is disabled on
   * this instance — the backend has always returned `Option<OrgItemDetail>`; the signature used to
   * claim otherwise, which is how a withdrawn item could go on being rendered. Callers MUST treat
   * `null` as "no longer available", not as an error.
   */
  orgGetItem(itemId: string): Promise<OrgItemDetail | null> {
    return invoke<OrgItemDetail | null>("org_get_item", { itemId });
  }

  /**
   * Edit-in-place + re-publish an org item the caller AUTHORED (works from any of
   * their machines, unlike the redirect-to-local-source path which only works on
   * the machine that first shared it). Goes through the same consent + seal +
   * verify-before-egress gates as sharing; supersedes the old item (rev+1) and
   * returns the NEW server item id so the caller can navigate to it. Mirrors
   * `org_update_item`.
   */
  orgUpdateItem(
    itemId: string,
    title: string,
    markdown: string,
  ): Promise<string> {
    return invoke<string>("org_update_item", { itemId, title, markdown });
  }

  /** Change a Shared Brain document's member access. Server authorizes managers. */
  orgSetItemAccess(itemId: string, access: OrgAccess): Promise<void> {
    return invoke<void>("org_set_item_access", { itemId, access });
  }

  /**
   * Remove the caller's OWN org item from the shared org space, from a device
   * that never itself shared it (e.g. the author's other Mac). Deliberately
   * "leave/remove from org", NOT "destroy the original" — the origin device's
   * local note/meeting source is untouched; only the shared org-space copy
   * (this device's replica now, the origin device's replica on its own next
   * sync) is tombstoned. Refuses (Auth) for anyone but the stored author.
   * Mirrors `delete_org_item_as_author`.
   */
  deleteOrgItemAsAuthor(itemId: string): Promise<void> {
    return invoke<void>("delete_org_item_as_author", { itemId });
  }

  /**
   * Resolve an org item back to THIS device's local editable source (the note or
   * meeting it was shared FROM), or `null` when the caller is NOT the author (no
   * local source). Lets the `/org-item/:id` viewer send an author straight to
   * their editable original (`/notes/:id` or `/meeting/:id`, whose edits
   * re-publish) while a non-author gets the read-only replica view. Mirrors
   * `org_resolve_source`.
   */
  orgResolveSource(itemId: string): Promise<OrgSourceRef | null> {
    return invoke<OrgSourceRef | null>("org_resolve_source", { itemId });
  }

  /**
   * The active shares (link + user + org) for a folder — gathered before a lock so
   * the lock×shares dialog can warn + offer Revoke & lock. Mirrors `folder_active_shares`.
   */
  folderActiveShares(folderId: string): Promise<ActiveSharesReport> {
    return invoke<ActiveSharesReport>("folder_active_shares", { folderId });
  }

  /** Revoke EVERY active share (link + user + org) from a folder — used by "Revoke & lock". */
  revokeSharesForFolder(folderId: string): Promise<void> {
    return invoke<void>("revoke_shares_for_folder", { folderId });
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
   * Stage 4 — the model catalog for ONE connection: `claude_code`/`anthropic` →
   * the backend's Claude-id constant (single source of truth — no more
   * hardcoded ids in FE templates), `ollama` → the live `/api/tags` list,
   * `gateway` → its `/v1/models`, `local` → the GGUF registry ids. Rejects when
   * the connection's endpoint is unreachable — the FE falls back to a free-text
   * model input (the {@link listGatewayModels} pattern).
   */
  listModels(connection: string): Promise<ModelCatalog> {
    return invoke<ModelCatalog>("list_models", { connection });
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
   * Phase 6 — content-free egress ledger summary for the given rolling window.
   * Returns aggregate call + token counts, per-model and per-day breakdowns,
   * total PII redaction counts, and the most-recent call rows — all metadata,
   * NO transcript text. The command never errors (an empty ledger returns zero
   * aggregates and empty arrays).
   */
  getEgressLedger(days: number): Promise<EgressLedger> {
    return invoke<EgressLedger>("get_egress_ledger", { days });
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

  /**
   * brain2 connectors (Phase 2) — grant the one-time consent for JIRA egress. The ONLY
   * supported way to flip `jiraConsented` true: the backend persists the flag AND updates
   * its in-memory config cache, so the next brain/Ask answer may expose the Jira connector
   * (provided Jira is enabled AND configured AND a token is stored). Idempotent. Mirrors
   * {@link consentToWebSearch}.
   */
  consentToJira(): Promise<void> {
    return invoke<void>("consent_to_jira");
  }

  /**
   * brain2 connectors — store/replace the BYO Jira API token in the Keychain. An empty
   * string clears it. NEVER logged / NEVER returned to the FE — only {@link hasJiraToken}
   * reports presence. Mirrors {@link setWebSearchApiKey}.
   */
  setJiraToken(key: string): Promise<void> {
    return invoke<void>("set_jira_token", { key });
  }

  /** Whether a Jira API token is currently stored. Never the value. */
  hasJiraToken(): Promise<boolean> {
    return invoke<boolean>("has_jira_token");
  }

  /**
   * brain2 connectors (Phase 3) — grant the one-time consent for SLACK egress. The ONLY
   * supported way to flip `slackConsented` true: the backend persists the flag AND updates
   * its in-memory config cache, so the next brain/Ask answer may expose the Slack connector
   * (provided Slack is enabled AND a user token is stored). Idempotent. Mirrors
   * {@link consentToJira}.
   */
  consentToSlack(): Promise<void> {
    return invoke<void>("consent_to_slack");
  }

  /**
   * brain2 connectors — store/replace the BYO Slack user token in the Keychain. An empty
   * string clears it. NEVER logged / NEVER returned to the FE — only {@link hasSlackToken}
   * reports presence. Mirrors {@link setJiraToken}.
   */
  setSlackToken(key: string): Promise<void> {
    return invoke<void>("set_slack_token", { key });
  }

  /** Whether a Slack user token is currently stored. Never the value. */
  hasSlackToken(): Promise<boolean> {
    return invoke<boolean>("has_slack_token");
  }

  /**
   * brain2 connectors — grant the one-time consent for NOTION egress. The ONLY supported way to
   * flip `notionConsented` true: the backend persists the flag AND updates its in-memory config
   * cache, so the next brain/Ask answer may expose the Notion connector (provided Notion is enabled
   * AND an integration token is stored). Idempotent. Mirrors {@link consentToSlack}.
   */
  consentToNotion(): Promise<void> {
    return invoke<void>("consent_to_notion");
  }

  /**
   * brain2 connectors — store/replace the BYO Notion integration token in the Keychain. An empty
   * string clears it. NEVER logged / NEVER returned to the FE — only {@link hasNotionToken} reports
   * presence. Mirrors {@link setSlackToken}.
   */
  setNotionToken(key: string): Promise<void> {
    return invoke<void>("set_notion_token", { key });
  }

  /** Whether a Notion integration token is currently stored. Never the value. */
  hasNotionToken(): Promise<boolean> {
    return invoke<boolean>("has_notion_token");
  }

  /**
   * brain2 connectors — grant the one-time consent for CLICKUP egress. The ONLY supported way to
   * flip `clickupConsented` true: the backend persists the flag AND updates its in-memory config
   * cache, so the next brain/Ask answer may expose the ClickUp connector (provided ClickUp is
   * enabled AND a workspace id + token are configured). Idempotent. Mirrors {@link consentToNotion}.
   */
  consentToClickup(): Promise<void> {
    return invoke<void>("consent_to_clickup");
  }

  /**
   * brain2 connectors — store/replace the BYO ClickUp personal API token in the Keychain. An empty
   * string clears it. NEVER logged / NEVER returned to the FE — only {@link hasClickupToken}
   * reports presence. Mirrors {@link setNotionToken}.
   */
  setClickupToken(key: string): Promise<void> {
    return invoke<void>("set_clickup_token", { key });
  }

  /** Whether a ClickUp API token is currently stored. Never the value. */
  hasClickupToken(): Promise<boolean> {
    return invoke<boolean>("has_clickup_token");
  }

  providerStatuses(): Promise<ProviderStatus[]> {
    return invoke<ProviderStatus[]>("provider_statuses");
  }

  resummarize(meetingId: string): Promise<StopResult> {
    return invoke<StopResult>("resummarize", { meetingId });
  }

  /**
   * Generate or refresh this meeting's canonical companion note. Omitting the
   * template id asks the backend to resolve the user's Settings default.
   */
  async convertMeetingToNote(
    meetingId: string,
    templateId?: string,
  ): Promise<CompanionAppendResult> {
    const result = await invoke<CompanionAppendResult>(
      "convert_meeting_to_note",
      {
        meetingId,
        ...(templateId ? { templateId } : {}),
      },
    );
    this._workspaceMutationRevision.update((revision) => revision + 1);
    return result;
  }

  /**
   * Re-run the FULL pipeline (ASR + summarize + export) from a failed recording's on-disk archive
   * audio — the recovery for a pipeline that died mid-transcription (re-summarize alone cannot
   * re-run ASR). Backend gates: meeting must be in the Error state, its folder unlocked, its
   * archive audio present on disk, and no recording in progress.
   */
  retryTranscription(meetingId: string): Promise<StopResult> {
    return invoke<StopResult>("retry_transcription", { meetingId });
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

  // --- Saved views over a list surface (Feature B; Notes added 2026-07-14) ---

  /** All saved views for a scope, in `sortOrder`. `scope` is `"meetings"` or `"notes"`. */
  listSavedViews(scope: "meetings" | "notes"): Promise<SavedView[]> {
    return invoke<SavedView[]>("list_saved_views", { scope });
  }

  /** Create or update a saved view (id present ⇒ update). Returns the persisted row. */
  upsertSavedView(view: SavedView): Promise<SavedView> {
    return invoke<SavedView>("upsert_saved_view", { view });
  }

  /** Permanently delete a saved view. */
  deleteSavedView(id: string): Promise<void> {
    return invoke<void>("delete_saved_view", { id });
  }

  /** Persist a new left-to-right ordering of a scope's saved views. */
  reorderSavedViews(
    scope: "meetings" | "notes",
    orderedIds: string[],
  ): Promise<void> {
    return invoke<void>("reorder_saved_views", { scope, orderedIds });
  }

  /**
   * Per-meeting open/done action-item counts, for the Table/Board views. Gated
   * server-side exactly like every meeting read — a sealed-and-not-session-
   * unlocked meeting contributes no summary (so a locked row shows no counts).
   */
  listMeetingActionSummaries(): Promise<MeetingActionSummary[]> {
    return invoke<MeetingActionSummary[]>("list_meeting_action_summaries");
  }

  /** Rename a meeting's title. */
  renameMeeting(meetingId: string, title: string): Promise<void> {
    return invoke<void>("rename_meeting", { meetingId, title });
  }

  /**
   * Ask a grounded question about a meeting transcript and selected sources.
   *
   * `explicitSources` (source-scoped Brain) OPTIONALLY pins the answer to exactly
   * the passed sources + their links (each `{kind, id}`; an extra `title` is
   * ignored backend-side). Omitting it / `[]` / `null` keeps the transcript-only
   * behavior; a NON-empty array adds those gated sources to the grounding. Only
   * sent when non-empty so existing callers are unaffected.
   */
  chatMeeting(
    meetingId: string,
    question: string,
    history: ChatTurn[],
    explicitSources?: SourceRef[],
  ): Promise<string> {
    return invoke<string>("chat_meeting", {
      meetingId,
      question,
      history,
      ...(explicitSources?.length ? { explicitSources } : {}),
    });
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

  /** User-authored note templates (Granola-style named sections). */
  listNoteTemplates(): Promise<NoteTemplate[]> {
    return invoke<NoteTemplate[]>("list_note_templates");
  }

  /**
   * Save (create or replace) a note template. Pass `id: null` to create; the backend rejects
   * scripting tokens (`<%`, `tp.`, `require(`, `process.`) and returns the stored row.
   */
  saveNoteTemplate(
    id: string | null,
    name: string,
    tone: string,
    sections: NoteTemplateSection[],
    extraFrontmatterKeys: string[],
  ): Promise<NoteTemplate> {
    return invoke<NoteTemplate>("save_note_template", {
      id,
      name,
      tone,
      sections,
      extraFrontmatterKeys,
    });
  }

  /** Delete a saved note template by id. */
  deleteNoteTemplate(id: string): Promise<void> {
    return invoke<void>("delete_note_template", { id });
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

  // ── Dashboards ────────────────────────────────────────────────────────────
  //
  // Boards of tiles over EXISTING sources. `getDashboard` returns every tile
  // already resolved through the backend's gated readers — a sealed source
  // arrives as `{ kind: "locked" }` with no payload, so the FE cannot leak what
  // it was never given. Board-scoped Ask sends only `dashboardId`; the backend
  // resolves the live composite under the same privacy/egress seams.

  /** Every board with its layout metadata (no gated payload read). */
  listDashboards(): Promise<DashboardSummary[]> {
    return invoke<DashboardSummary[]>("list_dashboards");
  }

  createDashboard(
    title: string,
    emoji?: string,
    tint?: DashboardTint,
  ): Promise<Dashboard> {
    return invoke<Dashboard>("create_dashboard", { title, emoji, tint });
  }

  updateDashboard(
    id: string,
    patch: {
      title?: string;
      emoji?: string;
      tint?: DashboardTint;
      pinned?: boolean;
    },
  ): Promise<Dashboard> {
    return invoke<Dashboard>("update_dashboard", { id, ...patch });
  }

  deleteDashboard(id: string): Promise<boolean> {
    return invoke<boolean>("delete_dashboard", { id });
  }

  reorderDashboards(ids: string[]): Promise<void> {
    return invoke<void>("reorder_dashboards", { ids });
  }

  /** One board with every tile resolved (and gated). `null` when unknown. */
  getDashboard(id: string): Promise<DashboardDetail | null> {
    return invoke<DashboardDetail | null>("get_dashboard", { id });
  }

  /**
   * The board's VISIBLE sources, to hand straight to {@link askVault} as
   * `explicitSources`. Sealed sources are absent, so a board-scoped Ask can
   * never retrieve from a locked folder.
   */
  getDashboardSources(id: string): Promise<SourceRef[]> {
    return invoke<SourceRef[]>("get_dashboard_sources", { id });
  }

  /**
   * Returns nothing on purpose: the backend refuses to hand back the raw stored
   * row, because that would ship `title`/`config` unredacted. Reload the board.
   */
  addDashboardTile(
    dashboardId: string,
    kind: TileKind,
    opts?: {
      refId?: string;
      title?: string;
      span?: number;
      config?: string;
    },
  ): Promise<void> {
    return invoke<void>("add_dashboard_tile", {
      dashboardId,
      kind,
      ...opts,
    });
  }

  updateDashboardTile(
    id: string,
    patch: { title?: string; span?: number; config?: string },
  ): Promise<void> {
    return invoke<void>("update_dashboard_tile", { id, ...patch });
  }

  deleteDashboardTile(id: string): Promise<boolean> {
    return invoke<boolean>("delete_dashboard_tile", { id });
  }

  /**
   * Rebuild one Living Answer from the dashboard's current backend-gated scope.
   * The caller supplies identity + question only; answer/provenance stay backend-owned.
   */
  refreshDashboardAnswer(
    dashboardId: string,
    tileId: string,
    question: string,
  ): Promise<LivingAnswerTileData> {
    return invoke<LivingAnswerTileData>("refresh_dashboard_answer", {
      dashboardId,
      tileId,
      question,
    });
  }

  reorderDashboardTiles(dashboardId: string, tileIds: string[]): Promise<void> {
    return invoke<void>("reorder_dashboard_tiles", { dashboardId, tileIds });
  }

  /** Content-free startup/sidebar count; does not read reminder titles or details. */
  getReminderSummary(): Promise<ReminderSummary> {
    return invoke<ReminderSummary>("get_reminder_summary");
  }

  /** Canonical Inbox / Upcoming / Completed snapshot. */
  listReminders(): Promise<RemindersSnapshot> {
    return invoke<RemindersSnapshot>("list_reminders");
  }

  createMurmurReminder(draft: ReminderDraft): Promise<ReminderView> {
    return invoke<ReminderView>("create_reminder", { draft });
  }

  updateMurmurReminder(
    reminderId: string,
    draft: ReminderDraft,
  ): Promise<ReminderView> {
    return invoke<ReminderView>("update_reminder", { reminderId, draft });
  }

  deleteMurmurReminder(reminderId: string): Promise<void> {
    return invoke<void>("delete_reminder", { reminderId });
  }

  completeMurmurReminder(
    reminderId: string,
    expectedDueAt: number,
  ): Promise<void> {
    return invoke<void>("complete_reminder", { reminderId, expectedDueAt });
  }

  dismissMurmurReminderOccurrence(occurrenceId: string): Promise<void> {
    return invoke<void>("dismiss_reminder_occurrence", { occurrenceId });
  }

  auditReminderSuggestions(
    source: ReminderSourceAnchor,
  ): Promise<ReminderSuggestionView[]> {
    return invoke<ReminderSuggestionView[]>("audit_reminder_suggestions", {
      sourceKind: source.kind,
      sourceId: source.id,
    });
  }

  acceptReminderSuggestion(
    suggestionId: string,
    draft: ReminderDraft,
  ): Promise<ReminderView> {
    return invoke<ReminderView>("accept_reminder_suggestion", {
      suggestionId,
      draft,
    });
  }

  dismissReminderSuggestion(suggestionId: string): Promise<void> {
    return invoke<void>("dismiss_reminder_suggestion", { suggestionId });
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

  /**
   * The FULL-BRAIN graph (Brain v3 PR-4): TYPED nodes (entities + VISIBLE
   * meetings + notes + documents) and TYPED edges (co-occurrence + entity→meeting
   * mentions + `links` rows). Snapshots the live session `unlocked` set exactly
   * like {@link getGraph}, so re-fetch on a FoldersService lock-state change to
   * drop / re-admit sealed nodes live. `opts.includeSuggested` (default `false`)
   * admits un-accepted (`status: "suggested"`) semantic links — the ONLY option
   * the FE must re-fetch on (every other lens filters the returned graph
   * client-side). All-default, so a bare call returns the confirmed graph.
   */
  getFullGraph(opts?: FullGraphOpts): Promise<FullGraphData> {
    return invoke<FullGraphData>("get_full_graph", { opts: opts ?? null });
  }

  /** Detail for one entity: the entity + its visible backlinked meetings + top neighbors. */
  getEntityDetail(entityId: string): Promise<EntityDetail> {
    return invoke<EntityDetail>("get_entity_detail", { entityId });
  }

  /**
   * Brain v3 PR-6 (Knowledge Diff) — the between-two-instants set diff PLUS the full
   * chronological decision ledger for one entity. GATED server-side through the
   * visible-facts reader: a sealed-and-not-session-unlocked meeting's fact enters no
   * snapshot, diff entry, or ledger row (and the entity itself won't resolve if all
   * its mentions are sealed). `from`/`to` are ISO-8601 instants (the range the FE is
   * comparing). The backend command parameter is `entity` (a name OR an entity id it
   * resolves), NOT `entityId` — pass the resolved detail id through as `entity`.
   */
  getEntityKnowledgeDiff(
    entityId: string,
    from: string,
    to: string,
  ): Promise<EntityKnowledgeDiff> {
    return invoke<EntityKnowledgeDiff>("get_entity_knowledge_diff", {
      entity: entityId,
      from,
      to,
    });
  }

  /**
   * Note↔note backlinks ("Linked mentions") — the VISIBLE inbound sources
   * (meetings + authored notes) that mention/link the given target. GATED
   * server-side like every content read: a sealed-and-not-session-unlocked
   * source never appears, so the caller MUST skip the fetch entirely while the
   * target itself is locked/masked (never surface backlinks behind a lock).
   * Returns `[]` when nothing links to the target.
   */
  getBacklinks(
    targetKind: SourceKind,
    targetId: string,
  ): Promise<BacklinkSource[]> {
    return invoke<BacklinkSource[]>("get_backlinks", { targetKind, targetId });
  }

  /**
   * Brain v3 PR-3 — every persisted link edge incident on `(kind, id)` for the
   * "Connections" panel: deterministic `wikilink`/`companion` edges + `semantic`
   * suggestions (with a cosine `score`). BOTH endpoints are visibility-gated
   * server-side — a sealed queried item yields `[]` (no existence leak) and a
   * sealed neighbour is dropped — so the caller MUST STILL skip the fetch while
   * the item itself is locked/masked (never surface connections behind a lock).
   * `dismissed` edges are never returned.
   */
  listLinks(kind: LinkKind, id: string): Promise<LinkEdge[]> {
    return invoke<LinkEdge[]>("list_links", { kind, id });
  }

  /**
   * Brain v3 PR-3 — ACCEPT a suggested (semantic) link: flip it active and (when
   * either endpoint is a locally-owned, session-visible note) materialize the
   * neighbour's `[[Title]]` into that note's managed link block. GATED + idempotent
   * server-side. The caller re-runs {@link listLinks} afterward to reflect the flip.
   */
  acceptLink(id: number): Promise<void> {
    return invoke<void>("accept_link", { id });
  }

  /**
   * Brain v3 PR-3 — DISMISS a suggested link: tombstone it so no later auto pass
   * re-suggests it (graph-only, no markdown touched). Idempotent. The caller
   * re-runs {@link listLinks} afterward to drop the row.
   */
  dismissLink(id: number): Promise<void> {
    return invoke<void>("dismiss_link", { id });
  }

  /**
   * PR-1 — CREATE a user-initiated link from `(srcKind, srcId)` → `(dstKind, dstId)`
   * (the anchored item is the src). Directed: linking FROM a note materializes the
   * neighbour's `[[Title]]` into the note body server-side; FROM a meeting it creates
   * a pure relation row. GATED — refuses (`Locked`) when either endpoint is sealed.
   * The caller re-runs {@link listLinks} afterward so the new chip appears.
   */
  linkItems(
    srcKind: LinkKind,
    srcId: string,
    dstKind: LinkKind,
    dstId: string,
  ): Promise<void> {
    return invoke<void>("link_items", { srcKind, srcId, dstKind, dstId });
  }

  /**
   * PR-1 — REMOVE a collapsed chip's exact manual rows (`manualEdges`, one or both directions).
   * The legacy pair remains the display identity and fallback for older payloads. Only manual links
   * (`LinkEdge.manual === true`) are removable. GATED — refuses (`Locked`) when either endpoint is
   * sealed. The caller re-runs {@link listLinks} afterward so a surviving derived edge becomes
   * non-removable, or the chip drops out when no derived relation remains.
   */
  unlinkItems(
    srcKind: LinkKind,
    srcId: string,
    dstKind: LinkKind,
    dstId: string,
    manualEdges?: readonly ManualLinkEdge[],
  ): Promise<void> {
    const pair = { srcKind, srcId, dstKind, dstId };
    return manualEdges?.length
      ? invoke<void>("unlink_items", { ...pair, manualEdges })
      : invoke<void>("unlink_items", pair);
  }

  /**
   * Resolve a clicked `[[Title]]` wikilink to the VISIBLE note/meeting to navigate to,
   * or `null` when nothing matches / the only match is sealed-and-not-session-unlocked
   * (gated server-side). The caller routes on `kind` or offers to create a note.
   */
  resolveWikilink(title: string): Promise<WikiTarget | null> {
    return invoke<WikiTarget | null>("resolve_wikilink", { title });
  }

  /**
   * Live keystroke-prefix candidates for the inline `[[` / slash-menu link-insertion
   * autocomplete — a lightweight, GATED title-prefix scan over VISIBLE notes + meetings +
   * (when joined) the Shared Brain. Distinct from {@link resolveWikilink}
   * (exact-title resolve on Enter/click) and from `noteAssistantAction`'s `find_related`
   * (SELECTION+semantic retrieval — the wrong shape for filtering on a short, growing
   * prefix). Reuses the {@link NoteCitation} shape so the popover renders it exactly like a
   * `find_related` row. An empty/blank `prefix` returns the most-recently-updated visible
   * candidates (so the popover has something to show the instant it opens).
   *
   * PAGINATED: returns the `limit`-sized page starting at `offset` of one stable combined
   * ordering (notes, then meetings, then org hits) — `LinkPickerComponent`'s infinite
   * scroll walks it page by page. The backend clamps `limit` to a sane ceiling (100).
   */
  listLinkCandidates(
    prefix: string,
    offset: number,
    limit: number,
  ): Promise<NoteCitation[]> {
    return invoke<NoteCitation[]>("list_link_candidates", {
      prefix,
      offset,
      limit,
    });
  }

  /**
   * The `/people` personal-CRM list: one {@link PersonCard} per VISIBLE Person
   * entity (name + last-talked + open-commitment / fact counts), rolled up over
   * the SAME gated graph/facts/commitment readers as {@link getGraph}. GATED
   * server-side — a person whose mentions/facts/commitments live solely in
   * sealed-not-unlocked meetings never appears, and every count reflects visible
   * sources only — so re-fetch on a FoldersService lock-state change to shift the
   * list live (mirrors {@link getGraph}). Each `id` links to the entity detail.
   * Returns {@link PeopleList}, not a bare array — `people` may itself be capped by
   * the backend's 500-row limit, and `totalVisiblePeople` is the true count.
   */
  listPeople(): Promise<PeopleList> {
    return invoke<PeopleList>("list_people");
  }

  /**
   * The STRUCTURED, egress-free person dossier for the `/people` detail pane: the
   * entity + its mentioning-meeting TIMELINE + open COMMITMENTS (who-owes-what) +
   * bitemporal FACTS (open + recently-closed) + co-occurring NEIGHBOURS, assembled
   * DETERMINISTICALLY from the gated DB with NO cloud call (unlike the cloud-
   * synthesizing `entity_dossier`). GATED server-side exactly like
   * {@link getEntityDetail}: a sealed-and-not-session-unlocked meeting contributes
   * nothing, and an unknown / sealed-only id REJECTS (InvalidArg) — so re-fetch on
   * the entity id change (and, upstream, on a FoldersService lock-state change) to
   * shift content live. The note-body `corpus` is serde-skipped and never crosses IPC.
   */
  getPersonDossier(entityId: string): Promise<DossierData> {
    return invoke<DossierData>("get_person_dossier", { entityId });
  }

  // ── brain2 — the Brain page "what's in my brain" overview ──────────────

  /**
   * Headline counts + semantic flags for the Brain page's status header:
   * VISIBLE meetings / documents / typed notes, the indexed-chunk count, and
   * whether semantic search is enabled + the e5 embedding model is present.
   * Counts ONLY visible/unlocked content (sealed-not-unlocked items are never
   * counted) and carries NO text, so re-fetch on a FoldersService lock-state
   * change to shift the counts live (mirrors {@link getGraph}).
   */
  brainOverview(): Promise<BrainOverview> {
    return invoke<BrainOverview>("brain_overview");
  }

  // ── user memory — "what the brain knows about you" ─────────────────────

  /**
   * The current user-memory facts (subject/predicate/object + provenance) plus
   * the synthesized brief that is injected into grounding. GATED server-side:
   * only facts whose SOURCE meeting is visible under the live unlocked snapshot
   * are returned — a sealed-not-unlocked meeting's user memory surfaces NOTHING,
   * so re-fetch on a FoldersService lock-state change to shift the list live
   * (mirrors {@link brainOverview}).
   */
  getUserMemory(): Promise<UserMemory> {
    return invoke<UserMemory>("get_user_memory");
  }

  /**
   * Forget ONE user-memory fact by id (a bitemporal INVALIDATE — the row is
   * CLOSED, never silently deleted, so history is preserved). After this the
   * fact drops out of {@link getUserMemory} and the regenerated brief.
   * Idempotent (an already-closed / unknown id is a no-op).
   */
  forgetUserFact(id: string): Promise<void> {
    return invoke<void>("forget_user_fact", { id });
  }

  /**
   * Clear ALL user memory: bitemporally close every currently-open user fact
   * (invalidate, never delete — closed history stays). After this
   * {@link getUserMemory} and the brief are empty.
   */
  clearUserMemory(): Promise<void> {
    return invoke<void>("clear_user_memory");
  }

  /**
   * Import pasted memories from another AI assistant (a ChatGPT/Claude "what I
   * remember about you" export) into the user-memory store. Extraction runs
   * STRICTLY on the on-device light brain model (local-or-stub, NEVER cloud —
   * the pasted export never leaves the device); the candidates are reconciled
   * (deduped) against the existing memory and anchored to one synthetic
   * "Memory Import" meeting — deleting that meeting undoes the import.
   * Resolves with the number of NEW facts added (0 on a duplicate re-import or
   * when no on-device brain model is installed). Rejects with
   * `AppError::InvalidArg` on empty text or when memory is disabled.
   */
  importMemories(text: string): Promise<number> {
    return invoke<number>("import_memories", { text });
  }

  // ── Brain v2 L5 — scheduled briefs (schedule CRUD + propose-accept runs) ─

  /** All brief schedules (config rows — labels, timing, hints). */
  listBriefSchedules(): Promise<BriefSchedule[]> {
    return invoke<BriefSchedule[]>("list_brief_schedules");
  }

  /**
   * Create one brief schedule. `dayOfWeek`: 0 = Monday … 6 = Sunday, null =
   * daily. The backend runner fires it at most once per local day, at the first
   * 60s tick at/after `hour:minute` local. Rejects `InvalidArg` on out-of-range
   * timing / an empty label.
   */
  createBriefSchedule(input: {
    label: string;
    dayOfWeek: number | null;
    hourLocal: number;
    minuteLocal: number;
    scopeDays?: number;
    promptHint?: string;
  }): Promise<BriefSchedule> {
    return invoke<BriefSchedule>("create_brief_schedule", {
      label: input.label,
      dayOfWeek: input.dayOfWeek,
      hourLocal: input.hourLocal,
      minuteLocal: input.minuteLocal,
      scopeDays: input.scopeDays ?? null,
      promptHint: input.promptHint ?? null,
    });
  }

  /** Update a schedule's editable fields (label / timing / window / hint / enabled). */
  updateBriefSchedule(schedule: BriefSchedule): Promise<void> {
    return invoke<void>("update_brief_schedule", { schedule });
  }

  /** Delete a schedule AND its staged (pending) runs. */
  deleteBriefSchedule(scheduleId: string): Promise<void> {
    return invoke<void>("delete_brief_schedule", { scheduleId });
  }

  /**
   * The PENDING proposed brief runs (the cards). `noteMd` was synthesized from
   * visible-only content backend-side (the runner reads with the empty unlock
   * set — sealed content can never be in a brief by construction).
   */
  listBriefRuns(): Promise<BriefRun[]> {
    return invoke<BriefRun[]>("list_brief_runs");
  }

  /**
   * Accept a proposed brief: exports its markdown to `<vault>/Briefs/` and
   * CONSUMES the staged copy. Resolves with the exported path. Rejects
   * `InvalidArg` when no vault is configured or the run was already handled.
   */
  acceptBrief(runId: string): Promise<string> {
    return invoke<string>("accept_brief", { runId });
  }

  /** Dismiss a proposed brief — the staged row (markdown included) is deleted. */
  dismissBrief(runId: string): Promise<void> {
    return invoke<void>("dismiss_brief", { runId });
  }

  /**
   * A brief was STAGED by the background runner (propose-accept). Payload is
   * run id + label + size only — refresh the pending list via
   * {@link listBriefRuns} to render the card.
   */
  onBriefProposed(cb: (p: BriefProposedPayload) => void): Promise<UnlistenFn> {
    return listen<BriefProposedPayload>(EVENT_BRIEF_PROPOSED, (e) =>
      cb(e.payload),
    );
  }

  // ── Vault Audit — deterministic hygiene passes (propose-accept inbox) ────

  /**
   * Run a full audit pass NOW (broken links / orphans / stale / contradictions
   * / unlinked mentions) and resolve with the run's summary. Findings are only
   * STAGED for review — nothing in the vault changes until one is accepted.
   */
  runVaultAudit(): Promise<AuditRunSummary> {
    return invoke<AuditRunSummary>("run_vault_audit");
  }

  /** Audit findings by status — defaults to the PENDING inbox rows. */
  listAuditFindings(status = "pending"): Promise<AuditFinding[]> {
    return invoke<AuditFinding[]>("list_audit_findings", { status });
  }

  /**
   * Accept (apply its `acceptAction`) or dismiss ONE finding. Resolves with the
   * updated row — the FE swaps the row in only AFTER this confirms; a rejection
   * leaves it pending (no optimistic success UI).
   */
  resolveAuditFinding(
    id: string,
    action: "accept" | "dismiss",
  ): Promise<AuditFinding> {
    return invoke<AuditFinding>("resolve_audit_finding", { id, action });
  }

  /**
   * The audit state changed (a run finished / findings were purged by a seal
   * or a delete). The payload shape is deliberately NOT trusted — refetch via
   * {@link listAuditFindings} instead of reading it.
   */
  onAuditUpdated(cb: () => void): Promise<UnlistenFn> {
    return listen(EVENT_AUDIT_UPDATED, () => cb());
  }

  /** The weekly-audit schedule state (enabled + last-run / next-due epochs). */
  getAuditSchedule(): Promise<AuditSchedule> {
    return invoke<AuditSchedule>("get_audit_schedule");
  }

  /**
   * Turn the weekly scheduled audit on or off. Resolves with the CONFIRMED
   * schedule — the FE reflects the response, never an optimistic flip.
   * Scheduled runs emit the same audit-updated event manual runs do.
   */
  setAuditSchedule(enabled: boolean): Promise<AuditSchedule> {
    return invoke<AuditSchedule>("set_audit_schedule", { enabled });
  }

  /**
   * Ask the configured AI provider to explain ONE staged finding (any kind).
   * May reject with a consent-missing or `AppError::Locked` message — surface
   * it verbatim. Cloud providers only ever see redacted content.
   */
  explainAuditFinding(id: string): Promise<AuditExplanation> {
    return invoke<AuditExplanation>("explain_audit_finding", { id });
  }

  // ── brain2 documents — expand the brain with imported .md/.txt files ────

  /**
   * Ingest TYPED text as a brain NOTE (`kind='note'`) into `folderId` — the twin
   * of {@link importDocument} for the "+ Add note" editor. Chunks + (when the e5
   * model is present) vector-indexes it exactly like an uploaded document, minus
   * the file/extension step. Resolves with the new document id. Rejects with
   * `AppError::Locked` when the folder is sealed-and-NOT-session-unlocked (never
   * resurrect plaintext behind a lock) and with `AppError::InvalidArg` on empty
   * text — surface both as friendly messages.
   */
  importText(name: string, text: string, folderId: string): Promise<string> {
    return invoke<string>("import_text", { name, text, folderId });
  }

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
   * SMART-NOTE ENGINE — turn an already-ingested document/photo (`documentId`) into a
   * formatted Obsidian note through the provider seam, in one of two recipe shapes
   * ({@link NoteRecipe}: `"synthesis"` for whiteboards/screenshots, `"structure-mirror"`
   * for forms/tables). Resolves with the NEW `kind='note'` document id (open it via the
   * usual note surfaces). Only REDACTED TEXT egresses (the redaction firewall lives in the
   * backend provider factory); no image bytes are ever sent. Rejects with `AppError::Locked`
   * when the document's folder is sealed-and-NOT-session-unlocked, `AppError::Unavailable`
   * when cloud egress isn't consented, and `AppError::InvalidArg` for an unknown id / a
   * document with no extractable text — surface each as a friendly message.
   */
  generateNoteFromDocument(
    documentId: string,
    recipe: NoteRecipe,
  ): Promise<string> {
    return invoke<string>("generate_note_from_document", {
      documentId,
      recipe,
    });
  }

  /**
   * Permanently delete a document (cascade-deletes its chunks + vectors). GATED:
   * a sealed-and-NOT-session-unlocked folder is refused (`AppError::Locked`);
   * an unknown id is an idempotent no-op.
   */
  deleteDocument(id: string): Promise<void> {
    return invoke<void>("delete_document", { id });
  }

  /**
   * Ask-My-Vault: grounded Q&A across ALL meetings, with source meetings.
   * `askThreadId` (optional, FE-minted per question) keys this turn's live
   * tool-trace: the backend stamps it onto the `murmur://ask-tool` events it
   * emits while answering, so the Ask page routes chips to the in-flight
   * question only. NOTE: the backend caps the effective history at the LAST
   * 12 messages — send the full conversation and let it truncate; do NOT
   * trim FE-side.
   */
  /**
   * Ask a grounded question across the whole vault. `explicitSources`
   * (source-scoped Brain) OPTIONALLY pins the answer to exactly the passed
   * sources + their links (each `{kind, id}`; an extra `title` is ignored
   * backend-side). Omitting it / `[]` / `null` keeps today's whole-vault
   * behavior; a NON-empty array narrows the scope. Only sent when non-empty so
   * existing callers are unaffected.
   */
  askVault(
    question: string,
    history: ChatTurn[],
    askThreadId?: string,
    explicitSources?: SourceRef[],
    pinnedOrgItemId?: string,
    dashboardId?: string,
  ): Promise<AskVaultResult> {
    return invoke<AskVaultResult>("ask_vault", {
      question,
      history,
      askThreadId,
      ...(explicitSources?.length ? { explicitSources } : {}),
      // Org-item viewer: pin a read-only SHARED (org-feed) note into the Ask context server-side
      // (the local Brain never retrieves org content via search, so pinning is what grounds it).
      ...(pinnedOrgItemId ? { pinnedOrgItemId } : {}),
      // Board-scoped Ask: the board's DERIVED tiles (promises, drift, pulse, reminders,
      // person, living answer) are what the user is looking at, but they are not
      // retrievable documents, so `get_dashboard_sources` deliberately never turns them
      // into a `SourceRef`. Sending the board ID lets the BACKEND render them through the
      // same gated path MCP already reads. An ID, never the finished text — handing a
      // string straight into a prompt would be an injection surface and would build
      // content outside the gate.
      ...(dashboardId ? { dashboardId } : {}),
    });
  }

  /** Newest-first durable Ask conversations for one exact local scope. */
  listAskConversations(
    scope: AskConversationScope,
  ): Promise<AskConversationSummary[]> {
    return invoke<AskConversationSummary[]>("list_ask_conversations", {
      scope,
    });
  }

  /** Load one canonical, visibility-gated conversation in its exact scope. */
  loadAskConversation(
    scope: AskConversationScope,
    conversationId: string,
  ): Promise<AskConversation> {
    return invoke<AskConversation>("load_ask_conversation", {
      scope,
      conversationId,
    });
  }

  /**
   * Persisted vault/note Ask. Canonical history stays backend-owned;
   * `askTraceId` is distinct and routes only this request's live tool events.
   */
  askVaultPersisted(
    scope: AskConversationScope,
    question: string,
    conversationId?: string,
    explicitSources?: SourceRef[],
    askTraceId?: string,
    dashboardId?: string,
  ): Promise<AskConversationSendResult> {
    return invoke<AskConversationSendResult>("ask_vault_persisted", {
      scope,
      question,
      conversationId,
      askTraceId,
      ...(explicitSources?.length ? { explicitSources } : {}),
      ...(dashboardId ? { dashboardId } : {}),
    });
  }

  /** Persisted meeting Ask, preserving the dedicated transcript-chat core. */
  chatMeetingPersisted(
    meetingId: string,
    question: string,
    conversationId?: string,
    explicitSources?: SourceRef[],
    dashboardId?: string,
  ): Promise<AskConversationSendResult> {
    return invoke<AskConversationSendResult>("chat_meeting_persisted", {
      meetingId,
      question,
      conversationId,
      ...(explicitSources?.length ? { explicitSources } : {}),
      ...(dashboardId ? { dashboardId } : {}),
    });
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
   * Per-claim "Receipts" (Brain v3 PR-5): each note line that clears the
   * token-overlap bar aligned to the transcript segment it derives from, so the
   * UI can seek the audio player to `startS` and prove the claim. GATED — a
   * sealed-and-not-session-unlocked meeting returns `[]` (no segment times,
   * speakers, or overlaps leak behind the lock), as does a meeting with no note
   * yet. Carries no note/transcript TEXT (line index + audio coordinates + ASR
   * metadata only).
   */
  getNoteReceipts(meetingId: string): Promise<ClaimAlignment[]> {
    return invoke<ClaimAlignment[]>("get_note_receipts", { meetingId });
  }

  /**
   * The audio receipt for ONE fact's text against its SOURCE meeting (Brain v3
   * audit PR-8) — lets the decision-ledger "Source" chip deep-seek the meeting's
   * audio to the second the fact derives from. Same gate and alignment floor as
   * `getNoteReceipts`: a sealed-and-not-session-unlocked meeting — or a fact the
   * transcript doesn't clearly support — resolves `null`, and the caller falls
   * back to plainly opening the meeting.
   */
  getFactReceipt(
    meetingId: string,
    factText: string,
  ): Promise<ClaimAlignment | null> {
    return invoke<ClaimAlignment | null>("get_fact_receipt", {
      meetingId,
      factText,
    });
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

  /**
   * READ-ONLY: the cached AI-derived speaker + topic timeline (or an EMPTY one when none is cached).
   * NEVER generates — a passive Audio-tab open must not load a multi-GB on-device model. Use
   * `generateTimeline` to derive it, gated by `timelineGenerationOnDevice` (perf/OOM).
   */
  getTimeline(meetingId: string): Promise<MeetingTimeline> {
    return invoke<MeetingTimeline>("get_timeline", { meetingId });
  }

  /**
   * The meeting's transcript segments, fetched LAZILY only when the Audio tab
   * opens (`get_meeting_detail` now returns an EMPTY `segments` so a plain
   * Note-tab open never ships the whole transcript). GATED server-side exactly
   * like every content read: a sealed-and-not-session-unlocked meeting returns
   * `[]`, never leaking transcript text behind the lock. Mirrors
   * {@link getTimeline}'s `{ meetingId }` invoke-arg convention.
   */
  getMeetingSegments(meetingId: string): Promise<Segment[]> {
    return invoke<Segment[]>("get_meeting_segments", { meetingId });
  }

  /**
   * EXPLICIT (heavy) timeline generation — derives + caches the speaker/topic map via the Notes-role
   * provider. For an on-device provider this loads a multi-GB model, so the FE only calls it
   * automatically for cheap cloud providers and behind a user click for on-device ones.
   */
  generateTimeline(meetingId: string): Promise<MeetingTimeline> {
    return invoke<MeetingTimeline>("generate_timeline", { meetingId });
  }

  /**
   * True when deriving this install's timeline would load a residency-bound on-device model (local
   * GGUF / Ollama / Apple FM) — i.e. generation is HEAVY and must be hidden behind an explicit
   * click, not auto-fired on tab open. False for cloud providers (cheap → auto-generate).
   */
  timelineGenerationOnDevice(): Promise<boolean> {
    return invoke<boolean>("timeline_generation_on_device");
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

  /**
   * Speaker voiceprints (opt-in) — suggest a person name for each diarized
   * `others-{n}` cluster of a meeting, by cosine re-identification against prior
   * LABELED voiceprints. GATED backend: a locked meeting yields `[]`, and a sealed
   * prior is never a match candidate. Empty when the opt-in is off, nothing matches,
   * or no voiceprint exists. Accepting a suggestion is a one-tap `renameSpeaker`.
   */
  suggestSpeakerLabels(meetingId: string): Promise<SpeakerSuggestion[]> {
    return invoke<SpeakerSuggestion[]>("suggest_speaker_labels", { meetingId });
  }

  /**
   * List stored voiceprints for the Settings management view (label + provenance +
   * dim — NEVER the raw embedding). GATED: a sealed-not-unlocked meeting's row is
   * excluded.
   */
  listVoiceprints(): Promise<VoiceprintInfo[]> {
    return invoke<VoiceprintInfo[]>("list_voiceprints");
  }

  /** Forget (hard-delete) one stored voiceprint by id. Idempotent. */
  forgetVoiceprint(id: string): Promise<void> {
    return invoke<void>("forget_voiceprint", { id });
  }

  /** Clear every stored voiceprint (the "forget all captured voices" affordance). */
  clearVoiceprints(): Promise<void> {
    return invoke<void>("clear_voiceprints");
  }

  /** Whether a usable Whisper model is present (configured path or default models dir). */
  modelPresent(): Promise<boolean> {
    return invoke<boolean>("model_present");
  }

  /**
   * Download the Whisper model for the SAVED language + size (plus the live-caption
   * companion when one is planned). Progress streams over {@link onModelDownload}.
   *
   * Resolves with a typed OUTCOME: `status: "cancelled"` when
   * {@link cancelModelDownload} interrupted it. A cancel is NOT a rejection —
   * the FE must never string-match an error message to tell a user-initiated
   * cancel apart from a dead link.
   */
  downloadModel(): Promise<ModelDownloadOutcome> {
    return invoke<ModelDownloadOutcome>("download_model");
  }

  /**
   * Cancel the Whisper model download that is in flight right now. Idempotent
   * and infallible — with nothing running it is a no-op, and it can never reach
   * forward into a download that has not started yet (the backend watches a
   * generation counter, not a global flag).
   */
  cancelModelDownload(): Promise<void> {
    return invoke<void>("cancel_model_download");
  }

  /**
   * Delete ONE downloaded Whisper model to reclaim disk. Rejects (with the
   * backend's reason) when the size is the effective model, the live-caption
   * pin, or not a catalog id — the backend owns every refusal, so the FE never
   * re-implements them.
   */
  deleteWhisperModel(size: string): Promise<void> {
    return invoke<void>("delete_whisper_model", { size });
  }

  /** Whether the OPTIONAL parakeet live-ASR engine's models are all present on disk. */
  parakeetModelsPresent(): Promise<boolean> {
    return invoke<boolean>("parakeet_models_present");
  }

  /**
   * Download the parakeet live-ASR engine's int8 models (~600 MB) if missing. Progress streams
   * over {@link onModelDownload} (EVENT_MODEL_DOWNLOAD, shared with the whisper download); the
   * promise resolves when the download finishes (or rejects on failure).
   */
  downloadParakeetModels(): Promise<void> {
    return invoke<void>("download_parakeet_models");
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

  /** Whether a usable on-device brain (reasoning GGUF) is present at the resolved path. */
  brainModelPresent(): Promise<boolean> {
    return invoke<boolean>("brain_model_present");
  }

  // ── Murmur Brain — posture (Cloud / Hybrid / Fully local) ──────────────

  /**
   * The DERIVED Murmur Brain posture for the Settings display — computed by the
   * backend from the live config (never stored), so the label can never lie about
   * egress. `"custom"` when the config matches no preset; never settable.
   */
  brainPosture(): Promise<Posture> {
    return invoke<Posture>("brain_posture");
  }

  /** The resolved "what runs where" rows for the Settings AI map (read-only config projection). */
  resolvedAiMap(): Promise<AiMapRow[]> {
    return invoke<AiMapRow[]>("resolved_ai_map");
  }

  /**
   * Apply a Murmur Brain posture PRESET (`cloud` / `hybrid` / `fully_local`) and
   * persist it. The single writer of the posture presets — a raw settings save
   * preserves the posture keys. Rejects (`InvalidArg`) on the derived-only
   * `"custom"`.
   */
  setBrainPosture(posture: Posture): Promise<void> {
    return invoke<void>("set_brain_posture", { posture });
  }

  /**
   * The installed-base migration nudge, or `null`: non-null when the persisted
   * brain model is a RETIRED (non-commercial) id, telling the FE to offer the
   * Apache-licensed replacement. Read-only capability probe (no content, no egress).
   */
  brainModelRetirementNudge(): Promise<RetiredModelNudge | null> {
    return invoke<RetiredModelNudge | null>("brain_model_retirement_nudge");
  }

  // ── P1 — this Mac, the whisper catalog, and the recommendation ──────────

  /**
   * ONE command answering the machine profile, the whisper catalog and BOTH
   * "which model?" answers (`recommendedId` = the honest hardware answer,
   * `autoDefaultId` = what a blank config resolves to today). It is deliberately
   * one call rather than two: free disk is volatile, so two commands reading it
   * at different instants could disagree.
   */
  whisperRecommendation(): Promise<WhisperRecommendationDto> {
    return invoke<WhisperRecommendationDto>("whisper_recommendation");
  }

  /**
   * The one-shot notice that this install last ran on a DIFFERENT Mac. `null`
   * when there is nothing to say. PULLED rather than pushed — an event emitted
   * during backend `setup` would be lost, because the webview has not called
   * `listen()` yet at that point.
   */
  machineChangeNudge(): Promise<MachineChangeNudge | null> {
    return invoke<MachineChangeNudge | null>("machine_change_nudge");
  }

  /** Clear the machine-change notice so it never shows again for this move. */
  dismissMachineChangeNudge(): Promise<void> {
    return invoke<void>("dismiss_machine_change_nudge");
  }

  /**
   * The Realtime-Reactions SHADOW counter: how many contradiction cards WOULD
   * have fired this recording while the sub-toggle is OFF. Resets each
   * `startRecording`. Lets the FE offer user-local "the brain would have flagged
   * N — enable?" calibration (no telemetry).
   */
  brainReactionsShadowCount(): Promise<number> {
    return invoke<number>("brain_reactions_shadow_count");
  }

  /**
   * Flip the Realtime-Reactions CONTRADICTION-card sub-toggle. Default OFF
   * (shadow mode). Dedicated command (not the raw settings save, which only
   * preserves it) so a partial save can never silently enable the ⚠ cards.
   */
  setBrainContradictionCards(enabled: boolean): Promise<void> {
    return invoke<void>("set_brain_contradiction_cards", { enabled });
  }

  /**
   * Whether this Mac has enough RAM to run Realtime Reactions (the light engine)
   * alongside a live recording — the combined-residency guard. Lets the Brain
   * Live enablement card show a non-blocking "needs more RAM" warning. `true`
   * when total RAM can't be read (never block behind a failed probe). Read-only
   * capability probe (no content, no egress).
   */
  brainLiveRamOk(): Promise<boolean> {
    return invoke<boolean>("brain_live_ram_ok");
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

  // ── Phase D — on-device PERSON-name redaction (NER) model ──────────────

  /**
   * Whether the on-device PERSON-name NER model (mDeBERTa-v3) is present — i.e.
   * the redaction firewall additionally masks people's NAMES before any cloud
   * egress. Cheap existence probe; NEVER errors on a missing models dir. When
   * false, only emails / card numbers / phone numbers are redacted by default —
   * names can leave alongside the transcript on a cloud provider. Mirrors
   * {@link embedModelPresent}.
   */
  nerModelPresent(): Promise<boolean> {
    return invoke<boolean>("ner_model_present");
  }

  /**
   * Download the on-device PERSON-name NER model (mDeBERTa-v3, 3 files) into the
   * shared models dir. Progress streams over {@link onNerDownload}
   * (EVENT_NER_DOWNLOAD); the promise resolves with the model dir when the
   * download finishes. INBOUND-ONLY — sends NO meeting content (no egress). The
   * model is picked up lazily on the next cloud summarization. Mirrors
   * {@link downloadEmbedModel}.
   */
  downloadNerModel(): Promise<string> {
    return invoke<string>("download_ner_model");
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
   * Phase 6 — tell the backend which meeting the user is currently VIEWING /
   * anchored to (the FOCUS pointer), distinct from the recording pointer. Call
   * with the meeting id when a meeting-detail / conversation view opens, and with
   * `null` when it closes. This is a backend SAFETY-NET: the brain resolves its
   * "this meeting" scope as `explicit meetingId > focus > recording`, so any
   * assistant path that falls back off an explicit id (the voice/wake twin) still
   * scopes to the meeting on screen — even when nothing is recording, and even
   * when a DIFFERENT meeting is recording. Focus is only an id (never content);
   * the sealed-content gate is unchanged (a relocked meeting stays masked).
   * Best-effort — a failure never blocks opening the view.
   */
  setFocusMeeting(meetingId: string | null): Promise<void> {
    return invoke<void>("set_focus_meeting", { meetingId });
  }

  /**
   * Ask the in-meeting assistant a TYPED question (the text composer — the twin of
   * the voice trigger). Routes the typed command through the SAME gated agentic
   * brain as voice: the model decides which gated tools to call, the live tool
   * trace streams via {@link onAssistantTool}, and the answer arrives via
   * {@link onVoiceActionResult}. Resolves once the turn is dispatched (not when
   * the answer is ready). Rejects (InvalidArg) on an empty question.
   *
   * Pass `threadId` to persist the exchange under that thread (the result +
   * tool-trace events come back stamped with it); omit it for an anchorless ask.
   *
   * Pass `meetingId` (Phase 4) to bind the turn to a SPECIFIC meeting: the backend
   * resolves `meetingId ?? state.current_meeting`, so an explicit id wins (a
   * past/anchored @brain thread scopes to ITS meeting) while omitting it keeps the
   * live-recording scope. Omitting it preserves the pre-Phase-4 behavior for the
   * voice/wake twin.
   */
  askAssistantText(
    text: string,
    threadId?: string,
    meetingId?: string,
  ): Promise<void> {
    return invoke<void>("ask_assistant_text", { text, threadId, meetingId });
  }

  /**
   * Ask the in-meeting assistant a CHAT message (the dedicated multi-turn panel).
   * Sends the FULL conversation (incl. the new user message as the last item) so
   * the brain has multi-turn memory, and RESOLVES with the reply (summary +
   * citations + status) so the panel can resolve the in-flight assistant bubble.
   * The live tool-trace streams via {@link onChatTool}.
   *
   * Pass `threadId` (the FE-generated thread key) + `anchorText` (the note line
   * the thread hangs under) so the exchange persists under THIS thread and
   * rehydrates attached to its anchor via {@link listAssistantThreads}. Both
   * optional — but an omitted `threadId` is NOT ephemeral: the backend generates
   * a UUID itself and the exchange STILL persists (it then rehydrates as a
   * standalone anchorless thread). Always pass the thread's own id so its
   * exchanges stay grouped.
   *
   * Pass `meetingId` (Phase 4) to bind the thread to a SPECIFIC meeting so the
   * brain scopes "this meeting" correctly: the backend resolves
   * `meetingId ?? state.current_meeting`, so an explicit id wins (a past/anchored
   * thread answers about ITS meeting even while a different meeting records) and
   * omitting it keeps the live-recording scope. This is what kills the
   * wrong-meeting bug — always pass the thread's bound meeting id.
   */
  askAssistantChat(
    messages: ChatMsg[],
    threadId?: string,
    anchorText?: string,
    meetingId?: string,
    explicitSources?: SourceRef[],
  ): Promise<VoiceActionResultPayload> {
    return invoke<VoiceActionResultPayload>("ask_assistant_chat", {
      messages,
      threadId,
      anchorText,
      meetingId,
      ...(explicitSources?.length ? { explicitSources } : {}),
    });
  }

  /**
   * A meeting's PERSISTED `@brain` thread exchanges, oldest → newest, ONLY rows
   * that carry a threadId. GATED server-side: a sealed-and-not-session-unlocked
   * meeting returns an EMPTY array (masked — never a question/answer behind the
   * lock). The record screen groups rows by `threadId` to rebuild its threads.
   */
  listAssistantThreads(meetingId: string): Promise<AssistantThreadRow[]> {
    return invoke<AssistantThreadRow[]>("list_assistant_threads", {
      meetingId,
    });
  }

  // ── brain2 realtime typed @brain notes (record screen "My notes") ──────

  /**
   * Persist a meeting's live typed-notes buffer (the legacy `manual_notes` mirror).
   * GATED server-side: a sealed-and-not-session-unlocked meeting is refused with
   * `AppError::Locked` (never resurrect typed plaintext behind a lock). The text is
   * the user's OWN words (no new egress). NOTE: since the v2 document-first redesign
   * the recording panel's note-taking autosaves through the companion NOTE editor
   * path (`save_note_text`/`update_note_doc`), and the summary reads the companion
   * note; this wrapper stays for the backend command + any legacy caller.
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

  /**
   * DOCUMENT-FIRST (v2): EAGERLY get-or-create the meeting's ONE companion note so
   * the recording panel's "Note" tab has a stable note id to mount the embedded
   * `app-note-editor` on. The backend gets-or-creates the note (Notes ROOT,
   * `meeting_id` set, managed title synced to the meeting, front-matter `[[Meeting]]`
   * link stamped) WITHOUT writing any body, and returns `{ noteId, meetingWikilink }`.
   * Idempotent — a second call reuses the same note (one-note-per-meeting). GATED:
   * a sealed-and-not-session-unlocked meeting is refused with `AppError::Locked`.
   */
  getOrCreateCompanionNote(meetingId: string): Promise<CompanionAppendResult> {
    return invoke<CompanionAppendResult>("get_or_create_companion_note", {
      meetingId,
    });
  }

  /**
   * Append an accepted Ask-Brain draft to the meeting's ONE living companion note.
   * The backend lazily gets-or-creates the companion note (in the always-open Notes
   * ROOT, `meeting_id` set, managed title synced to the meeting), appends the
   * markdown block atomically (single-writer — no FE read-modify-write race),
   * refreshes the front-matter `[[Meeting]]` link, and re-exports the vault `.md`.
   * Returns the note's id + the display wikilink. The text is an accepted draft
   * (the user's own request) — no new cloud egress. GATED like every content write;
   * a failure must not blank prior content (verify-before-destroy N/A — additive only).
   */
  appendToCompanionNote(
    meetingId: string,
    markdown: string,
  ): Promise<CompanionAppendResult> {
    return invoke<CompanionAppendResult>("append_to_companion_note", {
      meetingId,
      markdown,
    });
  }

  // ── workspace hierarchy (Projects › Folders › typed item groups) ──

  /**
   * The whole container forest: projects, their folders, and the first page of
   * each container's items per kind, with per-kind totals.
   *
   * A sealed-and-not-session-unlocked container comes back with NO groups — the
   * backend refuses to describe what it holds, so the tree renders it collapsed
   * and count-free rather than pretending it is empty.
   */
  listWorkspaceTree(): Promise<ContainerNode[]> {
    return invoke<ContainerNode[]>("list_workspace_tree");
  }

  /** Create one new top-level Workspace and its safe vault-relative directory. */
  createSpace(name: string): Promise<Folder> {
    return invoke<Folder>("create_space", { name });
  }

  /**
   * One page of a single container's items of a single kind — what "Zobacz
   * wszystkie" pages through.
   *
   * `containerId: null` addresses the unfiled inbox. A sealed-and-not-unlocked
   * container is REFUSED here (a rejected promise), never answered with an empty
   * page: an empty page and a sealed one must not look alike to the caller.
   */
  listContainerItems(
    containerId: string | null,
    kind: ItemKind,
    offset: number,
    limit: number,
  ): Promise<ItemPage> {
    return invoke<ItemPage>("list_container_items", {
      containerId,
      kind,
      offset,
      limit,
    });
  }

  /** One container's own metadata (never its contents); `null` when unknown. */
  getContainer(id: string): Promise<ContainerNode | null> {
    return invoke<ContainerNode | null>("get_container", { id });
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

  /** Explicitly seal locally while remote share recipients retain access. */
  lockFolderAllowRemoteAccess(folderId: string): Promise<void> {
    return invoke<void>("lock_folder_allow_remote_access", { folderId });
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

  // ── Notes — first-class authored notes (documents kind='note') ──────────
  // One typed method per command in docs/notes-feature/DESIGN.md §2. Every
  // read/list/export/assistant path is GATED backend-side on the note's
  // folder-unlock — a sealed-and-not-session-unlocked note is MASKED (title
  // "🔒 Locked", no body/snippet/tags), never leaked per-row. Args are camelCase;
  // Tauri maps them onto the snake_case Rust params.

  /**
   * Create an empty note in `folderId` (null ⇒ the default "Notes" folder).
   * Returns the new note id — navigate to `/notes/:id` to open the editor.
   */
  createNote(folderId: string | null, title: string): Promise<string> {
    return invoke<string>("create_note", { folderId, title });
  }

  /**
   * Auto-title an "Untitled" note from its body — the on-device model when present,
   * else a first-line heuristic. LOCAL-ONLY (never egresses). No-ops (returns the
   * current title) when the note already has a title, is empty, or sits in a locked
   * folder. Returns the resulting title. Called fire-and-forget when the editor closes.
   */
  suggestNoteTitle(noteId: string): Promise<string> {
    return invoke<string>("suggest_note_title", { noteId });
  }

  /**
   * Read ONE note in full for the editor. GATED: a sealed-and-not-session-unlocked
   * note returns a MASKED {@link NoteDoc} (`locked: true`, title "🔒 Locked", empty
   * markdown/tags/properties) — render the lock gate, not the body.
   */
  getNote(id: string): Promise<NoteDoc> {
    return invoke<NoteDoc>("get_note", { id });
  }

  /**
   * Persist a note's title + FULL markdown (incl. front-matter), re-index it for
   * the brain, re-export the vault `.md`, and bump `updatedAt`. Write-gated:
   * rejects (`Locked`) for a sealed-and-not-session-unlocked note. Returns the
   * reconciled {@link NoteDoc}.
   *
   * NAME NOTE: the meeting-note editor already owns `updateNote(meetingId,
   * markdown)` (a different Tauri command), so the authored-note editor's method
   * is `updateNoteDoc` to avoid a TS method collision — the editor agent binds to
   * THIS one. (Same reason `moveNoteDoc` / `exportNoteDoc` are suffixed: the
   * meeting `moveNote` / `exportNote` methods pre-exist.)
   */
  updateNoteDoc(id: string, title: string, markdown: string): Promise<NoteDoc> {
    return invoke<NoteDoc>("update_note_doc", { id, title, markdown });
  }

  /**
   * FAST autosave — persist a note's title + markdown ONLY (no re-index / no vault
   * re-export). The frequent debounced autosave uses this so typing stays smooth
   * even with the embed model present; the expensive re-index + export are deferred
   * to {@link updateNoteDoc}, which the editor runs on blur / close / preview.
   * Returns the new `updatedAt` (epoch ms).
   */
  saveNoteText(id: string, title: string, markdown: string): Promise<number> {
    return invoke<number>("save_note_text", { id, title, markdown });
  }

  /** Store one locally-normalized WebP attachment under a gated content owner. */
  addNoteAttachment(
    ownerKind: NoteAttachmentOwnerKind,
    ownerId: string,
    fileName: string,
    mimeType: string,
    dataBase64: string,
  ): Promise<NoteAttachmentDto> {
    return invoke<NoteAttachmentDto>("add_note_attachment", {
      ownerKind,
      ownerId,
      fileName,
      mimeType,
      dataBase64,
    });
  }

  /** List image attachments for one unlocked/readable owner. */
  listNoteAttachments(
    ownerKind: NoteAttachmentOwnerKind,
    ownerId: string,
  ): Promise<NoteAttachmentDto[]> {
    return invoke<NoteAttachmentDto[]>("list_note_attachments", {
      ownerKind,
      ownerId,
    });
  }

  /** Delete one attachment after its markdown reference has been removed. */
  deleteNoteAttachment(
    ownerKind: NoteAttachmentOwnerKind,
    ownerId: string,
    attachmentId: string,
  ): Promise<void> {
    return invoke<void>("delete_note_attachment", {
      ownerKind,
      ownerId,
      attachmentId,
    });
  }

  /**
   * List notes in `folderId` (null ⇒ all VISIBLE notes across note-folders). GATED
   * IN THE QUERY: a sealed-and-not-session-unlocked note is masked (title
   * "🔒 Locked", empty snippet/tags) — never a per-row skip that could leak a title.
   */
  listNotes(folderId: string | null): Promise<NoteSummary[]> {
    return invoke<NoteSummary[]>("list_notes", { folderId });
  }

  /**
   * Move a note into `folderId` (re-exports it under the new folder's vault path).
   * Gated on BOTH sides — rejects (`Locked`) when the source or target folder is
   * sealed-and-not-session-unlocked.
   */
  moveNoteDoc(id: string, folderId: string): Promise<void> {
    return invoke<void>("move_note_doc", { id, folderId });
  }

  /**
   * Permanently delete a note (cascade-deletes its chunks + vectors + vault `.md`).
   * GATED: a sealed-and-not-session-unlocked folder is refused (`Locked`); an
   * unknown id is an idempotent no-op.
   */
  deleteNote(id: string): Promise<void> {
    return invoke<void>("delete_note", { id });
  }

  /**
   * (Re)write a note's vault `.md` under its note-folder path and return the
   * written path. GATED: rejects (`Locked`) for a sealed-and-not-session-unlocked
   * note.
   */
  exportNoteDoc(id: string): Promise<string> {
    return invoke<string>("export_note_doc", { id });
  }

  /**
   * The selection Brain-assistant action (full set — edit / structure / brain /
   * extract / create; see the shared action catalog). `req` carries the action,
   * the selection + bounded context, and optionally `variant` (tone/language) or
   * `instruction` (custom free-text / an `ask` question). Routes via
   * `provider_for(Role::Notes)` (local vs cloud per posture, redaction firewall +
   * egress ledger for free; `find_related` is retrieval-only, no provider). The
   * result carries the `shape` the FE renders off, `title` for artifacts, and the
   * resolved `modelLabel`/`mode`/`redacted` for the mode chip. Optional `req`
   * fields ride through unchanged (the whole object is forwarded).
   */
  noteAssistantAction(req: NoteAssistRequest): Promise<NoteAssistResult> {
    return invoke<NoteAssistResult>("note_assistant_action", { req });
  }

  /**
   * Propose per-note folder assignments by content (auto-organize step 1). Returns
   * an {@link OrganizePlan} of moves with reasons; `folderId` (null ⇒ all notes)
   * scopes the run. Non-destructive — nothing moves until {@link applyOrganizePlan}.
   */
  async planOrganizeNotes(
    folderId: string | null,
    guidance: string | null = null,
  ): Promise<OrganizePlan> {
    const plan = await invoke<OrganizePlan>("plan_organize_notes", {
      folderId,
      guidance,
    });
    if (
      !Object.prototype.hasOwnProperty.call(plan, "scopeFolderId") ||
      plan.scopeFolderId !== folderId
    ) {
      throw new Error("The organizer returned a plan for a different scope.");
    }
    return {
      ...plan,
      moves: plan.moves.map((move) => ({
        ...move,
        reviewScopeFolderId: plan.scopeFolderId,
      })),
    };
  }

  /**
   * Apply an auto-organize plan (step 2): create the needed note-folders + move the
   * notes (gated; re-exports). Confirm-before-apply on the FE.
   */
  async applyOrganizePlan(
    plan: OrganizePlan | Pick<OrganizePlan, "moves">,
  ): Promise<OrganizeApplyResult> {
    let scopeFolderId: string | null;
    if ("scopeFolderId" in plan) {
      scopeFolderId = plan.scopeFolderId;
    } else if (plan.moves.length === 0) {
      scopeFolderId = null;
    } else {
      const scopes = plan.moves.map((move) => {
        if (
          !Object.prototype.hasOwnProperty.call(move, "reviewScopeFolderId") ||
          move.reviewScopeFolderId === undefined ||
          move.reviewScopeFolderId === ""
        ) {
          throw new Error(
            "The selected organizer move is missing its reviewed scope.",
          );
        }
        return move.reviewScopeFolderId;
      });
      scopeFolderId = scopes[0];
      if (scopes.some((scope) => scope !== scopeFolderId)) {
        throw new Error(
          "Selected organizer moves came from different review scopes.",
        );
      }
    }
    const wireMoves = plan.moves.map((move) => ({
      noteId: move.noteId,
      title: move.title,
      fromFolderId: move.fromFolderId,
      fromFolder: move.fromFolder,
      toFolder: move.toFolder,
      toFolderId: move.toFolderId,
      confidence: move.confidence,
      reason: move.reason,
    }));
    const normalized = {
      scopeFolderId,
      moves: wireMoves,
      totalScanned:
        "totalScanned" in plan ? plan.totalScanned : plan.moves.length,
      alreadyOrganized: "alreadyOrganized" in plan ? plan.alreadyOrganized : 0,
      deferred: "deferred" in plan ? plan.deferred : 0,
      targets: "targets" in plan ? plan.targets : [],
    };
    return invoke<OrganizeApplyResult>("apply_organize_plan", {
      plan: normalized,
    });
  }

  /** Propose where unfiled recordings belong. Nothing moves until apply. */
  planWorkspaceOrganization(
    guidance: string | null = null,
  ): Promise<WorkspaceOrganizePlan> {
    return invoke<WorkspaceOrganizePlan>("plan_workspace_organization", {
      guidance,
    });
  }

  /** Apply only the recording moves the user kept selected in the review sheet. */
  applyWorkspaceOrganization(
    moves: WorkspaceOrganizeMove[],
  ): Promise<WorkspaceOrganizeApplyResult> {
    return invoke<WorkspaceOrganizeApplyResult>(
      "apply_workspace_organization",
      { moves },
    );
  }

  /** Content-free health for crash-safe filing recovery; no ids, paths, or titles. */
  getFilingRecoveryStatus(): Promise<FilingRecoveryStatus> {
    return invoke<FilingRecoveryStatus>("get_filing_recovery_status");
  }

  /** Retry exact-identity recovery after the user resolves a vault-side conflict. */
  retryFilingRecovery(): Promise<FilingRecoveryStatus> {
    return invoke<FilingRecoveryStatus>("retry_filing_recovery");
  }

  /** Preserve one external vault occupant after an explicit destructive confirmation. */
  keepExistingFilingFile(
    issueToken: string,
    confirmed: true,
  ): Promise<FilingRecoveryStatus> {
    return invoke<FilingRecoveryStatus>("keep_existing_filing_file", {
      issueToken,
      confirmed,
    });
  }

  // ── Feature C — typed note front-matter properties (folder-level schema +
  // the typed notes list feeding the Table/Board views). All three are GATED
  // backend-side on the folder unlock: a LOCKED folder returns `[]` from BOTH
  // reads (empty schema + empty rows), so no typed view is offered for it and
  // no sealed content leaks. `properties` (the plaintext front-matter map) is
  // untouched — the schema is a NEW parallel layer describing how to render it.

  /**
   * The property SCHEMA for a note-folder: one {@link PropertySchemaField} per
   * defined property (its key + kind + select options). Returns `[]` for a
   * LOCKED (sealed-and-not-session-unlocked) folder — the backend gates it, so a
   * locked folder never exposes a typed view.
   */
  getNoteFolderSchema(folderId: string): Promise<PropertySchemaField[]> {
    return invoke<PropertySchemaField[]>("get_note_folder_schema", {
      folderId,
    });
  }

  /**
   * Persist a note-folder's property schema (replaces the whole field set).
   * Gated — rejects (`Locked`) for a sealed-and-not-session-unlocked folder.
   */
  setNoteFolderSchema(
    folderId: string,
    fields: PropertySchemaField[],
  ): Promise<void> {
    return invoke<void>("set_note_folder_schema", { folderId, fields });
  }

  /**
   * The TYPED notes list for a folder — one {@link TypedNoteRow} per note, each
   * carrying its property key → {@link PropertyValue} map, tags, and updatedAt,
   * for the folder's Table/Board views. GATED IN THE QUERY: a SEALED folder
   * returns `[]` and a masked row carries no values/tags — never a per-row leak.
   */
  listNotesTyped(folderId: string): Promise<TypedNoteRow[]> {
    return invoke<TypedNoteRow[]>("list_notes_typed", { folderId });
  }

  /** The note-kind folder list (`kind='note'` only). */
  listNoteFolders(): Promise<NoteFolder[]> {
    return invoke<NoteFolder[]>("list_note_folders");
  }

  /**
   * DRY-RUN an import: report what it would do WITHOUT writing anything. `path` is the export
   * archive/folder for Notion and Obsidian, and null for Apple Notes (the library IS the source).
   * Zero egress - every source is already on this machine.
   */
  scanImport(
    source: ImportSourceId,
    path: string | null,
  ): Promise<ImportScanReport> {
    return invoke<ImportScanReport>("scan_import", { source, path });
  }

  /**
   * Import into `folderId` (or the Notes root when null), optionally mirroring the source tree as
   * nested note folders. Progress arrives on {@link onBulkImport}.
   */
  runImport(
    source: ImportSourceId,
    path: string | null,
    folderId: string | null,
    mirrorHierarchy: boolean,
  ): Promise<ImportReport> {
    return invoke<ImportReport>("run_import", {
      source,
      path,
      folderId,
      mirrorHierarchy,
    });
  }

  /** Ask an in-flight import to stop after the current page. Already-written notes stay. */
  cancelImport(): Promise<void> {
    return invoke<void>("cancel_import");
  }

  /** Create a note-kind folder under an optional parent. Returns the new {@link NoteFolder}. */
  createNoteFolder(name: string, parentId: string | null): Promise<NoteFolder> {
    return invoke<NoteFolder>("create_note_folder", { name, parentId });
  }

  /** Rename a note-kind folder (metadata + vault subdir). */
  renameNoteFolder(id: string, name: string): Promise<void> {
    return invoke<void>("rename_note_folder", { id, name });
  }

  /** Delete a note-kind folder (its notes move to the default note-folder). */
  deleteNoteFolder(id: string): Promise<void> {
    return invoke<void>("delete_note_folder", { id });
  }

  /** Re-parent a note-kind folder (null ⇒ the Notes root). */
  moveNoteFolder(id: string, parentId: string | null): Promise<void> {
    return invoke<void>("move_note_folder", { id, parentId });
  }

  /**
   * Create a dashboard, optionally INSIDE a container.
   *
   * `folderId` is the board's container anchor and its LOCK anchor: a board with no folder
   * cannot be sealed, because there is no folder whose key would seal it. The backend refuses a
   * container that is sealed and not session-unlocked, for the same reason it refuses filing a
   * note there — a board created inside a sealed tree would be born readable.
   */
  createDashboardIn(
    title: string,
    folderId: string | null,
    emoji?: string | null,
    tint?: string | null,
  ): Promise<Dashboard> {
    return invoke<Dashboard>("create_dashboard", {
      title,
      emoji: emoji ?? null,
      tint: tint ?? null,
      folderId,
    });
  }

  /**
   * File a task into a local container, or unfile it with `containerId: null`.
   *
   * Placement is LOCAL and never egresses — it is not part of the task envelope that reaches an
   * org, so a user's private folder structure stays on the device. Sealing a container unfiles
   * the tasks in it, because a task's content lives in the org's E2EE store and a folder key
   * cannot seal it; leaving one inside would mean the lock said sealed while the task stayed as
   * readable as before.
   */
  setTaskContainer(id: string, containerId: string | null): Promise<void> {
    return invoke<void>("set_task_container", { id, containerId });
  }

  /**
   * Re-file a dashboard into a container, or unfile it with `folderId: null`.
   *
   * Refused at BOTH ends when sealed, for different reasons: a sealed TARGET would receive the
   * board in plaintext inside a tree the user believes is unreadable, and a sealed SOURCE holds
   * that board as ciphertext bound to its current container — moving the row without unsealing
   * first would carry blobs somewhere no key can open them.
   */
  moveDashboardToContainer(id: string, folderId: string | null): Promise<void> {
    return invoke<void>("move_dashboard_to_container", { id, folderId });
  }

  /**
   * ESCAPE HATCH for a folder whose master key is genuinely unrecoverable: the backend FIRST
   * proves no key can unwrap it (else it refuses with a "key was found — unlock normally" error),
   * then discards ONLY that folder's UNRECOVERABLE sealed contents (never-sealed readable content is
   * preserved) and reopens it. Destructive —
   * the FE must confirm first. Returns the reopened FolderNode.
   */
  discardUnrecoverableFolderLock(folderId: string): Promise<FolderNode> {
    return invoke<FolderNode>("discard_unrecoverable_folder_lock", {
      folderId,
    });
  }

  /**
   * Meeting-aware escape hatch (mirrors {@link unlockMeeting}): resolves the meeting's owning folder
   * and discards its lock IF the key is unrecoverable. `null` when the meeting is at the vault root
   * or its folder is already open. Refuses (rejects) if the folder is actually recoverable.
   */
  discardUnrecoverableMeetingLock(
    meetingId: string,
  ): Promise<FolderNode | null> {
    return invoke<FolderNode | null>("discard_unrecoverable_meeting_lock", {
      meetingId,
    });
  }

  // ── Update check + product info (GitHub-release update flow) ────────────

  /**
   * Check GitHub for a newer release. Resolves with an {@link UpdateInfo}; the
   * command REJECTS on network failure / rate-limit, so a thrown error means
   * "couldn't check" (never treat it as "up to date").
   */
  checkForUpdate(): Promise<UpdateInfo> {
    return invoke<UpdateInfo>("check_for_update");
  }

  /** Static product identity (name / version / description / repository) for About. */
  appInfo(): Promise<AppInfo> {
    return invoke<AppInfo>("app_info");
  }

  /** Open a GitHub release page in the user's default browser. */
  openReleasePage(url: string): Promise<void> {
    return invoke<void>("open_release_page", { url });
  }

  onStatus(cb: (payload: StatusPayload) => void): Promise<UnlistenFn> {
    return listen<StatusPayload>(EVENT_STATUS, (event) => cb(event.payload));
  }

  /** Cross-stream echo dedup removed segments after a recording (user was on speakers). */
  onEchoSuppressed(
    cb: (payload: EchoSuppressedPayload) => void,
  ): Promise<UnlistenFn> {
    return listen<EchoSuppressedPayload>(EVENT_ECHO_SUPPRESSED, (event) =>
      cb(event.payload),
    );
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

  /** Fires after an AUTO-prune removed ≥1 old recording's audio to stay under the storage cap. */
  onStoragePruned(
    cb: (p: { freedBytes: number; prunedCount: number }) => void,
  ): Promise<UnlistenFn> {
    return listen<{ freedBytes: number; prunedCount: number }>(
      EVENT_STORAGE_PRUNED,
      (e) => cb(e.payload),
    );
  }

  /**
   * Fires ONCE per recording when the 4h hard TIME cap (`MAX_RECORDING_SECONDS`)
   * is reached and the capture self-stops. The FE surfaces a notice and finalizes
   * the meeting via `stop_recording`. Length only, no PII.
   */
  onRecordingCapped(
    cb: (p: RecordingCappedPayload) => void,
  ): Promise<UnlistenFn> {
    return listen<RecordingCappedPayload>(EVENT_RECORDING_CAPPED, (e) =>
      cb(e.payload),
    );
  }

  /** A terminal capture fault occurred; the exact durable prefix can still be finalized. */
  onRecordingCaptureFault(
    cb: (p: RecordingCaptureFaultPayload) => void,
  ): Promise<UnlistenFn> {
    return listen<RecordingCaptureFaultPayload>(
      EVENT_RECORDING_CAPTURE_FAULT,
      (event) => cb(event.payload),
    );
  }

  /** System audio vanished while muted; Rust already restored the microphone. */
  onMicAutoUnmuted(cb: () => void): Promise<UnlistenFn> {
    return listen<void>(EVENT_MIC_AUTO_UNMUTED, () => cb());
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
  onAssistantTool(cb: (p: AssistantToolPayload) => void): Promise<UnlistenFn> {
    return listen<AssistantToolPayload>(EVENT_ASSISTANT_TOOL, (e) =>
      cb(e.payload),
    );
  }

  /** The CHAT panel's own live tool-trace (separate from the assistant card's). */
  onChatTool(cb: (p: AssistantToolPayload) => void): Promise<UnlistenFn> {
    return listen<AssistantToolPayload>(EVENT_CHAT_TOOL, (e) => cb(e.payload));
  }

  /**
   * The ASK page's own live tool-trace (separate from the record-screen
   * streams): one event per tool call the `ask_vault` agentic loop makes,
   * stamped with the `askThreadId` the FE passed to {@link askVault}.
   */
  onAskTool(cb: (p: AssistantToolPayload) => void): Promise<UnlistenFn> {
    return listen<AssistantToolPayload>(EVENT_ASK_TOOL, (e) => cb(e.payload));
  }

  /** Fires with progress for the in-flight Whisper transcribe-model download. */
  onModelDownload(cb: (p: ModelDownloadProgress) => void): Promise<UnlistenFn> {
    return listen<ModelDownloadProgress>(EVENT_MODEL_DOWNLOAD, (e) =>
      cb(e.payload),
    );
  }

  /**
   * Fires when the proactive-brain matcher surfaces a recall hint during a
   * recording (≤1 per cooldown window — throttled + deduped + visibility-gated
   * in the backend). IDs + a display title only, never content bodies.
   */
  onProactiveHint(cb: (p: ProactiveHintPayload) => void): Promise<UnlistenFn> {
    return listen<ProactiveHintPayload>(EVENT_PROACTIVE_HINT, (e) =>
      cb(e.payload),
    );
  }

  /** Fires with progress for an in-flight local brain-model download. */
  onBrainDownload(cb: (p: BrainDownloadProgress) => void): Promise<UnlistenFn> {
    return listen<BrainDownloadProgress>(EVENT_BRAIN_DOWNLOAD, (e) =>
      cb(e.payload),
    );
  }

  /**
   * Fires when the on-device Realtime-Reactions layer surfaces a "whisper"
   * contradiction card during a recording (far-side utterance conflicts with a
   * known fact). The `oldQuote` is an EXTRACTIVE citation of the prior fact — the
   * card never fabricates. Only emitted when the contradiction sub-toggle is on
   * (shadow mode emits nothing); visibility-gated in the backend.
   */
  onWhisperCard(cb: (p: WhisperCard) => void): Promise<UnlistenFn> {
    return listen<WhisperCard>(EVENT_WHISPER_CARD, (e) => cb(e.payload));
  }

  /** Fires with per-file progress for the in-flight embedding-model download. */
  onEmbedDownload(cb: (p: EmbedDownloadProgress) => void): Promise<UnlistenFn> {
    return listen<EmbedDownloadProgress>(EVENT_EMBED_DOWNLOAD, (e) =>
      cb(e.payload),
    );
  }

  /** Fires with COUNT-only progress for the in-flight semantic reindex backfill. */
  onReindex(cb: (p: ReindexProgress) => void): Promise<UnlistenFn> {
    return listen<ReindexProgress>(EVENT_REINDEX, (e) => cb(e.payload));
  }

  /**
   * Fires with STAGE + COUNT-only progress for the in-flight document import
   * (`importDocument`): extracting → chunking → embedding → done. Counts-only,
   * NO PII (the `documentId` is a random UUID; no filename/text). Subscribe once
   * per open Brain view and release the returned {@link UnlistenFn} on teardown.
   */
  onDocImportProgress(cb: (p: DocImportProgress) => void): Promise<UnlistenFn> {
    return listen<DocImportProgress>(EVENT_DOC_IMPORT, (e) => cb(e.payload));
  }

  /** Fires with per-page progress for an in-flight bulk import (Settings -> Imports). */
  onBulkImport(cb: (p: BulkImportProgress) => void): Promise<UnlistenFn> {
    return listen<BulkImportProgress>(EVENT_BULK_IMPORT, (e) => cb(e.payload));
  }

  /** Fires with per-file progress for the in-flight PERSON-name NER model download. */
  onNerDownload(cb: (p: NerDownloadProgress) => void): Promise<UnlistenFn> {
    return listen<NerDownloadProgress>(EVENT_NER_DOWNLOAD, (e) =>
      cb(e.payload),
    );
  }

  /**
   * Fires after the background org-sync loop INGESTED/TOMBSTONED ≥1 org (Shared
   * Brain) item this tick — i.e. the local org replica actually changed. Lets an
   * open view (the Notes org picker + merged All-notes list, the Settings
   * shared-brain list) re-fetch its org items WITHOUT polling. Content-free — a
   * count only, NO item ids / titles / content; treat any arrival as "re-fetch
   * the org lists". Mirrors the {@link onBriefProposed} shape.
   */
  onOrgFeedUpdated(
    cb: (p: OrgFeedUpdatedPayload) => void,
  ): Promise<UnlistenFn> {
    return listen<OrgFeedUpdatedPayload>(EVENT_ORG_FEED_UPDATED, (e) =>
      cb(e.payload),
    );
  }

  /**
   * Fires once a note/meeting delete has FULLY succeeded on the backend
   * (delete-fan-out fix). Content-free — id + kind only; a subscriber prunes
   * its OWN state for that id (the tab-strip closes a matching tab; a list
   * store removes a matching row as a safety net for the case where the
   * delete happened from a different surface than the one holding it).
   */
  onContentDeleted(
    cb: (p: ContentDeletedPayload) => void,
  ): Promise<UnlistenFn> {
    return listen<ContentDeletedPayload>(EVENT_CONTENT_DELETED, (e) =>
      cb(e.payload),
    );
  }

  /** Count-only reminder invalidation. Canonical rows are always refetched. */
  onRemindersUpdated(
    cb: (p: RemindersUpdatedPayload) => void,
  ): Promise<UnlistenFn> {
    return listen<RemindersUpdatedPayload>(EVENT_REMINDERS_UPDATED, (e) =>
      cb(e.payload),
    );
  }

  /**
   * One canonical Smart-audit source changed. The event carries no title,
   * content, hash, or revision; the mounted card filters by opaque identity and
   * re-enters `auditReminderSuggestions`, whose lock gate remains authoritative.
   */
  onReminderSourceUpdated(
    cb: (p: ReminderSourceUpdatedPayload) => void,
  ): Promise<UnlistenFn> {
    return listen<ReminderSourceUpdatedPayload>(
      EVENT_REMINDER_SOURCE_UPDATED,
      (e) => cb(e.payload),
    );
  }

  /**
   * Lock authority changed. The event intentionally carries no ids or content:
   * consumers synchronously purge all cached source metadata, then re-fetch
   * through the ordinary lock-gated commands.
   */
  onReminderVisibilityInvalidated(cb: () => void): Promise<UnlistenFn> {
    return listen<void>(EVENT_REMINDER_VISIBILITY_INVALIDATED, () => cb());
  }

  /**
   * A lock, move, or delete invalidated durable Ask history. The event carries
   * no content; mounted consumers synchronously drop all conversation state.
   */
  onAskHistoryInvalidated(cb: () => void): Promise<UnlistenFn> {
    return listen<void>(EVENT_ASK_HISTORY_INVALIDATED, () => cb());
  }

  // ── Shared containers ──────────────────────────────────────────────────────

  /**
   * What sharing this Workspace or Folder would publish — counts only, no egress.
   * Refuses a SEALED container: its content is not readable, and letting the
   * user "share" it and see nothing arrive would be a silent failure.
   */
  previewContainerShare(
    orgId: string,
    folderId: string,
  ): Promise<ContainerSharePreview> {
    return invoke<ContainerSharePreview>("preview_container_share", {
      orgId,
      folderId,
    });
  }

  /**
   * Publish a whole Workspace or Folder to an Org: every manifest, then every
   * eligible document, all under one inherited `access`. Emits
   * {@link onContainerShareProgress} after each item.
   */
  shareContainerToOrg(
    orgId: string,
    folderId: string,
    access: OrgAccess,
    scrub: boolean,
  ): Promise<ContainerShareResult> {
    return invoke<ContainerShareResult>("share_container_to_org", {
      orgId,
      folderId,
      access,
      scrub,
    });
  }

  /**
   * Stop sharing a container. Withdraws every document the container itself
   * published; a note the user shared deliberately keeps its own share and
   * merely loses its placement.
   */
  unshareContainer(orgId: string, folderId: string): Promise<void> {
    return invoke<void>("unshare_container", { orgId, folderId });
  }

  /** Re-permission a container AND every document filed under it. */
  setContainerShareAccess(
    orgId: string,
    folderId: string,
    access: OrgAccess,
  ): Promise<void> {
    return invoke<void>("set_container_share_access", {
      orgId,
      folderId,
      access,
    });
  }

  /**
   * Items this device publishes to an org on their own (not via a container).
   * Read-gated: a sealed source discloses no share status.
   */
  listOrgShareTargets(): Promise<OrgShareTargetRow[]> {
    return invoke<OrgShareTargetRow[]>("list_org_share_targets");
  }

  /** Every container THIS device publishes — drives the sidebar's shared marker. */
  listContainerShareStatus(): Promise<ContainerShareStatus[]> {
    return invoke<ContainerShareStatus[]>("list_container_share_status");
  }

  /**
   * Bring every shared container back in line with the local tree, returning the
   * number of mutations. Call it after a workspace mutation so a note added to a
   * shared folder publishes right away; a background tick is the safety net.
   */
  syncContainerShares(): Promise<number> {
    return invoke<number>("sync_container_shares");
  }

  /** The forest of containers and items other members shared with this user. */
  listSharedWorkspace(): Promise<SharedWorkspace> {
    return invoke<SharedWorkspace>("list_shared_workspace");
  }

  /**
   * File a received container or document somewhere in this user's own tree.
   * DEVICE-LOCAL: nothing is published, the owner sees nothing, and the content
   * keeps updating from the org feed exactly as before.
   */
  setSharedPlacement(
    orgId: string,
    targetKind: SharedPlacementTarget,
    targetId: string,
    localParentId: string | null,
    position: number,
  ): Promise<void> {
    return invoke<void>("set_shared_placement", {
      orgId,
      targetKind,
      targetId,
      localParentId,
      position,
    });
  }

  /** Return a received object to wherever its owner filed it. */
  clearSharedPlacement(
    orgId: string,
    targetKind: SharedPlacementTarget,
    targetId: string,
  ): Promise<void> {
    return invoke<void>("clear_shared_placement", {
      orgId,
      targetKind,
      targetId,
    });
  }

  /**
   * Progress of a running container share. Counts only — never a folder name or
   * an item id.
   */
  onContainerShareProgress(
    cb: (done: number, total: number) => void,
  ): Promise<UnlistenFn> {
    return listen<{ done: number; total: number }>(
      EVENT_CONTAINER_SHARE_PROGRESS,
      (e) => cb(e.payload.done, e.payload.total),
    );
  }
}
