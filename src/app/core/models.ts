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

/** Payload of murmur://echo-suppressed — counts only, no content. */
export interface EchoSuppressedPayload {
  suppressed: number;
  meetingId: string;
}

/**
 * Payload of murmur://recording-capped — the 4h `MAX_RECORDING_SECONDS` hard
 * TIME cap was reached and the capture self-stopped. Length only, NO PII (no
 * content, meeting id, or path). Fires once per recording (rising edge).
 */
export interface RecordingCappedPayload {
  limitSeconds: number;
}

/**
 * GitHub-release update check (`check_for_update`). Mirrors the Rust `UpdateInfo`
 * (serde camelCase). `updateAvailable` is the sole "should we nudge" flag;
 * `releaseName` / `releaseNotes` are null when GitHub omits them. The command
 * REJECTS (throws) on network failure / rate-limit — a thrown error means
 * "couldn't check", not "up to date".
 */
export interface UpdateInfo {
  currentVersion: string;
  latestVersion: string;
  updateAvailable: boolean;
  releaseUrl: string;
  releaseName: string | null;
  releaseNotes: string | null;
}

/** Static product identity for the Settings "About" section (`app_info`). */
export interface AppInfo {
  name: string;
  version: string;
  description: string;
  repository: string;
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
  /**
   * Speaker voiceprints — recognise the SAME diarized speaker across meetings,
   * fully on-device. OPT-IN, default false: captures a voice fingerprint of remote
   * participants (never egressed, but a biometric nonetheless). Settable and
   * round-tripped on every `save_config` like `diarizeOthers`; an omitted key
   * serde-defaults to false in the backend, so the FE MUST always send it or a
   * normal save silently disables it. Mirrors Rust `AppConfigDto.voiceprint_enabled`.
   */
  voiceprintEnabled: boolean;
  aecEnabled: boolean;
  postAecEnabled: boolean;
  /** Recording-storage cap in GB (`null` = no cap). Mirrors Rust `audio_storage_limit_gb`. */
  audioStorageLimitGb: number | null;
  /** Auto-delete oldest recordings' audio over the cap. Opt-in, default false. Mirrors Rust `audio_auto_prune`. */
  audioAutoPrune: boolean;
  modelSize: string;
  /**
   * OPTIONAL live-caption ASR engine (mirrors Rust `live_asr_engine`): `"whisper"` (default) or
   * `"parakeet"`. When `"parakeet"` AND its models are downloaded, live captions decode on the
   * CPU-only NVIDIA parakeet engine (off the Metal GPU); whisper stays the batch authority and the
   * fallback. Settable from the Transcription settings toggle.
   */
  liveAsrEngine: string;
  /**
   * Brain-sidecar IDLE-KILL window in seconds (mirrors Rust `brain_idle_timeout_secs`, default
   * 300): after this long with no on-device brain request, the host kills the `meetnotes-brain`
   * child to reclaim ALL its model RAM to the OS.
   */
  brainIdleTimeoutSecs: number;
  /**
   * Brain-sidecar READY-handshake timeout in seconds (mirrors Rust `brain_ready_timeout_secs`,
   * default 90): the bounded wait for the child's `Ready` after spawn (model load can be slow). On
   * timeout the brain call degrades to Cloud/floor rather than blocking.
   */
  brainReadyTimeoutSecs: number;
  /**
   * Brain-sidecar HARD per-generation cap in seconds (mirrors Rust `brain_hard_cap_secs`, default
   * 180) for a call with no explicit timeout: a wedged child is killed + respawned at this cap so it
   * can never hold model RAM forever.
   */
  brainHardCapSecs: number;
  voiceTrigger: boolean;
  onboarded: boolean;
  /**
   * First-run SHARING decision latch (mirrors Rust `AppConfigDto.sharing_choice_made`).
   * `false` (default) means the user has NOT yet resolved the first-run sharing
   * choice — the init gateway (`/welcome`) is shown when this is false AND no
   * account is logged in (`!sharingChoiceMade && !accountStatus.loggedIn`).
   * Picking "use locally" OR completing the account flow flips it true via the
   * dedicated `mark_sharing_choice_made` command, so the gateway never nags again
   * (a one-way latch). PRESERVE-ONLY on `saveConfig` — a normal settings save can
   * never set or clear it (the backend's `dto_to_config` carries the stored value
   * back), exactly like `shareEgressConsented`. `onboarding.persistConfig()` must
   * still round-trip it (`base?.sharingChoiceMade ?? false`) since all DTO fields
   * are required. Display-only otherwise.
   */
  sharingChoiceMade: boolean;
  noteStyle: string;
  /** ENHANCE-MY-NOTES: how typed in-meeting notes shape the summary — "enhance" (they
   *  become the skeleton of the note) | "append" (verbatim `## My notes` section). */
  notesMode: string;
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
   * `"bielik-11b"`). Only meaningful when `brainBackend === "local"`. Null =
   * none selected yet. A custom GGUF FILE PATH goes in `brainModelPath` (below),
   * NOT here — the backend validates this against the fixed registry and
   * discards a typed non-registry value.
   */
  brainModelId: string | null;
  /**
   * An explicit custom GGUF FILE PATH the resolver honors verbatim (bypasses the
   * `brainModelId` registry validation). Settable and round-tripped on every
   * `save_config`; empty → null. When set it WINS over `brainModelId` in the
   * backend's `resolve_brain_model`, so the two are mutually exclusive from the
   * UI (picking a registry model clears the path; typing a path clears the id).
   * Mirrors Rust `AppConfigDto.brain_model_path`.
   */
  brainModelPath: string | null;
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
  /**
   * brain2 connectors (Phase 2) — the JIRA connector MASTER toggle. NEW CLOUD EGRESS:
   * when on (AND `jiraConsented` AND a base URL + email + API token are configured),
   * the brain/Ask answer may send a REDACTED query off-device to the user's Jira Cloud
   * site. A settable flag, round-tripped on every `save_config`. Default false.
   * Mirrors Rust `AppConfigDto.jira_enabled`.
   */
  jiraEnabled: boolean;
  /**
   * brain2 connectors — one-time consent for the Jira egress. Like `webSearchConsented`,
   * PRESERVE-ONLY on `save_config` (a normal save carries the current value back, never
   * flips it) and granted SOLELY by the dedicated `consent_to_jira` command. Default
   * false (fail-closed). Mirrors Rust `AppConfigDto.jira_consented`.
   */
  jiraConsented: boolean;
  /**
   * The Jira Cloud site base URL, e.g. `https://acme.atlassian.net` (non-secret).
   * Settable, round-tripped on `save_config`. Default "". Mirrors Rust `AppConfigDto.jira_base_url`.
   */
  jiraBaseUrl: string;
  /**
   * The Atlassian account email paired with the API token for Basic auth (non-secret).
   * Settable, round-tripped on `save_config`. Default "". Mirrors Rust `AppConfigDto.jira_email`.
   */
  jiraEmail: string;
  /**
   * brain2 connectors (Phase 3) — the SLACK connector MASTER toggle. NEW CLOUD EGRESS:
   * when on (AND `slackConsented` AND a user token is configured), the brain/Ask answer
   * may send a REDACTED query off-device to the user's Slack workspace. A settable flag,
   * round-tripped on every `save_config`. Default false. Mirrors Rust `AppConfigDto.slack_enabled`.
   */
  slackEnabled: boolean;
  /**
   * brain2 connectors — one-time consent for the Slack egress. Like `jiraConsented`,
   * PRESERVE-ONLY on `save_config` (a normal save carries the current value back, never
   * flips it) and granted SOLELY by the dedicated `consent_to_slack` command. Default
   * false (fail-closed). Mirrors Rust `AppConfigDto.slack_consented`.
   */
  slackConsented: boolean;
  /**
   * Opt-in: pass the shell environment through to the `claude` CLI subprocess, so an env
   * `ANTHROPIC_API_KEY` (and proxy / base-url vars) reach it again — restores how older versions
   * authenticated the CLI before the env-hardening. Settable flag, round-tripped on `save_config`.
   * Default false = the hardened, env-cleared run. Even when on, the DB encryption keys are never
   * inherited. Mirrors Rust `AppConfigDto.claude_code_inherit_env`.
   */
  claudeCodeInheritEnv: boolean;
  /**
   * AI Gateway (Phase 1) — base URL of the user's OpenAI-compatible gateway
   * (LiteLLM / Kong / Portkey / vLLM / …). Default `""` (unset). Required
   * when `providerId === "gateway"`. Mirrors Rust `AppConfigDto.gateway_base_url`
   * (camelCase via `#[serde(rename_all = "camelCase")]`).
   */
  gatewayBaseUrl: string;
  /**
   * AI Gateway (Phase 1) — model id forwarded to the gateway (e.g. `"gpt-4o"`,
   * `"mistral/mistral-7b"`). An empty string lets the gateway use its own default.
   * Mirrors Rust `AppConfigDto.gateway_model`.
   */
  gatewayModel: string;
  /**
   * M3-CLIENT — base URL of the Murmur sharing server (self-host or hosted).
   * Default `""` (unset ⇒ account/share commands fail closed). Mirrors Rust
   * `AppConfigDto.share_base_url`. Validated like `gatewayBaseUrl`.
   */
  shareBaseUrl: string;
  /**
   * M3-CLIENT — one-time SHARE-egress consent. DISPLAY-ONLY on this DTO (like
   * `cloudEgressConsented`): carried OUT so the FE can show consent status, but
   * PRESERVE-ONLY on `saveConfig` — mutated only by `consentToShareEgress` /
   * `revokeShareEgress`. Mirrors Rust `AppConfigDto.share_egress_consented`.
   */
  shareEgressConsented: boolean;
  /**
   * Proactive brain (P2) — the GLOBAL MUTE for in-meeting recall hints
   * (`EVENT_PROACTIVE_HINT` cards). Off silences the event source in the
   * BACKEND (the live-loop matcher never runs), and the FE additionally never
   * renders a card (belt and braces). Settable flag, round-tripped on every
   * `save_config` like the other flags. Default TRUE (conservative thresholds).
   * Mirrors Rust `AppConfigDto.proactive_hints_enabled`.
   */
  proactiveHintsEnabled: boolean;
  /**
   * Cross-meeting USER MEMORY master gate. Off turns memory off ENTIRELY in the
   * BACKEND: no user-fact extraction after a meeting, no memory brief injected
   * into any surface (the @brain loop, Ask, per-meeting chat), and
   * `getUserMemory()` reports `disabled`. Existing facts are NOT deleted by
   * flipping it off (the user forgets/clears them). Settable flag, round-tripped
   * on every `save_config`. Default TRUE. Mirrors Rust
   * `AppConfigDto.user_memory_enabled`.
   */
  userMemoryEnabled: boolean;
  /**
   * Stage 4 — per-feature model-role overrides (Notes / Ask / Live), mirroring
   * Rust `AppConfigDto.role_*` (camelCase). `""` = inherit: the role follows
   * the legacy mapping (Notes → the Default AI triple; Ask/Live → the
   * `brainBackend` fallback). The CONNECTION key is the override switch — a
   * lone model/effort with an empty connection is ignored by the backend
   * resolver. All nine are settable and MUST ride every `save_config` payload
   * (like the Stage E flags above) so a save never clears a role override.
   */
  roleNotesConnection: string;
  roleNotesModel: string;
  roleNotesEffort: string;
  roleAskConnection: string;
  roleAskModel: string;
  roleAskEffort: string;
  roleLiveConnection: string;
  roleLiveModel: string;
  roleLiveEffort: string;
  /**
   * Notes feature — the three in-note selection-assistant actions (Refine ·
   * Shorten · Enhance context), each independently toggleable. All default TRUE
   * (mirrors Rust `AppConfigDto.note_assist_refine` / `note_assist_shorten` /
   * `note_assist_enhance`, camelCase). Round-tripped on every `save_config` like
   * the other flags. OPTIONAL on the DTO so the Settings UI works before the
   * backend fields land — the FE treats an absent value as TRUE.
   */
  noteAssistRefine?: boolean;
  noteAssistShorten?: boolean;
  noteAssistEnhance?: boolean;
  /**
   * Notes feature — ids of the NEW (post-refine/shorten/enhance) selection-assistant
   * actions the user has turned OFF. Scales to any number of actions without one
   * column each. Enabled check: refine/shorten/enhance follow their bools above;
   * every other action id is enabled unless it appears here; `custom` is always
   * enabled (the escape hatch). OPTIONAL — an absent value means nothing is off
   * (mirrors Rust `AppConfigDto.note_assist_actions_off`, camelCase). Round-tripped
   * on every `save_config` like the other flags.
   */
  noteAssistActionsOff?: string[];
}

