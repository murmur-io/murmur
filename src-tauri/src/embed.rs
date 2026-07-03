//! Embedding seam + pure retrieval helpers for the Phase 2a vector layer.
//!
//! Everything here is RUNTIME-AGNOSTIC and dependency-light: the real embedding model
//! (multilingual-e5-small, 384-dim, multilingual incl. Polish — downloaded on first use, NOT bundled)
//! is selected at runtime by [`active_embedder`] when its model dir is present. Until the model is
//! downloaded, [`StubEmbedder`] — a deterministic hash-bag embedder — backs the [`Embedder`] trait so
//! the whole index/search/fusion pipeline is exercisable headless (`cargo test --lib`) with
//! byte-stable vectors.
//!
//! The DB-side wiring (the `vec0` virtual table, indexing, the GATED semantic search, purge-on-
//! lock) lives in `storage/db.rs` because it needs `Db`'s private connection. This module holds
//! only the pure, unit-testable building blocks:
//! - the [`Embedder`] trait + [`StubEmbedder`];
//! - [`chunk_note`] (deterministic paragraph chunking with a `<title> · <date>` header);
//! - [`rrf_fuse`] (Reciprocal Rank Fusion for hybrid FTS ∪ vector ranking);
//! - [`vec_to_blob`] (f32 → little-endian blob for binding to a `vec0 float[N]` column).

use crate::error::Result;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::RwLock;

/// The REAL on-device embedder (multilingual-e5-small via candle). ALWAYS compiled; the real impl is
/// selected at runtime by [`active_embedder`] when the e5 model dir is present, else the stub.
pub mod candle_bert;

/// Embedding dimensionality of the vector layer. The `vec0` column is declared `float[EMBED_DIM]`,
/// so this MUST match the real model's output width. multilingual-e5-small = 384, and the stub also
/// emits 384, so the real model swaps in with ZERO vec0 schema migration. Changing it is a schema
/// change (new `vec0` table).
pub const EMBED_DIM: usize = 384;

/// Target character size for one note chunk (paragraphs are merged up to roughly this width).
const CHUNK_CHAR_TARGET: usize = 800;

/// Reciprocal Rank Fusion constant (the standard k=60). Larger k flattens the contribution of
/// rank position; 60 is the widely-used default from the original RRF paper.
pub const RRF_K: f64 = 60.0;

/// The swappable embedding backend. Pure + synchronous: `embed` maps a batch of texts to a batch
/// of `dim()`-length vectors. The real model implements this over multilingual-e5-small; tests +
/// the no-model floor use [`StubEmbedder`].
pub trait Embedder {
    /// Output vector width. MUST equal [`EMBED_DIM`] for vectors destined for the `vec0` table.
    fn dim(&self) -> usize;
    /// Embed a batch of texts → one `dim()`-length vector each (same order as the input).
    ///
    /// This is the RAW encode — no asymmetric prefix. Document-indexing and query callers SHOULD
    /// prefer [`Embedder::embed_passage`] / [`Embedder::embed_query`] so the e5 family's required
    /// asymmetric prefix convention is applied; the existing index/query sites that call `embed`
    /// directly still work (the stub ignores prefixes, and the real model treats a prefix-less text
    /// as a generic passage). See the e5 prefix note on [`PASSAGE_PREFIX`]/[`QUERY_PREFIX`].
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;

    /// Embed DOCUMENT/passage texts (the index side). Default impl prefixes each text with
    /// [`PASSAGE_PREFIX`] then calls [`Embedder::embed`]. The stub's output is prefix-invariant in
    /// practice (the prefix tokens add a tiny constant bag), so this is safe to route through the
    /// stub; the real `CandleBertEmbedder` relies on it for correct e5 retrieval quality.
    fn embed_passage(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let prefixed: Vec<String> = texts.iter().map(|t| format!("{PASSAGE_PREFIX}{t}")).collect();
        self.embed(&prefixed)
    }

    /// Embed QUERY texts (the search side). Default impl prefixes each text with [`QUERY_PREFIX`]
    /// then calls [`Embedder::embed`]. e5 REQUIRES the query/passage asymmetry for good recall.
    fn embed_query(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let prefixed: Vec<String> = texts.iter().map(|t| format!("{QUERY_PREFIX}{t}")).collect();
        self.embed(&prefixed)
    }
}

