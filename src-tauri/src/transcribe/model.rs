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

/// OPTIONAL parakeet live-ASR engine (NVIDIA parakeet-tdt-0.6b-v3 int8, sherpa-onnx nemo
/// transducer, CPU-only). The four model files live under `<models_dir>/<PARAKEET_SUBDIR>/`,
/// downloaded on demand from the csukuangfj sherpa-onnx HF mirror. ~600 MB total (encoder +
/// decoder are the bulk; the joiner + tokens are tiny). See `transcribe::parakeet`.
pub const PARAKEET_SUBDIR: &str = "parakeet-tdt-0.6b-v3-int8";
pub const PARAKEET_ENCODER: &str = "encoder.int8.onnx";
pub const PARAKEET_DECODER: &str = "decoder.int8.onnx";
pub const PARAKEET_JOINER: &str = "joiner.int8.onnx";
pub const PARAKEET_TOKENS: &str = "tokens.txt";

/// The four parakeet model files, in a stable order for download aggregation + presence checks.
const PARAKEET_FILES: &[&str] = &[
    PARAKEET_ENCODER,
    PARAKEET_DECODER,
    PARAKEET_JOINER,
    PARAKEET_TOKENS,
];

/// RAM floor for loading the parakeet CPU recognizer alongside whisper + the brain: the int8
/// transducer is ~600 MB resident, so refuse it on affirmatively-below-8-GB machines (parity with
/// the reasoner's whisper-large refuse). A BROKEN RAM probe (`None`) fails OPEN — never refuse
/// captions on a measurement we couldn't take.
const PARAKEET_MIN_RAM_BYTES: u64 = 8 * 1024 * 1024 * 1024;
/// Floor for the startup topic-chunk backfill — much lighter than parakeet (a small e5 embed
/// pass, not real-time streaming ASR), so the floor is lower too.
const TOPIC_BACKFILL_MIN_RAM_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// QUANT-SUFFIXED model sizes accepted by [`model_filename`] (T2 quant plumbing; the CONDITIONAL
/// default flip lives in [`default_model_size`]). Each maps `"<size>-<quant>"` →
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

/// T2 DEFAULT FLIP (the SAFE shape) — the size a NO-CHOICE config defaults to when the machine
/// qualifies. Measured evidence (docs/research/2026-07-09-transcription-performance.md,
/// "Measured results"): the turbo q8_0 quant runs an Accurate batch in the same wall-clock as
/// `small` with near-perfect Polish, at ~875 MB download. Only [`default_model_size`] decides
/// WHO gets it — existing installs and low-RAM machines stay on `small`.
pub const TURBO_DEFAULT_SIZE: &str = "large-v3-turbo-q8_0";

/// On-disk filename of [`TURBO_DEFAULT_SIZE`]'s (multilingual-only) build — must equal
/// `model_filename(TURBO_DEFAULT_SIZE, _)` (test: `turbo_default_file_matches_model_filename`).
const TURBO_DEFAULT_FILE: &str = "ggml-large-v3-turbo-q8_0.bin";

/// RAM floor for defaulting a FRESH install to the turbo quant: the ~1 GB-resident turbo is a
/// safe default on ≥ 12 GB machines; 8 GB Macs stay on `small` (the A2 RAM-safety rationale).
const TURBO_DEFAULT_MIN_RAM_BYTES: u64 = 12 * 1024 * 1024 * 1024;

