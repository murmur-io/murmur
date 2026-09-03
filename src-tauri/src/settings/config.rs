use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::storage::Db;

/// Which reasoning backend powers the on-device "brain" pre-analysis step (Flow A,
/// `orchestrate.rs`). The brain is a SEAM — see [`crate::reason::active_reasoner`]:
///
/// - **`Cloud`** (the DEFAULT, the user's choice): the SMARTEST option — the cloud LLM
///   ([`crate::reason::CloudReasoner`]) reasons via the SAME `make_provider` factory the note
///   summary uses, so the egress posture is IDENTICAL (RedactingProvider + fail-closed
///   `cloud_egress_consented` gate). No local model required.
/// - **`Local`**: the on-device GGUF (`MistralReasoner`, e.g. Bielik) when the selected model file
///   is present on disk (mistralrs is always compiled); otherwise the `StubReasoner`.
/// - **`Off`**: the dependency-free `StubReasoner` (the deterministic floor — zero brain).
/// - **`AppleFoundation`** (EXPERIMENTAL, opt-in — the default stays `Cloud`): the on-device Apple
///   Foundation Models reasoner ([`crate::reason::afm`]) via the `meetnotes-afm` Swift sidecar
///   (macOS 26+, Apple Silicon). ON-DEVICE like `Local`: no cloud egress, no consent gate, no
///   redaction wrap. Until a signed macOS-26 build bundles the native sidecar it is ABSENT on every
///   machine, so `AppleFoundation` degrades to the deterministic `StubReasoner` — byte-identical to
///   `Off`/`Local`-without-a-model. Do NOT flip the default to this until on-Mac-verified.
///
/// `#[serde(default)]` on the field ⇒ a config persisted before this field existed loads as
/// `Cloud` (the chosen default). NEVER changes the egress envelope: a Cloud brain that lacks
/// consent simply falls back to the deterministic floor at call time (best-effort).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum BrainBackend {
    /// Cloud LLM via the summarizer-provider seam — the default brain.
    #[default]
    Cloud,
    /// On-device GGUF when present, else the stub.
    Local,
    /// No brain — the deterministic floor.
    Off,
    /// EXPERIMENTAL — on-device Apple Foundation Models via the `meetnotes-afm` sidecar (macOS 26+
    /// Apple Silicon); falls back to the stub when the sidecar/model is absent. Explicit
    /// `#[serde(rename = "apple")]` so the persisted token is the single word `apple` (the container
    /// `rename_all = "lowercase"` would otherwise emit `applefoundation`).
    #[serde(rename = "apple")]
    AppleFoundation,
}

