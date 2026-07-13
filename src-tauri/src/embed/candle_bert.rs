//! Real on-device embedding model (Phase C) — an [`Embedder`] backed by candle-transformers 0.10.2
//! BERT inference (Metal). ALWAYS compiled; [`crate::embed::active_embedder`] selects it at runtime
//! when the multilingual-e5-small model dir is present, else ships the dependency-free `StubEmbedder`.
//!
//! ## Honest scope (READ THIS)
//!
//! Everything here is **COMPILE-proven only** in the headless CI loop. What can ONLY be verified on a
//! signed/dev build on a real Mac, with the multilingual-e5-small files actually present:
//! - real embedding quality (do the vectors retrieve sanely at all);
//! - the **mean-pool + L2-normalize** producing useful e5 embeddings;
//! - the **e5 asymmetric prefix** (`"query: "` / `"passage: "`) being the right convention for recall
//!   (a Mac-eval TUNABLE — see [`crate::embed::QUERY_PREFIX`]/[`crate::embed::PASSAGE_PREFIX`]);
//! - **Polish** recall;
//! - **Metal** performance + correctness (load time, throughput, the Metal-vs-CPU fallback).
//!
//! `cargo test --lib` NEVER runs a forward pass here (the smoke test is `#[ignore]`d). Treat a green
//! build as proof the impl typechecks/links against candle 0.10.2 — NOT as proof embedding works. The
//! real quality/Metal/Polish-recall bake-off is @Mac.
//!
//! ## Graceful + crash-safe
//!
//! Every fallible step returns [`AppError`] — config parse, model load, tokenize, forward, and pooling
//! NEVER panic or `unwrap`. Construction ([`CandleBertEmbedder::new`]) is cheap and infallible-by-design
//! beyond holding the dir: the heavy safetensors+tokenizer load is **lazy** (first `embed` call) and
//! cached behind a `Mutex`, so this can back `active_embedder` without ever blocking or aborting app
//! startup. `Device::new_metal(0)` is tried first with a CPU fallback so a Metal-less context still
//! works.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use candle_core::{Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config, DTYPE};
use tokenizers::{Tokenizer, TruncationDirection, TruncationParams, TruncationStrategy};

use crate::embed::{Embedder, EMBED_DIM, EMBED_MODEL_FILES};
use crate::error::{AppError, Result};

/// BERT's absolute position embeddings top out at 512 tokens (e5-small included) — a text longer
/// than this cannot be encoded at all, so truncation isn't just a perf safety net, it's required
/// for correctness. Without it, a single unbounded-length topic chunk (e.g. a continuous
/// monologue-style meeting with no natural topic boundary — [`crate::embed::segment_topics`] has
/// no upper bound on a chunk's own text length) still drives an oversized tensor row inside ONE
/// `embed_passage` call regardless of how small the sub-batch is (2026-07-13 follow-up finding).
const MAX_SEQ_LEN: usize = 512;

/// An [`Embedder`] running multilingual-e5-small in-process via candle BERT (Metal, CPU fallback).
/// The model + tokenizer load lazily on the first `embed` and are cached behind an `Arc` so repeated
/// calls reuse one engine. Output is mean-pooled over tokens then L2-normalized (the e5 contract).
pub struct CandleBertEmbedder {
    /// Directory holding `model.safetensors` + `tokenizer.json` + `config.json`.
    model_dir: PathBuf,
    /// Asymmetric QUERY prefix for the selected model (e.g. `"query: "`). Applied by
    /// [`Embedder::embed_query`]'s default impl via [`Self::query_prefix`].
    query_prefix: String,
    /// Asymmetric PASSAGE prefix for the selected model (e.g. `"passage: "`). Applied by
    /// [`Embedder::embed_passage`]'s default impl via [`Self::passage_prefix`].
    passage_prefix: String,
    /// Lazily-built, cached `(model, tokenizer, device)`. `None` until the first `embed` call.
    inner: Mutex<Option<Arc<Loaded>>>,
}

