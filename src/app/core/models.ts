// TS mirrors of Rust DTOs (camelCase-serialized). Keep in sync with PHASE0-PLAN §6.

export type Stage =
  | "idle"
  | "recording"
  | "transcribing"
  | "summarizing"
  | "exporting"
  | "saved"
  | "finalized"
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
 * Mirror of Rust `commands::RecordingStatus` (serde camelCase): whether the
 * backend recorder is capturing RIGHT NOW, plus the in-progress meeting id and
 * its RFC3339 start time. A freshly-(re)loaded webview queries this once in
 * `RecorderStore.init()` to reconcile its stage with the backend's truth (a
 * webview reload/crash swaps the FE without restarting the Rust process).
 */
export interface RecordingStatus {
  recording: boolean;
  meetingId: string | null;
  startedAt: string | null;
  /**
   * Whether SYSTEM audio capture is positively live. `null` when idle, or when this recording
   * never asked for it — absent is not the same as broken.
   *
   * The helper is a separate process and can die mid-recording. Until this existed the mic kept
   * recording and the timer kept counting while the far side of the call went missing from the
   * transcript, discovered after a meeting nobody can repeat.
   */
  systemCaptureAlive: boolean | null;
  /** Why it is not live, in words the user can act on. `null` while healthy. */
  systemCaptureNote: string | null;
}

/**
 * Payload of murmur://recording-capped — the 4h `MAX_RECORDING_SECONDS` hard
 * TIME cap was reached and the capture self-stopped. Length only, NO PII (no
 * content, meeting id, or path). Fires once per recording (rising edge).
 */
export interface RecordingCappedPayload {
  limitSeconds: number;
}

