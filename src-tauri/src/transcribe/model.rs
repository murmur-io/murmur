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

/// The whisper.cpp ggml Silero VAD model (~885 kB) used by the Accurate batch path's VAD
/// pre-segmentation. Served by the ggml-org/whisper-vad HF repo.
pub const VAD_MODEL_FILE: &str = "ggml-silero-v5.1.2.bin";

/// Diarization (#8): pyannote segmentation + CAM++ embedding ONNX models (sherpa-converted),
/// downloaded on demand to [`models_dir`]. ~12 MB + ~28 MB.
pub const DIARIZE_SEG_MODEL_FILE: &str = "sherpa-pyannote-segmentation-3.0.onnx";
pub const DIARIZE_EMB_MODEL_FILE: &str = "wespeaker_en_voxceleb_CAM++.onnx";

/// Map a chosen size + language to a whisper.cpp GGML model filename.
///
/// Supported sizes (all served by the ggerganov/whisper.cpp HF mirror):
/// `tiny`, `base`, `small`, `medium`, `large-v3-turbo`, `large-v3`.
///
/// English-only (`.en`) builds exist for tiny/base/small/medium — smaller + faster — and
/// are used ONLY when the user explicitly selects English. Any other language (incl.
/// Polish) or auto-detect needs the multilingual build. `large-v3` and `large-v3-turbo`
/// are multilingual-only (no `.en` variant), so Polish always resolves the full
/// multilingual `ggml-large-v3.bin`.
///
/// An empty size falls back to the app default (`small`), matching
/// `AppConfig::default().model_size` — a RAM-safe default so a config that bypasses onboarding
/// no longer lands on the ~3 GB `large-v3`. All sizes (incl. `large-v3`) stay selectable.
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

/// Hugging Face repo for whisper.cpp's ggml Silero VAD model (separate from the main
/// whisper models, which live in the ggerganov/whisper.cpp repo).
pub fn vad_model_url(filename: &str) -> String {
    format!("https://huggingface.co/ggml-org/whisper-vad/resolve/main/{filename}")
}

/// The directory where MeetNotes keeps downloaded models:
/// `<app-data>/MeetNotes/models`. Created if absent.
///
/// Uses `dirs::data_dir()` (e.g. `~/Library/Application Support` on macOS). The
/// application identifier mirrors `tauri.conf.json` (`com.meetnotes.app`); we use the
/// human-friendly `MeetNotes` folder name to match the rest of the app-data layout.
///
/// INTENTIONALLY SHARED across dev + release: unlike the DB + audio dirs (which split via
/// `crate::state::app_dir_name` into `MeetNotes-dev` for dev), the models dir hard-codes
/// `MeetNotes` so the ~3GB whisper model is downloaded ONCE and reused by every build. The model
/// is not sensitive and is keyless, so there is no cross-build collision. Do NOT route this
/// through `app_dir_name()`.
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
pub async fn ensure_model<F>(
    configured: Option<&Path>,
    size: &str,
    language: &str,
    on_progress: F,
) -> Result<PathBuf>
where
    F: FnMut(u64, Option<u64>),
{
    if let Some(found) = resolve_model_path(configured, size, language)? {
        return Ok(found);
    }

    let dir = models_dir()?;
    let file = model_filename(size, language);
    let dest = dir.join(&file);
    download_model_streaming(&model_url(&file), &dest, on_progress).await?;
    Ok(dest)
}

/// Ensure the Silero VAD model exists in [`models_dir`] and return its path (best-effort,
/// atomic download). Used by the Accurate batch path's VAD pre-segmentation; a failure is
/// non-fatal — the caller transcribes the whole buffer instead.
pub async fn ensure_vad_model() -> Result<PathBuf> {
    let dest = models_dir()?.join(VAD_MODEL_FILE);
    if dest.is_file() {
        return Ok(dest);
    }
    download_model(&vad_model_url(VAD_MODEL_FILE), &dest).await?;
    Ok(dest)
}

/// Ensure the diarization models exist in [`models_dir`]; returns `(segmentation, embedding)`
/// paths. Best-effort atomic downloads — a failure leaves the caller with the single "others"
/// label (diarization is opt-in + non-fatal).
pub async fn ensure_diarization_models() -> Result<(PathBuf, PathBuf)> {
    let dir = models_dir()?;
    let seg = dir.join(DIARIZE_SEG_MODEL_FILE);
    if !seg.is_file() {
        download_model(
            "https://huggingface.co/csukuangfj/sherpa-onnx-pyannote-segmentation-3-0/resolve/main/model.onnx",
            &seg,
        )
        .await?;
    }
    let emb = dir.join(DIARIZE_EMB_MODEL_FILE);
    if !emb.is_file() {
        download_model(
            "https://huggingface.co/csukuangfj/speaker-embedding-models/resolve/main/wespeaker_en_voxceleb_CAM++.onnx",
            &emb,
        )
        .await?;
    }
    Ok((seg, emb))
}

