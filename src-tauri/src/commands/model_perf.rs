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

/// macOS total physical RAM in whole GB via `sysctl -n hw.memsize` (no new FFI/crate). Returns
/// `None` on any error — the caller then treats every model as fitting rather than HIDING it behind
/// a failed probe.
fn total_ram_gb() -> Option<u64> {
    let out = std::process::Command::new("sysctl")
        .arg("-n")
        .arg("hw.memsize")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let bytes: u64 = String::from_utf8(out.stdout).ok()?.trim().parse().ok()?;
    Some(bytes / (1024 * 1024 * 1024))
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
