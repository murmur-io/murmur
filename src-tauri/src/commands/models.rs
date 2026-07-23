//! ML-MODEL + AI-GATEWAY management command surface (NOT content-gated).
//!
//! Extracted verbatim from `commands/mod.rs` (God-file split, PURE MOVE — every body is
//! byte-identical, only relocated). Two clusters, both OUTSIDE the lock model — neither reads or
//! writes sealed meeting/note content, so there is NO `meeting_is_unlocked` / `visibility_clause`
//! gate here (nothing to gate):
//!   1. AI-GATEWAY / provider catalog + health (`list_gateway_models`, `list_models`,
//!      `gateway_health`, `provider_statuses` + the `gateway_model_ids` / `static_connection_models`
//!      cores + the `GatewayModelDto` / `GatewayHealthDto` DTOs). INBOUND-ONLY: at most an
//!      `Authorization: Bearer` header to the configured gateway ever leaves — NO meeting content —
//!      so no redaction firewall / consent gate is needed (the `list_gateway_models` precedent).
//!   2. MODEL DOWNLOADS + vector REINDEX trigger (`download_model`, `download_parakeet_models`,
//!      `download_brain_model`, `download_embed_model`, `reindex_embeddings`). The downloads are
//!      INBOUND-ONLY model-file fetches (no egress). `reindex_embeddings` IS visibility-aware, but
//!      it does so entirely through the SHARED `reindex_embeddings_inner` — whose corpus is exactly
//!      `list_meetings_visible(unlocked)` (a sealed-not-session-unlocked meeting is never indexed) —
//!      which STAYS in `commands/mod.rs` (it is also called by the startup repair tick + covered by
//!      the lifecycle tests). The `ReindexResult` DTO and every reindex/backfill HELPER
//!      (`reindex_embeddings_inner`, `backfill_document_chunks`, `index_document_row_kind_routed`)
//!      likewise STAY in `commands/mod.rs`; the moved `reindex_embeddings` reaches them through
//!      `use super::*`.
//!
//! Bound as `models_commands` (via `#[path]`) to avoid colliding with the crate-level model modules
//! (`crate::transcribe::model` / `crate::reason` / `crate::embed`) these commands call (E0255). The
//! glob re-export makes every moved command resolve UNCHANGED at `crate::commands::…` for
//! `generate_handler!` in `lib.rs` and every caller. The shared imports (`AppError`, `AppHandle`,
//! `AppState`, `State`, `AppConfig`, `ProviderStatus`, `SearchHit`, `all_providers`, `secrets`,
//! `GATEWAY_KEY_ACCOUNT`, `ReindexResult`, …) come in via `use super::*`.

use super::*;

/// DTO for a single model returned by `list_gateway_models`.
///
/// Shape: `{ "id": "gpt-4o" }` (camelCase). The FE populates the model picker from this.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayModelDto {
    pub id: String,
}

/// Fetch the model catalog from the configured AI Gateway (`GET {base}/v1/models`) and return
/// the raw list of model ids. Shared core of `list_gateway_models` and `list_models("gateway")`.
///
/// Inbound-only: this call sends NO meeting content — only an optional `Authorization: Bearer`
/// header carrying the stored gateway key. It therefore does NOT need the redaction firewall or
/// the cloud-egress consent gate.
///
/// Returns `AppError::InvalidArg` when no gateway base URL is configured.
async fn gateway_model_ids(state: &AppState) -> Result<Vec<String>, AppError> {
    let (base_url, model, api_key) = {
        let config = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
        let base_url = config.gateway_base_url.clone();
        let model = config.gateway_model.clone();
        // R3: resolve the gateway key; never falls back to the Anthropic key.
        let api_key = secrets::get_secret(GATEWAY_KEY_ACCOUNT).ok().flatten();
        (base_url, model, api_key)
    };

    if base_url.trim().is_empty() {
        return Err(AppError::InvalidArg(
            "no gateway base URL configured — set it in Settings before fetching the model catalog"
                .into(),
        ));
    }

    let provider = crate::summarize::gateway::OpenAiCompatProvider::new(base_url, model, api_key)?;
    provider.list_models().await
}