/// The loaded engine: the BERT weights, its tokenizer, and the device they live on.
struct Loaded {
    model: BertModel,
    tokenizer: Tokenizer,
    device: Device,
}

impl CandleBertEmbedder {
    /// Build an embedder for the BERT files in `model_dir`, applying the selected model's asymmetric
    /// `query_prefix`/`passage_prefix` (e5 and mmlw both use `"query: "`/`"passage: "`). CHEAP +
    /// non-blocking: it only validates the three files exist and stores the dir/prefixes — the
    /// safetensors/tokenizer load is deferred to first use, so this is safe to call from
    /// `active_embedder` on the startup path. Returns `Err` (never panics) if a required file is missing.
    pub fn new(
        model_dir: PathBuf,
        query_prefix: impl Into<String>,
        passage_prefix: impl Into<String>,
    ) -> Result<Self> {
        for f in EMBED_MODEL_FILES {
            let p = model_dir.join(f);
            if !p.is_file() {
                return Err(AppError::Storage(format!(
                    "embed model missing required file: {f}"
                )));
            }
        }
        Ok(Self {
            model_dir,
            query_prefix: query_prefix.into(),
            passage_prefix: passage_prefix.into(),
            inner: Mutex::new(None),
        })
    }

    /// Pick a compute device: Metal first (the Mac fast path), CPU as a graceful fallback. NEVER
    /// panics — a Metal-init failure logs (no PII) and falls back to CPU.
    fn pick_device() -> Device {
        match Device::new_metal(0) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(target: "embed", error = %e, "metal device init failed; falling back to CPU");
                Device::Cpu
            }
        }
    }

    /// Lazily load (once) + return the cached engine. Serializes concurrent first-loads behind the
    /// mutex; a load failure surfaces as `Err` and leaves the cache empty (a later call may retry).
    fn loaded(&self) -> Result<Arc<Loaded>> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| AppError::Storage("embed model mutex poisoned".into()))?;
        if let Some(l) = guard.as_ref() {
            return Ok(l.clone());
        }

        let config_path = self.model_dir.join("config.json");
        let tokenizer_path = self.model_dir.join("tokenizer.json");
        let weights_path = self.model_dir.join("model.safetensors");

        let config_str = std::fs::read_to_string(&config_path)
            .map_err(|e| AppError::Storage(format!("read embed config.json: {e}")))?;
        let config: Config = serde_json::from_str(&config_str)
            .map_err(|e| AppError::Storage(format!("parse embed config.json: {e}")))?;

        let mut tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| AppError::Storage(format!("load embed tokenizer.json: {e}")))?;
        // Truncate to the model's own position-embedding limit — see MAX_SEQ_LEN's doc comment.
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: MAX_SEQ_LEN,
                strategy: TruncationStrategy::LongestFirst,
                direction: TruncationDirection::Right,
                stride: 0,
            }))
            .map_err(|e| AppError::Storage(format!("configure embed tokenizer truncation: {e}")))?;

        let device = Self::pick_device();
        // SAFETY: `from_mmaped_safetensors` mmaps the weights read-only; the file is a trusted model
        // artifact we downloaded ourselves into the app models dir. The `VarBuilder` borrows the mmap
        // for the lifetime of the load (the tensors are copied onto `device`).
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(&[&weights_path], DTYPE, &device)
                .map_err(|e| AppError::Storage(format!("mmap embed safetensors: {e}")))?
        };
        let model = BertModel::load(vb, &config)
            .map_err(|e| AppError::Storage(format!("load embed BERT weights: {e}")))?;

        // e5-small is a 384-hidden model; guard the contract so a wrong-width model fails LOUD here
        // rather than producing off-width vectors the vec0 insert would silently mishandle.
        if config.hidden_size != EMBED_DIM {
            return Err(AppError::Storage(format!(
                "embed model hidden_size {} != EMBED_DIM {EMBED_DIM} (vec0 schema mismatch)",
                config.hidden_size
            )));
        }

        let arc = Arc::new(Loaded {
            model,
            tokenizer,
            device,
        });
        *guard = Some(arc.clone());
        tracing::info!(target: "embed", "embed model loaded");
        Ok(arc)
    }
}

