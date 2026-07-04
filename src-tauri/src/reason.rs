//! Local on-device reasoning seam — WIRED into production.
//!
//! [`LocalReasoner`] is the trait the whole brain reasons through. It is NO longer inert: the live
//! dispatch runs through [`ReasonerCell`] (held on `AppState.reasoner`) and drives the shipped
//! reasoning paths — the agentic tool-choice loop ([`crate::agent`]), the retrieval planner
//! ([`crate::orchestrate`]), fact/user-memory extraction ([`crate::facts`] / [`crate::user_memory`]),
//! voice-intent interpretation ([`crate::voice_action`]), and the note/Ask commands
//! (`ask_vault`, note synthesis in `commands.rs` / `pipeline.rs`). [`active_reasoner`] selects the
//! concrete impl at runtime from the configured [`BrainBackend`]: the cloud provider
//! ([`CloudReasoner`], claude_code / anthropic / gateway / local Ollama), the on-device GGUF
//! ([`mistral::MistralReasoner`], selected when a model resolves on disk), the experimental Apple
//! Foundation sidecar ([`afm::AfmReasoner`]), or the dependency-free [`StubReasoner`] fallback.
//!
//! The [`LocalReasoner::structured`] method is the load-bearing seam: it is where reliable
//! tool-call / NER-classification / retrieval-plan JSON comes from. NONE of the shipped reasoners
//! grammar-constrain the decode — the on-device GGUF path DELIBERATELY abandoned
//! `Constraint::JsonSchema` (it overflowed the model context on Bielik-11B; see
//! [`mistral::MistralReasoner::structured`]) and instead INSTRUCTS the JSON schema in the prompt,
//! recovering the object with [`extract_first_json`], the SAME schema-in-prompt approach
//! [`CloudReasoner`] uses. The robust recover-JSON-from-noisy-text path is therefore load-bearing
//! for every backend, and every reasoner (stub included) routes through it.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde_json::Value;

use crate::error::{AppError, Result};
use crate::settings::{AppConfig, BrainBackend};
use crate::summarize::provider::SummarizerProvider;
use crate::summarize::roles::{self, Role};

/// The REAL on-device reasoner (mistral.rs / GGUF). ALWAYS compiled; the real impl is selected at
/// runtime by [`local_reasoner`] when a GGUF resolves on disk, else the dependency-free stub.
pub mod mistral;

/// WS2 — the EXPERIMENTAL on-device Apple Foundation Models reasoner ([`afm::AfmReasoner`]), driven
/// by the `meetnotes-afm` Swift sidecar (macOS 26+). Selected at runtime for
/// [`BrainBackend::AppleFoundation`]; falls back to the [`StubReasoner`] when the sidecar is absent
/// (the current state on every non-macOS-26 machine).
pub mod afm;

/// The Murmur Brain engine CLASS a registry model serves. `Light` = fast, small-context work run
/// during a recording (realtime reactions, fact-triple extraction, short classification); `Heavy` =
/// note/Ask summarization + post-call analysis. The posture presets resolve "the light model" / "the
/// heavy model" through this tag rather than hardcoding ids (spec §3.1). Serialized lowercase for the
/// FE picker (`light` / `heavy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelClass {
    Light,
    Heavy,
}

/// A curated, mistral.rs-arch-SAFE on-device reasoning model the user can pick from. The app must
/// serve BOTH English and Polish, so the brain offers a CHOICE — not one hardcoded GGUF. Every entry
/// here is a `Q4_K_M` GGUF whose architecture mistral.rs parses today (`llama` / `qwen2` / `qwen3`,
/// NOT `qwen35` / `qwen3vl`) AND whose license permits commercial shipping (Apache-2.0 Qwen3 /
/// Bielik-v3 — NEVER the Qwen *Research* License; see [`RETIRED_BRAIN_MODELS`]). Static, compile-time
/// data — no I/O, no allocation.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrainModel {
    /// Stable id used by `download_brain_model` / `select_brain_model` and persisted in config.
    pub id: &'static str,
    /// Human-friendly display name for the Phase-H picker.
    pub name: &'static str,
    /// On-disk filename inside the shared models dir.
    pub filename: &'static str,
    /// Hugging Face `resolve/main` raw-file URL. INBOUND ONLY — fetched, never sent meeting content.
    pub url: &'static str,
    /// Approximate download / on-disk size in bytes (for the picker; not a verification hash).
    pub approx_size_bytes: u64,
    /// Minimum system RAM (whole GB) the model realistically needs RUN ALONE — drives RAM-gating in
    /// the picker. NOTE: co-residency during a live call needs the combined-residency guard, not this
    /// per-model figure (spec §3.3).
    pub min_ram_gb: u32,
    /// Language tags the model serves (`pl`, `en`, `multi`).
    pub languages: &'static [&'static str],
    /// mistral.rs architecture key (`llama` / `qwen2` / `qwen3`) — all parse-safe.
    pub arch: &'static str,
    /// The engine class this model serves (`Light` / `Heavy`).
    pub class: ModelClass,
    /// Pinned lower-case hex SHA-256 of the GGUF, verified after download (spec §5). `None` = NOT yet
    /// pinned: the downloader logs a warning and skips verification (an honest interim state — the
    /// real hashes must be filled from Hugging Face before any "default-offered" ship). NEVER fabricate
    /// a hash — a wrong pin would fail every download; a missing pin only forfeits the integrity check.
    pub sha256: Option<&'static str>,
}

/// The curated registry. Display order groups the recommended "Murmur Brain" defaults first (light,
/// then heavy), then the Polish-native Bielik alternates, then the large multilingual option.
///
/// LICENSE INVARIANT: every id here is Apache-2.0 (Qwen3 dense / Bielik-v3) so Murmur may curate,
/// ship, and (later) fine-tune it commercially. The former `qwen2.5-3b` was RETIRED — it is under the
/// non-commercial Qwen Research License; see [`RETIRED_BRAIN_MODELS`] for the installed-base handling.
///
/// NOTE on URLs/sizes: the exact bartowski/speakleash GGUF filenames + byte sizes below are the
/// documented naming patterns; a wrong filename fails the download LOUDLY (HTTP 404 → graceful `Err`),
/// never silently. `sha256` is `None` until pinned from HF (a release-gate task — see [`BrainModel`]).
pub static BRAIN_MODELS: &[BrainModel] = &[
    // ── Light class (realtime reactions + fact extraction; run during a recording) ──────────────
    BrainModel {
        id: "qwen3-1.7b",
        name: "Qwen3 1.7B (light · multilingual)",
        filename: "Qwen_Qwen3-1.7B-Q4_K_M.gguf",
        url: "https://huggingface.co/bartowski/Qwen_Qwen3-1.7B-GGUF/resolve/main/Qwen_Qwen3-1.7B-Q4_K_M.gguf",
        approx_size_bytes: 1_100_000_000, // ~1.1 GB (verify on first download)
        min_ram_gb: 4,
        languages: &["en", "multi", "pl"],
        arch: "qwen3",
        class: ModelClass::Light,
        sha256: None,
    },
    BrainModel {
        id: "bielik-1.5b-v3",
        name: "Bielik 1.5B v3 (light · Polish-native)",
        filename: "Bielik-1.5B-v3.0-Instruct.Q4_K_M.gguf",
        url: "https://huggingface.co/speakleash/Bielik-1.5B-v3.0-Instruct-GGUF/resolve/main/Bielik-1.5B-v3.0-Instruct.Q4_K_M.gguf",
        approx_size_bytes: 1_050_000_000, // ~1.0 GB (verify on first download)
        min_ram_gb: 4,
        languages: &["pl", "en"],
        arch: "llama",
        class: ModelClass::Light,
        sha256: None,
    },
    // ── Heavy class (local Notes/Ask + post-call analysis; never resident during a recording) ────
    BrainModel {
        id: "qwen3-4b-instruct-2507",
        name: "Qwen3 4B Instruct 2507 (heavy · multilingual)",
        filename: "Qwen_Qwen3-4B-Instruct-2507-Q4_K_M.gguf",
        url: "https://huggingface.co/bartowski/Qwen_Qwen3-4B-Instruct-2507-GGUF/resolve/main/Qwen_Qwen3-4B-Instruct-2507-Q4_K_M.gguf",
        approx_size_bytes: 2_500_000_000, // ~2.5 GB
        min_ram_gb: 6,
        languages: &["en", "multi", "pl"],
        arch: "qwen3",
        class: ModelClass::Heavy,
        sha256: None,
    },
    BrainModel {
        id: "bielik-4.5b-v3",
        name: "Bielik 4.5B v3 (heavy · Polish-native)",
        filename: "Bielik-4.5B-v3.0-Instruct.Q4_K_M.gguf",
        url: "https://huggingface.co/speakleash/Bielik-4.5B-v3.0-Instruct-GGUF/resolve/main/Bielik-4.5B-v3.0-Instruct.Q4_K_M.gguf",
        approx_size_bytes: 2_800_000_000, // ~2.8 GB (verify on first download)
        min_ram_gb: 8,
        languages: &["pl", "en"],
        arch: "llama",
        class: ModelClass::Heavy,
        sha256: None,
    },
    BrainModel {
        id: "bielik-11b-v3",
        name: "Bielik 11B v3 (heavy · Polish-native)",
        filename: "Bielik-11B-v3.0-Instruct.Q4_K_M.gguf",
        url: "https://huggingface.co/speakleash/Bielik-11B-v3.0-Instruct-GGUF/resolve/main/Bielik-11B-v3.0-Instruct.Q4_K_M.gguf",
        approx_size_bytes: 7_215_545_057, // ~6.72 GB
        min_ram_gb: 10,
        languages: &["pl", "en"],
        arch: "llama",
        class: ModelClass::Heavy,
        sha256: None,
    },
    BrainModel {
        id: "qwen3-14b",
        name: "Qwen3 14B (heavy · multilingual/English)",
        filename: "Qwen_Qwen3-14B-Q4_K_M.gguf",
        url: "https://huggingface.co/bartowski/Qwen_Qwen3-14B-GGUF/resolve/main/Qwen_Qwen3-14B-Q4_K_M.gguf",
        approx_size_bytes: 9_663_676_416, // ~9 GB
        min_ram_gb: 14,
        languages: &["en", "multi"],
        arch: "qwen3",
        class: ModelClass::Heavy,
        sha256: None,
    },
];

/// A RETIRED registry model: removed from the picker (un-selectable fresh) but kept RESOLVABLE while
/// its GGUF is still on disk, so a user who already downloaded + selected it is NOT silently degraded
/// to the stub (spec §3.1 installed-base migration; the P1 invariant "no silent capability loss").
#[derive(Debug, Clone)]
pub struct RetiredModel {
    /// The retired id, still matched against a persisted `brain_model_id`.
    pub id: &'static str,
    /// On-disk filename — resolved from the shared models dir if present (never downloaded again).
    pub filename: &'static str,
    /// The active replacement id the migration nudge points the user at.
    pub replacement_id: &'static str,
    /// Human-readable retirement reason (surfaced in the nudge).
    pub reason: &'static str,
}

