//! Local on-device reasoning seam (Phase 3a — PROD-INERT).
//!
//! [`LocalReasoner`] is the trait the heavy local reasoner (mistral.rs with grammar-constrained
//! decoding, Phase 3b) will implement. This increment ships ONLY the seam plus a deterministic
//! [`StubReasoner`] and a robust JSON extractor — NO ML crate, NO model download. The real impl is
//! a one-line swap at the construction site; nothing here is wired into a production path yet
//! (`graph.rs`/`timeline.rs` keep their existing brittle parse until a later increment).
//!
//! The [`LocalReasoner::structured`] method is the load-bearing seam: it is where reliable
//! tool-call / NER-classification JSON comes from. A real impl constrains decoding to the schema;
//! the stub fakes it but still routes its output through [`extract_first_json`] so the
//! recover-JSON-from-noisy-text path is exercised and testable.

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use crate::error::{AppError, Result};
use crate::settings::AppConfig;

/// The REAL on-device reasoner (Phase B), compiled ONLY under `--features local-brain`. The default
/// build never pulls mistralrs, keeping the fast `cargo test --lib` loop intact.
#[cfg(feature = "local-brain")]
pub mod mistral;

/// A curated, mistral.rs-arch-SAFE on-device reasoning model the user can pick from. The app must
/// serve BOTH English and Polish, so the brain offers a CHOICE — not one hardcoded GGUF. Every entry
/// here is a `Q4_K_M` GGUF whose architecture mistral.rs parses today (`llama` / `qwen2` / `qwen3`,
/// NOT `qwen35` / `qwen3vl`). Static, compile-time data — no I/O, no allocation.
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
    /// Minimum system RAM (whole GB) the model realistically needs — drives RAM-gating in the picker.
    pub min_ram_gb: u32,
    /// Language tags the model serves (`pl`, `en`, `multi`).
    pub languages: &'static [&'static str],
    /// mistral.rs architecture key (`llama` / `qwen2` / `qwen3`) — all parse-safe.
    pub arch: &'static str,
}

/// The curated registry. Order is display order (Polish-native first, then multilingual, then small).
pub static BRAIN_MODELS: &[BrainModel] = &[
    BrainModel {
        id: "bielik-11b-v3",
        name: "Bielik 11B v3 (Polish-native)",
        filename: "Bielik-11B-v3.0-Instruct.Q4_K_M.gguf",
        url: "https://huggingface.co/speakleash/Bielik-11B-v3.0-Instruct-GGUF/resolve/main/Bielik-11B-v3.0-Instruct.Q4_K_M.gguf",
        approx_size_bytes: 7_215_545_057, // ~6.72 GB
        min_ram_gb: 10,
        languages: &["pl", "en"],
        arch: "llama",
    },
    BrainModel {
        id: "qwen3-14b",
        name: "Qwen3 14B (multilingual/English)",
        filename: "Qwen_Qwen3-14B-Q4_K_M.gguf",
        url: "https://huggingface.co/bartowski/Qwen_Qwen3-14B-GGUF/resolve/main/Qwen_Qwen3-14B-Q4_K_M.gguf",
        approx_size_bytes: 9_663_676_416, // ~9 GB
        min_ram_gb: 14,
        languages: &["en", "multi"],
        arch: "qwen3",
    },
    BrainModel {
        id: "qwen2.5-3b",
        name: "Qwen2.5 3B (small / low-RAM)",
        filename: "Qwen2.5-3B-Instruct-Q4_K_M.gguf",
        url: "https://huggingface.co/bartowski/Qwen2.5-3B-Instruct-GGUF/resolve/main/Qwen2.5-3B-Instruct-Q4_K_M.gguf",
        approx_size_bytes: 2_147_483_648, // ~2 GB
        min_ram_gb: 4,
        languages: &["en", "multi", "pl"],
        arch: "qwen2",
    },
];

/// Look up a registry entry by id. `None` for an unknown id (the caller rejects with `InvalidArg`).
pub fn brain_model_by_id(id: &str) -> Option<&'static BrainModel> {
    BRAIN_MODELS.iter().find(|m| m.id == id)
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
            downloaded: models_dir.join(m.filename).is_file(),
            fits_ram: total_ram_gb.map(|g| u64::from(m.min_ram_gb) <= g).unwrap_or(true),
            selected: selected_id == Some(m.id),
        })
        .collect()
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
    Ok(None)
}