/// T2 DEFAULT FLIP — the ONE place that decides what an EMPTY `model_size` means. PURE over the
/// models-dir file names + total RAM so every branch is headless-testable:
///
/// 1. `ggml-large-v3-turbo-q8_0.bin` already downloaded → `large-v3-turbo-q8_0` (the user
///    already paid the download; same wall-clock as `small`, much better Polish).
/// 2. NO whisper `ggml-*.bin` at all (fresh install — onboarding will download whatever we
///    return) AND total RAM ≥ 12 GB → `large-v3-turbo-q8_0`.
/// 3. Otherwise → `small`. An EXISTING install (any whisper model on disk, e.g. `small`) NEVER
///    gets a surprise 874 MB download or a behavior change, and unknown RAM (`None`) never
///    triggers one either (fail-SMALL, not fail-open — the opposite of the reasoner's RAM guard,
///    because "open" here would mean a large unrequested download).
///
/// The VAD model (`ggml-silero-v5.1.2.bin`) is a `ggml-*.bin` but NOT a whisper model — its
/// presence alone still counts as a fresh install. `.part` partials never count (no `.bin`
/// suffix). The LIVE pin (`live_model_pin`, default `small`) is deliberately untouched — turbo
/// is a BATCH default only, ENFORCED at the record-start pin resolution: when the pinned file
/// is absent the live tick falls back to [`live_fallback_model`] (live-safe sizes only) and
/// NEVER to a medium/large configured model ([`is_live_heavy_model_file`]).
pub fn default_model_size<S: AsRef<str>>(
    models_dir_files: &[S],
    total_ram_bytes: Option<u64>,
) -> &'static str {
    if models_dir_files
        .iter()
        .any(|f| f.as_ref() == TURBO_DEFAULT_FILE)
    {
        return TURBO_DEFAULT_SIZE;
    }
    let any_whisper_model = models_dir_files.iter().any(|f| {
        let name = f.as_ref();
        name.starts_with("ggml-") && name.ends_with(".bin") && name != VAD_MODEL_FILE
    });
    if !any_whisper_model && total_ram_bytes.is_some_and(|b| b >= TURBO_DEFAULT_MIN_RAM_BYTES) {
        return TURBO_DEFAULT_SIZE;
    }
    "small"
}

/// [`default_model_size`] over the REAL machine: lists [`models_dir`] + reads total RAM via
/// `sysctl -n hw.memsize` (the same no-new-dep pattern as `commands.rs::total_ram_gb`). Any
/// probe/listing failure resolves `small` — a broken probe must never trigger a large download.
/// Called by `AppConfig::default()` (the onboarding preselect) and by [`effective_model_size`].
pub fn default_model_size_now() -> &'static str {
    let Ok(dir) = models_dir() else {
        return "small";
    };
    let Ok(read) = std::fs::read_dir(&dir) else {
        return "small";
    };
    let files: Vec<String> = read
        .filter_map(|e| e.ok().and_then(|e| e.file_name().into_string().ok()))
        .collect();
    default_model_size(&files, total_ram_bytes())
}

/// Resolve a possibly-empty configured `model_size` to the size that should actually load:
/// non-empty passes through verbatim; empty/blank resolves the machine-conditional default
/// ([`default_model_size_now`]). [`resolve_model_path`] / [`ensure_model`] route through this so
/// a legacy config that persisted `""` follows the same ONE decision as a fresh default.
pub fn effective_model_size(size: &str) -> String {
    let s = size.trim();
    if s.is_empty() {
        default_model_size_now().to_string()
    } else {
        s.to_string()
    }
}

/// macOS total physical RAM in bytes via `sysctl -n hw.memsize` — mirrors
/// `commands.rs::total_ram_gb` (no new FFI/crate). `None` on any failure.
fn total_ram_bytes() -> Option<u64> {
    let out = std::process::Command::new("sysctl")
        .arg("-n")
        .arg("hw.memsize")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()?.trim().parse().ok()
}

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
/// An empty size falls back to `small` here as a STATIC safety net only — the real
/// machine-conditional default (T2 flip: turbo when already downloaded / fresh big-RAM install)
/// is applied BEFORE this function by [`effective_model_size`] inside [`resolve_model_path`] /
/// [`ensure_model`], and by `AppConfig::default()` via [`default_model_size_now`]. All sizes
/// (incl. `large-v3`) stay selectable.
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
    first_present_model_in(dir, &["tiny", "base", "small"], language)
}

/// T2 default-flip follow-up — the LIVE-pin ABSENT-FILE fallback: the pinned live model
/// (default `small`) is not on disk, so pick the LARGEST downloaded live-SAFE model instead
/// (small → small-q8_0 → base → tiny; captions want the best of the safe sizes). NEVER
/// medium/large — falling through to a large-class configured model is exactly the T1.3
/// "large live tick saturates the shared Metal GPU" heat scenario the pin exists to prevent,
/// and on a fresh turbo-default install (ONLY `ggml-large-v3-turbo-q8_0.bin` downloaded) it
/// would be the DEFAULT experience. `None` = nothing live-safe downloaded (the caller skips
/// captions and may background-download the pinned size).
pub fn live_fallback_model(language: &str) -> Option<PathBuf> {
    let dir = models_dir().ok()?;
    live_fallback_model_in(&dir, language)
}

/// File-presence core of [`live_fallback_model`], testable headless with a temp dir.
pub fn live_fallback_model_in(dir: &Path, language: &str) -> Option<PathBuf> {
    first_present_model_in(dir, &["small", "small-q8_0", "base", "tiny"], language)
}