/// Models pulled from the active registry. Resolvable-if-present, hidden from the picker.
pub static RETIRED_BRAIN_MODELS: &[RetiredModel] = &[RetiredModel {
    id: "qwen2.5-3b",
    filename: "Qwen2.5-3B-Instruct-Q4_K_M.gguf",
    replacement_id: "qwen3-1.7b",
    reason: "Qwen2.5 ships under the non-commercial Qwen Research License; replaced by the Apache-2.0 Qwen3 1.7B.",
}];

/// Look up an ACTIVE registry entry by id. `None` for unknown OR retired ids (the caller rejects a
/// fresh selection with `InvalidArg`; retired-but-on-disk resolution goes through
/// [`resolve_brain_model`] via [`retired_model_by_id`]).
pub fn brain_model_by_id(id: &str) -> Option<&'static BrainModel> {
    BRAIN_MODELS.iter().find(|m| m.id == id)
}

/// Look up a RETIRED model by id (installed-base migration path).
pub fn retired_model_by_id(id: &str) -> Option<&'static RetiredModel> {
    RETIRED_BRAIN_MODELS.iter().find(|m| m.id == id)
}

/// The FE-facing migration nudge when the persisted selection points at a retired model. `None` when
/// the selection is active/absent. Lets the settings UI say "this model was retired for licensing;
/// switch to <replacement>; optionally delete the old file" without any silent change.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetiredModelNudge {
    pub retired_id: String,
    pub replacement_id: String,
    pub replacement_name: String,
    pub reason: String,
    /// The retired GGUF is still on disk (so it currently keeps working; deletion is offered).
    pub file_on_disk: bool,
}

/// Build the retirement nudge for a persisted `selected_id` against the shared `models_dir`. Pure
/// (only a per-file existence probe), so it is unit-testable with a fake dir.
pub fn retired_model_nudge(
    selected_id: Option<&str>,
    models_dir: &Path,
) -> Option<RetiredModelNudge> {
    let retired = retired_model_by_id(selected_id?)?;
    let replacement = brain_model_by_id(retired.replacement_id);
    Some(RetiredModelNudge {
        retired_id: retired.id.to_string(),
        replacement_id: retired.replacement_id.to_string(),
        replacement_name: replacement.map(|m| m.name.to_string()).unwrap_or_default(),
        reason: retired.reason.to_string(),
        file_on_disk: models_dir.join(retired.filename).is_file(),
    })
}

/// IPC view of a [`BrainModel`] for the picker: the static metadata plus the two runtime flags the
/// FE needs — `downloaded` (file present in the models dir) and `fits_ram` (the model's `min_ram_gb`
/// is within the machine's total RAM). `selected` mirrors the persisted `brain_model_id`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrainModelDto {
    pub id: String,
    pub name: String,
    pub filename: String,
    pub url: String,
    pub approx_size_bytes: u64,
    pub min_ram_gb: u32,
    pub languages: Vec<String>,
    pub arch: String,
    /// The engine class (`light` / `heavy`) — lets the FE group the picker by role.
    pub class: ModelClass,
    /// The GGUF already exists in the shared models dir.
    pub downloaded: bool,
    /// `min_ram_gb` fits the machine. When total RAM is unknown this is `true` (never HIDE a model
    /// behind a failed probe — the user can still try it).
    pub fits_ram: bool,
    /// This id is the persisted `brain_model_id` selection.
    pub selected: bool,
}

/// Build the picker DTOs from the registry against a given models dir + (optional) total RAM in GB +
/// the current selection. Pure (the only I/O is the per-file `is_file` existence probe) so it is
/// unit-testable with a fake models dir + a tiny RAM threshold. Unknown RAM ⇒ `fits_ram = true`.
pub fn brain_model_dtos(
    models_dir: &Path,
    total_ram_gb: Option<u64>,
    selected_id: Option<&str>,
) -> Vec<BrainModelDto> {
    BRAIN_MODELS
        .iter()
        .map(|m| BrainModelDto {
            id: m.id.to_string(),
            name: m.name.to_string(),
            filename: m.filename.to_string(),
            url: m.url.to_string(),
            approx_size_bytes: m.approx_size_bytes,
            min_ram_gb: m.min_ram_gb,
            languages: m.languages.iter().map(|s| s.to_string()).collect(),
            arch: m.arch.to_string(),
            class: m.class,
            downloaded: models_dir.join(m.filename).is_file(),
            fits_ram: total_ram_gb.map(|g| u64::from(m.min_ram_gb) <= g).unwrap_or(true),
            selected: selected_id == Some(m.id),
        })
        .collect()
}

/// The registry's DEFAULT model for a class — the first entry of that class in display order
/// (`Light` → qwen3-1.7b, `Heavy` → qwen3-4b-instruct-2507). Used when the user hasn't picked a
/// class-specific model; once its GGUF is downloaded it activates automatically.
pub fn default_model_for_class(class: ModelClass) -> Option<&'static BrainModel> {
    BRAIN_MODELS.iter().find(|m| m.class == class)
}

/// The EFFECTIVE model id for a class: the user's explicit pick
/// (`brain_light_model_id` / `brain_heavy_model_id`) when set, else the registry default for the
/// class. Drives the Brain-Live `light()` / `heavy()` handles.
pub fn class_model_id(cfg: &AppConfig, class: ModelClass) -> Option<String> {
    let explicit = match class {
        ModelClass::Light => cfg.brain_light_model_id.as_deref(),
        ModelClass::Heavy => cfg.brain_heavy_model_id.as_deref(),
    };
    explicit
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.to_string())
        .or_else(|| default_model_for_class(class).map(|m| m.id.to_string()))
}

/// A fixed call-overhead RAM budget (GB) for the combined-residency guard: the OS + a Zoom/Meet call
/// + a browser + the webview + whisper + the embedder, all live during a recording (spec §3.3).
pub const CALL_OVERHEAD_GB: u64 = 4;

/// Estimated combined-residency RAM (GB) for a set of resident models INCLUDING a coarse KV-cache
/// budget — the registry's per-model `min_ram_gb` is per-model-ALONE and LIES for co-residency during
/// a live call (spec §3.3; adversarial review perf #3, which flagged the missing KV line). Weights are
/// ceil-GB of `approx_size_bytes`; KV is a coarse per-model estimate; plus [`CALL_OVERHEAD_GB`]. Pure.
pub fn combined_residency_gb(models: &[&BrainModel], call_overhead_gb: u64) -> u64 {
    /// A heavy model over a long transcript allocates ~1–2 GB of KV; budget 1 GB/model coarsely.
    const KV_PER_MODEL_GB: u64 = 1;
    let gib = 1u64 << 30;
    let weights_gb: u64 = models
        .iter()
        .map(|m| m.approx_size_bytes.div_ceil(gib))
        .sum();
    weights_gb + models.len() as u64 * KV_PER_MODEL_GB + call_overhead_gb
}

/// Whether a resident model set fits the machine's total RAM (spec §3.3 combined guard). `None` total
/// ⇒ `true` — never block behind a failed RAM probe (the picker's existing philosophy).
pub fn residency_fits(
    models: &[&BrainModel],
    total_ram_gb: Option<u64>,
    call_overhead_gb: u64,
) -> bool {
    match total_ram_gb {
        Some(total) => combined_residency_gb(models, call_overhead_gb) <= total,
        None => true,
    }
}

/// Resolve a user-supplied CUSTOM brain GGUF path override, or `Ok(None)`.
///
/// `configured` is the explicit path from settings (`brain_model_path`), used verbatim if it points
/// at an existing file. This is ONLY the custom-override layer; registry-id resolution is layered on
/// top by [`resolve_brain_model`]. A missing file is `Ok(None)`, never an error. NEVER panics.
pub fn brain_model_path(configured: Option<&Path>) -> Result<Option<PathBuf>> {
    if let Some(p) = configured {
        if p.is_file() {
            return Ok(Some(p.to_path_buf()));
        }
    }
    Ok(None)
}

/// Resolve the GGUF the local brain should load, or `Ok(None)` when none is present.
///
/// Resolution order:
/// 1. `configured` — an explicit custom path override (`brain_model_path`); used verbatim if the
///    file exists ([`brain_model_path`]).
/// 2. The file for the selected `model_id` (from the registry) inside the shared models dir, if it
///    already exists on disk.
///
/// Creating the models dir can fail (returns `Err`); a missing/unselected model is `Ok(None)`, NOT
/// an error — the app falls back to the stub. NEVER panics.
pub fn resolve_brain_model(
    configured: Option<&Path>,
    model_id: Option<&str>,
) -> Result<Option<PathBuf>> {
    if let Some(p) = brain_model_path(configured)? {
        return Ok(Some(p));
    }
    if let Some(m) = model_id.and_then(brain_model_by_id) {
        let derived = crate::transcribe::models_dir()?.join(m.filename);
        if derived.is_file() {
            return Ok(Some(derived));
        }
    }
    // Installed-base migration: a RETIRED selection still resolves while its GGUF is on disk, so an
    // existing local brain is never silently downgraded to the stub. Never downloaded again; the
    // picker hides it and nudges the replacement (spec §3.1).
    if let Some(r) = model_id.and_then(retired_model_by_id) {
        let derived = crate::transcribe::models_dir()?.join(r.filename);
        if derived.is_file() {
            return Ok(Some(derived));
        }
    }
    Ok(None)
}

/// Compute the lower-case hex SHA-256 of `path` via the macOS-native `shasum -a 256` (no new crate —
/// CLAUDE.md forbids adding a hashing dependency). Sync core, so it is unit-testable without a
/// runtime; the async wrapper runs it on the blocking pool (hashing a multi-GB file is CPU-heavy).
fn sha256_hex_sync(path: &Path) -> Result<String> {
    let out = std::process::Command::new("shasum")
        .arg("-a")
        .arg("256")
        .arg(path)
        .output()
        .map_err(|e| AppError::Summarize(format!("shasum spawn failed: {e}")))?;
    if !out.status.success() {
        return Err(AppError::Summarize("shasum returned a non-zero status".into()));
    }
    // Output shape: "<hex>  <path>\n" — the hash is the first whitespace-delimited token.
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .map(|s| s.to_lowercase())
        .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| AppError::Summarize("shasum produced no 64-hex digest".into()))
}

async fn sha256_hex(path: &Path) -> Result<String> {
    let p = path.to_path_buf();
    tokio::task::spawn_blocking(move || sha256_hex_sync(&p))
        .await
        .map_err(|e| AppError::Summarize(format!("shasum task join failed: {e}")))?
}