/** Phase H — which backend powers the brain / in-meeting voice assistant. */
export type BrainBackend = "cloud" | "local" | "off";

/**
 * The Murmur Brain engine CLASS a registry model serves (mirrors the Rust
 * `ModelClass`, serialized lowercase). `"light"` = fast, small-context work run
 * DURING a recording (realtime reactions / fact-triple extraction); `"heavy"` =
 * note/Ask summarization + post-call analysis. Lets the picker group by role and
 * the Brain-Live card pick the smallest LIGHT model.
 */
export type ModelClass = "light" | "heavy";

/**
 * Phase H — a selectable local brain model from the registry (`list_brain_models`).
 * Mirrors the Rust `BrainModelDto` (camelCase). RAM-fit / download / selected
 * state are computed by the backend against this Mac.
 */
export interface BrainModelDto {
  id: string;
  name: string;
  /** On-disk filename inside the shared models dir. */
  filename: string;
  /** Hugging Face raw-file URL (inbound-only — never sent meeting content). */
  url: string;
  /** Approximate download / on-disk size in bytes (for the picker size label). */
  approxSizeBytes: number;
  /** Minimum recommended RAM in GB to run this model alone. */
  minRamGb: number;
  /** Languages the model handles well (e.g. ["pl", "en"]). */
  languages: string[];
  /** mistral.rs architecture key (`llama` / `qwen2` / `qwen3`). */
  arch: string;
  /** The engine class (`light` / `heavy`) — lets the FE group the picker by role. */
  class: ModelClass;
  /** Already downloaded on this Mac. */
  downloaded: boolean;
  /** Fits in this Mac's RAM (false → warn / discourage). */
  fitsRam: boolean;
  /** Currently the selected brain model (the single last-selected `brain_model_id`, any class). */
  selected: boolean;
  /** The EFFECTIVE light-class model (explicit `brain_light_model_id`, else registry default) — what
   * realtime reactions run on. Reflects the true per-class choice, not just the single `selected`. */
  selectedLight: boolean;
  /** The EFFECTIVE heavy-class model (explicit `brain_heavy_model_id`, else registry default) — what
   * local Notes/Ask run on. Drives the effort slider's position. */
  selectedHeavy: boolean;
}

/**
 * The DERIVED Murmur Brain posture for the Settings display (`brain_posture`),
 * NEVER stored — the backend computes it from the live config so the label can
 * never lie about egress. `"cloud"` (Default AI writes everything, no on-device
 * reactions) | `"hybrid"` (cloud notes/answers + LOCAL realtime reactions — the
 * ⭐ recommendation) | `"fully_local"` (nothing leaves the device) | `"custom"`
 * (a hand-tuned combination matching no preset). Only the first three are
 * SETTABLE via `set_brain_posture`; `"custom"` is a read-only display state.
 */
export type Posture = "cloud" | "hybrid" | "fully_local" | "custom";

/** One row of the Settings "What runs where" map (mirrors Rust `AiMapRow`, camelCase). */
export interface AiMapRow {
  job: string;
  title: string;
  engine: string;
  model: string;
  onDevice: boolean;
  redacted: boolean;
  active: boolean;
  routable: boolean;
}

/**
 * The installed-base migration nudge (`brain_model_retirement_nudge`): non-null
 * when the persisted `brainModelId` points at a RETIRED model (the non-commercial
 * `qwen2.5-3b`), telling the FE to offer the Apache-licensed replacement. Mirrors
 * the Rust `RetiredModelNudge` (camelCase). The retired GGUF keeps working until
 * the user switches — nothing changes silently.
 */
export interface RetiredModelNudge {
  retiredId: string;
  replacementId: string;
  replacementName: string;
  reason: string;
  /** The retired GGUF is still on disk (so deletion could be offered). */
  fileOnDisk: boolean;
}

