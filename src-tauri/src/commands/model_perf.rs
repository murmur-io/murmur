//! Model / performance / capability-probe commands — extracted verbatim from `commands` (God-file
//! split, a PURE MOVE — no behavior change). This is the on-device model-management + capability
//! surface: which whisper / brain / embed / NER models are present, RAM-fit + residency probes, the
//! resolved AI map, brain posture, and the NER model download. It is a NON-content-gated domain —
//! every command here is a config read, a file-on-disk existence probe, a RAM probe, or a model
//! download (inbound-only, no meeting content, no egress). None touch `meeting_is_unlocked` /
//! `visibility_clause` / any seal path (the sibling `generate_timeline` / `get_meeting_detail`, which
//! ARE gated, deliberately stayed in `commands/mod.rs`). Every symbol keeps its EXACT prior
//! body/signature and is re-exported at `crate::commands` via `pub use model_perf::*;` in
//! `commands/mod.rs`, so `generate_handler![commands::model_present]` in `lib.rs` and every
//! `crate::commands::…` caller resolve UNCHANGED. `select_brain_model_inner` /
//! `select_embed_model_inner` / `brain_download_target` are promoted to `pub(crate)` so the shared
//! `lifecycle_tests` harness (kept in `commands/mod.rs`) and the `download_brain_model` command (also
//! kept there) still reach them through the re-export.

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use crate::error::AppError;
use crate::state::AppState;

/// Would generating THIS install's timeline load a residency-bound on-device model? True when the
/// resolved Notes-role provider is on-device (local GGUF / Ollama / Apple FM) — the residency-bound
/// engines whose synchronous multi-GB load on a passive Audio-tab open OOM-beachballed the Mac. The
/// FE uses this to decide: auto-generate for CLOUD (cheap), or hide generation behind an explicit
/// "Generate timeline" click for on-device (never a surprise heavy load). Cheap: config read only.
#[tauri::command]
pub fn timeline_generation_on_device(state: State<'_, AppState>) -> Result<bool, AppError> {
    let config = {
        let c = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
        c.clone()
    };
    let target = crate::summarize::roles::resolve(crate::summarize::roles::Role::Notes, &config);
    Ok(crate::summarize::timeline::is_on_device_provider(
        &target.connection,
    ))
}

/// Whether a usable Whisper model is present for the chosen size + language (or the
/// explicit configured path). Lets the UI auto-detect + offer a download when missing.
#[tauri::command]
pub fn model_present(state: State<'_, AppState>) -> Result<bool, AppError> {
    let (configured, size, language) = {
        let c = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
        (
            c.whisper_model_path.clone(),
            c.model_size.clone(),
            c.language.clone().unwrap_or_default(),
        )
    };
    let p = configured.as_deref().map(std::path::Path::new);
    Ok(crate::transcribe::resolve_model_path(p, &size, &language)?.is_some())
}

/// Whether a usable on-device brain (reasoning GGUF) is present at the resolved path — the
/// configured custom `brain_model_path`, else the selected `brain_model_id`'s file in the shared
/// models dir. Lets the UI offer a download. Purely a file-on-disk check (mistralrs is always
/// compiled; the real brain activates on model presence).
#[tauri::command]
pub fn brain_model_present(state: State<'_, AppState>) -> Result<bool, AppError> {
    let (configured, selected) = {
        let c = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
        (c.brain_model_path.clone(), c.brain_model_id.clone())
    };
    let p = configured.as_deref().map(std::path::Path::new);
    Ok(crate::reason::resolve_brain_model(p, selected.as_deref())?.is_some())
}

/// macOS total physical RAM in whole GB. Since P1 this reads the CACHED
/// [`crate::machine::MachineProfile`] instead of re-spawning `sysctl -n hw.memsize` on every call
/// (a single `list_brain_models` used to pay a subprocess). Returns `None` on any error — the
/// caller then treats every model as fitting rather than HIDING it behind a failed probe.
fn total_ram_gb() -> Option<u64> {
    crate::machine::total_ram_bytes().map(|bytes| bytes / (1024 * 1024 * 1024))
}

/// The curated on-device brain model registry, each row carrying the picker flags `downloaded`
/// (file present in the shared models dir), `fits_ram` (min RAM within the machine's total RAM —
/// `true` when total RAM can't be read), and `selected` (the persisted `brain_model_id`). Feeds the
/// Phase-H model picker. No content read / no egress — static metadata + on-disk existence only.
#[tauri::command]
pub fn list_brain_models(
    state: State<'_, AppState>,
) -> Result<Vec<crate::reason::BrainModelDto>, AppError> {
    let (selected, light, heavy) = {
        let c = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
        (
            c.brain_model_id.clone(),
            c.brain_light_model_id.clone(),
            c.brain_heavy_model_id.clone(),
        )
    };
    let dir = crate::transcribe::models_dir()?;
    Ok(crate::reason::brain_model_dtos(
        &dir,
        total_ram_gb(),
        selected.as_deref(),
        light.as_deref(),
        heavy.as_deref(),
    ))
}