impl BrainBackend {
    /// Stable lowercase token persisted in the settings table (`cloud` | `local` | `off` | `apple`).
    pub fn as_str(self) -> &'static str {
        match self {
            BrainBackend::Cloud => "cloud",
            BrainBackend::Local => "local",
            BrainBackend::Off => "off",
            BrainBackend::AppleFoundation => "apple",
        }
    }

    /// Parse the persisted token; an unknown/empty value falls back to the default (`Cloud`) — so an
    /// OLD build reading a config written by a NEW build (e.g. an `apple` token it doesn't know)
    /// downgrades gracefully to `Cloud`, never crashes.
    pub fn from_str_or_default(s: &str) -> Self {
        match s {
            "local" => BrainBackend::Local,
            "off" => BrainBackend::Off,
            "apple" => BrainBackend::AppleFoundation,
            _ => BrainBackend::Cloud,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    /// default "claude_code"
    pub provider_id: String,
    pub vault_path: Option<String>,
    pub vault_subfolder: Option<String>,
    pub whisper_model_path: Option<String>,
    /// "en" | None=auto
    pub language: Option<String>,
    /// default "claude-opus-4-8"
    pub anthropic_model: String,
    /// Brain/AI model OVERRIDE for the active cloud provider. Empty `""` (the default) means
    /// "use the provider's own default". For `claude_code` it is passed as `--model <id>`; for
    /// `anthropic` it takes precedence over `anthropic_model`. `#[serde(default)]` ⇒ a config
    /// persisted before this field existed loads as `""` (provider default).
    #[serde(default)]
    pub provider_model: String,
    /// Brain/AI reasoning EFFORT for the active cloud provider: `""` (provider default), `"low"`,
    /// `"medium"`, or `"high"`. The direct `anthropic` HTTP provider uses adaptive thinking +
    /// output effort; the isolated `codex_cli` provider forwards it as `model_reasoning_effort`.
    /// The `claude_code` CLI has no effort flag, so it remains a no-op there.
    /// `#[serde(default)]` ⇒ a config persisted before this field existed loads as `""`.
    #[serde(default)]
    pub provider_effort: String,
    /// default "http://localhost:11434"
    pub ollama_base_url: String,
    /// default "llama3.1"
    pub ollama_model: String,
    /// default "claude"
    pub claude_binary: String,
    /// Preferred microphone input device by NAME (cpal exposes no stable id). None = system
    /// default. A saved device that is no longer present falls back to default at capture time.
    pub input_device: Option<String>,
    /// Capture system audio (the other side of the call) via the Core Audio process tap (macOS
    /// 14.4+) or the ScreenCaptureKit sidecar (13–14.3). Default ON (WS8) — a fresh user's first
    /// Zoom/Meet/Teams call captures BOTH sides, which is the differentiated edge and the
    /// diarization/voiceprint keystone. SAFE to default on because capture degrades GRACEFULLY to
    /// mic-only, crash-free, whenever it can't run: no helper bundled/available
    /// (`audio::system::is_available` false ⇒ never attempted), a spawn failure (the start call's
    /// `Err` arm logs + records mic-only), or Screen-Recording (TCC) permission denied — the
    /// fresh-install state — where the helper exits non-zero and `SystemAudioRecorder::stop` returns
    /// `Ok(None)`, so the pipeline runs the mic-only single pass (everything attributed `me`). It
    /// NEVER panics/aborts/fails the recording. The settings-table load treats an absent key as this
    /// new default (`K_CAPTURE_SYSTEM_AUDIO`), so BOTH fresh and existing key-less installs pick it
    /// up; an explicit stored opt-out (`false`) is still honored across reload.
    ///
    /// NOT verifiable headless — SIGNED-MAC ONLY (per the honesty bar): the real macOS
    /// Screen-Recording TCC prompt appearing, that a grant-then-restart actually lights up SCK/tap
    /// capture, genuine dual-stream both-sides capture, and whether capture survives Bluetooth
    /// headphones — all FFI/permission/live-audio, unprovable by `cargo test`.
    pub capture_system_audio: bool,
    /// Voice-activity-detection pre-segmentation + ASR-feed loudness normalisation for the
    /// Accurate batch transcription. Default ON; off = transcribe the whole buffer (legacy).
    pub vad_enabled: bool,
    /// Keep faithful per-stream float32 MASTER archives (mic native + system 48k) alongside the
    /// 16 kHz playback mix. Default OFF — on, it roughly doubles audio disk use per recording.
    pub keep_hires_masters: bool,
    /// Run N-way speaker diarization on the system ("others") stream to label remote speakers
    /// (others-0/1/2). Default ON; requires system-audio capture + downloads ~40 MB of models.
    /// Existing configs that predate the field inherit the same ON default; a stored `false`
    /// remains an explicit opt-out.
    #[serde(default = "default_true")]
    pub diarize_others: bool,
    /// Capture a per-cluster VOICE BIOMETRIC (speaker embedding) for the diarized "others" clusters,
    /// stored on-device (SQLCipher, folder-lock-sealed, NEVER egressed) so a remote speaker can later
    /// be re-identified across meetings once enrolled by name. Requires `diarize_others` to also be
    /// on. Default OFF — capturing a non-consenting participant's voiceprint is an explicit opt-in
    /// (untested under BIPA/CIPA; never default it on). `#[serde(default)]` ⇒ a config persisted
    /// before this field existed loads as `false`.
    #[serde(default)]
    pub voiceprint_enabled: bool,
    /// Echo cancellation (VPIO): capture an AEC'd mic in parallel with cpal and use it as the ASR
    /// feed (the raw cpal mic stays the archive). Default OFF; EXPERIMENTAL — best with speakers.
    pub aec_enabled: bool,
    /// Post-hoc on-device echo cancellation (WebRTC AEC3): after Stop, cancel the captured
    /// system-audio out of the mic track (ASR feed AND playback mix) when a speaker leak is
    /// detected. Pure offline DSP, no live-call impact; falls back to the raw mic on any anomaly.
    /// Fixes the doubled voice when recording on speakers — but rides an UNPROVEN v0.1 AEC3 crate
    /// on every system-audio recording, so it is DEFAULT-OFF until real-Mac-verified. Users can
    /// opt in from Settings.
    /// `#[serde(default)]` ⇒ a config persisted before this field existed loads as `false`
    /// (the safe default) instead of resetting the whole config.
    #[serde(default)]
    pub post_aec_enabled: bool,
    /// Recording-storage cap in GB (`None` = no cap). Drives auto-prune of the OLDEST
    /// recordings' audio when exceeded; notes/transcripts are never touched.
    /// `#[serde(default)]` ⇒ a config persisted before this field existed loads as `None`.
    #[serde(default)]
    pub audio_storage_limit_gb: Option<u32>,
    /// When true AND a cap is set, delete the oldest recordings' audio after each
    /// recording / on save to stay under the cap. OPT-IN, default OFF.
    /// `#[serde(default)]` ⇒ a config persisted before this field existed loads as `false`.
    #[serde(default)]
    pub audio_auto_prune: bool,
    /// Whisper model size: "tiny" | "base" | "small" | "medium" | "large-v3-turbo" |
    /// "large-v3" | the quant variants (e.g. "large-v3-turbo-q8_0"). The DEFAULT is the
    /// machine-conditional T2 flip (`transcribe::model::default_model_size_now`):
    /// `large-v3-turbo-q8_0` when it is already downloaded OR on a FRESH install (no whisper
    /// model on disk) with ≥ 12 GB RAM — measured same batch wall-clock as `small` at far better
    /// Polish (docs/research/2026-07-09-transcription-performance.md); `small` (~470 MB,
    /// RAM-safe) everywhere else, so an existing install never gets a surprise download. All
    /// sizes stay selectable; the chosen model is downloaded on demand via `download_model`.
    pub model_size: String,
    /// OPTIONAL live-caption ASR engine selector: `"whisper"` (the default) or `"parakeet"`. When
    /// `"parakeet"` AND its models are downloaded (`transcribe::model::parakeet_models_present`),
    /// the live caption tick decodes on the CPU-only NVIDIA parakeet engine (off the Metal GPU, so
    /// the brain LLM keeps the GPU) via the `LiveAsr` seam; whisper stays the BATCH authority and
    /// the wake/manual-capture paths. Any other value — or the models being absent — falls back to
    /// whisper (`transcribe::live_asr::should_use_parakeet`), so a mis-set value can never wedge the
    /// loop. `#[serde(default = "default_live_asr_engine")]` ⇒ a config persisted before this field
    /// existed loads as `"whisper"` (byte-identical to today's behavior).
    #[serde(default = "default_live_asr_engine")]
    pub live_asr_engine: String,
    /// Brain-sidecar host-authoritative IDLE-KILL window (seconds): after this long with no
    /// on-device brain request AND nothing in flight, the host kills the `murmur-brain` child to
    /// reclaim ALL its model RAM to the OS. Default 300. `#[serde(default = "…")]` ⇒ a config
    /// persisted before this field existed loads as 300 (byte-identical to the built-in default).
    #[serde(default = "default_brain_idle_timeout_secs")]
    pub brain_idle_timeout_secs: u64,
    /// Brain-sidecar READY-handshake timeout (seconds): the bounded wait for the child's `Ready`
    /// after spawn (model load can be slow on a cold disk / first Metal shader compile). On timeout
    /// the child is killed and the brain call degrades to Cloud/floor — the app never blocks forever.
    /// Default 90. `#[serde(default = "…")]` ⇒ pre-existing configs load as 90.
    #[serde(default = "default_brain_ready_timeout_secs")]
    pub brain_ready_timeout_secs: u64,
    /// Brain-sidecar HARD per-generation cap (seconds) applied when a call carries NO `GenOptions`
    /// timeout (note-gen / the FullyLocal Ask floor). Unlike the old in-process path (unbounded), a
    /// wedged child could hold model RAM forever, so the host guillotines + respawns at this cap.
    /// Default 180. `#[serde(default = "…")]` ⇒ pre-existing configs load as 180.
    #[serde(default = "default_brain_hard_cap_secs")]
    pub brain_hard_cap_secs: u64,
    /// T1.3 (transcription heat) — the LIVE caption tick's model PIN. Non-empty (default
    /// `"small"`) ⇒ while recording, the live loop decodes with THIS size whenever its file is
    /// downloaded, regardless of `model_size` and `brain_live` — live captions are throwaway
    /// (the authoritative transcript is the post-Stop Accurate pass on the CONFIGURED model),
    /// and a `large-v3` live tick saturates the shared Metal GPU for the whole meeting (the
    /// heat complaint). `""` disables the pin → the configured model (today's pre-pin
    /// behavior, incl. the legacy `brain_live` pin-to-small). If the pinned model file is
    /// absent the loop falls back to the configured model (never a dead live loop).
    /// `#[serde(default = "default_live_model_pin")]` ⇒ a config persisted before this field
    /// existed loads as `"small"` (the fix applies to existing installs).
    #[serde(default = "default_live_model_pin")]
    pub live_model_pin: String,
    /// T1.4 (transcription heat) — the Silero VAD TICK GATE for the live caption loop: run the
    /// CPU-only Silero VAD on the newest ~3 s of each tick's window and SKIP the whisper decode
    /// on silent ticks (with a 2-tick hangover). Default ON. Bypassed (always decode) while a
    /// manual voice-capture is armed or a wake-suppression window is active, and disabled
    /// automatically when the VAD model file is absent — so it can never eat a caption the
    /// user's flows need. `#[serde(default = "default_true")]` ⇒ pre-existing configs load ON.
    #[serde(default = "default_true")]
    pub live_vad_gate: bool,
    /// Voice trigger: start recording when a wake phrase is heard. Default off.
    pub voice_trigger: bool,
    /// Whether the first-run onboarding has been completed.
    pub onboarded: bool,
    /// Summary style preset: "standard" | "brief" | "detailed" | "action".
    pub note_style: String,
    /// ENHANCE-MY-NOTES: how the user's typed in-meeting notes shape the summary.
    /// "enhance" (default) — the notes become the SKELETON of the generated note (they ride
    /// INSIDE the redacted provider prompt — a deliberate, loud, consent-riding egress);
    /// "append" — legacy: transcript-only summary + verbatim `## My notes` section.
    /// Empty/unknown values fall back to "enhance".
    /// `#[serde(default)]` ⇒ a config persisted before this field existed loads as `"enhance"`.
    #[serde(default)]
    pub notes_mode: String,
    /// When true, Claude files each note into a thematic subfolder of the vault.
    pub auto_organize: bool,
    /// NOTES feature — the selection Brain-assistant action toggles (Refine / Shorten / Enhance).
    /// Each default ON (`#[serde(default = "default_true")]` ⇒ a config persisted before these
    /// fields existed loads as `true`, and the FE — which sends all three on every save — treats a
    /// missing field as `true`). `note_assistant_action` REFUSES a disabled action with
    /// `AppError::Unavailable`, and the popover hides a disabled action.
    #[serde(default = "default_true")]
    pub note_assist_refine: bool,
    #[serde(default = "default_true")]
    pub note_assist_shorten: bool,
    #[serde(default = "default_true")]
    pub note_assist_enhance: bool,
    /// NOTES feature — the ids of the FULL-SET assistant actions (grammar/expand/tone/…, everything
    /// beyond the three legacy bools) the user has turned OFF. This one opt-OUT list scales to any
    /// number of actions without a column per action: an action is ENABLED unless its id is here.
    /// `custom` is always enabled (the escape hatch) and can never be listed here. Persisted as a
    /// JSON string array (`db.get_setting`/`set_setting` are string-only). `#[serde(default)]` ⇒ a
    /// config persisted before this field existed loads as an empty list (all new actions enabled).
    #[serde(default)]
    pub note_assist_actions_off: Vec<String>,
    /// How to name the `me` capture lane in a generated note's `participants` front-matter.
    ///
    /// Empty (the default) means unset, and the note keeps the bare lane label `me`. This is the
    /// only honest way to put a real name in `participants` for a collapsed-lane recording: the
    /// far side is merged into ONE `others` lane, so who said what over there is genuinely
    /// unknown, and `speaker_attribution_directive_collapsed` rightly forbids guessing it. The
    /// `me` lane is not a guess though — it is the person running the app, and they can simply
    /// say who that is. Configuration, never inference; nothing here loosens the no-fabrication
    /// rule.
    #[serde(default)]
    pub user_display_name: String,
    /// Summary note language: "auto" (match the meeting) | "en" | "pl" | "de" | ... .
    pub note_language: String,
    /// Workspace glossary used to keep domain names stable in generated notes. Each non-empty line
    /// is either `Canonical` or `Canonical = alias, alias`. This is user-authored prompt content:
    /// the pipeline bounds and structures it before use, and the cloud path applies the same
    /// regex + on-device name-redaction firewall as every other prompt field.
    ///
    /// `serde(default)` keeps configs written before this field existed loadable.
    #[serde(default)]
    pub glossary: String,
    /// Require an `Authorization: Bearer <token>` on EVERY MCP method (E3) — including
    /// initialize / tools/list / ping, not just tools/call. Default ON (fail-closed): an
    /// unauthenticated localhost process must not be able to even enumerate the meeting tools.
    /// Bind is always 127.0.0.1.
    pub mcp_require_token: bool,
    /// Let the app ask GitHub, once at launch, whether a newer release exists. Default ON, and
    /// visible — this is the only network call Murmur makes on its own initiative, so it is opt-OUT
    /// rather than silent. Turning it off makes the automatic check send nothing at all: the refusal
    /// is enforced in `check_for_update` before any request is built, not in the frontend.
    ///
    /// The MANUAL "Check for updates" button in Settings ignores this flag on purpose. A user who
    /// presses a button asking GitHub a question has consented to that question by pressing it; the
    /// flag governs what happens WITHOUT them asking.
    ///
    /// Defaulted so an older config that predates this field deserializes to the shipped behaviour
    /// rather than failing to parse — and so a partial write can never erase a user's OFF by
    /// omission.
    #[serde(default = "default_true")]
    pub update_check_enabled: bool,
    /// Require a passing biometric (Touch ID, falling back to device passcode) before unlocking a
    /// sealed folder. Default ON. Degrades to allow when no biometric/passcode policy is available
    /// (no Touch ID hardware / CI), so it never locks the user out.
    pub lock_require_biometric: bool,
    /// Auto-relock all session-unlocked folders (and zeroize the cached KEK) when screen
    /// capture/sharing STARTS. Default ON.
    pub relock_on_screenshare: bool,
    /// E10 — one-time cloud-egress consent. The FIRST time the user would send meeting content
    /// to a cloud LLM (claude_code / anthropic), the egress path refuses until this is flipped
    /// `true` via the dedicated `consent_to_cloud_egress` command. Default OFF (fail-closed): no
    /// content leaves the device until the user has explicitly acknowledged it once.
    ///
    /// SECURITY: this flag DOES round-trip through `AppConfigDto` (so `get_config` can carry the
    /// current value out for the FE to DISPLAY consent status), but `dto_to_config`/`save_config`
    /// IGNORE the incoming DTO value and PRESERVE whatever is already stored — the FE cannot set,
    /// clear, or clobber it as a side effect of a normal settings save (BLK-4). It is mutated ONLY
    /// by the purpose-built `consent_to_cloud_egress` / `revoke_cloud_egress` commands, so flipping
    /// it either way is an explicit, auditable user act.
    pub cloud_egress_consented: bool,
    /// M3-CLIENT — one-time SHARE-egress consent (spec §7 inv. 5). The FIRST time the user would
    /// upload an encrypted note to the sharing server, `share_note_to_link` refuses until this is
    /// flipped `true` via the dedicated `consent_to_share_egress` command. Default OFF (fail-closed):
    /// no note ciphertext leaves the device until the user has explicitly acknowledged the upload once.
    /// This is a DIFFERENT consent class from `cloud_egress_consented` (LLM egress of REDACTED text):
    /// a link share is a deliberate FULL-CONTENT transfer (E2EE, the redaction firewall is NOT applied
    /// — the modal states this). Same discipline: round-trips through the DTO for DISPLAY, but
    /// `dto_to_config`/`save_config` PRESERVE the stored value — the FE can never set/clear it as a
    /// settings-save side effect; only `consent_to_share_egress` / `revoke_share_egress` mutate it.
    /// `#[serde(default)]` ⇒ a config persisted before this field existed loads as `false`
    /// (fail-closed — a pre-existing install has NOT consented to share egress).
    #[serde(default)]
    pub share_egress_consented: bool,
    /// M6 Shared Brain — one-time ORG-egress consent (spec §"App: Tauri commands"). Publishing a note
    /// to an Org Brain uploads an OCK-sealed envelope to the org ciphertext feed — a distinct egress
    /// class from a 1:1 share, so it has its OWN one-time consent. Same PRESERVE-ONLY discipline as
    /// `share_egress_consented`: it round-trips through the DTO for DISPLAY, but
    /// `dto_to_config`/`save_config` PRESERVE the stored value — a settings save can never grant or
    /// clear it; only `consent_to_org_egress` / `revoke_org_egress` mutate it. `#[serde(default)]` ⇒ a
    /// config persisted before this field existed loads as `false` (fail-closed — no org egress until
    /// the user confirms the one-time notice).
    #[serde(default)]
    pub org_egress_consented: bool,
    /// Sharing-onboarding gate — has the user RESOLVED the first-run sharing decision? Either they
    /// chose "use Murmur locally (no account)" OR they went through the account door. A one-way
    /// latch: once `true` it is never auto-cleared, so the init gateway (`/welcome`) never nags a
    /// user who has already decided. It is NOT a consent flag (no egress rides on it) — it purely
    /// suppresses the first-run prompt. Same preserve-only discipline as `share_egress_consented`:
    /// it round-trips through the DTO for DISPLAY (so the gateway gate can read it) but
    /// `dto_to_config`/`save_config` PRESERVE the stored value — a normal settings save can never
    /// set or clear it; the dedicated `mark_sharing_choice_made` command is the ONLY mutator.
    /// `#[serde(default)]` ⇒ a config persisted before this field existed loads as `false`, so a
    /// pre-existing install sees the gateway once (until it makes a choice).
    #[serde(default)]
    pub sharing_choice_made: bool,
    /// brain2 RAG Tier 1 — master gate for the on-device semantic (vector) retrieval layer.
    /// Default ON (`#[serde(default = "default_true")]` ⇒ a config persisted before this field existed
    /// loads as `true`). SAFE to default on: when the e5 model is PRESENT, note chunks auto-index on
    /// creation and Ask / MCP `search_semantic` / related-meetings use hybrid FTS+vector retrieval;
    /// when the model is ABSENT (the common fresh-install state) `should_auto_index` stays false and
    /// every retrieval leg DEGENERATES to the same gated FTS as before — so default-on changes NOTHING
    /// for a model-less install and only lights up once the user downloads e5. The Settings opt-out
    /// persists (a stored `false` is honored across reload; see `semantic_flag_off_round_trips`).
    #[serde(default = "default_true")]
    pub semantic_search_enabled: bool,
    /// Vault Audit Phase 3 — the WEEKLY scheduled audit pass (an hourly due-check inside the
    /// consolidation-loop cadence; see `crate::audit::audit_weekly_tick`). Default ON — the
    /// feature's promise is ambient vault health (the pass is deterministic, zero-egress and
    /// thermal/RAM-gated, so default-on costs a model-less install nothing beyond one weekly
    /// deterministic scan). `#[serde(default = "default_true")]` ⇒ a config persisted before this
    /// field existed loads as `true`. Preserve-only on the settings DTO: the dedicated
    /// `set_audit_schedule` command is the ONLY mutator (a partial/older settings save can never
    /// silently flip the schedule).
    #[serde(default = "default_true")]
    pub vault_audit_weekly_enabled: bool,
    /// brain2 RAG Phase 2 — the SELECTED on-device embedding model id (from
    /// [`crate::embed::EMBED_MODELS`], e.g. `"multilingual-e5-small"` (default) / `"mmlw-retrieval-e5-small"`).
    /// `None`/empty (the default) resolves to `multilingual-e5-small` ⇒ BYTE-IDENTICAL to the
    /// historical hardcoded behavior. Set via the `select_embed_model` command, which also triggers a
    /// re-index (a different model's vectors are not comparable). `#[serde(default)]` ⇒ a config
    /// persisted before this field existed loads as `None`. All bundled options are BERT/384 so
    /// switching costs NO `vec0` schema migration (only a re-embed).
    #[serde(default)]
    pub embed_model_id: Option<String>,
    /// Phase B — optional explicit path to a local reasoning GGUF for the on-device brain
    /// (`MistralReasoner`). `None` (the default) means the resolver falls back to the default model
    /// filename inside the shared models dir. Consulted at runtime when `brain_backend == Local`;
    /// with no model on disk the resolver falls back to the stub. `#[serde(default)]` ⇒ a config
    /// persisted before this field existed loads as `None`.
    #[serde(default)]
    pub brain_model_path: Option<String>,
    /// Phase B (model registry) — the SELECTED on-device brain model id (from `reason::BRAIN_MODELS`,
    /// e.g. `qwen3-1.7b` / `qwen3-4b-instruct-2507` / `bielik-11b-v3`). `None` (the default) means no model is
    /// chosen ⇒ the resolver falls back to the `StubReasoner`. Set via the `select_brain_model`
    /// command. Consulted at runtime when `brain_backend == Local`. `#[serde(default)]` ⇒ a config
    /// persisted before this field existed loads as `None`.
    #[serde(default)]
    pub brain_model_id: Option<String>,
    /// Which reasoning backend powers the brain pre-analysis (Flow A). Default `Cloud` — the
    /// cloud LLM is the smartest reasoner and routes through the SAME `make_provider` egress
    /// envelope as the note summary. `#[serde(default)]` ⇒ a config persisted before this field
    /// existed loads as `Cloud`. See [`BrainBackend`].
    #[serde(default)]
    pub brain_backend: BrainBackend,
    /// Murmur Brain LIVE master switch (spec §2.2). When ON, the on-device LIGHT engine powers
    /// realtime work; durable fact extraction is also local, using HEAVY in Fully Local and LIGHT in
    /// Hybrid. OPT-IN, default OFF. DISTINCT from `realtime_reactions` (the wake-word voice-action
    /// gate) — this gates the model-driven whisper layer and re-routes fact extraction fully
    /// on-device. When OFF the brain behaves EXACTLY as today. `#[serde(default)]` ⇒ a pre-existing
    /// config loads as `false`.
    #[serde(default)]
    pub brain_live: bool,
    /// The selected LIGHT-class on-device model id (realtime reactions and Hybrid fact extraction).
    /// `None` (the default) ⇒ the registry's default light model
    /// (`reason::default_model_for_class`). Resolved LOCAL-or-stub, NEVER cloud (P1 invariant).
    /// `#[serde(default)]`.
    #[serde(default)]
    pub brain_light_model_id: Option<String>,
    /// The selected HEAVY-class on-device model id (local Notes/Ask and Fully Local post-call
    /// analysis). `None` (the default) ⇒ the registry's default heavy model. `#[serde(default)]`.
    #[serde(default)]
    pub brain_heavy_model_id: Option<String>,
    /// Realtime Reactions CONTRADICTION sub-toggle (spec §4.2). Default OFF: contradiction detection
    /// runs in SHADOW mode (counts would-have-fired, emits nothing), so precision is calibrated on the
    /// user's OWN meetings before the ⚠ cards are shown. Flipped ON per-user once the shadow bar clears.
    /// Only meaningful while `brain_live` is on. `#[serde(default)]` ⇒ pre-existing configs load OFF.
    #[serde(default)]
    pub brain_contradiction_cards: bool,
    /// Phase E (Flow B) — the in-meeting VOICE ACTION DISPATCH master gate. When ON, a wake-word
    /// hit in a live caption ("Claudku, zrób research o X") DISPATCHES the parsed action against the
    /// gated vault (research/recall/reminder/note) and emits a live result. OPT-IN, default OFF
    /// (`#[serde(default)]` ⇒ a config persisted before this field existed loads as `false`):
    /// the always-on in-meeting assistant is a privacy + surprise-egress decision — a wake
    /// false-fire must never silently trigger a cloud call or a mid-meeting action. With it OFF the
    /// live loop behaves EXACTLY as before (wake detected + surfaced, NO dispatch).
    #[serde(default)]
    pub realtime_reactions: bool,
    /// brain2 connector framework (Phase F) — master toggle for the WEB SEARCH connector. Default
    /// OFF (`#[serde(default)]` ⇒ a config persisted before this field existed loads as `false`).
    /// When OFF the web connector is ABSENT from the brain's tool registry (no web tool offered), so
    /// shipping this changes NOTHING vs today. Settable from the settings DTO (the Settings UI owns
    /// the toggle). Even when ON, the connector is exposed only once `web_search_consented` is granted
    /// AND a Brave API key is stored — see `connectors::web::WebConnector::from_config_if_available`.
    #[serde(default)]
    pub web_search_enabled: bool,
    /// brain2 connector framework — one-time WEB SEARCH egress consent. The web connector reaches an
    /// EXTERNAL service (a NEW EGRESS CLASS): the outgoing (redacted) query leaves the device. Default
    /// OFF (fail-closed): no query egresses until the user explicitly consents once via the dedicated
    /// `consent_to_web_search` command. Like `cloud_egress_consented`, this is PRESERVE-ONLY on the
    /// settings DTO — `dto_to_config` ignores the incoming value and keeps the stored one, so a normal
    /// settings save can neither grant nor clear it. `#[serde(default)]` ⇒ pre-existing configs load
    /// as `false`.
    #[serde(default)]
    pub web_search_consented: bool,
    /// brain2 connector framework (Phase 2) — master toggle for the JIRA connector (Settings ▸
    /// Connectors). Default OFF (`#[serde(default)]` ⇒ a config persisted before this field existed
    /// loads as `false`). Even when ON, the connector is exposed only once `jira_consented` is granted
    /// AND a base URL + email + API token are configured — see
    /// `connectors::jira::JiraConnector::from_config_if_available`.
    #[serde(default)]
    pub jira_enabled: bool,
    /// brain2 connector framework — one-time JIRA egress consent. The Jira connector reaches an
    /// EXTERNAL service (a NEW EGRESS CLASS): the outgoing (redacted) query leaves the device.
    /// PRESERVE-ONLY: `dto_to_config` ignores the incoming DTO value and a plain `save` never writes
    /// it, so a normal settings save can neither grant nor clear it. Flipped true SOLELY by the
    /// dedicated `consent_to_jira` command. `#[serde(default)]` ⇒ pre-existing configs load as `false`.
    #[serde(default)]
    pub jira_consented: bool,
    /// The Jira Cloud site base URL, e.g. `https://acme.atlassian.net` (non-secret). Default `""`
    /// (unset). `#[serde(default)]` ⇒ pre-existing configs load as `""`.
    #[serde(default)]
    pub jira_base_url: String,
    /// The Atlassian account email paired with the API token for Basic auth (non-secret). Default
    /// `""` (unset). `#[serde(default)]` ⇒ pre-existing configs load as `""`.
    #[serde(default)]
    pub jira_email: String,
    /// brain2 connector framework (Phase 3) — master toggle for the SLACK connector (Settings ▸
    /// Connectors). Default OFF (`#[serde(default)]` ⇒ a config persisted before this field existed
    /// loads as `false`). Even when ON, the connector is exposed only once `slack_consented` is
    /// granted AND a user token is in the Keychain — see
    /// `connectors::slack::SlackConnector::from_config_if_available`.
    #[serde(default)]
    pub slack_enabled: bool,
    /// brain2 connector framework — one-time SLACK egress consent. The Slack connector reaches an
    /// EXTERNAL service (a NEW EGRESS CLASS): the outgoing (redacted) query leaves the device.
    /// PRESERVE-ONLY: `dto_to_config` ignores the incoming DTO value and a plain `save` never writes
    /// it, so a normal settings save can neither grant nor clear it. Flipped true SOLELY by the
    /// dedicated `consent_to_slack` command. `#[serde(default)]` ⇒ pre-existing configs load as `false`.
    #[serde(default)]
    pub slack_consented: bool,
    /// brain2 connector framework — master toggle for the NOTION connector (Settings ▸ Connectors).
    /// Default OFF (`#[serde(default)]` ⇒ a config persisted before this field existed loads as
    /// `false`). Even when ON, the connector is exposed only once `notion_consented` is granted AND
    /// an integration token is in the Keychain — see
    /// `connectors::notion::NotionConnector::from_config_if_available`.
    #[serde(default)]
    pub notion_enabled: bool,
    /// brain2 connector framework — one-time NOTION egress consent. The Notion connector reaches an
    /// EXTERNAL service (a NEW EGRESS CLASS): the outgoing (redacted) query leaves the device.
    /// PRESERVE-ONLY: `dto_to_config` ignores the incoming DTO value and a plain `save` never writes
    /// it, so a normal settings save can neither grant nor clear it. Flipped true SOLELY by the
    /// dedicated `consent_to_notion` command. `#[serde(default)]` ⇒ pre-existing configs load as `false`.
    #[serde(default)]
    pub notion_consented: bool,
    /// brain2 connector framework — master toggle for the CLICKUP connector (Settings ▸ Connectors).
    /// Default OFF (`#[serde(default)]` ⇒ a config persisted before this field existed loads as
    /// `false`). Even when ON, the connector is exposed only once `clickup_consented` is granted AND
    /// a workspace (team) id + API token are configured — see
    /// `connectors::clickup::ClickUpConnector::from_config_if_available`.
    #[serde(default)]
    pub clickup_enabled: bool,
    /// brain2 connector framework — one-time CLICKUP egress consent. Same PRESERVE-ONLY discipline
    /// as `notion_consented`; flipped true SOLELY by the dedicated `consent_to_clickup` command.
    /// `#[serde(default)]` ⇒ pre-existing configs load as `false`.
    #[serde(default)]
    pub clickup_consented: bool,
    /// The ClickUp workspace ("team") id the task search reads, e.g. `9001` (non-secret). Default
    /// `""` (unset). `#[serde(default)]` ⇒ pre-existing configs load as `""`.
    #[serde(default)]
    pub clickup_team_id: String,
    /// Opt-in: restore the OLDER-VERSION behavior of INHERITING the shell environment into the
    /// `claude` CLI subprocess, so env vars set in the user's shell — `ANTHROPIC_API_KEY`,
    /// `ANTHROPIC_BASE_URL`, proxy vars (`HTTPS_PROXY`) — reach the CLI again. The F2 audit hardening
    /// started CLEARING the child env, which broke users who authenticated the CLI via an env API key
    /// ("worked in older versions, exit 1 after update"). Default OFF (`#[serde(default)]` ⇒
    /// pre-existing configs load as `false`) = the hardened, env-cleared behavior. When ON the child
    /// inherits the environment EXCEPT `MURMUR_DEV_DEK` / `MURMUR_DEV_KEK` (the DB encryption keys),
    /// which are ALWAYS stripped — they decrypt the whole library and must NEVER reach a subprocess.
    /// Affects ONLY the `claude_code` provider (the `anthropic` provider uses the Keychain key).
    #[serde(default)]
    pub claude_code_inherit_env: bool,
    /// Base URL of the user's OpenAI-compatible AI gateway (LiteLLM / Kong / Portkey / vLLM / …).
    /// Default `""` (unset). Required when `provider_id == "gateway"`. Must be https:// (or http://
    /// on loopback). Validated + stored via `getConfig`/`saveConfig`; validated again at provider-
    /// construction time by `validate_gateway_url`. `#[serde(default)]` ⇒ pre-existing configs load
    /// as `""` (unset) — no behavioral change for existing installs.
    #[serde(default)]
    pub gateway_base_url: String,
    /// Model id to send to the gateway (e.g. `"gpt-4o"`, `"mistral/mistral-7b"`). Default `""`.
    /// An empty value sends whatever the gateway's default is. `#[serde(default)]` ⇒ pre-existing
    /// configs load as `""`.
    #[serde(default)]
    pub gateway_model: String,
    /// M3-CLIENT — base URL of the Murmur sharing server (spec §7 inv. 9). Default `""` (unset ⇒ the
    /// account/share commands fail closed `Unavailable`). Set in Settings → Account; validated at
    /// `ShareClient::new` exactly like `gateway_base_url` (https required, http loopback-only, no
    /// embedded creds). `#[serde(default)]` ⇒ pre-existing configs load as `""` — no behavior change.
    #[serde(default)]
    pub share_base_url: String,
    /// Proactive brain P1 — while recording, the deterministic ZERO-EGRESS matcher
    /// (`crate::proactive`) surfaces dismissible recall cards ("you discussed this on … →
    /// [[meeting]]", "open commitment: …") from the live-caption tail. Default ON (spec D2: the
    /// conservative threshold + the hard ≥120 s cooldown keep the default quiet); flipping it OFF
    /// mutes the scanner IN THE BACKEND — the matcher never runs, not just UI hiding. No egress
    /// either way: this path never touches a provider or a consent gate.
    /// `#[serde(default = "default_true")]` ⇒ a config persisted before this field existed loads
    /// as `true`.
    #[serde(default = "default_true")]
    pub proactive_hints_enabled: bool,
    /// Cross-meeting USER MEMORY master gate (Phase 3, `crate::user_memory`). Default ON (spec: memory
    /// is a headline capability). When OFF, memory is turned off ENTIRELY in the BACKEND: no user-fact
    /// extraction runs after a meeting (`persist_user_facts_for_meeting` early-returns), NO memory
    /// brief is injected into ANY surface (the @brain agentic loop, Ask, per-meeting chat), and
    /// `get_user_memory` reports the disabled marker — not just UI hiding. Existing facts are NOT
    /// deleted by flipping it (the user can forget/clear them); flipping it back ON resumes injection
    /// from the still-present facts. `#[serde(default = "default_true")]` ⇒ a config persisted before
    /// this field existed loads as `true` (memory stays on for existing installs).
    #[serde(default = "default_true")]
    pub user_memory_enabled: bool,
    /// Brain v2 L2.1 — the hourly memory CONSOLIDATION/REFLECTION job (`crate::memory`): scores
    /// user facts, synthesizes entity/weekly rollups on the LIGHT local reasoner (never cloud) and
    /// exports them to `<vault>/brain/memory/`. Default ON; effective only when `user_memory_enabled`
    /// is also on AND a local light model is present (the stub tick is a no-op). ADDITIVE:
    /// `#[serde(default = "default_true")]` ⇒ configs persisted before this field load as `true`.
    #[serde(default = "default_true")]
    pub memory_consolidation_enabled: bool,
    /// Tier 3b (B) anti-hallucination — DETERMINISTIC GROUNDING of the generated note. When ON, a
    /// pure, on-device, ZERO-EGRESS pass (`crate::summarize::grounding`) runs after summarization and
    /// annotates any summary bullet / action item / prose line whose content words are NOT supported
    /// by this meeting's OWN transcript segments with a non-destructive `> unverified` blockquote
    /// (`> unverified (low audio confidence)` when the best-overlapping segments were acoustically
    /// shaky). NON-DESTRUCTIVE: the original line stays byte-identical (action-item parsing is
    /// unaffected); it only APPENDS a marker. The overlap thresholds remain UNCALIBRATED, so a
    /// marker is a conservative review cue, never proof that a sentence is true or false. Default
    /// ON, but user-disableable in Settings. Existing configs that predate the field inherit the
    /// same ON default; a stored `false` stays OFF.
    #[serde(default = "default_true")]
    pub ground_summary: bool,
    /// Brain v2 L3 — gate for the TINY-schema grammar constraint on the on-device structured
    /// decode (`GenOptions::use_grammar_constraint` → mistralrs `Constraint::JsonSchema`, schemas
    /// < 512 bytes only, graceful fallback to schema-in-prompt). Default OFF / OPT-IN
    /// (`#[serde(default)]` ⇒ `false`): constrained-decode quality on Qwen3-4B needs a real-Mac
    /// spike before this can default on (spec decision #4). ADDITIVE; not on the settings DTO
    /// (preserve-only in `dto_to_config`); round-trips via K_BRAIN_HEAVY_GRAMMAR_ENABLED.
    #[serde(default)]
    pub brain_heavy_grammar_enabled: bool,
    /// Brain v2 L3 — JUST-IN-TIME retrieval for the AGENTIC Ask path: instead of the model
    /// searching blind, its persona is seeded with a compact GATED meeting LISTING
    /// (id | title | date, top-30 hybrid hits, ~80 chars each) + search-then-`get_meeting`
    /// instructions. Default OFF (`#[serde(default)]` ⇒ `false`; spec decision #3: stays off until
    /// the eval run compares JIT vs packed-corpus answer faithfulness) — OFF is BYTE-IDENTICAL to
    /// the legacy agentic prompt. The non-agentic FLOOR keeps its packed corpus either way.
    /// ADDITIVE; not on the settings DTO; round-trips via K_ASK_JIT_RETRIEVAL.
    #[serde(default)]
    pub ask_jit_retrieval: bool,
    /// Brain v2 L3 — deterministic agentic-loop transcript COMPACTION (keep the user request +
    /// last 2 tool results + an "[N earlier results omitted]" marker once the loop transcript
    /// passes 32k chars). Default ON (`#[serde(default = "default_true")]` ⇒ pre-existing configs
    /// load `true`) — the escape hatch exists so a compaction-suspected regression can be ruled
    /// out in the field. ADDITIVE; not on the settings DTO; round-trips via
    /// K_LOOP_TRANSCRIPT_COMPACTION.
    #[serde(default = "default_true")]
    pub loop_transcript_compaction: bool,
    /// Brain v2 L4 — INCREMENTAL LIVE BULLETS: the reactions worker maintains running
    /// `- [topic]: point` notes of the recording in progress on the LOCAL light engine
    /// (`transcribe::bullets`, zero egress). Default ON — the worker itself no-ops to the legacy
    /// behavior when no local light model is present (the stub guard), so ON is safe on every
    /// install; OFF is the field escape hatch (legacy raw-tail reactions substrate, no bullets
    /// row, legacy 6k live inject). ADDITIVE; not on the settings DTO; round-trips via
    /// K_LIVE_BULLETS_ENABLED.
    #[serde(default = "default_true")]
    pub live_bullets_enabled: bool,
    /// Model-role override — the CONNECTION serving the **Notes** role (everything Murmur writes).
    /// `""` (the default, and every pre-role install) = inherit the legacy mapping EXACTLY — see
    /// [`crate::summarize::roles::resolve`]. Values: `claude_code`/`anthropic`/`ollama`/`gateway`
    /// (or `local`/`off` for the reasoner-only roles). ADDITIVE: the legacy keys are never
    /// rewritten; `#[serde(default)]` ⇒ pre-existing configs load as `""` (zero behavior change).
    #[serde(default)]
    pub role_notes_connection: String,
    /// Model-role override — the MODEL for the Notes role. `""` = the connection's own default.
    /// Consulted only when `role_notes_connection` is set (the connection key is the override
    /// switch). `#[serde(default)]` ⇒ `""`.
    #[serde(default)]
    pub role_notes_model: String,
    /// Model-role override — the reasoning EFFORT for the Notes role (`""`/`low`/`medium`/`high`;
    /// honored by `anthropic` and `codex_cli`, like `provider_effort`). Consulted only when
    /// `role_notes_connection` is set. `#[serde(default)]` ⇒ `""`.
    #[serde(default)]
    pub role_notes_effort: String,
    /// Model-role override — the CONNECTION serving the **Ask** role (vault Q&A + meeting chat).
    /// Same semantics as `role_notes_connection`.
    #[serde(default)]
    pub role_ask_connection: String,
    /// Model-role override — the MODEL for the Ask role. Same semantics as `role_notes_model`.
    #[serde(default)]
    pub role_ask_model: String,
    /// Model-role override — the EFFORT for the Ask role. Same semantics as `role_notes_effort`.
    #[serde(default)]
    pub role_ask_effort: String,
    /// Model-role override — the CONNECTION serving the **Live** role (the in-meeting assistant,
    /// @brain threads, voice). Same semantics as `role_notes_connection`.
    #[serde(default)]
    pub role_live_connection: String,
    /// Model-role override — the MODEL for the Live role. Same semantics as `role_notes_model`.
    #[serde(default)]
    pub role_live_model: String,
    /// Model-role override — the EFFORT for the Live role. Same semantics as `role_notes_effort`.
    #[serde(default)]
    pub role_live_effort: String,
}

/// serde default for flags that default ON (mirrors `commands::default_true` for the DTO side).
fn default_true() -> bool {
    true
}

/// serde default for [`AppConfig::live_model_pin`] — pin the live tick to `small` (T1.3).
fn default_live_model_pin() -> String {
    "small".to_string()
}

/// serde default for [`AppConfig::live_asr_engine`] — the whisper live path (today's behavior).
fn default_live_asr_engine() -> String {
    crate::transcribe::live_asr::ENGINE_WHISPER.to_string()
}

/// serde default for [`AppConfig::brain_idle_timeout_secs`] — 300 s (5 min) idle-kill window.
fn default_brain_idle_timeout_secs() -> u64 {
    300
}

/// serde default for [`AppConfig::brain_ready_timeout_secs`] — 90 s ready-handshake bound.
fn default_brain_ready_timeout_secs() -> u64 {
    90
}

/// serde default for [`AppConfig::brain_hard_cap_secs`] — 180 s hard per-generation cap.
fn default_brain_hard_cap_secs() -> u64 {
    180
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            provider_id: "claude_code".to_string(),
            vault_path: None,
            vault_subfolder: None,
            whisper_model_path: None,
            language: None,
            anthropic_model: "claude-opus-4-8".to_string(),
            provider_model: String::new(),
            provider_effort: String::new(),
            ollama_base_url: "http://localhost:11434".to_string(),
            ollama_model: "llama3.1".to_string(),
            claude_binary: "claude".to_string(),
            input_device: None,
            capture_system_audio: true,
            vad_enabled: true,
            keep_hires_masters: false,
            diarize_others: true,
            voiceprint_enabled: false,
            aec_enabled: false,
            post_aec_enabled: false,
            audio_storage_limit_gb: None,
            audio_auto_prune: false,
            // T2 DEFAULT FLIP — machine-conditional (turbo-q8_0 when already downloaded / fresh
            // big-RAM install, else "small"); the ONE decision lives in
            // `transcribe::model::default_model_size`. Onboarding preselects THIS value.
            model_size: crate::transcribe::model::default_model_size_now().to_string(),
            live_asr_engine: default_live_asr_engine(),
            brain_idle_timeout_secs: default_brain_idle_timeout_secs(),
            brain_ready_timeout_secs: default_brain_ready_timeout_secs(),
            brain_hard_cap_secs: default_brain_hard_cap_secs(),
            live_model_pin: default_live_model_pin(),
            live_vad_gate: true,
            voice_trigger: false,
            onboarded: false,
            note_style: "standard".to_string(),
            notes_mode: "enhance".to_string(),
            note_assist_refine: true,
            note_assist_shorten: true,
            note_assist_enhance: true,
            note_assist_actions_off: Vec::new(),
            auto_organize: false,
            user_display_name: String::new(),
            note_language: "auto".to_string(),
            glossary: String::new(),
            mcp_require_token: true,
            update_check_enabled: true,
            lock_require_biometric: true,
            relock_on_screenshare: true,
            cloud_egress_consented: false,
            share_egress_consented: false,
            org_egress_consented: false,
            sharing_choice_made: false,
            semantic_search_enabled: true,
            vault_audit_weekly_enabled: true,
            embed_model_id: None,
            brain_model_path: None,
            brain_model_id: None,
            brain_backend: BrainBackend::default(),
            brain_live: false,
            brain_light_model_id: None,
            brain_heavy_model_id: None,
            brain_contradiction_cards: false,
            realtime_reactions: false,
            web_search_enabled: false,
            web_search_consented: false,
            jira_enabled: false,
            jira_consented: false,
            jira_base_url: String::new(),
            jira_email: String::new(),
            slack_enabled: false,
            slack_consented: false,
            notion_enabled: false,
            notion_consented: false,
            clickup_enabled: false,
            clickup_consented: false,
            clickup_team_id: String::new(),
            claude_code_inherit_env: false,
            gateway_base_url: String::new(),
            gateway_model: String::new(),
            // Hosted sharing relay (murmur-io/murmur-server) — the default so Murmur↔Murmur sharing
            // works out of the box; self-hosters override it in Settings. Swap for the production
            // domain at release (this is the current Railway instance).
            share_base_url: "https://murmur-server-production-b9e8.up.railway.app".to_string(),
            proactive_hints_enabled: true,
            user_memory_enabled: true,
            memory_consolidation_enabled: true,
            ground_summary: true,
            brain_heavy_grammar_enabled: false,
            ask_jit_retrieval: false,
            loop_transcript_compaction: true,
            live_bullets_enabled: true,
            role_notes_connection: String::new(),
            role_notes_model: String::new(),
            role_notes_effort: String::new(),
            role_ask_connection: String::new(),
            role_ask_model: String::new(),
            role_ask_effort: String::new(),
            role_live_connection: String::new(),
            role_live_model: String::new(),
            role_live_effort: String::new(),
        }
    }
}