/** Content-free terminal capture fault; the retained prefix remains finalizable. */
export interface RecordingCaptureFaultPayload {
  code:
    | "STREAM_ERROR"
    | "CAPTURE_THREAD_FAILED"
    | "RESIDENT_CAPACITY_EXHAUSTED"
    | "BUFFER_LOCK_CONTENDED"
    | "INVALID_INTERLEAVED_INPUT"
    | "FRAME_COUNTER_OVERFLOW"
    | "CHECKPOINT_AUTHORITY_LOST";
  retainedFrames: number;
  sampleRate: number;
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
  { Available: true } | { Unavailable: { reason: string } };

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
   * round-tripped on every `save_config` like `diarizeOthers`. The backend treats an
   * omitted key as PRESERVE for older/partial clients; Settings still sends the explicit
   * current choice. Mirrors Rust `AppConfigDto.voiceprint_enabled`.
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
   * WHO put {@link modelSize} there — `"auto"` (Murmur's recommendation was
   * accepted as-is) or `"user"` (a deliberate pick). Mirrors Rust
   * `AppConfigDto::model_size_source`, and it is deliberately OPTIONAL in both
   * directions: OMITTING it means PRESERVE, so every existing save path (the
   * whole Settings form, every onboarding write that is not about the model
   * choice) leaves the stored value exactly as it is instead of clobbering it.
   *
   * It matters because the onboarding wizard PRESELECTS the recommendation and
   * then persists it like any other field: without sending `"auto"` there, a
   * fresh install that simply accepted the default would be recorded as a
   * deliberate user choice, and the backfill/nudge logic that keys off "did
   * they actually choose this?" would never be able to tell them apart.
   *
   * Read it back through `whisperRecommendation().modelSizeSource`, never from
   * a config round-trip (`config_to_dto` leaves it null so a read-modify-write
   * is a preserve).
   */
  modelSizeSource?: string | null;
  /**
   * OPTIONAL live-caption ASR engine (mirrors Rust `live_asr_engine`): `"whisper"` (default) or
   * `"parakeet"`. When `"parakeet"` AND its models are downloaded, live captions decode on the
   * CPU-only NVIDIA parakeet engine (off the Metal GPU); whisper stays the batch authority and the
   * fallback. Settable from the Transcription settings toggle.
   */
  liveAsrEngine: string;
  /**
   * Brain-sidecar IDLE-KILL window in seconds (mirrors Rust `brain_idle_timeout_secs`, default
   * 300): after this long with no on-device brain request, the host kills the `murmur-brain`
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
   * Run the local post-generation support scan. Its conservative, uncalibrated markers are
   * review cues, not proof that a claim is true or false. Default true.
   */
  groundSummary: boolean;
  /**
   * Workspace glossary for canonical domain spellings, one `Canonical = alias, alias` entry per
   * line. OPTIONAL on the wire: omission preserves the backend's stored value, while an explicit
   * empty string clears it. Settings always loads and sends the real string.
   */
  glossary?: string;
  /**
   * Stage E security flags. These are part of the `get_config` / `save_config`
   * round-trip (Rust `AppConfigDto`), so the FE MUST read the current values and
   * send them back unchanged on every save — otherwise the backend's serde
   * defaults clobber them (`mcpRequireToken` → false, `cloudEgressConsented` →
   * false). Never drop these from a `saveConfig` payload.
   */
  /** Require a bearer token on every MCP method (E3). Default true. */
  mcpRequireToken: boolean;
  updateCheckEnabled: boolean;
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
   * OPTIONAL by design (unlike the older `jiraEnabled`/`slackEnabled`): the five Notion/ClickUp
   * keys below are OMISSION-SAFE on the Rust side — an ABSENT key means "don't touch" and the
   * backend PRESERVES the stored value, while an explicit `false`/`""` still clears it. That lets a
   * caller which round-trips the whole DTO but predates these fields (the onboarding wizard) save
   * without silently disabling a connector the user enabled. Settings ALWAYS sends them explicitly.
   */
  /**
   * brain2 connectors — the NOTION connector MASTER toggle. NEW CLOUD EGRESS: when on (AND
   * `notionConsented` AND an integration token is configured), the brain/Ask answer may send a
   * REDACTED query off-device to the user's Notion workspace (a READ-ONLY page/database search).
   * A settable flag, round-tripped on every `save_config`. Default false. Mirrors Rust
   * `AppConfigDto.notion_enabled`.
   */
  notionEnabled?: boolean;
  /**
   * brain2 connectors — one-time consent for the Notion egress. Like `slackConsented`,
   * PRESERVE-ONLY on `save_config` (a normal save carries the current value back, never flips it)
   * and granted SOLELY by the dedicated `consent_to_notion` command. Default false (fail-closed).
   * Mirrors Rust `AppConfigDto.notion_consented`.
   */
  notionConsented?: boolean;
  /**
   * brain2 connectors — the CLICKUP connector MASTER toggle. NEW CLOUD EGRESS: when on (AND
   * `clickupConsented` AND a workspace id + API token are configured), the brain/Ask answer may
   * reach the user's ClickUp workspace for a READ-ONLY task search. Settable, round-tripped on
   * every `save_config`. Default false. Mirrors Rust `AppConfigDto.clickup_enabled`.
   */
  clickupEnabled?: boolean;
  /**
   * brain2 connectors — one-time consent for the ClickUp egress. PRESERVE-ONLY on `save_config`,
   * granted SOLELY by the dedicated `consent_to_clickup` command. Default false (fail-closed).
   * Mirrors Rust `AppConfigDto.clickup_consented`.
   */
  clickupConsented?: boolean;
  /**
   * The ClickUp workspace ("team") id the task search reads (non-secret). Settable, round-tripped
   * on `save_config`. Default "". Mirrors Rust `AppConfigDto.clickup_team_id`.
   */
  clickupTeamId?: string;
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

// ── P1 — this Mac, the whisper catalog, and the recommendation ────────────

/**
 * The hardware facts a consumer actually READS. Deliberately narrow: core
 * counts, OS version and thermal state are cheap to probe but do not cross IPC
 * until something branches on them. Every field is nullable because every probe
 * fails SOFT — an unreadable probe is `null`, never a guess.
 */
export interface MachineProfileDto {
  totalRamBytes: number | null;
  appleSilicon: boolean | null;
  /** Normalised to `Apple …`; Intel's long brand string is rejected outright. */
  chipName: string | null;
  /** Free space on the volume holding the models dir. Read once, backend-side. */
  freeDiskBytes: number | null;
}

/** One whisper catalog row, plus whether its file is on disk right now. */
export interface WhisperModelDto {
  id: string;
  /** Ladder rung (`Light` / `Balanced` / `Sharp` / `Maximum`), or null for a long-tail size. */
  tier: string | null;
  headline: string;
  approxDownloadBytes: number | null;
  approxRamBytes: number | null;
  liveSafe: boolean;
  power: number;
  downloaded: boolean;
}

/**
 * Why {@link WhisperRecommendationDto.autoDefaultId} is what it is. Authored in
 * Rust next to the branch that produced it so it cannot drift. The FE maps a
 * variant to a sentence and never assembles the reasoning itself — in
 * particular the RAM-causal sentence belongs to `freshInstallAmpleRam` and to
 * nothing else, because every other branch is presence-first.
 */
export type RecommendReason =
  /** Presence, not RAM, decided this — never render a RAM-causal sentence. */
  | "alreadyDownloaded"
  /** The ONE branch where "your Mac has N GB, so Murmur picked Sharp" is true. */
  | "freshInstallAmpleRam"
  /** Proven not Apple Silicon. The ONLY variant whose copy may name a chip family. */
  | "notAppleSilicon"
  /** The chip probe failed. Copy must NOT name a chip — on a real Intel Mac the
   * `hw.optional.arm64` key is absent rather than false, so this state is reached
   * by both a genuine Intel Mac and an Apple-Silicon Mac that could not answer. */
  | "archUnknown"
  /** Apple Silicon with MEASURED RAM below the floor — causal, but the "not enough" one. */
  | "modestRam"
  /** Apple Silicon whose RAM could not be measured. Makes no claim. */
  | "ramUnknown"
  /** A non-turbo model is already on disk: history, not hardware, kept it conservative. */
  | "existingInstall";

/** The on-device brain posture we ADVISE here. Advice only — it gates nothing. */
export type BrainAdvice = "full" | "reactions";

/**
 * What one command answers: the machine, the catalog, and the TWO different
 * answers to "which model?".
 *
 * {@link recommendedId} is the honest hardware answer, blind to what is on
 * disk. {@link autoDefaultId} is what a blank config resolves to today, which
 * is presence-first and therefore differs on most existing installs. Shipping
 * only one of the two would make the badge contradict the selected size.
 */
export interface WhisperRecommendationDto {
  machine: MachineProfileDto;
  /** The visible catalog (provisional rows excluded), ascending by cost. */
  models: WhisperModelDto[];
  /** The size that will actually load right now (a blank `modelSize` resolved). */
  selectedId: string;
  recommendedId: string;
  autoDefaultId: string;
  reason: RecommendReason;
  /** `"auto"` / `"user"` / null — who put `selectedId` there. */
  modelSizeSource: string | null;
  /** A custom model file is configured AND exists, so it overrides the ladder. */
  customPathOverride: boolean;
  /**
   * Bytes a download would transfer right now for the selected size, INCLUDING
   * the live-caption companion when one is planned. Computed in Rust so the FE
   * never sums model sizes itself.
   *
   * `0` = nothing to fetch. `null` = a download IS pending but its size is not
   * known — render "size unknown", NEVER "free". The two are deliberately kept
   * apart so an unmeasured id cannot promise a free multi-GB transfer.
   */
  pendingDownloadBytes: number | null;
  /**
   * LIVE-caption readiness, from the SAME classification `get_config` ships as
   * `liveCaptions` — resolved over the one models-dir listing this DTO already
   * took. `"noModel"` / `"modelMissing"` are what the picker's repair
   * affordance keys off; `"pinnedHeavy"` is a deliberate configuration, not a
   * failure, so it is NOT offered a repair.
   */
  liveCaptions: LiveCaptionsState;
  brainAdvice: BrainAdvice;
}

/**
 * How the live-caption tick's model resolved. `""` = not probed yet.
 * Mirrors the Rust `LiveCaptions::dto_state`.
 */
export type LiveCaptionsState =
  "" | "ready" | "noModel" | "modelMissing" | "pinnedHeavy";

/**
 * What a `download_model` call ended up doing. A user-initiated CANCEL is a
 * NORMAL outcome (`status: "cancelled"`), never a rejected promise — the FE
 * must never have to string-match an error message to tell a cancel apart from
 * a dead link.
 */
export interface ModelDownloadOutcome {
  status: "ready" | "cancelled";
  path: string | null;
}

/**
 * The one-shot machine-change notice — this install last ran on a DIFFERENT Mac
 * (restore-from-backup / Migration Assistant) and has not dismissed the notice.
 * Deliberately PULLED, never pushed: an event emitted during backend `setup`
 * would be lost, because the webview has not called `listen()` yet.
 */
export interface MachineChangeNudge {
  recommendedId: string;
  recommendedTier: string | null;
  selectedId: string;
  chipName: string | null;
  totalRamBytes: number | null;
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
 * Brain v3 PR-2/PR-4 — progress for an in-flight document import (`importDocument`):
 * the extract → chunk → embed pipeline for ONE document. Mirrors the backend
 * `DocImportPayload` (camelCase). Counts + a stage ONLY — NO PII (`documentId`
 * is a random UUID; no filename / text). `done`/`total` are REAL counts within a
 * stage: PAGES for `"extracting"` (page k of N — including a scanned-PDF OCR) and
 * embed SUB-BATCHES for `"embedding"` (batch k of M); `0`/`0` for `"chunking"`.
 * `truncated` is `true` ONLY on the final `"done"` event when the scanned-PDF OCR
 * page cap was reached (some scanned pages were skipped — partial content). `stage`:
 * `"extracting"` | `"chunking"` | `"embedding"` | `"done"`.
 */
export interface DocImportProgress {
  documentId: string;
  stage: "extracting" | "chunking" | "embedding" | "done";
  done: number;
  total: number;
  truncated: boolean;
}

/**
 * Progress for a BULK import (Settings -> Imports). Counts only, never a title or a path.
 * `total` is the page count for the stage in flight, so the bar keeps moving through a
 * thousand-page workspace instead of freezing after the counting phase.
 */
export interface BulkImportProgress {
  stage: "scanning" | "importing" | "linking" | "done";
  done: number;
  total: number;
}

/**
 * The DRY-RUN plan for a Notion export: what an import WOULD do. Produced without writing
 * anything, so the user confirms against real numbers rather than a promise.
 */
export interface ImportScanReport {
  /** Pages that would be imported or updated. */
  pages: number;
  /** Of those, how many already exist here (they update in place, never duplicate). */
  alreadyImported: number;
  /** Images and other non-page files. Counted and weighed, not imported yet. */
  attachments: number;
  attachmentBytes: number;
  /** Database CSV exports. Not imported yet. */
  databases: number;
  /** The `_all.csv` twins Notion ships beside each database view - the same data again. */
  csvAllDuplicates: number;
  /** Nested `Export-...-Part-N.zip` archives descended into automatically. */
  nestedArchives: number;
  /** Titles occurring more than once in the export. */
  titleCollisions: string[];
  /** A few titles for the preview, so the user can confirm this is the right export. */
  sampleTitles: string[];
  /** The source exceeded the per-import page cap and the plan was cut short. */
  truncated: boolean;
  /**
   * The chosen Obsidian folder IS the vault Murmur exports to - importing it would read our own
   * notes back in as copies of themselves.
   */
  isMurmurVault: boolean;
  /**
   * Name of the container an unfiled import lands in, for THIS source ("Imported from Notion",
   * ...). The picker's default option shows it, so the destination is stated before the run.
   */
  defaultDestination: string;
}

/** Where an import reads from. The value is the wire contract with the Rust side. */
export type ImportSourceId = "notion" | "obsidian" | "apple-notes";

/** What an import actually did. A partial run stays legible instead of silent. */
export interface ImportReport {
  imported: number;
  updated: number;
  skipped: number;
  failed: number;
  /** Up to 20 `title: reason` lines for the failures. */
  failures: string[];
  foldersCreated: number;
  /** The user cancelled: everything already written stays. */
  cancelled: boolean;
  /** Vectors were deferred - keyword search works now, semantic search after a Reindex. */
  embeddingDeferred: boolean;
  /**
   * The note-folder the notes actually landed in - the per-source container for an unfiled
   * import, or whatever the user picked. Counters alone never answered "where did my pages go".
   */
  destinationId: string;
  destinationName: string;
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
}

export type MeetingStatus =
  "DRAFT" | "RECORDING" | "TRANSCRIBED" | "SUMMARIZED" | "EXPORTED" | "ERROR";

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
  /**
   * `"meeting"` or `"note"` — the backend returns EVERY folder here (both
   * namespaces share one table so lock-reactive consumers see all of them), so a
   * view that should only show ONE namespace filters on `kind`: the Meetings
   * sidebar tree renders `kind !== "note"`, and a note folder must never leak
   * into it. Legacy rows default to `"meeting"`.
   */
  kind: string;
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
 * One note claim → the transcript segment it most likely derives from (Brain v3
 * PR-5 "Receipts"). Mirrors the Rust `ClaimAlignment` (serde camelCase). Carries
 * NO note or transcript TEXT — only the claim's raw-markdown-line index, the
 * segment's audio coordinates (RAW SECONDS, for the player seek), and non-content
 * metadata (speaker label + ASR confidence) — so it is safe to hand a caller that
 * has already passed the read gate. EMPTY list for a sealed-and-not-session-
 * unlocked meeting (the backend returns nothing behind a lock).
 */
export interface ClaimAlignment {
  /** Index of the claim in the note's raw `markdown.split("\n")` lines. */
  claimIndex: number;
  /** `Segment.idx` of the best-matching transcript segment (flash target). */
  segmentId: number;
  /** Segment start in RAW SECONDS — the audio player seek target. */
  startS: number;
  /** Segment end in RAW SECONDS. */
  endS: number;
  /** `"me"` / `"others"` / `null` — straight from the segment. */
  speaker?: "me" | "others" | string | null;
  /** Per-segment ASR confidence in `[0,1]`, or `null` when whisper didn't compute it. */
  confidence?: number | null;
  /** The token-overlap ratio that won this alignment (`[0.5, 1.0]`). */
  overlap: number;
}

/**
 * One persisted in-meeting voice-assistant interaction (Q&A): the user's spoken
 * command, the assistant's answer, the grounding citations, and the dispatch
 * status. Surfaced in the meeting detail's "Assistant — Q&A" section. EMPTY when
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
   * "🎙 Assistant — Q&A" detail section.
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
 * The MINIMAL descriptor the read-only document preview modal needs to open one
 * `documents` row (`DocumentPreviewComponent`'s `doc` input + `DocumentPreviewService`'s
 * `open()` argument): just the `id` (the gated `getDocument(id)` read key), the display
 * `name`, and the `kind` (drives the header badge — `"note"` is always "Note", a
 * `"document"` badge is derived from the filename extension). Deliberately a SUBSET of
 * {@link DocumentInfo} (which stays structurally assignable, so the Brain page's existing
 * `DocumentInfo`-typed call sites keep compiling), so a document reachable ONLY as a link
 * target (a graph node, a `[[wikilink]]`, a Related chip — where we have an id + a label
 * but no full `DocumentInfo`) can still be previewed.
 */
export interface DocumentPreviewTarget {
  id: string;
  name: string;
  kind: "document" | "note";
}

/**
 * The two selectable SMART-NOTE recipe shapes for `IpcService.generateNoteFromDocument`
 * (mirrors the Rust `summarize::recipes::NoteRecipe` tokens):
 * - `"synthesis"` — the flagship free-form path for a whiteboard photo / screenshot / slide
 *   deck with no inherent schema (summary → outline → action items).
 * - `"structure-mirror"` — for forms/tables: a deterministic transpile of the document's
 *   structure into markdown, every value an opaque string (never summed — §10).
 */
export type NoteRecipe = "synthesis" | "structure-mirror";

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

/** Vault Audit — the deterministic hygiene-pass kinds. */
export type AuditFindingKind =
  "broken_link" | "orphan" | "stale" | "contradiction" | "unlinked_mention";

/**
 * Vault Audit — one run's summary (`run_vault_audit` / the audit-updated
 * event). `counts` maps a finding kind → how many PENDING findings of that
 * kind exist after the run. Timestamps are epoch numbers.
 */
export interface AuditRunSummary {
  runId: string;
  startedAt: number;
  finishedAt: number;
  findingsNew: number;
  findingsTotalPending: number;
  counts: Record<string, number>;
}

/**
 * Vault Audit — one STAGED finding (`audit_findings`, propose-accept).
 * `evidenceMd` is a short markdown snippet built backend-side from
 * VISIBLE-only content; `acceptAction` is the human description of what
 * Accept will do — "" means the finding is dismiss-only. Timestamps are
 * epoch numbers.
 */
export interface AuditFinding {
  id: string;
  kind: AuditFindingKind;
  sourceKind: "meeting" | "note";
  sourceId: string;
  sourceTitle: string;
  targetTitle?: string | null;
  evidenceMd: string;
  acceptAction: string;
  status: "pending" | "accepted" | "dismissed";
  createdAt: number;
  resolvedAt?: number | null;
}

/**
 * Vault Audit — the weekly-schedule state (`get_audit_schedule` /
 * `set_audit_schedule`). Timestamps are epoch numbers; `lastRunAt` is
 * null/absent before the first scheduled run, `nextDueAt` while disabled.
 */
export interface AuditSchedule {
  enabled: boolean;
  lastRunAt?: number | null;
  nextDueAt?: number | null;
}

/**
 * Vault Audit — one AI explanation of a staged finding
 * (`explain_audit_finding`). `explanationMd` renders through the shared
 * markdown component; `provider` names the AI provider that produced it
 * (cloud providers only ever see redacted content).
 */
export interface AuditExplanation {
  findingId: string;
  explanationMd: string;
  provider: string;
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

/** Exact durable Ask-history namespace. Org and dashboard Ask stay stateless. */
export type AskConversationScope =
  { kind: "vault" } | { kind: "note" | "meeting"; refId: string };

/** One bounded, newest-first row in the durable Ask-history browser. */
export interface AskConversationSummary {
  id: string;
  scope: AskConversationScope;
  title: string;
  createdAt: string;
  updatedAt: string;
  messageCount: number;
}

/** One canonical, ordered message loaded from SQLite for a durable Ask thread. */
export interface AskConversationMessage {
  id: string;
  ordinal: number;
  role: "user" | "assistant";
  content: string;
  sources: VaultSource[];
  citations: string[];
  createdAt: string;
}

/** A durable Ask thread plus the source selection saved with its latest turn. */
export interface AskConversation {
  id: string;
  scope: AskConversationScope;
  title: string;
  selectedSources: SourceRef[];
  /** Live-resolved composite scope; null when absent, deleted or not readable. */
  dashboard?: DashboardScopeRef | null;
  messages: AskConversationMessage[];
  createdAt: string;
  updatedAt: string;
}

/** Successful atomic send: the backend owns persistence and canonical thread id. */
export interface AskConversationSendResult {
  conversationId: string;
  userMessageId: string;
  assistantMessageId: string;
  answer: string;
  sources: VaultSource[];
  citations: string[];
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

/** One ordered `## {heading}` block of a user-authored note template (declarative data). */
export interface NoteTemplateSection {
  heading: string;
  instruction: string;
}

/**
 * A user-authored NOTE TEMPLATE (Granola-style named sections). Selected by `id` via the note-style
 * selector and rendered into the summarizer system prompt. Mirrors the Rust `storage::models::
 * NoteTemplate`. Declarative data only — the backend rejects scripting tokens (`<%`, `tp.`,
 * `require(`, `process.`) at save.
 */
export interface NoteTemplate {
  id: string;
  name: string;
  tone: string;
  sections: NoteTemplateSection[];
  extraFrontmatterKeys: string[];
  createdAt: string;
}

export interface ActionItem {
  idx: number;
  done: boolean;
  text: string;
  owner: string | null;
  dueDate: string | null;
}

// ── First-class Murmur reminders ──────────────────────────────────────────

/** Calendar recurrence units accepted by the reminder composer/backend. */
export type ReminderRepeatUnit = "days" | "weeks" | "months" | "years";

export type ReminderState = "active" | "completed";
export type ReminderOrigin = "manual" | "smart";

/** The only identity persisted for a reminder source. */
export interface ReminderSourceAnchor {
  kind: SourceKind;
  id: string;
}

/** Live-gated metadata for a currently visible reminder source. */
export interface ReminderSourceView extends ReminderSourceAnchor {
  title: string;
}

/** Create/update payload. Source titles are deliberately never sent. */
export interface ReminderDraft {
  title: string;
  details: string | null;
  /** UTC epoch milliseconds derived from a calendar-valid local date/time. */
  dueAt: number;
  repeatEvery: number | null;
  repeatUnit: ReminderRepeatUnit | null;
  sources: ReminderSourceAnchor[];
}

export interface ReminderView {
  id: string;
  title: string;
  details: string | null;
  dueAt: number;
  repeatEvery: number | null;
  repeatUnit: ReminderRepeatUnit | null;
  state: ReminderState;
  origin: ReminderOrigin;
  createdAt: number;
  updatedAt: number;
  completedAt: number | null;
  sources: ReminderSourceView[];
}

export interface ReminderInboxItem {
  occurrenceId: string;
  dueAt: number;
  reminder: ReminderView;
}

export interface RemindersSnapshot {
  inbox: ReminderInboxItem[];
  upcoming: ReminderView[];
  completed: ReminderView[];
  dueInboxCount: number;
}

/** Content-free startup/nav projection. */
export interface ReminderSummary {
  dueInboxCount: number;
}

/** Count-only invalidation event; consumers refetch canonical rows. */
export type RemindersUpdatedPayload = ReminderSummary;

/**
 * Content-free invalidation for one mounted Smart-reminder source. Canonical
 * writes publish only the kind + opaque id; consumers re-enter the gated audit
 * command for any source-derived content.
 */
export type ReminderSourceUpdatedPayload = ReminderSourceAnchor;

/** Review-only Smart candidate; it is not a reminder until explicitly accepted. */
export interface ReminderSuggestionView {
  id: string;
  title: string;
  suggestedDueAt: number | null;
  source: ReminderSourceView;
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

/**
 * Note↔note backlinks ("Linked mentions") — the KIND of a local owned source
 * that links to (mentions) the current target. `"meeting"` routes to
 * `/meeting/:id`; `"note"` routes to `/notes/:id`. Mirrors the Rust
 * `SourceKind` (serde camelCase / lowercase).
 */
export type SourceKind = "meeting" | "note";

/**
 * Note↔note backlinks — one INBOUND source that mentions the current target
 * (`get_backlinks(targetKind, targetId)`). A chip in the "Linked mentions" row:
 * `id` + `kind` decide the click-through route, `title` + `timestamp` (ISO-8601)
 * render the chip. GATED server-side like every content read — a
 * sealed-and-not-session-unlocked source never appears in the list. Mirrors the
 * Rust `BacklinkSource` (serde camelCase).
 */
export interface BacklinkSource {
  id: string;
  kind: SourceKind;
  title: string;
  timestamp: string;
}

/**
 * Brain v3 PR-3 — one persisted `links` edge incident on `(kind, id)`, surfaced to the
 * "Connections" panel (`list_links(kind, id)`). Both endpoints are visibility-gated
 * server-side: a sealed queried item yields `[]` (no existence leak) and a sealed
 * neighbour is never included, so the caller MUST STILL skip/hide the panel while the
 * item itself is locked/masked (never surface connections behind a lock).
 *
 * - `direction` — `"out"` (the queried item is this edge's `src`) | `"in"` (it is `dst`).
 * - `otherKind` — the neighbour endpoint kind: `"meeting" | "note" | "document"`
 *   (`meeting` → `/meeting/:id`, `note` → `/notes/:id`; a `document` has no route —
 *   its chip opens the read-only preview modal via `DocumentPreviewService`).
 * - `edgeType` — `"wikilink" | "companion" | "semantic" | "manual"`. `wikilink`/
 *   `companion`/`manual` are DETERMINISTIC edges (rendered as plain link chips);
 *   `semantic` is a SUGGESTED edge the user can Accept/Dismiss, with `score` the
 *   cosine confidence.
 * - `createdBy` — `"user" | "auto" | "accepted"`; `status` — `"active" | "suggested"`
 *   (`"dismissed"` rows are never returned). `score` is `1.0` for deterministic edges.
 * - `manual` (PR-1) — backward-compatible `true` when this chip represents a USER-created link (via the
 *   `+ Link` chooser → `link_items`) that is removable with a `×` (→ `unlink_items`).
 *   The backend dedupes a manual+wikilink pair into ONE chip and sets `manual:true`
 *   on it. Defaults to `false`/absent for auto (wikilink/companion) and semantic
 *   edges, which are NOT user-removable from the panel. `manualEdges` carries the authoritative one
 *   or two exact directed rows to unlink atomically. (`#[serde(default)]`.)
 *
 * Mirrors the Rust `LinkEdge` (serde camelCase).
 */
export interface ManualLinkEdge {
  srcKind: LinkKind;
  srcId: string;
  dstKind: LinkKind;
  dstId: string;
}

export interface LinkEdge {
  id: number;
  direction: "out" | "in";
  otherKind: LinkKind;
  otherId: string;
  /**
   * Current routable identity for a revision-stable neighbour. For `org`,
   * `otherId` is the stable document id while this is the current live item id.
   * Absent for local endpoints and older backends.
   */
  navigationId?: string | null;
  otherTitle: string;
  /**
   * For an `otherKind === "container"` neighbour only: `"project"` (a Space) or
   * `"folder"`. NON-CONTENT metadata (the same `folders.level` the sidebar draws),
   * carried so the chip can pick its glyph and its noun without a second IPC call.
   * Absent for every other kind (`#[serde(skip_serializing_if)]`).
   */
  otherContainerLevel?: ContainerLevel | null;
  edgeType: "wikilink" | "companion" | "semantic" | "manual";
  createdBy: "user" | "auto" | "accepted";
  status: "active" | "suggested";
  score: number;
  createdAt: number;
  /**
   * PR-1 — `true` when the chip is a user-created link that should show a
   * removable `×`. Backend field `manual` (`#[serde(default)]` ⇒ may be absent).
   */
  manual?: boolean;
  /**
   * Exact directed manual rows collapsed into this chip. A representative may be an
   * oppositely-directed wikilink, and both manual directions may coexist, so unlink must send this
   * complete list instead of reconstructing one tuple from `direction`.
   */
  manualEdges?: ManualLinkEdge[];
}

/**
 * The link-endpoint kind an `app-connections` panel is anchored to (`list_links` `kind`).
 *
 * `container` is a whole LOCAL place — a Space or a folder, discriminated by
 * {@link LinkEdge.otherContainerLevel}, not by a second kind. It is Related-panel
 * METADATA: it never expands to what the container holds and never enters a Brain
 * scope, a provider context or a conversion source (see {@link CONTENT_LINK_KINDS}).
 */
export type LinkKind = "meeting" | "note" | "document" | "org" | "container";

/** Space vs folder — `folders.level`, mirrored for the container chip's glyph + copy. */
export type ContainerLevel = "project" | "folder";

/**
 * The MATERIAL content kinds — the ones that carry text a Brain/provider may read.
 *
 * The frontend twin of the Rust `LinkKind::is_content_source`. Every place that turns
 * a {@link LinkEdge} into a Brain source must filter through {@link isContentLinkKind}
 * rather than accepting whatever kind the edge happens to carry: `org` is somebody
 * else's Shared Brain document and `container` is a PLACE, so neither has a body to
 * put in a prompt.
 */
export const CONTENT_LINK_KINDS: readonly LinkKind[] = [
  "meeting",
  "note",
  "document",
];

/** Whether this endpoint kind carries material content a Brain scope may include. */
export function isContentLinkKind(kind: LinkKind): boolean {
  return CONTENT_LINK_KINDS.includes(kind);
}

/**
 * One source the Brain has been scoped to (the `mur-source-picker` chip model) —
 * a note/meeting/document the user picked to constrain an Ask over. Reuses the
 * existing {@link LinkKind} tri-state (the picker drops person/entity/org
 * `NoteCitation` rows). `title` is carried purely for chip display; identity is
 * `kind + id` (a picked candidate is deduped, and re-picking never doubles it).
 */
export interface SourceRef {
  kind: LinkKind;
  id: string;
  title?: string;
}

/**
 * Metadata-only identity of the ONE user-composed dashboard that scopes an Ask.
 *
 * This deliberately is not a {@link SourceRef}: a dashboard is a composite over
 * live, backend-gated material and derived views, never a fourth `LinkKind`.
 * `title` and `emoji` are display metadata resolved live by the backend on
 * history load; only `id` is authoritative and persisted.
 */
export interface DashboardScopeRef {
  id: string;
  title: string;
  emoji: string | null;
}

/**
 * A resolved `[[Title]]` wikilink navigation target — the VISIBLE note/meeting/org
 * (Shared Brain) item to open (`resolveWikilink(title)`). `null` when nothing matches
 * OR the only local match is sealed-and-not-session-unlocked (gated server-side, so a
 * click can never reveal or open locked content). `kind` is a raw string (NOT
 * `SourceKind` — mirrors `NoteCitation`'s convention for the same tri-state, since
 * `SourceKind` stays local-only for backlinks): `"org"` carries the org item's id,
 * routed to `TabsService.openOrgItem`, never a local id; `"document"` (a brain-ingested
 * `documents` row, e.g. a PDF) carries the document id, opened in the read-only
 * {@link DocumentPreviewTarget} modal (via `DocumentPreviewService`), never a route.
 * Mirrors the Rust `WikiTarget` (serde camelCase).
 */
export interface WikiTarget {
  kind: "meeting" | "note" | "org" | "document";
  id: string;
}

/**
 * The result of appending a recording-time jot (or an accepted `@brain` draft) to
 * a meeting's ONE living companion note (`append_to_companion_note`). The backend
 * lazily gets-or-creates the companion note in the Notes ROOT on the first send,
 * appends the markdown block, and returns:
 *   - `noteId` — the companion note's document id, so the "✓ Saved to Notes" card
 *     can open it by id (via `TabsService.openNote`), never by a fragile title;
 *   - `meetingWikilink` — the visible `[[Meeting]]` display link, rendered on the
 *     card's meeting chip (navigation to the meeting itself goes by the store's
 *     `meetingId`, not by resolving this string). Mirrors the Rust
 *     `CompanionAppendResult` (serde camelCase).
 */
export interface CompanionAppendResult {
  noteId: string;
  meetingWikilink: string;
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
  /**
   * The TRUE count of VISIBLE entities, BEFORE the backend's 500-row render cap trims `nodes`.
   * `totalVisibleEntities > nodes.length` means the cap silently dropped rows — independent of
   * `hasHidden` (which only reflects LOCKED folders, never the render cap).
   */
  totalVisibleEntities: number;
}

// ── Brain v3 PR-4 — the FULL-BRAIN graph (typed, multi-kind) ─────────────────
//
// A SEPARATE, additive payload (`getFullGraph`) from the entity-only `getGraph`.
// It unifies entities + VISIBLE meetings + notes + documents as TYPED nodes and
// every relation (entity co-occurrence + entity→meeting mentions + `links` rows)
// as TYPED edges. Fully lock-gated server-side: a sealed-and-not-session-unlocked
// item contributes NOTHING — no node, no edge touching it.

/**
 * The kind of a full-brain graph NODE (Rust `FullGraphNodeKind`, snake_case →
 * these lowercase literals). Drives the per-kind node color + the node lens.
 */
export type FullGraphNodeKind = "entity" | "meeting" | "note" | "document";

/**
 * One TYPED node in the full-brain graph. `label` is the display title (already
 * resolved through the visibility gate — a sealed item never produces a node).
 * `date` is an ISO-8601 / epoch-derived string when the source carries one
 * (`null` for entities). `degree` is the node's edge count WITHIN the returned
 * (gated + capped) graph — a layout hint, never the true corpus degree.
 */
export interface FullGraphNode {
  id: string;
  kind: FullGraphNodeKind;
  label: string;
  date: string | null;
  degree: number;
}

/**
 * The relation a full-brain edge encodes (Rust `FullGraphEdgeKind`, snake_case).
 * `co_occurrence` = entity↔entity; `mention` = entity→meeting; `wikilink` /
 * `companion` / `manual` / `semantic` = a `links` row. `manual` is a USER-created
 * "Related" link (the most intentional edge — always active). Drives the per-kind
 * edge style + the edge lens.
 */
export type FullGraphEdgeKind =
  | "co_occurrence"
  | "mention"
  | "wikilink"
  | "companion"
  | "manual"
  | "semantic";

/**
 * One TYPED edge in the full-brain graph. `src`/`dst` are node ids that BOTH
 * appear in `nodes` (both-endpoint-gated). `srcKind`/`dstKind` carry the ENDPOINT
 * node kinds the backend gated on (a links edge can connect `meeting↔note`, so the
 * endpoint kinds are NOT derivable from `kind` alone) — the FE matches endpoints by
 * `(kind, id)`, not bare `id`, safe against a cross-kind id collision. `status` is
 * `"active"` for deterministic edges (co-occurrence/mention/wikilink/companion +
 * accepted semantic) and `"suggested"` for un-accepted semantic edges (only present
 * when `includeSuggested` was on) — a suggested edge is drawn dashed.
 */
export interface FullGraphEdge {
  src: string;
  dst: string;
  srcKind: FullGraphNodeKind;
  dstKind: FullGraphNodeKind;
  kind: FullGraphEdgeKind;
  score: number;
  status: "active" | "suggested";
}

/**
 * Options for `getFullGraph`. All-default so the FE can call it with none.
 * `includeSuggested` (default `false`) admits un-accepted (`status: "suggested"`)
 * semantic `links` rows — the "Suggested links" lens toggle. Toggling it is the
 * ONE case that re-fetches (the backend must include/exclude the rows); every
 * other lens filters the already-fetched graph client-side (no re-fetch).
 */
export interface FullGraphOpts {
  includeSuggested: boolean;
}

/**
 * The full-brain graph payload from `getFullGraph()`: TYPED nodes + TYPED edges +
 * the same honest disclosure the entity graph makes. `hasHidden` is true when
 * ≥1 folder is sealed-and-not-unlocked (some nodes/edges may be hidden).
 * `totalVisibleNodes` is the TRUE count of visible nodes BEFORE the per-kind
 * render caps trimmed `nodes` (`totalVisibleNodes > nodes.length` = a cap
 * dropped rows — distinct from `hasHidden`, which only reflects LOCKED folders).
 * `edgesTruncated` is true when an EDGE-leg cap (the mention or links LIMIT)
 * trimmed edges, so the FE can disclose "some links are hidden" — distinct from
 * `totalVisibleNodes` (a node-leg cap) and `hasHidden` (a locked folder).
 */
export interface FullGraphData {
  nodes: FullGraphNode[];
  edges: FullGraphEdge[];
  hasHidden: boolean;
  totalVisibleNodes: number;
  edgesTruncated: boolean;
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
 * Brain v3 PR-6 — one END of a supersession pair (or one added/removed fact) in a
 * {@link EntityKnowledgeDiff}, projected to what a decision ledger row needs: the
 * human-readable old→new values + provenance. Mirrors the Rust `FactStateChange`
 * (serde camelCase). Semantics of `oldObject`/`newObject` by list:
 * `added` → `oldObject: null`; `removed` → `newObject: null`; `changed` → both present.
 */
export interface FactStateChange {
  /** Entity name at assertion time (the fact's subject). */
  subject: string;
  /** The attribute (the fact's predicate), e.g. "status", "owner". */
  predicate: string;
  /** OLD object: `null` for an added fact; the (was-current) object for removed/changed. */
  oldObject: string | null;
  /** NEW object: `null` for a removed fact; the now-current object for added/changed. */
  newObject: string | null;
  /** Valid-time start (ISO 8601) of the state this row carries — the ledger's date. */
  validFrom: string;
  /**
   * The source meeting the fact was learned from — the gating + provenance anchor.
   * `null` only for legacy unattributed rows (already gate-dropped upstream, so in
   * practice always present); a present id navigates to `/meeting/:id`.
   */
  sourceMeetingId: string | null;
}

/**
 * Brain v3 PR-6 — the deterministic set diff of two `snapshot_as_of` snapshots for
 * one entity, keyed by `(norm(subject), norm(predicate))`. Mirrors the Rust
 * `KnowledgeDiff` (serde camelCase). All lists are deterministically sorted.
 */
export interface KnowledgeDiff {
  /** Attributes present at `to` but not at `from`. */
  added: FactStateChange[];
  /** Attributes present at `from` but not at `to`. */
  removed: FactStateChange[];
  /** Attributes present at both with a DIFFERENT object (old → new). */
  changed: FactStateChange[];
}

/**
 * Brain v3 PR-6 — the payload of `getEntityKnowledgeDiff(entityId, from, to)`: the
 * between-two-instants set diff PLUS the full chronological decision ledger for one
 * entity. GATED server-side through the visible-facts reader — a
 * sealed-and-not-session-unlocked meeting's fact enters no snapshot, diff entry, or
 * ledger row. Mirrors the Rust `EntityKnowledgeDiff` (serde camelCase).
 */
export interface EntityKnowledgeDiff {
  entityId: string;
  /** The `from` instant echoed back (normalized UTC). */
  from: string;
  /** The `to` instant echoed back (normalized UTC). */
  to: string;
  /** added / removed / changed between the `from` and `to` snapshots. */
  diff: KnowledgeDiff;
  /**
   * Every supersession for this entity, oldest → newest — the decision ledger.
   * Independent of the from/to window (the entity's whole history), so the FE can
   * render the full timeline. Empty when the entity has no supersessions.
   */
  ledger: FactStateChange[];
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
 * The payload from `listPeople()`: the (possibly render-capped) roster of {@link PersonCard}s
 * plus the TRUE count of visible people. `list_people`'s candidate set is itself capped upstream
 * by the same 500-row limit that backs {@link GraphData.totalVisibleEntities}, so
 * `totalVisiblePeople > people.length` is the only signal that the roster — and therefore any
 * "Show all N people" affordance — understates the real total.
 */
export interface PeopleList {
  people: PersonCard[];
  totalVisiblePeople: number;
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
/**
 * One selectable model, from `list_models`.
 *
 * `source` is the load-bearing field: `"live"` means the list came off a real endpoint during this
 * call, so a Refresh button does something; `"bundled"` means it was baked into the binary and may
 * be out of date. A bundled catalog is a HINT — an id the user typed that appears in no catalog is
 * a valid custom id, and nothing may clear it.
 */
export interface ModelOption {
  id: string;
  label: string;
  source: "live" | "bundled";
}

/**
 * A connection's catalog plus WHERE IT CAME FROM.
 *
 * Provenance sits on the catalog, not on each option, because the case that matters is the EMPTY
 * one: a gateway or Ollama daemon answering successfully with zero models is exactly when the user
 * wants Refresh, and an empty option list has no option to read a source from.
 */
export interface ModelCatalog {
  source: "live" | "bundled";
  options: ModelOption[];
}

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
  /** True after this device has successfully signed in at least once. */
  accountExpected: boolean;
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
  /** Local durable setup/DELETE cleanup; remote terminal state is unproven and destruction stays paused. */
  revokePending: boolean;
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

/** Owner namespace used by the gated note-attachment commands. */
export type NoteAttachmentOwnerKind = "note" | "task" | "meeting" | "org";

/**
 * One locally-stored image attachment. The DTO deliberately contains no original
 * filename, filesystem path, or capture timestamp: renderers get only an opaque id,
 * verified image metadata, and a browser-safe data URL returned by the gated backend.
 */
export interface NoteAttachmentDto {
  id: string;
  ownerKind: NoteAttachmentOwnerKind;
  ownerId: string;
  mimeType: string;
  extension: string;
  byteLen: number;
  width: number;
  height: number;
  sha256: string;
  dataUrl: string;
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
  /**
   * `"container"` is a Space or folder, added when `list_link_candidates` gained a container leg so
   * one can be picked as an Ask SCOPE. It is NOT a document: it is never a `[[` wikilink target and
   * never pinned as content — the note-editor link picker filters it out, and only a
   * `<mur-source-picker>` that opts into the `container` kind ever renders it.
   */
  kind: "meeting" | "note" | "person" | "entity" | "org" | "container";
  /**
   * For link-candidate `kind === "org"` rows this is the revision-stable
   * `orgId:docId` endpoint composite. Other citation surfaces may still carry a
   * current org item id for direct `/org-item/:id` navigation.
   */
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
  /** Brain's review confidence. Every proposal still requires confirmation. */
  confidence: "high" | "medium" | "low";
  /**
   * Frontend review receipt copied from the plan that produced this move.
   * NotesHome applies only selected moves, so the IPC boundary uses this to
   * preserve the reviewed scope without trusting current UI state.
   */
  reviewScopeFolderId: string | null;
}

/** An auto-organize plan (`plan_organize_notes`). Mirrors the Rust `OrganizePlan`. */
export interface OrganizePlan {
  scopeFolderId: string | null;
  moves: OrganizeMove[];
  totalScanned: number;
  alreadyOrganized: number;
  deferred: number;
  targets: WorkspaceOrganizeTarget[];
}

/** One per-note refusal returned by `apply_organize_plan`. */
export interface OrganizeFailure {
  noteId: string;
  reason: string;
  retryable: boolean;
}

/** Honest auto-organize apply receipt. */
export interface OrganizeApplyResult {
  appliedIds: string[];
  failures: OrganizeFailure[];
}

/** One reviewed recording move proposed by `plan_workspace_organization`. */
export interface WorkspaceOrganizeMove {
  itemId: string;
  title: string;
  fromContainerId: string | null;
  fromContainer: string;
  toContainerId: string;
  toContainer: string;
  reason: string;
}

/** A recording the planner inspected but could not safely classify or move. */
export interface WorkspaceOrganizeSkip {
  itemId: string;
  title: string;
  reason: string;
  code: "notReady" | "emptyNote" | "deferred" | "noDestination";
}

/** Content-bearing recording that Brain deliberately leaves for a human destination choice. */
export interface WorkspaceOrganizeReview {
  itemId: string;
  title: string;
  suggestedTargetId: string | null;
  suggestedTarget: string | null;
  reason: string;
  code: "uncertain" | "noMatch" | "invalidDecision";
}

/** Backend-allowlisted manual destination, labelled with its full hierarchy breadcrumb. */
export interface WorkspaceOrganizeTarget {
  id: string;
  label: string;
}

/** Review-before-apply result for the visible workspace Brain organizer. */
export interface WorkspaceOrganizePlan {
  moves: WorkspaceOrganizeMove[];
  review: WorkspaceOrganizeReview[];
  skipped: WorkspaceOrganizeSkip[];
  targets: WorkspaceOrganizeTarget[];
  totalScanned: number;
}

/** One per-item refusal returned by `apply_workspace_organization`. */
export interface WorkspaceOrganizeFailure {
  itemId: string;
  reason: string;
  /** True only when submitting the same move again can succeed without a fresh plan. */
  retryable: boolean;
}

/** Honest bulk-apply receipt: successes and failures are reported separately. */
export interface WorkspaceOrganizeApplyResult {
  appliedIds: string[];
  failures: WorkspaceOrganizeFailure[];
}

/** Content-free filing-journal health shown when crash recovery needs attention. */
export interface FilingRecoveryStatus {
  degraded: boolean;
  attemptCount: number;
  projectionCount: number;
  sourceSnapshotCount: number;
  /** Opaque, single-issue confirmation token. It contains no id, path, or title. */
  issueToken: string | null;
  /** Content-free reason category; copy only, never recovery authority. */
  issueKind: "externalTargetOccupant" | "externalSourceReplacement" | null;
  canKeepExisting: boolean;
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
  /** Sealed (encrypted) on disk. */
  locked: boolean;
  /**
   * Sealed on disk BUT session-unlocked (decrypted for this session). Mirrors
   * {@link FolderNode.unlocked} — the backend joins the live session set in
   * `list_note_folders`. An open folder is `locked=false, unlocked=false`. Drives
   * the Notes lock gate (`locked && !unlocked`) so unlocking actually lifts it.
   */
  unlocked: boolean;
  /**
   * The reserved always-open note-root that backs the "Notes" section (2026-07-14).
   * Exactly one note-folder has this set. The sidebar tree HIDES it (it IS the
   * section root — where unfiled new notes live), and it can never be locked.
   */
  isRoot: boolean;
  kind: string;
}

// ── Feature C — typed note front-matter properties (a NEW, PARALLEL layer over
// the plaintext `NoteDoc.properties: Record<string,string>`, which is UNCHANGED).
// A folder-level SCHEMA names each property's KIND so the note editor can render
// the right widget and the folder Table/Board views can render typed cells; the
// underlying front-matter string round-trip (`front-matter.ts`) is untouched.
// The pure coercion helpers live beside the editor
// (`features/notes/note-editor/property-field-types.ts`) and re-export these.

/** The kind a note-folder's property schema assigns to a property (drives the widget). */
export type PropertyKind = "text" | "select" | "date" | "checkbox" | "number";

/**
 * One field in a note-folder's property schema (`get_note_folder_schema` /
 * `set_note_folder_schema`). Mirrors the Rust `PropertySchemaField`. `options`
 * is only meaningful for `kind === "select"` (empty for the others). `key`
 * matches the front-matter property key.
 */
export interface PropertySchemaField {
  key: string;
  kind: PropertyKind;
  /** Allowed values for a `select`; empty for other kinds. */
  options: string[];
}

/**
 * A typed property value (mirrors the Rust `PropertyValue`, adjacently tagged
 * `{ kind, value }`). The `value`'s runtime type follows the `kind`:
 * checkbox → boolean, number → number, everything else → string.
 */
export type PropertyValue =
  | { kind: "text"; value: string }
  | { kind: "select"; value: string }
  | { kind: "date"; value: string }
  | { kind: "checkbox"; value: boolean }
  | { kind: "number"; value: number };

/**
 * One row of the typed notes list (`list_notes_typed`). Mirrors the Rust
 * `TypedNoteRow`. `values` is keyed by property key (an absent key = no value
 * for that field). Leak-free: a sealed folder returns `[]` and a masked row
 * carries no `values`/`tags` — the backend gates it, so a locked folder shows
 * no typed view.
 */
export interface TypedNoteRow {
  id: string;
  title: string;
  folderId: string;
  /** Property key → its typed value (absent key = no value). */
  values: Record<string, PropertyValue>;
  tags: string[];
  /** Epoch ms of the last edit. */
  updatedAt: number;
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
  /**
   * PER-INSTANCE org toggle: whether this org contributes content (browsing +
   * brain/assistant context) on THIS Murmur install. `true` by default; flip via
   * {@link IpcService.orgSetContextEnabled}. Disabling never deletes the local
   * replica — it is purely a local, reversible read gate.
   */
  contextEnabled: boolean;
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

/** Result of importing a received Shared Brain replica into a local Workspace. */
export interface OrgItemImportResult {
  kind: "meeting" | "note";
  id: string;
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
  /** Number of referenced images included in the same encrypted share bundle. */
  attachmentCount: number;
  /** Total encoded image bytes included in that bundle. */
  attachmentBytes: number;
  /** Always false today: regex text redaction cannot inspect image pixels. */
  imagePixelsScrubbed: boolean;
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
  /** Stable opaque identity shared by every revision of this document. */
  docId?: string | null;
  kind: "note" | "summary";
  title: string;
  sharedAt: string;
  rev: number;
  /** Organization-wide document access. Historical rows default to `view`. */
  access?: OrgAccess;
  state: "queued" | "uploaded" | "failed" | "revoke_pending" | "revoked";
}

/** Per-document Shared Brain access for active organization members. */
export type OrgAccess = "view" | "edit";

// ── Org Tasks ──────────────────────────────────────────────────────────────

export type TaskStatus = "todo" | "inProgress" | "done";

export interface TaskSubtask {
  id: string;
  title: string;
  done: boolean;
}

/** An encrypted reference to another stable document in the SAME org. */
export interface TaskOrgRef {
  orgId: string;
  docId: string;
}

/** A verified image token whose bytes ride the outer encrypted OrgEnvelope. */
export interface TaskImageRef {
  reference: string;
  alt: string;
}

/** Device-private pointer: this never enters TaskEnvelope or leaves this Mac. */
export interface TaskLocalRef {
  kind: "note" | "meeting" | "dashboard";
  refId: string;
}

export interface TaskDraft {
  orgId: string;
  title: string;
  description: string;
  status: TaskStatus;
  dueAt: string | null;
  assigneeUserId: string | null;
  subtasks: TaskSubtask[];
  orgRefs: TaskOrgRef[];
  images: TaskImageRef[];
  access: OrgAccess;
}

/** One SQLCipher-canonical task projection from the shared org feed. */
export interface OrgTask extends TaskDraft {
  id: string;
  docId: string;
  itemId: string;
  sourceDocumentId: string | null;
  version: number;
  createdAt: string;
  canEdit: boolean;
  canManage: boolean;
  localRefs: TaskLocalRef[];
  updatedAt: string;
}

/** Privacy-minimal assignee option; no full member email crosses this surface. */
export interface TaskAssignee {
  userId: string;
  label: string;
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
 * Which org already holds a LIVE (`uploaded`) share of a given LOCAL source
 * (meeting/note), so the share sheet can mark that org "Already added ✓" and
 * BLOCK a re-share (the double-click duplicate fix). Content-free: only the org
 * id + server item id + rev. Populated by `orgLiveSharesForSource`. Mirrors the
 * Rust `OrgSourceShareStatus`.
 */
export interface OrgSourceShareStatus {
  orgId: string;
  itemId: string | null;
  rev: number;
  /** Current member access for the live document; historical rows default to view. */
  access?: OrgAccess;
  /** Fixed, content-free signal that automatic source updates stopped on a CAS conflict. */
  conflicted: boolean;
}

/**
 * The full decrypted org item for the read-only viewer route (`orgGetItem`).
 * `markdown` is the plaintext envelope body — this is deliberately-disclosed org
 * content (no lock gate applies to org items), rendered read-only with an
 * author + date header. Mirrors the Rust `OrgItemDetail`.
 */
export interface OrgItemDetail {
  itemId: string;
  /** Server doc id; absent on historical rows created before stable identities. */
  docId?: string | null;
  /** Backend-composed private-link identity (`orgId:docId`); absent historically. */
  linkId?: string | null;
  authorHint: string;
  title: string;
  createdAt: string;
  rev: number;
  markdown: string;
  /** Effective organization-wide access selected by the document manager. */
  access: OrgAccess;
  /** Server-authoritative permission to publish a new encrypted revision. */
  canEdit: boolean;
  /** Server-authoritative permission to change access or withdraw the document. */
  canManage: boolean;
  /**
   * Backward-compatible alias of `canEdit`. Stable documents derive authorization from the
   * server-owned document owner/access metadata; historical items without a `docId` may still use
   * their server-authoritative revision author. The viewer saves through `orgUpdateItem` only
   * when this value is true. Mirrors the Rust `OrgItemDetail.editable`.
   */
  editable?: boolean;
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
 * Payload of the `murmur://org-feed-updated` event (`onOrgFeedUpdated`). Any
 * arrival invalidates org replica/head/visibility-derived state and requires a
 * refetch. Content-free — a single count, NO item ids / titles / content.
 * Mirrors the Rust `OrgFeedUpdatedPayload`.
 */
export interface OrgFeedUpdatedPayload {
  /** Number of replicas changed; may be 0 for a local visibility/head invalidation. */
  orgsChanged: number;
}

/**
 * Payload of the `murmur://content-deleted` event (`onContentDeleted`). Emitted
 * once a note/meeting delete has FULLY succeeded on the backend (local rows
 * gone AND any live org shares of it already revoked) — the delete-fan-out fix
 * so OTHER open surfaces (most visibly the tab-strip) learn content vanished
 * even when the delete happened from a different surface than the one showing
 * a stale tab. Content-free: an id + a kind discriminator only, never a title.
 * Mirrors the Rust `ContentDeletedPayload`.
 */
export interface ContentDeletedPayload {
  /** `"note"` | `"meeting"`. */
  kind: "note" | "meeting";
  /** The deleted note's or meeting's id. */
  id: string;
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

// ---------------------------------------------------------------------------
// Saved views over the meetings list (Feature B — Table + Board views)
// ---------------------------------------------------------------------------

/**
 * A user-saved view over the meetings list (Table / Board layout + a
 * persisted filter/sort/columns config). Mirrors the Rust `SavedView`
 * (serde camelCase). `config` is a JSON STRING on the wire — parsed
 * client-side into a {@link ViewConfig} via {@link parseViewConfig}, which
 * NEVER trusts the payload beyond `JSON.parse` (a malformed/partial config
 * degrades to the safe default, it never throws). `scope` is `"meetings"` or
 * `"notes"` — each list surface persists its own view roster (Notes was added
 * 2026-07-14, mirroring Meetings).
 */
export interface SavedView {
  id: string;
  scope: "meetings" | "notes";
  /**
   * Presentation mode. Only `"table"` is created now — Board layout was removed
   * 2026-07-14. `"board"` is still accepted so a legacy row on disk parses; it
   * renders as a table.
   */
  layout: "table" | "board";
  name: string;
  /** JSON-encoded {@link ViewConfig}; parse with {@link parseViewConfig}. */
  config: string;
  sortOrder: number;
  createdAt: string;
  updatedAt: string;
}

/**
 * The parsed shape of {@link SavedView.config}. Purely presentational — the
 * {@link import("../services/view-engine").ViewEngine} applies it CLIENT-SIDE
 * over the already-gated `Meeting[]` the backend returned; it never re-queries
 * or unmasks. `columns` names the visible table columns (see
 * {@link MEETING_VIEW_FIELDS}); `groupBy` is the board's column axis (a field
 * id, e.g. `"status"` or `"tag"`) or null for the table.
 */
export interface ViewConfig {
  filters: ViewFilter[];
  sort: ViewSort[];
  groupBy: string | null;
  columns: string[];
}

/** One filter clause in a {@link ViewConfig}. `value` is unused for the *Empty ops. */
export interface ViewFilter {
  field: string;
  op: "eq" | "neq" | "contains" | "before" | "after" | "isEmpty" | "isNotEmpty";
  value: string;
}

/** One sort key in a {@link ViewConfig} (applied in array order, stable). */
export interface ViewSort {
  field: string;
  direction: "asc" | "desc";
}

/**
 * Per-meeting open/done action-item counts, merged into a Table column and a
 * Board card. Gated exactly like every meeting read: a sealed-and-not-session-
 * unlocked meeting contributes no summary (the backend omits it), so a locked
 * row shows no counts. Mirrors the Rust `MeetingActionSummary`.
 */
export interface MeetingActionSummary {
  meetingId: string;
  openCount: number;
  doneCount: number;
}

/** The safe default config used when a {@link SavedView.config} string can't be parsed. */
export const DEFAULT_VIEW_CONFIG: ViewConfig = {
  filters: [],
  sort: [{ field: "date", direction: "desc" }],
  groupBy: null,
  columns: ["title", "date", "folder", "status", "actions"],
};

/**
 * The default config for a freshly-created NOTES saved view (mirrors
 * {@link DEFAULT_VIEW_CONFIG} but over note fields — see `NOTE_VIEW_FIELDS`).
 * Sorts by last-modified desc, no filters. `columns` is unused by the notes
 * table today (fixed columns), kept for shape-compatibility.
 */
export const DEFAULT_NOTES_VIEW_CONFIG: ViewConfig = {
  filters: [],
  sort: [{ field: "updated", direction: "desc" }],
  groupBy: null,
  columns: ["title", "folder", "updated"],
};

/**
 * One row of the unified Notes content list — a discriminated union over the
 * two sources the pane merges. A `"note"` row is YOUR authored note (opens the
 * editor); an `"org"` row is a READ-ONLY org (Shared Brain) replica (opens the
 * `/org-item/:id` viewer). Both carry an epoch-ms `sortAt` so the merged list
 * orders by date regardless of source; `id` is namespaced (`note:`/`org:`) for
 * a stable, collision-free `@for` / table track key. Lives here (not in
 * `notes-home`) so the notes view engine can filter/sort it without a
 * component→engine import cycle.
 */
export type NotesListItem =
  | {
      kind: "note";
      id: string;
      sortAt: number;
      note: NoteSummary;
    }
  | {
      kind: "org";
      id: string;
      sortAt: number;
      item: OrgItemHeader;
      /** The origin org's display name (drives the "shared brain" badge label). */
      orgName: string;
    };

/**
 * Parse a {@link SavedView.config} JSON string into a {@link ViewConfig},
 * NEVER trusting the wire beyond `JSON.parse`: any parse error, non-object, or
 * missing/mistyped field falls back to a safe default rather than throwing (a
 * corrupt config must never take down the whole meetings list). Unknown filter
 * ops / fields are kept as-is (the ViewEngine treats an unknown op as a no-op),
 * but the container shape is always coerced to valid arrays.
 */
export function parseViewConfig(config: string): ViewConfig {
  let raw: unknown;
  try {
    raw = JSON.parse(config);
  } catch {
    return { ...DEFAULT_VIEW_CONFIG };
  }
  if (typeof raw !== "object" || raw === null) {
    return { ...DEFAULT_VIEW_CONFIG };
  }
  const obj = raw as Record<string, unknown>;
  const filters = Array.isArray(obj["filters"])
    ? (obj["filters"] as unknown[]).filter(
        (f): f is ViewFilter =>
          typeof f === "object" &&
          f !== null &&
          typeof (f as Record<string, unknown>)["field"] === "string" &&
          typeof (f as Record<string, unknown>)["op"] === "string",
      )
    : [];
  const sort = Array.isArray(obj["sort"])
    ? (obj["sort"] as unknown[]).filter(
        (s): s is ViewSort =>
          typeof s === "object" &&
          s !== null &&
          typeof (s as Record<string, unknown>)["field"] === "string" &&
          ((s as Record<string, unknown>)["direction"] === "asc" ||
            (s as Record<string, unknown>)["direction"] === "desc"),
      )
    : [];
  const groupBy =
    typeof obj["groupBy"] === "string" ? (obj["groupBy"] as string) : null;
  const columns = Array.isArray(obj["columns"])
    ? (obj["columns"] as unknown[]).filter(
        (c): c is string => typeof c === "string",
      )
    : [...DEFAULT_VIEW_CONFIG.columns];
  return {
    filters,
    sort,
    groupBy,
    columns: columns.length > 0 ? columns : [...DEFAULT_VIEW_CONFIG.columns],
  };
}

// ── Dashboards (2026-08-03) ─────────────────────────────────────────────────
//
// A board is LAYOUT + POINTERS. The backend resolves every tile through the
// gated readers on each read, so a tile whose source is sealed arrives as
// `{ kind: "locked" }` with NO payload — the renderer has nothing to leak.

/** Cosmetic accent key; the FE maps it to a design token, never a raw colour. */
export type DashboardTint =
  "indigo" | "amber" | "mint" | "orchid" | "azure" | "coral";

/** Every tile kind the backend can store AND resolve. */
export type TileKind =
  | "note"
  | "meeting"
  | "document"
  | "person"
  | "reminders"
  | "drift"
  | "numbers"
  | "pulse"
  | "promises"
  | "living_answer";

export interface Dashboard {
  id: string;
  title: string;
  emoji: string | null;
  tint: DashboardTint | null;
  pinned: boolean;
  position: number;
  createdAt: string;
  updatedAt: string;
}

/** Layout metadata only — what the list view's miniature preview draws. */
export interface TilePreview {
  kind: TileKind;
  span: number;
}

/** One board in the LIST view (the board's own chrome + its layout shape). */
export interface DashboardSummary extends Dashboard {
  tileCount: number;
  tileKinds: TilePreview[];
}

export interface DashboardTile {
  id: string;
  dashboardId: string;
  kind: TileKind;
  refId: string | null;
  /** User-authored heading; `null` ⇒ the resolved data supplies one. */
  title: string | null;
  span: number;
  position: number;
  /** JSON blob, shaped like {@link TileConfig}. */
  config: string | null;
  createdAt: string;
}

/** The `config` bag persisted on a tile. Every field optional. */
export interface TileConfig {
  predicate?: string;
  owner?: string;
  question?: string;
  answer?: string;
  answeredAt?: string;
  /**
   * The sources a cached Living answer was built from. The backend gates the
   * cached answer against these, so a paraphrase never outlives the folder it
   * came from (a legacy answer with no recorded sources is withheld).
   */
  answerSources?: SourceRef[];
}

/** One row inside a list-shaped tile — display-ready, never raw content. */
export interface TileRow {
  text: string;
  meta: string | null;
  /** `ok` | `late` | `due` | `open` | `now` | `old` | free-form ("was $240k"). */
  status: string | null;
  source: SourceRef | null;
}

/**
 * The resolved payload of a tile, discriminated by `kind`.
 *
 * `locked` / `missing` / `unconfigured` carry NOTHING — that is the lock-model
 * contract, asserted backend-side by `locked_tile_serializes_with_no_payload`.
 */
export type TileData =
  | { kind: "locked" }
  | { kind: "missing" }
  | { kind: "unconfigured" }
  | {
      kind: "note";
      id: string;
      title: string;
      snippet: string;
      updatedAt: number;
    }
  | {
      kind: "meeting";
      id: string;
      title: string;
      startedAt: string;
      durationS: number;
      hasAudio: boolean;
    }
  | { kind: "document"; id: string; title: string; snippet: string }
  | {
      kind: "person";
      id: string;
      name: string;
      mentionCount: number;
      openCommitments: number;
    }
  | { kind: "reminders"; rows: TileRow[]; dueCount: number }
  | { kind: "drift"; entity: string; predicate: string; rows: TileRow[] }
  | { kind: "numbers"; entity: string; rows: TileRow[] }
  | {
      kind: "pulse";
      entity: string;
      weekly: number[];
      total: number;
      quietDays: number | null;
    }
  | { kind: "promises"; owner: string | null; rows: TileRow[] }
  | {
      kind: "livingAnswer";
      question: string;
      answer: string | null;
      answeredAt: string | null;
      /** True when a cached answer is being withheld because a source is sealed. */
      withheld: boolean;
    };

/** The only tile payload the backend-owned Living Answer refresh may return. */
export type LivingAnswerTileData = Extract<TileData, { kind: "livingAnswer" }>;

/** A tile plus its resolved (already gated) payload. */
export interface ResolvedTile extends DashboardTile {
  data: TileData;
}

/** One board with every tile resolved. */
export interface DashboardDetail extends Dashboard {
  tiles: ResolvedTile[];
  /** Device-private Task refs, intentionally excluded from board Ask. */
  work: OrgTask[];
}

// ── WORKSPACE HIERARCHY (Projects › Folders › typed item groups) ─────────────
// Mirrors `src-tauri/src/storage/models.rs`. Every field name below is the
// SERIALIZED name the backend emits (its DTOs carry `rename_all = "camelCase"`),
// not a name chosen here — a hand-written interface that disagrees with the wire
// is the #566/#568 failure, where every field read `undefined` and the renderer
// took the view down with it.

/** The four kinds of item a container can hold, in render order. */
export type ItemKind = "meeting" | "note" | "task" | "dashboard";

/** One row inside a container's type group. Carries NO on-disk path, by design. */
export interface ItemRow {
  kind: ItemKind;
  id: string;
  /** `null` for an untitled item — the FE supplies its own placeholder. */
  title: string | null;
  /** Meetings only; `null` for every other kind. */
  durationS: number | null;
  /** Newest-first sort key, epoch MILLISECONDS for every kind. */
  sortAt: number;
}

/** One container's items of one kind: the first page, plus the full visible count. */
export interface TypeGroup {
  kind: ItemKind;
  /** The FULL visible count for this kind — `items` is only the first page. */
  total: number;
  items: ItemRow[];
}

/**
 * A container in the workspace tree: a Project (with child folders) or a Folder.
 *
 * A sealed-and-not-session-unlocked container carries NO groups at all — not even
 * totals — so the tree renders it collapsed and count-free rather than empty.
 */
export interface ContainerNode {
  id: string;
  name: string;
  /** Canonical namespace; drives honest creation/move affordances, never write authority. */
  kind: "meeting" | "note";
  level: "project" | "folder";
  emoji: string | null;
  tint: string | null;
  /** Sealed on disk (the `folders.locked` column). */
  locked: boolean;
  /** Sealed AND session-unlocked (decrypted for this session only). */
  unlocked: boolean;
  /** The reserved always-open note root — it can never be sealed. */
  isRoot: boolean;
  folders: ContainerNode[];
  groups: TypeGroup[];
}

/** One page of a single container's items of a single kind. */
export interface ItemPage {
  kind: ItemKind;
  items: ItemRow[];
  total: number;
}

// ── RELATED PICKER (the gated hierarchy the "Add related" modal walks) ────────
// Mirrors `src-tauri/src/commands/related_picker.rs` + `storage/models.rs`. A
// DELIBERATELY separate family from `ItemKind`/`ContainerNode` above: the global
// `ItemKind` carries `task`/`dashboard`, which this surface must never offer, and
// "four variants of which two are filtered at every call site" is exactly how one
// gets back in. Three variants, no filtering.

/** The three LINKABLE leaf kinds: a recording, an authored note, an imported document. */
export type PickerItemKind = "meeting" | "note" | "document";

/** One linkable leaf row. No path, no snippet — a title and an id are the whole row. */
export interface PickerRow {
  kind: PickerItemKind;
  id: string;
  /** Always present: the backend substitutes the per-kind placeholder. */
  title: string;
}

/** One leaf kind a scope holds, with its true visible total. */
export interface PickerGroup {
  kind: PickerItemKind;
  total: number;
}

/**
 * One container in the picker hierarchy. Metadata only — leaves arrive lazily
 * through {@link IpcService.listRelatedPickerItems}, which is what keeps the
 * bootstrap bounded however large a container grows.
 */
export interface PickerContainerNode {
  id: string;
  name: string;
  level: ContainerLevel;
  emoji: string | null;
  /** Sealed on disk. */
  locked: boolean;
  /** Sealed AND session-unlocked. */
  unlocked: boolean;
  /** Whether this container is a valid `container` link endpoint right now. */
  linkable: boolean;
  /** EMPTY for a sealed-not-unlocked container — not even a zero. */
  groups: PickerGroup[];
  folders: PickerContainerNode[];
}

/** Where the anchor sits, and the bounded window the modal opens on. */
export interface PickerAnchorLocation {
  kind: PickerItemKind;
  /** `null` ⇒ the synthetic "Not classified" node. */
  containerId: string | null;
  /** Ancestors ROOT-FIRST, so exactly that path is expanded and nothing else. */
  path: string[];
  /** The anchor's 0-based position in its `(scope, kind)` ordering. */
  index: number;
  /** Where `items` starts — `index - offset` is the anchor's row inside it. */
  offset: number;
  items: PickerRow[];
  total: number;
}

/** The picker's first frame. */
export interface RelatedPickerBootstrap {
  spaces: PickerContainerNode[];
  /** "Not classified" groups. Disclosure-only — it is not a container and never linkable. */
  unclassified: PickerGroup[];
  /** `null` when the anchor has no place in the local hierarchy (a Shared Brain item). */
  anchor: PickerAnchorLocation | null;
}

/** One lazy page of a scope's leaves. */
export interface RelatedPickerPage {
  kind: PickerItemKind;
  offset: number;
  items: PickerRow[];
  total: number;
}

/** One search hit with its full `Space / folder` breadcrumb. */
export interface RelatedPickerHit {
  kind: PickerItemKind;
  id: string;
  title: string;
  breadcrumb: string[];
}

/** One bounded page of search results. */
export interface RelatedPickerSearchPage {
  offset: number;
  hits: RelatedPickerHit[];
  total: number;
}

// ── Shared containers: a whole Folder or Workspace published to an Org ────────────

/**
 * What sharing a container would publish — counts only, never content.
 * Mirrors the Rust `ContainerSharePreview`.
 */
export interface ContainerSharePreview {
  folderId: string;
  name: string;
  /** `"space"` or `"folder"`. */
  level: string;
  noteCount: number;
  meetingCount: number;
  /** Sub-folders whose own manifest will be published; the root is not counted. */
  folderCount: number;
  /** Sealed descendants deliberately left behind — their content is never read. */
  skippedSealed: number;
  /** Dashboards are not shared yet: one can reference items nobody shared. */
  skippedDashboards: number;
  totalItems: number;
}

/** The outcome of one container share. Mirrors the Rust `ContainerShareResult`. */
export interface ContainerShareResult {
  containerId: string;
  published: number;
  failed: number;
}

/**
 * One container THIS user publishes, for the sidebar's shared marker.
 * Mirrors the Rust `ContainerShareStatus`.
 */
export interface ContainerShareStatus {
  orgId: string;
  orgName: string;
  /** The LOCAL `folders.id`. */
  folderId: string;
  containerId: string;
  access: OrgAccess;
  /** True for the container the user picked; false for a descendant carried along. */
  isRoot: boolean;
  /** `queued` | `published` | `failed` | `revoke_pending` | `revoked`. */
  state: string;
}

/** One received document, as a sidebar row. Mirrors the Rust `SharedItemRow`. */
export interface SharedItemRow {
  itemId: string;
  docId?: string | null;
  title: string;
  /**
   * `"document"` | `"meeting"`, or absent when the sender's client predates the
   * source-kind wire field. Absent means UNCLASSIFIED — never assume a bucket.
   */
  kind?: "document" | "meeting" | null;
  authorHint: string;
  createdAt: string;
  orgId: string;
  orgName: string;
  access: OrgAccess;
  position: number;
}

/**
 * One node of the received forest: a shared Workspace, a shared Folder, or the
 * synthetic Shared Brains root. Mirrors the Rust `SharedContainerNode`.
 */
export interface SharedContainerNode {
  /** Absent only for the synthetic Shared Brains root, which nobody published. */
  containerId?: string | null;
  orgId: string;
  orgName: string;
  name: string;
  level: "space" | "folder" | "virtual";
  emoji?: string | null;
  tint?: string | null;
  access: OrgAccess;
  authorHint: string;
  folders: SharedContainerNode[];
  items: SharedItemRow[];
  /**
   * This device's PRIVATE placement: the local `folders.id` the user filed this
   * under, or absent for "wherever its owner put it". Never published — the
   * owner and every other member see nothing of it.
   */
  localParentId?: string | null;
  position: number;
}

/** Everything shared WITH this user. Mirrors the Rust `SharedWorkspace`. */
export interface SharedWorkspace {
  /** Received Workspaces — each becomes its own top-level sidebar row. */
  spaces: SharedContainerNode[];
  /**
   * Received Folders with no shared-Workspace parent, plus every received item with
   * no container at all — one virtual Workspace so loose shared content has a home.
   */
  sharedBrains: SharedContainerNode;
}

/** What a private placement can point at. */
export type SharedPlacementTarget = "container" | "doc";

/**
 * What the local server for Claude is actually doing.
 *
 * `portInUse` is deliberately its own state, not folded into a generic failure: the user action
 * that fixes it (quit whatever else holds the port) is completely different from the one that
 * fixes `unavailable`, and the listener retries a `portInUse` on its own, so the copy can promise
 * recovery without a restart.
 */
export type McpListenerState =
  | "starting"
  | "listening"
  | "portInUse"
  | "unavailable";

export interface McpStatus {
  state: McpListenerState;
  port: number;
}

/**
 * One LOCAL item this user publishes to an org on its own — not because a
 * container carries it. Drives the sidebar marker on the user's own rows.
 * Mirrors the Rust `OrgShareTargetRow`.
 */
export interface OrgShareTargetRow {
  kind: "meeting" | "note";
  id: string;
  orgId: string;
  orgName: string;
  access: OrgAccess;
}

/**
 * One entry in the Trash — something the user deleted that is still recoverable.
 * Mirrors the Rust `TrashEntry` DTO (camelCase asserted by
 * `trash_tests::trash_entry_dto_is_camel_case`).
 *
 * A sealed entry arrives MASKED: `locked: true`, `label: "🔒 Locked"`, empty
 * `detail`. The backend decides that from the LIVE session unlock set — the FE
 * must never try to infer it, and must not offer Restore on a locked row (the
 * backend refuses it anyway).
 */
export interface TrashEntry {
  /** The TRASH ENTRY id — what restore/delete-forever take. Not `sourceId`. */
  id: string;
  kind: TrashKind;
  /** The deleted entity's own id. */
  sourceId: string;
  sourceFolderId: string | null;
  /** Display title, or the lock sentinel when masked. */
  label: string;
  /** RFC3339. */
  deletedAt: string;
  /** RFC3339 — when this entry is purged. Derived from the LIVE retention setting. */
  expiresAt: string;
  /** Whole days remaining; `0` on the final day, never negative. */
  daysLeft: number;
  /** Masked: its folder is sealed and not unlocked this session. */
  locked: boolean;
  /** Content-FREE one-liner ("30 min · 42 segments"). Empty when masked. */
  detail: string;
}

export type TrashKind = "meeting" | "note" | "folder" | "noteFolder";

/**
 * Payload of `murmur://trash-updated`. CONTENT-FREE by design — a count only, so
 * the sidebar badge can update without any surface learning a label or payload.
 * Mirrors the Rust `TrashUpdatedPayload`.
 */
export interface TrashUpdatedPayload {
  count: number;
}

/**
 * Which generation of the on-device log to read. `current` is this session;
 * `previous` is the run before it — the one a crash happened in, kept because
 * the relaunch that goes looking for it used to destroy it.
 */
export type AppLogSession = "current" | "previous";

/**
 * One parsed line of the `tracing` log file. Mirrors the Rust `LogEntry`
 * (camelCase asserted by `applog::tests::dtos_serialize_camel_case_keys`).
 */
export interface AppLogEntry {
  /** Position in the returned window — the stable `@for` identity. */
  seq: number;
  /** RFC3339 UTC as written, or `null` for a fragment with no header. */
  timestamp: string | null;
  /** `ERROR` | `WARN` | `INFO` | `DEBUG` | `TRACE` | `OTHER`. */
  level: string;
  /** Event target (`murmur::pipeline`, `panic`, …); empty when unparseable. */
  target: string;
  /** Message plus any structured fields; continuation lines are folded in. */
  message: string;
  /** The entry exactly as written in the file — what the expanded row shows. */
  raw: string;
}

/**
 * A window over one log generation. Mirrors the Rust `AppLog`.
 *
 * `exists: false` is the honest first-launch answer for `previous` — it is not
 * an error and must not be rendered as one.
 */
export interface AppLog {
  session: AppLogSession;
  /** Absolute path on disk, so a bug report can name the file. */
  path: string;
  exists: boolean;
  /** Size of the WHOLE file, not of the returned window. */
  sizeBytes: number;
  /** Older entries exist above the window that was returned. */
  truncated: boolean;
  /** Newest last, matching file order. */
  entries: AppLogEntry[];
}