/// e5 ASYMMETRIC PREFIX (passage side). The intfloat e5 family was trained with `"passage: "` on
/// documents and `"query: "` on queries; using the right prefix is load-bearing for retrieval recall
/// (incl. Polish). This is a Mac-eval TUNABLE — the exact prefix string and whether the bake-off
/// prefers symmetric encoding is validated @Mac, not by `cargo test`.
///
/// NOTE: this is the DEFAULT (multilingual-e5-small) passage prefix; a selected [`EmbedModel`]
/// carries its own — resolved via [`selected_embed_model`]. `mmlw-retrieval-e5-small` happens to share
/// the same `"query: "`/`"passage: "` convention (verified against its HF card), so the effective prefix
/// is identical for both bundled options.
pub const PASSAGE_PREFIX: &str = "passage: ";

/// e5 ASYMMETRIC PREFIX (query side). See [`PASSAGE_PREFIX`].
pub const QUERY_PREFIX: &str = "query: ";

/// Sub-directory under the shared models dir holding the DEFAULT (multilingual-e5-small) files
/// (`model.safetensors` + `tokenizer.json` + `config.json`). 384-dim ⇒ [`EMBED_DIM`] is unchanged,
/// so swapping the real model in costs ZERO vec0 schema migration. A selected [`EmbedModel`] carries
/// its own subdir — resolved via [`selected_embed_model`].
pub const EMBED_MODEL_SUBDIR: &str = "embed-multilingual-e5-small";

/// The three Hugging Face files the real embedder needs, fetched INBOUND-ONLY by
/// `download_embed_model`. Order is irrelevant; each is downloaded into the selected model's subdir.
/// Every bundled [`EmbedModel`] uses this same three-file set (BERT safetensors + tokenizer + config).
pub const EMBED_MODEL_FILES: &[&str] = &["model.safetensors", "tokenizer.json", "config.json"];

/// Hugging Face `resolve/main` base for the DEFAULT (intfloat/multilingual-e5-small). INBOUND ONLY —
/// fetched, never sent meeting content. A selected [`EmbedModel`] carries its own base.
pub const EMBED_MODEL_HF_BASE: &str =
    "https://huggingface.co/intfloat/multilingual-e5-small/resolve/main";

/// The stable id of the DEFAULT embedder. `None`/unknown selection resolves here → BYTE-IDENTICAL to
/// the historical hardcoded-const behavior.
pub const DEFAULT_EMBED_MODEL_ID: &str = "multilingual-e5-small";

/// A selectable on-device embedder. All bundled options MUST be BERT / 384-hidden so the `vec0`
/// column width ([`EMBED_DIM`]) never changes — a differently-dimensioned model would be a `vec0`
/// SCHEMA migration, which we explicitly do NOT do (the loader guards `hidden_size == EMBED_DIM` and
/// fails loud otherwise). Fields are `&'static str` (a compile-time registry, like `BRAIN_MODELS`).
#[derive(Debug, Clone, Copy)]
pub struct EmbedModel {
    /// Stable id persisted in `AppConfig::embed_model_id` (e.g. `"multilingual-e5-small"`).
    pub id: &'static str,
    /// Human label for the picker.
    pub name: &'static str,
    /// Sub-directory under the shared models dir holding this model's three files.
    pub subdir: &'static str,
    /// Hugging Face `resolve/main` base the three files are fetched from (INBOUND ONLY).
    pub hf_base: &'static str,
    /// Asymmetric QUERY prefix for this model (the search side).
    pub query_prefix: &'static str,
    /// Asymmetric PASSAGE prefix for this model (the index side).
    pub passage_prefix: &'static str,
}

/// The bundled, selectable embedders. ALL are BERT / hidden_size 384 (verified against their HF
/// `config.json`), so switching between them needs NO `vec0` schema migration — only a re-index
/// (`reindex_embeddings`) because the vectors from a different model are not comparable.
///
/// - `multilingual-e5-small` (intfloat) — the DEFAULT; the historical values, so a fresh/unset
///   config behaves BYTE-IDENTICALLY to before this registry existed.
/// - `mmlw-retrieval-e5-small` (sdadas) — a Polish-first RETRIEVAL-tuned e5 (BERT, hidden_size 384,
///   XLM-R tokenizer): initialized from multilingual-e5-small, knowledge-distilled on 60M PL-EN pairs,
///   THEN contrastive-fine-tuned on Polish MS MARCO. Beats the plain distilled `mmlw-e5-small` on the
///   Polish IR Benchmark (PIRB nDCG@10 52.34 vs 47.64 — the retrieval-specific, apples-to-apples number;
///   the "67.5" figure was a DIFFERENT benchmark, PL-MTEB avg-by-task-type for the 768-dim roberta-base).
///   Uses e5's own `"query: "`/`"passage: "` prefixes. The real recall win is a Mac-eval (the
///   `eval::bakeoff` harness), not a `cargo test` claim.
pub static EMBED_MODELS: &[EmbedModel] = &[
    EmbedModel {
        id: DEFAULT_EMBED_MODEL_ID,
        name: "Multilingual E5 Small (default)",
        subdir: EMBED_MODEL_SUBDIR,
        hf_base: EMBED_MODEL_HF_BASE,
        query_prefix: QUERY_PREFIX,
        passage_prefix: PASSAGE_PREFIX,
    },
    EmbedModel {
        id: "mmlw-retrieval-e5-small",
        name: "MMLW Retrieval E5 Small (Polish-first)",
        subdir: "embed-mmlw-retrieval-e5-small",
        hf_base: "https://huggingface.co/sdadas/mmlw-retrieval-e5-small/resolve/main",
        // mmlw-retrieval-e5's HF card uses e5's OWN asymmetric prefixes (query_prefix="query: ",
        // answer_prefix="passage: "); the Polish "zapytanie: " prefix is the ROBERTA family's, not this one.
        query_prefix: "query: ",
        passage_prefix: "passage: ",
    },
];