/// The installed-base migration nudge: `Some` when the persisted `brain_model_id` points at a RETIRED
/// model (e.g. the non-commercial `qwen2.5-3b`), telling the FE to offer the Apache-licensed
/// replacement. `None` for an active/absent selection. Read-only capability probe (no content, no
/// egress) — like [`brain_model_present`]. The retired GGUF keeps working until the user switches;
/// nothing is changed silently.
#[tauri::command]
pub fn brain_model_retirement_nudge(
    state: State<'_, AppState>,
) -> Result<Option<crate::reason::RetiredModelNudge>, AppError> {
    let selected = {
        let c = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
        c.brain_model_id.clone()
    };
    let dir = crate::transcribe::models_dir()?;
    Ok(crate::reason::retired_model_nudge(
        selected.as_deref(),
        &dir,
    ))
}

/// The DERIVED Murmur Brain posture (spec §2.1) for the Settings display — computed from the live
/// config (resolved role targets + `brain_live`), NEVER stored. `custom` when the dispatch keys match
/// no preset, so the label can never lie about egress. Read-only capability probe (no content).
#[tauri::command]
pub fn brain_posture(state: State<'_, AppState>) -> Result<crate::settings::Posture, AppError> {
    let c = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
    Ok(crate::settings::postures::derive_posture(&c))
}

/// Apply a Murmur Brain posture PRESET (`cloud` / `hybrid` / `fully_local`) and persist it. `custom`
/// is a derived-only label and is rejected (`InvalidArg`). This is the SINGLE writer of the posture
/// presets — the raw settings save preserves `brain_live` + the posture role keys (see
/// `dto_to_config`), so a partial save can never change the posture. The Hybrid preset deliberately
/// leaves `role_live_*` untouched (the @brain assistant stays intact).
#[tauri::command]
pub fn set_brain_posture(state: State<'_, AppState>, posture: String) -> Result<(), AppError> {
    let p = crate::settings::Posture::from_settable(&posture)
        .ok_or_else(|| AppError::InvalidArg(format!("not a settable posture: {posture}")))?;
    let mut c = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
    crate::settings::postures::apply_posture(&mut c, p);
    c.save(&state.db)?;
    Ok(())
}

/// The RESOLVED "what runs where" map for the Settings AI page — one row per AI job with its
/// resolved engine/model/locality (mirrors `roles::resolve`; display-only, steers nothing).
/// Read-only config projection: no content, no PII, no keys — NOT a gated content read.
#[tauri::command]
pub fn resolved_ai_map(
    state: State<'_, AppState>,
) -> Result<Vec<crate::settings::ai_map::AiMapRow>, AppError> {
    let c = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
    Ok(crate::settings::ai_map::ai_map_rows(&c))
}

/// Whether the machine has enough RAM to run Realtime Reactions (the light engine) alongside a live
/// recording — the combined-residency guard (spec §3.3), including a KV estimate + call-overhead. Lets
/// the Brain Live card warn / gate before enabling. `true` when total RAM can't be read (never block
/// behind a failed probe). Read-only capability probe.
#[tauri::command]
pub fn brain_live_ram_ok() -> Result<bool, AppError> {
    let light = crate::reason::default_model_for_class(crate::reason::ModelClass::Light);
    let models: Vec<&crate::reason::BrainModel> = light.into_iter().collect();
    Ok(crate::reason::residency_fits(
        &models,
        total_ram_gb(),
        crate::reason::CALL_OVERHEAD_GB,
    ))
}

/// The Realtime-Reactions SHADOW counter (spec §4.2): how many contradiction cards WOULD have fired
/// this recording while the sub-toggle is OFF. Lets the FE offer "the brain would have flagged N —
/// enable?" (user-local calibration, no telemetry). Read-only; resets each `start_recording`.
#[tauri::command]
pub fn brain_reactions_shadow_count(state: State<'_, AppState>) -> Result<u64, AppError> {
    Ok(state
        .reactions_shadow_count
        .load(std::sync::atomic::Ordering::Relaxed))
}