// Settings-table keys. Kept as constants so `load`/`save` stay in sync.
const K_PROVIDER_ID: &str = "provider_id";
const K_VAULT_PATH: &str = "vault_path";
const K_VAULT_SUBFOLDER: &str = "vault_subfolder";
const K_WHISPER_MODEL_PATH: &str = "whisper_model_path";
const K_LANGUAGE: &str = "language";
const K_ANTHROPIC_MODEL: &str = "anthropic_model";
const K_PROVIDER_MODEL: &str = "provider_model";
const K_PROVIDER_EFFORT: &str = "provider_effort";
const K_OLLAMA_BASE_URL: &str = "ollama_base_url";
const K_OLLAMA_MODEL: &str = "ollama_model";
const K_CLAUDE_BINARY: &str = "claude_binary";
const K_INPUT_DEVICE: &str = "input_device";
const K_CAPTURE_SYSTEM_AUDIO: &str = "capture_system_audio";
const K_VAD_ENABLED: &str = "vad_enabled";
const K_KEEP_HIRES_MASTERS: &str = "keep_hires_masters";
const K_DIARIZE_OTHERS: &str = "diarize_others";
const K_VOICEPRINT_ENABLED: &str = "voiceprint_enabled";
const K_AEC_ENABLED: &str = "aec_enabled";
const K_POST_AEC_ENABLED: &str = "post_aec_enabled";
const K_AUDIO_STORAGE_LIMIT_GB: &str = "audio_storage_limit_gb";
const K_AUDIO_AUTO_PRUNE: &str = "audio_auto_prune";
const K_MODEL_SIZE: &str = "model_size";
const K_LIVE_ASR_ENGINE: &str = "live_asr_engine";
const K_BRAIN_IDLE_TIMEOUT_SECS: &str = "brain_idle_timeout_secs";
const K_BRAIN_READY_TIMEOUT_SECS: &str = "brain_ready_timeout_secs";
const K_BRAIN_HARD_CAP_SECS: &str = "brain_hard_cap_secs";
const K_LIVE_MODEL_PIN: &str = "live_model_pin";
const K_LIVE_VAD_GATE: &str = "live_vad_gate";
const K_VOICE_TRIGGER: &str = "voice_trigger";
const K_ONBOARDED: &str = "onboarded";
const K_NOTE_STYLE: &str = "note_style";
const K_NOTES_MODE: &str = "notes_mode";
const K_NOTE_ASSIST_REFINE: &str = "note_assist_refine";
const K_NOTE_ASSIST_SHORTEN: &str = "note_assist_shorten";
const K_NOTE_ASSIST_ENHANCE: &str = "note_assist_enhance";
const K_NOTE_ASSIST_ACTIONS_OFF: &str = "note_assist_actions_off";
const K_AUTO_ORGANIZE: &str = "auto_organize";
const K_USER_DISPLAY_NAME: &str = "user_display_name";
const K_NOTE_LANGUAGE: &str = "note_language";
const K_GLOSSARY: &str = "glossary";
const K_MCP_REQUIRE_TOKEN: &str = "mcp_require_token";
const K_UPDATE_CHECK_ENABLED: &str = "update_check_enabled";
const K_LOCK_REQUIRE_BIOMETRIC: &str = "lock_require_biometric";
const K_RELOCK_ON_SCREENSHARE: &str = "relock_on_screenshare";
const K_CLOUD_EGRESS_CONSENTED: &str = "cloud_egress_consented";
const K_SHARE_EGRESS_CONSENTED: &str = "share_egress_consented";
const K_ORG_EGRESS_CONSENTED: &str = "org_egress_consented";
const K_SHARING_CHOICE_MADE: &str = "sharing_choice_made";
const K_SEMANTIC_SEARCH_ENABLED: &str = "semantic_search_enabled";
const K_VAULT_AUDIT_WEEKLY_ENABLED: &str = "vault_audit_weekly_enabled";
const K_EMBED_MODEL_ID: &str = "embed_model_id";
const K_BRAIN_MODEL_PATH: &str = "brain_model_path";
const K_BRAIN_MODEL_ID: &str = "brain_model_id";
const K_BRAIN_BACKEND: &str = "brain_backend";
const K_REALTIME_REACTIONS: &str = "realtime_reactions";
const K_BRAIN_LIVE: &str = "brain_live";
const K_BRAIN_LIGHT_MODEL_ID: &str = "brain_light_model_id";
const K_BRAIN_HEAVY_MODEL_ID: &str = "brain_heavy_model_id";
const K_BRAIN_CONTRADICTION_CARDS: &str = "brain_contradiction_cards";
const K_WEB_SEARCH_ENABLED: &str = "web_search_enabled";
const K_WEB_SEARCH_CONSENTED: &str = "web_search_consented";
const K_JIRA_ENABLED: &str = "jira_enabled";
const K_JIRA_CONSENTED: &str = "jira_consented";
const K_JIRA_BASE_URL: &str = "jira_base_url";
const K_JIRA_EMAIL: &str = "jira_email";
const K_SLACK_ENABLED: &str = "slack_enabled";
const K_SLACK_CONSENTED: &str = "slack_consented";
const K_NOTION_ENABLED: &str = "notion_enabled";
const K_NOTION_CONSENTED: &str = "notion_consented";
const K_CLICKUP_ENABLED: &str = "clickup_enabled";
const K_CLICKUP_CONSENTED: &str = "clickup_consented";
const K_CLICKUP_TEAM_ID: &str = "clickup_team_id";
const K_CLAUDE_CODE_INHERIT_ENV: &str = "claude_code_inherit_env";
const K_GATEWAY_BASE_URL: &str = "gateway_base_url";
const K_GATEWAY_MODEL: &str = "gateway_model";
const K_SHARE_BASE_URL: &str = "share_base_url";
const K_PROACTIVE_HINTS_ENABLED: &str = "proactive_hints_enabled";
const K_USER_MEMORY_ENABLED: &str = "user_memory_enabled";
const K_MEMORY_CONSOLIDATION_ENABLED: &str = "memory_consolidation_enabled";
const K_GROUND_SUMMARY: &str = "ground_summary";
const K_BRAIN_HEAVY_GRAMMAR_ENABLED: &str = "brain_heavy_grammar_enabled";
const K_ASK_JIT_RETRIEVAL: &str = "ask_jit_retrieval";
const K_LOOP_TRANSCRIPT_COMPACTION: &str = "loop_transcript_compaction";
const K_LIVE_BULLETS_ENABLED: &str = "live_bullets_enabled";
const K_ROLE_NOTES_CONNECTION: &str = "role_notes_connection";
const K_ROLE_NOTES_MODEL: &str = "role_notes_model";
const K_ROLE_NOTES_EFFORT: &str = "role_notes_effort";
const K_ROLE_ASK_CONNECTION: &str = "role_ask_connection";
const K_ROLE_ASK_MODEL: &str = "role_ask_model";
const K_ROLE_ASK_EFFORT: &str = "role_ask_effort";
/// ACTION-ITEM RECALL NET (opt-in) — see [`action_item_recall_net_enabled`]. Deliberately a
/// STANDALONE settings row rather than an `AppConfig` field: `commands::dto_to_config` builds
/// `AppConfig` as an EXHAUSTIVE struct literal, so a new field would have to be threaded through
/// that file too. Keeping the flag as its own key makes it preserve-only: a normal settings save
/// neither grants nor clears it (`AppConfig::save` never writes this key), and it round-trips
/// durably. Promoting it to a real `AppConfig` field + Settings toggle is a separate follow-up.
const K_ACTION_ITEM_RECALL_NET: &str = "action_item_recall_net";
/// P1 — WHO chose the current `model_size`: `"auto"` (Murmur's recommendation) or `"user"` (a
/// deliberate pick). STANDALONE rows for the same reason as [`K_ACTION_ITEM_RECALL_NET`]: an
/// `AppConfig` field would have to be threaded through `commands::dto_to_config`'s exhaustive
/// struct literal. See [`model_size_source`].
const K_MODEL_SIZE_SOURCE: &str = "model_size_source";
/// P1 — the last-seen [`crate::machine::fingerprint`]. Compared once at startup so a
/// restore-from-backup / Migration Assistant move to a different Mac can re-offer the
/// recommendation.
const K_MACHINE_FINGERPRINT: &str = "machine_fingerprint";
/// P1 — set when the startup compare saw a DIFFERENT machine, cleared when the user dismisses the
/// nudge. A settings ROW, not an event: Tauri does not buffer events and the webview has not called
/// `listen()` during `setup`, so an event emitted there is simply lost. The nudge is a PULL.
const K_MACHINE_CHANGE_PENDING: &str = "machine_change_pending";
const K_ROLE_LIVE_CONNECTION: &str = "role_live_connection";
const K_ROLE_LIVE_MODEL: &str = "role_live_model";
const K_ROLE_LIVE_EFFORT: &str = "role_live_effort";