/// Download `url` to `dest` with HTTP Range **resume** + optional pinned **SHA-256** verify (spec §5).
///
/// - Resume: a surviving `dest.part` from an interrupted run is continued via `Range: bytes=<n>-`; if
///   the server ignores Range (200 not 206) the partial is discarded and the download restarts.
/// - Integrity: when `expected_sha256` is `Some`, the completed `.part` is verified BEFORE the atomic
///   rename — a mismatch deletes the file and returns `Err` (never promote an unverified GGUF, which
///   feeds the P1 no-silent-fallback invariant). `None` = unpinned entry: verification is SKIPPED with
///   a loud warning (an honest interim state until the registry hashes are pinned).
///
/// INBOUND ONLY: fetches a model file and sends NO request body / NO meeting content (no egress).
/// Streams via `Response::chunk` (no extra stream-combinator dep). NO PII logged — byte counts only.
pub async fn download_brain_model<F>(
    url: &str,
    dest: &Path,
    expected_sha256: Option<&str>,
    mut on_progress: F,
) -> Result<()>
where
    F: FnMut(u64, Option<u64>),
{
    use tokio::io::AsyncWriteExt;

    tracing::info!(target: "reason", file = %dest.display(), "downloading brain model");

    let part = dest.with_extension("part");
    // Resume from a surviving partial, if any.
    let mut existing: u64 = tokio::fs::metadata(&part).await.map(|m| m.len()).unwrap_or(0);

    let client = reqwest::Client::new();
    let mut req = client.get(url);
    if existing > 0 {
        req = req.header(reqwest::header::RANGE, format!("bytes={existing}-"));
        tracing::info!(target: "reason", resume_from = existing, "resuming brain model download");
    }
    let resp = req
        .send()
        .await
        .map_err(|e| AppError::Summarize(format!("brain model download request failed: {e}")))?;
    let status = resp.status();
    // A resumed request whose range starts at/after EOF returns 416 Range Not Satisfiable: the `.part`
    // is already COMPLETE. Skip the body and go straight to verify + promote — otherwise a
    // fully-downloaded `.part` would brick every retry with a permanent HTTP error (deep-review).
    let already_complete = existing > 0 && status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE;
    if !already_complete && !status.is_success() {
        return Err(AppError::Summarize(format!(
            "brain model download HTTP {status}"
        )));
    }

    let mut downloaded: u64 = existing;
    if !already_complete {
        // We requested a resume but the server sent the FULL body (200, not 206 Partial) → it ignored
        // Range; discard the stale partial and restart from zero so we never concatenate mismatched bytes.
        let resuming = status == reqwest::StatusCode::PARTIAL_CONTENT;
        if existing > 0 && !resuming {
            let _ = tokio::fs::remove_file(&part).await;
            existing = 0;
            downloaded = 0;
        }
        // On a 206 the Content-Length is the REMAINING bytes; total = already-on-disk + remaining.
        let total = resp.content_length().map(|r| existing + r);

        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&part)
            .await
            .map_err(|e| AppError::Summarize(format!("open brain model temp file: {e}")))?;
        let mut resp = resp;
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| AppError::Summarize(format!("brain model download body failed: {e}")))?
        {
            file.write_all(&chunk)
                .await
                .map_err(|e| AppError::Summarize(format!("write brain model chunk: {e}")))?;
            downloaded += chunk.len() as u64;
            on_progress(downloaded, total);
        }
        file.flush()
            .await
            .map_err(|e| AppError::Summarize(format!("flush brain model file: {e}")))?;
        drop(file);
    }

    if downloaded == 0 {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(AppError::Summarize(
            "brain model download returned empty body".into(),
        ));
    }

    // Verify BEFORE promoting — an unverified GGUF must never be loaded.
    match expected_sha256 {
        Some(expected) => {
            let got = sha256_hex(&part).await.map_err(|e| {
                // Can't verify ⇒ don't promote; leave the .part for a resume-verify retry.
                AppError::Summarize(format!("brain model integrity check failed to run: {e}"))
            })?;
            if !got.eq_ignore_ascii_case(expected) {
                let _ = tokio::fs::remove_file(&part).await;
                return Err(AppError::Summarize(
                    "brain model SHA-256 mismatch — download corrupt or tampered; deleted".into(),
                ));
            }
        }
        None => {
            tracing::warn!(
                target: "reason",
                file = %dest.display(),
                "no pinned SHA-256 for this model — integrity check SKIPPED (unpinned registry entry)"
            );
        }
    }

    tokio::fs::rename(&part, dest)
        .await
        .map_err(|e| AppError::Summarize(format!("rename brain model file: {e}")))?;

    tracing::info!(target: "reason", file = %dest.display(), bytes = downloaded, "brain model ready");
    Ok(())
}

/// Resolve the reasoning backend for ONE config value — a point-in-time resolution. The live app
/// dispatches through [`ReasonerCell`] (which re-resolves per call over the shared config handle);
/// this function is the shared resolution logic plus the seam for one-shot/test use.
///
/// Graceful degradation, in priority order:
/// - a GGUF is present at the resolved [`resolve_brain_model`] → the real
///   [`mistral::MistralReasoner`] (lazy: the model loads on first use, not here, so this never blocks
///   startup and never panics);
/// - otherwise (no model, or a path-resolution error) → the dependency-free [`StubReasoner`]. The app
///   works either way — just less smart without the model.
///
/// NEVER panics and NEVER blocks: a missing/failed model is logged (no PII) and falls back to stub.
///
/// Dispatch is on [`AppConfig::brain_backend`] (default `Cloud`, the user's choice):
/// - **`Cloud`** → [`CloudReasoner`] — the cloud LLM via the SAME `make_provider` egress envelope
///   as the note summary. Construction is cheap + infallible; the consent gate fires at CALL time
///   (a no-consent / no-CLI / offline failure degrades to the deterministic floor in `orchestrate.rs`).
/// - **`Local`** → the real `MistralReasoner` when a GGUF is present on disk ([`local_reasoner`]),
///   else the dependency-free `StubReasoner`.
/// - **`Off`** → `StubReasoner` (the deterministic floor).
pub fn active_reasoner(config: &AppConfig) -> Box<dyn LocalReasoner> {
    match config.brain_backend {
        BrainBackend::Cloud => {
            // Cheap: just clones the config; the provider (+ consent gate + RedactingProvider) is
            // built per-call inside CloudReasoner via the same `make_provider` the summary uses.
            tracing::info!(target: "reason", "brain backend = cloud; reasoning via the summarizer-provider seam");
            Box::new(CloudReasoner::new(Arc::new(Mutex::new(config.clone()))))
        }
        BrainBackend::Local => local_reasoner(config),
        BrainBackend::Off => {
            tracing::info!(target: "reason", "brain backend = off; using stub reasoner");
            Box::new(StubReasoner)
        }
        BrainBackend::AppleFoundation => {
            tracing::info!(target: "reason", "brain backend = apple foundation (on-device sidecar)");
            afm::afm_reasoner(config)
        }
    }
}

/// The LIVE reasoner dispatch held in `AppState` — resolves which brain handles each call from the
/// CURRENT config, not a startup snapshot.
///
/// The LOCAL-reasoner cache slot: the resolved GGUF path it was built from (`None` = nothing
/// resolved ⇒ the stub) paired with the loaded instance. Factored out per clippy::type_complexity.
type CachedLocalReasoner = Option<(Option<PathBuf>, Arc<dyn LocalReasoner>)>;

/// `AppState.reasoner` used to be a `Box<dyn LocalReasoner>` resolved ONCE at startup, which made
/// consent grants, consent revocations, provider switches, and `brain_backend` flips require an app
/// restart (and kept a REVOKED consent egressing — the privacy-critical direction). `ReasonerCell`
/// instead holds the SAME `Arc<Mutex<AppConfig>>` the settings/consent commands write and
/// re-resolves the dispatch on every [`ReasonerCell::current`] call:
/// - **Cloud** → a fresh [`CloudReasoner`] over the shared handle (construction is cheap — the
///   provider + consent gate are built per call inside it);
/// - **Local** → the CACHED model instance, keyed by the resolved GGUF path — the loaded model is
///   expensive and must NOT reload per call; it is rebuilt only when the resolved path changes
///   (e.g. the user selects or downloads a different model mid-session);
/// - **Off** → the deterministic [`StubReasoner`].
///
/// Fail-closed: a poisoned config mutex means the consent/provider state is unknowable, so dispatch
/// degrades to the no-egress stub rather than risk a cloud call under stale consent.
pub struct ReasonerCell {
    /// The SAME shared handle the settings/consent commands write (`AppState.config`).
    config: Arc<Mutex<AppConfig>>,
    /// Cached LOCAL reasoner keyed by the GGUF path it resolved from (`None` = nothing resolved ⇒
    /// the stub). The loaded model is expensive, so the instance is reused across calls and rebuilt
    /// only when the resolved path changes — which also means a model downloaded or selected
    /// mid-session activates on the next call.
    local: Mutex<CachedLocalReasoner>,
    /// TEST-ONLY pinned reasoner (mirrors `CloudReasoner::provider_override`): `Some` makes
    /// `current()` always return this instance, so command tests keep a deterministic stub.
    /// `#[cfg(test)]` makes "no fixed reasoner in production" STRUCTURAL — the field does not
    /// exist in a release build, so no production path can ever pin a dispatch.
    #[cfg(test)]
    fixed: Option<Arc<dyn LocalReasoner>>,
}

impl ReasonerCell {
    /// Build the live dispatch over the shared config handle. Cheap: nothing is resolved until the
    /// first [`current`](Self::current) call.
    pub fn new(config: Arc<Mutex<AppConfig>>) -> Self {
        Self {
            config,
            local: Mutex::new(None),
            #[cfg(test)]
            fixed: None,
        }
    }

    /// TEST-ONLY: pin `current()` to a fixed reasoner instance (the old `Box<StubReasoner>` test
    /// shape). Production dispatch always goes through [`new`] + live config resolution.
    #[cfg(test)]
    pub(crate) fn fixed(reasoner: Arc<dyn LocalReasoner>) -> Self {
        Self {
            config: Arc::new(Mutex::new(AppConfig::default())),
            local: Mutex::new(None),
            fixed: Some(reasoner),
        }
    }

    /// The reasoner for THIS call, resolved from the CURRENT config — never a startup snapshot.
    /// Legacy shape: dispatches the NOTES role (whose fallback is exactly the pre-role
    /// `brain_backend` mapping). Role-aware call sites use [`current_for`](Self::current_for).
    pub fn current(&self) -> Arc<dyn LocalReasoner> {
        self.current_for(Role::Notes)
    }

    /// The reasoner serving `role` for THIS call, resolved from the CURRENT config — never a
    /// startup snapshot. Dispatch keys on [`roles::reasoner_target`]: with role keys absent it is
    /// the EXACT legacy `brain_backend` mapping for every role (byte-identical dispatch); with a
    /// role's connection key set, that role dispatches its own target.
    pub fn current_for(&self, role: Role) -> Arc<dyn LocalReasoner> {
        #[cfg(test)]
        if let Some(f) = &self.fixed {
            return Arc::clone(f);
        }
        // Fail-closed: a poisoned config mutex makes the consent/provider state unknowable —
        // dispatch the no-egress stub rather than risk a cloud call under stale consent.
        let cfg = match self.config.lock() {
            Ok(c) => c.clone(),
            Err(_) => {
                tracing::warn!(target: "reason", "config mutex poisoned; dispatching the stub (fail-closed)");
                return Arc::new(StubReasoner);
            }
        };
        let target = roles::reasoner_target(role, &cfg);
        match target.connection.as_str() {
            roles::CONN_OFF => Arc::new(StubReasoner),
            roles::CONN_LOCAL => self.local_cached(&cfg, &target),
            // WS2 anti-egress ORDERING (load-bearing): the on-device AFM arm MUST come BEFORE the
            // catch-all cloud arm below. The `_` sends anything non-off/non-local/non-apple to the
            // cloud (egress); without this explicit arm an `apple` target would silently
            // cloud-dispatch. AfmReasoner holds only a PathBuf, so build per call like CloudReasoner
            // (no cache slot); a missing sidecar falls back to the stub inside `afm_reasoner_arc`.
            roles::CONN_AFM => afm::afm_reasoner_arc(&cfg),
            // Cheap per call: the provider (+ consent gate) is built lazily inside, from a fresh
            // config read, so this instance can never pin a stale provider/consent state.
            _ => Arc::new(CloudReasoner::for_role(Arc::clone(&self.config), role)),
        }
    }