/// Flip the Realtime-Reactions CONTRADICTION-card sub-toggle (spec §4.2). Default OFF (shadow mode);
/// the FE offers this only once the user's OWN shadow count clears a bar. Dedicated command (not the
/// raw settings save, which preserves it) so a partial/older save can never silently enable ⚠ cards.
#[tauri::command]
pub fn set_brain_contradiction_cards(
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<(), AppError> {
    let mut c = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
    c.brain_contradiction_cards = enabled;
    c.save(&state.db)?;
    Ok(())
}

/// Persist the user's SELECTED on-device brain model id. Validates `model_id` against the registry
/// (unknown id ⇒ `AppError::InvalidArg`) and saves it to config; the reasoner dispatch
/// (`ReasonerCell`) re-resolves per call, so the model takes effect on the next reasoning call
/// once its GGUF is present — no restart. Does NOT download — the FE calls
/// `download_brain_model(model_id)` for that.
#[tauri::command]
pub fn select_brain_model(state: State<'_, AppState>, model_id: String) -> Result<(), AppError> {
    select_brain_model_inner(&state, model_id)
}

/// Testable core of [`select_brain_model`]: validate the id against the registry, persist it.
pub(crate) fn select_brain_model_inner(state: &AppState, model_id: String) -> Result<(), AppError> {
    let model = crate::reason::brain_model_by_id(&model_id)
        .ok_or_else(|| AppError::InvalidArg(format!("unknown brain model id: {model_id}")))?;
    let mut c = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
    // Keep the legacy single-model id (BrainBackend::Local + custom-path fallback) …
    c.brain_model_id = Some(model_id.clone());
    // … AND wire the CLASS handle so the Brain-Live `light()` / Fully-Local `heavy()` handles actually
    // use what the user just selected. Without this, selecting a light model leaves `light()` pointing
    // at the registry DEFAULT (a different, un-downloaded GGUF) → it silently resolves to the stub and
    // Realtime Reactions never fire despite Brain Live being "on".
    match model.class {
        crate::reason::ModelClass::Light => c.brain_light_model_id = Some(model_id),
        crate::reason::ModelClass::Heavy => c.brain_heavy_model_id = Some(model_id),
    }
    c.save(&state.db)?;
    Ok(())
}

/// Download the on-device brain model identified by `model_id` (from the curated registry) into the
/// shared models dir if missing; returns its path. No-op (returns the existing path) when already
/// present. Unknown id ⇒ `AppError::InvalidArg`. INBOUND ONLY — fetches a model file and sends NO
/// meeting content (no egress). Emits [`crate::events::EVENT_BRAIN_DOWNLOAD`] progress events
/// (throttled). The downloaded file is NOT loaded here — wiring the brain is a later step.
/// Resolve a registry `model_id` to its `(download url, on-disk dest)`. Unknown id ⇒
/// `AppError::InvalidArg` (the rejection [`download_brain_model`] enforces). Testable sync core.
pub(crate) fn brain_download_target(
    model_id: &str,
) -> Result<(&'static str, std::path::PathBuf), AppError> {
    let model = crate::reason::brain_model_by_id(model_id)
        .ok_or_else(|| AppError::InvalidArg(format!("unknown brain model id: {model_id}")))?;
    Ok((
        model.url,
        crate::transcribe::models_dir()?.join(model.filename),
    ))
}

/// WS2 (EXPERIMENTAL) — availability of the on-device Apple Foundation Models sidecar
/// (`meetnotes-afm`): is it bundled, and (if so) does its on-device model report available? Lets the
/// FE offer the "Apple Intelligence (on-device)" brain option ONLY on macOS-26 Apple-Silicon
/// hardware where the native sidecar is present — never advertise it elsewhere. On every current
/// (non-macOS-26) build the sidecar is ABSENT, so this returns
/// `{sidecar_present:false, model_available:None}`.
///
/// Opens NO content-read path (a device-capability probe only, like [`brain_model_present`]), so no
/// `meeting_is_unlocked` / `visibility_clause` gate applies. NEVER panics, NEVER egresses (the probe
/// is a local `--probe` spawn; a missing/wedged sidecar degrades gracefully).
#[tauri::command]
pub fn afm_available(app: AppHandle) -> Result<crate::reason::afm::AfmStatus, AppError> {
    Ok(crate::reason::afm::probe(Some(&app)))
}

/// `true` when all three multilingual-e5-small files are present in the shared models dir's embed
/// sub-dir — i.e. the REAL embedder would load (candle is always compiled; it activates on model
/// presence). Cheap existence probe; NEVER errors on a missing models dir (treats it as "not present").
#[tauri::command]
pub fn embed_model_present() -> Result<bool, AppError> {
    Ok(crate::embed::embed_model_present())
}

/// The bundled selectable embedders (multilingual-e5-small default + mmlw-retrieval-e5-small), each with
/// `downloaded` (files present in its own subdir) and `selected` (mirrors the persisted
/// `embed_model_id`). Feeds the embedder picker. No content read / no egress — static metadata +
/// on-disk existence only.
#[tauri::command]
pub fn list_embed_models(
    state: State<'_, AppState>,
) -> Result<Vec<crate::embed::EmbedModelDto>, AppError> {
    let selected = {
        let c = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
        c.embed_model_id.clone()
    };
    Ok(crate::embed::embed_model_dtos(selected.as_deref()))
}

/// Result of [`select_embed_model`]. `reindex_needed` is `true` when the selection actually CHANGED
/// the resolved model (so its old vectors are stale — a different model's embeddings are not
/// comparable): the FE should prompt the user to run `reindex_embeddings` (and download the new
/// model first via `download_embed_model` if `!embed_model_present()`). Counts/flags only — no PII.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectEmbedModelResult {
    pub selected: String,
    pub reindex_needed: bool,
    pub model_present: bool,
}