impl Embedder for CandleBertEmbedder {
    fn dim(&self) -> usize {
        EMBED_DIM
    }

    /// Override the trait default so the SELECTED model's passage prefix is applied (not the module
    /// const). e5 and mmlw share `"passage: "`, so this is behavior-identical for both bundled models
    /// while correctly generalizing to any future model whose convention differs.
    fn embed_passage(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let prefixed: Vec<String> = texts
            .iter()
            .map(|t| format!("{}{t}", self.passage_prefix))
            .collect();
        self.embed(&prefixed)
    }

    /// Override the trait default so the SELECTED model's query prefix is applied. See
    /// [`Self::embed_passage`].
    fn embed_query(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let prefixed: Vec<String> = texts
            .iter()
            .map(|t| format!("{}{t}", self.query_prefix))
            .collect();
        self.embed(&prefixed)
    }

    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let loaded = self.loaded()?;
        let device = &loaded.device;

        // Tokenize the batch (padding to the longest sequence so the batch is one rectangular tensor).
        let encodings = loaded
            .tokenizer
            .encode_batch(texts.to_vec(), true)
            .map_err(|e| AppError::Storage(format!("embed tokenize failed: {e}")))?;

        let mut id_rows: Vec<Vec<u32>> = Vec::with_capacity(encodings.len());
        let mut mask_rows: Vec<Vec<u32>> = Vec::with_capacity(encodings.len());
        let max_len = encodings
            .iter()
            .map(|e| e.get_ids().len())
            .max()
            .unwrap_or(0);
        for enc in &encodings {
            let mut ids = enc.get_ids().to_vec();
            let mut mask = enc.get_attention_mask().to_vec();
            // Right-pad to the batch max so all rows share a shape (token id 0 + mask 0 are ignored
            // by the attention mask anyway).
            ids.resize(max_len, 0);
            mask.resize(max_len, 0);
            id_rows.push(ids);
            mask_rows.push(mask);
        }

        let batch = id_rows.len();
        let flat_ids: Vec<u32> = id_rows.into_iter().flatten().collect();
        let flat_mask: Vec<u32> = mask_rows.iter().flatten().copied().collect();

        let input_ids = Tensor::from_vec(flat_ids, (batch, max_len), device)
            .map_err(|e| AppError::Storage(format!("embed input tensor: {e}")))?;
        let attention_mask = Tensor::from_vec(flat_mask.clone(), (batch, max_len), device)
            .map_err(|e| AppError::Storage(format!("embed mask tensor: {e}")))?;
        let token_type_ids = input_ids
            .zeros_like()
            .map_err(|e| AppError::Storage(format!("embed token-type tensor: {e}")))?;

        // [batch, seq, hidden]
        let sequence = loaded
            .model
            .forward(&input_ids, &token_type_ids, Some(&attention_mask))
            .map_err(|e| AppError::Storage(format!("embed forward failed: {e}")))?;

        // MEAN-POOL over tokens with the attention mask (e5 requires mean-pooling, NOT the CLS token),
        // then L2-normalize each row.
        let pooled = mean_pool(&sequence, &attention_mask)
            .map_err(|e| AppError::Storage(format!("embed mean-pool failed: {e}")))?;
        let normed = l2_normalize(&pooled)
            .map_err(|e| AppError::Storage(format!("embed normalize failed: {e}")))?;

        let out: Vec<Vec<f32>> = normed
            .to_vec2::<f32>()
            .map_err(|e| AppError::Storage(format!("embed extract vectors: {e}")))?;

        // Width contract: every vector MUST be EMBED_DIM (the vec0 column width).
        for v in &out {
            if v.len() != EMBED_DIM {
                return Err(AppError::Storage(format!(
                    "embed produced width {} != EMBED_DIM {EMBED_DIM}",
                    v.len()
                )));
            }
        }
        Ok(out)
    }
}