/// First size in `sizes` whose model file exists in `dir`, honoring the language-appropriate
/// build ([`model_filename`]). An English selection can also ride a downloaded MULTILINGUAL
/// build of the same size (multilingual handles English fine; the reverse is not true, so a
/// non-English language never falls back onto an `.en` build).
fn first_present_model_in(dir: &Path, sizes: &[&str], language: &str) -> Option<PathBuf> {
    for size in sizes {
        let preferred = dir.join(model_filename(size, language));
        if preferred.is_file() {
            return Some(preferred);
        }
        if language == "en" {
            let multilingual = dir.join(model_filename(size, ""));
            if multilingual.is_file() {
                return Some(multilingual);
            }
        }
    }
    None
}

/// Whether a whisper model FILE is too heavy for the 3 s LIVE tick (medium/large class, incl.
/// every large-v3 / turbo / quant variant — the name-based check covers `ggml-large-v3.bin`,
/// `ggml-large-v3-turbo-q8_0.bin`, `ggml-medium.en.bin`, …). Best-effort by design: an
/// unrecognizable custom filename classifies NOT-heavy, which preserves the pre-pin behavior
/// for explicit `whisper_model_path` users.
pub fn is_live_heavy_model_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| {
            let n = n.to_ascii_lowercase();
            n.contains("large") || n.contains("medium")
        })
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

/// The directory holding the OPTIONAL parakeet live-ASR models:
/// `<models_dir>/parakeet-tdt-0.6b-v3-int8`. Created if absent (mirrors [`models_dir`]).
pub fn parakeet_dir() -> Result<PathBuf> {
    let dir = models_dir()?.join(PARAKEET_SUBDIR);
    std::fs::create_dir_all(&dir)
        .map_err(|e| AppError::Transcribe(format!("create parakeet dir: {e}")))?;
    Ok(dir)
}

/// The four resolved parakeet model file paths under [`parakeet_dir`] (encoder/decoder/joiner
/// int8 ONNX + tokens). Errors only if the models dir can't be resolved/created; whether the
/// files actually EXIST is [`ParakeetModelPaths::all_present`] / [`parakeet_models_present`].
pub fn parakeet_model_paths() -> Result<crate::transcribe::live_asr::ParakeetModelPaths> {
    let dir = parakeet_dir()?;
    Ok(crate::transcribe::live_asr::ParakeetModelPaths {
        encoder: dir.join(PARAKEET_ENCODER),
        decoder: dir.join(PARAKEET_DECODER),
        joiner: dir.join(PARAKEET_JOINER),
        tokens: dir.join(PARAKEET_TOKENS),
    })
}

/// Whether ALL FOUR parakeet model files exist under [`parakeet_dir`]. GRACEFUL: any error
/// resolving the dir ⇒ `false` (mirrors `embed_model_present` — a probe failure means "not
/// present", never a panic). Consumed by the `parakeet_models_present` command + `build_live_asr`.
pub fn parakeet_models_present() -> bool {
    let Ok(dir) = parakeet_dir() else {
        return false;
    };
    PARAKEET_FILES.iter().all(|f| dir.join(f).is_file())
}

/// HF mirror URL for a parakeet model file (csukuangfj's sherpa-onnx nemo parakeet release). All
/// four files (`encoder.int8.onnx` / `decoder.int8.onnx` / `joiner.int8.onnx` / `tokens.txt`) are
/// verified HTTP-200 against `resolve/main`. INBOUND ONLY (no request body / no egress).
pub fn parakeet_model_url(filename: &str) -> String {
    format!(
        "https://huggingface.co/csukuangfj/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8/resolve/main/{filename}"
    )
}

/// RAM guard for loading the parakeet CPU recognizer — `false` ONLY when total RAM is
/// affirmatively below [`PARAKEET_MIN_RAM_BYTES`]. Fails OPEN (returns `true`) when the probe
/// can't read RAM, so a broken measurement never silently disables captions. Mirrors the pattern
/// used elsewhere for RAM-sensitive model loads.
pub fn parakeet_ram_permits_now() -> bool {
    match total_ram_bytes() {
        Some(b) => b >= PARAKEET_MIN_RAM_BYTES,
        None => true,
    }
}