/// Persist the user's SELECTED on-device embedding model id. Validates `model_id` against the
/// registry (unknown ⇒ `AppError::InvalidArg`) and saves it to config; the selection update seam
/// republishes the process-global resolver so `embed::active_embedder`/`embed_model_present`/
/// `download_embed_model` pick up the new model with NO restart. Switching the model INVALIDATES
/// existing vectors (a different model's embeddings are not comparable), so `reindex_needed` is
/// `true` when the resolved model actually changed — the FE then prompts the user to download (if
/// missing) + re-index. All
/// bundled options are BERT/384 ⇒ NO `vec0` schema migration. Does NOT download and does NOT auto-
/// reindex (both are explicit user actions).
#[tauri::command]
pub fn select_embed_model(
    state: State<'_, AppState>,
    model_id: String,
) -> Result<SelectEmbedModelResult, AppError> {
    select_embed_model_inner(&state, model_id)
}

/// Testable core of [`select_embed_model`]: validate the id, compute whether the resolved model
/// changed, persist it. No `AppHandle`, so it runs headless.
pub(crate) fn select_embed_model_inner(
    state: &AppState,
    model_id: String,
) -> Result<SelectEmbedModelResult, AppError> {
    let model = crate::embed::embed_model_by_id(&model_id)
        .ok_or_else(|| AppError::InvalidArg(format!("unknown embed model id: {model_id}")))?;

    let reindex_needed = crate::embed::with_embed_selection_update(|| {
        let mut c = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?;

        // The PREVIOUS resolved model id (None/empty/unknown ⇒ the default) — the re-index trigger
        // keys on a real resolved-model change, not merely a config-string write.
        let prev_resolved = c
            .embed_model_id
            .as_deref()
            .filter(|s| !s.is_empty())
            .and_then(crate::embed::embed_model_by_id)
            .map(|m| m.id)
            .unwrap_or(crate::embed::DEFAULT_EMBED_MODEL_ID);
        let reindex_needed = prev_resolved != model.id;

        // Persist a candidate first. Mutating the live cache before a fallible DB transaction would
        // leave cache=B while the process resolver remains A. A real model switch atomically purges
        // all old-model vector partitions with the setting write; chunks/FTS remain available.
        let mut candidate = c.clone();
        candidate.embed_model_id = Some(model.id.to_string());
        state
            .db
            .set_embed_model_selection(model.id, reindex_needed)?;
        let selected_id = candidate.embed_model_id.clone();
        *c = candidate;
        Ok((reindex_needed, selected_id))
    })?;

    Ok(SelectEmbedModelResult {
        selected: model.id.to_string(),
        reindex_needed,
        model_present: crate::embed::embed_model_present(),
    })
}

/// True iff the on-device PERSON-name NER model (Phase D) is present on disk. Pure existence probe;
/// NEVER errors on a missing models dir (treats it as "not present"). When false, the redaction
/// firewall uses the byte-identical NoopNameRedactor (candle NER is always compiled; it activates on
/// model presence).
#[tauri::command]
pub fn ner_model_present() -> Result<bool, AppError> {
    Ok(crate::summarize::redact::ner_model_present())
}

/// Download the multilingual mDeBERTa-v3 NER model (3 HF files) into the shared models dir,
/// INBOUND-ONLY, emitting [`crate::events::EVENT_NER_DOWNLOAD`] progress (throttled per file). Sends
/// NO meeting content (no egress). The downloaded model is NOT loaded here — it is picked up lazily by
/// `summarize::redact::active_name_redactor` on the next cloud summarization (selected on model
/// presence). Returns the model dir.
#[tauri::command]
pub async fn download_ner_model(app: AppHandle) -> Result<String, AppError> {
    let file_count = crate::summarize::redact::NER_MODEL_FILES.len();
    // Throttle progress to roughly every 2 MB so the model download doesn't flood the FE.
    const EMIT_EVERY: u64 = 2 * 1024 * 1024;
    let mut last_emit: u64 = 0;
    let mut last_index: usize = usize::MAX;
    let dir = crate::summarize::redact::download_ner_model(|file_index, downloaded, total| {
        if file_index != last_index || downloaded - last_emit >= EMIT_EVERY {
            last_index = file_index;
            last_emit = downloaded;
            let _ = app.emit(
                crate::events::EVENT_NER_DOWNLOAD,
                crate::events::NerDownloadPayload {
                    file_index,
                    file_count,
                    downloaded,
                    total,
                    done: false,
                },
            );
        }
    })
    .await?;

    let _ = app.emit(
        crate::events::EVENT_NER_DOWNLOAD,
        crate::events::NerDownloadPayload {
            file_index: file_count,
            file_count,
            downloaded: 0,
            total: None,
            done: true,
        },
    );
    Ok(dir.to_string_lossy().to_string())
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// P1 — the hardware-aware whisper recommendation (ONE command) + the machine-change nudge.
//
// Everything below is a config read, a models-dir listing, or a hardware probe. NO meeting content
// is touched, so no `meeting_is_unlocked` / `visibility_clause` gate applies (same posture as the
// rest of this module). Nothing egresses. Nothing here logs.
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// The hardware facts a consumer actually READS. Deliberately narrow (C11): logical/perf core
/// counts, OS version, low-power mode and the thermal level are cheap to probe but do NOT cross
/// IPC until something branches on them.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineProfileDto {
    pub total_ram_bytes: Option<u64>,
    pub apple_silicon: Option<bool>,
    /// Normalised to `Apple …`, or dropped entirely (Intel's long brand string is rejected).
    pub chip_name: Option<String>,
    /// Free space on the volume holding the models dir. VOLATILE — read once, here, so two
    /// consumers can never disagree about it.
    pub free_disk_bytes: Option<u64>,
}