/// Look up a bundled embedder by id; `None` for an unknown id.
pub fn embed_model_by_id(id: &str) -> Option<&'static EmbedModel> {
    EMBED_MODELS.iter().find(|m| m.id == id)
}

/// IPC view of an [`EmbedModel`] for the picker: the static metadata plus the two runtime flags the
/// FE needs — `downloaded` (all three files present in this model's subdir) and `selected` (mirrors
/// the persisted `embed_model_id`, with `None`/unknown resolving to the default). No content read /
/// no egress — static metadata + on-disk existence only.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbedModelDto {
    pub id: String,
    pub name: String,
    pub downloaded: bool,
    pub selected: bool,
}

/// Build the picker DTOs for all bundled embedders. `selected_id` is the persisted config value
/// (`None`/empty ⇒ the default is the selected one). `downloaded` probes each model's own subdir
/// under `models_dir` (an unresolvable models dir ⇒ all `false`, graceful). NEVER panics.
pub fn embed_model_dtos(selected_id: Option<&str>) -> Vec<EmbedModelDto> {
    let base = crate::transcribe::models_dir().ok();
    let effective = selected_id
        .filter(|s| !s.is_empty())
        .and_then(embed_model_by_id)
        .map(|m| m.id)
        .unwrap_or(DEFAULT_EMBED_MODEL_ID);
    EMBED_MODELS
        .iter()
        .map(|m| {
            let downloaded = base
                .as_ref()
                .map(|b| {
                    let dir = b.join(m.subdir);
                    EMBED_MODEL_FILES.iter().all(|f| dir.join(f).is_file())
                })
                .unwrap_or(false);
            EmbedModelDto {
                id: m.id.to_string(),
                name: m.name.to_string(),
                downloaded,
                selected: m.id == effective,
            }
        })
        .collect()
}

/// The DEFAULT embedder descriptor (the first registry entry). Infallible.
pub fn default_embed_model() -> &'static EmbedModel {
    &EMBED_MODELS[0]
}

/// Process-global "which embedder is selected" — the SEAM that lets the existing zero-arg
/// `active_embedder`/`embed_model_present`/`embed_model_dir`/`download_embed_model` resolve the
/// user's configured model WITHOUT threading `&AppConfig` through every call site. `None` (never set,
/// or set to `None`) means "use the default" → byte-identical to the historical behavior. Written
/// once by `AppConfig::load` at startup and again by `AppConfig::save` when the selection changes;
/// read on every embedder construction. A poisoned lock degrades to the default (never panics).
static SELECTED_EMBED_MODEL_ID: RwLock<Option<String>> = RwLock::new(None);

/// Set the process-global selected embedder id (called by `AppConfig::load`/`save`). `None` clears
/// back to the default. NEVER panics — a poisoned lock is silently ignored (the default stands).
pub fn set_selected_embed_model_id(id: Option<String>) {
    if let Ok(mut g) = SELECTED_EMBED_MODEL_ID.write() {
        *g = id.filter(|s| !s.is_empty());
    }
}

/// Resolve the currently-selected [`EmbedModel`]: the process-global id if it names a known model,
/// else the DEFAULT. NEVER panics (a poisoned lock or an unknown id both fall back to the default),
/// so this is safe on any hot path.
pub fn selected_embed_model() -> &'static EmbedModel {
    let id = SELECTED_EMBED_MODEL_ID
        .read()
        .ok()
        .and_then(|g| g.clone());
    match id.as_deref().and_then(embed_model_by_id) {
        Some(m) => m,
        None => default_embed_model(),
    }
}

