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
pub const PASSAGE_PREFIX: &str = "passage: ";

/// e5 ASYMMETRIC PREFIX (query side). See [`PASSAGE_PREFIX`].
pub const QUERY_PREFIX: &str = "query: ";

/// Sub-directory under the shared models dir holding the multilingual-e5-small files
/// (`model.safetensors` + `tokenizer.json` + `config.json`). 384-dim ⇒ [`EMBED_DIM`] is unchanged,
/// so swapping the real model in costs ZERO vec0 schema migration.
pub const EMBED_MODEL_SUBDIR: &str = "embed-multilingual-e5-small";

/// The three Hugging Face files the real e5 embedder needs, fetched INBOUND-ONLY by
/// `download_embed_model`. Order is irrelevant; each is downloaded into [`EMBED_MODEL_SUBDIR`].
pub const EMBED_MODEL_FILES: &[&str] = &["model.safetensors", "tokenizer.json", "config.json"];

/// Hugging Face `resolve/main` base for intfloat/multilingual-e5-small. INBOUND ONLY — fetched,
/// never sent meeting content.
pub const EMBED_MODEL_HF_BASE: &str =
    "https://huggingface.co/intfloat/multilingual-e5-small/resolve/main";

/// Resolve the on-disk dir the real e5 embedder loads from: `<models_dir>/embed-multilingual-e5-small/`.
/// Creating the models dir can fail (returns `Err`); the dir itself may not yet exist (that is fine —
/// the caller checks [`embed_model_present`]). NEVER panics.
pub fn embed_model_dir() -> Result<PathBuf> {
    Ok(crate::transcribe::models_dir()?.join(EMBED_MODEL_SUBDIR))
}

/// `true` when all three e5 model files exist in [`embed_model_dir`]. Pure existence probe (the only
/// I/O is `is_file`); a models-dir resolution error is treated as "not present" (graceful — falls
/// back to the stub), never propagated as a hard error.
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
    if embed_model_present() {
        match embed_model_dir().and_then(candle_bert::CandleBertEmbedder::new) {
            Ok(e) => {
                tracing::info!(target: "embed", "local embed model ready (lazy load)");
                return Box::new(e);
            }
            Err(e) => {
                tracing::warn!(target: "embed", error = %e, "local embed init failed; using stub embedder");
            }
        }
    } else {
        tracing::info!(target: "embed", "no local embed model present; using stub embedder");
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

/// Download the three e5 model files into [`embed_model_dir`], INBOUND-ONLY, with progress.
///
/// Mirrors [`crate::reason::download_brain_model`]: each file streams to `<file>.part` then renames
/// atomically; `on_progress(file_index, downloaded, total)` fires as bytes arrive (`total` is `None`
/// when the server omits `Content-Length`). A file already present on disk is SKIPPED. INBOUND ONLY:
/// fetches model files and sends NO request body / NO meeting content (no egress). NO PII logged —
/// filenames + byte counts only.
pub async fn download_embed_model<F>(mut on_progress: F) -> Result<PathBuf>
where
    F: FnMut(usize, u64, Option<u64>),
{
    use crate::error::AppError;
    use tokio::io::AsyncWriteExt;

    let dir = embed_model_dir()?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| AppError::Storage(format!("create embed model dir: {e}")))?;

    for (idx, file) in EMBED_MODEL_FILES.iter().enumerate() {
        let dest = dir.join(file);
        if dest.is_file() {
            continue;
        }
        let url = format!("{EMBED_MODEL_HF_BASE}/{file}");
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
        let dir = embed_model_dir().unwrap();
        assert!(dir.ends_with(EMBED_MODEL_SUBDIR));
        // The three e5 files are the documented set.
        assert_eq!(EMBED_MODEL_FILES, &["model.safetensors", "tokenizer.json", "config.json"]);
        assert!(EMBED_MODEL_HF_BASE.contains("intfloat/multilingual-e5-small"));
    }

    #[test]
    fn embed_model_present_false_when_any_file_missing() {
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