/// One catalog row, plus whether its file is on disk right now.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WhisperModelDto {
    pub id: String,
    /// The ladder rung label (`Light` / `Balanced` / `Sharp` / `Maximum`), or `None` for a
    /// long-tail size — the FE then shows the raw id.
    pub tier: Option<String>,
    pub headline: String,
    pub approx_download_bytes: Option<u64>,
    pub approx_ram_bytes: Option<u64>,
    pub live_safe: bool,
    pub power: u8,
    pub downloaded: bool,
}

/// What ONE command answers: the machine, the catalog, and the two DIFFERENT answers to "which
/// model?" — see [`WhisperRecommendationDto::recommended_id`] vs
/// [`WhisperRecommendationDto::auto_default_id`].
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WhisperRecommendationDto {
    pub machine: MachineProfileDto,
    /// The visible catalog (provisional rows excluded), ascending by cost.
    pub models: Vec<WhisperModelDto>,
    /// The size that will actually load right now (a blank `model_size` resolved).
    pub selected_id: String,
    /// The HONEST hardware answer — what this Mac deserves, blind to what is on disk.
    pub recommended_id: String,
    /// The NEVER-SURPRISE answer — what a blank config resolves to today (presence-first). It
    /// differs from `recommended_id` on most existing installs, and shipping only one of the two
    /// would make the badge contradict the selected size.
    pub auto_default_id: String,
    /// Why `auto_default_id` is what it is. The RAM-causal sentence belongs to
    /// `freshInstallAmpleRam` and to nothing else.
    pub reason: crate::transcribe::catalog::RecommendReason,
    /// `"auto"` / `"user"` / absent — who put `selected_id` there.
    pub model_size_source: Option<String>,
    /// A custom `whisper_model_path` is configured AND exists, so it overrides the ladder entirely.
    pub custom_path_override: bool,
    /// Bytes a download would transfer RIGHT NOW for the selected size, INCLUDING the live-caption
    /// companion when one is planned. Computed here so the FE never sums model sizes itself.
    ///
    /// `Some(0)` = nothing to fetch. `None` = a download IS pending but its size is unknown — the
    /// FE must say "size unknown", never "free". The two are deliberately NOT collapsed: an id with
    /// no measured size would otherwise promise a free multi-GB transfer.
    pub pending_download_bytes: Option<u64>,
    /// The brain posture we would advise on this machine (advice only — it gates nothing).
    pub brain_advice: crate::reason::BrainAdvice,
}

/// The one-shot machine-change nudge (C3). `Some` only while the pending row is set — i.e. this
/// install last ran on a DIFFERENT Mac and has not dismissed the notice.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineChangeNudge {
    /// What this (new) machine deserves.
    pub recommended_id: String,
    /// Its ladder label, when it has one.
    pub recommended_tier: Option<String>,
    /// What is selected today.
    pub selected_id: String,
    pub chip_name: Option<String>,
    pub total_ram_bytes: Option<u64>,
}

/// Build the DTO from already-gathered facts. PURE over `(cfg, dir, files, profile, …)` — no
/// subprocess and no clock — so every branch is headless-testable with a temp dir.
fn build_recommendation(
    cfg: &crate::settings::AppConfig,
    dir: &std::path::Path,
    files: &[String],
    profile: &crate::machine::MachineProfile,
    free_disk_bytes: Option<u64>,
    model_size_source: Option<String>,
) -> WhisperRecommendationDto {
    use crate::transcribe::catalog;

    let language = cfg.language.as_deref().unwrap_or("");
    let configured = cfg
        .whisper_model_path
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(std::path::Path::new);
    let custom_path_override = configured.is_some_and(|p| p.is_file());

    let turbo_on_disk = crate::transcribe::model::turbo_present_in(files);
    let auto_default_id = crate::transcribe::model::default_model_size_for(
        files,
        profile.total_ram_bytes,
        profile.apple_silicon,
    );

    // The size that will actually load. A blank config resolves through `default_model_size_for`
    // over the SAME `files` + `profile` this DTO already holds — deliberately NOT through
    // `effective_model_size`, which would call `default_model_size_now()` and re-list the models dir
    // and re-read the machine. Two listings taken at different instants can disagree mid-download,
    // which is exactly what the one-listing rule exists to prevent; it would also break this
    // function's purity (and with it the headless tests, which pass a temp dir this fn would ignore).
    let selected_id = {
        let s = cfg.model_size.trim();
        if s.is_empty() {
            auto_default_id.to_string()
        } else {
            s.to_string()
        }
    };
    let hardware = catalog::recommend_for(profile);
    // `any_whisper_model_in` is what separates "install history kept the default conservative" from
    // "this machine's hardware did". Without it a genuinely FRESH install on a modest Apple-Silicon
    // Mac was reported as `ExistingInstall` — a history claim about a machine with no history.
    let any_model_on_disk = crate::transcribe::model::any_whisper_model_in(files);
    let reason = catalog::auto_default_reason(
        auto_default_id,
        turbo_on_disk,
        any_model_on_disk,
        hardware.reason,
    );

    let present = |id: &str| {
        let name = crate::transcribe::model_filename(id, language);
        files.iter().any(|f| f == &name)
    };

    let models: Vec<WhisperModelDto> = catalog::visible()
        .map(|m| WhisperModelDto {
            id: m.id.to_string(),
            tier: m.tier.map(|t| t.label().to_string()),
            headline: m.headline.to_string(),
            approx_download_bytes: m.approx_download_bytes,
            approx_ram_bytes: m.approx_ram_bytes,
            live_safe: m.live_safe,
            power: m.power,
            downloaded: present(m.id),
        })
        .collect();

    WhisperRecommendationDto {
        machine: MachineProfileDto {
            total_ram_bytes: profile.total_ram_bytes,
            apple_silicon: profile.apple_silicon,
            chip_name: profile.chip_name.clone(),
            free_disk_bytes,
        },
        models,
        pending_download_bytes: pending_download_bytes(cfg, dir, &selected_id, present),
        selected_id,
        recommended_id: hardware.id.to_string(),
        auto_default_id: auto_default_id.to_string(),
        reason,
        model_size_source,
        custom_path_override,
        brain_advice: crate::reason::brain_advice_for(profile.total_ram_bytes),
    }
}

