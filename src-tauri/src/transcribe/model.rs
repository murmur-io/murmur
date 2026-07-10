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

/// QUANT-SUFFIXED model sizes accepted by [`model_filename`] (T2 quant plumbing — NO default
/// flip: `AppConfig::default().model_size` stays `"small"`). Each maps `"<size>-<quant>"` →
/// `ggml-<size>-<quant>.bin`, verified BY URL SHAPE against the ggerganov/whisper.cpp HF
/// mirror's file tree (which hosts `ggml-small-q8_0.bin`, `ggml-medium-q8_0.bin`,
/// `ggml-large-v3-turbo.bin`, `ggml-large-v3-turbo-q8_0.bin`, `ggml-large-v3-q5_0.bin`).
/// The mirror ALSO hosts `.en` quant builds (`ggml-small.en-q8_0.bin`, …) but we deliberately
/// resolve quant selections to the MULTILINGUAL build only — the `.en` shortcut applies to the
/// four plain small sizes exactly as before, never to a quant/large variant (the conservative
/// URL-shape-only contract; the `.en` quant rows can be added later once someone actually
/// wants them, with their own tests).
pub const QUANT_MODEL_SIZES: &[&str] = &[
    "small-q8_0",
    "medium-q8_0",
    "large-v3-turbo-q8_0",
    "large-v3-q5_0",
];

/// Map a chosen size + language to a whisper.cpp GGML model filename.
///
/// Supported sizes (all served by the ggerganov/whisper.cpp HF mirror):
/// `tiny`, `base`, `small`, `medium`, `large-v3-turbo`, `large-v3`, plus the quant-suffixed
/// variants in [`QUANT_MODEL_SIZES`] (`small-q8_0`, `medium-q8_0`, `large-v3-turbo-q8_0`,
/// `large-v3-q5_0`) — `"<size>-<quant>"` maps to `ggml-<size>-<quant>.bin`.
///
/// English-only (`.en`) builds exist for PLAIN tiny/base/small/medium — smaller + faster — and
/// are used ONLY when the user explicitly selects English. Any other language (incl.
/// Polish) or auto-detect needs the multilingual build. `large-v3` / `large-v3-turbo` and
/// EVERY quant-suffixed size resolve multilingual-only (no `.en` is ever appended to a
/// quant/large variant — see [`QUANT_MODEL_SIZES`]).
///
/// An empty size falls back to the app default (`small`), matching
/// `AppConfig::default().model_size` — a RAM-safe default so a config that bypasses onboarding
/// no longer lands on the ~3 GB `large-v3`. All sizes (incl. `large-v3`) stay selectable.
pub fn model_filename(size: &str, language: &str) -> String {
    let size = match size.trim() {
        "" => "small",
        s => s,
    };
    // The `.en` shortcut applies ONLY to the four plain small sizes (exact match — a
    // quant-suffixed size like `small-q8_0` deliberately does NOT match, so quants always
    // resolve the multilingual `ggml-<size>-<quant>.bin`).
    let en_only = language == "en" && matches!(size, "tiny" | "base" | "small" | "medium");
    if en_only {
        format!("ggml-{size}.en.bin")
    } else {
        format!("ggml-{size}.bin")
    }
}

/// T1.3 — the LIVE-tick model-pin decision (pure; the file-presence resolution stays with the
/// caller). Returns the SIZE the live loop should pin to, or `None` = use the configured model:
///
/// 1. A non-empty `live_model_pin` (config; serde-default `"small"`) pins UNCONDITIONALLY —
///    live captions are throwaway (the authoritative transcript is the post-Stop Accurate
///    pass) and a `large-v3` live tick alone can saturate the shared Metal GPU for the whole
///    meeting (the heat complaint).
/// 2. An EMPTY pin disables it → today's pre-pin behavior, which still includes the legacy
///    `brain_live` pin-to-small (D1, spec §4.3: the live tick must not starve the light
///    reasoner) — so turning the new pin off never regresses the Brain-Live guarantee.
pub fn live_pin_size(live_model_pin: &str, brain_live: bool) -> Option<String> {
    let pin = live_model_pin.trim();
    if !pin.is_empty() {
        return Some(pin.to_string());
    }
    if brain_live {
        return Some("small".to_string());
    }
    None
}

/// T1.3 — resolve the SMALLEST downloaded whisper model for the WAKE listener
/// (`audio/listener.rs`): tiny → base → small, first present in [`models_dir`], respecting the
/// language-appropriate build ([`model_filename`]). NEVER medium/large — wake-phrase matching
/// needs rough text only, and a large standby decode every ~2.2 s is a heat/RAM source.
/// `None` = no suitable model downloaded (the caller does not start the listener).
pub fn smallest_wake_model(language: &str) -> Option<PathBuf> {
    let dir = models_dir().ok()?;
    smallest_wake_model_in(&dir, language)
}