/// Fetch the model catalog from the configured AI Gateway (`GET {base}/v1/models`) and return
/// the list of model ids the gateway exposes. Thin DTO wrapper over [`gateway_model_ids`] —
/// kept until the FE swaps to the unified `list_models("gateway")`.
#[tauri::command]
pub async fn list_gateway_models(
    state: State<'_, AppState>,
) -> Result<Vec<GatewayModelDto>, AppError> {
    let ids = gateway_model_ids(&state).await?;
    Ok(ids.into_iter().map(|id| GatewayModelDto { id }).collect())
}

/// Model ids for the connections whose catalog is static, compile-time data — pure so it is
/// unit-testable without state or network. `None`-network connections only:
///   - `"claude_code"` / `"anthropic"` → the curated [`crate::summarize::provider::CLAUDE_MODELS`],
///   - `"local"` → the on-device `reason::BRAIN_MODELS` registry ids,
///   - `"off"` → empty (a valid connection that runs no models),
///   - anything else → `AppError::InvalidArg`.
pub(crate) fn static_connection_models(connection: &str) -> Result<Vec<String>, AppError> {
    match connection {
        "claude_code" | "anthropic" => Ok(crate::summarize::provider::CLAUDE_MODELS
            .iter()
            .map(|id| id.to_string())
            .collect()),
        "local" => Ok(crate::reason::BRAIN_MODELS
            .iter()
            .map(|m| m.id.to_string())
            .collect()),
        "off" => Ok(vec![]),
        other => Err(AppError::InvalidArg(format!(
            "unknown connection '{other}' — expected claude_code, anthropic, ollama, gateway, local, or off"
        ))),
    }
}

/// Unified model catalog for a connection — the ONE source of truth behind the FE per-role
/// model dropdowns. Dispatch by `connection`:
///   - `"gateway"` → `GET {gateway_base_url}/v1/models` (shares [`gateway_model_ids`] with
///     `list_gateway_models`),
///   - `"ollama"` → `GET {ollama_base_url}/api/tags`, model names only,
///   - `"claude_code"` / `"anthropic"` / `"local"` / `"off"` → static lists
///     (see [`static_connection_models`]),
///   - anything else → `AppError::InvalidArg`.
///
/// Inbound-only on every arm: NO meeting content is sent — at most an `Authorization: Bearer`
/// header to the configured gateway — so no redaction firewall / consent gate is needed (the
/// `list_gateway_models` precedent). No content read → no lock-model surface.
#[tauri::command]
pub async fn list_models(
    state: State<'_, AppState>,
    connection: String,
) -> Result<Vec<String>, AppError> {
    match connection.as_str() {
        "gateway" => gateway_model_ids(&state).await,
        "ollama" => {
            let base_url = {
                let config = state
                    .config
                    .lock()
                    .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
                config.ollama_base_url.clone()
            };
            // OllamaProvider::new normalizes an empty base URL to the localhost default and
            // trims trailing slashes; the model arg is unused for a catalog fetch.
            crate::summarize::ollama::OllamaProvider::new(base_url, String::new())
                .list_models()
                .await
        }
        other => static_connection_models(other),
    }
}

/// DTO returned by `gateway_health`.
///
/// Shape: `{ "reachable": true, "modelCount": 6 }` (camelCase, matches the FE `GatewayHealth`
/// type). `reachable: false` with `model_count: 0` means the gateway is unreachable or not
/// configured — the FE renders a red dot.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayHealthDto {
    pub reachable: bool,
    pub model_count: u32,
}