/// Bytes a `download_model` would transfer right now: the selected batch model when its file is
/// absent, PLUS the live-caption companion when `companion_pending_size_in` says one is planned.
/// A size with no measured figure contributes nothing (we never invent one), and a custom
/// `whisper_model_path` that already exists means there is nothing to fetch at all.
/// Returns `None` when a download IS pending but its size is not known — deliberately distinct from
/// `Some(0)` ("nothing to fetch"). Collapsing the two would let an id whose size we never measured
/// (`large-v3-q5_0`, or any id from an older or forked build) promise a free download while a
/// multi-GB transfer is actually queued. Under-disclosing a download size is exactly the dishonesty
/// this workstream exists to remove.
fn pending_download_bytes(
    cfg: &crate::settings::AppConfig,
    dir: &std::path::Path,
    selected_id: &str,
    present: impl Fn(&str) -> bool,
) -> Option<u64> {
    use crate::transcribe::catalog;

    let language = cfg.language.as_deref().unwrap_or("");
    let configured = cfg
        .whisper_model_path
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(std::path::Path::new);

    // `Some(0)` = nothing pending; `None` = something IS pending but its size is unknown.
    let batch: Option<u64> = if configured.is_some_and(|p| p.is_file()) || present(selected_id) {
        Some(0)
    } else {
        catalog::model_by_id(selected_id).and_then(|m| m.approx_download_bytes)
    };
    let companion: Option<u64> = match super::live_captions::companion_pending_size_in(
        dir,
        configured,
        &cfg.model_size,
        language,
        &cfg.live_model_pin,
        cfg.brain_live,
    ) {
        None => Some(0),
        Some(size) => catalog::model_by_id(&size).and_then(|m| m.approx_download_bytes),
    };

    // Unknown on EITHER leg makes the total unknown: a partial sum presented as the whole would
    // understate the transfer, which is the failure direction that matters.
    Some(batch?.saturating_add(companion?))
}

/// THE command (C12): one call answers "what Mac is this, what can it run, what is selected, and
/// what would a download cost". A separate `machine_profile()` command is deliberately NOT shipped
/// — two sources re-reading a volatile field (free disk) at different instants can disagree.
///
/// Read-only capability probe: config + a models-dir listing + hardware. No content, no egress.
#[tauri::command]
pub fn whisper_recommendation(
    state: State<'_, AppState>,
) -> Result<WhisperRecommendationDto, AppError> {
    let cfg = {
        let c = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
        c.clone()
    };
    let dir = crate::transcribe::models_dir()?;
    // ONE listing feeds every presence fact in the DTO, so the auto default and the per-row
    // `downloaded` flags cannot disagree mid-download.
    let files = crate::transcribe::model::models_dir_file_names(&dir).unwrap_or_default();
    let free_disk = crate::machine::free_disk_bytes(&dir);
    let source = crate::settings::model_size_source(&state.db);
    Ok(build_recommendation(
        &cfg,
        &dir,
        &files,
        crate::machine::profile(),
        free_disk,
        source,
    ))
}

/// The machine-change nudge as a PULL (C3). `Some` while the pending settings row is set — the row
/// is written at startup, so it survives a webview that had not yet subscribed to anything. There
/// is deliberately NO `EVENT_MACHINE_CHANGED`: Tauri does not buffer events, and one emitted during
/// `setup` is simply lost. Precedent: [`brain_model_retirement_nudge`].
#[tauri::command]
pub fn machine_change_nudge(
    state: State<'_, AppState>,
) -> Result<Option<MachineChangeNudge>, AppError> {
    if !crate::settings::machine_change_pending(&state.db) {
        return Ok(None);
    }
    let size = {
        let c = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
        c.model_size.clone()
    };
    let profile = crate::machine::profile();
    let rec = crate::transcribe::catalog::recommend_for(profile);
    Ok(Some(MachineChangeNudge {
        recommended_id: rec.id.to_string(),
        recommended_tier: crate::transcribe::catalog::tier_label(rec.id).map(str::to_string),
        selected_id: crate::transcribe::effective_model_size(&size),
        chip_name: profile.chip_name.clone(),
        total_ram_bytes: profile.total_ram_bytes,
    }))
}

