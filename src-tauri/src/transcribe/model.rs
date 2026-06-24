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

/// Map a chosen size + language to a whisper.cpp GGML model filename.
///
/// English-only (`.en`) builds exist for tiny/base/small/medium — smaller + faster — and
/// are used ONLY when the user explicitly selects English. Any other language (incl.
/// Polish) or auto-detect needs the multilingual build. `large-v3` is multilingual-only.
pub fn model_filename(size: &str, language: &str) -> String {
    let size = match size.trim() {
        "" => "small",
        s => s,
    };
    let en_only = language == "en" && matches!(size, "tiny" | "base" | "small" | "medium");
    if en_only {
        format!("ggml-{size}.en.bin")
    } else {
        format!("ggml-{size}.bin")
    }
}

/// Hugging Face mirror of the official whisper.cpp GGML models (ggerganov/whisper.cpp).
/// `resolve/main` serves the raw file; whisper-rs loads the GGML/GGUF binary directly.
pub fn model_url(filename: &str) -> String {
    format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{filename}")
}

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
/// 2. The model derived from the chosen `size` + `language` inside [`models_dir`], if it
///    already exists on disk.
///
/// Returns `Ok(None)` when no usable model is present (caller may then call
/// [`ensure_model`] to download one, or surface a "set model path" hint to the user).
pub fn resolve_model_path(
    configured: Option<&Path>,
    size: &str,
    language: &str,
) -> Result<Option<PathBuf>> {
    if let Some(p) = configured {
        if p.is_file() {
            return Ok(Some(p.to_path_buf()));
        }
    }
    let derived = models_dir()?.join(model_filename(size, language));
    if derived.is_file() {
        return Ok(Some(derived));
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
pub async fn ensure_model(
    configured: Option<&Path>,
    size: &str,
    language: &str,
) -> Result<PathBuf> {
    if let Some(found) = resolve_model_path(configured, size, language)? {
        return Ok(found);
    }

    let dir = models_dir()?;
    let file = model_filename(size, language);
    let dest = dir.join(&file);
    download_model(&model_url(&file), &dest).await?;
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