/// Probe the configured AI Gateway for reachability and (optionally) catalog size.
///
/// Uses `OpenAiCompatProvider::probe()` which sends a GET to the models endpoint when one
/// exists, or to the chat endpoint for custom routes (a GET to a POST-only route returns
/// 4xx/405 but proves the server is reachable — no LLM call, no cost). Any HTTP response
/// (any status) → `reachable: true`; a transport failure → `reachable: false`.
///
/// This command NEVER returns an `Err` variant so the FE health dot always gets a clean value.
/// Inbound-only: sends NO meeting content, only an optional `Authorization: Bearer` header. Does
/// NOT need the redaction firewall or the consent gate (same rationale as `list_gateway_models`).
#[tauri::command]
pub async fn gateway_health(state: State<'_, AppState>) -> Result<GatewayHealthDto, AppError> {
    let (base_url, model, api_key) = {
        let config = match state.config.lock() {
            Ok(c) => c,
            Err(_) => {
                return Ok(GatewayHealthDto {
                    reachable: false,
                    model_count: 0,
                })
            }
        };
        let base_url = config.gateway_base_url.clone();
        let model = config.gateway_model.clone();
        // R3: resolve the gateway key; never falls back to the Anthropic key.
        let api_key = secrets::get_secret(GATEWAY_KEY_ACCOUNT).ok().flatten();
        (base_url, model, api_key)
    };

    if base_url.trim().is_empty() {
        // Not configured → degrade silently.
        return Ok(GatewayHealthDto {
            reachable: false,
            model_count: 0,
        });
    }

    let provider =
        match crate::summarize::gateway::OpenAiCompatProvider::new(base_url, model, api_key) {
            Ok(p) => p,
            Err(_) => {
                return Ok(GatewayHealthDto {
                    reachable: false,
                    model_count: 0,
                })
            }
        };

    // probe() never returns Err — degrades to (false, 0) on transport failure.
    let (reachable, model_count) = provider.probe().await;
    Ok(GatewayHealthDto {
        reachable,
        model_count,
    })
}

/// availability() fan-out across all three providers for the Settings UI.
#[tauri::command]
pub async fn provider_statuses(
    state: State<'_, AppState>,
) -> Result<Vec<ProviderStatus>, AppError> {
    use crate::summarize::provider::Availability;

    let config: AppConfig = {
        let guard = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
        guard.clone()
    };

    let providers = all_providers(&config);
    let mut out = Vec::with_capacity(providers.len());
    for p in providers {
        let (available, reason) = match p.availability().await {
            Availability::Available => (true, None),
            Availability::Unavailable { reason } => (false, Some(reason)),
        };
        out.push(ProviderStatus {
            id: p.id().to_string(),
            available,
            reason,
        });
    }
    Ok(out)
}