/// Dismiss the machine-change nudge. Idempotent; the fingerprint itself was already moved forward
/// at startup, so dismissing simply stops the notice from coming back.
#[tauri::command]
pub fn dismiss_machine_change_nudge(state: State<'_, AppState>) -> Result<(), AppError> {
    crate::settings::clear_machine_change_pending(&state.db)
}

#[cfg(test)]
mod recommendation_tests {
    use super::*;
    use crate::machine::MachineProfile;
    use crate::settings::AppConfig;
    use crate::transcribe::catalog::{RecommendReason, BALANCED_ID, MAXIMUM_ID, SHARP_ID};

    const GIB: u64 = 1024 * 1024 * 1024;

    fn tmp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "murmur-reco-{tag}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn apple(ram_gib: u64) -> MachineProfile {
        MachineProfile {
            total_ram_bytes: Some(ram_gib * GIB),
            apple_silicon: Some(true),
            rosetta: Some(false),
            chip_name: Some("Apple M4 Max".to_string()),
        }
    }

    fn dto(
        cfg: &AppConfig,
        dir: &std::path::Path,
        files: &[&str],
        profile: &MachineProfile,
    ) -> WhisperRecommendationDto {
        let owned: Vec<String> = files.iter().map(|s| s.to_string()).collect();
        build_recommendation(cfg, dir, &owned, profile, None, None)
    }

    /// The flagship split (C1): on a 64 GiB Apple-Silicon Mac that ALREADY has `ggml-small.bin`,
    /// the hardware answer is Sharp while the never-surprise auto default stays Balanced. If the
    /// DTO carried only one of the two, the badge would contradict the selected size on every
    /// existing install.
    #[test]
    fn dto_carries_both_the_hardware_answer_and_the_no_surprise_default() {
        let dir = tmp_dir("split");
        let cfg = AppConfig {
            model_size: "small".to_string(),
            ..AppConfig::default()
        };
        let d = dto(&cfg, &dir, &["ggml-small.bin"], &apple(64));
        assert_eq!(d.recommended_id, SHARP_ID);
        assert_eq!(d.auto_default_id, BALANCED_ID);
        assert_eq!(d.selected_id, "small");
        assert_eq!(
            d.reason,
            RecommendReason::ExistingInstall,
            "a presence-first decision must NOT be reported as RAM-causal"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The ONE branch that earns the RAM-causal sentence, and the one that must not: a fresh
    /// ample-RAM install reads `freshInstallAmpleRam`; the same machine with the turbo file already
    /// downloaded reads `alreadyDownloaded`.
    #[test]
    fn reason_distinguishes_fresh_ample_ram_from_already_downloaded() {
        let dir = tmp_dir("reason");
        let cfg = AppConfig::default();

        let fresh = dto(&cfg, &dir, &[], &apple(32));
        assert_eq!(fresh.auto_default_id, SHARP_ID);
        assert_eq!(fresh.reason, RecommendReason::FreshInstallAmpleRam);

        let downloaded = dto(&cfg, &dir, &["ggml-large-v3-turbo-q8_0.bin"], &apple(32));
        assert_eq!(downloaded.auto_default_id, SHARP_ID);
        assert_eq!(downloaded.reason, RecommendReason::AlreadyDownloaded);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Intel keeps its own reason all the way out to the DTO, and drops the chip clause.
    #[test]
    fn intel_reason_survives_to_the_dto() {
        let dir = tmp_dir("intel");
        let cfg = AppConfig::default();
        let intel = MachineProfile {
            apple_silicon: Some(false),
            chip_name: None,
            ..apple(32)
        };
        let d = dto(&cfg, &dir, &[], &intel);
        assert_eq!(d.recommended_id, BALANCED_ID);
        assert_eq!(d.auto_default_id, BALANCED_ID);
        assert_eq!(d.reason, RecommendReason::NotAppleSilicon);
        assert_eq!(d.machine.chip_name, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The companion to the test above, and the one that matters on real hardware: an UNREADABLE
    /// arch probe must reach the same conservative size but a reason that makes NO chip claim.
    /// `hw.optional.arm64` is absent on Intel, so `None` — not `Some(false)` — is where a real
    /// Intel Mac lands, and it is equally where an Apple-Silicon Mac lands when the probe fails.
    /// A shared reason would print chip-family copy for a machine we never identified.
    #[test]
    fn an_unreadable_arch_probe_reaches_the_dto_without_a_chip_claim() {
        let dir = tmp_dir("arch-unknown");
        let cfg = AppConfig::default();
        let unknown = MachineProfile {
            apple_silicon: None,
            chip_name: None,
            ..apple(32)
        };
        let d = dto(&cfg, &dir, &[], &unknown);
        assert_eq!(d.recommended_id, BALANCED_ID);
        assert_eq!(d.reason, RecommendReason::ArchUnknown);
        assert_ne!(
            d.reason,
            RecommendReason::NotAppleSilicon,
            "an unmeasured chip must never be described as not-Apple-Silicon"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Per-row `downloaded` flags come from the SAME listing the auto default used, and only the
    /// visible (non-provisional) rows ship.
    #[test]
    fn rows_report_presence_and_hide_provisional_sizes() {
        let dir = tmp_dir("rows");
        let cfg = AppConfig::default();
        let d = dto(&cfg, &dir, &["ggml-small.bin", "ggml-base.bin"], &apple(16));
        let row = |id: &str| d.models.iter().find(|m| m.id == id).unwrap().clone();
        assert!(row("small").downloaded);
        assert!(row("base").downloaded);
        assert!(!row(SHARP_ID).downloaded);
        assert!(!d.models.iter().any(|m| m.id == "large-v3-q5_0"));
        // The ladder labels ride along; long-tail rows carry none.
        assert_eq!(row("small").tier.as_deref(), Some("Balanced"));
        assert_eq!(row("medium").tier, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// C8 — the pending byte total is computed in Rust and INCLUDES the live-caption companion.
    /// A fresh install on the turbo batch model needs a live-safe companion (turbo is not live-safe),
    /// so the total must be turbo + small, never turbo alone.
    #[test]
    fn pending_bytes_include_the_live_caption_companion() {
        use crate::transcribe::catalog;
        let dir = tmp_dir("pending");
        let cfg = AppConfig {
            model_size: SHARP_ID.to_string(),
            ..AppConfig::default()
        };
        let turbo = catalog::model_by_id(SHARP_ID)
            .unwrap()
            .approx_download_bytes
            .unwrap();
        let small = catalog::model_by_id(BALANCED_ID)
            .unwrap()
            .approx_download_bytes
            .unwrap();

        // Nothing on disk: both the batch model and its companion are pending.
        let d = dto(&cfg, &dir, &[], &apple(32));
        assert_eq!(d.pending_download_bytes, Some(turbo + small));

        // With a live-safe model already on disk the companion is no longer planned — only the
        // batch model is pending. The file must REALLY exist: `companion_size_lazy` stats the dir.
        std::fs::write(dir.join("ggml-small.bin"), b"x").unwrap();
        let d = dto(&cfg, &dir, &["ggml-small.bin"], &apple(32));
        assert_eq!(d.pending_download_bytes, Some(turbo));

        // With the batch model also on disk nothing is pending at all.
        std::fs::write(dir.join("ggml-large-v3-turbo-q8_0.bin"), b"x").unwrap();
        let d = dto(
            &cfg,
            &dir,
            &["ggml-small.bin", "ggml-large-v3-turbo-q8_0.bin"],
            &apple(32),
        );
        assert_eq!(d.pending_download_bytes, Some(0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An UNKNOWN pending size must be `None`, never `Some(0)`. `Some(0)` is the wire's promise of
    /// "nothing to fetch", so a size we never measured must not borrow it — that would advertise a
    /// free download for a transfer that is really multiple gigabytes.
    #[test]
    fn an_unmeasured_pending_size_is_unknown_not_free() {
        let dir = tmp_dir("unknown-size");
        // `large-v3-q5_0` is a real, downloadable size that deliberately carries NO measured
        // download figure in the registry (it ships `provisional`), so it is the honest fixture.
        let cfg = AppConfig {
            model_size: "large-v3-q5_0".to_string(),
            ..AppConfig::default()
        };
        let d = dto(&cfg, &dir, &[], &apple(64));
        assert_eq!(
            d.pending_download_bytes, None,
            "an unmeasured size must report UNKNOWN, never a free download"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// B13 — a custom `whisper_model_path` that exists overrides the ladder, and there is nothing
    /// left to download.
    #[test]
    fn custom_model_path_overrides_the_ladder() {
        let dir = tmp_dir("custom");
        let custom = dir.join("my-own-model.bin");
        std::fs::write(&custom, b"x").unwrap();
        let cfg = AppConfig {
            whisper_model_path: Some(custom.to_string_lossy().to_string()),
            model_size: MAXIMUM_ID.to_string(),
            ..AppConfig::default()
        };
        let d = dto(&cfg, &dir, &[], &apple(64));
        assert!(d.custom_path_override);
        assert_eq!(d.pending_download_bytes, Some(0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The brain advice rides the same profile and holds the raised 24 GB floor.
    #[test]
    fn brain_advice_rides_the_same_profile() {
        let dir = tmp_dir("brain");
        let cfg = AppConfig::default();
        assert_eq!(
            dto(&cfg, &dir, &[], &apple(64)).brain_advice,
            crate::reason::BrainAdvice::Full
        );
        assert_eq!(
            dto(&cfg, &dir, &[], &apple(16)).brain_advice,
            crate::reason::BrainAdvice::Reactions
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