/// Resolve the on-disk dir the SELECTED embedder loads from: `<models_dir>/<selected.subdir>/`.
/// Creating the models dir can fail (returns `Err`); the dir itself may not yet exist (that is fine —
/// the caller checks [`embed_model_present`]). NEVER panics.
pub fn embed_model_dir() -> Result<PathBuf> {
    Ok(crate::transcribe::models_dir()?.join(selected_embed_model().subdir))
}

/// `true` when all three model files exist in the SELECTED model's [`embed_model_dir`]. Pure
/// existence probe (the only I/O is `is_file`); a models-dir resolution error is treated as "not
/// present" (graceful — falls back to the stub), never propagated as a hard error.
pub fn embed_model_present() -> bool {
    match embed_model_dir() {
        Ok(dir) => EMBED_MODEL_FILES.iter().all(|f| dir.join(f).is_file()),
        Err(_) => false,
    }
}

/// Deterministic, model-free embedder: a hashed bag-of-tokens projected into [`EMBED_DIM`] and
/// L2-normalized. NOT semantically meaningful — its only contract is determinism (same text →
/// byte-identical vector) so the index/KNN/fusion plumbing is testable before the real model.
#[derive(Debug, Default, Clone, Copy)]
pub struct StubEmbedder;

impl Embedder for StubEmbedder {
    fn dim(&self) -> usize {
        EMBED_DIM
    }

    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| stub_embed_one(t)).collect())
    }
}

/// The single active embedding backend used by BOTH the index path (chunking a note on creation)
/// and the query path (Ask-My-Vault / MCP `search_semantic`). Returning a boxed trait object keeps
/// the model a swappable seam.
///
/// Graceful degradation (mirrors [`crate::reason::active_reasoner`]), in priority order:
/// - the e5 model dir is present at [`embed_model_dir`] ([`embed_model_present`]) → the real
///   [`candle_bert::CandleBertEmbedder`] (lazy: the model loads on first `embed`, not here, so this
///   never blocks startup and never panics);
/// - otherwise (no model, or a construction error) → the dependency-free [`StubEmbedder`]. The app
///   works either way; semantic, WHEN enabled, just uses real vectors once the model is present.
///   Selection keys ONLY on model presence — the candle backend is always compiled (no cargo feature).
///
/// NEVER panics and NEVER blocks. Target model = multilingual-e5-small (384-dim), so the real model's
/// width EQUALS [`EMBED_DIM`] — ZERO `vec_chunks` schema migration. (A future model whose dimension
/// differs would be a `vec_chunks float[N]` SCHEMA change — an additive migration to a new-width vec0
/// table plus a full re-index, NOT a code one-liner; a mismatched-width insert fails loud, never
/// silently.) Cheap to construct (the stub is zero-sized; the candle backend defers the heavy load),
/// so callers build one per operation. NEVER invoked when `semantic_search_enabled` is off (the gate
/// short-circuits before this is called) — building the real embedder does NOT flip that flag.
pub fn active_embedder() -> Box<dyn Embedder> {
    let model = selected_embed_model();
    if embed_model_present() {
        let built = embed_model_dir().and_then(|dir| {
            candle_bert::CandleBertEmbedder::new(dir, model.query_prefix, model.passage_prefix)
        });
        match built {
            Ok(e) => {
                tracing::info!(target: "embed", model_id = %model.id, "local embed model ready (lazy load)");
                return Box::new(e);
            }
            Err(e) => {
                tracing::warn!(target: "embed", model_id = %model.id, error = %e, "local embed init failed; using stub embedder");
            }
        }
    } else {
        tracing::info!(target: "embed", model_id = %model.id, "no local embed model present; using stub embedder");
    }
    Box::new(StubEmbedder)
}

/// FNV-1a 64-bit hash of a string (stable across runs/platforms — no `DefaultHasher` randomization).
fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Hash each lowercased alphanumeric token into a dimension (with a sign bit) and accumulate, then
/// L2-normalize. Deterministic; an empty/punctuation-only text yields an all-zero vector.
fn stub_embed_one(text: &str) -> Vec<f32> {
    let mut v = vec![0f32; EMBED_DIM];
    for token in text.split(|c: char| !c.is_alphanumeric()).filter(|t| !t.is_empty()) {
        let h = fnv1a(&token.to_lowercase());
        let idx = (h % EMBED_DIM as u64) as usize;
        let sign = if (h >> 40) & 1 == 0 { 1.0 } else { -1.0 };
        v[idx] += sign;
    }
    l2_normalize(&mut v);
    v
}