/// Download `url` to `dest` atomically (`dest.part` → rename), invoking `on_progress(downloaded,
/// total)` as bytes arrive (`total` is `None` when the server omits `Content-Length`). INBOUND
/// ONLY: fetches a model file and sends NO request body / NO meeting content (no egress). Streams
/// via `Response::chunk` (no extra stream-combinator dep) so a multi-GB whisper model reports live
/// progress instead of buffering the whole body in memory. Overwrites any stale partial. Verifies a
/// non-empty body before the rename. NO PII is logged (model id / byte counts only).
async fn download_model_streaming<F>(url: &str, dest: &Path, mut on_progress: F) -> Result<()>
where
    F: FnMut(u64, Option<u64>),
{
    use tokio::io::AsyncWriteExt;

    tracing::info!(target: "transcribe", file = %dest.display(), "downloading whisper model");

    let mut resp = reqwest::get(url)
        .await
        .map_err(|e| AppError::Transcribe(format!("model download request failed: {e}")))?;
    if !resp.status().is_success() {
        return Err(AppError::Transcribe(format!(
            "model download HTTP {}",
            resp.status()
        )));
    }
    let total = resp.content_length();

    let part = dest.with_extension("part");
    let mut file = tokio::fs::File::create(&part)
        .await
        .map_err(|e| AppError::Transcribe(format!("create model temp file: {e}")))?;
    let mut downloaded: u64 = 0;
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| AppError::Transcribe(format!("model download body failed: {e}")))?
    {
        file.write_all(&chunk)
            .await
            .map_err(|e| AppError::Transcribe(format!("write model chunk: {e}")))?;
        downloaded += chunk.len() as u64;
        on_progress(downloaded, total);
    }
    file.flush()
        .await
        .map_err(|e| AppError::Transcribe(format!("flush model file: {e}")))?;
    drop(file);

    if downloaded == 0 {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(AppError::Transcribe("model download returned empty body".into()));
    }
    tokio::fs::rename(&part, dest)
        .await
        .map_err(|e| AppError::Transcribe(format!("rename model file: {e}")))?;

    tracing::info!(
        target: "transcribe",
        file = %dest.display(),
        bytes = downloaded,
        "whisper model ready"
    );
    Ok(())
}

/// Non-progress download for the VAD + diarization models (small, no UI progress bar). Delegates to
/// [`download_model_streaming`] with a no-op callback so all model fetches share one atomic path.
async fn download_model(url: &str, dest: &Path) -> Result<()> {
    download_model_streaming(url, dest, |_, _| {}).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_resolves_en_only_build_for_small_sizes() {
        assert_eq!(model_filename("tiny", "en"), "ggml-tiny.en.bin");
        assert_eq!(model_filename("base", "en"), "ggml-base.en.bin");
        assert_eq!(model_filename("small", "en"), "ggml-small.en.bin");
        assert_eq!(model_filename("medium", "en"), "ggml-medium.en.bin");
    }

    #[test]
    fn large_v3_is_multilingual_only_even_for_english() {
        // large-v3 / large-v3-turbo have no `.en` build — never append `.en`.
        assert_eq!(model_filename("large-v3", "en"), "ggml-large-v3.bin");
        assert_eq!(
            model_filename("large-v3-turbo", "en"),
            "ggml-large-v3-turbo.bin"
        );
    }

    #[test]
    fn polish_always_resolves_multilingual_build() {
        // The global "Polish" option (language = "pl") must NEVER get an `.en` model.
        assert_eq!(model_filename("large-v3", "pl"), "ggml-large-v3.bin");
        assert_eq!(model_filename("medium", "pl"), "ggml-medium.bin");
        assert_eq!(model_filename("small", "pl"), "ggml-small.bin");
        assert_eq!(model_filename("tiny", "pl"), "ggml-tiny.bin");
    }

    #[test]
    fn autodetect_resolves_multilingual_build() {
        // Empty language (auto-detect) → multilingual for every size.
        assert_eq!(model_filename("medium", ""), "ggml-medium.bin");
        assert_eq!(model_filename("large-v3", ""), "ggml-large-v3.bin");
    }

    #[test]
    fn empty_size_falls_back_to_small_default() {
        // Mirrors AppConfig::default().model_size — a RAM-safe default (was large-v3, ~3 GB).
        // For English, the empty-size fallback resolves the multilingual build (no explicit "en").
        assert_eq!(model_filename("", ""), "ggml-small.bin");
        assert_eq!(model_filename("   ", "pl"), "ggml-small.bin");
        // large-v3 stays selectable when explicitly chosen.
        assert_eq!(model_filename("large-v3", ""), "ggml-large-v3.bin");
    }

    #[test]
    fn url_points_at_whispercpp_hf_mirror() {
        assert_eq!(
            model_url("ggml-large-v3.bin"),
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3.bin"
        );
    }

    #[test]
    fn vad_url_points_at_whisper_vad_repo() {
        assert_eq!(
            vad_model_url(VAD_MODEL_FILE),
            "https://huggingface.co/ggml-org/whisper-vad/resolve/main/ggml-silero-v5.1.2.bin"
        );
    }
}