impl AppConfig {
    /// Read all known keys from the settings table, falling back to `Default` for any
    /// key that has never been written. Empty stored strings for `Option` fields are
    /// treated as `None`.
    pub fn load(db: &Db) -> Result<Self> {
        let mut cfg = AppConfig::default();

        if let Some(v) = db.get_setting(K_PROVIDER_ID)? {
            if !v.is_empty() {
                cfg.provider_id = v;
            }
        }
        cfg.vault_path = opt(db.get_setting(K_VAULT_PATH)?);
        cfg.vault_subfolder = opt(db.get_setting(K_VAULT_SUBFOLDER)?);
        cfg.whisper_model_path = opt(db.get_setting(K_WHISPER_MODEL_PATH)?);
        cfg.language = opt(db.get_setting(K_LANGUAGE)?);
        if let Some(v) = db.get_setting(K_ANTHROPIC_MODEL)? {
            if !v.is_empty() {
                cfg.anthropic_model = v;
            }
        }
        // `""` is a VALID value (= provider default) and also the default, so the stored value
        // is taken verbatim — no non-empty guard (unlike anthropic_model, whose default is a real id).
        if let Some(v) = db.get_setting(K_PROVIDER_MODEL)? {
            cfg.provider_model = v;
        }
        if let Some(v) = db.get_setting(K_PROVIDER_EFFORT)? {
            cfg.provider_effort = v;
        }
        if let Some(v) = db.get_setting(K_OLLAMA_BASE_URL)? {
            if !v.is_empty() {
                cfg.ollama_base_url = v;
            }
        }
        if let Some(v) = db.get_setting(K_OLLAMA_MODEL)? {
            if !v.is_empty() {
                cfg.ollama_model = v;
            }
        }
        if let Some(v) = db.get_setting(K_CLAUDE_BINARY)? {
            if !v.is_empty() {
                cfg.claude_binary = v;
            }
        }
        cfg.input_device = opt(db.get_setting(K_INPUT_DEVICE)?);
        if let Some(v) = db.get_setting(K_CAPTURE_SYSTEM_AUDIO)? {
            cfg.capture_system_audio = v == "true";
        }
        if let Some(v) = db.get_setting(K_VAD_ENABLED)? {
            cfg.vad_enabled = v == "true";
        }
        if let Some(v) = db.get_setting(K_KEEP_HIRES_MASTERS)? {
            cfg.keep_hires_masters = v == "true";
        }
        if let Some(v) = db.get_setting(K_DIARIZE_OTHERS)? {
            cfg.diarize_others = v == "true";
        }
        if let Some(v) = db.get_setting(K_VOICEPRINT_ENABLED)? {
            cfg.voiceprint_enabled = v == "true";
        }
        if let Some(v) = db.get_setting(K_AEC_ENABLED)? {
            cfg.aec_enabled = v == "true";
        }
        if let Some(v) = db.get_setting(K_POST_AEC_ENABLED)? {
            cfg.post_aec_enabled = v == "true";
        }
        if let Some(v) = db.get_setting(K_MODEL_SIZE)? {
            if !v.is_empty() {
                cfg.model_size = v;
            }
        }
        // Live ASR engine: an empty stored value keeps the `"whisper"` default (mirrors
        // `model_size`, not `provider_model` — `""` is never a meaningful engine).
        if let Some(v) = db.get_setting(K_LIVE_ASR_ENGINE)? {
            if !v.is_empty() {
                cfg.live_asr_engine = v;
            }
        }
        // Brain-sidecar timeouts: parse the stored decimal; a missing key OR an unparseable value
        // keeps the built-in default (never a 0-second window that would kill/degrade immediately).
        if let Some(v) = db.get_setting(K_BRAIN_IDLE_TIMEOUT_SECS)? {
            if let Ok(n) = v.parse::<u64>() {
                cfg.brain_idle_timeout_secs = n;
            }
        }
        if let Some(v) = db.get_setting(K_BRAIN_READY_TIMEOUT_SECS)? {
            if let Ok(n) = v.parse::<u64>() {
                cfg.brain_ready_timeout_secs = n;
            }
        }
        if let Some(v) = db.get_setting(K_BRAIN_HARD_CAP_SECS)? {
            if let Ok(n) = v.parse::<u64>() {
                cfg.brain_hard_cap_secs = n;
            }
        }
        // `""` is a VALID stored value (= pin disabled), so it is taken verbatim — only an
        // ABSENT key keeps the `"small"` default (mirrors `provider_model`, not `anthropic_model`).
        if let Some(v) = db.get_setting(K_LIVE_MODEL_PIN)? {
            cfg.live_model_pin = v;
        }
        if let Some(v) = db.get_setting(K_LIVE_VAD_GATE)? {
            cfg.live_vad_gate = v == "true";
        }
        if let Some(v) = db.get_setting(K_VOICE_TRIGGER)? {
            cfg.voice_trigger = v == "true";
        }
        if let Some(v) = db.get_setting(K_ONBOARDED)? {
            cfg.onboarded = v == "true";
        }
        if let Some(v) = db.get_setting(K_NOTE_STYLE)? {
            if !v.is_empty() {
                cfg.note_style = v;
            }
        }
        if let Some(v) = db.get_setting(K_NOTES_MODE)? {
            if !v.is_empty() {
                cfg.notes_mode = v;
            }
        }
        if let Some(v) = db.get_setting(K_AUTO_ORGANIZE)? {
            cfg.auto_organize = v == "true";
        }
        // NOTES assistant action toggles — each defaults ON (a missing setting keeps the Default's
        // `true`, so a config persisted before these existed enables all three).
        if let Some(v) = db.get_setting(K_NOTE_ASSIST_REFINE)? {
            cfg.note_assist_refine = v == "true";
        }
        if let Some(v) = db.get_setting(K_NOTE_ASSIST_SHORTEN)? {
            cfg.note_assist_shorten = v == "true";
        }
        if let Some(v) = db.get_setting(K_NOTE_ASSIST_ENHANCE)? {
            cfg.note_assist_enhance = v == "true";
        }
        // The opt-OUT list is a JSON string array. A malformed / legacy value falls back to an
        // empty list (all actions enabled) rather than erroring the whole config load.
        if let Some(v) = db.get_setting(K_NOTE_ASSIST_ACTIONS_OFF)? {
            cfg.note_assist_actions_off = serde_json::from_str(&v).unwrap_or_default();
        }
        if let Some(v) = db.get_setting(K_USER_DISPLAY_NAME)? {
            cfg.user_display_name = v;
        }
        if let Some(v) = db.get_setting(K_NOTE_LANGUAGE)? {
            if !v.is_empty() {
                cfg.note_language = v;
            }
        }
        if let Some(v) = db.get_setting(K_GLOSSARY)? {
            cfg.glossary = v;
        }
        if let Some(v) = db.get_setting(K_MCP_REQUIRE_TOKEN)? {
            cfg.mcp_require_token = v == "true";
        }
        if let Some(v) = db.get_setting(K_UPDATE_CHECK_ENABLED)? {
            cfg.update_check_enabled = v == "true";
        }
        if let Some(v) = db.get_setting(K_LOCK_REQUIRE_BIOMETRIC)? {
            cfg.lock_require_biometric = v == "true";
        }
        if let Some(v) = db.get_setting(K_RELOCK_ON_SCREENSHARE)? {
            cfg.relock_on_screenshare = v == "true";
        }
        if let Some(v) = db.get_setting(K_CLOUD_EGRESS_CONSENTED)? {
            cfg.cloud_egress_consented = v == "true";
        }
        if let Some(v) = db.get_setting(K_SHARE_EGRESS_CONSENTED)? {
            cfg.share_egress_consented = v == "true";
        }
        if let Some(v) = db.get_setting(K_ORG_EGRESS_CONSENTED)? {
            cfg.org_egress_consented = v == "true";
        }
        if let Some(v) = db.get_setting(K_SHARING_CHOICE_MADE)? {
            cfg.sharing_choice_made = v == "true";
        }
        if let Some(v) = db.get_setting(K_SEMANTIC_SEARCH_ENABLED)? {
            cfg.semantic_search_enabled = v == "true";
        }
        if let Some(v) = db.get_setting(K_VAULT_AUDIT_WEEKLY_ENABLED)? {
            cfg.vault_audit_weekly_enabled = v == "true";
        }
        cfg.embed_model_id = opt(db.get_setting(K_EMBED_MODEL_ID)?);
        // Publish the selection to the process-global seam the zero-arg embedder resolvers read
        // (`embed::active_embedder`/`embed_model_present`/`embed_model_dir`/`download_embed_model`).
        // `None`/empty ⇒ the default model ⇒ byte-identical to the historical behavior.
        crate::embed::set_selected_embed_model_id(cfg.embed_model_id.clone());
        cfg.brain_model_path = opt(db.get_setting(K_BRAIN_MODEL_PATH)?);
        cfg.brain_model_id = opt(db.get_setting(K_BRAIN_MODEL_ID)?);
        if let Some(v) = db.get_setting(K_BRAIN_BACKEND)? {
            if !v.is_empty() {
                cfg.brain_backend = BrainBackend::from_str_or_default(&v);
            }
        }
        if let Some(v) = db.get_setting(K_REALTIME_REACTIONS)? {
            cfg.realtime_reactions = v == "true";
        }
        if let Some(v) = db.get_setting(K_BRAIN_LIVE)? {
            cfg.brain_live = v == "true";
        }
        cfg.brain_light_model_id = opt(db.get_setting(K_BRAIN_LIGHT_MODEL_ID)?);
        cfg.brain_heavy_model_id = opt(db.get_setting(K_BRAIN_HEAVY_MODEL_ID)?);
        if let Some(v) = db.get_setting(K_BRAIN_CONTRADICTION_CARDS)? {
            cfg.brain_contradiction_cards = v == "true";
        }
        if let Some(v) = db.get_setting(K_WEB_SEARCH_ENABLED)? {
            cfg.web_search_enabled = v == "true";
        }
        if let Some(v) = db.get_setting(K_WEB_SEARCH_CONSENTED)? {
            cfg.web_search_consented = v == "true";
        }
        if let Some(v) = db.get_setting(K_JIRA_ENABLED)? {
            cfg.jira_enabled = v == "true";
        }
        if let Some(v) = db.get_setting(K_JIRA_CONSENTED)? {
            cfg.jira_consented = v == "true";
        }
        // `""` is valid (= unset) for the jira string fields, so take the stored value verbatim
        // (mirrors the gateway fields).
        if let Some(v) = db.get_setting(K_JIRA_BASE_URL)? {
            cfg.jira_base_url = v;
        }
        if let Some(v) = db.get_setting(K_JIRA_EMAIL)? {
            cfg.jira_email = v;
        }
        if let Some(v) = db.get_setting(K_SLACK_ENABLED)? {
            cfg.slack_enabled = v == "true";
        }
        if let Some(v) = db.get_setting(K_SLACK_CONSENTED)? {
            cfg.slack_consented = v == "true";
        }
        if let Some(v) = db.get_setting(K_NOTION_ENABLED)? {
            cfg.notion_enabled = v == "true";
        }
        if let Some(v) = db.get_setting(K_NOTION_CONSENTED)? {
            cfg.notion_consented = v == "true";
        }
        if let Some(v) = db.get_setting(K_CLICKUP_ENABLED)? {
            cfg.clickup_enabled = v == "true";
        }
        if let Some(v) = db.get_setting(K_CLICKUP_CONSENTED)? {
            cfg.clickup_consented = v == "true";
        }
        // `""` is valid (= unset) for the ClickUp team id, so take the stored value verbatim
        // (mirrors the jira string fields).
        if let Some(v) = db.get_setting(K_CLICKUP_TEAM_ID)? {
            cfg.clickup_team_id = v;
        }
        if let Some(v) = db.get_setting(K_CLAUDE_CODE_INHERIT_ENV)? {
            cfg.claude_code_inherit_env = v == "true";
        }
        // `""` is valid (= unset) for the gateway fields, so we take the stored value verbatim
        // (no non-empty guard — mirrors `provider_model` rather than `anthropic_model`).
        if let Some(v) = db.get_setting(K_GATEWAY_BASE_URL)? {
            cfg.gateway_base_url = v;
        }
        if let Some(v) = db.get_setting(K_GATEWAY_MODEL)? {
            cfg.gateway_model = v;
        }
        // M3-CLIENT sharing-server base URL — a NON-EMPTY stored value overrides the hosted default;
        // an absent/empty setting keeps the default so sharing works out of the box.
        if let Some(v) = db
            .get_setting(K_SHARE_BASE_URL)?
            .filter(|s| !s.trim().is_empty())
        {
            cfg.share_base_url = v;
        }
        if let Some(v) = db.get_setting(K_PROACTIVE_HINTS_ENABLED)? {
            cfg.proactive_hints_enabled = v == "true";
        }
        if let Some(v) = db.get_setting(K_USER_MEMORY_ENABLED)? {
            cfg.user_memory_enabled = v == "true";
        }
        if let Some(v) = db.get_setting(K_MEMORY_CONSOLIDATION_ENABLED)? {
            cfg.memory_consolidation_enabled = v == "true";
        }
        if let Some(v) = db.get_setting(K_GROUND_SUMMARY)? {
            cfg.ground_summary = v == "true";
        }
        if let Some(v) = db.get_setting(K_BRAIN_HEAVY_GRAMMAR_ENABLED)? {
            cfg.brain_heavy_grammar_enabled = v == "true";
        }
        if let Some(v) = db.get_setting(K_ASK_JIT_RETRIEVAL)? {
            cfg.ask_jit_retrieval = v == "true";
        }
        if let Some(v) = db.get_setting(K_LOOP_TRANSCRIPT_COMPACTION)? {
            cfg.loop_transcript_compaction = v == "true";
        }
        if let Some(v) = db.get_setting(K_LIVE_BULLETS_ENABLED)? {
            cfg.live_bullets_enabled = v == "true";
        }
        // Model-role keys: `""` is a VALID value (= inherit legacy) and also the default, so the
        // stored value is taken verbatim (mirrors `provider_model`, not `anthropic_model`).
        if let Some(v) = db.get_setting(K_ROLE_NOTES_CONNECTION)? {
            cfg.role_notes_connection = v;
        }
        if let Some(v) = db.get_setting(K_ROLE_NOTES_MODEL)? {
            cfg.role_notes_model = v;
        }
        if let Some(v) = db.get_setting(K_ROLE_NOTES_EFFORT)? {
            cfg.role_notes_effort = v;
        }
        if let Some(v) = db.get_setting(K_ROLE_ASK_CONNECTION)? {
            cfg.role_ask_connection = v;
        }
        if let Some(v) = db.get_setting(K_ROLE_ASK_MODEL)? {
            cfg.role_ask_model = v;
        }
        if let Some(v) = db.get_setting(K_ROLE_ASK_EFFORT)? {
            cfg.role_ask_effort = v;
        }
        if let Some(v) = db.get_setting(K_ROLE_LIVE_CONNECTION)? {
            cfg.role_live_connection = v;
        }
        if let Some(v) = db.get_setting(K_ROLE_LIVE_MODEL)? {
            cfg.role_live_model = v;
        }
        if let Some(v) = db.get_setting(K_ROLE_LIVE_EFFORT)? {
            cfg.role_live_effort = v;
        }
        cfg.audio_storage_limit_gb = db
            .get_setting(K_AUDIO_STORAGE_LIMIT_GB)?
            .and_then(|v| v.trim().parse::<u32>().ok())
            .filter(|n| *n > 0);
        if let Some(v) = db.get_setting(K_AUDIO_AUTO_PRUNE)? {
            cfg.audio_auto_prune = v == "true";
        }

        // SANITIZE MODEL IDS ON LOAD, not only on save, and by the SAME rule `dto_to_config`
        // uses: the predicate follows the connection that will SEND the id. The two must agree, or
        // a value one accepts the other clears on the next launch.
        //
        // Load-time matters on its own: a row written before this boundary existed — or by hand —
        // stays hostile until the next save, and in that window `effective_model_requested` reports
        // it to the egress ledger while the wire silently drops it. That is the ledger lying.
        let provider_id = cfg.provider_id.clone();
        let by_connection = [
            // Judged by `provider_id` — the only arm that reads it. It was pinned to
            // `"claude_code"` here on the belief that an explicit CLI role inherits this value;
            // `roles::is_explicit` keys on the connection alone, so it does not, and pinning it
            // cleared a legitimate long Anthropic id on every launch.
            (&mut cfg.provider_model, provider_id.clone()),
            (&mut cfg.role_notes_model, cfg.role_notes_connection.clone()),
            (&mut cfg.role_ask_model, cfg.role_ask_connection.clone()),
            (&mut cfg.role_live_model, cfg.role_live_connection.clone()),
            (&mut cfg.anthropic_model, "anthropic".to_string()),
            (&mut cfg.ollama_model, "ollama".to_string()),
            (&mut cfg.gateway_model, "gateway".to_string()),
        ];
        for (field, connection) in by_connection {
            if field.is_empty() {
                continue;
            }
            // An INHERITED (empty) role connection is judged by transport safety alone, matching
            // `dto_to_config`'s `for_role`. Resolving `""` to `provider_id` here was destructive
            // for the same reason it was there: `roles::is_explicit` keys on the connection key,
            // so an inheriting role never reads `role_*_model`, and applying the default engine's
            // CLI rule cleared a legitimate long id on every launch. The fixed-arm fields below
            // pass their own connection, so they are unaffected.
            let usable = if connection.trim().is_empty() {
                crate::summarize::provider::valid_catalog_model_id(field)
            } else {
                crate::commands::model_predicate_for(connection.trim())(field)
            };
            if !usable {
                tracing::warn!(
                    target: "config",
                    "stored model id is not usable on its connection; clearing to the default"
                );
                field.clear();
            }
        }

        Ok(cfg)
    }

    /// Persist every field into the settings table (NOT the api key — that's Keychain).
    /// `Option` fields persist as an empty string when `None`.
    pub fn save(&self, db: &Db) -> Result<()> {
        db.set_setting(K_PROVIDER_ID, &self.provider_id)?;
        db.set_setting(K_VAULT_PATH, self.vault_path.as_deref().unwrap_or(""))?;
        db.set_setting(
            K_VAULT_SUBFOLDER,
            self.vault_subfolder.as_deref().unwrap_or(""),
        )?;
        db.set_setting(
            K_WHISPER_MODEL_PATH,
            self.whisper_model_path.as_deref().unwrap_or(""),
        )?;
        db.set_setting(K_LANGUAGE, self.language.as_deref().unwrap_or(""))?;
        db.set_setting(K_ANTHROPIC_MODEL, &self.anthropic_model)?;
        db.set_setting(K_PROVIDER_MODEL, &self.provider_model)?;
        db.set_setting(K_PROVIDER_EFFORT, &self.provider_effort)?;
        db.set_setting(K_OLLAMA_BASE_URL, &self.ollama_base_url)?;
        db.set_setting(K_OLLAMA_MODEL, &self.ollama_model)?;
        db.set_setting(K_CLAUDE_BINARY, &self.claude_binary)?;
        db.set_setting(K_INPUT_DEVICE, self.input_device.as_deref().unwrap_or(""))?;
        db.set_setting(
            K_CAPTURE_SYSTEM_AUDIO,
            if self.capture_system_audio {
                "true"
            } else {
                "false"
            },
        )?;
        db.set_setting(
            K_VAD_ENABLED,
            if self.vad_enabled { "true" } else { "false" },
        )?;
        db.set_setting(
            K_KEEP_HIRES_MASTERS,
            if self.keep_hires_masters {
                "true"
            } else {
                "false"
            },
        )?;
        db.set_setting(
            K_DIARIZE_OTHERS,
            if self.diarize_others { "true" } else { "false" },
        )?;
        db.set_setting(
            K_VOICEPRINT_ENABLED,
            if self.voiceprint_enabled {
                "true"
            } else {
                "false"
            },
        )?;
        db.set_setting(
            K_AEC_ENABLED,
            if self.aec_enabled { "true" } else { "false" },
        )?;
        db.set_setting(
            K_POST_AEC_ENABLED,
            if self.post_aec_enabled {
                "true"
            } else {
                "false"
            },
        )?;
        db.set_setting(K_MODEL_SIZE, &self.model_size)?;
        db.set_setting(K_LIVE_ASR_ENGINE, &self.live_asr_engine)?;
        db.set_setting(
            K_BRAIN_IDLE_TIMEOUT_SECS,
            &self.brain_idle_timeout_secs.to_string(),
        )?;
        db.set_setting(
            K_BRAIN_READY_TIMEOUT_SECS,
            &self.brain_ready_timeout_secs.to_string(),
        )?;
        db.set_setting(K_BRAIN_HARD_CAP_SECS, &self.brain_hard_cap_secs.to_string())?;
        db.set_setting(K_LIVE_MODEL_PIN, &self.live_model_pin)?;
        db.set_setting(
            K_LIVE_VAD_GATE,
            if self.live_vad_gate { "true" } else { "false" },
        )?;
        db.set_setting(
            K_VOICE_TRIGGER,
            if self.voice_trigger { "true" } else { "false" },
        )?;
        db.set_setting(K_ONBOARDED, if self.onboarded { "true" } else { "false" })?;
        db.set_setting(K_NOTE_STYLE, &self.note_style)?;
        db.set_setting(K_NOTES_MODE, &self.notes_mode)?;
        db.set_setting(
            K_AUTO_ORGANIZE,
            if self.auto_organize { "true" } else { "false" },
        )?;
        db.set_setting(
            K_NOTE_ASSIST_REFINE,
            if self.note_assist_refine {
                "true"
            } else {
                "false"
            },
        )?;
        db.set_setting(
            K_NOTE_ASSIST_SHORTEN,
            if self.note_assist_shorten {
                "true"
            } else {
                "false"
            },
        )?;
        db.set_setting(
            K_NOTE_ASSIST_ENHANCE,
            if self.note_assist_enhance {
                "true"
            } else {
                "false"
            },
        )?;
        // JSON-encode the opt-out list (settings are string-only). `unwrap_or_default` keeps a
        // serialize hiccup from failing the whole save — an empty array just re-enables all actions.
        db.set_setting(
            K_NOTE_ASSIST_ACTIONS_OFF,
            &serde_json::to_string(&self.note_assist_actions_off).unwrap_or_else(|_| "[]".into()),
        )?;
        db.set_setting(K_USER_DISPLAY_NAME, &self.user_display_name)?;
        db.set_setting(K_NOTE_LANGUAGE, &self.note_language)?;
        db.set_setting(K_GLOSSARY, &self.glossary)?;
        db.set_setting(
            K_MCP_REQUIRE_TOKEN,
            if self.mcp_require_token {
                "true"
            } else {
                "false"
            },
        )?;
        db.set_setting(
            K_UPDATE_CHECK_ENABLED,
            if self.update_check_enabled {
                "true"
            } else {
                "false"
            },
        )?;
        db.set_setting(
            K_LOCK_REQUIRE_BIOMETRIC,
            if self.lock_require_biometric {
                "true"
            } else {
                "false"
            },
        )?;
        db.set_setting(
            K_RELOCK_ON_SCREENSHARE,
            if self.relock_on_screenshare {
                "true"
            } else {
                "false"
            },
        )?;
        db.set_setting(
            K_CLOUD_EGRESS_CONSENTED,
            if self.cloud_egress_consented {
                "true"
            } else {
                "false"
            },
        )?;
        db.set_setting(
            K_SHARE_EGRESS_CONSENTED,
            if self.share_egress_consented {
                "true"
            } else {
                "false"
            },
        )?;
        db.set_setting(
            K_SHARING_CHOICE_MADE,
            if self.sharing_choice_made {
                "true"
            } else {
                "false"
            },
        )?;
        db.set_setting(
            K_SEMANTIC_SEARCH_ENABLED,
            if self.semantic_search_enabled {
                "true"
            } else {
                "false"
            },
        )?;
        db.set_setting(
            K_VAULT_AUDIT_WEEKLY_ENABLED,
            if self.vault_audit_weekly_enabled {
                "true"
            } else {
                "false"
            },
        )?;
        db.set_setting(
            K_EMBED_MODEL_ID,
            self.embed_model_id.as_deref().unwrap_or(""),
        )?;
        db.set_setting(
            K_BRAIN_MODEL_PATH,
            self.brain_model_path.as_deref().unwrap_or(""),
        )?;
        db.set_setting(
            K_BRAIN_MODEL_ID,
            self.brain_model_id.as_deref().unwrap_or(""),
        )?;
        db.set_setting(K_BRAIN_BACKEND, self.brain_backend.as_str())?;
        db.set_setting(
            K_REALTIME_REACTIONS,
            if self.realtime_reactions {
                "true"
            } else {
                "false"
            },
        )?;
        db.set_setting(K_BRAIN_LIVE, if self.brain_live { "true" } else { "false" })?;
        db.set_setting(
            K_BRAIN_LIGHT_MODEL_ID,
            self.brain_light_model_id.as_deref().unwrap_or(""),
        )?;
        db.set_setting(
            K_BRAIN_HEAVY_MODEL_ID,
            self.brain_heavy_model_id.as_deref().unwrap_or(""),
        )?;
        db.set_setting(
            K_BRAIN_CONTRADICTION_CARDS,
            if self.brain_contradiction_cards {
                "true"
            } else {
                "false"
            },
        )?;
        db.set_setting(
            K_WEB_SEARCH_ENABLED,
            if self.web_search_enabled {
                "true"
            } else {
                "false"
            },
        )?;
        db.set_setting(
            K_WEB_SEARCH_CONSENTED,
            if self.web_search_consented {
                "true"
            } else {
                "false"
            },
        )?;
        db.set_setting(
            K_JIRA_ENABLED,
            if self.jira_enabled { "true" } else { "false" },
        )?;
        // PRESERVE-ONLY: `jira_consented` is NEVER written by a plain save — it is persisted solely by
        // `grant_jira_consent`, so a settings save can neither grant nor clear the egress consent
        // (stronger than the DTO-layer preserve; a save carrying `false` can never clobber the grant).
        // The non-secret base URL + email ARE saved verbatim (like the gateway fields); `""` = unset.
        db.set_setting(K_JIRA_BASE_URL, &self.jira_base_url)?;
        db.set_setting(K_JIRA_EMAIL, &self.jira_email)?;
        db.set_setting(
            K_SLACK_ENABLED,
            if self.slack_enabled { "true" } else { "false" },
        )?;
        // PRESERVE-ONLY: `slack_consented` is NEVER written by a plain save — it is persisted solely
        // by `grant_slack_consent`, so a settings save can neither grant nor clear the egress consent
        // (stronger than the DTO-layer preserve; a save carrying `false` can never clobber the grant).
        db.set_setting(
            K_NOTION_ENABLED,
            if self.notion_enabled { "true" } else { "false" },
        )?;
        // PRESERVE-ONLY: `notion_consented` is NEVER written by a plain save — only
        // `grant_notion_consent` persists it (same discipline as jira/slack).
        db.set_setting(
            K_CLICKUP_ENABLED,
            if self.clickup_enabled {
                "true"
            } else {
                "false"
            },
        )?;
        // PRESERVE-ONLY: `clickup_consented` is NEVER written by a plain save — only
        // `grant_clickup_consent` persists it. The non-secret workspace id IS saved verbatim
        // (like the jira base URL/email); `""` = unset.
        db.set_setting(K_CLICKUP_TEAM_ID, &self.clickup_team_id)?;
        db.set_setting(
            K_CLAUDE_CODE_INHERIT_ENV,
            if self.claude_code_inherit_env {
                "true"
            } else {
                "false"
            },
        )?;
        db.set_setting(K_GATEWAY_BASE_URL, &self.gateway_base_url)?;
        db.set_setting(K_GATEWAY_MODEL, &self.gateway_model)?;
        db.set_setting(K_SHARE_BASE_URL, &self.share_base_url)?;
        db.set_setting(
            K_PROACTIVE_HINTS_ENABLED,
            if self.proactive_hints_enabled {
                "true"
            } else {
                "false"
            },
        )?;
        db.set_setting(
            K_USER_MEMORY_ENABLED,
            if self.user_memory_enabled {
                "true"
            } else {
                "false"
            },
        )?;
        db.set_setting(
            K_MEMORY_CONSOLIDATION_ENABLED,
            if self.memory_consolidation_enabled {
                "true"
            } else {
                "false"
            },
        )?;
        db.set_setting(
            K_GROUND_SUMMARY,
            if self.ground_summary { "true" } else { "false" },
        )?;
        db.set_setting(
            K_BRAIN_HEAVY_GRAMMAR_ENABLED,
            if self.brain_heavy_grammar_enabled {
                "true"
            } else {
                "false"
            },
        )?;
        db.set_setting(
            K_ASK_JIT_RETRIEVAL,
            if self.ask_jit_retrieval {
                "true"
            } else {
                "false"
            },
        )?;
        db.set_setting(
            K_LOOP_TRANSCRIPT_COMPACTION,
            if self.loop_transcript_compaction {
                "true"
            } else {
                "false"
            },
        )?;
        db.set_setting(
            K_LIVE_BULLETS_ENABLED,
            if self.live_bullets_enabled {
                "true"
            } else {
                "false"
            },
        )?;
        db.set_setting(K_ROLE_NOTES_CONNECTION, &self.role_notes_connection)?;
        db.set_setting(K_ROLE_NOTES_MODEL, &self.role_notes_model)?;
        db.set_setting(K_ROLE_NOTES_EFFORT, &self.role_notes_effort)?;
        db.set_setting(K_ROLE_ASK_CONNECTION, &self.role_ask_connection)?;
        db.set_setting(K_ROLE_ASK_MODEL, &self.role_ask_model)?;
        db.set_setting(K_ROLE_ASK_EFFORT, &self.role_ask_effort)?;
        db.set_setting(K_ROLE_LIVE_CONNECTION, &self.role_live_connection)?;
        db.set_setting(K_ROLE_LIVE_MODEL, &self.role_live_model)?;
        db.set_setting(K_ROLE_LIVE_EFFORT, &self.role_live_effort)?;
        db.set_setting(
            K_AUDIO_STORAGE_LIMIT_GB,
            &self
                .audio_storage_limit_gb
                .map(|n| n.to_string())
                .unwrap_or_default(),
        )?;
        db.set_setting(
            K_AUDIO_AUTO_PRUNE,
            if self.audio_auto_prune {
                "true"
            } else {
                "false"
            },
        )?;
        Ok(())
    }