/// Download the Whisper model matching the chosen size + language (multilingual unless
/// English is selected) from the whisper.cpp HuggingFace mirror into the app models dir if
/// missing; returns its path. No-op (returns the existing path) when already present. Emits
/// [`crate::events::EVENT_MODEL_DOWNLOAD`] progress (throttled) so the FE can show a progress bar.
#[tauri::command]
pub async fn download_model(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<String, AppError> {
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

    // Throttle progress events to roughly every 8 MB so a multi-GB download doesn't flood the FE.
    const EMIT_EVERY: u64 = 8 * 1024 * 1024;
    let mut last_emit: u64 = 0;
    let path = crate::transcribe::ensure_model(p, &size, &language, |downloaded, total| {
        if downloaded - last_emit >= EMIT_EVERY {
            last_emit = downloaded;
            let _ = app.emit(
                crate::events::EVENT_MODEL_DOWNLOAD,
                crate::events::ModelDownloadPayload {
                    downloaded,
                    total,
                    done: false,
                },
            );
        }
    })
    .await?;

    let _ = app.emit(
        crate::events::EVENT_MODEL_DOWNLOAD,
        crate::events::ModelDownloadPayload {
            downloaded: 0,
            total: None,
            done: true,
        },
    );
    Ok(path.to_string_lossy().to_string())
}

/// Download the OPTIONAL parakeet live-ASR engine's four int8 models (~600 MB) from the csukuangfj
/// sherpa-onnx HF mirror into `<models_dir>/parakeet-tdt-0.6b-v3-int8` if missing (atomic per-file
/// `.part` → rename; a file already present is skipped). Emits the SAME
/// [`crate::events::EVENT_MODEL_DOWNLOAD`] progress (throttled) as the whisper download so the FE
/// reuses one progress bar. No-op when all four are already present.
#[tauri::command]
pub async fn download_parakeet_models(app: AppHandle) -> Result<(), AppError> {
    // Throttle progress events to roughly every 8 MB (parity with `download_model`).
    const EMIT_EVERY: u64 = 8 * 1024 * 1024;
    let mut last_emit: u64 = 0;
    crate::transcribe::model::ensure_parakeet_models(|downloaded, total| {
        if downloaded - last_emit >= EMIT_EVERY {
            last_emit = downloaded;
            let _ = app.emit(
                crate::events::EVENT_MODEL_DOWNLOAD,
                crate::events::ModelDownloadPayload {
                    downloaded,
                    total,
                    done: false,
                },
            );
        }
    })
    .await?;

    let _ = app.emit(
        crate::events::EVENT_MODEL_DOWNLOAD,
        crate::events::ModelDownloadPayload {
            downloaded: 0,
            total: None,
            done: true,
        },
    );
    Ok(())
}

/// Download the on-device brain model identified by `model_id` (from the curated registry) into the
/// shared models dir if missing; returns its path. No-op (returns the existing path) when already
/// present. Unknown id ⇒ `AppError::InvalidArg`. INBOUND ONLY — fetches a model file and sends NO
/// meeting content (no egress). Emits [`crate::events::EVENT_BRAIN_DOWNLOAD`] progress events
/// (throttled). The downloaded file is NOT loaded here — wiring the brain is a later step.
/// (The registry-lookup core `brain_download_target` lives in `commands/model_perf.rs`.)
#[tauri::command]
pub async fn download_brain_model(app: AppHandle, model_id: String) -> Result<String, AppError> {
    let (url, dest) = brain_download_target(&model_id)?;

    if dest.is_file() {
        return Ok(dest.to_string_lossy().to_string());
    }

    // The pinned integrity hash for this model (None until the registry entry is pinned — the
    // downloader then verifies before promoting the file, or warns + skips when None).
    let expected_sha = crate::reason::brain_model_by_id(&model_id).and_then(|m| m.sha256);

    // Throttle progress events to roughly every 8 MB so a multi-GB download doesn't flood the FE.
    const EMIT_EVERY: u64 = 8 * 1024 * 1024;
    let mut last_emit: u64 = 0;
    crate::reason::download_brain_model(url, &dest, expected_sha, |downloaded, total| {
        if downloaded - last_emit >= EMIT_EVERY {
            last_emit = downloaded;
            let _ = app.emit(
                crate::events::EVENT_BRAIN_DOWNLOAD,
                crate::events::BrainDownloadPayload {
                    downloaded,
                    total,
                    done: false,
                },
            );
        }
    })
    .await?;

    let _ = app.emit(
        crate::events::EVENT_BRAIN_DOWNLOAD,
        crate::events::BrainDownloadPayload {
            downloaded: 0,
            total: None,
            done: true,
        },
    );
    Ok(dest.to_string_lossy().to_string())
}

/// Download the multilingual-e5-small model (3 HF files) into the shared models dir, INBOUND-ONLY,
/// emitting [`crate::events::EVENT_EMBED_DOWNLOAD`] progress (throttled per file). Sends NO meeting
/// content (no egress). The downloaded model is NOT loaded here — it is picked up lazily by
/// `embed::active_embedder` on the next embed (selected on model presence). Returns the model dir.
#[tauri::command]
pub async fn download_embed_model(app: AppHandle) -> Result<String, AppError> {
    let file_count = crate::embed::EMBED_MODEL_FILES.len();
    // Throttle progress to roughly every 2 MB so the (small) model download doesn't flood the FE.
    const EMIT_EVERY: u64 = 2 * 1024 * 1024;
    let mut last_emit: u64 = 0;
    let mut last_index: usize = usize::MAX;
    let dir = crate::embed::download_embed_model(|file_index, downloaded, total| {
        // Always emit on a file boundary; otherwise throttle by bytes.
        if file_index != last_index || downloaded - last_emit >= EMIT_EVERY {
            last_index = file_index;
            last_emit = downloaded;
            let _ = app.emit(
                crate::events::EVENT_EMBED_DOWNLOAD,
                crate::events::EmbedDownloadPayload {
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
        crate::events::EVENT_EMBED_DOWNLOAD,
        crate::events::EmbedDownloadPayload {
            file_index: file_count,
            file_count,
            downloaded: 0,
            total: None,
            done: true,
        },
    );
    Ok(dir.to_string_lossy().to_string())
}

/// brain2 RAG — BACKFILL the semantic vector index for ALL VISIBLE meetings (the one-shot the user
/// runs after turning `semantic_search_enabled` on, or after installing the e5 model so the old
/// STUB-embedded chunks get replaced by real e5 vectors).
///
/// GATING (lock-model): the corpus is exactly `list_meetings_visible(unlocked)` — a sealed-and-not-
/// session-unlocked meeting is NEVER returned, so its plaintext is never chunked/embedded, and its
/// chunks STAY purged (the seal already purged them; we don't touch them). For each visible meeting
/// we re-fetch its note through `get_note_if_visible(unlocked)` (defense-in-depth: skip if the note
/// is not visible) and call `index_meeting_chunks`, which PURGES-then-reinserts — so any stale stub
/// vectors are replaced with e5 ones. No new read path: every read routes through `visibility_clause`.
///
/// MODEL GUARD: if the real e5 model is absent (`!embed_model_present()` ⇒ `active_embedder` is the
/// stub), MEETING indexing does nothing and the result is `{ status: "model_missing" }` — re-indexing
/// with garbage stub vectors is strictly worse than leaving the (old, possibly-stub) chunks alone;
/// the FE prompts the user to download e5 first via `download_embed_model`. DOCUMENT chunk/FTS
/// backfill still runs (chunk-only, zero vectors) so keyword retrieval over documents works
/// regardless of the model.
///
/// Emits [`crate::events::EVENT_REINDEX`] `{ done, total }` progress (counts only, NO PII).
/// EMBED_DIM stays 384 (e5 == stub width) ⇒ NO `vec_chunks` schema migration.
#[tauri::command]
pub async fn reindex_embeddings(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<ReindexResult, AppError> {
    // Snapshot the LIVE session unlock set — visibility is evaluated against exactly this set, so a
    // sealed-not-unlocked folder's meetings are invisible and never indexed.
    let unlocked = state
        .unlocked_folders
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default();

    // 2026-07-13 perf audit (MODERATE): this ran `reindex_embeddings_inner` — a vault-wide loop
    // (up to 100_000 meetings + every visible document) doing real Candle/Metal embed calls —
    // INLINE on the async command's Tokio worker, the same shape as the original startup-freeze
    // bug this whole investigation started from (now fixed there via a RAM floor + per-run cap;
    // the per-meeting embed BATCH SIZE is already safe here too, since index_meeting_chunks/
    // index_meeting_topic_chunks were sub-batched earlier today). This one is user-TRIGGERED (the
    // Settings "Reindex" button), not automatic, so the risk is lower, but the fix is the same
    // shape: move off the async runtime + gate on a RAM floor before starting.
    if !crate::transcribe::model::topic_backfill_ram_permits_now() {
        return Err(AppError::Unavailable(
            "not enough free memory to reindex right now — close some apps and try again".into(),
        ));
    }
    let db = state.db.clone();
    let heavy_inference = state.heavy_inference.clone();
    crate::perf::run_heavy(&heavy_inference, move || {
        let model_present = crate::embed::embed_model_present();
        // The absent-model branch only performs model-independent document chunk/FTS work. The
        // present branch persists vectors, so it must pin one REAL model and fail rather than ever
        // falling back to StubEmbedder between reindex sub-batches.
        let embedder: Box<dyn crate::embed::Embedder> = if model_present {
            crate::embed::active_persistence_embedder()?
        } else {
            Box::new(crate::embed::StubEmbedder)
        };
        reindex_embeddings_inner(
            &db,
            &unlocked,
            model_present,
            embedder.as_ref(),
            |done, total| {
                let _ = app.emit(
                    crate::events::EVENT_REINDEX,
                    crate::events::ReindexPayload { done, total },
                );
            },
        )
    })
    .await
}