    /// The LIGHT-engine handle for Brain-Live features (realtime reactions, fact extraction). Resolves
    /// the posture's light model to a LOADED local instance, or the dependency-free [`StubReasoner`]
    /// when its GGUF is absent/unloadable — **NEVER a cloud fallback** (P1 invariant: a missing local
    /// model DEGRADES the feature, it does not silently egress). Presence is rechecked per call.
    /// Callers consult this ONLY when Brain Live is ON; with it OFF the existing role-resolved paths
    /// apply unchanged. (Adversarial review, code-truth #1/#3 + product #1: the v0.1 "else the
    /// role-resolved reasoner" order would have silently egressed facts on a missing model.)
    pub fn light(&self) -> Arc<dyn LocalReasoner> {
        self.class_or_stub(ModelClass::Light)
    }

    /// The HEAVY-engine handle (local Notes/Ask + post-call analysis). Same LOCAL-or-stub,
    /// NEVER-cloud contract as [`light`](Self::light).
    pub fn heavy(&self) -> Arc<dyn LocalReasoner> {
        self.class_or_stub(ModelClass::Heavy)
    }

    /// Resolve a class engine LOCAL-or-stub — never cloud, never the single class-agnostic custom path
    /// override (`brain_model_path`), so the light and heavy handles can't collide on one custom file.
    /// A poisoned config mutex still yields the stub (fail-closed, no egress).
    fn class_or_stub(&self, class: ModelClass) -> Arc<dyn LocalReasoner> {
        let model_id = match self.config.lock() {
            Ok(c) => class_model_id(&c, class),
            Err(p) => class_model_id(&p.into_inner(), class),
        };
        match resolve_brain_model(None, model_id.as_deref()) {
            Ok(Some(_)) => Arc::from(local_reasoner_resolved(None, model_id.as_deref())),
            _ => Arc::new(StubReasoner),
        }
    }

    /// The reasoner for FACT / user-memory / pre-analysis EXTRACTION. When Brain Live is ON it is the
    /// LOCAL light engine ([`light`](Self::light) — local-or-stub, NEVER cloud), so fact extraction
    /// stops egressing (the enablement card's "facts go fully local" promise; spec §3.4). When OFF it
    /// is the role-resolved Notes reasoner EXACTLY as today (cloud by default). Rechecked per call.
    pub fn extraction_reasoner(&self) -> Arc<dyn LocalReasoner> {
        let brain_live = self
            .config
            .lock()
            .map(|c| c.brain_live)
            .unwrap_or(false);
        if brain_live {
            self.light()
        } else {
            self.current_for(Role::Notes)
        }
    }

    /// The LOCAL dispatch: reuse the cached instance while the resolved GGUF path is unchanged;
    /// rebuild (inside the cache lock, so concurrent callers never double-load a model) only when
    /// it changes. Resolution is a cheap filesystem probe per call — never a model load.
    /// The effective model id comes from the resolved role target (`""` = the persisted
    /// `brain_model_id`, exactly the legacy resolution), so the cache stays keyed on the resolved
    /// GGUF path even when a role key names a different registry model.
    fn local_cached(&self, cfg: &AppConfig, target: &roles::RoleTarget) -> Arc<dyn LocalReasoner> {
        let configured = cfg.brain_model_path.as_deref().map(Path::new);
        let model_id = if target.model.trim().is_empty() {
            cfg.brain_model_id.clone()
        } else {
            Some(target.model.clone())
        };
        let key = resolve_brain_model(configured, model_id.as_deref())
            .ok()
            .flatten();
        let mut cache = match self.local.lock() {
            Ok(g) => g,
            // A pure cache slot carries no invalid state — recover the data instead of poisoning
            // every future local dispatch.
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some((cached_key, cached)) = cache.as_ref() {
            if *cached_key == key {
                return Arc::clone(cached);
            }
        }
        let built: Arc<dyn LocalReasoner> =
            Arc::from(local_reasoner_resolved(configured, model_id.as_deref()));
        *cache = Some((key, Arc::clone(&built)));
        built
    }
}

/// Resolve the LOCAL on-device reasoner: the real [`mistral::MistralReasoner`] when a GGUF resolves
/// on disk (lazy load — never blocks or panics at startup), otherwise the dependency-free
/// [`StubReasoner`]. Used only for [`BrainBackend::Local`]; with no model present it always yields
/// the stub (selection keys ONLY on model presence — mistralrs is always compiled).
fn local_reasoner(config: &AppConfig) -> Box<dyn LocalReasoner> {
    local_reasoner_resolved(
        config.brain_model_path.as_deref().map(Path::new),
        config.brain_model_id.as_deref(),
    )
}

/// The parameterized core of [`local_reasoner`]: build from an explicit custom path override +
/// registry model id (the role layer supplies the effective id; the legacy path passes
/// `brain_model_path`/`brain_model_id` verbatim).
fn local_reasoner_resolved(
    configured: Option<&Path>,
    model_id: Option<&str>,
) -> Box<dyn LocalReasoner> {
    match resolve_brain_model(configured, model_id) {
        Ok(Some(path)) => match mistral::MistralReasoner::new(path) {
            Ok(r) => {
                tracing::info!(target: "reason", id = r.id(), "local brain ready (lazy model load)");
                return Box::new(r);
            }
            Err(e) => {
                tracing::warn!(target: "reason", error = %e, "local brain init failed; using stub reasoner");
            }
        },
        Ok(None) => {
            tracing::info!(target: "reason", "no local brain model present; using stub reasoner");
        }
        Err(e) => {
            tracing::warn!(target: "reason", error = %e, "local brain path resolution failed; using stub reasoner");
        }
    }
    Box::new(StubReasoner)
}

/// Generation options for ONE reasoning call (spec §3.1 / §4.3). Defaults reproduce today's behavior
/// for every reasoner that IGNORES them; the LIGHT realtime-extraction path sets a small `max_tokens`
/// so a rambling on-device generation can't saturate Metal for tens of seconds — the sampler cap is
/// the ONLY real per-call bound (an in-flight generation is not cancellable). `enable_thinking=false`
/// keeps qwen3 thinking traces out of structured extraction (a no-op on non-thinking models).
#[derive(Debug, Clone, Copy, Default)]
pub struct GenOptions {
    /// Hard decode cap (mistralrs `set_sampler_max_len`). `None` = the model's own default (today's
    /// behavior). Load-bearing for the realtime path.
    pub max_tokens: Option<usize>,
    /// Sampling temperature. `None` = the model's default sampler.
    pub temperature: Option<f64>,
    /// Whether to allow qwen3 "thinking" traces. Default `false` (the derived bool default) — we never
    /// want thinking in structured extraction; on non-thinking models this is a no-op.
    pub enable_thinking: bool,
}

impl GenOptions {
    /// The LIGHT realtime-extraction preset: a hard 128-token cap, low temperature, no thinking
    /// (spec §4.3 — triples are short; the cap is the contention lever).
    pub fn light_extraction() -> Self {
        Self {
            max_tokens: Some(128),
            temperature: Some(0.2),
            enable_thinking: false,
        }
    }
}

/// A local (on-device, no-egress) reasoning model. Synchronous: the real impl runs a local model
/// to completion on a worker thread; the stub is pure. All methods are deterministic for a given
/// input in the stub.
pub trait LocalReasoner: Send + Sync {
    /// Stable id of the backing model ("stub" until the real model lands).
    fn id(&self) -> &str;

    /// Free-form reasoning: run `system` + `user` and return the model's text.
    fn reason(&self, system: &str, user: &str) -> Result<String>;

    /// Structured reasoning: run `system` + `user` and return a JSON value. `json_schema` is the
    /// shape the output must conform to. NONE of the shipped impls hard-constrain the decode: the
    /// GGUF path (`MistralReasoner`) and the cloud path (`CloudReasoner`) both INSTRUCT the schema in
    /// the prompt and recover the object via [`extract_first_json`] (mistral.rs abandoned
    /// `Constraint::JsonSchema` — it overflowed the context on Bielik-11B); the stub likewise ignores
    /// the schema but still returns valid JSON through the same extractor.
    fn structured(&self, system: &str, user: &str, json_schema: &Value) -> Result<Value>;