/// Download `url` to `dest` atomically (`dest.part` → rename), invoking `on_progress(downloaded,
/// total)` as bytes arrive (total is `None` when the server omits `Content-Length`). INBOUND ONLY:
/// this fetches a model file and sends NO request body / NO meeting content (no egress). Streams via
/// `Response::chunk` (no extra stream-combinator dep). NO PII logged — model id / byte counts only.
pub async fn download_brain_model<F>(url: &str, dest: &Path, mut on_progress: F) -> Result<()>
where
    F: FnMut(u64, Option<u64>),
{
    use tokio::io::AsyncWriteExt;

    tracing::info!(target: "reason", file = %dest.display(), "downloading brain model");

    let mut resp = reqwest::get(url)
        .await
        .map_err(|e| AppError::Summarize(format!("brain model download request failed: {e}")))?;
    if !resp.status().is_success() {
        return Err(AppError::Summarize(format!(
            "brain model download HTTP {}",
            resp.status()
        )));
    }
    let total = resp.content_length();

    let part = dest.with_extension("part");
    let mut file = tokio::fs::File::create(&part)
        .await
        .map_err(|e| AppError::Summarize(format!("create brain model temp file: {e}")))?;
    let mut downloaded: u64 = 0;
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

    if downloaded == 0 {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(AppError::Summarize(
            "brain model download returned empty body".into(),
        ));
    }
    tokio::fs::rename(&part, dest)
        .await
        .map_err(|e| AppError::Summarize(format!("rename brain model file: {e}")))?;

    tracing::info!(target: "reason", file = %dest.display(), bytes = downloaded, "brain model ready");
    Ok(())
}

/// The single active reasoning backend, used wherever the app needs local on-device reasoning.
///
/// Graceful degradation, in priority order:
/// - the `local-brain` feature is ON **and** a GGUF is present at the resolved [`resolve_brain_model`]
///   → the real [`mistral::MistralReasoner`] (lazy: the model loads on first use, not here, so this
///   never blocks startup and never panics);
/// - otherwise (feature off, no model, or a path-resolution error) → the dependency-free
///   [`StubReasoner`]. The app works either way — just less smart without the model.
///
/// NEVER panics and NEVER blocks: a missing/failed model is logged (no PII) and falls back to stub.
pub fn active_reasoner(config: &AppConfig) -> Box<dyn LocalReasoner> {
    #[cfg(feature = "local-brain")]
    {
        let configured = config.brain_model_path.as_deref().map(Path::new);
        match resolve_brain_model(configured, config.brain_model_id.as_deref()) {
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
    }
    #[cfg(not(feature = "local-brain"))]
    {
        let _ = config; // model path is only consulted when the feature is compiled in.
    }
    Box::new(StubReasoner)
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
    /// shape the output must conform to — a real impl constrains decoding to it; the stub ignores
    /// the constraint but still returns valid JSON recovered via [`extract_first_json`].
    fn structured(&self, system: &str, user: &str, json_schema: &Value) -> Result<Value>;
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
        // qwen2.5-3b file in the real models dir, assert it resolves, then clean it up.
        let model = brain_model_by_id("qwen2.5-3b").unwrap();
        let dir = crate::transcribe::models_dir().unwrap();
        let dest = dir.join(model.filename);
        let pre_existing = dest.is_file();
        if !pre_existing {
            std::fs::write(&dest, b"GGUF").unwrap();
        }
        assert_eq!(
            resolve_brain_model(None, Some("qwen2.5-3b")).unwrap().as_deref(),
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
        let small = brain_model_by_id("qwen2.5-3b").unwrap();
        std::fs::write(dir.join(small.filename), b"GGUF").unwrap();

        // RAM threshold of 10 GB: bielik(10) fits, qwen3-14b(14) does NOT, qwen2.5-3b(4) fits.
        let dtos = brain_model_dtos(&dir, Some(10), Some("qwen2.5-3b"));
        let by = |id: &str| dtos.iter().find(|d| d.id == id).unwrap();
        assert!(by("qwen2.5-3b").downloaded);
        assert!(!by("bielik-11b-v3").downloaded);
        assert!(by("bielik-11b-v3").fits_ram);
        assert!(!by("qwen3-14b").fits_ram);
        assert!(by("qwen2.5-3b").fits_ram);
        // selection mirrors the passed id.
        assert!(by("qwen2.5-3b").selected);
        assert!(!by("bielik-11b-v3").selected);

        // Unknown RAM ⇒ everything fits (never hide behind a probe failure).
        let unknown = brain_model_dtos(&dir, None, None);
        assert!(unknown.iter().all(|d| d.fits_ram));
        assert!(unknown.iter().all(|d| !d.selected));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The factory's graceful-degradation contract: with NO usable model (a configured path that
    /// doesn't exist, and — on a clean machine — no default model) `active_reasoner` returns the
    /// StubReasoner. With the `local-brain` feature OFF this is unconditional; with it ON it still
    /// holds as long as no GGUF is present. This is the headless proof of the swap wiring's fallback.
    #[test]
    fn active_reasoner_falls_back_to_stub_without_model() {
        // No custom path (a non-existent one) AND no selected brain_model_id ⇒ nothing resolves, so
        // even a feature-on build returns the stub. With no selection there is no registry file to
        // probe, so this holds regardless of what GGUFs happen to live in the shared models dir.
        let cfg = AppConfig {
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
}