/// File-presence core of [`smallest_wake_model`], factored over an explicit `dir` so the
/// tiny→base→small preference order is testable headless with a temp dir.
pub fn smallest_wake_model_in(dir: &Path, language: &str) -> Option<PathBuf> {
    for size in ["tiny", "base", "small"] {
        let preferred = dir.join(model_filename(size, language));
        if preferred.is_file() {
            return Some(preferred);
        }
        // An English selection can also ride a downloaded MULTILINGUAL build of the same size
        // (multilingual handles English fine; the reverse is not true, so a non-English
        // language never falls back onto an `.en` build).
        if language == "en" {
            let multilingual = dir.join(model_filename(size, ""));
            if multilingual.is_file() {
                return Some(multilingual);
            }
        }
    }
    None
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
        return Err(AppError::Transcribe(
            "model download returned empty body".into(),
        ));
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

    /// T2 — every supported quant-suffixed size maps `"<size>-<quant>"` →
    /// `ggml-<size>-<quant>.bin`, for EVERY language (`.en` never applies to a quant/large
    /// variant — URL-shape contract against the HF mirror).
    #[test]
    fn quant_sizes_map_to_quant_filenames_never_en() {
        for size in QUANT_MODEL_SIZES {
            let expected = format!("ggml-{size}.bin");
            for lang in ["", "en", "pl"] {
                assert_eq!(model_filename(size, lang), expected, "size={size} lang={lang}");
            }
        }
        // The concrete rows, spelled out (the mirror's actual file names):
        assert_eq!(model_filename("small-q8_0", "pl"), "ggml-small-q8_0.bin");
        assert_eq!(model_filename("medium-q8_0", "en"), "ggml-medium-q8_0.bin");
        assert_eq!(
            model_filename("large-v3-turbo-q8_0", ""),
            "ggml-large-v3-turbo-q8_0.bin"
        );
        assert_eq!(model_filename("large-v3-q5_0", "en"), "ggml-large-v3-q5_0.bin");
    }

    /// T2 — quant filenames ride the same HF mirror URL as the plain sizes.
    #[test]
    fn quant_url_points_at_whispercpp_hf_mirror() {
        assert_eq!(
            model_url(&model_filename("large-v3-turbo-q8_0", "pl")),
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q8_0.bin"
        );
    }

    /// T1.3 — the live pin decision: a non-empty pin wins unconditionally; an empty pin falls
    /// back to the legacy `brain_live` pin-to-small; neither ⇒ the configured model.
    #[test]
    fn live_pin_size_resolution() {
        // Default config pin ("small") pins regardless of brain_live.
        assert_eq!(live_pin_size("small", false).as_deref(), Some("small"));
        assert_eq!(live_pin_size("small", true).as_deref(), Some("small"));
        // Any explicit size (incl. a quant) pins.
        assert_eq!(live_pin_size("base", false).as_deref(), Some("base"));
        assert_eq!(
            live_pin_size("small-q8_0", false).as_deref(),
            Some("small-q8_0")
        );
        // Whitespace counts as empty.
        assert_eq!(live_pin_size("  ", false), None);
        // Empty pin = today's behavior: configured model, EXCEPT the legacy brain_live pin.
        assert_eq!(live_pin_size("", false), None);
        assert_eq!(live_pin_size("", true).as_deref(), Some("small"));
    }

    /// T1.3 — the wake listener resolves the SMALLEST downloaded model (tiny → base → small),
    /// never medium/large, and honors the `.en` build for English.
    #[test]
    fn smallest_wake_model_prefers_tiny_and_never_medium_or_large() {
        let dir = std::env::temp_dir().join(format!("murmur-wake-model-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        // Nothing downloaded → None (the listener does not start).
        assert_eq!(smallest_wake_model_in(&dir, ""), None);

        // Only medium/large present → STILL None (never a big standby model).
        std::fs::write(dir.join("ggml-medium.bin"), b"x").unwrap();
        std::fs::write(dir.join("ggml-large-v3.bin"), b"x").unwrap();
        assert_eq!(smallest_wake_model_in(&dir, ""), None);

        // small appears → picked; base appears → preferred; tiny appears → wins.
        std::fs::write(dir.join("ggml-small.bin"), b"x").unwrap();
        assert_eq!(
            smallest_wake_model_in(&dir, "pl"),
            Some(dir.join("ggml-small.bin"))
        );
        std::fs::write(dir.join("ggml-base.bin"), b"x").unwrap();
        assert_eq!(
            smallest_wake_model_in(&dir, ""),
            Some(dir.join("ggml-base.bin"))
        );
        std::fs::write(dir.join("ggml-tiny.bin"), b"x").unwrap();
        assert_eq!(
            smallest_wake_model_in(&dir, ""),
            Some(dir.join("ggml-tiny.bin"))
        );

        // English PREFERS the `.en` build of a size but falls back to the multilingual build
        // of the SAME size (multilingual handles English; smallest-size still wins overall).
        assert_eq!(
            smallest_wake_model_in(&dir, "en"),
            Some(dir.join("ggml-tiny.bin")),
            "en falls back to the multilingual tiny when no .en build exists"
        );
        std::fs::write(dir.join("ggml-tiny.en.bin"), b"x").unwrap();
        assert_eq!(
            smallest_wake_model_in(&dir, "en"),
            Some(dir.join("ggml-tiny.en.bin"))
        );

        let _ = std::fs::remove_dir_all(&dir);
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