    /// Structured reasoning WITH generation options (a token cap etc.). Default: IGNORE the options
    /// and delegate to [`structured`](Self::structured) — the Stub / Cloud / AFM reasoners have no
    /// local sampler to bound. [`mistral::MistralReasoner`] OVERRIDES this to honor the cap, the only
    /// real per-call bound on a runaway on-device generation (spec §4.3).
    fn structured_with(
        &self,
        system: &str,
        user: &str,
        json_schema: &Value,
        _opts: GenOptions,
    ) -> Result<Value> {
        self.structured(system, user, json_schema)
    }
}

/// Extract the first BALANCED top-level JSON object `{...}` from `text`.
///
/// Honors braces that appear inside JSON string literals (and `\"` escapes), so it is robust where
/// the `find('{')..=rfind('}')` slice in `graph.rs`/`timeline.rs` mis-cuts — e.g. when prose after
/// the object contains a stray `}`, when the model emits two objects, or when a string value itself
/// contains braces. Returns the matched substring, or `None` if no balanced object is present.
///
/// Byte-scanning is UTF-8-safe: the structural bytes `{` `}` `"` `\` are all ASCII, and UTF-8
/// continuation bytes (>= 0x80) never collide with them, so multibyte content between the braces is
/// passed through untouched and the returned slice always lands on char boundaries.
pub fn extract_first_json(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    let start = text.find('{')?;
    let mut depth: usize = 0;
    let mut in_str = false;
    let mut escaped = false;
    let mut i = start;
    while i < bytes.len() {
        let c = bytes[i];
        if in_str {
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                in_str = false;
            }
        } else {
            match c {
                b'"' => in_str = true,
                b'{' => depth += 1,
                b'}' => {
                    // `start` is at a '{', so depth is >= 1 here; the saturating guard is defensive.
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return Some(&text[start..=i]);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

/// Recover the first balanced JSON object from a (possibly noisy / fenced) reply and deserialize
/// it into `T`. The robust counterpart to the brittle slice in `graph.rs`/`timeline.rs`.
pub fn parse_first_json<T: serde::de::DeserializeOwned>(text: &str) -> Result<T> {
    let json = extract_first_json(text)
        .ok_or_else(|| AppError::Summarize("reasoner: no JSON object in reply".to_string()))?;
    serde_json::from_str(json)
        .map_err(|e| AppError::Summarize(format!("reasoner: invalid JSON ({e})")))
}

/// Deterministic, dependency-free stand-in for the real local reasoner (Phase 3b). Produces stable
/// output for a given input so the seam + the JSON-recovery path can be unit-tested without a model.
#[derive(Debug, Default, Clone, Copy)]
pub struct StubReasoner;

impl LocalReasoner for StubReasoner {
    fn id(&self) -> &str {
        "stub"
    }

    fn reason(&self, system: &str, user: &str) -> Result<String> {
        // Deterministic echo-shape: stable for a given (system, user) so tests can assert equality.
        Ok(format!(
            "[stub-reason] system={} chars user={} chars",
            system.chars().count(),
            user.chars().count()
        ))
    }

    fn structured(&self, _system: &str, user: &str, _json_schema: &Value) -> Result<Value> {
        // Build a deterministic object, then SIMULATE a real model emitting it wrapped in prose +
        // code fences, and recover it through the robust extractor — exercising the exact
        // noisy-reply → JSON path the grammar-constrained model output will take in Phase 3b.
        let obj = serde_json::json!({
            "stub": true,
            "echo": user,
            "chars": user.chars().count(),
        });
        let noisy = format!("Sure, here is the JSON:\n```json\n{obj}\n```\nHope that helps!");
        parse_first_json(&noisy)
    }
}

/// The CLOUD brain — the smartest reasoner, with NO local model. It implements [`LocalReasoner`]
/// (the "local" in the trait name is historical — it is the on-device *seam*, not a no-egress
/// promise) by delegating to the cloud LLM through the EXACT same provider factory the note summary
/// uses.
///
/// ## PRIVACY INVARIANT (load-bearing — audited by the lock-security reviewer)
/// `CloudReasoner` opens NO new egress class. Every call routes through
/// [`crate::summarize::make_provider`], so it inherits — byte-for-byte — the summary's egress
/// posture:
/// - the **fail-closed `cloud_egress_consented` gate** (`make_provider` returns
///   `AppError::Unavailable` for a cloud provider until the user has consented once);
/// - the **[`RedactingProvider`](crate::summarize::redact::RedactingProvider) firewall** (PII is
///   scrubbed before any content leaves the device, restored in the reply).
///
/// It holds NO `reqwest`/HTTP client and makes NO direct network call — it cannot bypass the gate
/// or the redactor. The brain input (a ≤2k-char transcript excerpt, per `orchestrate.rs`) is a
/// SUBSET of what the summary already sends (the full transcript + grounding corpus), so the brain
/// never widens egress. With consent OFF (the default) `reason`/`structured` return `Err` and
/// `orchestrate.rs` falls back to the deterministic floor — no content leaves.
///
/// GRACEFUL: any failure (no consent, no `claude` CLI, offline) is returned as `Err`, never a panic;
/// the caller already treats `Err` as "use the deterministic floor".
pub struct CloudReasoner {
    /// Stable id, e.g. `cloud:claude_code` — computed once so [`LocalReasoner::id`] can return a
    /// borrow.
    id: String,
    /// The SHARED live config handle — the SAME `Arc<Mutex<AppConfig>>` the settings/consent
    /// commands write (`AppState.config`). `build_provider` reads it FRESH on every call, so a
    /// consent grant/revocation or a provider switch applies to the very next provider call —
    /// even on a held instance mid-turn. Never snapshotted.
    config: Arc<Mutex<AppConfig>>,
    /// The model ROLE this reasoner serves — `build_provider` resolves the provider FOR THIS ROLE
    /// (with role keys absent that is exactly the legacy default provider, byte-identical).
    role: Role,
    /// TEST-ONLY injected provider. When `Some`, `build_provider` returns it instead of calling
    /// the factory, so the wiring/parse can be exercised with a mock — NO real Claude/network.
    /// `None` in every production path (the only constructors reachable in release are [`new`] /
    /// [`for_role`](Self::for_role)).
    provider_override: Option<Arc<dyn SummarizerProvider>>,
}

impl CloudReasoner {
    /// Build the cloud brain over the shared live config handle for the legacy default role
    /// (Notes — whose fallback is the default provider triple). Cheap + infallible: the provider
    /// (and thus the consent gate) is constructed lazily, per call, from the config AS IT IS THEN.
    pub fn new(config: Arc<Mutex<AppConfig>>) -> Self {
        Self::for_role(config, Role::Notes)
    }

    /// Build the cloud brain serving `role`. The id names the connection the role RESOLVES to
    /// (`cloud:<connection>`) — under the legacy fallback that is `provider_id`, unchanged.
    pub fn for_role(config: Arc<Mutex<AppConfig>>, role: Role) -> Self {
        let connection = config
            .lock()
            .map(|c| roles::provider_target(role, &c).connection)
            .unwrap_or_else(|p| roles::provider_target(role, &p.into_inner()).connection);
        Self {
            id: format!("cloud:{connection}"),
            config,
            role,
            provider_override: None,
        }
    }

    /// TEST-ONLY: build a CloudReasoner that delegates to an injected provider instead of the real
    /// factory, so `reason`/`structured` can be asserted without a network/CLI call. NOT
    /// compiled in non-test builds, so production can never inject a provider that skips the gate.
    #[cfg(test)]
    fn with_provider(config: AppConfig, provider: Arc<dyn SummarizerProvider>) -> Self {
        Self {
            id: format!("cloud:{}", config.provider_id),
            config: Arc::new(Mutex::new(config)),
            role: Role::Notes,
            provider_override: Some(provider),
        }
    }

    /// Resolve the provider for this call. THE egress seam: in production this is ALWAYS
    /// `provider_for(self.role, …)` (role resolution → the same consent gate + RedactingProvider
    /// as every factory build) over a FRESH read of the shared config — never a construction-time
    /// snapshot — so a consent grant unblocks, and a consent REVOCATION refuses, on the very next
    /// call (fail-closed both directions, no restart). A poisoned config mutex makes the consent
    /// state unknowable, so it refuses too. A test override short-circuits only under `#[cfg(test)]`.
    fn build_provider(&self) -> Result<Arc<dyn SummarizerProvider>> {
        if let Some(p) = &self.provider_override {
            return Ok(p.clone());
        }
        let cfg = self
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?
            .clone();
        crate::summarize::provider_for(self.role, &cfg)
    }
}

/// Drive an `async` `complete` to completion from a SYNCHRONOUS trait method without panicking,
/// regardless of caller context. The reasoner is called synchronously from WITHIN the async note
/// pipeline (`pipeline.rs` → `orchestrate.rs`), so a plain `Handle::block_on`/new-runtime-`block_on`
/// here would panic ("Cannot start a runtime from within a runtime"). We instead run the provider
/// call on a DEDICATED scoped OS thread with its own current-thread runtime (`enable_all` covers the
/// IO + time drivers the `claude_code` process / `anthropic` reqwest paths need). The future never
/// crosses a thread boundary, only the `Result<String>` does.
fn block_on_complete(
    provider: &Arc<dyn SummarizerProvider>,
    system: &str,
    user: &str,
) -> Result<String> {
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| {
                        AppError::Summarize(format!("cloud reasoner: runtime build failed: {e}"))
                    })?;
                rt.block_on(provider.complete(system, user))
            })
            .join()
            .map_err(|_| AppError::Summarize("cloud reasoner: worker thread panicked".into()))?
    })
}

impl LocalReasoner for CloudReasoner {
    fn id(&self) -> &str {
        &self.id
    }

    fn reason(&self, system: &str, user: &str) -> Result<String> {
        // EGRESS SEAM: provider built via `make_provider` → consent gate + RedactingProvider.
        let provider = self.build_provider()?;
        block_on_complete(&provider, system, user)
    }

    fn structured(&self, system: &str, user: &str, json_schema: &Value) -> Result<Value> {
        // Cloud providers have no native constrained decode — embed the schema as an instruction and
        // recover the object from the (possibly fenced/prose-wrapped) reply via the robust extractor.
        let schema = serde_json::to_string(json_schema)
            .map_err(|e| AppError::Summarize(format!("cloud reasoner: schema serialize: {e}")))?;
        let augmented_system = format!(
            "{system}\n\nRespond with ONLY a JSON object conforming to this schema: {schema}. \
             No prose, no fences."
        );
        let text = self.reason(&augmented_system, user)?;
        parse_first_json(&text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_object_from_surrounding_prose() {
        let got = extract_first_json("blah blah {\"a\":1} trailing").unwrap();
        assert_eq!(got, "{\"a\":1}");
    }

    #[test]
    fn handles_braces_inside_strings() {
        // A naive rfind('}') would over-slice here; the string-aware scanner stops at the real end.
        let got = extract_first_json("noise {\"text\":\"a } b { c\"} more } junk").unwrap();
        assert_eq!(got, "{\"text\":\"a } b { c\"}");
    }

    #[test]
    fn handles_nested_objects() {
        let got = extract_first_json("x {\"o\":{\"i\":2}} y").unwrap();
        assert_eq!(got, "{\"o\":{\"i\":2}}");
    }

    #[test]
    fn returns_first_of_two_objects() {
        let got = extract_first_json("{\"first\":1} {\"second\":2}").unwrap();
        assert_eq!(got, "{\"first\":1}");
    }

    #[test]
    fn respects_escaped_quote_inside_string() {
        let got = extract_first_json("{\"q\":\"he said \\\"hi}\\\"\"}").unwrap();
        assert_eq!(got, "{\"q\":\"he said \\\"hi}\\\"\"}");
    }

    #[test]
    fn none_when_no_json() {
        assert!(extract_first_json("no json here").is_none());
        assert!(extract_first_json("unbalanced {\"a\":1").is_none());
    }

    #[test]
    fn parse_first_json_deserializes() {
        #[derive(serde::Deserialize)]
        struct P {
            a: i32,
        }
        let p: P = parse_first_json("prefix {\"a\":7} suffix").unwrap();
        assert_eq!(p.a, 7);
        assert!(parse_first_json::<P>("no json").is_err());
    }

    #[test]
    fn stub_reason_is_deterministic() {
        let r = StubReasoner;
        assert_eq!(r.id(), "stub");
        let a = r.reason("sys", "hello").unwrap();
        let b = r.reason("sys", "hello").unwrap();
        assert_eq!(a, b);
        // Different input → different output (the length fields move).
        assert_ne!(r.reason("sys", "hello").unwrap(), r.reason("sys", "hi").unwrap());
    }

    #[test]
    fn stub_structured_returns_valid_json_via_extractor() {
        let r = StubReasoner;
        let schema = serde_json::json!({ "type": "object" });
        let v = r.structured("sys", "find Atlas", &schema).unwrap();
        assert_eq!(v["stub"], serde_json::json!(true));
        assert_eq!(v["echo"], serde_json::json!("find Atlas"));
        assert_eq!(v["chars"], serde_json::json!("find Atlas".chars().count()));
    }

    #[test]
    fn stub_structured_survives_braces_in_user_text() {
        // The echoed user text contains braces; the extractor must still recover the whole object.
        let r = StubReasoner;
        let schema = serde_json::json!({});
        let v = r.structured("sys", "json like {a:1} please", &schema).unwrap();
        assert_eq!(v["echo"], serde_json::json!("json like {a:1} please"));
    }

    fn tmp_file(tag: &str, contents: &[u8]) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "murmur-brain-{tag}-{}-{}.gguf",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&p, contents).unwrap();
        p
    }

    #[test]
    fn brain_model_path_prefers_existing_configured_file() {
        let f = tmp_file("configured", b"GGUF");
        let got = brain_model_path(Some(&f)).unwrap();
        assert_eq!(got.as_deref(), Some(f.as_path()));
        let _ = std::fs::remove_file(&f);
    }