/// RAM guard for any bulk Candle/Metal embed pass over the vault — the startup topic-chunk
/// backfill (`Db::backfill_topic_chunks_idempotent`) and the user-triggered "Reindex" command
/// (`commands::reindex_embeddings`) both gate on this before starting. `false` ONLY when total
/// RAM is affirmatively below [`TOPIC_BACKFILL_MIN_RAM_BYTES`]. Fails OPEN when the probe can't
/// read RAM (a broken measurement never silently disables catch-up indexing). A genuinely
/// RAM-starved machine defers the whole pass rather than starting a Metal/Candle embed burst —
/// both callers are safe to retry later (the backfill is content-hash idempotent; reindex is a
/// user-initiated retry). Mirrors [`parakeet_ram_permits_now`].
pub fn topic_backfill_ram_permits_now() -> bool {
    match total_ram_bytes() {
        Some(b) => b >= TOPIC_BACKFILL_MIN_RAM_BYTES,
        None => true,
    }
}

/// Ensure all four parakeet model files exist under [`parakeet_dir`], downloading any missing one
/// (atomic `.part` → rename, via [`download_model_streaming`]) from the csukuangfj HF mirror.
/// `on_progress(downloaded, total)` reports the AGGREGATE byte progress across the four files
/// (per-file byte sum; `total` is `None` once any file's server omits `Content-Length`). A file
/// already present is skipped (its bytes are counted toward the running total so the bar doesn't
/// jump). Best-effort + atomic; a failure leaves the caller on whisper captions (the seam falls
/// back). No PII logged (file id / byte counts only).
pub async fn ensure_parakeet_models<F>(mut on_progress: F) -> Result<()>
where
    F: FnMut(u64, Option<u64>),
{
    let dir = parakeet_dir()?;
    // Aggregate progress across the four files: carry the bytes of already-finished files forward
    // so each file's per-file `downloaded` is offset onto the running total.
    let mut base: u64 = 0;
    for file in PARAKEET_FILES {
        let dest = dir.join(file);
        if dest.is_file() {
            // Count an existing file's size toward the running total so a resumed/partial set
            // reports monotonically increasing progress.
            base = base.saturating_add(std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0));
            continue;
        }
        download_model_streaming(&parakeet_model_url(file), &dest, |d, _t| {
            // The aggregate total is unknown across four files (mixed Content-Length availability),
            // so report the running byte sum with `None` total — the FE shows an indeterminate/byte
            // progress, consistent with the whisper multi-GB bar.
            on_progress(base.saturating_add(d), None);
        })
        .await?;
        base = base.saturating_add(std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0));
    }
    Ok(())
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
    // An empty configured size resolves the machine-conditional default (T2 flip) so a legacy
    // `""` config follows the same ONE decision as `AppConfig::default()`.
    let derived = models_dir()?.join(model_filename(&effective_model_size(size), language));
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
    // Resolve the effective size ONCE so the presence check and the download name can't diverge
    // (an empty size resolves the machine-conditional default — see `effective_model_size`).
    let size = effective_model_size(size);
    if let Some(found) = resolve_model_path(configured, &size, language)? {
        return Ok(found);
    }

    let dir = models_dir()?;
    let file = model_filename(&size, language);
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
    // R1: guard the `.part` so ANY early return below — a mid-stream body/write/flush error, a
    // truncated (empty) body, or a task drop — removes the partial file instead of orphaning up to
    // ~3.1 GB on disk. `disarm()`ed ONLY after the successful atomic rename (the part no longer
    // exists then, so it's a no-op either way — but disarming keeps the log honest). The sync
    // `std::fs::remove_file` in Drop is safe here (best-effort cleanup of a small metadata op).
    let mut guard = PartFileGuard::new(part.clone());

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
        // The guard removes the empty `.part` on the early return below.
        return Err(AppError::Transcribe(
            "model download returned empty body".into(),
        ));
    }
    tokio::fs::rename(&part, dest)
        .await
        .map_err(|e| AppError::Transcribe(format!("rename model file: {e}")))?;
    // Renamed successfully — the `.part` is gone; don't let the guard log/try to remove it.
    guard.disarm();

    tracing::info!(
        target: "transcribe",
        file = %dest.display(),
        bytes = downloaded,
        "whisper model ready"
    );
    Ok(())
}

/// RAII guard that removes a partial `<model>.part` download file on drop unless [`disarm`]ed after
/// the successful atomic rename (R1). Guarantees a mid-stream error / aborted model switch never
/// leaves multi-GB residue: every `?` early return in [`download_model_streaming`] unwinds through
/// this drop. Best-effort + panic-free (a failed remove is ignored — startup [`sweep_stale_model_parts`]
/// is the crash/force-quit safety net). No PII: the `.part` path carries only a model filename.
struct PartFileGuard {
    part: PathBuf,
    armed: bool,
}