/// Masked mean-pool a `[batch, seq, hidden]` tensor over the sequence dimension, weighting each token
/// by its attention mask, to a `[batch, hidden]` tensor. Pad tokens (mask 0) contribute nothing.
fn mean_pool(sequence: &Tensor, attention_mask: &Tensor) -> candle_core::Result<Tensor> {
    // mask: [batch, seq] → [batch, seq, 1] float, broadcast over hidden.
    let mask = attention_mask.to_dtype(DTYPE)?.unsqueeze(2)?; // [batch, seq, 1]
    let masked = sequence.broadcast_mul(&mask)?; // [batch, seq, hidden]
    let summed = masked.sum(1)?; // [batch, hidden]
                                 // Per-row token count (clamped to >=1 to avoid div-by-zero on an all-pad row).
    let counts = mask.sum(1)?; // [batch, 1]
    let counts = counts.clamp(1f32, f32::INFINITY)?;
    summed.broadcast_div(&counts)
}

/// L2-normalize each row of a `[batch, hidden]` tensor (e5 vectors are unit-length; cosine == dot).
fn l2_normalize(x: &Tensor) -> candle_core::Result<Tensor> {
    let norm = x.sqr()?.sum_keepdim(1)?.sqrt()?; // [batch, 1]
                                                 // Clamp the norm away from 0 so an all-zero row stays finite (no NaN).
    let norm = norm.clamp(1e-12f32, f32::INFINITY)?;
    x.broadcast_div(&norm)
}

/// On-Mac smoke test — does the e5 safetensors actually LOAD via candle + produce a sane,
/// L2-normalized, dimension-correct, semantically-ordered embedding? `#[ignore]`d (needs the model
/// on disk + Metal), so it never runs in the normal `cargo test` loop. Run:
///
/// ```text
/// cargo test embed::candle_bert::smoke -- --ignored --nocapture
/// ```
#[cfg(test)]
mod smoke {
    use super::CandleBertEmbedder;
    use crate::embed::{embed_model_dir, Embedder, EMBED_DIM};

    #[test]
    #[ignore = "needs the e5 model on disk + Metal; run manually on a Mac"]
    fn e5_loads_and_embeds() {
        let dir = embed_model_dir().expect("embed_model_dir");
        assert!(
            dir.join("model.safetensors").is_file(),
            "e5 model not found at {dir:?}"
        );
        let e = CandleBertEmbedder::new(dir, "query: ", "passage: ")
            .expect("construct CandleBertEmbedder");

        let passages = vec![
            "Notatki ze spotkania o projekcie Atlas i terminie integracji API.".to_string(),
            "The quarterly budget review is scheduled for next Friday.".to_string(),
        ];
        let pv = e.embed_passage(&passages).expect("embed_passage failed");
        assert_eq!(pv.len(), 2);
        assert_eq!(pv[0].len(), EMBED_DIM, "wrong embedding dim");

        let norm: f32 = pv[0].iter().map(|x| x * x).sum::<f32>().sqrt();
        println!(
            "dim={} L2-norm={norm:.4} first5={:?}",
            pv[0].len(),
            &pv[0][..5]
        );
        assert!((norm - 1.0).abs() < 0.05, "not L2-normalized: {norm}");

        // Semantic sanity: a Polish query about the meeting should be closer to the Polish
        // meeting passage than to the unrelated English budget passage.
        let q = e
            .embed_query(&["o czym było spotkanie projektu Atlas".to_string()])
            .expect("embed_query failed");
        let cos = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
        let sim_related = cos(&q[0], &pv[0]);
        let sim_unrelated = cos(&q[0], &pv[1]);
        println!(
            "cos(query, PL-meeting)={sim_related:.4}  cos(query, EN-budget)={sim_unrelated:.4}"
        );
        assert!(
            sim_related > sim_unrelated,
            "PL query should rank the PL meeting passage above the unrelated EN passage"
        );
    }
}