    #[test]
    fn brain_model_path_none_when_configured_missing() {
        // A configured custom path that does not exist must NOT be returned — the graceful
        // "use the stub" signal. (Custom-override layer only; no registry/default fallback here.)
        let missing = std::env::temp_dir().join("murmur-brain-does-not-exist-xyz.gguf");
        let _ = std::fs::remove_file(&missing);
        assert!(brain_model_path(Some(&missing)).unwrap().is_none());
        assert!(brain_model_path(None).unwrap().is_none());
    }

    #[test]
    fn sha256_hex_sync_is_stable_64hex_and_content_sensitive() {
        // The macOS-native shasum path (no new crate): 64 hex chars, deterministic per content.
        let a = tmp_file("sha-a", b"murmur brain");
        let a2 = tmp_file("sha-a2", b"murmur brain");
        let b = tmp_file("sha-b", b"different bytes entirely");
        let ha = sha256_hex_sync(&a).unwrap();
        let ha2 = sha256_hex_sync(&a2).unwrap();
        let hb = sha256_hex_sync(&b).unwrap();
        assert_eq!(ha.len(), 64);
        assert!(ha.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(ha, ha2, "identical content ⇒ identical hash");
        assert_ne!(ha, hb, "different content ⇒ different hash");
        for f in [a, a2, b] {
            let _ = std::fs::remove_file(f);
        }
    }

    #[test]
    fn registry_lookup_by_id() {
        // Every advertised id resolves; an unknown id is None.
        for m in BRAIN_MODELS {
            assert_eq!(brain_model_by_id(m.id).map(|x| x.id), Some(m.id));
        }
        let bielik = brain_model_by_id("bielik-11b-v3").unwrap();
        assert_eq!(bielik.arch, "llama");
        assert_eq!(bielik.min_ram_gb, 10);
        assert!(bielik.languages.contains(&"pl"));
        assert!(brain_model_by_id("does-not-exist").is_none());
        assert!(brain_model_by_id("").is_none());
    }

    #[test]
    fn registry_archs_are_mistralrs_safe() {
        // Guardrail: only the arch keys mistral.rs parses today — never qwen35 / qwen3vl.
        for m in BRAIN_MODELS {
            assert!(
                matches!(m.arch, "llama" | "qwen2" | "qwen3"),
                "unsafe arch in registry: {}",
                m.arch
            );
        }
    }

    #[test]
    fn resolve_brain_model_prefers_custom_path_then_selected_id() {
        // (a) custom path override wins.
        let f = tmp_file("resolve-custom", b"GGUF");
        assert_eq!(
            resolve_brain_model(Some(&f), Some("qwen2.5-3b")).unwrap().as_deref(),
            Some(f.as_path())
        );
        let _ = std::fs::remove_file(&f);

        // (b) selected id resolves to the registry file IN the models dir when present. We place the
        // qwen3-1.7b file in the real models dir, assert it resolves, then clean it up.
        let model = brain_model_by_id("qwen3-1.7b").unwrap();
        let dir = crate::transcribe::models_dir().unwrap();
        let dest = dir.join(model.filename);
        let pre_existing = dest.is_file();
        if !pre_existing {
            std::fs::write(&dest, b"GGUF").unwrap();
        }
        assert_eq!(
            resolve_brain_model(None, Some("qwen3-1.7b")).unwrap().as_deref(),
            Some(dest.as_path())
        );
        if !pre_existing {
            let _ = std::fs::remove_file(&dest);
        }

        // (c) no custom path + no selection ⇒ None (stub).
        assert!(resolve_brain_model(None, None).unwrap().is_none());
        // unknown id resolves to None (no models dir hit for it).
        assert!(resolve_brain_model(None, Some("nope-bad-id")).unwrap().is_none());
    }

    #[test]
    fn brain_model_dtos_mark_downloaded_fits_ram_and_selected() {
        // Fake models dir holding ONLY the small model's file.
        let dir = std::env::temp_dir().join(format!(
            "murmur-brain-dtos-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let small = brain_model_by_id("qwen3-1.7b").unwrap();
        std::fs::write(dir.join(small.filename), b"GGUF").unwrap();

        // RAM threshold of 10 GB: bielik-11b(10) fits, qwen3-14b(14) does NOT, qwen3-1.7b(4) fits.
        let dtos = brain_model_dtos(&dir, Some(10), Some("qwen3-1.7b"));
        let by = |id: &str| dtos.iter().find(|d| d.id == id).unwrap();
        assert!(by("qwen3-1.7b").downloaded);
        assert!(!by("bielik-11b-v3").downloaded);
        assert!(by("bielik-11b-v3").fits_ram);
        assert!(!by("qwen3-14b").fits_ram);
        assert!(by("qwen3-1.7b").fits_ram);
        // class tag surfaces to the picker.
        assert_eq!(by("qwen3-1.7b").class, ModelClass::Light);
        assert_eq!(by("qwen3-14b").class, ModelClass::Heavy);
        // selection mirrors the passed id.
        assert!(by("qwen3-1.7b").selected);
        assert!(!by("bielik-11b-v3").selected);

        // Unknown RAM ⇒ everything fits (never hide behind a probe failure).
        let unknown = brain_model_dtos(&dir, None, None);
        assert!(unknown.iter().all(|d| d.fits_ram));
        assert!(unknown.iter().all(|d| !d.selected));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn retired_model_is_hidden_but_still_resolves_on_disk() {
        // The retired id is GONE from the active registry (un-selectable fresh) …
        assert!(brain_model_by_id("qwen2.5-3b").is_none());
        // … but still recognized as retired, pointing at an ACTIVE replacement.
        let retired = retired_model_by_id("qwen2.5-3b").expect("qwen2.5-3b is a retired model");
        assert_eq!(retired.replacement_id, "qwen3-1.7b");
        assert!(brain_model_by_id(retired.replacement_id).is_some());

        // Installed-base migration: a persisted selection of the retired model STILL resolves while
        // its GGUF is on disk — no silent downgrade to the stub. Place the file, assert, clean up.
        let dir = crate::transcribe::models_dir().unwrap();
        let dest = dir.join(retired.filename);
        let pre_existing = dest.is_file();
        if !pre_existing {
            std::fs::write(&dest, b"GGUF").unwrap();
        }
        assert_eq!(
            resolve_brain_model(None, Some("qwen2.5-3b")).unwrap().as_deref(),
            Some(dest.as_path())
        );
        // The nudge fires and reports the file present.
        let nudge = retired_model_nudge(Some("qwen2.5-3b"), &dir).expect("nudge for retired");
        assert_eq!(nudge.replacement_id, "qwen3-1.7b");
        assert!(nudge.file_on_disk);
        if !pre_existing {
            let _ = std::fs::remove_file(&dest);
        }
        // An active or absent selection produces no nudge.
        assert!(retired_model_nudge(Some("qwen3-1.7b"), &dir).is_none());
        assert!(retired_model_nudge(None, &dir).is_none());
    }

    #[test]
    fn residency_guard_counts_weights_kv_and_overhead() {
        let light = brain_model_by_id("qwen3-1.7b").unwrap();
        let heavy = brain_model_by_id("qwen3-4b-instruct-2507").unwrap();
        // light(~1.1→2) + heavy(~2.5→3) weights + KV(2) + overhead(4) ≈ 11 GB.
        let g = combined_residency_gb(&[light, heavy], CALL_OVERHEAD_GB);
        assert!((10..=13).contains(&g), "combined residency ~11 GB, got {g}");
        // 8 GB can't co-reside light+heavy during a call; 16 GB fits; unknown RAM never blocks.
        assert!(!residency_fits(&[light, heavy], Some(8), CALL_OVERHEAD_GB));
        assert!(residency_fits(&[light, heavy], Some(16), CALL_OVERHEAD_GB));
        assert!(residency_fits(&[light, heavy], None, CALL_OVERHEAD_GB));
        // Light alone (the Brain-Live-during-recording set) fits 8 GB.
        assert!(residency_fits(&[light], Some(8), CALL_OVERHEAD_GB));
    }

    #[test]
    fn registry_offers_both_classes_and_no_retired_id() {
        assert!(BRAIN_MODELS.iter().any(|m| m.class == ModelClass::Light));
        assert!(BRAIN_MODELS.iter().any(|m| m.class == ModelClass::Heavy));
        // The recommended defaults are present and correctly classed.
        assert_eq!(brain_model_by_id("qwen3-1.7b").unwrap().class, ModelClass::Light);
        assert_eq!(
            brain_model_by_id("qwen3-4b-instruct-2507").unwrap().class,
            ModelClass::Heavy
        );
        // The non-commercial id must NEVER be back in the active registry.
        assert!(!BRAIN_MODELS.iter().any(|m| m.id == "qwen2.5-3b"));
    }

    /// The LOCAL backend's graceful-degradation contract: with NO usable model (a configured path
    /// that doesn't exist, and — on a clean machine — no default model) `active_reasoner` returns the
    /// StubReasoner. mistralrs is always compiled now, so selection keys ONLY on model presence —
    /// no GGUF ⇒ stub. (`brain_backend` is pinned to `Local` here — the default is now `Cloud`.) This
    /// is the headless proof of the swap wiring's fallback.
    #[test]
    fn active_reasoner_falls_back_to_stub_without_model() {
        // No custom path (a non-existent one) AND no selected brain_model_id ⇒ nothing resolves, so
        // the always-compiled mistralrs backend returns the stub. With no selection there is no
        // registry file to probe, so this holds regardless of what GGUFs live in the shared models dir.
        let cfg = AppConfig {
            brain_backend: BrainBackend::Local,
            brain_model_path: Some(
                std::env::temp_dir()
                    .join("murmur-brain-absent-model-xyz.gguf")
                    .to_string_lossy()
                    .to_string(),
            ),
            brain_model_id: None,
            ..Default::default()
        };
        assert_eq!(active_reasoner(&cfg).id(), "stub");
    }

    // ---- CloudReasoner (the cloud brain) ----------------------------------------------------

    /// A mock `SummarizerProvider` that records the `complete` system prompt and returns a canned
    /// reply — so the CloudReasoner wiring/parse is exercised WITHOUT any real Claude/network call.
    struct MockProvider {
        reply: String,
        last_system: std::sync::Mutex<Option<String>>,
    }
    impl MockProvider {
        fn new(reply: &str) -> Self {
            Self {
                reply: reply.to_string(),
                last_system: std::sync::Mutex::new(None),
            }
        }
    }
    #[async_trait::async_trait]
    impl SummarizerProvider for MockProvider {
        fn id(&self) -> &str {
            "mock"
        }
        async fn availability(&self) -> crate::summarize::provider::Availability {
            crate::summarize::provider::Availability::Available
        }
        async fn summarize(
            &self,
            _req: &crate::summarize::provider::SummarizeRequest,
        ) -> Result<String> {
            Ok(self.reply.clone())
        }
        async fn complete(&self, system: &str, _user: &str) -> Result<String> {
            *self.last_system.lock().unwrap() = Some(system.to_string());
            Ok(self.reply.clone())
        }
    }

    /// Wrap a config in the shared-handle shape `AppState.config` uses, so tests can mutate it
    /// after construction exactly like the settings/consent commands do.
    fn shared(cfg: AppConfig) -> Arc<Mutex<AppConfig>> {
        Arc::new(Mutex::new(cfg))
    }

    #[test]
    fn cloud_reasoner_id_is_namespaced_by_provider() {
        let cfg = AppConfig {
            provider_id: "claude_code".to_string(),
            ..Default::default()
        };
        assert_eq!(CloudReasoner::new(shared(cfg)).id(), "cloud:claude_code");
    }

    #[test]
    fn cloud_reasoner_reason_returns_provider_text_via_injected_provider() {
        // The injected mock stands in for `make_provider`'s output — proving `reason` returns the
        // provider's `complete` text verbatim (the sync→async bridge works, no network).
        let provider = Arc::new(MockProvider::new("the cloud answer"));
        let r = CloudReasoner::with_provider(AppConfig::default(), provider);
        assert_eq!(r.reason("sys", "user").unwrap(), "the cloud answer");
    }

    #[test]
    fn cloud_reasoner_structured_builds_json_instruction_and_parses_reply() {
        // The provider replies with a FENCED, prose-wrapped JSON object (what a real cloud model
        // emits); `structured` must (a) embed the schema as an instruction and (b) recover the
        // object via `parse_first_json`.
        let reply = "Sure! Here you go:\n```json\n{\"entities\":[\"Atlas\"],\"n\":2}\n```\nDone.";
        let provider = Arc::new(MockProvider::new(reply));
        let r = CloudReasoner::with_provider(AppConfig::default(), provider.clone());

        let schema = serde_json::json!({ "type": "object", "properties": { "n": { "type": "number" } } });
        let v = r.structured("plan retrieval", "transcript excerpt", &schema).unwrap();

        // (b) parsed JSON value from the noisy reply.
        assert_eq!(v["entities"], serde_json::json!(["Atlas"]));
        assert_eq!(v["n"], serde_json::json!(2));

        // (a) the system prompt actually carried the schema-as-instruction.
        let sent = provider.last_system.lock().unwrap().clone().unwrap();
        assert!(sent.contains("conforming to this schema"), "schema instruction embedded: {sent}");
        assert!(sent.contains("\"properties\""), "the JSON schema itself is embedded: {sent}");
        assert!(sent.contains("No prose, no fences"));
    }

    #[test]
    fn cloud_reasoner_propagates_provider_error_as_err() {
        // A provider whose `complete` errors must surface as Err (orchestrate.rs then floors).
        struct ErrProvider;
        #[async_trait::async_trait]
        impl SummarizerProvider for ErrProvider {
            fn id(&self) -> &str {
                "err"
            }
            async fn availability(&self) -> crate::summarize::provider::Availability {
                crate::summarize::provider::Availability::Available
            }
            async fn summarize(
                &self,
                _req: &crate::summarize::provider::SummarizeRequest,
            ) -> Result<String> {
                Err(AppError::Summarize("boom".into()))
            }
            async fn complete(&self, _system: &str, _user: &str) -> Result<String> {
                Err(AppError::Summarize("boom".into()))
            }
        }
        let r = CloudReasoner::with_provider(AppConfig::default(), Arc::new(ErrProvider));
        assert!(r.reason("s", "u").is_err());
        assert!(r.structured("s", "u", &serde_json::json!({})).is_err());
    }

    /// EGRESS POSTURE (by construction): the production `reason`/`structured` path builds its
    /// provider through `make_provider`, which is fail-closed on `cloud_egress_consented`. A
    /// CloudReasoner with NO injected provider + consent OFF (the default) therefore returns the
    /// SAME `AppError::Unavailable` the summary would — proving it routes through the consent gate
    /// and opens no side channel.
    #[test]
    fn cloud_reasoner_without_consent_is_refused_by_the_same_gate() {
        let cfg = AppConfig {
            provider_id: "claude_code".to_string(),
            cloud_egress_consented: false,
            ..Default::default()
        };
        let r = CloudReasoner::new(shared(cfg));
        match r.reason("s", "u") {
            Err(AppError::Unavailable(_)) => {}
            other => panic!("expected the make_provider consent gate to refuse, got {other:?}"),
        }
    }

    // ---- config freshness: consent / provider / backend changes apply WITHOUT a restart --------
    //
    // These tests discriminate "refused by the consent gate" from "got PAST the gate" by the error
    // VARIANT `make_provider` returns for a deliberately-unknown provider id: the fail-closed gate
    // fires FIRST (`Unavailable("cloud egress not consented …")`); past it, the unknown id yields
    // `InvalidArg("unknown provider id: …")`. No network, no keychain, no CLI is ever touched.

    /// An unknown provider id: classified as CLOUD by `egress_is_cloud` (fail-safe default), so it
    /// hits the consent gate first, and can never construct a real provider after it.
    const BOGUS_PROVIDER: &str = "no_such_provider_for_gate_probe";

    fn gate_probe_cfg(consented: bool) -> AppConfig {
        AppConfig {
            brain_backend: BrainBackend::Cloud,
            provider_id: BOGUS_PROVIDER.to_string(),
            cloud_egress_consented: consented,
            ..Default::default()
        }
    }

    /// Granting cloud consent mid-session must unblock the VERY NEXT call on an already-held
    /// CloudReasoner — no app restart, no reconstruction. Before the grant the consent gate
    /// refuses (`Unavailable`); after flipping the SAME shared config cache the consent command
    /// writes, the call must get PAST the gate (here: `InvalidArg` for the bogus id).
    #[test]
    fn cloud_reasoner_sees_consent_granted_after_construction() {
        let cfg = shared(gate_probe_cfg(false));
        let r = CloudReasoner::new(Arc::clone(&cfg));
        match r.reason("s", "u") {
            Err(AppError::Unavailable(_)) => {}
            other => panic!("pre-grant call must be refused by the consent gate, got {other:?}"),
        }

        cfg.lock().unwrap().cloud_egress_consented = true;
        match r.reason("s", "u") {
            Err(AppError::InvalidArg(_)) => {} // past the gate — the unknown id is now the error
            other => panic!(
                "post-grant call must reach the provider path without a rebuild, got {other:?}"
            ),
        }
    }

    /// PRIVACY-CRITICAL direction: REVOKING consent must make the very next cloud-bound call on the
    /// SAME held instance refuse (`Unavailable`) — a stale construction-time snapshot would keep
    /// egressing after the user withdrew consent.
    #[test]
    fn cloud_reasoner_refuses_next_call_after_consent_revoked() {
        let cfg = shared(gate_probe_cfg(true));
        let r = CloudReasoner::new(Arc::clone(&cfg));
        match r.reason("s", "u") {
            Err(AppError::InvalidArg(_)) => {} // consented: past the gate
            other => panic!("pre-revoke call must pass the consent gate, got {other:?}"),
        }

        cfg.lock().unwrap().cloud_egress_consented = false;
        match r.reason("s", "u") {
            Err(AppError::Unavailable(_)) => {} // fail-closed: the revocation applies NOW
            other => panic!(
                "post-revoke call must be refused by the consent gate (fail-closed), got {other:?}"
            ),
        }
    }

    /// Flipping `brain_backend` mid-session redirects the NEXT `current()` dispatch — Off→Cloud→Off
    /// without rebuilding the cell (i.e. without an app restart).
    #[test]
    fn reasoner_cell_dispatches_backend_flips_mid_session() {
        let cfg = shared(AppConfig {
            brain_backend: BrainBackend::Off,
            ..Default::default()
        });
        let cell = ReasonerCell::new(Arc::clone(&cfg));
        assert_eq!(cell.current().id(), "stub", "Off backend dispatches the stub");

        cfg.lock().unwrap().brain_backend = BrainBackend::Cloud;
        let id = cell.current().id().to_string();
        assert!(
            id.starts_with("cloud:"),
            "flipping to Cloud must dispatch the cloud brain on the next call, got {id}"
        );

        cfg.lock().unwrap().brain_backend = BrainBackend::Off;
        assert_eq!(
            cell.current().id(),
            "stub",
            "flipping back to Off must dispatch the stub on the next call"
        );
    }

    /// Switching the summarizer provider mid-session re-targets the cloud brain on the NEXT call.
    #[test]
    fn reasoner_cell_sees_provider_switch_mid_session() {
        let cfg = shared(AppConfig {
            brain_backend: BrainBackend::Cloud,
            provider_id: "claude_code".to_string(),
            ..Default::default()
        });
        let cell = ReasonerCell::new(Arc::clone(&cfg));
        assert_eq!(cell.current().id(), "cloud:claude_code");

        cfg.lock().unwrap().provider_id = "anthropic".to_string();
        assert_eq!(
            cell.current().id(),
            "cloud:anthropic",
            "a provider switch must re-target the cloud brain without a restart"
        );
    }

    /// The no-reload constraint: under the LOCAL backend the resolved instance is CACHED across
    /// `current()` calls (a loaded GGUF is expensive) — and survives a Cloud excursion; only the
    /// resolved model path changing may rebuild it. Cloud dispatch mid-flip still works.
    #[test]
    fn reasoner_cell_caches_local_instance_across_calls_and_backend_flips() {
        let cfg = shared(AppConfig {
            brain_backend: BrainBackend::Local,
            brain_model_path: Some(
                std::env::temp_dir()
                    .join("murmur-cell-absent-model-xyz.gguf")
                    .to_string_lossy()
                    .to_string(),
            ),
            brain_model_id: None,
            ..Default::default()
        });
        let cell = ReasonerCell::new(Arc::clone(&cfg));
        let a = cell.current();
        let b = cell.current();
        assert_eq!(a.id(), "stub", "no GGUF resolves ⇒ the local dispatch is the stub");
        assert!(
            Arc::ptr_eq(&a, &b),
            "the local instance must be reused across calls, never rebuilt per call"
        );

        cfg.lock().unwrap().brain_backend = BrainBackend::Cloud;
        assert!(
            cell.current().id().starts_with("cloud:"),
            "the Local→Cloud flip must dispatch the cloud brain"
        );

        cfg.lock().unwrap().brain_backend = BrainBackend::Local;
        let c = cell.current();
        assert!(
            Arc::ptr_eq(&a, &c),
            "the cached local instance must survive a backend excursion (same resolved path)"
        );
    }

    // ---- model roles: current_for / CloudReasoner::for_role ----------------------------------

    /// FALLBACK IDENTITY at the dispatch level: with role keys absent, `current_for(role)`
    /// dispatches EXACTLY like the legacy `current()` — the `brain_backend` mapping — for EVERY
    /// role, Notes included (a Notes reasoner resolved from `provider_id` instead would
    /// cloud-dispatch an Off/Local install's pre-analysis: an egress change).
    #[test]
    fn current_for_matches_legacy_dispatch_under_fallback() {
        use crate::summarize::roles::Role;
        for backend in [BrainBackend::Cloud, BrainBackend::Local, BrainBackend::Off] {
            let cfg = shared(AppConfig {
                brain_backend: backend,
                // Local resolves nothing on a clean machine (absent custom path, no selection)
                // ⇒ the stub — same shape as the legacy Local tests above.
                brain_model_path: Some(
                    std::env::temp_dir()
                        .join("murmur-rolecell-absent-model-xyz.gguf")
                        .to_string_lossy()
                        .to_string(),
                ),
                brain_model_id: None,
                ..Default::default()
            });
            let cell = ReasonerCell::new(Arc::clone(&cfg));
            let legacy = cell.current().id().to_string();
            for role in [Role::Notes, Role::Ask, Role::Live] {
                assert_eq!(
                    cell.current_for(role).id(),
                    legacy,
                    "{backend:?}/{role:?} must dispatch identically to the legacy current()"
                );
            }
            match backend {
                BrainBackend::Cloud => assert!(legacy.starts_with("cloud:"), "got {legacy}"),
                _ => assert_eq!(legacy, "stub"),
            }
        }
    }

    /// EXPLICIT role keys re-route ONLY their role: Ask→off dispatches the stub while Notes (no
    /// key) keeps the legacy Cloud dispatch — per-role steering without a restart.
    #[test]
    fn current_for_dispatches_explicit_role_targets_independently() {
        use crate::summarize::roles::Role;
        let cfg = shared(AppConfig {
            brain_backend: BrainBackend::Cloud,
            provider_id: "claude_code".to_string(),
            role_ask_connection: "off".to_string(),
            ..Default::default()
        });
        let cell = ReasonerCell::new(Arc::clone(&cfg));
        assert_eq!(cell.current_for(Role::Ask).id(), "stub", "explicit Ask→off is the stub");
        assert_eq!(cell.current_for(Role::Notes).id(), "cloud:claude_code");
        assert_eq!(cell.current_for(Role::Live).id(), "cloud:claude_code");

        // Clearing the key mid-session restores the legacy dispatch on the next call.
        cfg.lock().unwrap().role_ask_connection = String::new();
        assert_eq!(cell.current_for(Role::Ask).id(), "cloud:claude_code");
    }

    /// A role key pointing at a DIFFERENT cloud connection names it in the reasoner id (and
    /// `build_provider` resolves THAT role — proven by the gate-probe test below).
    #[test]
    fn cloud_reasoner_for_role_names_the_resolved_connection() {
        use crate::summarize::roles::Role;
        let cfg = AppConfig {
            provider_id: "claude_code".to_string(),
            role_ask_connection: "anthropic".to_string(),
            ..Default::default()
        };
        let r = CloudReasoner::for_role(shared(cfg), Role::Ask);
        assert_eq!(r.id(), "cloud:anthropic");
    }

    /// EGRESS POSTURE per role (by construction): a role-keyed CloudReasoner builds its provider
    /// through `provider_for(role)` → the SAME fail-closed consent gate. Discriminated exactly
    /// like the legacy gate probes: no consent ⇒ `Unavailable` (the gate), consent ⇒ `InvalidArg`
    /// (past the gate, the bogus connection id fails construction).
    #[test]
    fn cloud_reasoner_role_target_rides_the_same_consent_gate() {
        use crate::summarize::roles::Role;
        let cfg = shared(AppConfig {
            role_live_connection: BOGUS_PROVIDER.to_string(),
            cloud_egress_consented: false,
            ..Default::default()
        });
        let r = CloudReasoner::for_role(Arc::clone(&cfg), Role::Live);
        match r.reason("s", "u") {
            Err(AppError::Unavailable(_)) => {}
            other => panic!("role-keyed cloud call must be consent-gated, got {other:?}"),
        }
        cfg.lock().unwrap().cloud_egress_consented = true;
        match r.reason("s", "u") {
            Err(AppError::InvalidArg(_)) => {} // past the gate — the bogus id is now the error
            other => panic!("consented role-keyed call must reach the factory, got {other:?}"),
        }

        // REVOKE mid-session through the REAL mutator (`revoke_cloud_egress`, persisted): the
        // provider factory reads the live config per call, so the very NEXT call is refused
        // fail-closed again — no restart, no cached grant.
        let p = crate::storage::db::unique_temp_path("meetnotes-reason-revoke-test", "sqlite");
        let db = crate::storage::Db::open_with_key(
            &p,
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )
        .unwrap();
        cfg.lock().unwrap().revoke_cloud_egress(&db).unwrap();
        match r.reason("s", "u") {
            Err(AppError::Unavailable(_)) => {} // back behind the gate
            other => panic!("revoked role-keyed call must be consent-gated again, got {other:?}"),
        }
    }

    // ---- active_reasoner backend selection --------------------------------------------------

    #[test]
    fn active_reasoner_cloud_backend_yields_cloud_reasoner() {
        let cfg = AppConfig {
            brain_backend: BrainBackend::Cloud,
            provider_id: "claude_code".to_string(),
            ..Default::default()
        };
        assert_eq!(active_reasoner(&cfg).id(), "cloud:claude_code");
    }

    #[test]
    fn active_reasoner_off_backend_yields_stub() {
        let cfg = AppConfig {
            brain_backend: BrainBackend::Off,
            ..Default::default()
        };
        assert_eq!(active_reasoner(&cfg).id(), "stub");
    }

    #[test]
    fn active_reasoner_local_backend_without_model_yields_stub() {
        // Local backend with a configured path that does not exist AND no selected model id ⇒
        // nothing resolves, so the always-compiled mistralrs backend returns the stub.
        let cfg = AppConfig {
            brain_backend: BrainBackend::Local,
            brain_model_path: Some(
                std::env::temp_dir()
                    .join("murmur-cloud-absent-model-xyz.gguf")
                    .to_string_lossy()
                    .to_string(),
            ),
            brain_model_id: None,
            ..Default::default()
        };
        assert_eq!(active_reasoner(&cfg).id(), "stub");
    }

    /// Default config (the user's choice) selects the Cloud brain.
    #[test]
    fn active_reasoner_default_is_cloud() {
        let id = active_reasoner(&AppConfig::default()).id().to_string();
        assert!(id.starts_with("cloud:"), "default brain must be cloud, got {id}");
    }

    // ---- WS2: AppleFoundation dispatch + anti-egress ordering --------------------------------

    /// GRACEFUL FALLBACK: the AppleFoundation backend with NO sidecar (this CLT-only machine)
    /// resolves to the deterministic stub — byte-identical to Off, zero egress, no panic. Mirrors
    /// `active_reasoner_local_backend_without_model_yields_stub`. The env override is force-unset
    /// under the shared lock so a concurrent spawn test can't leak a fixture sidecar into this probe.
    #[test]
    fn active_reasoner_apple_backend_without_sidecar_yields_stub() {
        let _g = crate::reason::afm::AFM_ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("MURMUR_AFM_SIDECAR");
        let cfg = AppConfig {
            brain_backend: BrainBackend::AppleFoundation,
            ..Default::default()
        };
        assert_eq!(active_reasoner(&cfg).id(), "stub");
    }

    /// ANTI-EGRESS ORDERING regression (the load-bearing `CONN_AFM`-before-`_` arm): a mid-session
    /// flip to AppleFoundation must dispatch the on-device path — which, with the sidecar absent,
    /// is the stub — and must NEVER fall through to the catch-all CloudReasoner (a `cloud:` id would
    /// mean an on-device backend silently egressed). RED if the `CONN_AFM` arm is removed: the
    /// `apple` target would hit `_ => CloudReasoner` and this assertion would see `cloud:`.
    #[test]
    fn reasoner_cell_apple_backend_dispatches_stub_never_cloud() {
        use crate::summarize::roles::Role;
        let _g = crate::reason::afm::AFM_ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("MURMUR_AFM_SIDECAR");
        let cfg = shared(AppConfig {
            brain_backend: BrainBackend::Off,
            ..Default::default()
        });
        let cell = ReasonerCell::new(Arc::clone(&cfg));
        assert_eq!(cell.current().id(), "stub", "Off dispatches the stub");

        cfg.lock().unwrap().brain_backend = BrainBackend::AppleFoundation;
        let id = cell.current().id().to_string();
        assert_eq!(id, "stub", "AFM without a sidecar dispatches the stub, got {id}");
        assert!(
            !id.starts_with("cloud:"),
            "AFM (on-device) must NEVER fall through to the cloud reasoner (anti-egress), got {id}"
        );

        // Per-role explicit `apple` key routes the same on-device path (also stub-when-absent),
        // never cloud — proving the CONN_AFM arm covers the explicit-target path too.
        cfg.lock().unwrap().brain_backend = BrainBackend::Cloud;
        cfg.lock().unwrap().role_ask_connection = "apple".to_string();
        let ask_id = cell.current_for(Role::Ask).id().to_string();
        assert_eq!(ask_id, "stub", "explicit Ask→apple dispatches on-device (stub when absent)");
        // Notes (no key) keeps the legacy Cloud dispatch — the AFM arm didn't hijack other roles.
        assert!(cell.current_for(Role::Notes).id().starts_with("cloud:"));
    }

    /// P1 INVARIANT (spec §2.1 / §3.4; adversarial review code-truth #1/#3 + product #1): the
    /// Brain-Live `light()` / `heavy()` engine handles are LOCAL-or-STUB and **NEVER** the cloud
    /// reasoner — even under a Cloud backend with cloud consent. A missing local model must degrade
    /// the feature to the stub (empty extraction), it must never silently egress. Deterministic
    /// regardless of what GGUFs happen to live in the shared models dir (asserts "not cloud").
    #[test]
    fn brain_live_class_handles_are_local_or_stub_never_cloud() {
        let cfg = shared(AppConfig {
            brain_live: true,
            brain_backend: BrainBackend::Cloud,
            cloud_egress_consented: true,
            // default class ids (qwen3-1.7b / qwen3-4b-instruct-2507) — absent on disk in CI ⇒ stub.
            ..Default::default()
        });
        let cell = ReasonerCell::new(Arc::clone(&cfg));
        for id in [cell.light().id().to_string(), cell.heavy().id().to_string()] {
            assert!(
                id == "stub" || id.starts_with("mistralrs:"),
                "class handle must be local-or-stub, got {id}"
            );
            assert!(
                !id.starts_with("cloud:"),
                "class handle must NEVER be a cloud reasoner (anti-egress), got {id}"
            );
        }
    }

    /// P2 (spec §3.4): fact / user-memory / pre-analysis extraction routes to the LOCAL light engine
    /// when Brain Live is ON (facts stop egressing) and to the cloud Notes reasoner when OFF (today's
    /// behavior). The ON path must NEVER be a cloud reasoner.
    #[test]
    fn extraction_reasoner_is_local_when_brain_live_and_cloud_otherwise() {
        // OFF (default) ⇒ the legacy Notes dispatch (cloud under the default backend).
        let off = shared(AppConfig {
            brain_backend: BrainBackend::Cloud,
            ..Default::default()
        });
        assert!(
            ReasonerCell::new(off).extraction_reasoner().id().starts_with("cloud:"),
            "Brain Live OFF keeps the legacy cloud Notes extraction"
        );
        // ON ⇒ the local light engine (local-or-stub), never cloud — even with cloud consent.
        let on = shared(AppConfig {
            brain_live: true,
            brain_backend: BrainBackend::Cloud,
            cloud_egress_consented: true,
            ..Default::default()
        });
        let id = ReasonerCell::new(on).extraction_reasoner().id().to_string();
        assert!(
            !id.starts_with("cloud:"),
            "Brain Live ON must route fact extraction LOCAL, never cloud (got {id})"
        );
    }
}