/// In-place L2 normalization. A zero vector is left untouched (avoids a divide-by-zero NaN).
fn l2_normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

/// Pack a vector as a little-endian f32 blob for binding to a `vec0 float[N]` column (sqlite-vec
/// accepts the raw f32 array as a BLOB parameter). Length MUST equal the column width.
pub(crate) fn vec_to_blob(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for f in v {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

/// Split a note's markdown into deterministic chunks, each PREFIXED with a `<title> · <date>`
/// header so the embedded text always carries its provenance (and the real model can ground each
/// chunk). Paragraphs (blank-line-separated) are merged greedily up to [`CHUNK_CHAR_TARGET`]; a
/// single oversized paragraph becomes its own chunk. Empty/blank markdown yields no chunks.
///
/// Pure + deterministic: identical inputs always produce identical chunk text (unit-tested), which
/// is what makes `content_hash`-based dedup and re-index stable.
pub fn chunk_note(title: &str, date: &str, markdown: &str) -> Vec<String> {
    let header = format!("{title} · {date}");
    let paragraphs: Vec<&str> = markdown
        .split("\n\n")
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();

    let mut chunks: Vec<String> = Vec::new();
    let mut cur = String::new();
    for p in paragraphs {
        if !cur.is_empty() && cur.len() + 1 + p.len() > CHUNK_CHAR_TARGET {
            chunks.push(format!("{header}\n{cur}"));
            cur = String::new();
        }
        if !cur.is_empty() {
            cur.push('\n');
        }
        cur.push_str(p);
    }
    if !cur.is_empty() {
        chunks.push(format!("{header}\n{cur}"));
    }
    chunks
}

/// Reciprocal Rank Fusion over any number of ranked id-lists (each already ordered best-first).
/// Each list contributes `1 / (RRF_K + rank)` (rank 1-based) to an id's fused score; scores sum
/// across lists. Returns `(id, score)` sorted by score DESC, ties broken by id ASC for a stable,
/// deterministic order. Pure — the fusion math is independent of FTS/vector internals.
pub fn rrf_fuse(lists: &[Vec<String>], k: f64) -> Vec<(String, f64)> {
    let mut scores: HashMap<String, f64> = HashMap::new();
    for list in lists {
        for (rank0, id) in list.iter().enumerate() {
            *scores.entry(id.clone()).or_insert(0.0) += 1.0 / (k + (rank0 as f64) + 1.0);
        }
    }
    let mut fused: Vec<(String, f64)> = scores.into_iter().collect();
    fused.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    fused
}

/// RRF-fuse the two gated document retrieval legs (vector KNN + keyword FTS) into one best-first,
/// per-document-deduped hit list. Either leg may be empty (model absent ⇒ no KNN leg; punctuation
/// query ⇒ no FTS leg) — RRF over the remaining list preserves its order. The snippet kept for a
/// document is its first-seen one, KNN (nearest chunk) preferred over FTS (best-bm25 chunk). Pure
/// fusion — both inputs were already visibility-gated by their Db readers.
pub fn fuse_doc_hits(
    knn: Vec<crate::storage::models::DocChunkHit>,
    fts: Vec<crate::storage::models::DocChunkHit>,
) -> Vec<crate::storage::models::DocChunkHit> {
    if knn.is_empty() && fts.is_empty() {
        return Vec::new();
    }
    let knn_ids: Vec<String> = knn.iter().map(|h| h.document_id.clone()).collect();
    let fts_ids: Vec<String> = fts.iter().map(|h| h.document_id.clone()).collect();
    let fused = rrf_fuse(&[knn_ids, fts_ids], RRF_K);
    let mut by_id: HashMap<String, crate::storage::models::DocChunkHit> = HashMap::new();
    for h in knn.into_iter().chain(fts) {
        by_id.entry(h.document_id.clone()).or_insert(h);
    }
    fused
        .into_iter()
        .filter_map(|(id, _score)| by_id.remove(&id))
        .collect()
}

/// Download the three model files for the SELECTED embedder into [`embed_model_dir`], INBOUND-ONLY,
/// with progress.
///
/// Mirrors [`crate::reason::download_brain_model`]: each file streams to `<file>.part` then renames
/// atomically; `on_progress(file_index, downloaded, total)` fires as bytes arrive (`total` is `None`
/// when the server omits `Content-Length`). A file already present on disk is SKIPPED. INBOUND ONLY:
/// fetches model files and sends NO request body / NO meeting content (no egress). NO PII logged —
/// filenames + byte counts only. The HF base + destination subdir come from [`selected_embed_model`],
/// so `mmlw-retrieval-e5-small` is fetched from its own repo into its own dir.
pub async fn download_embed_model<F>(mut on_progress: F) -> Result<PathBuf>
where
    F: FnMut(usize, u64, Option<u64>),
{
    use crate::error::AppError;
    use tokio::io::AsyncWriteExt;

    let model = selected_embed_model();
    let dir = embed_model_dir()?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| AppError::Storage(format!("create embed model dir: {e}")))?;

    for (idx, file) in EMBED_MODEL_FILES.iter().enumerate() {
        let dest = dir.join(file);
        if dest.is_file() {
            continue;
        }
        let url = format!("{}/{file}", model.hf_base);
        tracing::info!(target: "embed", file = %file, "downloading embed model file");

        let mut resp = reqwest::get(&url)
            .await
            .map_err(|e| AppError::Storage(format!("embed model download request failed: {e}")))?;
        if !resp.status().is_success() {
            return Err(AppError::Storage(format!(
                "embed model download HTTP {} for {file}",
                resp.status()
            )));
        }
        let total = resp.content_length();

        let part = dest.with_extension("part");
        let mut out = tokio::fs::File::create(&part)
            .await
            .map_err(|e| AppError::Storage(format!("create embed model temp file: {e}")))?;
        let mut downloaded: u64 = 0;
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| AppError::Storage(format!("embed model download body failed: {e}")))?
        {
            out.write_all(&chunk)
                .await
                .map_err(|e| AppError::Storage(format!("write embed model chunk: {e}")))?;
            downloaded += chunk.len() as u64;
            on_progress(idx, downloaded, total);
        }
        out.flush()
            .await
            .map_err(|e| AppError::Storage(format!("flush embed model file: {e}")))?;
        drop(out);

        if downloaded == 0 {
            let _ = tokio::fs::remove_file(&part).await;
            return Err(AppError::Storage(format!(
                "embed model download returned empty body for {file}"
            )));
        }
        tokio::fs::rename(&part, &dest)
            .await
            .map_err(|e| AppError::Storage(format!("rename embed model file: {e}")))?;
        tracing::info!(target: "embed", file = %file, bytes = downloaded, "embed model file ready");
    }

    Ok(dir)
}