/**
 * Realtime Reactions — one "whisper" contradiction card (`EVENT_WHISPER_CARD`).
 * Mirrors the Rust `WhisperCard` (camelCase). EPHEMERAL (emitted as an event,
 * never persisted). Surfaced to the user ALONE during a recording when a far-side
 * utterance contradicts a fact already in their history. `oldQuote` is the
 * EXTRACTIVE citation — a real prior fact value, never model-generated — so the
 * card can never fabricate an accusation. `sourceMeetingId` (when set) is the
 * `[[wikilink]]` / click-through to the meeting the old fact came from.
 *
 * PRIVACY (lock-model): a whisper card that already crossed to the FE cites a
 * meeting that may be re-sealed mid-session (screen-share auto-relock / Lock all)
 * — the reactions rail therefore PURGES every card on a lock transition, the FE
 * analogue of the `convertFileSrc` gate. See {@link MeetingConversationStore}.
 */
export interface WhisperCard {
  kind: "contradiction";
  /** Neutral one-line framing ("Earlier, X said Y") — never accusatory. */
  summary: string;
  /** The old fact's value — the extractive citation. */
  oldQuote: string;
  /** The entity (subject) the fact is about. */
  entity: string;
  /** The attribute that changed. */
  predicate: string;
  /** The source meeting to open ([[wikilink]] / click-through), when known. */
  sourceMeetingId: string | null;
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
 * Progress for the Whisper transcribe-model (GGML) download (`EVENT_MODEL_DOWNLOAD`).
 * Mirrors the backend `ModelDownloadPayload`. `total` is null when the server omits
 * Content-Length; `done` fires once the file is written + renamed into place. The
 * backend downloads one model at a time, so the component tracks WHICH size it started
 * locally (errors surface via the download command's promise).
 */
export interface ModelDownloadProgress {
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
 * Phase 5 — which BRAIN CASCADE tier answered an in-meeting @brain turn. Set
 * DETERMINISTICALLY by the backend ladder (never string-sniffed): the current
 * meeting in isolation, the user's vault, or the connectors/web. Mirrors the
 * backend `AnsweredFrom` enum (`voice_action.rs`, serialized `snake_case`).
 * Absent/null when the turn didn't run through the cascade (the deterministic
 * floor, an error, or the vault-wide Ask page).
 */
export type AnsweredFrom = "current_meeting" | "vault" | "connectors";

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
  /**
   * The agent's PROPOSED note draft, or `null`. NON-null ONLY when the model
   * decided the user asked it to MAKE/SAVE a note (it called the `propose_note`
   * tool) — for a plain answer/question it is `null`. The FE shows the quiet
   * "✓ Add to notes" affordance ONLY when this is non-null, and on accept appends
   * THIS draft (not the whole reply) to the user's notes. The agent never
   * auto-writes; accept is the only path content enters the notes.
   */
  proposedNote: string | null;
  /**
   * The persistent thread this result belongs to, when the backend stamps one
   * (a turn initiated with a `threadId`, or one the backend generated). Absent /
   * null on an older backend — the FE then falls back to its voice-target
   * routing. Lets the FE resolve the RIGHT thread when several are in flight.
   */
  threadId?: string | null;
  /**
   * Phase 5 — which BRAIN CASCADE tier answered (current meeting / vault /
   * connectors), set deterministically by the backend ladder. Absent/null when
   * the turn did not run through the cascade. Drives the visible tier chip.
   */
  answeredFrom?: AnsweredFrom | null;
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

/**
 * Fired once per TOOL CALL the in-meeting brain makes during an agentic turn
 * (`EVENT_ASSISTANT_TOOL`), so the assistant card can render the live tool-trace
 * chips ("Searching notes… ✓", "Checking the web…"). NO PII — the tool NAME +
 * a coarse result-size count only (never args, results, or content). The same
 * shape rides the chat panel's `EVENT_CHAT_TOOL` and the Ask page's
 * `EVENT_ASK_TOOL` streams (each surface subscribes to its OWN stream).
 */
export interface AssistantToolPayload {
  /** The tool name (search_meetings / search_semantic / web_search / calendar_lookup / …). */
  tool: string;
  /** "running" when the call starts, "done" when it finishes. */
  state: "running" | "done";
  /** False when the tool call errored (the chip shows a muted state). */
  ok: boolean;
  /** Coarse result-size signal for the "✓ N" badge — never the content. */
  count: number | null;
  /**
   * The thread whose agentic turn made this tool call, when the backend stamps
   * one. Absent / null on an older backend — the FE then falls back to the
   * most-recently-opened pending turn. Fixes cross-attribution when two
   * threads are in flight simultaneously.
   */
  threadId?: string | null;
}

/** Proactive brain — what a recall hint points at (drives the card's icon + label). */
export type ProactiveHintKind = "past_meeting" | "open_commitment" | "fact";

/**
 * Proactive brain (P2) — one zero-egress recall hint surfaced by the live-loop
 * matcher (`EVENT_PROACTIVE_HINT`). IDs + a display title only — never content
 * bodies (the matcher reads only visibility-gated sources, and the payload
 * carries no more than a card must show). The backend enforces the throttle
 * (≤1 per 120 s cooldown + session dedup by kind/targetId + a relevance
 * threshold) — the FE renders at most ONE card, newest replacing the previous.
 */
export interface ProactiveHintPayload {
  kind: ProactiveHintKind;
  /** The card's display line (a meeting title / commitment / fact headline). */
  title: string;
  /** Stable id of the surfaced item (dedup key together with `kind`). */
  targetId: string;
  /** The meeting to open on click-through; absent/null → no navigation. */
  meetingId?: string | null;
  /** The matcher's relevance score (already ≥ the backend threshold). */
  score: number;
}

/**
 * One message in the in-meeting CHAT conversation (the dedicated chat panel,
 * `ask_assistant_chat`). The FE sends the FULL conversation (incl. the new user
 * message as the last item) on every turn, so the brain gets multi-turn memory.
 */
export interface ChatMsg {
  role: "user" | "assistant";
  text: string;
}

/**
 * One persisted `@brain` thread EXCHANGE (`list_assistant_threads`): the user's
 * `command` + the brain's `answer` for one turn of a thread, keyed by the
 * FE-generated `threadId`. Rows arrive oldest → newest and ONLY for turns that
 * carry a threadId; a sealed-and-not-session-unlocked meeting returns an EMPTY
 * array (gated server-side — never a question or answer behind the lock). The
 * record screen groups rows by `threadId` to rebuild the Slack-style threads
 * when a meeting is reopened.
 */
export interface AssistantThreadRow {
  threadId: string;
  /** The note line the thread hangs under (null for an anchorless/voice thread). */
  anchorText: string | null;
  /** The user's question for this exchange. */
  command: string;
  /** The brain's reply markdown. */
  answer: string;
  /** Flat citation strings (same shape as `VoiceActionResultPayload.citations`). */
  citations: string[];
  /** The turn's terminal status (a `VoiceActionStatus` string). */
  status: string;
  /** ISO-8601 creation timestamp (ordering is already oldest → newest). */
  createdAt: string;
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

/**
 * One deterministic note-verify finding: a ticket claim in the note checked against LIVE Jira.
 * Mirrors Rust `crate::verify::VerifyFinding`. NOTE the serde casing — `Verdict` derives
 * `#[serde(rename_all = "lowercase")]`, so `Verdict::NotFound` serializes as `"notfound"`
 * (NOT `"not_found"`); keep this union in lockstep with the backend enum.
 */
export interface VerifyFindingDto {
  lineNo: number;
  key: string;
  verdict: "confirmed" | "notfound" | "conflict";
  detail: string;
  url: string;
}

/**
 * One piece of live connector context to fold into a note (connector-agnostic — `source` is the
 * truthful connector label, e.g. "Jira" / "Slack"). Mirrors the Rust `enrich::ContextHit`.
 */
export interface ContextHit {
  source: string;
  detail: string;
  url: string | null;
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
  /**
   * Phase 5 — AI Gateway model provenance. Populated by the backend from the
   * `egress_log` table (the recorded `provider_id` + `model_requested` /
   * `model_served` from the note-generation call). All three are `null` when
   * the meeting is locked, or when no provenance was recorded (legacy meetings
   * pre-Phase 5, or providers that don't emit a `CallMeta`). The FE renders a
   * small provenance badge in the Analysis section when any field is present.
   */
  aiProvider: string | null;
  /** The model name that was REQUESTED when generating the note (e.g. "claude-opus-4-8"). */
  aiModel: string | null;
  /** The model name actually SERVED by the provider (may differ from requested when the
   *  gateway/proxy remaps the id). Preferred over `aiModel` for display when present. */
  modelServed: string | null;
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

/**
 * Speaker voiceprints (opt-in, on-device re-identification). Mirrors the Rust
 * `SpeakerSuggestion` (serde camelCase). One suggested person name for a diarized
 * `others-{n}` cluster of the CURRENT meeting, produced by cosine matching against
 * prior LABELED voiceprints — surfaced by `suggestSpeakerLabels`. Accepting it is a
 * one-tap `renameSpeaker(speaker → suggestedLabel)` (which also enrolls the cluster).
 *
 * HONESTY: re-identification ACCURACY (cross-mic / cross-meeting) is UNVERIFIED —
 * only the backend match threshold decides what surfaces here; a suggestion is a
 * best-effort guess, never a certain identity.
 */
export interface SpeakerSuggestion {
  /** The timeline label being suggested FOR (e.g. `"others-1"`). */
  speaker: string;
  /** The suggested person name (the label the accept-tap renames to). */
  suggestedLabel: string;
  /** Cosine match score, 0..=1 — a "how confident" affordance. */
  score: number;
}

/**
 * One stored voiceprint for the Settings management list. Mirrors the Rust
 * `VoiceprintInfo` (serde camelCase). NEVER carries the raw embedding — only the
 * label + provenance + dimension needed to list/forget. Read ONLY through the
 * GATED `listVoiceprints` command: a sealed-and-not-session-unlocked meeting's
 * voiceprint is EXCLUDED (never listed, never a match candidate).
 */
export interface VoiceprintInfo {
  id: string;
  /** Source meeting the voice fingerprint was captured in. */
  meetingId: string;
  /** The diarized cluster index within its source meeting (the `others-{n}` suffix). */
  clusterIndex: number;
  /** The bound person name once the cluster is enrolled by rename (null until then). */
  label: string | null;
  /** Embedding dimensionality (a harmless count; NOT the embedding itself). */
  dim: number;
  createdAt: string;
}

export interface SearchHit {
  meeting: Meeting;
  snippet: string;
  matchedIn: string;
}

/**
 * brain2 documents — lightweight metadata for one uploaded document (the Brain
 * view's document list DTO). Mirrors the Rust `DocumentInfo` (serde camelCase):
 * carries NO text (the text is gated content surfaced only by `getDocument`,
 * never in the list). `createdAt` is epoch MILLISECONDS (i64), so format it via
 * `new Date(createdAt)`. A sealed-and-NOT-session-unlocked folder returns an
 * EMPTY list (masked — never even a document name behind the lock).
 */
export interface DocumentInfo {
  id: string;
  name: string;
  /**
   * `"document"` (an uploaded `.md`/`.txt` file) or `"note"` (a typed brain
   * note). Lets the Brain page split the two source kinds into their own cards.
   * Both ride the SAME seal/gating path — this is presentation only. Mirrors the
   * Rust `DocumentInfo.kind`. Empty when the folder is sealed (masked list).
   */
  kind: "document" | "note";
  createdAt: number;
}

/**
 * brain2 — headline counts + semantic flags for the Brain page ("what's in my
 * brain"). Mirrors the Rust `BrainOverview` (serde camelCase). Every count is
 * over VISIBLE/unlocked content only (a sealed-not-unlocked folder's items are
 * never counted). Carries NO text — counts + the two flags, so it is leak-free.
 * Re-fetch on a FoldersService lock-state change so a session unlock/relock
 * shifts the counts live, like the graph.
 */
export interface BrainOverview {
  meetingCount: number;
  documentCount: number;
  noteCount: number;
  indexedChunkCount: number;
  /** `config.semantic_search_enabled` — the semantic-search master flag. */
  semanticEnabled: boolean;
  /** The on-device e5 embedding model is present (so vectors can be built). */
  embedModelPresent: boolean;
}

/**
 * ONE user-memory fact — "what the brain knows about you". Mirrors the backend
 * `crate::user_memory::UserMemoryFact` (camelCase via `#[serde(rename_all)]`).
 * The visible facts are the same set synthesized into the grounding brief, so the
 * Memory view is a faithful mirror of what the brain actually injects. `subject`
 * is the person/topic the fact is about, `predicate` the relationship, `object`
 * the value (e.g. "you" / "prefers" / "async standups"). `sourceMeetingId` is the
 * provenance link (always set for a visible fact — the gated reader drops
 * NULL-source rows fail-closed). Forgetting one bitemporally CLOSES it (never a
 * silent delete), so it drops out of `getUserMemory()` and the regenerated brief.
 */
export interface UserMemoryFact {
  id: string;
  subject: string;
  predicate: string;
  object: string;
  /** Valid-time origin (the source meeting's time — when the brain learned it). */
  validFrom: string;
  /** The meeting this was derived from (provenance + purge anchor). */
  sourceMeetingId: string | null;
  confidence: number;
}

/**
 * The full user-memory audit payload for the Memory view (`get_user_memory`).
 * Mirrors the backend `crate::user_memory::UserMemory`. GATED server-side: only
 * facts whose SOURCE meeting is visible under the live unlocked snapshot are
 * returned — a sealed-not-unlocked meeting's user memory surfaces NOTHING, so
 * re-fetch on a FoldersService lock-state change to shift the list live. `brief`
 * is the exact injected grounding text (empty when memory is empty).
 */
export interface UserMemory {
  facts: UserMemoryFact[];
  brief: string;
  /**
   * TRUE when cross-meeting memory is turned OFF entirely (`userMemoryEnabled`
   * is false). In that state `facts`/`brief` are empty and nothing is injected
   * into any prompt — the FE shows a "memory is off" affordance rather than an
   * empty list. Optional (older payloads omit it) ⇒ treat absent as `false`.
   */
  disabled?: boolean;
}

/**
 * Brain v2 L5 — one SCHEDULED-BRIEF definition (`brief_schedules`). Mirrors the
 * backend `crate::storage::models::BriefSchedule`. Config data the user typed —
 * never meeting content. `dayOfWeek`: 0 = Monday … 6 = Sunday, null = daily;
 * `lastRunAt` is the LOCAL `YYYY-MM-DD` of the last fire (once-per-day guard).
 */
export interface BriefSchedule {
  id: string;
  label: string;
  dayOfWeek: number | null;
  hourLocal: number;
  minuteLocal: number;
  scopeDays: number;
  promptHint: string | null;
  enabled: boolean;
  lastRunAt: string | null;
  createdAt: string;
}

/**
 * Brain v2 L5 — one PROPOSED brief run (`brief_runs`, propose-accept staging).
 * `noteMd` was synthesized backend-side from VISIBLE-ONLY content (the runner
 * reads with the empty unlock set) and is CONSUMED (blanked) on accept;
 * `meetingIds` are opaque source ids only.
 */
export interface BriefRun {
  id: string;
  scheduleId: string;
  status: "pending" | "accepted";
  noteMd: string;
  meetingIds: string[];
  proposedAt: string;
  acceptedAt: string | null;
}

/**
 * Brain v2 L5 — payload of `EVENT_BRIEF_PROPOSED` (a brief was staged). Carries
 * the run id + the schedule's user-authored label + a size signal — never the
 * brief markdown (fetch pending runs via `listBriefRuns`).
 */
export interface BriefProposedPayload {
  runId: string;
  label: string;
  charCount: number;
}

/**
 * Phase D — progress for the on-device PERSON-name NER model (mDeBERTa-v3)
 * download (`EVENT_NER_DOWNLOAD`). Mirrors the backend `NerDownloadPayload`: the
 * model is 3 files, so progress is reported per-file (`fileIndex` / `fileCount`).
 * `total` is null when the server omits Content-Length; `done` fires once all
 * files are written + renamed into place. Sends NO meeting content (inbound-only).
 */
export interface NerDownloadProgress {
  fileIndex: number;
  fileCount: number;
  downloaded: number;
  total: number | null;
  done: boolean;
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
  /**
   * Shared Brain v1 — provenance of the retrieval hit. Absent/null for a plain
   * LOCAL owned-content source (a meeting or note the user recorded/authored
   * themselves); present with `kind:"org"` for an org-brain hit synced from a
   * colleague's share. Lets `SourcesComponent` render an org-origin chip (author +
   * date) that routes to the read-only org-item viewer instead of `/meeting/:id`.
   * Mirrors the Rust `VaultSource.origin` (serde camelCase, `#[serde(skip_serializing_if)]`).
   */
  origin?: SourceOrigin | null;
}

/**
 * Shared Brain v1 — where a retrieval hit came from. `kind:"local"` (the default
 * for owned content) or `kind:"org"` (an org-brain item synced from a colleague).
 * For an org hit `author` is the `author_hint` label and `orgItemId` is the id
 * the org-item viewer loads via `orgGetItem`. Mirrors the Rust `SourceOrigin`.
 */
export interface SourceOrigin {
  kind: "local" | "org";
  /** The org item's author hint (present only for an org hit). */
  author?: string | null;
  /** The org item id for the read-only viewer route (present only for an org hit). */
  orgItemId?: string | null;
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

/**
 * One card in the `/people` personal-CRM list (`listPeople()`). Mirrors the Rust
 * `PersonCard` (camelCase via `#[serde(rename_all)]`) — a Person entity rolled up
 * over the SAME gated graph/facts/commitment readers as the graph. GATED
 * server-side: every count reflects VISIBLE sources only, and a person whose
 * mentions/facts/commitments live solely in sealed-not-unlocked meetings never
 * appears — so re-fetch on a FoldersService lock-state change to shift the list
 * live (mirrors {@link GraphData}). The `id` links to the existing entity detail.
 */
export interface PersonCard {
  id: string;
  name: string;
  /** VISIBLE meetings mentioning this person (sealed-not-unlocked meetings drop out). */
  meetingCount: number;
  /**
   * ISO 8601 start of the most-recent VISIBLE meeting that mentioned this person,
   * or null when there is no visible mention.
   */
  lastTalked: string | null;
  /** Open (`- [ ]`) action items across VISIBLE meetings owned by this person. */
  openCommitmentCount: number;
  /** Currently-valid (open) facts about this person from VISIBLE meetings. */
  currentFactCount: number;
}

/**
 * One OPEN action-item "commitment" rolled up across the library, with its meeting
 * context. Mirrors the Rust `Commitment` (camelCase via `#[serde(rename_all)]`).
 * Produced by the deterministic gated aggregation — only OPEN (`- [ ]`) items from
 * VISIBLE meetings contribute, so a sealed-not-unlocked meeting yields nothing.
 */
export interface Commitment {
  meetingId: string;
  meetingTitle: string;
  /** ISO 8601 meeting start (recency ordering + the [[Title]] context). */
  startedAt: string;
  /** The item owner (name), or null when unattributed. */
  owner: string | null;
  /** The due date (as written in the note), or null. */
  dueDate: string | null;
  text: string;
}

/**
 * One persisted bitemporal FACT row about an entity. Mirrors the Rust `Fact`
 * (camelCase via `#[serde(rename_all)]`). `validTo == null` ⇒ CURRENTLY valid (the
 * present state); a non-null `validTo` ⇒ CLOSED/superseded at that instant (history —
 * the fact is never deleted, only closed). GATED like the rest of the dossier: a
 * sealed-not-unlocked source meeting's facts never surface.
 */
export interface Fact {
  id: string;
  entityId: string;
  subject: string;
  predicate: string;
  object: string;
  /** Valid-time start — when the fact became true (the source meeting's time). */
  validFrom: string;
  /** Valid-time end — null while currently valid, set when superseded. */
  validTo: string | null;
  /** Transaction time — when the reconcile run recorded it. */
  recordedAt: string;
  /** The meeting the fact was derived from (gating + purge anchor); null for legacy rows. */
  meetingId: string | null;
  confidence: number;
}

/**
 * The structured, GATED, egress-free person dossier for the `/people` detail pane
 * (`getPersonDossier`). Mirrors the Rust `DossierData` (camelCase) field-for-field
 * EXCEPT `corpus`: that field is `#[serde(skip)]` on the backend (the full note
 * bodies are a synthesis-only input that NEVER crosses IPC), so there is deliberately
 * NO `corpus` field here. Assembled DETERMINISTICALLY from the encrypted DB with NO
 * provider/cloud call. Every list is visibility-gated — a sealed-and-not-session-
 * unlocked meeting contributes nothing (its meeting, commitments, and facts stay
 * invisible) until the folder is session-unlocked.
 */
export interface DossierData {
  entity: GraphEntity;
  /** Visible meetings mentioning this entity, newest first (the mention timeline). */
  meetings: VaultSource[];
  /** Open commitments tied to this entity (a mentioning meeting's item OR owner-name match). */
  commitments: Commitment[];
  /** Top co-occurring neighbour entities (shared visible meetings). */
  neighbors: EntityNeighbor[];
  /** Visible bitemporal facts (open + recently-closed), newest first. */
  facts: Fact[];
}

export interface AskVaultResult {
  answer: string;
  sources: VaultSource[];
  /**
   * Agentic-loop grounding citations, verbatim ("[[Title]]" wikilinks from
   * GATED tool output). Empty/absent on the deterministic floor path — the
   * backend serializes it with a serde default, so older payloads parse fine.
   */
  citations?: string[];
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

/**
 * AI Gateway (Phase 3) — one selectable model from the gateway's `/v1/models`
 * catalog (`list_gateway_models`). Mirrors the Rust `GatewayModel` DTO (camelCase).
 */
export interface GatewayModel {
  id: string;
}

/**
 * AI Gateway (Phase 4) — result of `gateway_health`. The backend never errors
 * on this command (unreachable → `reachable: false, modelCount: 0`), so the FE
 * can safely `.catch(() => ({reachable:false, modelCount:0}))` as an extra guard.
 * Mirrors Rust `GatewayHealth` (camelCase via `serde(rename_all = "camelCase")`).
 */
export interface GatewayHealth {
  reachable: boolean;
  modelCount: number;
}

// ── Phase 6 — Egress & Usage ledger ─────────────────────────────────────────

/**
 * PII redaction counts by kind. Each field is the number of items of that kind
 * that were scrubbed before the content left the device.
 */
export interface RedactionCounts {
  email: number;
  card: number;
  phone: number;
  name: number;
}

/**
 * One egress event row from the local content-free egress ledger
 * (`get_egress_ledger`). Carries metadata only — NO transcript text.
 * Mirrors Rust `EgressRow` (camelCase via `serde(rename_all = "camelCase")`).
 */
export interface EgressRow {
  /** Unix timestamp (seconds) — matches the Rust backend's `as_secs()`. */
  ts: number;
  /** Provider id (e.g. `"anthropic"`, `"gateway"`). */
  providerId: string;
  /** Destination host / URL recorded at egress time. */
  destination: string;
  /** Model actually served by the remote (may be null when not parsed). */
  modelServed: string | null;
  /** Total tokens sent in this call (null when not reported). */
  totalTokens: number | null;
  /** PII item counts scrubbed before this call left the device. */
  redactions: RedactionCounts;
}

/**
 * Per-model aggregate (one row per distinct `modelServed` value seen in the
 * ledger window). Used for the tokens-by-model bar chart.
 */
export interface EgressByModel {
  model: string;
  calls: number;
  tokens: number;
}

/**
 * Per-day aggregate (one row per calendar day that had ≥1 egress call).
 * `day` is `"YYYY-MM-DD"`.
 */
export interface EgressByDay {
  day: string;
  tokens: number;
}

/**
 * Egress ledger summary for a given time window (`get_egress_ledger`).
 * Content-free — only metadata aggregates and row-level metadata.
 * Mirrors Rust `EgressLedger` (camelCase).
 */
export interface EgressLedger {
  /** Total cloud calls in the window. */
  totalCalls: number;
  /** Total tokens sent across all calls in the window. */
  totalTokens: number;
  /** Per-model token breakdown (sorted by tokens desc). */
  byModel: EgressByModel[];
  /** Per-day token totals (sorted by day asc). */
  byDay: EgressByDay[];
  /** Total PII items scrubbed across all calls in the window. */
  totalRedactions: RedactionCounts;
  /** Most-recent calls (newest first, capped server-side). */
  recent: EgressRow[];
}

// ── M3-CLIENT: sharing account + zero-knowledge link shares (mode A) ──

/** Mirrors Rust `commands::AccountStatus` — the sharing-account session state. */
export interface AccountStatus {
  /** A session is present (logged in this session, or restorable from the Keychain). */
  loggedIn: boolean;
  /** The account email, when logged in. */
  email: string | null;
  /** MK is in the session so a share can actually be sealed without re-auth. */
  unlockedForSharing: boolean;
  /** The one-time share-egress consent has been granted. */
  shareConsented: boolean;
  /** A sharing server base URL is configured (sharing is impossible without one). */
  serverConfigured: boolean;
  /**
   * A one-tap Touch ID unlock is possible: logged in AND a cached account key
   * exists on this device, so `unlock_sharing_with_biometric` can restore the
   * session MK with a single biometric sheet (no password re-entry). When false,
   * fall back to the password sign-in flow to re-unlock for sharing.
   */
  biometricUnlockAvailable: boolean;
}

/** Mirrors Rust `commands::MyShareEntry` — one row of the user's shares. Content-free: `title` is
 * present ONLY when the local meeting is unlocked (a sealed meeting is masked `locked:true`). */
export interface MyShareEntry {
  shareId: string;
  /** The local meeting title, or `null` when the meeting is sealed/unknown (then `locked:true`). */
  title: string | null;
  locked: boolean;
  rev: number;
  createdAt: string;
  expiresAt: string | null;
  revoked: boolean;
  downloadCount: number;
  /**
   * The LOCAL meeting this share belongs to — the filter key for THIS note's Active-links list.
   * `null` when the share was created on another device (no local `outbound_shares` row → masked,
   * same as `title`).
   */
  meetingId: string | null;
  /**
   * The LOCAL authored-note `document_id` this share belongs to (WP6) — the filter key for a NOTE's
   * share panel. `null` for a meeting share or a share created on another device (masked like `title`).
   */
  documentId: string | null;
  /** The server-enforced open cap (`null` ⇒ uncapped) driving the `X / Y opens` label. Display-only. */
  maxDownloads: number | null;
  /**
   * `link` (mode-A zero-knowledge link) · `user` (mode-B Murmur↔Murmur grant) ·
   * `org` (Shared Brain — an org-wide E2EE item published to the org feed). The
   * `org` mode drives the "In Org Brain" `.pill` badge in the detail header and
   * library rows (mirrors the existing per-mode share badges).
   */
  mode: "link" | "user" | "org";
}

// ── M5-CLIENT: Murmur↔Murmur (mode B) ──

/** Mirrors Rust `commands::RecipientPreview` — a read-only lookup of a recipient email. */
export interface RecipientPreview {
  /** The address is a registered Murmur account (else: suggest a protected link instead). */
  registered: boolean;
  /** The safety-word fingerprint of their current key (present iff registered). */
  fingerprint: string | null;
  /** First contact — show the fingerprint for out-of-band verification, then confirm to share. */
  firstContact: boolean;
  /** Their key CHANGED since you last shared — BLOCK + re-verify out of band (never click-through). */
  keyChanged: boolean;
}

/** Mirrors Rust `commands::ShareToUserResult`. */
export interface ShareToUserResult {
  /** `"sent"` (registered → wrapped now) or `"invited"` (unregistered → pending invite). */
  status: string;
  /** The recipient's safety-word fingerprint (present for a registered recipient). */
  fingerprint: string | null;
}

/** Mirrors Rust `commands::ShareInboxItem` — one incoming pending-accept share (content-free). */
export interface ShareInboxItem {
  shareId: string;
  /** The sender's safety-word fingerprint (show for out-of-band verification on accept). */
  senderFingerprint: string;
  rev: number;
  size: number;
  createdAt: string;
  /** Already accepted locally (idempotency) — render as done. */
  alreadyAccepted: boolean;
}

/** Mirrors Rust `commands::AcceptedShare` — the new local meeting a share was accepted into. */
export interface AcceptedShare {
  meetingId: string;
  title: string;
}

/** Recording-storage usage report (mirrors Rust `StorageReportDto`). Bytes + counts only. */
export interface StorageReport {
  audioDir: string;
  usedBytes: number;
  limitBytes: number | null;
  playbackBytes: number;
  mastersBytes: number;
  sealedBytes: number;
  recordingCount: number;
  autoPrune: boolean;
}

/** Result of a prune / free-up-space run (mirrors Rust `PruneSummaryDto`). */
export interface PruneSummary {
  freedBytes: number;
  prunedCount: number;
  mastersDeleted: number;
}

/**
 * Re-Truth — one fact that a just-finished meeting SUPERSEDES from an earlier
 * note (mirrors the Rust `SupersessionDto`, camelCase). The vault "moved on":
 * an `entity`'s `predicate` changed from `oldValue` to `newValue`, and the note
 * that first recorded it (`sourceNoteTitle`) is now stale. `preview_supersessions`
 * returns pending-only rows (`applied` always false); `apply_supersessions`
 * APPENDS an Obsidian `[!superseded]` callout to the source note (append-only,
 * reversible) — it never edits the user's own prose line.
 *
 * PRIVACY (lock-model): `sourceNotePath` is an ABSOLUTE on-disk path used only
 * by the backend to locate the file — never display it; render `sourceNoteTitle`.
 * `supersedingNoteTitle` is `null` when the superseding note lives in a sealed
 * folder (masked) — the row still renders, just without that title.
 */
export interface SupersessionDto {
  id: string;
  entity: string;
  predicate: string;
  oldValue: string;
  newValue: string;
  /** Human title of the now-stale note (render this). */
  sourceNoteTitle: string;
  /** Absolute on-disk path — backend-only, NEVER displayed. */
  sourceNotePath: string;
  sourceMeetingId: string;
  supersedingMeetingId: string;
  /** `null` when the superseding note is sealed — handle gracefully. */
  supersedingNoteTitle: string | null;
  /** Preview rows are always pending (`false`); an applied row round-trips true. */
  applied: boolean;
}

/**
 * Result of applying (healing) a batch of supersessions (mirrors the Rust
 * `ApplyResult`). `applied` = callouts stamped; `skippedSealed` = rows whose
 * source note was sealed and could not be stamped this session.
 */
export interface ApplyResult {
  applied: number;
  skippedSealed: number;
}

// ── Notes feature — first-class authored notes (documents kind='note') ──────
// DTOs mirror the frozen IPC contract in docs/notes-feature/DESIGN.md §2 (Rust
// `#[serde(rename_all = "camelCase")]`). Every list/get/export/assistant path is
// GATED on the note's folder-unlock: a sealed-and-not-session-unlocked note is
// MASKED (title "🔒 Locked", no body/snippet/tags) — never leaked per-row.

/**
 * One row of the Notes list (`list_notes`). Leak-free: a sealed-not-unlocked note
 * carries NO body (`snippet` "", `tags` []) and its `title` is masked to
 * "🔒 Locked" (topics can leak through a title). Mirrors the Rust `NoteSummary`.
 */
export interface NoteSummary {
  id: string;
  /** Display title; "🔒 Locked" when sealed-not-unlocked. */
  title: string;
  folderId: string;
  /** Body excerpt; "" when locked. */
  snippet: string;
  /** Front-matter tags; [] when locked. */
  tags: string[];
  /** Epoch ms of the last edit. */
  updatedAt: number;
  createdAt: number;
  /** Sealed AND not session-unlocked (render a lock badge, no snippet). */
  locked: boolean;
  /** Has an active outbound share. */
  shared: boolean;
}

/**
 * The full note payload for the editor (`get_note` / `update_note`). Mirrors the
 * Rust `NoteDoc`. When `locked` is true the payload is MASKED: `title`
 * "🔒 Locked", `markdown` "", `tags` [], `properties` {} — the editor shows the
 * lock gate instead of the body. `markdown` is the FULL document INCLUDING the
 * YAML front-matter (properties/tags are vault-native, owned-file).
 */
export interface NoteDoc {
  id: string;
  title: string;
  folderId: string;
  /** FULL markdown incl. front-matter; "" when masked. */
  markdown: string;
  /** Front-matter tags; [] when masked. */
  tags: string[];
  /** Parsed front-matter (excluding tags); {} when masked. */
  properties: Record<string, string>;
  updatedAt: number;
  createdAt: number;
  /** Vault `.md` path, or null when never exported / sealed. */
  exportedPath: string | null;
  /** Masked (no markdown) when true. */
  locked: boolean;
  /** Has an active outbound share. */
  shared: boolean;
}

/**
 * The full selection-assistant action set (mirrors the Rust `NoteAssistAction`).
 * Grouped EDIT / STRUCTURE / FROM YOUR BRAIN / EXTRACT / CREATE (see the shared
 * action catalog in `note-assist-catalog.ts`). Each action maps to a RESULT
 * SHAPE (replace/insert/info/artifact) the backend reports in
 * `NoteAssistResult.shape` — the FE renders off that shape, never re-deriving it.
 * `custom` is the free-text escape hatch (always enabled).
 */
export type NoteAssistAction =
  | "refine"
  | "grammar"
  | "shorten"
  | "expand"
  | "simplify"
  | "tone"
  | "translate"
  | "bullets"
  | "table"
  | "keypoints"
  | "enhance"
  | "find_related"
  | "link_entities"
  | "fact_check"
  | "ask"
  | "action_items"
  | "decisions"
  | "draft_followup"
  | "spinoff_note"
  | "custom";

/**
 * How the backend result maps to the FE rendering (mirrors the Rust
 * `NoteAssistResult.shape`). `replace` = struck original vs suggestion + Accept;
 * `insert` = keep the selection, append the suggestion; `info` = read-only
 * answer + citations (Copy / Insert as note); `artifact` = a titled draft
 * (email / new note) with Copy / Create note. TRUST this over the action id.
 */
export type NoteAssistShape = "replace" | "insert" | "info" | "artifact";

/**
 * A selection-assistant request (`note_assistant_action`). Mirrors the Rust
 * `NoteAssistRequest`. `before`/`after` carry a bounded slice of surrounding
 * context (~500 chars each) so the model can act coherently on the selection.
 * `variant` carries a tone name / target language for the variant actions;
 * `instruction` carries the free-text for `custom` or the question for `ask`.
 */
export interface NoteAssistRequest {
  noteId: string;
  action: NoteAssistAction;
  /** The selected text to act on. */
  selection: string;
  /** Up to ~500 chars of context before the selection. */
  before?: string;
  /** Up to ~500 chars of context after the selection. */
  after?: string;
  /** Tone name (tone) or target language (translate); undefined otherwise. */
  variant?: string;
  /** Free-text instruction (`custom`) or question (`ask`); undefined otherwise. */
  instruction?: string;
}

/**
 * One provenance citation for an `enhance` result — the brain source the additive
 * passage drew on. Mirrors the Rust `NoteCitation`. Click-through opens the source
 * note/meeting.
 */
export interface NoteCitation {
  kind: "meeting" | "note" | "person" | "entity";
  id: string;
  title: string;
  snippet: string;
}

/**
 * The result of a selection-assistant action (`note_assistant_action`). Mirrors
 * the Rust `NoteAssistResult`. The FE renders GENERICALLY off `shape` (never
 * re-derived from `action`):
 * - `replace`  — `suggestion` replaces the selection.
 * - `insert`   — `suggestion` is appended after the selection (additive).
 * - `info`     — `suggestion` is the answer text; `citations` the sources.
 * - `artifact` — `title` + `suggestion` is a draft (email subject/body or note
 *                title/body) the user copies or turns into a note.
 * `citations` is populated for the brain-reading actions (else []). `modelLabel`/
 * `mode`/`redacted` reflect the resolved `provider_for(Role::Notes)` routing —
 * the popover DISPLAYS them (never decides them). `find_related` is retrieval-only
 * (`redacted=false`, no shield).
 */
export interface NoteAssistResult {
  action: NoteAssistAction;
  /** How to render + apply this result — replace/insert/info/artifact. */
  shape: NoteAssistShape;
  /** Artifact title (email subject / note title); null/undefined for non-artifact. */
  title?: string | null;
  /** replace: the replacement. insert: additive passage. info: the answer. artifact: the body. */
  suggestion: string;
  /** Brain-source provenance; [] for actions that don't read the brain. */
  citations: NoteCitation[];
  /** e.g. "Claude" | "Qwen2.5 4B (local)" — shown in the popover mode chip. */
  modelLabel: string;
  /** Whether the action ran on-device or in the cloud. */
  mode: "local" | "cloud";
  /** True when the cloud path redacted the payload before egress. */
  redacted: boolean;
}

/**
 * One proposed move in an auto-organize plan (`plan_organize_notes`). Mirrors the
 * Rust `OrganizeMove`. `toFolderId` is null when `toFolder` names a NEW folder to
 * create on apply. Non-destructive: nothing moves until `apply_organize_plan`.
 */
export interface OrganizeMove {
  noteId: string;
  title: string;
  fromFolderId: string;
  fromFolder: string;
  /** The proposed folder NAME (existing or new). */
  toFolder: string;
  /** null ⇒ a new folder to create on apply. */
  toFolderId: string | null;
  /** One-line why (shown to the user). */
  reason: string;
}

/** An auto-organize plan (`plan_organize_notes`). Mirrors the Rust `OrganizePlan`. */
export interface OrganizePlan {
  moves: OrganizeMove[];
}

/**
 * A note-kind folder (`list_note_folders` / `create_note_folder`). Reuses the
 * folder machinery with `kind='note'`; mirrors the Rust `NoteFolder`. `path` is
 * rooted under a "Notes/" vault prefix so it never collides with meeting-folder
 * paths.
 */
export interface NoteFolder {
  id: string;
  name: string;
  path: string;
  parentId: string | null;
  locked: boolean;
  kind: string;
}

// ── Shared Brain v1 — org-wide E2EE replicated brain (DTOs mirror the spec
// contract `docs/superpowers/specs/2026-07-10-shared-brain-v1-spec.md`, Rust
// `#[serde(rename_all = "camelCase")]`). Org items live OUTSIDE the folder-lock
// domain (they are deliberately org-disclosed content); egress from THIS user is
// gated by consent + `meeting_is_unlocked`. All the list/preview/status commands
// are leak-conscious per the spec's "no content-derived strings" discipline.

/** The member's role in an org. `owner` can invite/remove members + drive OCK rotation. */
export type OrgRole = "owner" | "member";

/**
 * The membership + sync state for ONE org (`orgCreate` returns one; `orgStatus`
 * returns the legacy single-org view or `null`; `orgListStatuses` returns one
 * per org the user belongs to — created OR invited-into). An empty
 * `orgListStatuses` array ⇒ the user is in no org (show the empty state + create
 * form). `memberCount` + `itemCount` are counts only (no names/content);
 * `lastSeq` is the last synced feed sequence; `pendingShares` is the count of
 * queued/failed local outbound org shares awaiting the sweep. Mirrors the Rust
 * `OrgStatus`.
 */
export interface OrgStatus {
  orgId: string;
  name: string;
  role: OrgRole;
  memberCount: number;
  /** The one-time org-egress consent has been granted (mirrors `shareConsented`). */
  consented: boolean;
  /** Last synced feed sequence (drives the "N items synced" status). */
  lastSeq: number;
  /**
   * Items YOU uploaded into this org (your own outbound shares). Distinct from
   * {@link receivedCount} — labelling them separately kills the "0 items" lie
   * where a member who received a colleague's share still saw `0` because this
   * only counted the caller's OWN uploads.
   */
  itemCount: number;
  /**
   * Items IN the org brain that THIS member has synced + ingested (everyone's
   * shares, including received ones). This is the count of browsable org items —
   * pair it with {@link itemCount} in the UI ("N in the org brain · M shared by
   * you"). Mirrors the Rust `OrgStatus.receivedCount`.
   */
  receivedCount: number;
  /** Local outbound org shares still queued/failed (awaiting the launch sweep). */
  pendingShares: number;
}

/**
 * One browsable row of an org's shared brain (`listOrgItems(orgId)`) — the
 * header of a synced+decrypted org item, enough to render a list row that links
 * to the read-only {@link OrgItemDetail} viewer (`/org-item/:id`). Content-lean:
 * `title` + an author HINT (never a full identity) + the created date; the full
 * body is fetched lazily by the viewer. `seq` is the item's feed sequence (stable
 * order). Mirrors the Rust `OrgItemHeader`.
 */
export interface OrgItemHeader {
  itemId: string;
  title: string;
  /** A short author hint (e.g. an email prefix) — never a full identity. */
  authorHint: string;
  createdAt: string;
  /** The item's feed sequence — stable ordering key for the browse list. */
  seq: number;
  /**
   * The item's source kind — `"document"` (a shared authored note, for the Notes
   * view) or `"meeting"` (a shared meeting note, for the Library/Meetings view) —
   * so a per-org list can be filtered into "shared meetings" vs "shared notes".
   * Populated backend-side (`Db::list_org_items` reads the stored `org_items
   * .source_kind` column) for ANY item ingested off a v2 `OrgEnvelope` — including
   * a colleague's item, not just one THIS device published. Still absent/`null`
   * for an item ingested off an old v1 envelope (published before the source-kind
   * wire field existed) — treat `null` as "unclassified", do NOT default it into
   * either bucket. Mirrors the Rust `OrgItemHeader.kind`.
   */
  kind?: "document" | "meeting" | null;
  /**
   * When THIS user published the item AND their local source is readable, the
   * editable original behind it (resolved backend-side, unlock-gated). Present ⇒
   * the row links straight to `/notes/:id` | `/meeting/:id` and `title` is the
   * source's CURRENT title (never a stale publish-time snapshot). Absent ⇒ a
   * read-only replica shared by someone else (or a locked own source), opened via
   * the `/org-item/:id` viewer. Mirrors the Rust `OrgItemHeader.owned_source`.
   */
  ownedSource?: { kind: "document" | "meeting"; id: string } | null;
}

/**
 * One org member row for the owner's member list (`orgListMembers`). Content-free:
 * a `role` + join time, and an `email` ONLY when the server discloses it — the
 * server currently withholds member emails (content-minimization), so `email` is
 * `null` and the row falls back to the opaque `userId`. `removed` marks a former
 * member (rendered muted / historical). Mirrors the Rust `OrgMember`.
 */
export interface OrgMember {
  userId: string;
  /** The member's email when the server discloses it; `null` otherwise (show `userId`). */
  email: string | null;
  role: OrgRole;
  addedAt: string;
  removed: boolean;
}

/**
 * The preview of an outgoing org share (`previewOrgShare`) — rendered in the
 * OPAQUE preview sheet (trap T3) so the user sees EXACTLY the markdown that would
 * leave the device before confirming. `markdown` is the cleaned+scrubbed outgoing
 * envelope body; `bytes` its size; `chunkCount` how many retrieval chunks it makes.
 * `scrubbed` counts what the regex PII scrub removed at the CURRENT `scrub` setting
 * (all zero when `scrub` is off). Re-fetched whenever the scrub toggle flips (the
 * markdown + counts change). Mirrors the Rust `OrgSharePreview`.
 */
export interface OrgSharePreview {
  title: string;
  /** The exact outgoing markdown (scrolled + shown verbatim in the sheet). */
  markdown: string;
  /** Byte size of the outgoing envelope body. */
  bytes: number;
  /** Retrieval chunk count the body would produce. */
  chunkCount: number;
  /** What the regex PII scrub removed at the current `scrub` setting. */
  scrubbed: OrgScrubCounts;
  /** Whether the regex PII scrub is ON for this preview (drives the toggle). */
  scrub: boolean;
}

/** Per-kind PII scrub counts for the preview sheet. Zero for every kind when scrub is off. */
export interface OrgScrubCounts {
  emails: number;
  phones: number;
  cards: number;
}

/**
 * One row of this user's outgoing org shares (`listOrgShares`) — drives the
 * "In Org Brain" state + the per-item revoke. `state` mirrors the local
 * `org_shares` state machine (queued → uploaded, or revoke_pending → revoked).
 * Content-free beyond `title` (which renders only to the local owner who can
 * already read it). Mirrors the Rust `OrgShareEntry`.
 */
export interface OrgShareEntry {
  itemId: string;
  kind: "note" | "summary";
  title: string;
  sharedAt: string;
  rev: number;
  state: "queued" | "uploaded" | "failed" | "revoke_pending" | "revoked";
}

/**
 * One org a meeting is ACTIVELY shared into (`meetingOrgShares`) — drives the "Shared with
 * [org]" badge on the Library row + the Detail view. Content-free beyond the org's own display
 * name. Gated exactly like `MeetingDetail`: a sealed-and-not-session-unlocked meeting resolves
 * to `[]`, never leaking its share status. Only the text note/summary is ever shared through
 * this path — the audio recording never leaves the device. Mirrors the Rust `MeetingOrgShareInfo`.
 */
export interface MeetingOrgShareInfo {
  orgId: string;
  orgName: string;
}

/**
 * One row of `listMeetingOrgShares` — the BULK Library-row variant of
 * {@link MeetingOrgShareInfo}: every active meeting→org share pairing across ALL of the
 * caller's meetings in one call (avoids an N+1 per-row fetch). Same gate: a sealed-and-
 * not-session-unlocked meeting contributes no rows. Mirrors the Rust `MeetingOrgShareRow`.
 */
export interface MeetingOrgShareRow {
  meetingId: string;
  orgId: string;
  orgName: string;
}

/**
 * The full decrypted org item for the read-only viewer route (`orgGetItem`).
 * `markdown` is the plaintext envelope body — this is deliberately-disclosed org
 * content (no lock gate applies to org items), rendered read-only with an
 * author + date header. Mirrors the Rust `OrgItemDetail`.
 */
export interface OrgItemDetail {
  itemId: string;
  authorHint: string;
  title: string;
  createdAt: string;
  rev: number;
  markdown: string;
}

/**
 * The LOCAL source of an org item — resolved by `org_resolve_source` so the
 * viewer can route the AUTHOR straight to their editable original (a `/notes/:id`
 * note or a `/meeting/:id` detail) instead of the read-only replica, while a
 * non-author (no local source) still gets the read-only viewer. `kind` selects
 * the route family; `sourceId` is the local document/meeting id. Mirrors the Rust
 * `OrgSourceRef`. A `null` result (from `orgResolveSource`) ⇒ this user is NOT the
 * author (no local source) → render the read-only viewer.
 */
export interface OrgSourceRef {
  kind: "document" | "meeting";
  sourceId: string;
}

/**
 * The result of a manual `orgSyncNow()` — counts + errors only, no content.
 * `ftsOnly` is true when the local member has no real embedder (StubEmbedder ⇒
 * the org partition is FTS-only until a model appears + a re-embed runs).
 * Mirrors the Rust `OrgSyncReport`.
 */
export interface OrgSyncReport {
  pulled: number;
  ingested: number;
  tombstoned: number;
  lastSeq: number;
  /** No real embedder → org partition indexed FTS-only (re-embed when a model lands). */
  ftsOnly: boolean;
  errors: string[];
}

/**
 * Payload of the `murmur://org-feed-updated` event (`onOrgFeedUpdated`). Emitted
 * by the background org-sync loop ONLY on a PRODUCTIVE tick (≥1 ingest/tombstone
 * changed the local org replica). Content-free — a single count, NO item ids /
 * titles / content; the FE treats any arrival as "re-fetch the org lists".
 * Mirrors the Rust `OrgFeedUpdatedPayload`.
 */
export interface OrgFeedUpdatedPayload {
  /** Number of joined orgs whose replica changed this tick (≥1 when emitted). */
  orgsChanged: number;
}

/**
 * The active-shares report for the lock×shares dialog (`folderActiveShares`),
 * gathered when the user tries to lock a folder that has outgoing shares. Titles
 * render only to the local owner (who can already read them) — content-free
 * enough for a dialog. `links`/`users` are 1:1 share counts; `org` is the list
 * of org-brain items shared from this folder. Mirrors the Rust `ActiveSharesReport`.
 */
export interface ActiveSharesReport {
  /** Count of active zero-knowledge LINK shares from this folder. */
  links: number;
  /** Count of active Murmur↔Murmur USER shares from this folder. */
  users: number;
  /** Org-brain items shared from this folder (item id + title). */
  org: OrgActiveShare[];
}

/** One org-brain item active for a folder (lock×shares dialog). */
export interface OrgActiveShare {
  itemId: string;
  title: string;
}
