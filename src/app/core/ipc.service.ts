import { Injectable } from "@angular/core";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  AccountStatus,
  ActionItem,
  AiMapRow,
  Analytics,
  AppConfigDto,
  ContextHit,
  MyShareEntry,
  RecipientPreview,
  ShareToUserResult,
  ShareInboxItem,
  AcceptedShare,
  BrainDownloadProgress,
  BrainModelDto,
  Posture,
  RetiredModelNudge,
  WhisperCard,
  EmbedDownloadProgress,
  EgressLedger,
  GatewayHealth,
  GatewayModel,
  ReindexProgress,
  ReindexResult,
  InputDeviceInfo,
  AskVaultResult,
  AssistantThreadRow,
  BrainOverview,
  BuiltinRecipe,
  CalendarEvent,
  CalendarEventFull,
  CalendarContext,
  ChatTurn,
  DigestResult,
  DocumentInfo,
  DossierData,
  EntityDetail,
  Folder,
  FolderNode,
  GraphData,
  GraphPayload,
  Meeting,
  MeetingDetail,
  MeetingTimeline,
  SpeakerSuggestion,
  VoiceprintInfo,
  ModelDownloadProgress,
  NerDownloadProgress,
  NoteDto,
  NoteSummary,
  NoteDoc,
  NoteFolder,
  NoteAssistRequest,
  NoteAssistResult,
  OrganizePlan,
  PersonCard,
  PinResult,
  PruneSummary,
  StorageReport,
  SupersessionDto,
  ApplyResult,
  UserMemory,
  BriefSchedule,
  BriefRun,
  BriefProposedPayload,
  VerifyFindingDto,
  ProviderStatus,
  SavedRecipe,
  SearchHit,
  StartResult,
  StatusPayload,
  EchoSuppressedPayload,
  RecordingCappedPayload,
  StopResult,
  TopicThread,
  VoiceActionResultPayload,
  VoiceCommandListeningPayload,
  VoiceCommandProcessingPayload,
  AssistantToolPayload,
  ChatMsg,
  ProactiveHintPayload,
  WakeDetectedPayload,
  UpdateInfo,
  AppInfo,
  OrgStatus,
  OrgMember,
  OrgSharePreview,
  OrgShareEntry,
  OrgItemDetail,
  OrgSyncReport,
  ActiveSharesReport,
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
// Phase D — on-device PERSON-name NER (redaction) model download progress stream.
export const EVENT_NER_DOWNLOAD = "murmur://ner-download";
// Recording-storage: an AUTO-prune freed ≥1 old recording's audio to stay under the cap.
export const EVENT_STORAGE_PRUNED = "murmur://storage-pruned";
// Recording hit the 4h hard TIME cap and self-stopped (distinct from the byte-based prune).
export const EVENT_RECORDING_CAPPED = "murmur://recording-capped";
// Brain v2 L5 — a scheduled brief was STAGED (propose-accept; run id + label + size only).
export const EVENT_BRIEF_PROPOSED = "murmur://brief-proposed";

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
    return invoke<NoteDto>("apply_note_verify_markers", { meetingId, findings });
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
    return invoke<string | null>("account_signup", { email, code, password, saveRecovery });
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

  /** Invite a member by email (owner only). Idempotent on an already-invited address. */
  orgInviteMember(email: string): Promise<void> {
    return invoke<void>("org_invite_member", { email });
  }

  /** List the org's members (owner sees emails to manage; drives the member list). */
  orgListMembers(): Promise<OrgMember[]> {
    return invoke<OrgMember[]>("org_list_members");
  }

  /** Remove a member (owner only) — drives the OCK generation rotation server-side. */
  orgRemoveMember(userId: string): Promise<void> {
    return invoke<void>("org_remove_member", { userId });
  }

  /** Leave the org (self-removal). The local org replica is dropped. */
  orgLeave(): Promise<void> {
    return invoke<void>("org_leave");
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
  previewOrgShare(
    args: { meetingId?: string; documentId?: string; scrub: boolean },
  ): Promise<OrgSharePreview> {
    return invoke<OrgSharePreview>("preview_org_share", {
      meetingId: args.meetingId ?? null,
      documentId: args.documentId ?? null,
      scrub: args.scrub,
    });
  }

  /**
   * Publish a MEETING to the org brain (seal under the OCK + upload). Gate order
   * backend-side: unlocked → consent → clean → scrub → seal → upload → ledger.
   * Refuses (`Locked`) a sealed meeting; requires org membership + consent.
   */
  shareMeetingToOrg(meetingId: string, scrub: boolean): Promise<void> {
    return invoke<void>("share_meeting_to_org", { meetingId, scrub });
  }

  /** Publish an authored NOTE to the org brain. Same gated pipeline as the meeting path. */
  shareDocumentToOrg(documentId: string, scrub: boolean): Promise<void> {
    return invoke<void>("share_document_to_org", { documentId, scrub });
  }

  /** This user's outgoing org shares (drives the "In Org Brain" state + per-item revoke). */
  listOrgShares(): Promise<OrgShareEntry[]> {
    return invoke<OrgShareEntry[]>("list_org_shares");
  }

  /** Revoke an org share: tombstone the feed item + drop the local ciphertext. Idempotent. */
  revokeOrgShare(itemId: string): Promise<void> {
    return invoke<void>("revoke_org_share", { itemId });
  }

  /** Manually pull + ingest the org feed now → the {@link OrgSyncReport} (counts + errors only). */
  orgSyncNow(): Promise<OrgSyncReport> {
    return invoke<OrgSyncReport>("org_sync_now");
  }

  /** The full decrypted org item for the read-only viewer route. */
  orgGetItem(itemId: string): Promise<OrgItemDetail> {
    return invoke<OrgItemDetail>("org_get_item", { itemId });
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
  listModels(connection: string): Promise<string[]> {
    return invoke<string[]>("list_models", { connection });
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

  /**
   * The `/people` personal-CRM list: one {@link PersonCard} per VISIBLE Person
   * entity (name + last-talked + open-commitment / fact counts), rolled up over
   * the SAME gated graph/facts/commitment readers as {@link getGraph}. GATED
   * server-side — a person whose mentions/facts/commitments live solely in
   * sealed-not-unlocked meetings never appears, and every count reflects visible
   * sources only — so re-fetch on a FoldersService lock-state change to shift the
   * list live (mirrors {@link getGraph}). Each `id` links to the entity detail.
   */
  listPeople(): Promise<PersonCard[]> {
    return invoke<PersonCard[]>("list_people");
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
  askVault(
    question: string,
    history: ChatTurn[],
    askThreadId?: string,
  ): Promise<AskVaultResult> {
    return invoke<AskVaultResult>("ask_vault", {
      question,
      history,
      askThreadId,
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
  ): Promise<VoiceActionResultPayload> {
    return invoke<VoiceActionResultPayload>("ask_assistant_chat", {
      messages,
      threadId,
      anchorText,
      meetingId,
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
   * The selection Brain-assistant action: refine / shorten (replace the selection)
   * or enhance (retrieve related brain context + propose an ADDITIVE passage with
   * citations). Routes via `provider_for(Role::Notes)` (local Qwen vs cloud Claude
   * per posture, redaction firewall + egress ledger for free). The result carries
   * the resolved `modelLabel`/`mode`/`redacted` for the popover mode chip.
   */
  noteAssistantAction(req: NoteAssistRequest): Promise<NoteAssistResult> {
    return invoke<NoteAssistResult>("note_assistant_action", { req });
  }

  /**
   * Propose per-note folder assignments by content (auto-organize step 1). Returns
   * an {@link OrganizePlan} of moves with reasons; `folderId` (null ⇒ all notes)
   * scopes the run. Non-destructive — nothing moves until {@link applyOrganizePlan}.
   */
  planOrganizeNotes(folderId: string | null): Promise<OrganizePlan> {
    return invoke<OrganizePlan>("plan_organize_notes", { folderId });
  }

  /**
   * Apply an auto-organize plan (step 2): create the needed note-folders + move the
   * notes (gated; re-exports). Confirm-before-apply on the FE.
   */
  applyOrganizePlan(plan: OrganizePlan): Promise<void> {
    return invoke<void>("apply_organize_plan", { plan });
  }

  /** The note-kind folder list (`kind='note'` only). */
  listNoteFolders(): Promise<NoteFolder[]> {
    return invoke<NoteFolder[]>("list_note_folders");
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
   * ESCAPE HATCH for a folder whose master key is genuinely unrecoverable: the backend FIRST
   * proves no key can unwrap it (else it refuses with a "key was found — unlock normally" error),
   * then discards ONLY that folder's UNRECOVERABLE sealed contents (never-sealed readable content is
   * preserved) and reopens it. Destructive —
   * the FE must confirm first. Returns the reopened FolderNode.
   */
  discardUnrecoverableFolderLock(folderId: string): Promise<FolderNode> {
    return invoke<FolderNode>("discard_unrecoverable_folder_lock", { folderId });
  }

  /**
   * Meeting-aware escape hatch (mirrors {@link unlockMeeting}): resolves the meeting's owning folder
   * and discards its lock IF the key is unrecoverable. `null` when the meeting is at the vault root
   * or its folder is already open. Refuses (rejects) if the folder is actually recoverable.
   */
  discardUnrecoverableMeetingLock(meetingId: string): Promise<FolderNode | null> {
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

  /**
   * The ASK page's own live tool-trace (separate from the record-screen
   * streams): one event per tool call the `ask_vault` agentic loop makes,
   * stamped with the `askThreadId` the FE passed to {@link askVault}.
   */
  onAskTool(cb: (p: AssistantToolPayload) => void): Promise<UnlistenFn> {
    return listen<AssistantToolPayload>(EVENT_ASK_TOOL, (e) => cb(e.payload));
  }

  /** Fires with progress for the in-flight Whisper transcribe-model download. */
  onModelDownload(
    cb: (p: ModelDownloadProgress) => void,
  ): Promise<UnlistenFn> {
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
  onBrainDownload(
    cb: (p: BrainDownloadProgress) => void,
  ): Promise<UnlistenFn> {
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

  /** Fires with per-file progress for the in-flight PERSON-name NER model download. */
  onNerDownload(cb: (p: NerDownloadProgress) => void): Promise<UnlistenFn> {
    return listen<NerDownloadProgress>(EVENT_NER_DOWNLOAD, (e) =>
      cb(e.payload),
    );
  }
}