    /// E10 — record the user's one-time consent to send meeting content to a cloud LLM provider.
    /// Flips the in-memory flag AND persists it. This is the ONLY supported way to grant consent;
    /// it is deliberately separate from `save_config` so consent can never be granted as an
    /// incidental side effect of a settings write.
    ///
    /// The FE wires a first-cloud-send confirmation prompt that calls the `consent_to_cloud_egress`
    /// Tauri command before the first claude_code/anthropic run. Until the user confirms, the egress
    /// path returns `AppError::Unavailable(errcode::tag(errcode::CLOUD_CONSENT, …))`, and the FE
    /// matches the `[cloud-consent]` CODE (never the prose) to surface the consent dialog.
    ///
    /// FAIL-CLOSED ORDERING — persist FIRST, flip the in-memory flag ONLY on a durable success. If
    /// the write fails we return the error with the session still UNCONSENTED, so we never egress on
    /// a consent that isn't durably recorded. (The inverse — `revoke_cloud_egress` — deliberately
    /// flips first; see its doc.)
    pub fn grant_cloud_egress_consent(&mut self, db: &Db) -> Result<()> {
        db.set_setting(K_CLOUD_EGRESS_CONSENTED, "true")?;
        self.cloud_egress_consented = true;
        Ok(())
    }

    /// E10 — REVOKE the cloud-egress consent. Mirror of [`grant_cloud_egress_consent`]: flips the
    /// in-memory flag AND persists the single key, and is the ONLY supported way to clear consent
    /// (the DTO stays preserve-only in both directions, so a settings save can neither grant nor
    /// revoke). After revoke, every cloud-classified provider build is refused fail-closed
    /// (`AppError::Unavailable`) again — the gate reads the live config per call, so the very next
    /// summarize/ask/reasoner call refuses without a restart.
    ///
    /// FAIL-CLOSED ORDERING (opposite of the grants) — flip the in-memory flag FIRST, THEN persist.
    /// Revoke's safe-failure direction is "stop egressing immediately", so the session must go
    /// unconsented even if the durable write then fails; persist-first would keep the session
    /// egressing on a write error. Both grant and revoke thus fail toward NOT egressing.
    pub fn revoke_cloud_egress(&mut self, db: &Db) -> Result<()> {
        self.cloud_egress_consented = false;
        db.set_setting(K_CLOUD_EGRESS_CONSENTED, "false")
    }

    /// M3-CLIENT — record the user's one-time consent to upload an encrypted note to the sharing
    /// server (spec §7 inv. 5). Mirrors [`grant_cloud_egress_consent`]: persist FIRST, flip the flag
    /// ONLY on a durable write success (fail-closed — never egress on a consent that isn't recorded).
    /// The ONLY mutator that sets this true, so it can never be granted as a settings-save side effect.
    pub fn grant_share_egress_consent(&mut self, db: &Db) -> Result<()> {
        db.set_setting(K_SHARE_EGRESS_CONSENTED, "true")?;
        self.share_egress_consented = true;
        Ok(())
    }

    /// M3-CLIENT — REVOKE the share-egress consent. Mirror of [`revoke_cloud_egress`]: flips the
    /// in-memory flag FIRST, THEN persists (revoke's safe-failure direction is "stop egressing"), so
    /// the next `share_note_to_link` is refused fail-closed until re-consented.
    pub fn revoke_share_egress(&mut self, db: &Db) -> Result<()> {
        self.share_egress_consented = false;
        db.set_setting(K_SHARE_EGRESS_CONSENTED, "false")
    }

    /// M6 Shared Brain — record the user's one-time consent to publish an OCK-sealed note to an Org
    /// Brain feed (a distinct egress class from a 1:1 share). Mirrors [`grant_share_egress_consent`]:
    /// persist FIRST, flip the in-memory flag ONLY on a durable write success (fail-closed — never
    /// egress on a consent that isn't recorded). The ONLY mutator that sets this true.
    pub fn grant_org_egress_consent(&mut self, db: &Db) -> Result<()> {
        db.set_setting(K_ORG_EGRESS_CONSENTED, "true")?;
        self.org_egress_consented = true;
        Ok(())
    }

    /// M6 Shared Brain — REVOKE the org-egress consent. Mirror of [`revoke_share_egress`]: flips the
    /// in-memory flag FIRST, THEN persists (revoke's safe-failure direction is "stop egressing"), so
    /// the next `share_meeting_to_org` / `share_document_to_org` is refused fail-closed until
    /// re-consented.
    pub fn revoke_org_egress(&mut self, db: &Db) -> Result<()> {
        self.org_egress_consented = false;
        db.set_setting(K_ORG_EGRESS_CONSENTED, "false")
    }

    /// Latch that the user has RESOLVED the first-run sharing decision (chose local-only OR went
    /// through the account door), so the init gateway never shows again. Mirrors
    /// [`grant_share_egress_consent`]'s fail-safe ordering: persist FIRST, flip the in-memory flag
    /// ONLY on a durable write success. One-way latch — the ONLY mutator that sets it true, so it
    /// can never be set (or cleared) as a settings-save side effect. Idempotent.
    pub fn set_sharing_choice_made(&mut self, db: &Db) -> Result<()> {
        db.set_setting(K_SHARING_CHOICE_MADE, "true")?;
        self.sharing_choice_made = true;
        Ok(())
    }

    /// Vault Audit Phase 3 — flip the weekly scheduled-audit flag. The ONLY mutator (the field is
    /// preserve-only on the settings DTO, so a generic settings save can never flip it). Mirrors
    /// [`set_sharing_choice_made`]'s fail-safe ordering: persist FIRST, flip the in-memory flag
    /// ONLY on a durable write success. Idempotent.
    pub fn set_vault_audit_weekly(&mut self, db: &Db, enabled: bool) -> Result<()> {
        db.set_setting(
            K_VAULT_AUDIT_WEEKLY_ENABLED,
            if enabled { "true" } else { "false" },
        )?;
        self.vault_audit_weekly_enabled = enabled;
        Ok(())
    }

    /// brain2 connector framework — record the user's one-time consent to send the (redacted) web
    /// search query to an EXTERNAL search service. Mirrors [`grant_cloud_egress_consent`]: flips the
    /// in-memory flag AND persists it. This is the ONLY supported mutator (deliberately separate from
    /// `save_config`), so web-search egress consent can never be granted as an incidental side effect
    /// of a settings write. Until granted, the web connector is absent from the brain's tool registry.
    ///
    /// FAIL-CLOSED ORDERING — persist FIRST, flip the in-memory flag ONLY on a durable success, so a
    /// failed write leaves the session unconsented (no web egress on a consent that wasn't recorded).
    pub fn grant_web_search_consent(&mut self, db: &Db) -> Result<()> {
        db.set_setting(K_WEB_SEARCH_CONSENTED, "true")?;
        self.web_search_consented = true;
        Ok(())
    }

    /// brain2 connectors (Phase 2) — record the user's one-time consent to send the (redacted) Jira
    /// search query to an EXTERNAL service. Mirrors [`grant_web_search_consent`]: the ONLY supported
    /// mutator of `jira_consented` (deliberately separate from `save_config`), so Jira egress consent
    /// can never be granted as an incidental side effect of a settings write. Until granted, the Jira
    /// connector is absent from the brain's tool registry.
    ///
    /// FAIL-CLOSED ORDERING — persist FIRST, flip the in-memory flag ONLY on a durable success, so a
    /// failed write leaves the session unconsented (no Jira egress on a consent that wasn't recorded).
    pub fn grant_jira_consent(&mut self, db: &Db) -> Result<()> {
        db.set_setting(K_JIRA_CONSENTED, "true")?;
        self.jira_consented = true;
        Ok(())
    }

    /// brain2 connectors (Phase 3) — record the user's one-time consent to send the (redacted) Slack
    /// search query to an EXTERNAL service. Mirrors [`grant_jira_consent`]: the ONLY supported mutator
    /// of `slack_consented` (deliberately separate from `save_config`), so Slack egress consent can
    /// never be granted as an incidental side effect of a settings write. Until granted, the Slack
    /// connector is absent from the brain's tool registry.
    ///
    /// FAIL-CLOSED ORDERING — persist FIRST, flip the in-memory flag ONLY on a durable success, so a
    /// failed write leaves the session unconsented (no Slack egress on a consent that wasn't recorded).
    pub fn grant_slack_consent(&mut self, db: &Db) -> Result<()> {
        db.set_setting(K_SLACK_CONSENTED, "true")?;
        self.slack_consented = true;
        Ok(())
    }

    /// brain2 connectors — record the user's one-time consent to send the (redacted) Notion search
    /// query to an EXTERNAL service. Mirrors [`grant_slack_consent`]: the ONLY supported mutator of
    /// `notion_consented`, so Notion egress consent can never be granted as an incidental side
    /// effect of a settings write. Until granted, the Notion connector is absent from the brain's
    /// tool registry.
    ///
    /// FAIL-CLOSED ORDERING — persist FIRST, flip the in-memory flag ONLY on a durable success.
    pub fn grant_notion_consent(&mut self, db: &Db) -> Result<()> {
        db.set_setting(K_NOTION_CONSENTED, "true")?;
        self.notion_consented = true;
        Ok(())
    }

    /// brain2 connectors — record the user's one-time consent to reach ClickUp. Mirrors
    /// [`grant_notion_consent`]: the ONLY supported mutator of `clickup_consented`.
    ///
    /// FAIL-CLOSED ORDERING — persist FIRST, flip the in-memory flag ONLY on a durable success.
    pub fn grant_clickup_consent(&mut self, db: &Db) -> Result<()> {
        db.set_setting(K_CLICKUP_CONSENTED, "true")?;
        self.clickup_consented = true;
        Ok(())
    }
}

/// Map a stored setting to an `Option`: a missing key or an empty string → `None`.
fn opt(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.is_empty())
}

/// ACTION-ITEM RECALL NET — is the opt-in transcript cue scan
/// ([`crate::summarize::recall_net::append_possible_missed_items`]) enabled?
///
/// Default OFF / OPT-IN: this feature APPENDS candidate quotes to the note, unlike grounding's
/// non-destructive review markers. Its lexical cue scan is deliberately LOW-PRECISION, so an
/// always-on version would add noisy "Possible missed items" to the note 100% of users read. It
/// must stay opt-in until calibrated against real meetings. OFF is BYTE-IDENTICAL to the previous
/// pipeline (the scan never runs and the meeting's segments are not even fetched).
///
/// FAIL-CLOSED: an absent key, any value other than `"true"`, or a DB read error all read as OFF —
/// a storage hiccup can never silently switch a note-content feature on.
pub fn action_item_recall_net_enabled(db: &Db) -> bool {
    matches!(
        db.get_setting(K_ACTION_ITEM_RECALL_NET)
            .ok()
            .flatten()
            .as_deref(),
        Some("true")
    )
}

/// Persist the opt-in above. The ONLY mutator (the flag is not on the settings DTO), so it can never
/// be flipped as a side effect of a generic settings save. Idempotent.
pub fn set_action_item_recall_net(db: &Db, enabled: bool) -> Result<()> {
    db.set_setting(
        K_ACTION_ITEM_RECALL_NET,
        if enabled { "true" } else { "false" },
    )
}

/// `model_size_source` = `"auto"` (Murmur's recommendation put it there) or `"user"` (a deliberate
/// pick). Any other stored value — including a blank one — reads as ABSENT, so an unknown token can
/// never masquerade as a deliberate choice.
pub const MODEL_SIZE_SOURCE_AUTO: &str = "auto";
pub const MODEL_SIZE_SOURCE_USER: &str = "user";

/// Read the recorded source of the current `model_size`, or `None` when it was never recorded.
pub fn model_size_source(db: &Db) -> Option<String> {
    db.get_setting(K_MODEL_SIZE_SOURCE)
        .ok()
        .flatten()
        .filter(|v| v == MODEL_SIZE_SOURCE_AUTO || v == MODEL_SIZE_SOURCE_USER)
}

/// Record who chose the current `model_size`. REVERSIBLE, not write-once: "Switch to Sharp" records
/// `"auto"`, the Quality control records `"user"`, and either may overwrite the other. An
/// unrecognised token is rejected (`InvalidArg`) rather than stored, so the row can only ever hold
/// a value [`model_size_source`] will read back.
/// Validation ONLY — split out from [`set_model_size_source`] so a caller can refuse a bad token
/// BEFORE it persists anything else, while deferring the write itself until the `model_size` this
/// row describes is durable. Returns the trimmed, accepted token.
pub fn validate_model_size_source(source: &str) -> Result<&str> {
    let s = source.trim();
    if s != MODEL_SIZE_SOURCE_AUTO && s != MODEL_SIZE_SOURCE_USER {
        return Err(crate::error::AppError::InvalidArg(format!(
            "model_size_source must be \"auto\" or \"user\", got: {s}"
        )));
    }
    Ok(s)
}

pub fn set_model_size_source(db: &Db, source: &str) -> Result<()> {
    let s = validate_model_size_source(source)?;
    db.set_setting(K_MODEL_SIZE_SOURCE, s)
}

/// C9 BACKFILL — stamp an onboarded install's `model_size` as `"user"`, once.
///
/// EXACT REACH, stated plainly because an earlier version of this doc got it wrong: there is NO
/// "predates the row" marker anywhere. This runs from `lib.rs` setup on EVERY launch, so it stamps
/// ANY onboarded install whose row is still absent — a years-old install on its next launch, and
/// equally a brand-new install on the first launch after onboarding completes. Do not describe it
/// as an existing-install-only migration.
///
/// BE HONEST ABOUT WHAT IT CAN KNOW. Provenance was never recorded, so it is **unrecoverable**: a
/// user who deliberately picked `small` and a user who merely accepted what onboarding preselected
/// are byte-identical on disk. Both are marked `"user"`. That is a deliberate choice of failure
/// direction — `"user"` means "never move this selection on the user's behalf", the never-surprise
/// direction — and it must NOT be read as evidence the user actively chose the size.
///
/// The `model_size.trim().is_empty()` guard below is DEFENSIVE ONLY and discriminates nothing in
/// practice: `AppConfig::default()` always seeds `model_size` from `default_model_size_now()`, and
/// `AppConfig::load` only overwrites it when the stored value is non-empty, so the value reaching
/// this function is never blank outside a hand-edited config. Do not build behaviour on it.
///
/// Consequently `"auto"` is written by NOTHING in this tree today — the only non-backfill writer is
/// `save_config_inner`, and no caller sends the token yet.
///
/// FOLLOW-UP OWNED BY THE UI PR: the onboarding write that persists the PRESELECTED recommendation
/// should send `"auto"` explicitly, so a fresh install is not mislabelled as a deliberate choice.
/// Until it does, a fresh install is treated as `"user"`, which is inert but conservative.
///
/// Idempotent: it never overwrites an existing row, so a later `"auto"` cannot be clobbered back to
/// `"user"` on the next launch. Returns `true` when it actually wrote.
pub fn backfill_model_size_source(db: &Db, onboarded: bool, model_size: &str) -> Result<bool> {
    if model_size_source(db).is_some() || !onboarded || model_size.trim().is_empty() {
        return Ok(false);
    }
    set_model_size_source(db, MODEL_SIZE_SOURCE_USER)?;
    Ok(true)
}

/// Whether a machine-change nudge is waiting to be shown.
pub fn machine_change_pending(db: &Db) -> bool {
    matches!(
        db.get_setting(K_MACHINE_CHANGE_PENDING)
            .ok()
            .flatten()
            .as_deref(),
        Some("true")
    )
}