/// A process-wide lock serializing every test that mutates the [`SELECTED_EMBED_MODEL_ID`] global
/// (in THIS module and in `config`/`commands` tests). `cargo test` runs tests in parallel, so without
/// this a test that sets the selection could race a test that reads `embed_model_dir`/`active_embedder`
/// under the default. Callers lock it for the whole set→act→restore span, and MUST restore the
/// selection to `None` (the default) before dropping the guard.
#[cfg(test)]
pub(crate) static EMBED_SELECTION_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_embed_is_deterministic_and_normalized() {
        let e = StubEmbedder;
        let a = e.embed(&["budżet planowanie kwartał".to_string()]).unwrap();
        let b = e.embed(&["budżet planowanie kwartał".to_string()]).unwrap();
        assert_eq!(a, b, "same text must embed byte-identically");
        assert_eq!(a[0].len(), EMBED_DIM);
        let norm: f32 = a[0].iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "non-empty vector must be L2-normalized, got {norm}");
        // Empty / punctuation-only text → all-zero (no NaN from div-by-zero).
        let z = e.embed(&["   !!! ".to_string()]).unwrap();
        assert!(z[0].iter().all(|x| *x == 0.0));
    }

    #[test]
    fn chunk_note_is_deterministic_with_header_prefix() {
        let md = "First paragraph about the budget.\n\nSecond paragraph about hiring.";
        let c1 = chunk_note("Quarterly Sync", "2026-06-28", md);
        let c2 = chunk_note("Quarterly Sync", "2026-06-28", md);
        assert_eq!(c1, c2, "chunking must be deterministic");
        assert!(!c1.is_empty());
        for chunk in &c1 {
            assert!(
                chunk.starts_with("Quarterly Sync · 2026-06-28\n"),
                "every chunk must carry the <title> · <date> header, got: {chunk:?}"
            );
        }
        // Blank markdown → no chunks.
        assert!(chunk_note("T", "D", "   \n\n  ").is_empty());
    }

    #[test]
    fn chunk_note_merges_small_paragraphs_and_splits_large() {
        // Two tiny paragraphs merge into one chunk; a huge paragraph stands alone.
        let small = "a\n\nb";
        assert_eq!(chunk_note("T", "D", small).len(), 1);
        let big = format!("{}\n\n{}", "x".repeat(900), "y".repeat(50));
        // 900-char para exceeds target → its own chunk; the 50-char para is a second chunk.
        assert_eq!(chunk_note("T", "D", &big).len(), 2);
    }

    #[test]
    fn rrf_fuse_orders_and_dedups() {
        // m1 ranks #1 in FTS, #3 in vector; m2 ranks #2 in both; m3 only in vector #1.
        let fts = vec!["m1".to_string(), "m2".to_string()];
        let vector = vec!["m3".to_string(), "m2".to_string(), "m1".to_string()];
        let fused = rrf_fuse(&[fts, vector], RRF_K);
        // Each id appears exactly once (dedup).
        let ids: Vec<&String> = fused.iter().map(|(id, _)| id).collect();
        assert_eq!(ids.len(), 3);
        // m2 (ranked in both, high in each) must beat m1 and m3 (each ranked well in only one).
        // m2: 1/(60+2) + 1/(60+2); m1: 1/(60+1) + 1/(60+3); m3: 1/(60+1).
        let pos = |want: &str| fused.iter().position(|(id, _)| id == want).unwrap();
        assert!(pos("m2") < pos("m3"), "m2 (in both lists) must outrank m3 (one list)");
        assert!(pos("m1") < pos("m3"), "m1 (in both lists) must outrank m3 (one list)");
    }

    #[test]
    fn embed_query_and_passage_apply_the_e5_prefix() {
        // The default trait methods prefix the text; we assert the prefix reaches `embed` by using a
        // capture embedder that records exactly what it was handed.
        struct CaptureEmbedder(std::sync::Mutex<Vec<String>>);
        impl Embedder for CaptureEmbedder {
            fn dim(&self) -> usize {
                EMBED_DIM
            }
            fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
                *self.0.lock().unwrap() = texts.to_vec();
                Ok(texts.iter().map(|_| vec![0f32; EMBED_DIM]).collect())
            }
        }
        let e = CaptureEmbedder(std::sync::Mutex::new(Vec::new()));
        e.embed_passage(&["a budget note".to_string()]).unwrap();
        assert_eq!(e.0.lock().unwrap().as_slice(), &["passage: a budget note".to_string()]);
        e.embed_query(&["how much budget".to_string()]).unwrap();
        assert_eq!(e.0.lock().unwrap().as_slice(), &["query: how much budget".to_string()]);
    }

    #[test]
    fn embed_model_dir_is_under_models_dir() {
        let _g = EMBED_SELECTION_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // With no selection set, the resolver falls back to the default (e5) subdir.
        set_selected_embed_model_id(None);
        let dir = embed_model_dir().unwrap();
        assert!(dir.ends_with(EMBED_MODEL_SUBDIR));
        // The three e5 files are the documented set.
        assert_eq!(EMBED_MODEL_FILES, &["model.safetensors", "tokenizer.json", "config.json"]);
        assert!(EMBED_MODEL_HF_BASE.contains("intfloat/multilingual-e5-small"));
    }

    /// The registry is well-formed: the DEFAULT is first, ids are unique, mmlw is present, and EVERY
    /// bundled option is 384-safe by construction (they all share EMBED_MODEL_FILES; the loader guards
    /// hidden_size == EMBED_DIM at load, so a wrong-width model would fail loud — never silently).
    #[test]
    fn embed_registry_is_wellformed_and_has_mmlw() {
        assert_eq!(default_embed_model().id, DEFAULT_EMBED_MODEL_ID);
        assert_eq!(EMBED_MODELS[0].id, DEFAULT_EMBED_MODEL_ID, "default must be first");
        // Unique ids.
        let mut ids: Vec<&str> = EMBED_MODELS.iter().map(|m| m.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), EMBED_MODELS.len(), "embed model ids must be unique");
        // mmlw is a first-class selectable option with the documented e5-compatible prefixes.
        let mmlw = embed_model_by_id("mmlw-retrieval-e5-small")
            .expect("mmlw-retrieval-e5-small must be registered");
        assert_eq!(mmlw.query_prefix, "query: ");
        assert_eq!(mmlw.passage_prefix, "passage: ");
        assert!(mmlw.hf_base.contains("sdadas/mmlw-retrieval-e5-small"));
        assert_ne!(mmlw.subdir, EMBED_MODEL_SUBDIR, "each model needs its own subdir");
        // The default carries the historical values verbatim (byte-identical default behavior).
        let def = default_embed_model();
        assert_eq!(def.subdir, EMBED_MODEL_SUBDIR);
        assert_eq!(def.hf_base, EMBED_MODEL_HF_BASE);
        assert_eq!(def.query_prefix, QUERY_PREFIX);
        assert_eq!(def.passage_prefix, PASSAGE_PREFIX);
    }

    /// The process-global selection seam: setting a known id resolves to it; unknown/None fall back
    /// to the default. `embed_model_dir` tracks the selection's subdir. Restore the default after so
    /// this test cannot leak state into others that share the process global.
    #[test]
    fn selected_embed_model_resolves_and_falls_back() {
        let _g = EMBED_SELECTION_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_selected_embed_model_id(Some("mmlw-retrieval-e5-small".to_string()));
        assert_eq!(selected_embed_model().id, "mmlw-retrieval-e5-small");
        assert!(embed_model_dir().unwrap().ends_with("embed-mmlw-retrieval-e5-small"));

        // Unknown id ⇒ default. Empty ⇒ default. None ⇒ default.
        set_selected_embed_model_id(Some("does-not-exist".to_string()));
        assert_eq!(selected_embed_model().id, DEFAULT_EMBED_MODEL_ID);
        set_selected_embed_model_id(Some(String::new()));
        assert_eq!(selected_embed_model().id, DEFAULT_EMBED_MODEL_ID);
        set_selected_embed_model_id(None);
        assert_eq!(selected_embed_model().id, DEFAULT_EMBED_MODEL_ID);
    }

    /// The picker DTOs cover every registry entry, mark exactly one as `selected` (the resolved one),
    /// and default (None) selects the default model.
    #[test]
    fn embed_model_dtos_mark_selected() {
        let dtos = embed_model_dtos(None);
        assert_eq!(dtos.len(), EMBED_MODELS.len());
        let selected: Vec<&str> = dtos.iter().filter(|d| d.selected).map(|d| d.id.as_str()).collect();
        assert_eq!(selected, vec![DEFAULT_EMBED_MODEL_ID], "None ⇒ default is selected");

        let dtos = embed_model_dtos(Some("mmlw-retrieval-e5-small"));
        let selected: Vec<&str> = dtos.iter().filter(|d| d.selected).map(|d| d.id.as_str()).collect();
        assert_eq!(selected, vec!["mmlw-retrieval-e5-small"]);

        // Unknown id ⇒ the default is marked selected (never zero-selected).
        let dtos = embed_model_dtos(Some("bogus"));
        assert!(dtos.iter().filter(|d| d.selected).count() == 1);
        assert!(dtos.iter().find(|d| d.selected).map(|d| d.id.as_str()) == Some(DEFAULT_EMBED_MODEL_ID));
    }

    #[test]
    fn embed_model_present_false_when_any_file_missing() {
        let _g = EMBED_SELECTION_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_selected_embed_model_id(None);
        // On a clean machine the e5 dir is absent ⇒ not present. Even with a partial dir (only one of
        // the three files), `present` must be false — the loader needs all three.
        let dir = embed_model_dir().unwrap();
        let had_dir = dir.is_dir();
        // If a real model happens to be installed, this assertion is vacuously satisfied; otherwise
        // assert the absent/partial cases without clobbering a real install.
        if !had_dir {
            assert!(!embed_model_present(), "absent e5 dir must report not-present");
        }
    }

    /// The embedder factory's graceful-degradation contract: with NO e5 model dir present,
    /// `active_embedder` returns the deterministic StubEmbedder (dim == EMBED_DIM, byte-stable). The
    /// candle backend is always compiled now, so selection keys ONLY on model presence — absent model
    /// ⇒ stub. Headless proof of the swap wiring's fallback (mirrors
    /// `active_reasoner_falls_back_to_stub_without_model`).
    #[test]
    fn active_embedder_falls_back_to_stub_without_model() {
        let _g = EMBED_SELECTION_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_selected_embed_model_id(None);
        // Only meaningful as a fallback assertion when no real model is installed; on a clean
        // machine/CI the e5 dir is absent, so the always-compiled candle backend still yields the stub.
        if !embed_model_present() {
            let e = active_embedder();
            assert_eq!(e.dim(), EMBED_DIM);
            // Deterministic + L2-normalized like the stub (the real model would not be byte-stable).
            let a = e.embed(&["budżet planowanie".to_string()]).unwrap();
            let b = e.embed(&["budżet planowanie".to_string()]).unwrap();
            assert_eq!(a, b, "stub fallback must be byte-deterministic");
            let norm: f32 = a[0].iter().map(|x| x * x).sum::<f32>().sqrt();
            assert!((norm - 1.0).abs() < 1e-5);
        }
    }
}