impl PartFileGuard {
    fn new(part: PathBuf) -> Self {
        Self { part, armed: true }
    }

    /// Called after the successful rename (the `.part` no longer exists) so drop is a no-op.
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for PartFileGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_file(&self.part);
        }
    }
}

/// Best-effort startup sweep: remove any STALE `*.part` download residue in `models_dir` left by a
/// crash / force-quit / aborted model switch mid-download (up to ~3.1 GB per orphan). Only removes a
/// `.part` whose mtime is OLDER than [`STALE_PART_AGE_SECS`] so it can NEVER race a live in-progress
/// download (whose `.part` mtime stays fresh). This is the only thing that reclaims crash orphans —
/// the in-process [`PartFileGuard`] covers only clean error returns. Panic-free; logs a COUNT only
/// (no PII — a model `.part` name carries a whisper model id, nothing user-authored).
pub fn sweep_stale_model_parts(models_dir: &Path) {
    /// A `.part` older than 1 h cannot belong to a live download — the streaming writer touches its
    /// file on every chunk, so a real in-progress fetch keeps the mtime fresh.
    const STALE_PART_AGE_SECS: u64 = 3600;
    let Ok(entries) = std::fs::read_dir(models_dir) else {
        return;
    };
    let now = std::time::SystemTime::now();
    let mut removed = 0u32;
    for entry in entries.flatten() {
        // Match the `.part` EXTENSION (covers `ggml-tiny.part` from `with_extension("part")` and any
        // `<name>.bin.part` shape). Never touch a real model file.
        if entry.path().extension().and_then(|e| e.to_str()) != Some("part") {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .map(|age| age.as_secs() > STALE_PART_AGE_SECS)
            .unwrap_or(false);
        if stale && std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    if removed > 0 {
        tracing::warn!(target: "transcribe", removed, "swept stale model .part download residue at startup");
    }
}

/// Non-progress download for the VAD + diarization models (small, no UI progress bar). Delegates to
/// [`download_model_streaming`] with a no-op callback so all model fetches share one atomic path.
async fn download_model(url: &str, dest: &Path) -> Result<()> {
    download_model_streaming(url, dest, |_, _| {}).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// R1 helper: make a unique temp dir for a `.part`/sweep fixture.
    fn parts_tmp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "murmur-model-parts-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// R1 (RED-before-GREEN, mid-stream error path): a download that fails PART-WAY through the body
    /// must leave NO `.part` residue. Before the fix the write/flush/body loop returned via `?` and
    /// orphaned a multi-GB partial; the `PartFileGuard` now removes it on any early return.
    ///
    /// Drives the REAL `download_model_streaming` against a local socket that advertises a large
    /// `Content-Length` but closes after a few bytes → `reqwest`'s `chunk()` errors mid-stream. On the
    /// unpatched loop the `.part` survives (FAIL); with the guard it is gone (PASS).
    #[tokio::test]
    async fn download_removes_part_on_midstream_error() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        // Serve ONE connection: promise 1 MB, send 8 bytes, then drop the socket → truncated body.
        let server = std::thread::spawn(move || {
            if let Ok((mut sock, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf); // consume the request line/headers
                let _ = sock.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 1000000\r\nConnection: close\r\n\r\n",
                );
                let _ = sock.write_all(b"PARTIAL0");
                let _ = sock.flush();
                // Drop `sock` → connection closes with only 8/1000000 bytes delivered.
            }
        });

        let dir = parts_tmp_dir("midstream");
        let dest = dir.join("ggml-tiny.bin");
        let url = format!("http://{addr}/model.bin");

        let res = download_model_streaming(&url, &dest, |_, _| {}).await;
        assert!(res.is_err(), "a truncated body must surface an error");

        // `with_extension("part")` maps `ggml-tiny.bin` → `ggml-tiny.part`.
        let part = dest.with_extension("part");
        assert!(
            !part.exists(),
            "the partial `.part` must be removed on a mid-stream error (no multi-GB orphan)"
        );
        assert!(!dest.exists(), "no model file is produced from a failed download");

        let _ = server.join();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// R1 unit: the `PartFileGuard` removes the `.part` on drop UNLESS disarmed after a successful
    /// rename. This is the exact contract the mid-stream test above relies on.
    #[test]
    fn part_file_guard_removes_unless_disarmed() {
        let dir = parts_tmp_dir("guard");
        // (a) armed guard → drop removes the file.
        let armed = dir.join("armed.part");
        std::fs::write(&armed, b"partial").unwrap();
        {
            let _g = PartFileGuard::new(armed.clone());
        }
        assert!(!armed.exists(), "an armed guard removes the `.part` on drop");

        // (b) disarmed guard → drop leaves the file (success path already renamed it away).
        let kept = dir.join("kept.part");
        std::fs::write(&kept, b"renamed-away").unwrap();
        {
            let mut g = PartFileGuard::new(kept.clone());
            g.disarm();
        }
        assert!(kept.exists(), "a disarmed guard must NOT remove the file");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// R1 (RED-before-GREEN, crash-orphan path): the startup sweep reclaims a STALE `.part` (old
    /// mtime — a crash/force-quit orphan) while leaving a real `.bin` model AND a FRESH `.part` (a
    /// possibly-live download) untouched. Before the fix nothing ever reclaimed a crash orphan.
    #[test]
    fn sweep_removes_stale_part_keeps_bin_and_fresh_part() {
        let dir = parts_tmp_dir("sweep");

        // A stale orphan: old mtime (2 h) → must be swept.
        let stale = dir.join("ggml-tiny.bin.part");
        std::fs::write(&stale, b"orphaned-partial").unwrap();
        let two_hours_ago =
            std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 3600);
        filetime_set(&stale, two_hours_ago);

        // A fresh `.part` (possibly a live in-progress download) → must survive.
        let fresh = dir.join("ggml-small.part");
        std::fs::write(&fresh, b"in-progress").unwrap();

        // A real model file → must survive.
        let real = dir.join("ggml-tiny.bin");
        std::fs::write(&real, b"MODEL-BYTES").unwrap();

        sweep_stale_model_parts(&dir);

        assert!(!stale.exists(), "a stale `.part` orphan must be swept");
        assert!(fresh.exists(), "a fresh `.part` (possibly live) must NOT be raced/removed");
        assert!(real.exists(), "a real model `.bin` must never be touched");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Set a file's mtime to `when` via a truncate-in-place touch fallback. `std::fs` has no mtime
    /// setter, so we age the fixture with a no-dep `utimensat`-free trick: rewrite the file, then
    /// use the FS's own timestamp by sleeping is too slow — instead we spawn `/usr/bin/touch -t`.
    /// macOS-only test helper (this crate is macOS-first; tests run on the same platform).
    fn filetime_set(path: &Path, when: std::time::SystemTime) {
        let secs = when
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        // `touch -t [[CC]YY]MMDDhhmm[.SS]` — format the aged timestamp in local-agnostic UTC via
        // a simple civil-time breakdown is overkill; use `-d @epoch`-free `-t` with a computed
        // value. Simplest robust path: `touch -A -<seconds>` is not portable, so use `-t`.
        // Build the `-t` stamp from the epoch using `date -r`.
        let stamp = std::process::Command::new("/bin/date")
            .args(["-r", &secs.to_string(), "+%Y%m%d%H%M.%S"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .expect("date -r must format the aged mtime");
        let ok = std::process::Command::new("/usr/bin/touch")
            .args(["-t", &stamp])
            .arg(path)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(ok, "touch -t must age the fixture mtime");
    }

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

    const GIB: u64 = 1024 * 1024 * 1024;

    /// T2 DEFAULT FLIP branch 1 — turbo already downloaded wins regardless of RAM (even
    /// unknown/low): the user already paid the 874 MB.
    #[test]
    fn default_model_size_prefers_downloaded_turbo() {
        let files = ["ggml-large-v3-turbo-q8_0.bin", "ggml-small.bin"];
        assert_eq!(default_model_size(&files, None), TURBO_DEFAULT_SIZE);
        assert_eq!(default_model_size(&files, Some(8 * GIB)), TURBO_DEFAULT_SIZE);
        assert_eq!(
            default_model_size(&["ggml-large-v3-turbo-q8_0.bin"], Some(64 * GIB)),
            TURBO_DEFAULT_SIZE
        );
    }

    /// T2 DEFAULT FLIP branch 2 — a FRESH install (no whisper model on disk) with ≥ 12 GB RAM
    /// defaults to turbo; the VAD ggml file / onnx sidecars / `.part` partials do NOT make an
    /// install "existing".
    #[test]
    fn default_model_size_fresh_install_big_ram_gets_turbo() {
        let none: [&str; 0] = [];
        assert_eq!(default_model_size(&none, Some(16 * GIB)), TURBO_DEFAULT_SIZE);
        // The floor is inclusive.
        assert_eq!(default_model_size(&none, Some(12 * GIB)), TURBO_DEFAULT_SIZE);
        // Non-whisper residents of the models dir still count as fresh.
        let sidecars = [
            VAD_MODEL_FILE,
            DIARIZE_SEG_MODEL_FILE,
            DIARIZE_EMB_MODEL_FILE,
            "ggml-small.bin.part",
        ];
        assert_eq!(default_model_size(&sidecars, Some(16 * GIB)), TURBO_DEFAULT_SIZE);
    }

    /// T2 DEFAULT FLIP branch 3a — low or UNKNOWN RAM stays on `small` (a broken RAM probe must
    /// never trigger an 874 MB download: fail-SMALL, not fail-open).
    #[test]
    fn default_model_size_low_or_unknown_ram_stays_small() {
        let none: [&str; 0] = [];
        assert_eq!(default_model_size(&none, Some(8 * GIB)), "small");
        assert_eq!(default_model_size(&none, Some(12 * GIB - 1)), "small");
        assert_eq!(default_model_size(&none, None), "small");
    }

    /// T2 DEFAULT FLIP branch 3b — the existing-install-no-surprise PROPERTY: ANY whisper model
    /// already on disk (and no turbo) keeps the default at `small`, however big the RAM. An
    /// install that chose `small` never gets a surprise download or behavior change.
    #[test]
    fn default_model_size_existing_install_never_surprised() {
        for present in [
            "ggml-tiny.bin",
            "ggml-base.bin",
            "ggml-small.bin",
            "ggml-small.en.bin",
            "ggml-medium-q8_0.bin",
            "ggml-large-v3.bin",
        ] {
            assert_eq!(
                default_model_size(&[present], Some(64 * GIB)),
                "small",
                "existing install with {present} must stay on small"
            );
        }
    }

    /// T2 DEFAULT FLIP — the constant pair stays coherent with `model_filename` for every
    /// language (turbo is multilingual-only).
    #[test]
    fn turbo_default_file_matches_model_filename() {
        for lang in ["", "en", "pl"] {
            assert_eq!(model_filename(TURBO_DEFAULT_SIZE, lang), TURBO_DEFAULT_FILE);
        }
    }

    /// T2 DEFAULT FLIP — `effective_model_size`: non-empty passes through verbatim; blank
    /// resolves the same value as the machine-conditional resolver (ONE decision).
    #[test]
    fn effective_model_size_passthrough_and_blank_resolution() {
        assert_eq!(effective_model_size("small"), "small");
        assert_eq!(effective_model_size(" large-v3 "), "large-v3");
        let resolved = default_model_size_now();
        assert_eq!(effective_model_size(""), resolved);
        assert_eq!(effective_model_size("   "), resolved);
        assert!(resolved == "small" || resolved == TURBO_DEFAULT_SIZE);
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

    /// T2 default-flip follow-up — the live-pin ABSENT-FILE fallback picks the LARGEST
    /// downloaded live-SAFE model (small → small-q8_0 → base → tiny) and NEVER medium/large:
    /// a fresh turbo-only install (the flip's target machines) must resolve `None` so the
    /// live tick never decodes a large-v3 encoder.
    #[test]
    fn live_fallback_never_picks_medium_or_large() {
        let dir = std::env::temp_dir().join(format!("murmur-live-fb-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        // Fresh turbo-default install: ONLY the turbo file (+ VAD) on disk → None.
        std::fs::write(dir.join("ggml-large-v3-turbo-q8_0.bin"), b"x").unwrap();
        std::fs::write(dir.join(VAD_MODEL_FILE), b"x").unwrap();
        assert_eq!(live_fallback_model_in(&dir, ""), None);
        assert_eq!(live_fallback_model_in(&dir, "pl"), None);

        // medium/large additions still resolve None.
        std::fs::write(dir.join("ggml-medium.bin"), b"x").unwrap();
        std::fs::write(dir.join("ggml-large-v3.bin"), b"x").unwrap();
        assert_eq!(live_fallback_model_in(&dir, ""), None);

        // tiny appears → picked; base → preferred; small-q8_0 → preferred; small wins.
        std::fs::write(dir.join("ggml-tiny.bin"), b"x").unwrap();
        assert_eq!(
            live_fallback_model_in(&dir, "pl"),
            Some(dir.join("ggml-tiny.bin"))
        );
        std::fs::write(dir.join("ggml-base.bin"), b"x").unwrap();
        assert_eq!(
            live_fallback_model_in(&dir, ""),
            Some(dir.join("ggml-base.bin"))
        );
        std::fs::write(dir.join("ggml-small-q8_0.bin"), b"x").unwrap();
        assert_eq!(
            live_fallback_model_in(&dir, ""),
            Some(dir.join("ggml-small-q8_0.bin"))
        );
        std::fs::write(dir.join("ggml-small.bin"), b"x").unwrap();
        assert_eq!(
            live_fallback_model_in(&dir, ""),
            Some(dir.join("ggml-small.bin"))
        );

        // English prefers the `.en` build but rides a multilingual of the same size.
        assert_eq!(
            live_fallback_model_in(&dir, "en"),
            Some(dir.join("ggml-small.bin"))
        );
        std::fs::write(dir.join("ggml-small.en.bin"), b"x").unwrap();
        assert_eq!(
            live_fallback_model_in(&dir, "en"),
            Some(dir.join("ggml-small.en.bin"))
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// T2 default-flip follow-up — the heavy-file classifier: every medium/large-class name
    /// (incl. turbo + quants + `.en`) is heavy; live-safe sizes and unrecognizable custom
    /// names are not (custom `whisper_model_path` keeps its pre-pin fallback behavior).
    #[test]
    fn live_heavy_classifier_flags_medium_and_large_class_files() {
        for heavy in [
            "ggml-large-v3.bin",
            "ggml-large-v3-turbo.bin",
            "ggml-large-v3-turbo-q8_0.bin",
            "ggml-large-v3-q5_0.bin",
            "ggml-medium.bin",
            "ggml-medium.en.bin",
            "ggml-medium-q8_0.bin",
        ] {
            assert!(
                is_live_heavy_model_file(Path::new(heavy)),
                "{heavy} must classify heavy"
            );
        }
        for safe in [
            "ggml-tiny.bin",
            "ggml-base.en.bin",
            "ggml-small.bin",
            "ggml-small-q8_0.bin",
            "my-custom-model.bin",
        ] {
            assert!(
                !is_live_heavy_model_file(Path::new(safe)),
                "{safe} must classify live-safe"
            );
        }
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

    /// The parakeet download URLs point at the csukuangfj sherpa-onnx nemo mirror (`resolve/main`),
    /// one per file — the exact repo whose four files are verified HTTP-200.
    #[test]
    fn parakeet_urls_point_at_sherpa_nemo_mirror() {
        let base = "https://huggingface.co/csukuangfj/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8/resolve/main";
        for f in [
            PARAKEET_ENCODER,
            PARAKEET_DECODER,
            PARAKEET_JOINER,
            PARAKEET_TOKENS,
        ] {
            assert_eq!(parakeet_model_url(f), format!("{base}/{f}"));
        }
    }

    /// `parakeet_models_present` (via the same all-four-files predicate over a temp dir): a
    /// half-present bundle reads as ABSENT; only all four files ⇒ present. Uses the PARAKEET_FILES
    /// list directly against a temp dir so the check is testable without touching the real
    /// app-data models dir.
    #[test]
    fn parakeet_present_requires_every_file() {
        let dir = std::env::temp_dir().join(format!("murmur-parakeet-model-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let present = |d: &Path| PARAKEET_FILES.iter().all(|f| d.join(f).is_file());

        assert!(!present(&dir), "nothing on disk → absent");
        // Write three of four → still absent.
        for f in [PARAKEET_ENCODER, PARAKEET_DECODER, PARAKEET_JOINER] {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        assert!(!present(&dir), "three of four → still absent");
        // The fourth completes the set → present.
        std::fs::write(dir.join(PARAKEET_TOKENS), b"x").unwrap();
        assert!(present(&dir), "all four → present");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `parakeet_model_paths` resolves to `<parakeet_dir>/<file>` for each of the four files — the
    /// SAME layout `parakeet_models_present` checks (they must never diverge).
    #[test]
    fn parakeet_model_paths_match_dir_and_consts() {
        let paths = parakeet_model_paths().unwrap();
        let dir = parakeet_dir().unwrap();
        assert_eq!(paths.encoder, dir.join(PARAKEET_ENCODER));
        assert_eq!(paths.decoder, dir.join(PARAKEET_DECODER));
        assert_eq!(paths.joiner, dir.join(PARAKEET_JOINER));
        assert_eq!(paths.tokens, dir.join(PARAKEET_TOKENS));
    }
}