/// Clear the pending machine-change nudge (the user dismissed it).
pub fn clear_machine_change_pending(db: &Db) -> Result<()> {
    db.set_setting(K_MACHINE_CHANGE_PENDING, "false")
}

/// Startup machine-change compare. Returns `true` when this launch newly RAISED the nudge.
///
/// ORDER IS LOAD-BEARING: on a mismatch the pending row is written FIRST and the new fingerprint
/// only afterwards. Writing the fingerprint first (as an earlier design did, at "emit" time) makes
/// the nudge unrecoverable — a crash, or simply a webview that had not yet subscribed, loses the
/// signal forever because the machine now looks unchanged.
///
/// A FIRST-EVER launch (no stored fingerprint) records the fingerprint and raises NOTHING — there
/// is no "change" to report on a machine we have never seen. An unreadable fingerprint (`None`,
/// i.e. RAM could not be probed) is a complete no-op: it must never clear or overwrite a good
/// stored value, and it must never fire a nudge off a missing measurement.
pub fn note_machine_fingerprint(db: &Db, current: Option<&str>) -> Result<bool> {
    let Some(current) = current else {
        return Ok(false);
    };
    let stored = db.get_setting(K_MACHINE_FINGERPRINT)?;
    match stored.as_deref().filter(|s| !s.is_empty()) {
        Some(prev) if prev == current => Ok(false),
        Some(_) => {
            // Raise the nudge BEFORE moving the fingerprint (see the doc comment).
            db.set_setting(K_MACHINE_CHANGE_PENDING, "true")?;
            db.set_setting(K_MACHINE_FINGERPRINT, current)?;
            Ok(true)
        }
        None => {
            db.set_setting(K_MACHINE_FINGERPRINT, current)?;
            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_db() -> Db {
        let p = crate::storage::db::unique_temp_path("meetnotes-config-test", "sqlite");
        // Tests use an explicit key (NOT the Keychain) — Db::open would hit macOS Keychain and
        // prompt/block depending on the test-binary signature.
        Db::open_with_key(
            &p,
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )
        .unwrap()
    }

    /// ENHANCE-MY-NOTES: the mode defaults to "enhance" for fresh installs AND for existing
    /// users (AppConfig::load falls back to Default for a never-written key).
    #[test]
    fn notes_mode_defaults_to_enhance() {
        assert_eq!(AppConfig::default().notes_mode, "enhance");
    }

    /// A1 — offline AEC3 rides an UNPROVEN v0.1 crate on every system-audio recording, so it is
    /// DEFAULT-OFF until real-Mac-verified. Assert both the in-memory struct default AND the
    /// settings-table load path (empty DB / key absent) resolve to `false`. `#[serde(default)]`
    /// (bool default = false) covers a config persisted before the field flip.
    #[test]
    fn post_aec_defaults_off() {
        // In-memory struct default.
        assert!(!AppConfig::default().post_aec_enabled);
        // Settings-table path: empty DB (no key written) loads false.
        let db = temp_db();
        assert!(!AppConfig::load(&db).unwrap().post_aec_enabled);
        // But an explicit opt-in still persists + reloads ON (not a one-way latch).
        let cfg = AppConfig {
            post_aec_enabled: true,
            ..Default::default()
        };
        cfg.save(&db).unwrap();
        assert!(AppConfig::load(&db).unwrap().post_aec_enabled);
    }

    /// WS8 — system-audio capture defaults ON so a fresh user's first Zoom/Meet/Teams call captures
    /// BOTH sides (the differentiated edge + the diarization/voiceprint keystone). Default-on is safe
    /// because capture degrades GRACEFULLY to mic-only when Screen-Recording (TCC) permission is
    /// absent or no helper is bundled — never panics/aborts/fails the recording (see the field doc +
    /// `audio::system` spawn/stop + `audio::merge::merge_streams` single-stream passthrough). Assert
    /// the in-memory struct default AND the settings-table load (empty DB / absent key) resolve to
    /// `true`, and that an explicit opt-out still persists + reloads OFF (not a one-way latch — the
    /// existing user who deliberately turned it off stays off). RED on the pre-flip default (`false`).
    #[test]
    fn capture_system_audio_defaults_on() {
        // In-memory struct default.
        assert!(AppConfig::default().capture_system_audio);
        // Settings-table path: empty DB (no key written) loads the ON default.
        let db = temp_db();
        assert!(AppConfig::load(&db).unwrap().capture_system_audio);
        // But an explicit opt-out still persists + reloads OFF.
        let cfg = AppConfig {
            capture_system_audio: false,
            ..Default::default()
        };
        cfg.save(&db).unwrap();
        assert!(!AppConfig::load(&db).unwrap().capture_system_audio);
    }

    /// A2 + T2 flip — the app default whisper model is the MACHINE-CONDITIONAL resolver's pick
    /// (`transcribe::model::default_model_size_now`): turbo-q8_0 when already downloaded or on a
    /// fresh big-RAM install, `small` (RAM-safe) otherwise. Machine-dependent BY DESIGN, so
    /// assert consistency with the resolver (the ONE decision) rather than a literal, plus the
    /// closed set of sanctioned defaults. `large-v3` (~3 GB) is never a default.
    #[test]
    fn model_size_default_matches_conditional_resolver() {
        let expected = crate::transcribe::model::default_model_size_now();
        assert_eq!(AppConfig::default().model_size, expected);
        // Empty settings table loads the same default.
        let db = temp_db();
        assert_eq!(AppConfig::load(&db).unwrap().model_size, expected);
        // Whatever the machine, the default is one of the two sanctioned sizes.
        assert!(
            expected == "small" || expected == crate::transcribe::model::TURBO_DEFAULT_SIZE,
            "unsanctioned default: {expected}"
        );
    }

    /// T1.3/T1.4 — the live-tick pin defaults to `small` and the VAD tick gate defaults ON
    /// (both on a fresh config AND an empty settings table, so existing installs get the heat
    /// fix), while the explicit opt-outs (`""` pin / gate OFF) persist + reload verbatim.
    #[test]
    fn live_pin_defaults_small_and_vad_gate_on_and_opt_outs_round_trip() {
        assert_eq!(AppConfig::default().live_model_pin, "small");
        assert!(AppConfig::default().live_vad_gate);
        let db = temp_db();
        let loaded = AppConfig::load(&db).unwrap();
        assert_eq!(loaded.live_model_pin, "small");
        assert!(loaded.live_vad_gate);

        // The opt-outs are VALID stored values and must survive a save/load round-trip:
        // `""` = pin disabled (NOT re-defaulted to "small"), `false` = gate off.
        let cfg = AppConfig {
            live_model_pin: String::new(),
            live_vad_gate: false,
            ..Default::default()
        };
        cfg.save(&db).unwrap();
        let reloaded = AppConfig::load(&db).unwrap();
        assert_eq!(reloaded.live_model_pin, "");
        assert!(!reloaded.live_vad_gate);

        // And a quant pin round-trips verbatim too.
        let cfg = AppConfig {
            live_model_pin: "small-q8_0".to_string(),
            ..Default::default()
        };
        cfg.save(&db).unwrap();
        assert_eq!(AppConfig::load(&db).unwrap().live_model_pin, "small-q8_0");
    }

    /// OPTIONAL parakeet live-ASR: `live_asr_engine` defaults to `"whisper"` (fresh struct AND
    /// empty settings table — so today's behavior is unchanged), a `"parakeet"` selection
    /// round-trips through save/load, and an empty stored value falls back to the `"whisper"`
    /// default (never a blank engine).
    #[test]
    fn live_asr_engine_defaults_whisper_and_round_trips() {
        assert_eq!(AppConfig::default().live_asr_engine, "whisper");
        let db = temp_db();
        assert_eq!(AppConfig::load(&db).unwrap().live_asr_engine, "whisper");

        // An explicit parakeet selection persists + reloads.
        let cfg = AppConfig {
            live_asr_engine: "parakeet".to_string(),
            ..Default::default()
        };
        cfg.save(&db).unwrap();
        assert_eq!(AppConfig::load(&db).unwrap().live_asr_engine, "parakeet");

        // An empty stored value falls back to the "whisper" default (non-empty guard).
        let cfg = AppConfig {
            live_asr_engine: String::new(),
            ..Default::default()
        };
        cfg.save(&db).unwrap();
        assert_eq!(AppConfig::load(&db).unwrap().live_asr_engine, "whisper");
    }

    /// A config payload that OMITS `liveAsrEngine` (persisted before the field existed) deserializes
    /// it as `"whisper"` via `#[serde(default = "default_live_asr_engine")]`.
    #[test]
    fn missing_live_asr_engine_deserializes_whisper() {
        let json = r#"{
            "providerId":"claude_code","vaultPath":null,"vaultSubfolder":null,
            "whisperModelPath":null,"language":null,"anthropicModel":"claude-opus-4-8",
            "ollamaBaseUrl":"http://localhost:11434","ollamaModel":"llama3.1","claudeBinary":"claude",
            "inputDevice":null,"captureSystemAudio":false,"vadEnabled":true,"keepHiresMasters":false,
            "diarizeOthers":false,"aecEnabled":false,"postAecEnabled":true,"modelSize":"large-v3","voiceTrigger":false,
            "onboarded":false,"noteStyle":"standard","autoOrganize":false,"noteLanguage":"auto",
            "mcpRequireToken":true,"lockRequireBiometric":true,"relockOnScreenshare":true,
            "cloudEgressConsented":false
        }"#;
        let cfg: AppConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.live_asr_engine, "whisper");
    }

    /// Brain-sidecar timeouts: default to 300/90/180 (fresh struct AND empty settings table so
    /// today's behavior is unchanged), round-trip explicit values through save/load, and a payload
    /// that OMITS them (persisted before the fields existed) deserializes to the defaults via the
    /// `#[serde(default = "…")]` fns.
    #[test]
    fn brain_sidecar_timeouts_default_and_round_trip() {
        let d = AppConfig::default();
        assert_eq!(d.brain_idle_timeout_secs, 300);
        assert_eq!(d.brain_ready_timeout_secs, 90);
        assert_eq!(d.brain_hard_cap_secs, 180);

        let db = temp_db();
        let loaded = AppConfig::load(&db).unwrap();
        assert_eq!(loaded.brain_idle_timeout_secs, 300);
        assert_eq!(loaded.brain_ready_timeout_secs, 90);
        assert_eq!(loaded.brain_hard_cap_secs, 180);

        // Explicit values persist + reload verbatim.
        let cfg = AppConfig {
            brain_idle_timeout_secs: 120,
            brain_ready_timeout_secs: 45,
            brain_hard_cap_secs: 240,
            ..Default::default()
        };
        cfg.save(&db).unwrap();
        let r = AppConfig::load(&db).unwrap();
        assert_eq!(r.brain_idle_timeout_secs, 120);
        assert_eq!(r.brain_ready_timeout_secs, 45);
        assert_eq!(r.brain_hard_cap_secs, 240);

        // A JSON payload omitting the brain-timeout keys deserializes to the defaults.
        let json = r#"{
            "providerId":"claude_code","vaultPath":null,"vaultSubfolder":null,
            "whisperModelPath":null,"language":null,"anthropicModel":"claude-opus-4-8",
            "ollamaBaseUrl":"http://localhost:11434","ollamaModel":"llama3.1","claudeBinary":"claude",
            "inputDevice":null,"captureSystemAudio":false,"vadEnabled":true,"keepHiresMasters":false,
            "diarizeOthers":false,"aecEnabled":false,"postAecEnabled":true,"modelSize":"large-v3","voiceTrigger":false,
            "onboarded":false,"noteStyle":"standard","autoOrganize":false,"noteLanguage":"auto",
            "mcpRequireToken":true,"lockRequireBiometric":true,"relockOnScreenshare":true,
            "cloudEgressConsented":false
        }"#;
        let cfg: AppConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.brain_idle_timeout_secs, 300);
        assert_eq!(cfg.brain_ready_timeout_secs, 90);
        assert_eq!(cfg.brain_hard_cap_secs, 180);
    }

    #[test]
    fn load_returns_defaults_on_empty_db() {
        let db = temp_db();
        let cfg = AppConfig::load(&db).unwrap();
        assert_eq!(cfg.provider_id, "claude_code");
        assert_eq!(cfg.anthropic_model, "claude-opus-4-8");
        assert!(cfg.vault_path.is_none());
        // Stage E security flags default ON.
        assert!(cfg.lock_require_biometric);
        assert!(cfg.relock_on_screenshare);
        // E10 cloud-egress consent is fail-closed (OFF) until explicitly granted.
        assert!(!cfg.cloud_egress_consented);
        // Tier 1: semantic search defaults ON; graceful FTS fallback until the e5 model is downloaded.
        assert!(cfg.semantic_search_enabled);
    }

    /// TIER 1: a config whose JSON omits `semantic_search_enabled` now loads it as `true`
    /// (`#[serde(default = "default_true")]` for the JSON path), and an empty settings DB loads the ON
    /// struct default. The forward-compat path matches the struct default so old payloads pick up the
    /// new default. NOTE: an existing install that already PERSISTED `false` (any prior Settings save
    /// wrote the key) keeps FTS-only until re-enabled — reaching that installed base is a deliberate,
    /// reversible follow-up (a guarded one-time settings flip), intentionally NOT shipped in this change.
    #[test]
    fn missing_semantic_flag_defaults_on() {
        // JSON path: deserialize a payload that omits the field entirely.
        let json = r#"{
            "providerId":"claude_code","vaultPath":null,"vaultSubfolder":null,
            "whisperModelPath":null,"language":null,"anthropicModel":"claude-opus-4-8",
            "ollamaBaseUrl":"http://localhost:11434","ollamaModel":"llama3.1","claudeBinary":"claude",
            "inputDevice":null,"captureSystemAudio":false,"vadEnabled":true,"keepHiresMasters":false,
            "diarizeOthers":false,"aecEnabled":false,"postAecEnabled":true,"modelSize":"large-v3","voiceTrigger":false,
            "onboarded":false,"noteStyle":"standard","autoOrganize":false,"noteLanguage":"auto",
            "mcpRequireToken":true,"lockRequireBiometric":true,"relockOnScreenshare":true,
            "cloudEgressConsented":false
        }"#;
        let cfg: AppConfig = serde_json::from_str(json).unwrap();
        assert!(cfg.semantic_search_enabled, "serde default must be true");

        // Settings-table path: an empty DB (no key written) loads the ON struct default.
        let db = temp_db();
        assert!(AppConfig::load(&db).unwrap().semantic_search_enabled);
    }

    #[test]
    fn brain_model_id_round_trips_and_defaults_none() {
        let db = temp_db();
        // Absent key ⇒ None (prod-safe default for existing installs).
        assert!(AppConfig::load(&db).unwrap().brain_model_id.is_none());
        let cfg = AppConfig {
            brain_model_id: Some("bielik-11b-v3".to_string()),
            ..Default::default()
        };
        cfg.save(&db).unwrap();
        assert_eq!(
            AppConfig::load(&db).unwrap().brain_model_id.as_deref(),
            Some("bielik-11b-v3")
        );
    }

    #[test]
    fn brain_backend_defaults_cloud_and_round_trips() {
        let db = temp_db();
        // Absent key ⇒ Cloud (the chosen default for fresh + pre-existing installs).
        assert_eq!(
            AppConfig::load(&db).unwrap().brain_backend,
            BrainBackend::Cloud
        );

        // AppleFoundation (WS2) joins the DB save/load round-trip loop — a persisted `apple` token
        // reloads as AppleFoundation, never the Cloud default.
        for backend in [
            BrainBackend::Cloud,
            BrainBackend::Local,
            BrainBackend::Off,
            BrainBackend::AppleFoundation,
        ] {
            let cfg = AppConfig {
                brain_backend: backend,
                ..Default::default()
            };
            cfg.save(&db).unwrap();
            assert_eq!(AppConfig::load(&db).unwrap().brain_backend, backend);
        }
    }

    #[test]
    fn brain_backend_serde_round_trips_and_defaults() {
        // Token form persisted to the settings table.
        assert_eq!(BrainBackend::Cloud.as_str(), "cloud");
        assert_eq!(BrainBackend::Local.as_str(), "local");
        assert_eq!(BrainBackend::Off.as_str(), "off");
        // WS2 — AppleFoundation persists as the single token `apple`.
        assert_eq!(BrainBackend::AppleFoundation.as_str(), "apple");
        assert_eq!(
            BrainBackend::from_str_or_default("local"),
            BrainBackend::Local
        );
        assert_eq!(BrainBackend::from_str_or_default("off"), BrainBackend::Off);
        assert_eq!(
            BrainBackend::from_str_or_default("apple"),
            BrainBackend::AppleFoundation
        );
        // Unknown / empty falls back to the default brain — INCLUDING the un-renamed
        // `applefoundation` spelling, which an OLD build must NOT accidentally accept.
        assert_eq!(
            BrainBackend::from_str_or_default("bogus"),
            BrainBackend::Cloud
        );
        assert_eq!(BrainBackend::from_str_or_default(""), BrainBackend::Cloud);
        assert_eq!(
            BrainBackend::from_str_or_default("applefoundation"),
            BrainBackend::Cloud
        );

        // serde: AppleFoundation serializes to the QUOTED token `"apple"` and round-trips back.
        assert_eq!(
            serde_json::to_string(&BrainBackend::AppleFoundation).unwrap(),
            "\"apple\""
        );
        assert_eq!(
            serde_json::from_str::<BrainBackend>("\"apple\"").unwrap(),
            BrainBackend::AppleFoundation
        );

        // JSON DTO: a payload omitting brainBackend loads as Cloud (#[serde(default)]).
        let json = r#"{
            "providerId":"claude_code","vaultPath":null,"vaultSubfolder":null,
            "whisperModelPath":null,"language":null,"anthropicModel":"claude-opus-4-8",
            "ollamaBaseUrl":"http://localhost:11434","ollamaModel":"llama3.1","claudeBinary":"claude",
            "inputDevice":null,"captureSystemAudio":false,"vadEnabled":true,"keepHiresMasters":false,
            "diarizeOthers":false,"aecEnabled":false,"postAecEnabled":true,"modelSize":"large-v3","voiceTrigger":false,
            "onboarded":false,"noteStyle":"standard","autoOrganize":false,"noteLanguage":"auto",
            "mcpRequireToken":true,"lockRequireBiometric":true,"relockOnScreenshare":true,
            "cloudEgressConsented":false
        }"#;
        let cfg: AppConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.brain_backend, BrainBackend::Cloud);
        // And an explicit value round-trips through serde with the lowercase rename.
        let dto: AppConfig = serde_json::from_str(
            &serde_json::to_string(&AppConfig {
                brain_backend: BrainBackend::Off,
                ..Default::default()
            })
            .unwrap(),
        )
        .unwrap();
        assert_eq!(dto.brain_backend, BrainBackend::Off);
    }

    #[test]
    fn realtime_reactions_defaults_off_and_round_trips() {
        let db = temp_db();
        // Absent key ⇒ OFF (opt-in; the always-on in-meeting assistant must never default on).
        assert!(!AppConfig::load(&db).unwrap().realtime_reactions);
        let cfg = AppConfig {
            realtime_reactions: true,
            ..Default::default()
        };
        cfg.save(&db).unwrap();
        assert!(AppConfig::load(&db).unwrap().realtime_reactions);
    }

    #[test]
    fn web_search_flags_default_off_and_round_trip() {
        let db = temp_db();
        // Fail-closed defaults: both OFF until explicitly set/granted.
        let cfg = AppConfig::load(&db).unwrap();
        assert!(
            !cfg.web_search_enabled,
            "web search master toggle defaults OFF"
        );
        assert!(
            !cfg.web_search_consented,
            "web search egress consent fail-closed OFF"
        );

        // `web_search_enabled` is a settable flag; consent is granted via its dedicated method.
        let cfg = AppConfig {
            web_search_enabled: true,
            ..Default::default()
        };
        cfg.save(&db).unwrap();
        let loaded = AppConfig::load(&db).unwrap();
        assert!(loaded.web_search_enabled);
        // Consent was NOT granted by a plain save.
        assert!(!loaded.web_search_consented);
    }

    #[test]
    fn web_search_consent_grant_persists() {
        let db = temp_db();
        let mut cfg = AppConfig::load(&db).unwrap();
        assert!(!cfg.web_search_consented);
        cfg.grant_web_search_consent(&db).unwrap();
        assert!(cfg.web_search_consented);
        // Survives a reload from the settings table.
        assert!(AppConfig::load(&db).unwrap().web_search_consented);
    }

    #[test]
    fn semantic_search_flag_round_trips() {
        let db = temp_db();
        let cfg = AppConfig {
            semantic_search_enabled: true,
            ..Default::default()
        };
        cfg.save(&db).unwrap();
        assert!(AppConfig::load(&db).unwrap().semantic_search_enabled);
    }

    /// NOTES WP4 — the three note-assistant toggles default ON on a fresh DB (missing keys keep the
    /// Default's `true`), and an explicit opt-OUT round-trips through save/load (a user who turns
    /// Refine off stays off despite default-true — the same footgun guard as semantic search).
    #[test]
    fn note_assist_toggles_default_on_and_opt_out_round_trips() {
        let db = temp_db();
        // Fresh DB (no stored keys) ⇒ all three default ON.
        let fresh = AppConfig::load(&db).unwrap();
        assert!(fresh.note_assist_refine, "refine defaults ON");
        assert!(fresh.note_assist_shorten, "shorten defaults ON");
        assert!(fresh.note_assist_enhance, "enhance defaults ON");

        // An explicit opt-out of Refine persists; the other two stay ON.
        AppConfig {
            note_assist_refine: false,
            ..Default::default()
        }
        .save(&db)
        .unwrap();
        let loaded = AppConfig::load(&db).unwrap();
        assert!(!loaded.note_assist_refine, "a Refine opt-out persists");
        assert!(loaded.note_assist_shorten, "shorten still ON");
        assert!(loaded.note_assist_enhance, "enhance still ON");
    }

    /// NOTES full-set — the opt-OUT list (`note_assist_actions_off`) defaults empty (all new actions
    /// enabled) and round-trips as a JSON string array across save/load, so a user who turns an
    /// action off stays off.
    #[test]
    fn note_assist_actions_off_defaults_empty_and_round_trips() {
        let db = temp_db();
        // Fresh DB ⇒ no actions opted out (missing key keeps the Default's empty Vec).
        assert!(
            AppConfig::load(&db)
                .unwrap()
                .note_assist_actions_off
                .is_empty(),
            "the opt-out list defaults empty (all actions enabled)"
        );

        AppConfig {
            note_assist_actions_off: vec!["bullets".into(), "table".into()],
            ..Default::default()
        }
        .save(&db)
        .unwrap();
        let loaded = AppConfig::load(&db).unwrap();
        assert_eq!(
            loaded.note_assist_actions_off,
            vec!["bullets".to_string(), "table".to_string()],
            "an explicit opt-out list round-trips through the JSON-encoded setting"
        );
    }

    /// TIER 1 default-on: an EXPLICIT opt-out (a stored `false`) must PERSIST across reload — a user
    /// who turns semantic search off stays off despite the new default-true. The critical new
    /// regression that keeps default-on from being a footgun.
    #[test]
    fn semantic_flag_off_round_trips() {
        let db = temp_db();
        let cfg = AppConfig {
            semantic_search_enabled: false,
            ..Default::default()
        };
        cfg.save(&db).unwrap();
        assert!(!AppConfig::load(&db).unwrap().semantic_search_enabled);
    }

    /// Vault Audit Phase 3: the weekly-audit flag defaults ON (a fresh DB and a config persisted
    /// before the field existed both load `true`), an explicit opt-out round-trips, and the
    /// dedicated mutator (the only supported one) persists durably.
    #[test]
    fn vault_audit_weekly_defaults_on_and_mutator_round_trips() {
        let db = temp_db();
        assert!(AppConfig::default().vault_audit_weekly_enabled);
        assert!(
            AppConfig::load(&db).unwrap().vault_audit_weekly_enabled,
            "an empty DB (no stored key) must default the weekly audit ON"
        );
        // A config JSON that OMITS the field (persisted before it existed) loads as true —
        // the same pre-field payload shape `missing_semantic_flag_defaults_on` exercises.
        let json = r#"{
            "providerId":"claude_code","vaultPath":null,"vaultSubfolder":null,
            "whisperModelPath":null,"language":null,"anthropicModel":"claude-opus-4-8",
            "ollamaBaseUrl":"http://localhost:11434","ollamaModel":"llama3.1","claudeBinary":"claude",
            "inputDevice":null,"captureSystemAudio":false,"vadEnabled":true,"keepHiresMasters":false,
            "diarizeOthers":false,"aecEnabled":false,"postAecEnabled":true,"modelSize":"large-v3","voiceTrigger":false,
            "onboarded":false,"noteStyle":"standard","autoOrganize":false,"noteLanguage":"auto",
            "mcpRequireToken":true,"lockRequireBiometric":true,"relockOnScreenshare":true,
            "cloudEgressConsented":false
        }"#;
        let omitted: AppConfig = serde_json::from_str(json).unwrap();
        assert!(
            omitted.vault_audit_weekly_enabled,
            "serde default must be true"
        );

        let mut cfg = AppConfig::load(&db).unwrap();
        cfg.set_vault_audit_weekly(&db, false).unwrap();
        assert!(
            !cfg.vault_audit_weekly_enabled,
            "mutator flips the in-memory flag"
        );
        assert!(
            !AppConfig::load(&db).unwrap().vault_audit_weekly_enabled,
            "the opt-out persists across reload"
        );
        cfg.set_vault_audit_weekly(&db, true).unwrap();
        assert!(AppConfig::load(&db).unwrap().vault_audit_weekly_enabled);
    }

    /// The two analysis aids default ON on a fresh settings DB, while explicit opt-outs remain
    /// durable. `voiceprint_enabled` is deliberately outside this pair and stays OFF by default.
    #[test]
    fn diarization_and_grounding_default_on_and_round_trip_opt_outs() {
        let db = temp_db();
        let fresh = AppConfig::load(&db).unwrap();
        assert!(fresh.diarize_others, "diarization defaults ON");
        assert!(fresh.ground_summary, "grounding defaults ON");
        assert!(
            !fresh.voiceprint_enabled,
            "the privacy-sensitive voiceprint feature stays OFF"
        );

        AppConfig {
            diarize_others: false,
            ground_summary: false,
            ..Default::default()
        }
        .save(&db)
        .unwrap();
        let opted_out = AppConfig::load(&db).unwrap();
        assert!(
            !opted_out.diarize_others,
            "an explicit diarization opt-out persists"
        );
        assert!(
            !opted_out.ground_summary,
            "an explicit grounding opt-out persists"
        );
        assert!(!opted_out.voiceprint_enabled);
    }

    /// ACTION-ITEM RECALL NET: defaults OFF / opt-in on a fresh DB (the cue scan is low-precision
    /// until calibrated), both states round-trip, and — the preserve-only half — a normal
    /// `AppConfig::save` can neither grant nor clear it.
    #[test]
    fn recall_net_defaults_off_round_trips_and_survives_a_settings_save() {
        let db = temp_db();
        assert!(
            !action_item_recall_net_enabled(&db),
            "the recall net must default OFF on a fresh DB (opt-in until calibrated)"
        );

        set_action_item_recall_net(&db, true).unwrap();
        assert!(
            action_item_recall_net_enabled(&db),
            "an explicit opt-in must persist"
        );

        // A generic settings save writes every KNOWN key; this one is not among them, so the user's
        // opt-in survives (and, symmetrically, a save can never turn it ON).
        AppConfig::default().save(&db).unwrap();
        assert!(
            action_item_recall_net_enabled(&db),
            "a settings save must not clear the opt-in"
        );

        set_action_item_recall_net(&db, false).unwrap();
        assert!(!action_item_recall_net_enabled(&db), "an opt-out persists");

        // FAIL-CLOSED on a junk value (only the exact "true" enables it).
        db.set_setting(K_ACTION_ITEM_RECALL_NET, "yes").unwrap();
        assert!(
            !action_item_recall_net_enabled(&db),
            "any value other than \"true\" reads as OFF"
        );
    }

    /// Historical JSON that predates the two fields gets the same values as `AppConfig::default`.
    /// This catches the bool-serde trap where plain `#[serde(default)]` would silently yield false.
    #[test]
    fn missing_diarization_and_grounding_match_struct_defaults() {
        let expected = AppConfig::default();
        let mut json = serde_json::to_value(&expected).unwrap();
        let object = json.as_object_mut().unwrap();
        object.remove("diarizeOthers");
        object.remove("groundSummary");

        let cfg: AppConfig = serde_json::from_value(json).unwrap();
        assert_eq!(cfg.diarize_others, expected.diarize_others);
        assert_eq!(cfg.ground_summary, expected.ground_summary);
        assert!(
            !cfg.voiceprint_enabled,
            "omitting analysis flags must never enable voiceprints"
        );
    }

    #[test]
    fn embed_model_id_defaults_none_and_round_trips() {
        // Serialize with other tests that touch the process-global embedder selection.
        let _g = crate::embed::EMBED_SELECTION_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let db = temp_db();
        // Absent key ⇒ None (⇒ the default embedder ⇒ byte-identical to historical behavior).
        assert!(AppConfig::load(&db).unwrap().embed_model_id.is_none());

        let cfg = AppConfig {
            embed_model_id: Some("mmlw-retrieval-e5-small".to_string()),
            ..Default::default()
        };
        cfg.save(&db).unwrap();
        assert_eq!(
            AppConfig::load(&db).unwrap().embed_model_id.as_deref(),
            Some("mmlw-retrieval-e5-small")
        );

        // Clearing back to None (default model) also round-trips — not a one-way latch.
        let cleared = AppConfig {
            embed_model_id: None,
            ..Default::default()
        };
        cleared.save(&db).unwrap();
        assert!(AppConfig::load(&db).unwrap().embed_model_id.is_none());
    }

    #[test]
    fn cloud_egress_consent_grant_persists() {
        let db = temp_db();
        let mut cfg = AppConfig::load(&db).unwrap();
        assert!(!cfg.cloud_egress_consented);
        cfg.grant_cloud_egress_consent(&db).unwrap();
        assert!(cfg.cloud_egress_consented);
        // Survives a reload from the settings table.
        let reloaded = AppConfig::load(&db).unwrap();
        assert!(reloaded.cloud_egress_consented);
    }

    /// E10 revoke — `revoke_cloud_egress` mirrors the grant: it flips the in-memory flag AND
    /// persists the key, so the revocation is fail-closed across a reload/restart too.
    #[test]
    fn cloud_egress_consent_revoke_persists() {
        let db = temp_db();
        let mut cfg = AppConfig::load(&db).unwrap();
        cfg.grant_cloud_egress_consent(&db).unwrap();
        assert!(AppConfig::load(&db).unwrap().cloud_egress_consented);
        cfg.revoke_cloud_egress(&db).unwrap();
        assert!(
            !cfg.cloud_egress_consented,
            "in-memory flag must flip immediately"
        );
        // Survives a reload from the settings table.
        assert!(!AppConfig::load(&db).unwrap().cloud_egress_consented);
    }

    /// FAIL-CLOSED ORDERING (grants persist-then-flip). On the normal path the durable record and
    /// the in-memory flag must AGREE after a grant — the durable write landed and only then did the
    /// session flip. (The failure branch — a `set_setting` error leaving the flag false — is a
    /// STRUCTURAL guarantee: the grant returns `?` before touching `self`, so a write error can never
    /// flip the flag. We can't force a `set_setting` error without a Db mock the crate doesn't have,
    /// so we pin the observable agreement of the two sides here.)
    #[test]
    fn cloud_egress_grant_durable_record_and_flag_agree() {
        let db = temp_db();
        let mut cfg = AppConfig::load(&db).unwrap();
        // Pre-grant: neither side consents.
        assert!(!cfg.cloud_egress_consented);
        assert_ne!(
            db.get_setting(K_CLOUD_EGRESS_CONSENTED).unwrap().as_deref(),
            Some("true")
        );
        // Grant: the durable record is written FIRST, then the flag flips — both true afterwards.
        cfg.grant_cloud_egress_consent(&db).unwrap();
        assert!(cfg.cloud_egress_consented, "session flag flipped");
        assert_eq!(
            db.get_setting(K_CLOUD_EGRESS_CONSENTED).unwrap().as_deref(),
            Some("true"),
            "durable consent record persisted (persist-first)"
        );
    }

    /// Same fail-closed agreement for the web-search grant.
    #[test]
    fn web_search_grant_durable_record_and_flag_agree() {
        let db = temp_db();
        let mut cfg = AppConfig::load(&db).unwrap();
        assert!(!cfg.web_search_consented);
        cfg.grant_web_search_consent(&db).unwrap();
        assert!(cfg.web_search_consented);
        assert_eq!(
            db.get_setting(K_WEB_SEARCH_CONSENTED).unwrap().as_deref(),
            Some("true"),
            "durable web-search consent record persisted (persist-first)"
        );
    }

    #[test]
    fn jira_flags_default_off_and_round_trip() {
        let db = temp_db();
        // Fail-closed defaults: both flags OFF + strings empty until explicitly set/granted.
        let cfg = AppConfig::load(&db).unwrap();
        assert!(!cfg.jira_enabled, "jira must default OFF");
        assert!(!cfg.jira_consented, "jira consent must default ungranted");
        assert!(cfg.jira_base_url.is_empty());
        assert!(cfg.jira_email.is_empty());

        let cfg = AppConfig {
            jira_enabled: true,
            jira_base_url: "https://acme.atlassian.net".into(),
            jira_email: "me@acme.com".into(),
            ..cfg
        };
        cfg.save(&db).unwrap();
        let loaded = AppConfig::load(&db).unwrap();
        assert!(loaded.jira_enabled);
        assert_eq!(loaded.jira_base_url, "https://acme.atlassian.net");
        assert_eq!(loaded.jira_email, "me@acme.com");
        // PRESERVE-ONLY: a save can never grant consent.
        assert!(!loaded.jira_consented);
    }

    #[test]
    fn jira_consent_grant_persists_and_save_cannot_clobber() {
        let db = temp_db();
        let mut cfg = AppConfig::load(&db).unwrap();
        cfg.grant_jira_consent(&db).unwrap();
        assert!(cfg.jira_consented);
        assert!(AppConfig::load(&db).unwrap().jira_consented);
        // Durable record persisted (persist-first).
        assert_eq!(
            db.get_setting(K_JIRA_CONSENTED).unwrap().as_deref(),
            Some("true"),
            "durable jira consent record persisted (persist-first)"
        );
        // A later plain save must PRESERVE the granted consent — a plain `save` writes the flag
        // verbatim, so this proves the durable record is not clobbered by a save that carries false.
        let cfg2 = AppConfig {
            jira_consented: false,
            ..AppConfig::load(&db).unwrap()
        };
        cfg2.save(&db).unwrap();
        assert!(
            AppConfig::load(&db).unwrap().jira_consented,
            "grant_jira_consent's durable record survives; consent flips true only via the grant path"
        );
    }

    #[test]
    fn slack_flags_default_off_and_round_trip() {
        let db = temp_db();
        // Fail-closed defaults: both flags OFF until explicitly set/granted.
        let cfg = AppConfig::load(&db).unwrap();
        assert!(!cfg.slack_enabled, "slack must default OFF");
        assert!(!cfg.slack_consented, "slack consent must default ungranted");

        let cfg = AppConfig {
            slack_enabled: true,
            ..cfg
        };
        cfg.save(&db).unwrap();
        let loaded = AppConfig::load(&db).unwrap();
        assert!(loaded.slack_enabled);
        // PRESERVE-ONLY: a save can never grant consent.
        assert!(!loaded.slack_consented);
    }

    #[test]
    fn slack_consent_grant_persists_and_save_cannot_clobber() {
        let db = temp_db();
        let mut cfg = AppConfig::load(&db).unwrap();
        cfg.grant_slack_consent(&db).unwrap();
        assert!(cfg.slack_consented);
        assert!(AppConfig::load(&db).unwrap().slack_consented);
        // Durable record persisted (persist-first).
        assert_eq!(
            db.get_setting(K_SLACK_CONSENTED).unwrap().as_deref(),
            Some("true"),
            "durable slack consent record persisted (persist-first)"
        );
        // A later plain save must PRESERVE the granted consent — a plain `save` never writes the flag,
        // so this proves the durable record is not clobbered by a save that carries false.
        let cfg2 = AppConfig {
            slack_consented: false,
            ..AppConfig::load(&db).unwrap()
        };
        cfg2.save(&db).unwrap();
        assert!(
            AppConfig::load(&db).unwrap().slack_consented,
            "grant_slack_consent's durable record survives; consent flips true only via the grant path"
        );
    }

    #[test]
    fn notion_flags_default_off_and_round_trip() {
        let db = temp_db();
        // Fail-closed defaults: both flags OFF until explicitly set/granted.
        let cfg = AppConfig::load(&db).unwrap();
        assert!(!cfg.notion_enabled, "notion must default OFF");
        assert!(
            !cfg.notion_consented,
            "notion consent must default ungranted"
        );

        let cfg = AppConfig {
            notion_enabled: true,
            ..cfg
        };
        cfg.save(&db).unwrap();
        let loaded = AppConfig::load(&db).unwrap();
        assert!(loaded.notion_enabled);
        // PRESERVE-ONLY: a save can never grant consent.
        assert!(!loaded.notion_consented);
    }

    #[test]
    fn notion_consent_grant_persists_and_save_cannot_clobber() {
        let db = temp_db();
        let mut cfg = AppConfig::load(&db).unwrap();
        cfg.grant_notion_consent(&db).unwrap();
        assert!(cfg.notion_consented);
        assert!(AppConfig::load(&db).unwrap().notion_consented);
        assert_eq!(
            db.get_setting(K_NOTION_CONSENTED).unwrap().as_deref(),
            Some("true"),
            "durable notion consent record persisted (persist-first)"
        );
        // A later plain save must PRESERVE the granted consent — a plain `save` never writes the flag.
        let cfg2 = AppConfig {
            notion_consented: false,
            ..AppConfig::load(&db).unwrap()
        };
        cfg2.save(&db).unwrap();
        assert!(
            AppConfig::load(&db).unwrap().notion_consented,
            "grant_notion_consent's durable record survives; consent flips true only via the grant path"
        );
    }

    #[test]
    fn clickup_flags_default_off_and_round_trip() {
        let db = temp_db();
        let cfg = AppConfig::load(&db).unwrap();
        assert!(!cfg.clickup_enabled, "clickup must default OFF");
        assert!(
            !cfg.clickup_consented,
            "clickup consent must default ungranted"
        );
        assert!(cfg.clickup_team_id.is_empty());

        let cfg = AppConfig {
            clickup_enabled: true,
            clickup_team_id: "9001".into(),
            ..cfg
        };
        cfg.save(&db).unwrap();
        let loaded = AppConfig::load(&db).unwrap();
        assert!(loaded.clickup_enabled);
        assert_eq!(loaded.clickup_team_id, "9001");
        // PRESERVE-ONLY: a save can never grant consent.
        assert!(!loaded.clickup_consented);
    }

    #[test]
    fn clickup_consent_grant_persists_and_save_cannot_clobber() {
        let db = temp_db();
        let mut cfg = AppConfig::load(&db).unwrap();
        cfg.grant_clickup_consent(&db).unwrap();
        assert!(cfg.clickup_consented);
        assert!(AppConfig::load(&db).unwrap().clickup_consented);
        assert_eq!(
            db.get_setting(K_CLICKUP_CONSENTED).unwrap().as_deref(),
            Some("true"),
            "durable clickup consent record persisted (persist-first)"
        );
        let cfg2 = AppConfig {
            clickup_consented: false,
            ..AppConfig::load(&db).unwrap()
        };
        cfg2.save(&db).unwrap();
        assert!(
            AppConfig::load(&db).unwrap().clickup_consented,
            "grant_clickup_consent's durable record survives; consent flips true only via the grant path"
        );
    }

    #[test]
    fn security_flags_round_trip() {
        let db = temp_db();
        let cfg = AppConfig {
            lock_require_biometric: false,
            relock_on_screenshare: false,
            ..Default::default()
        };
        cfg.save(&db).unwrap();
        let loaded = AppConfig::load(&db).unwrap();
        assert!(!loaded.lock_require_biometric);
        assert!(!loaded.relock_on_screenshare);
    }

    /// Voiceprint capture is a PRIVACY-sensitive opt-in: it defaults OFF on a fresh install (the
    /// setting key is absent), and an explicit ON round-trips through save/load. Never let this
    /// default on (see the field doc — untested under BIPA/CIPA).
    #[test]
    fn voiceprint_enabled_defaults_off_and_round_trips() {
        let db = temp_db();
        // Absent key ⇒ OFF for fresh + pre-existing installs.
        assert!(
            !AppConfig::load(&db).unwrap().voiceprint_enabled,
            "voiceprint capture must default OFF (privacy opt-in)"
        );

        let cfg = AppConfig {
            voiceprint_enabled: true,
            ..Default::default()
        };
        cfg.save(&db).unwrap();
        assert!(
            AppConfig::load(&db).unwrap().voiceprint_enabled,
            "an explicit voiceprint opt-in must round-trip through save/load"
        );

        // And turning it back OFF round-trips (not a one-way latch).
        let off = AppConfig {
            voiceprint_enabled: false,
            ..Default::default()
        };
        off.save(&db).unwrap();
        assert!(!AppConfig::load(&db).unwrap().voiceprint_enabled);
    }

    #[test]
    fn save_then_load_round_trips() {
        let db = temp_db();
        let cfg = AppConfig {
            provider_id: "ollama".to_string(),
            vault_path: Some("/vault".to_string()),
            language: Some("en".to_string()),
            glossary: "Konnect = Connect, Kinect\nFastMCP".to_string(),
            ..Default::default()
        };
        cfg.save(&db).unwrap();

        let loaded = AppConfig::load(&db).unwrap();
        assert_eq!(loaded.provider_id, "ollama");
        assert_eq!(loaded.vault_path.as_deref(), Some("/vault"));
        assert_eq!(loaded.language.as_deref(), Some("en"));
        assert_eq!(
            loaded.glossary, "Konnect = Connect, Kinect\nFastMCP",
            "the user-authored glossary must round-trip byte-for-byte"
        );
    }

    #[test]
    fn glossary_defaults_empty_and_explicit_clear_round_trips() {
        let db = temp_db();
        assert_eq!(AppConfig::load(&db).unwrap().glossary, "");

        AppConfig {
            glossary: "Kong Operator = Kong, KO".to_string(),
            ..Default::default()
        }
        .save(&db)
        .unwrap();
        assert_eq!(
            AppConfig::load(&db).unwrap().glossary,
            "Kong Operator = Kong, KO"
        );

        AppConfig {
            glossary: String::new(),
            ..Default::default()
        }
        .save(&db)
        .unwrap();
        assert_eq!(
            AppConfig::load(&db).unwrap().glossary,
            "",
            "an explicit empty value clears the stored glossary"
        );
    }

    #[test]
    fn provider_model_and_effort_round_trip_and_default_empty() {
        let db = temp_db();
        // Absent keys ⇒ `""` (provider default) for fresh + pre-existing installs.
        let fresh = AppConfig::load(&db).unwrap();
        assert_eq!(fresh.provider_model, "");
        assert_eq!(fresh.provider_effort, "");

        let cfg = AppConfig {
            provider_model: "claude-sonnet-4-6".to_string(),
            provider_effort: "high".to_string(),
            ..Default::default()
        };
        cfg.save(&db).unwrap();
        let loaded = AppConfig::load(&db).unwrap();
        assert_eq!(loaded.provider_model, "claude-sonnet-4-6");
        assert_eq!(loaded.provider_effort, "high");

        // Clearing back to `""` (provider default) also round-trips — not a one-way latch.
        let cleared = AppConfig {
            provider_model: String::new(),
            provider_effort: String::new(),
            ..Default::default()
        };
        cleared.save(&db).unwrap();
        let reloaded = AppConfig::load(&db).unwrap();
        assert_eq!(reloaded.provider_model, "");
        assert_eq!(reloaded.provider_effort, "");
    }

    #[test]
    fn empty_option_persists_as_none() {
        let db = temp_db();
        let cfg = AppConfig::default(); // vault_path = None
        cfg.save(&db).unwrap();
        let loaded = AppConfig::load(&db).unwrap();
        assert!(loaded.vault_path.is_none());
        let _ = PathBuf::new();
    }

    /// Task 1.1 — gateway fields default to `""` and are never set by pre-existing configs.
    #[test]
    fn gateway_fields_default_empty() {
        // In-memory struct default.
        let cfg = AppConfig::default();
        assert_eq!(
            cfg.gateway_base_url, "",
            "gateway_base_url must default to empty"
        );
        assert_eq!(cfg.gateway_model, "", "gateway_model must default to empty");

        // Settings-table path: an empty DB (no key written) loads the defaults.
        let db = temp_db();
        let loaded = AppConfig::load(&db).unwrap();
        assert_eq!(loaded.gateway_base_url, "");
        assert_eq!(loaded.gateway_model, "");
    }

    /// Task 1.1 — gateway fields round-trip through save/load unchanged.
    #[test]
    fn gateway_fields_round_trip() {
        let db = temp_db();
        let cfg = AppConfig {
            gateway_base_url: "https://my-gateway.example.com/v1".to_string(),
            gateway_model: "gpt-4o".to_string(),
            ..Default::default()
        };
        cfg.save(&db).unwrap();
        let loaded = AppConfig::load(&db).unwrap();
        assert_eq!(loaded.gateway_base_url, "https://my-gateway.example.com/v1");
        assert_eq!(loaded.gateway_model, "gpt-4o");

        // Clearing back to `""` (unset) also round-trips — not a one-way latch.
        let cleared = AppConfig {
            ..Default::default()
        };
        cleared.save(&db).unwrap();
        let reloaded = AppConfig::load(&db).unwrap();
        assert_eq!(reloaded.gateway_base_url, "");
        assert_eq!(reloaded.gateway_model, "");
    }

    /// Proactive brain P1 — the mute flag defaults ON (spec D2: default-on with conservative
    /// thresholds), for fresh installs AND for configs persisted before the field existed (both
    /// the settings-table path and the serde path), and an explicit OFF round-trips (the backend
    /// mute is a real, persistable choice — not a one-way latch).
    #[test]
    fn proactive_hints_defaults_on_and_round_trips() {
        // In-memory default + empty settings table (key never written) ⇒ ON.
        assert!(AppConfig::default().proactive_hints_enabled);
        let db = temp_db();
        assert!(AppConfig::load(&db).unwrap().proactive_hints_enabled);

        // serde path: a JSON payload omitting the field entirely loads ON (default_true).
        let json = r#"{
            "providerId":"claude_code","vaultPath":null,"vaultSubfolder":null,
            "whisperModelPath":null,"language":null,"anthropicModel":"claude-opus-4-8",
            "ollamaBaseUrl":"http://localhost:11434","ollamaModel":"llama3.1","claudeBinary":"claude",
            "inputDevice":null,"captureSystemAudio":false,"vadEnabled":true,"keepHiresMasters":false,
            "diarizeOthers":false,"aecEnabled":false,"postAecEnabled":true,"modelSize":"large-v3","voiceTrigger":false,
            "onboarded":false,"noteStyle":"standard","autoOrganize":false,"noteLanguage":"auto",
            "mcpRequireToken":true,"lockRequireBiometric":true,"relockOnScreenshare":true,
            "cloudEgressConsented":false
        }"#;
        let cfg: AppConfig = serde_json::from_str(json).unwrap();
        assert!(
            cfg.proactive_hints_enabled,
            "an omitted key must default ON"
        );

        // Explicit OFF persists + reloads OFF (the backend-side mute).
        let cfg = AppConfig {
            proactive_hints_enabled: false,
            ..Default::default()
        };
        cfg.save(&db).unwrap();
        assert!(!AppConfig::load(&db).unwrap().proactive_hints_enabled);
    }

    /// Sharing-onboarding gate: `sharing_choice_made` defaults OFF (a fresh/pre-existing install
    /// sees the gateway once), and the dedicated `set_sharing_choice_made` mutator persists it as a
    /// one-way latch that survives reload. Mirrors the consent-mutator persistence test.
    #[test]
    fn sharing_choice_made_defaults_off_and_latches_via_mutator() {
        // In-memory default + empty settings table (key never written) ⇒ OFF (gateway shows).
        assert!(!AppConfig::default().sharing_choice_made);
        let db = temp_db();
        assert!(!AppConfig::load(&db).unwrap().sharing_choice_made);

        // The dedicated mutator persists it, and the value survives a reload (one-way latch).
        let mut cfg = AppConfig::load(&db).unwrap();
        cfg.set_sharing_choice_made(&db).unwrap();
        assert!(cfg.sharing_choice_made, "mutator flips the in-memory flag");
        assert!(
            AppConfig::load(&db).unwrap().sharing_choice_made,
            "the latch persists across reload"
        );
    }

    /// Cross-meeting USER MEMORY — the master gate defaults ON for fresh installs AND for configs
    /// persisted before the field existed (both the settings-table path and the serde path), and an
    /// explicit OFF round-trips (turning memory off is a real, persistable choice — not a one-way
    /// latch). ON-on-absent is the "memory stays on for existing installs" guarantee.
    #[test]
    fn user_memory_enabled_defaults_on_and_round_trips() {
        // In-memory default + empty settings table (key never written) ⇒ ON.
        assert!(AppConfig::default().user_memory_enabled);
        let db = temp_db();
        assert!(AppConfig::load(&db).unwrap().user_memory_enabled);

        // serde path: a JSON payload omitting the field entirely loads ON (default_true).
        let json = r#"{
            "providerId":"claude_code","vaultPath":null,"vaultSubfolder":null,
            "whisperModelPath":null,"language":null,"anthropicModel":"claude-opus-4-8",
            "ollamaBaseUrl":"http://localhost:11434","ollamaModel":"llama3.1","claudeBinary":"claude",
            "inputDevice":null,"captureSystemAudio":false,"vadEnabled":true,"keepHiresMasters":false,
            "diarizeOthers":false,"aecEnabled":false,"postAecEnabled":true,"modelSize":"large-v3","voiceTrigger":false,
            "onboarded":false,"noteStyle":"standard","autoOrganize":false,"noteLanguage":"auto",
            "mcpRequireToken":true,"lockRequireBiometric":true,"relockOnScreenshare":true,
            "cloudEgressConsented":false
        }"#;
        let cfg: AppConfig = serde_json::from_str(json).unwrap();
        assert!(cfg.user_memory_enabled, "an omitted key must default ON");

        // Explicit OFF persists + reloads OFF (the backend-side memory kill switch).
        let cfg = AppConfig {
            user_memory_enabled: false,
            ..Default::default()
        };
        cfg.save(&db).unwrap();
        assert!(!AppConfig::load(&db).unwrap().user_memory_enabled);
    }

    /// Model roles — the 9 role keys default `""` (inherit-legacy) for fresh installs AND for
    /// configs persisted before the keys existed (both the settings-table path and the serde
    /// path), and set values round-trip — including clearing back to `""` (not a one-way latch).
    /// `""`-on-absent is the ZERO-BEHAVIOR-CHANGE guarantee for existing installs.
    #[test]
    fn role_keys_default_empty_and_round_trip() {
        // In-memory struct default + empty settings table (keys never written) ⇒ "" everywhere.
        let d = AppConfig::default();
        let db = temp_db();
        let fresh = AppConfig::load(&db).unwrap();
        for cfg in [&d, &fresh] {
            assert_eq!(cfg.role_notes_connection, "");
            assert_eq!(cfg.role_notes_model, "");
            assert_eq!(cfg.role_notes_effort, "");
            assert_eq!(cfg.role_ask_connection, "");
            assert_eq!(cfg.role_ask_model, "");
            assert_eq!(cfg.role_ask_effort, "");
            assert_eq!(cfg.role_live_connection, "");
            assert_eq!(cfg.role_live_model, "");
            assert_eq!(cfg.role_live_effort, "");
        }

        // serde path: a pre-role JSON payload (fields omitted) loads "" (#[serde(default)]).
        let json = r#"{
            "providerId":"claude_code","vaultPath":null,"vaultSubfolder":null,
            "whisperModelPath":null,"language":null,"anthropicModel":"claude-opus-4-8",
            "ollamaBaseUrl":"http://localhost:11434","ollamaModel":"llama3.1","claudeBinary":"claude",
            "inputDevice":null,"captureSystemAudio":false,"vadEnabled":true,"keepHiresMasters":false,
            "diarizeOthers":false,"aecEnabled":false,"postAecEnabled":true,"modelSize":"large-v3","voiceTrigger":false,
            "onboarded":false,"noteStyle":"standard","autoOrganize":false,"noteLanguage":"auto",
            "mcpRequireToken":true,"lockRequireBiometric":true,"relockOnScreenshare":true,
            "cloudEgressConsented":false
        }"#;
        let cfg: AppConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.role_notes_connection, "");
        assert_eq!(cfg.role_ask_connection, "");
        assert_eq!(cfg.role_live_connection, "");

        // Set values persist + reload verbatim.
        let cfg = AppConfig {
            role_notes_connection: "anthropic".to_string(),
            role_notes_model: "claude-opus-4-8".to_string(),
            role_notes_effort: "high".to_string(),
            role_ask_connection: "ollama".to_string(),
            role_ask_model: "mistral-small".to_string(),
            role_live_connection: "off".to_string(),
            ..Default::default()
        };
        cfg.save(&db).unwrap();
        let loaded = AppConfig::load(&db).unwrap();
        assert_eq!(loaded.role_notes_connection, "anthropic");
        assert_eq!(loaded.role_notes_model, "claude-opus-4-8");
        assert_eq!(loaded.role_notes_effort, "high");
        assert_eq!(loaded.role_ask_connection, "ollama");
        assert_eq!(loaded.role_ask_model, "mistral-small");
        assert_eq!(loaded.role_ask_effort, "");
        assert_eq!(loaded.role_live_connection, "off");

        // Clearing back to "" (inherit legacy) also round-trips.
        AppConfig::default().save(&db).unwrap();
        let cleared = AppConfig::load(&db).unwrap();
        assert_eq!(cleared.role_notes_connection, "");
        assert_eq!(cleared.role_ask_connection, "");
        assert_eq!(cleared.role_live_connection, "");
    }

    /// Task 1.1 — serde: a payload omitting the gateway fields loads with empty defaults.
    #[test]
    fn gateway_fields_serde_default() {
        let json = r#"{
            "providerId":"claude_code","vaultPath":null,"vaultSubfolder":null,
            "whisperModelPath":null,"language":null,"anthropicModel":"claude-opus-4-8",
            "ollamaBaseUrl":"http://localhost:11434","ollamaModel":"llama3.1","claudeBinary":"claude",
            "inputDevice":null,"captureSystemAudio":false,"vadEnabled":true,"keepHiresMasters":false,
            "diarizeOthers":false,"aecEnabled":false,"postAecEnabled":true,"modelSize":"large-v3","voiceTrigger":false,
            "onboarded":false,"noteStyle":"standard","autoOrganize":false,"noteLanguage":"auto",
            "mcpRequireToken":true,"lockRequireBiometric":true,"relockOnScreenshare":true,
            "cloudEgressConsented":false
        }"#;
        let cfg: AppConfig = serde_json::from_str(json).unwrap();
        assert_eq!(
            cfg.gateway_base_url, "",
            "serde default must be empty string"
        );
        assert_eq!(cfg.gateway_model, "", "serde default must be empty string");
    }

    #[test]
    fn audio_storage_settings_round_trip() {
        let p = crate::storage::db::unique_temp_path("murmur-cfg-storage", "sqlite");
        let _ = std::fs::remove_file(&p);
        let db = Db::open_with_key(
            &p,
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )
        .unwrap();

        // Defaults: no cap, auto-prune OFF (fail-safe).
        let def = AppConfig::default();
        assert_eq!(def.audio_storage_limit_gb, None);
        assert!(!def.audio_auto_prune);

        let cfg = AppConfig {
            audio_storage_limit_gb: Some(2),
            audio_auto_prune: true,
            ..AppConfig::default()
        };
        cfg.save(&db).unwrap();

        let loaded = AppConfig::load(&db).unwrap();
        assert_eq!(loaded.audio_storage_limit_gb, Some(2));
        assert!(loaded.audio_auto_prune);
        let _ = std::fs::remove_file(&p);
    }

    /// C9 — `model_size_source` round-trips, rejects an unrecognised token, and is REVERSIBLE
    /// (auto → user → auto), never write-once.
    #[test]
    fn model_size_source_round_trips_and_is_reversible() {
        let db = temp_db();
        assert_eq!(model_size_source(&db), None, "absent until recorded");

        set_model_size_source(&db, MODEL_SIZE_SOURCE_AUTO).unwrap();
        assert_eq!(model_size_source(&db).as_deref(), Some("auto"));
        set_model_size_source(&db, MODEL_SIZE_SOURCE_USER).unwrap();
        assert_eq!(model_size_source(&db).as_deref(), Some("user"));
        // …and back again — the field is not write-once.
        set_model_size_source(&db, MODEL_SIZE_SOURCE_AUTO).unwrap();
        assert_eq!(model_size_source(&db).as_deref(), Some("auto"));

        // An unrecognised token is REFUSED, and the stored value survives untouched.
        assert!(set_model_size_source(&db, "whatever").is_err());
        assert!(set_model_size_source(&db, "").is_err());
        assert_eq!(model_size_source(&db).as_deref(), Some("auto"));
    }

    /// C9 BACKFILL — an existing (onboarded, non-blank size) install is recorded as a deliberate
    /// `"user"` choice exactly once; every other shape is left alone, and the backfill never
    /// overwrites a value already there.
    #[test]
    fn model_size_source_backfill_only_marks_existing_installs() {
        // Onboarded + a real size ⇒ backfilled once, then idempotent.
        let db = temp_db();
        assert!(backfill_model_size_source(&db, true, "small").unwrap());
        assert_eq!(model_size_source(&db).as_deref(), Some("user"));
        assert!(
            !backfill_model_size_source(&db, true, "small").unwrap(),
            "a second run must not rewrite the row"
        );

        // A later `"auto"` (the user took the recommendation) must NOT be clobbered back to
        // `"user"` on the next launch.
        set_model_size_source(&db, MODEL_SIZE_SOURCE_AUTO).unwrap();
        assert!(!backfill_model_size_source(&db, true, "small").unwrap());
        assert_eq!(model_size_source(&db).as_deref(), Some("auto"));

        // NOT onboarded ⇒ nothing recorded (a fresh install's size comes from the recommendation).
        let fresh = temp_db();
        assert!(!backfill_model_size_source(&fresh, false, "small").unwrap());
        assert_eq!(model_size_source(&fresh), None);

        // A BLANK size is refused — but ONLY as a defensive guard for a hand-edited config. It is
        // NOT a discriminator between "chose deliberately" and "accepted the preselection":
        // `AppConfig::default()` seeds `model_size` from `default_model_size_now()` and
        // `AppConfig::load` only overwrites a non-empty stored value, so the real call site at
        // launch never passes a blank. The next assertion states what actually happens to the
        // existing user base.
        let blank = temp_db();
        assert!(!backfill_model_size_source(&blank, true, "").unwrap());
        assert!(!backfill_model_size_source(&blank, true, "   ").unwrap());
        assert_eq!(model_size_source(&blank), None);

        // THE HONEST STATEMENT OF THIS FUNCTION'S REACH: an onboarded install is marked "user"
        // regardless of whether the size was actively chosen or merely accepted from onboarding's
        // preselection, because provenance was never recorded and is unrecoverable. "user" is the
        // never-surprise direction (do not move the selection on their behalf) — it must not be
        // read as evidence the user made a deliberate choice.
        let preselected = temp_db();
        let untouched_default = AppConfig::default().model_size;
        assert!(
            backfill_model_size_source(&preselected, true, &untouched_default).unwrap(),
            "an untouched onboarding preselection is still stamped, because it is indistinguishable from a real choice"
        );
        assert_eq!(model_size_source(&preselected).as_deref(), Some("user"));
    }

    /// C3 — the machine-change nudge is a durable PULL. First launch records silently; an identical
    /// launch is a no-op; a DIFFERENT machine raises the pending row AND moves the fingerprint (in
    /// that order), and the nudge survives until dismissed.
    #[test]
    fn machine_fingerprint_raises_a_durable_nudge_only_on_a_real_change() {
        let db = temp_db();
        assert!(!machine_change_pending(&db));

        // First ever launch: record, do not nudge.
        assert!(!note_machine_fingerprint(&db, Some("ram=1;arch=arm64;chip=Apple M1")).unwrap());
        assert!(!machine_change_pending(&db));

        // Same machine again: no-op.
        assert!(!note_machine_fingerprint(&db, Some("ram=1;arch=arm64;chip=Apple M1")).unwrap());
        assert!(!machine_change_pending(&db));

        // A different Mac: nudge raised.
        assert!(note_machine_fingerprint(&db, Some("ram=2;arch=arm64;chip=Apple M4 Max")).unwrap());
        assert!(machine_change_pending(&db));

        // The nudge SURVIVES a relaunch on the (now current) machine — it is a pull, not an event
        // that evaporates because nobody was listening.
        assert!(
            !note_machine_fingerprint(&db, Some("ram=2;arch=arm64;chip=Apple M4 Max")).unwrap()
        );
        assert!(machine_change_pending(&db));

        clear_machine_change_pending(&db).unwrap();
        assert!(!machine_change_pending(&db));
    }

    /// An UNREADABLE fingerprint (RAM probe failed) is a complete no-op: it must not fire a nudge
    /// off a missing measurement, and it must not clobber the good stored value.
    #[test]
    fn machine_fingerprint_unreadable_probe_is_a_no_op() {
        let db = temp_db();
        note_machine_fingerprint(&db, Some("ram=1;arch=arm64;chip=Apple M1")).unwrap();

        assert!(!note_machine_fingerprint(&db, None).unwrap());
        assert!(!machine_change_pending(&db));

        // The stored value survived, so the NEXT good read still compares as unchanged.
        assert!(!note_machine_fingerprint(&db, Some("ram=1;arch=arm64;chip=Apple M1")).unwrap());
        assert!(!machine_change_pending(&db));
    }
}
