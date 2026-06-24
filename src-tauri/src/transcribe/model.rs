//! GGUF Whisper model location + download.
//!
//! Phase 0 places the model manually (the path is a setting — see PHASE0-PLAN §0).
//! These helpers give the rest of the app a single place to (a) resolve the app-data
//! model directory, (b) check whether a usable model already exists there, and
//! (c) best-effort download a default model when none is configured.
//!
//! The binding `Transcriber::load(&Path)` signature (PHASE0-PLAN §5.7) is unchanged;
//! callers resolve a concrete path with [`ensure_model`] / [`resolve_model_path`] and
//! then hand it to `Transcriber::load`.

use std::path::{Path, PathBuf};

use crate::error::{AppError, Result};

/// App-data subdirectory that holds downloaded GGUF models.
pub const MODELS_SUBDIR: &str = "models";

/// Default model filename downloaded when nothing is configured.
///
/// `base.en` is a good Phase-0 default: ~142 MB, English-only, fast on Apple Silicon
/// with the Metal backend, and accurate enough for a walking-skeleton proof.
pub const DEFAULT_MODEL_FILE: &str = "ggml-base.en.bin";

/// Hugging Face mirror of the official whisper.cpp GGML models (ggerganov/whisper.cpp).
/// `resolve/main` serves the raw file. whisper-rs loads the GGML/GGUF binary directly.
const DEFAULT_MODEL_URL: &str =
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin";

/// The directory where MeetNotes keeps downloaded models:
/// `<app-data>/MeetNotes/models`. Created if absent.
///
/// Uses `dirs::data_dir()` (e.g. `~/Library/Application Support` on macOS). The
/// application identifier mirrors `tauri.conf.json` (`com.meetnotes.app`); we use the
/// human-friendly `MeetNotes` folder name to match the rest of the app-data layout.
pub fn models_dir() -> Result<PathBuf> {
    let base = dirs::data_dir()
        .ok_or_else(|| AppError::Transcribe("could not resolve app-data directory".into()))?;
    let dir = base.join("MeetNotes").join(MODELS_SUBDIR);
    std::fs::create_dir_all(&dir)
        .map_err(|e| AppError::Transcribe(format!("create models dir: {e}")))?;
    Ok(dir)
}

/// Resolve the model path the transcriber should load.
///
/// Resolution order:
/// 1. `configured` — an explicit path from settings (`whisper_model_path`). If it points
///    at an existing file it is used verbatim.
/// 2. The default model file inside [`models_dir`], if it already exists on disk.
///
/// Returns `Ok(None)` when no usable model is present (caller may then call
/// [`ensure_model`] to download one, or surface a "set model path" hint to the user).
pub fn resolve_model_path(configured: Option<&Path>) -> Result<Option<PathBuf>> {
    if let Some(p) = configured {
        if p.is_file() {
            return Ok(Some(p.to_path_buf()));
        }
    }
    let default = models_dir()?.join(DEFAULT_MODEL_FILE);
    if default.is_file() {
        return Ok(Some(default));
    }
    Ok(None)
}

/// Ensure a usable GGUF model exists on disk and return its path.
///
/// If `configured` already points at a real file, it is returned untouched. Otherwise,
/// if the default model is missing from [`models_dir`], it is downloaded once
/// (best-effort, atomic: download to `<name>.part` then rename). Subsequent calls are
/// no-ops once the file is present.
///
/// This is `async` because the download uses `reqwest`; it must be called from within a
/// Tokio runtime (the pipeline already runs on one).
pub async fn ensure_model(configured: Option<&Path>) -> Result<PathBuf> {
    if let Some(found) = resolve_model_path(configured)? {
        return Ok(found);
    }

    let dir = models_dir()?;
    let dest = dir.join(DEFAULT_MODEL_FILE);
    download_model(DEFAULT_MODEL_URL, &dest).await?;
    Ok(dest)
}

/// Download `url` to `dest` atomically (`dest.part` → rename). Overwrites any stale
/// partial. Verifies a non-empty body. NO PII is logged (model id / sizes only).
async fn download_model(url: &str, dest: &Path) -> Result<()> {
    tracing::info!(target: "transcribe", file = %dest.display(), "downloading whisper model");

    let resp = reqwest::get(url)
        .await
        .map_err(|e| AppError::Transcribe(format!("model download request failed: {e}")))?;
    if !resp.status().is_success() {
        return Err(AppError::Transcribe(format!(
            "model download HTTP {}",
            resp.status()
        )));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| AppError::Transcribe(format!("model download body failed: {e}")))?;
    if bytes.is_empty() {
        return Err(AppError::Transcribe("model download returned empty body".into()));
    }

    let part = dest.with_extension("part");
    tokio::fs::write(&part, &bytes)
        .await
        .map_err(|e| AppError::Transcribe(format!("write model temp file: {e}")))?;
    tokio::fs::rename(&part, dest)
        .await
        .map_err(|e| AppError::Transcribe(format!("rename model file: {e}")))?;

    tracing::info!(
        target: "transcribe",
        file = %dest.display(),
        bytes = bytes.len(),
        "whisper model ready"
    );
    Ok(())
}
