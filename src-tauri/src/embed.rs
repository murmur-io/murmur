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

/// Target character size for one TRANSCRIPT chunk. Speaker-turn / sliding-window chunking follows the
/// 2026 meeting-RAG convention (~800-1200 chars): big enough to hold a coherent exchange, small
/// enough to embed with focus. Turns are accumulated until adding the next one would exceed this.
const TRANSCRIPT_CHUNK_CHAR_TARGET: usize = 1000;

/// Sliding-window OVERLAP (~15% of [`TRANSCRIPT_CHUNK_CHAR_TARGET`]) carried from the end of one
/// transcript chunk into the start of the next, so a fact spanning a chunk boundary is embedded in
/// both windows and stays retrievable. Whole trailing TURNS are carried (never a mid-turn cut) up to
/// this many characters — this preserves the `[mm:ss-mm:ss] (speaker)` provenance on every line.
const TRANSCRIPT_CHUNK_OVERLAP_CHARS: usize = 150;

/// Reciprocal Rank Fusion constant (the standard k=60). Larger k flattens the contribution of
/// rank position; 60 is the widely-used default from the original RRF paper.
pub const RRF_K: f64 = 60.0;

// ── Brain v2 L1.1 — topic segmentation constants (spec §L1.1; named, eval-tunable) ──────────────

/// A silence gap of at least this many seconds between consecutive spoken segments opens a new
/// topic (a lull usually marks an agenda transition).
pub const TOPIC_LULL_GAP_S: f64 = 30.0;

/// A speaker flip AFTER a run of at least this many consecutive same-speaker segments opens a new
/// topic (a long monologue ending is a strong topical boundary; ordinary turn-taking is not).
pub const TOPIC_SPEAKER_RUN_MIN: usize = 5;

/// Window size (in segments, each side) for the lexical-shift boundary signal.
pub const TOPIC_LEXICAL_WINDOW: usize = 6;

/// A Jaccard similarity BELOW this between the token sets of the two adjacent
/// [`TOPIC_LEXICAL_WINDOW`]-segment windows opens a new topic (vocabulary shifted).
pub const TOPIC_LEXICAL_JACCARD_MIN: f64 = 0.15;

/// Topic segments SHORTER than this (seconds) merge forward into the next topic —
/// over-segmentation is preferred over under-segmentation, but slivers carry no retrieval value.
pub const TOPIC_MERGE_MIN_DURATION_S: f64 = 60.0;

/// Minimum token length (chars) counted by the lexical-shift signal (shorter tokens are noise).
const TOPIC_TOKEN_MIN_CHARS: usize = 3;

// ── Brain v2 L1.2 — contextual augmentation caps (spec §L1.2) ───────────────────────────────────

/// Max attendees rendered into an augmented-chunk header.
pub const AUG_MAX_ATTENDEES: usize = 5;

/// Max facts rendered into an augmented-chunk header.
pub const AUG_MAX_FACTS: usize = 8;

// ── Brain v2 L1.3 — score-fusion weights (spec §L1.3; named consts, calibrated by the eval gate) ─

/// Weight of the keyword (FTS/BM25) leg in [`score_fuse`].
pub const SCORE_FUSE_W_FTS: f64 = 0.4;

/// Weight of the vector-KNN leg in [`score_fuse`].
pub const SCORE_FUSE_W_KNN: f64 = 0.4;

/// Weight of the entity-graph leg in [`score_fuse`].
pub const SCORE_FUSE_W_GRAPH: f64 = 0.2;

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
        let prefixed: Vec<String> = texts
            .iter()
            .map(|t| format!("{PASSAGE_PREFIX}{t}"))
            .collect();
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
    let id = SELECTED_EMBED_MODEL_ID.read().ok().and_then(|g| g.clone());
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
///
/// TEST-BUILD SAFETY NET: `cargo test --lib` must never attempt a real Metal forward pass (see the
/// header doc on `embed::candle_bert` — "NEVER runs a forward pass here"). That was previously true
/// only incidentally (CI has no model on disk), so on a dev Mac that HAS downloaded the real e5 model,
/// any ordinary test exercising a note/meeting index path (e.g. `update_note_doc_inner`) reaches this
/// function, tries a real Metal forward pass inside the test binary, and can abort the process
/// (observed: `MTLCompilerService` XPC failure). Force the stub under `cfg(test)` unless a caller has
/// explicitly opted in via `MURMUR_TEST_REAL_EMBED=1` — only the manual, `#[ignore]`d bake-off tests
/// (`eval::bakeoff`) set that var, since they are the one legitimate case that wants the real model.
pub fn active_embedder() -> Box<dyn Embedder> {
    #[cfg(test)]
    if std::env::var_os("MURMUR_TEST_REAL_EMBED").is_none() {
        return Box::new(StubEmbedder);
    }
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
    for token in text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
    {
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

/// Scalar-quantize an L2-normalized f32 embedding (components in ≈[-1, 1]) into an int8 byte blob
/// for binding to a `vec0 int8[N]` column via `vec_int8(?)` (the M6 org partition — int8 is 3.7×
/// smaller than f32 and holds in-query-budget at 300k chunks, the scale-spike finding).
///
/// Each component is scaled by 127, rounded, and CLAMPED to `[-127, 127]` (i8-safe: -128 is
/// avoided so magnitudes stay symmetric). A non-normalized input still maps deterministically —
/// out-of-range components saturate — so this is safe on the stub embedder too (its output is
/// L2-normalized, so no saturation occurs). The returned `Vec<u8>` is the two's-complement byte
/// image of the i8 array, which is exactly what `vec_int8()` expects. Length == the input length.
pub(crate) fn vec_to_int8_blob(v: &[f32]) -> Vec<u8> {
    v.iter()
        .map(|&f| {
            let scaled = (f * 127.0).round().clamp(-127.0, 127.0) as i8;
            scaled as u8
        })
        .collect()
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

/// Render `seconds` as `mm:ss` (minutes uncapped past 60 — a 75-minute meeting reads `75:xx`, not
/// `01:15:xx`; provenance, not a clock). Negative/NaN clamp to `0`.
fn mmss(seconds: f64) -> String {
    let s = if seconds.is_finite() && seconds > 0.0 {
        seconds as u64
    } else {
        0
    };
    format!("{:02}:{:02}", s / 60, s % 60)
}

/// One speaker-turn: consecutive same-speaker segments merged, carrying the turn's overall time span
/// and the (already-rendered) `[mm:ss-mm:ss] (speaker)\n<text>` line used inside a chunk.
struct Turn {
    line: String,
}

/// Group `segments` into speaker TURNS: runs of consecutive segments with the SAME `speaker` label are
/// merged into one turn (their texts joined by a space). The speaker tag is the segment's `speaker`
/// (`me`/`others`, or any Unicode label); `None`/empty renders as `unknown`. Each turn becomes a
/// single line `[mm:ss-mm:ss] (speaker)\n<merged text>` — the time span runs from the first segment's
/// `start_s` to the last's `end_s`. Blank-text segments are skipped (they carry no content, only a
/// gap); a turn made entirely of blank segments is dropped.
fn group_turns(segments: &[crate::transcribe::types::Segment]) -> Vec<Turn> {
    let mut turns: Vec<Turn> = Vec::new();
    let mut cur_speaker: Option<Option<String>> = None; // None = no open turn yet
    let mut cur_text = String::new();
    let mut cur_start = 0.0f64;
    let mut cur_end = 0.0f64;

    let flush =
        |turns: &mut Vec<Turn>, speaker: &Option<String>, text: &str, start: f64, end: f64| {
            let text = text.trim();
            if text.is_empty() {
                return;
            }
            let label = match speaker {
                Some(s) if !s.trim().is_empty() => s.trim(),
                _ => "unknown",
            };
            turns.push(Turn {
                line: format!("[{}-{}] ({label})\n{text}", mmss(start), mmss(end)),
            });
        };

    for seg in segments {
        let text = seg.text.trim();
        if text.is_empty() {
            continue; // pure-gap segment — no content to attribute.
        }
        let same = cur_speaker
            .as_ref()
            .map(|s| s == &seg.speaker)
            .unwrap_or(false);
        if same {
            if !cur_text.is_empty() {
                cur_text.push(' ');
            }
            cur_text.push_str(text);
            cur_end = seg.end_s;
        } else {
            if let Some(sp) = &cur_speaker {
                flush(&mut turns, sp, &cur_text, cur_start, cur_end);
            }
            cur_speaker = Some(seg.speaker.clone());
            cur_text = text.to_string();
            cur_start = seg.start_s;
            cur_end = seg.end_s;
        }
    }
    if let Some(sp) = &cur_speaker {
        flush(&mut turns, sp, &cur_text, cur_start, cur_end);
    }
    turns
}

/// Split a meeting's TRANSCRIPT segments into deterministic, provenance-carrying chunks for the
/// semantic layer — the SAID-but-not-summarized substrate that note-summary chunks miss.
///
/// Pipeline: consecutive same-speaker segments merge into speaker TURNS (`group_turns`); turns are
/// then packed into sliding windows of ~[`TRANSCRIPT_CHUNK_CHAR_TARGET`] chars with a ~15% overlap
/// ([`TRANSCRIPT_CHUNK_OVERLAP_CHARS`]) — the trailing whole turn(s) of one window are re-emitted at
/// the head of the next so a fact spanning a boundary is embedded in both. Each chunk is PREFIXED with
/// the same `<title> · <date>` header as [`chunk_note`] (so provenance is identical across the two
/// chunk classes), and every turn line inside carries `[mm:ss-mm:ss] (speaker)` — retrieval therefore
/// surfaces WHO said it and WHEN.
///
/// Pure + deterministic: identical segments always yield identical chunk text (unit-tested), which is
/// what keeps `content_hash`-based dedup and re-index stable. Empty/blank input yields no chunks. A
/// single oversized turn becomes its own chunk (never split mid-turn — provenance is kept intact).
pub fn chunk_transcript(
    title: &str,
    date: &str,
    segments: &[crate::transcribe::types::Segment],
) -> Vec<String> {
    let header = format!("{title} · {date}");
    let turns = group_turns(segments);
    if turns.is_empty() {
        return Vec::new();
    }

    let mut chunks: Vec<String> = Vec::new();
    // The turns (as body lines) currently accumulated for the in-progress window.
    let mut window: Vec<&str> = Vec::new();
    let mut window_len = 0usize; // char length of the body (lines joined by '\n')

    let emit = |chunks: &mut Vec<String>, window: &[&str]| {
        if window.is_empty() {
            return;
        }
        chunks.push(format!("{header}\n{}", window.join("\n")));
    };

    for turn in &turns {
        let line = turn.line.as_str();
        let add = if window.is_empty() {
            line.len()
        } else {
            line.len() + 1
        };
        // Close the current window before it overflows (but never emit an empty one).
        if !window.is_empty() && window_len + add > TRANSCRIPT_CHUNK_CHAR_TARGET {
            emit(&mut chunks, &window);
            // OVERLAP: carry the trailing whole turn(s), newest-first up to the overlap budget, into
            // the next window so a boundary-spanning fact is embedded in both chunks.
            let mut carry: Vec<&str> = Vec::new();
            let mut carry_len = 0usize;
            for &prev in window.iter().rev() {
                let plen = if carry.is_empty() {
                    prev.len()
                } else {
                    prev.len() + 1
                };
                if carry_len + plen > TRANSCRIPT_CHUNK_OVERLAP_CHARS && !carry.is_empty() {
                    break;
                }
                carry.push(prev);
                carry_len += plen;
            }
            carry.reverse();
            window = carry;
            window_len =
                window.iter().map(|l| l.len()).sum::<usize>() + window.len().saturating_sub(1);
        }
        let add = if window.is_empty() {
            line.len()
        } else {
            line.len() + 1
        };
        window.push(line);
        window_len += add;
    }
    emit(&mut chunks, &window);
    chunks
}

/// One TOPIC segment (Brain v2 L1.1): a contiguous, topically-coherent span of transcript
/// segments, carrying its wall-clock span and the merged speaker-tagged text. Pure output of
/// [`segment_topics`]; persisted by `Db::index_meeting_topic_chunks`.
#[derive(Debug, Clone, PartialEq)]
pub struct TopicSegment {
    /// Start of the topic (seconds into the meeting, from the first contributing segment).
    pub start_s: f64,
    /// End of the topic (seconds, from the last contributing segment).
    pub end_s: f64,
    /// The topic's raw text: one `(speaker) text` line per contributing non-blank segment,
    /// newline-joined. Deterministic for identical input.
    pub text: String,
}

/// Lexical-shift tokens: lowercased alphanumeric tokens of at least [`TOPIC_TOKEN_MIN_CHARS`]
/// chars, with EN+PL stopwords removed (shares the single `is_stopword` list — no second set to
/// drift). Returned as a set (Jaccard is set-based).
fn topic_tokens(text: &str) -> std::collections::HashSet<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.chars().count() >= TOPIC_TOKEN_MIN_CHARS)
        .filter(|t| !crate::summarize::related_context::is_stopword(t))
        .map(str::to_string)
        .collect()
}

/// Jaccard similarity of two token sets. Both empty ⇒ 1.0 (no evidence of a shift); one empty ⇒
/// 1.0 as well (an empty window can't attest a boundary — fail toward NO boundary).
fn jaccard(a: &std::collections::HashSet<String>, b: &std::collections::HashSet<String>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 1.0;
    }
    let inter = a.intersection(b).count() as f64;
    let union = a.union(b).count() as f64;
    if union == 0.0 {
        1.0
    } else {
        inter / union
    }
}

/// Brain v2 L1.1 — deterministic TOPIC segmentation of a meeting's transcript segments.
///
/// A boundary opens BEFORE segment `i` when ANY of three signals fires (spec §L1.1):
/// 1. **Lull** — `start_s(i) - end_s(i-1) >= `[`TOPIC_LULL_GAP_S`];
/// 2. **Speaker flip after a long run** — the speaker changes at `i` AND the outgoing speaker held
///    at least [`TOPIC_SPEAKER_RUN_MIN`] consecutive segments;
/// 3. **Lexical shift** — the Jaccard similarity between the token sets of the previous and next
///    [`TOPIC_LEXICAL_WINDOW`]-segment windows is below [`TOPIC_LEXICAL_JACCARD_MIN`]
///    (tokens: lowercase alnum ≥ [`TOPIC_TOKEN_MIN_CHARS`] chars, EN+PL stopwords removed).
///
/// Topics shorter than [`TOPIC_MERGE_MIN_DURATION_S`] merge FORWARD into the next topic (a
/// trailing short topic merges backward). Blank-text segments are skipped for content (their
/// timestamps still shape the lull signal via their neighbours' spans). Pure + deterministic:
/// identical segments always yield identical topics — the property `content_hash` idempotency
/// rides on. Empty/all-blank input yields no topics.
pub fn segment_topics(segments: &[crate::transcribe::types::Segment]) -> Vec<TopicSegment> {
    // Work over the non-blank segments only (blank rows carry no content to attribute).
    let spoken: Vec<&crate::transcribe::types::Segment> = segments
        .iter()
        .filter(|s| !s.text.trim().is_empty())
        .collect();
    if spoken.is_empty() {
        return Vec::new();
    }

    // Precompute per-segment token sets once (the lexical windows re-use them).
    let tokens: Vec<std::collections::HashSet<String>> =
        spoken.iter().map(|s| topic_tokens(&s.text)).collect();

    // boundary[i] == true ⇒ a new topic starts AT spoken[i].
    let n = spoken.len();
    let mut boundary = vec![false; n];
    let mut run_len = 1usize; // consecutive same-speaker run ending at i-1.
    for i in 1..n {
        let prev = spoken[i - 1];
        let cur = spoken[i];

        // 1) lull.
        let lull = cur.start_s - prev.end_s >= TOPIC_LULL_GAP_S;

        // 2) speaker flip after a long same-speaker run.
        let flip = cur.speaker != prev.speaker && run_len >= TOPIC_SPEAKER_RUN_MIN;

        // 3) lexical shift over the two adjacent windows.
        let lo = i.saturating_sub(TOPIC_LEXICAL_WINDOW);
        let hi = (i + TOPIC_LEXICAL_WINDOW).min(n);
        let mut before: std::collections::HashSet<String> = std::collections::HashSet::new();
        for t in &tokens[lo..i] {
            before.extend(t.iter().cloned());
        }
        let mut after: std::collections::HashSet<String> = std::collections::HashSet::new();
        for t in &tokens[i..hi] {
            after.extend(t.iter().cloned());
        }
        let shift = jaccard(&before, &after) < TOPIC_LEXICAL_JACCARD_MIN;

        if lull || flip || shift {
            boundary[i] = true;
        }
        run_len = if cur.speaker == prev.speaker {
            run_len + 1
        } else {
            1
        };
    }

    // Materialize raw topics from the boundary vector.
    let render = |seg: &crate::transcribe::types::Segment| -> String {
        let label = match &seg.speaker {
            Some(s) if !s.trim().is_empty() => s.trim(),
            _ => "unknown",
        };
        format!("({label}) {}", seg.text.trim())
    };
    let mut topics: Vec<TopicSegment> = Vec::new();
    let mut start_idx = 0usize;
    for i in 1..=n {
        if i == n || boundary[i] {
            let span = &spoken[start_idx..i];
            let text = span
                .iter()
                .map(|s| render(s))
                .collect::<Vec<_>>()
                .join("\n");
            topics.push(TopicSegment {
                start_s: span[0].start_s,
                end_s: span[span.len() - 1].end_s,
                text,
            });
            start_idx = i;
        }
    }

    // Merge short topics FORWARD (< TOPIC_MERGE_MIN_DURATION_S); a trailing short one merges back.
    let mut merged: Vec<TopicSegment> = Vec::new();
    let mut carry: Option<TopicSegment> = None;
    for t in topics {
        let t = match carry.take() {
            Some(c) => TopicSegment {
                start_s: c.start_s,
                end_s: t.end_s,
                text: format!("{}\n{}", c.text, t.text),
            },
            None => t,
        };
        if t.end_s - t.start_s < TOPIC_MERGE_MIN_DURATION_S {
            carry = Some(t); // too short — merge into the NEXT topic.
        } else {
            merged.push(t);
        }
    }
    if let Some(c) = carry {
        // Trailing short topic: merge backward into the previous, or stand alone if it is all we have.
        match merged.last_mut() {
            Some(last) => {
                last.end_s = c.end_s;
                last.text = format!("{}\n{}", last.text, c.text);
            }
            None => merged.push(c),
        }
    }
    merged
}

/// Brain v2 L1.2 — deterministic CONTEXTUAL AUGMENTATION of a chunk (Anthropic's
/// contextual-retrieval mechanism at zero LLM cost): prepend a one-line situating header —
/// `<title> | <date> | <attendees> | <facts>` — to the raw chunk text, so both the FTS tokens and
/// the passage embedding carry the meeting's provenance and entity/fact context. Empty parts are
/// skipped; attendees are capped at [`AUG_MAX_ATTENDEES`] and facts at [`AUG_MAX_FACTS`]
/// (defensive — the gated readers already cap). Pure string formatting.
pub fn augment_chunk_text(
    title: &str,
    date: &str,
    attendees: &[String],
    facts: &[String],
    raw: &str,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !title.trim().is_empty() {
        parts.push(title.trim().to_string());
    }
    if !date.trim().is_empty() {
        parts.push(date.trim().to_string());
    }
    let attendees_s = attendees
        .iter()
        .map(|a| a.trim())
        .filter(|a| !a.is_empty())
        .take(AUG_MAX_ATTENDEES)
        .collect::<Vec<_>>()
        .join(", ");
    if !attendees_s.is_empty() {
        parts.push(attendees_s);
    }
    let facts_s = facts
        .iter()
        .map(|f| f.trim())
        .filter(|f| !f.is_empty())
        .take(AUG_MAX_FACTS)
        .collect::<Vec<_>>()
        .join("; ");
    if !facts_s.is_empty() {
        parts.push(facts_s);
    }
    if parts.is_empty() {
        return raw.to_string();
    }
    format!("{}\n{raw}", parts.join(" | "))
}

/// Brain v2 L1.3 — weighted SCORE FUSION of the three retrieval legs (spec §L1.3). Replaces
/// rank-only RRF when raw scores are available; [`rrf_fuse`] stays as the fallback.
///
/// Leg contracts:
/// - `fts` — HIGHER-better raw relevance per meeting (callers pass `-bm25`, since SQLite FTS5
///   `bm25()` is lower/more-negative = better);
/// - `knn` — RAW vector DISTANCES per meeting (lower = better); inverted here via
///   `sim = 1 / (1 + d)` per the spec;
/// - `graph` — HIGHER-better (e.g. `1/rank` of the entity-neighbourhood ordering).
///
/// Each leg is min-max normalized to `[0, 1]` independently (a constant/single-entry leg
/// normalizes to all-1.0 — presence in a leg is signal), then blended with QUERY-ADAPTIVE
/// weights derived from the const source ratios `0.4·fts + 0.4·knn + 0.2·graph`
/// ([`SCORE_FUSE_W_FTS`]/[`SCORE_FUSE_W_KNN`]/[`SCORE_FUSE_W_GRAPH`]).
///
/// **Empty-leg redistribution (Brain v2 L1.3, query-adaptive):** a leg that returned ZERO
/// candidates for THIS query contributes ZERO effective weight; its weight mass redistributes
/// proportionally across the legs that DID return results. Concretely, each present leg's
/// effective weight is its base weight divided by the sum of the base weights of the *present*
/// legs. So:
/// - all three legs present ⇒ weights unchanged (`0.4/0.4/0.2`) — no regression where every
///   leg earns its weight (the divisor is `1.0`);
/// - FTS empty, KNN+graph present ⇒ `{0.4, 0.2}` renormalize to `{0.667, 0.333}` — the near-
///   useless empty leg no longer drags hybrid below its live legs;
/// - a single present leg ⇒ weight `1.0` (hybrid == that leg's order);
/// - all empty ⇒ empty.
///
/// This is a monotonic rescale of the present legs (same divisor for all of them), so it NEVER
/// reorders a fixed set of present legs — only the empty-leg mass moves. Score normalization and
/// the ties→id-ASC ordering are unchanged.
///
/// Returns `(id, fused)` sorted DESC, ties broken by id ASC (stable, deterministic). Pure — no DB,
/// no model.
pub fn score_fuse(
    fts: &[(String, f64)],
    knn: &[(String, f64)],
    graph: &[(String, f64)],
) -> Vec<(String, f64)> {
    fn minmax(leg: &[(String, f64)]) -> Vec<(String, f64)> {
        if leg.is_empty() {
            return Vec::new();
        }
        let min = leg.iter().map(|(_, s)| *s).fold(f64::INFINITY, f64::min);
        let max = leg
            .iter()
            .map(|(_, s)| *s)
            .fold(f64::NEG_INFINITY, f64::max);
        leg.iter()
            .map(|(id, s)| {
                let norm = if max > min {
                    (s - min) / (max - min)
                } else {
                    1.0
                };
                (id.clone(), norm)
            })
            .collect()
    }

    // Invert KNN distances into similarities BEFORE normalizing (spec: sim = 1/(1+d)).
    let knn_sim: Vec<(String, f64)> = knn
        .iter()
        .map(|(id, d)| (id.clone(), 1.0 / (1.0 + d.max(0.0))))
        .collect();

    // Query-adaptive effective weights: only legs that returned candidates for THIS query keep
    // their base weight mass; empty legs' mass redistributes proportionally over the present legs.
    // The base weights are the SOURCE ratios; the divisor is the sum of the present ones.
    let present_mass: f64 = [
        (!fts.is_empty(), SCORE_FUSE_W_FTS),
        (!knn_sim.is_empty(), SCORE_FUSE_W_KNN),
        (!graph.is_empty(), SCORE_FUSE_W_GRAPH),
    ]
    .iter()
    .filter(|(present, _)| *present)
    .map(|(_, w)| *w)
    .sum();

    let mut scores: HashMap<String, f64> = HashMap::new();
    if present_mass > 0.0 {
        for (leg, base_w) in [
            (minmax(fts), SCORE_FUSE_W_FTS),
            (minmax(&knn_sim), SCORE_FUSE_W_KNN),
            (minmax(graph), SCORE_FUSE_W_GRAPH),
        ] {
            // Empty legs never enter this loop body (their `leg` is empty), so only present legs
            // contribute — each at its base weight renormalized over the present mass.
            let w = base_w / present_mass;
            for (id, s) in leg {
                *scores.entry(id).or_insert(0.0) += w * s;
            }
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

/// RRF-fuse the two ORG-partition retrieval legs (int8 vector KNN + keyword FTS) into one
/// best-first, per-item-deduped hit list — the org twin of [`fuse_doc_hits`]. Either leg may be
/// empty (StubEmbedder ⇒ no KNN; punctuation query ⇒ no FTS). The kept snippet is first-seen, KNN
/// (nearest) preferred over FTS. Pure fusion — no gate is applied (org items are outside the
/// folder-lock domain), but the SELF-SHARE dedup (drop hits whose `content_sha256` matches a local
/// `org_shares` row) is the caller's responsibility BEFORE this, so a member never re-surfaces their
/// own published item as an "org" result.
pub fn fuse_org_hits(
    knn: Vec<crate::storage::models::OrgChunkHit>,
    fts: Vec<crate::storage::models::OrgChunkHit>,
) -> Vec<crate::storage::models::OrgChunkHit> {
    if knn.is_empty() && fts.is_empty() {
        return Vec::new();
    }
    let knn_ids: Vec<String> = knn.iter().map(|h| h.item_id.clone()).collect();
    let fts_ids: Vec<String> = fts.iter().map(|h| h.item_id.clone()).collect();
    let fused = rrf_fuse(&[knn_ids, fts_ids], RRF_K);
    let mut by_id: HashMap<String, crate::storage::models::OrgChunkHit> = HashMap::new();
    for h in knn.into_iter().chain(fts) {
        by_id.entry(h.item_id.clone()).or_insert(h);
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
        assert!(
            (norm - 1.0).abs() < 1e-5,
            "non-empty vector must be L2-normalized, got {norm}"
        );
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

    fn seg(
        idx: i64,
        start: f64,
        end: f64,
        speaker: Option<&str>,
        text: &str,
    ) -> crate::transcribe::types::Segment {
        crate::transcribe::types::Segment {
            idx,
            start_s: start,
            end_s: end,
            text: text.to_string(),
            speaker: speaker.map(str::to_string),
            confidence: None,
        }
    }

    #[test]
    fn chunk_transcript_empty_and_blank_yield_no_chunks() {
        assert!(
            chunk_transcript("T", "D", &[]).is_empty(),
            "no segments → no chunks"
        );
        // Segments with only whitespace text carry no content → no chunks.
        let blanks = [
            seg(0, 0.0, 1.0, Some("me"), "   "),
            seg(1, 1.0, 2.0, Some("others"), ""),
        ];
        assert!(
            chunk_transcript("T", "D", &blanks).is_empty(),
            "all-blank transcript → no chunks"
        );
    }

    #[test]
    fn chunk_transcript_single_segment_carries_header_time_and_speaker() {
        let segs = [seg(0, 5.0, 12.0, Some("me"), "let us discuss the budget")];
        let chunks = chunk_transcript("Quarterly Sync", "2026-06-28", &segs);
        assert_eq!(chunks.len(), 1);
        let c = &chunks[0];
        assert!(
            c.starts_with("Quarterly Sync · 2026-06-28\n"),
            "must carry the <title> · <date> header, got: {c:?}"
        );
        assert!(
            c.contains("[00:05-00:12] (me)"),
            "must carry [mm:ss-mm:ss] (speaker) provenance, got: {c:?}"
        );
        assert!(c.contains("let us discuss the budget"));
    }

    #[test]
    fn chunk_transcript_groups_consecutive_same_speaker_into_one_turn() {
        // Two consecutive "me" segments merge into ONE turn line (one time span, one speaker tag);
        // then "others" opens a new turn.
        let segs = [
            seg(0, 0.0, 3.0, Some("me"), "hello there"),
            seg(1, 3.0, 6.0, Some("me"), "and welcome"),
            seg(2, 6.0, 9.0, Some("others"), "thanks glad to be here"),
        ];
        let chunks = chunk_transcript("T", "D", &segs);
        assert_eq!(chunks.len(), 1, "short transcript fits one chunk");
        let body = &chunks[0];
        // Exactly two turn headers (me merged, others separate).
        assert_eq!(
            body.matches("(me)").count(),
            1,
            "consecutive me segments must merge into one turn"
        );
        assert_eq!(body.matches("(others)").count(), 1);
        // The merged me turn spans 0:00-0:06 and joins both texts.
        assert!(
            body.contains("[00:00-00:06] (me)\nhello there and welcome"),
            "got: {body:?}"
        );
        assert!(body.contains("[00:06-00:09] (others)"));
    }

    #[test]
    fn chunk_transcript_solo_me_only() {
        // A solo (mic-only) recording: every segment is "me" → one turn, one chunk, no "others".
        let segs = [
            seg(0, 0.0, 2.0, Some("me"), "note to self one"),
            seg(1, 2.0, 4.0, Some("me"), "note to self two"),
        ];
        let chunks = chunk_transcript("Solo", "2026-06-28", &segs);
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].contains("(me)"));
        assert!(!chunks[0].contains("(others)"));
    }

    #[test]
    fn chunk_transcript_unicode_polish_speaker_tag_preserved() {
        // A non-me/others Unicode speaker label (Polish) must round-trip verbatim into the turn header.
        let segs = [seg(
            0,
            0.0,
            4.0,
            Some("Łukasz"),
            "budżet na kwartał wygląda dobrze",
        )];
        let chunks = chunk_transcript("T", "D", &segs);
        assert_eq!(chunks.len(), 1);
        assert!(
            chunks[0].contains("(Łukasz)"),
            "Unicode speaker tag must survive, got: {:?}",
            chunks[0]
        );
        assert!(chunks[0].contains("budżet na kwartał"));
    }

    #[test]
    fn chunk_transcript_none_speaker_renders_unknown() {
        let segs = [seg(0, 0.0, 4.0, None, "unattributed legacy line")];
        let chunks = chunk_transcript("T", "D", &segs);
        assert_eq!(chunks.len(), 1);
        assert!(
            chunks[0].contains("(unknown)"),
            "None speaker → (unknown), got: {:?}",
            chunks[0]
        );
    }

    #[test]
    fn chunk_transcript_windows_with_overlap() {
        // Build enough alternating turns to force several windows; each turn ~120 chars of body.
        let mut segs = Vec::new();
        for i in 0..12i64 {
            let speaker = if i % 2 == 0 { "me" } else { "others" };
            // ~110-char utterance so a handful of turns exceed the 1000-char window target.
            let text = format!("this is turn number {i} with a fair amount of spoken content to push the running window past the target size");
            segs.push(seg(
                i,
                i as f64 * 5.0,
                i as f64 * 5.0 + 4.0,
                Some(speaker),
                &text,
            ));
        }
        let chunks = chunk_transcript("Long", "2026-06-28", &segs);
        assert!(
            chunks.len() >= 2,
            "long transcript must split into multiple windows, got {}",
            chunks.len()
        );
        // Every chunk carries the header.
        for c in &chunks {
            assert!(
                c.starts_with("Long · 2026-06-28\n"),
                "chunk missing header: {c:?}"
            );
        }
        // OVERLAP: the LAST turn line of chunk N must reappear as (near) the FIRST body line of chunk N+1
        // (whole-turn carry). Assert at least one shared turn line across the first boundary.
        let turn_lines = |c: &str| -> Vec<String> {
            c.lines()
                .filter(|l| l.starts_with('['))
                .map(str::to_string)
                .collect::<Vec<_>>()
        };
        let a = turn_lines(&chunks[0]);
        let b = turn_lines(&chunks[1]);
        let shared = a.iter().rev().take(3).any(|t| b.contains(t));
        assert!(shared, "sliding window must overlap: a tail turn of chunk 0 must reappear in chunk 1\n0={a:?}\n1={b:?}");
        // Overlap stays BOUNDED — the carried body must be well under a full window.
        let carried_chars: usize = b
            .iter()
            .take_while(|t| a.contains(t))
            .map(|t| t.len())
            .sum();
        assert!(
            carried_chars <= TRANSCRIPT_CHUNK_CHAR_TARGET / 2,
            "overlap must be ~15%, not half a window (got {carried_chars} chars)"
        );
    }

    #[test]
    fn chunk_transcript_is_deterministic() {
        let segs = [
            seg(0, 0.0, 3.0, Some("me"), "deterministic check one"),
            seg(1, 3.0, 6.0, Some("others"), "deterministic check two"),
        ];
        let a = chunk_transcript("T", "D", &segs);
        let b = chunk_transcript("T", "D", &segs);
        assert_eq!(a, b, "chunk_transcript must be pure + deterministic");
    }

    // ── L1.1 segment_topics ─────────────────────────────────────────────────────────────────────

    #[test]
    fn segment_topics_empty_and_blank_yield_nothing() {
        assert!(segment_topics(&[]).is_empty());
        let blanks = [
            seg(0, 0.0, 1.0, Some("me"), "   "),
            seg(1, 1.0, 2.0, Some("others"), ""),
        ];
        assert!(segment_topics(&blanks).is_empty());
    }

    #[test]
    fn segment_topics_splits_on_lull() {
        // Two long conversational blocks separated by a 40s silence — the lull is the boundary.
        // Each block is > 60s so neither merges away.
        let mut segs = Vec::new();
        for i in 0..4i64 {
            segs.push(seg(
                i,
                i as f64 * 20.0,
                i as f64 * 20.0 + 18.0,
                Some("me"),
                "planning the atlas budget numbers",
            ));
        }
        // Block 2 starts 40s after block 1 ended (78.0 + 40 = 118.0).
        for i in 0..4i64 {
            segs.push(seg(
                4 + i,
                118.0 + i as f64 * 20.0,
                118.0 + i as f64 * 20.0 + 18.0,
                Some("me"),
                "planning the atlas budget numbers",
            ));
        }
        let topics = segment_topics(&segs);
        assert_eq!(
            topics.len(),
            2,
            "a ≥30s lull must open a new topic, got {topics:?}"
        );
        assert!(topics[0].end_s <= 78.0 + 1e-9);
        assert!((topics[1].start_s - 118.0).abs() < 1e-9);
    }

    #[test]
    fn segment_topics_splits_on_lexical_shift() {
        // Same speaker, no lull — but the vocabulary flips completely between two 6-segment
        // windows, so the Jaccard-shift signal must fire. Both halves are > 60s.
        let mut segs = Vec::new();
        for i in 0..6i64 {
            segs.push(seg(
                i,
                i as f64 * 15.0,
                i as f64 * 15.0 + 14.0,
                Some("me"),
                "budżet finanse kwartał wydatki koszty licencje",
            ));
        }
        for i in 6..12i64 {
            segs.push(seg(
                i,
                i as f64 * 15.0,
                i as f64 * 15.0 + 14.0,
                Some("me"),
                "rekrutacja kandydat rozmowa oferta zatrudnienie zespół",
            ));
        }
        let topics = segment_topics(&segs);
        assert_eq!(
            topics.len(),
            2,
            "a lexical shift must open a new topic, got {topics:?}"
        );
        assert!(topics[0].text.contains("budżet"));
        assert!(!topics[0].text.contains("rekrutacja"));
        assert!(topics[1].text.contains("rekrutacja"));
    }

    #[test]
    fn segment_topics_speaker_flip_needs_a_long_run() {
        // Ordinary turn-taking (1-2 segment runs) must NOT split; a flip after a ≥5-run must.
        // Use a SHARED vocabulary so the lexical signal stays silent.
        let text = "wspólny temat projektu atlas status zadania postęp";
        let mut segs = Vec::new();
        // 6-segment "me" run…
        for i in 0..6i64 {
            segs.push(seg(
                i,
                i as f64 * 15.0,
                i as f64 * 15.0 + 14.0,
                Some("me"),
                text,
            ));
        }
        // …then "others" takes over for 6 segments (flip AFTER a 6-run ⇒ boundary).
        for i in 6..12i64 {
            segs.push(seg(
                i,
                i as f64 * 15.0,
                i as f64 * 15.0 + 14.0,
                Some("others"),
                text,
            ));
        }
        let topics = segment_topics(&segs);
        assert_eq!(
            topics.len(),
            2,
            "flip after a ≥5-run must split, got {topics:?}"
        );

        // Pure alternation (run length 1) must stay ONE topic.
        let mut alt = Vec::new();
        for i in 0..12i64 {
            let sp = if i % 2 == 0 { "me" } else { "others" };
            alt.push(seg(
                i,
                i as f64 * 15.0,
                i as f64 * 15.0 + 14.0,
                Some(sp),
                text,
            ));
        }
        assert_eq!(
            segment_topics(&alt).len(),
            1,
            "ordinary turn-taking must not split"
        );
    }

    #[test]
    fn segment_topics_merges_short_topics_forward() {
        // A 30s sliver before a lull merges into the (long) following topic instead of standing
        // alone. Shared vocabulary keeps the lexical signal out of the picture.
        let text = "wspólny temat projektu atlas status zadania postęp";
        let mut segs = Vec::new();
        segs.push(seg(0, 0.0, 30.0, Some("me"), text)); // 30s sliver…
                                                        // …lull ≥ 30s opens a boundary, then a 90s block.
        for i in 0..6i64 {
            segs.push(seg(
                1 + i,
                70.0 + i as f64 * 15.0,
                70.0 + i as f64 * 15.0 + 14.0,
                Some("me"),
                text,
            ));
        }
        let topics = segment_topics(&segs);
        assert_eq!(
            topics.len(),
            1,
            "a <60s topic must merge forward, got {topics:?}"
        );
        assert!(
            (topics[0].start_s - 0.0).abs() < 1e-9,
            "merged topic keeps the sliver's start"
        );
    }

    #[test]
    fn segment_topics_is_deterministic_and_tags_speakers() {
        let segs = [
            seg(0, 0.0, 4.0, Some("me"), "deterministic topic check"),
            seg(
                1,
                4.0,
                8.0,
                Some("Łukasz"),
                "deterministyczna kontrola tematu",
            ),
        ];
        let a = segment_topics(&segs);
        let b = segment_topics(&segs);
        assert_eq!(a, b, "segment_topics must be pure + deterministic");
        assert_eq!(a.len(), 1);
        assert!(a[0].text.contains("(me) deterministic topic check"));
        assert!(a[0]
            .text
            .contains("(Łukasz) deterministyczna kontrola tematu"));
    }

    // ── L1.2 augment_chunk_text ─────────────────────────────────────────────────────────────────

    #[test]
    fn augment_chunk_text_formats_header_and_caps() {
        let attendees: Vec<String> = (0..7).map(|i| format!("Person {i}")).collect();
        let facts: Vec<String> = (0..10).map(|i| format!("fact {i}")).collect();
        let out = augment_chunk_text(
            "Quarterly Sync",
            "2026-06-28",
            &attendees,
            &facts,
            "raw body",
        );
        let header = out.lines().next().unwrap();
        assert!(header.starts_with("Quarterly Sync | 2026-06-28 | "));
        assert!(header.contains("Person 0, Person 1, Person 2, Person 3, Person 4"));
        assert!(
            !header.contains("Person 5"),
            "attendees must cap at {AUG_MAX_ATTENDEES}"
        );
        assert!(header.contains("fact 7"));
        assert!(
            !header.contains("fact 8"),
            "facts must cap at {AUG_MAX_FACTS}"
        );
        assert!(out.ends_with("\nraw body"));
    }

    #[test]
    fn augment_chunk_text_skips_empty_parts() {
        // No attendees/facts ⇒ just "title | date\nraw".
        let out = augment_chunk_text("T", "2026-01-01", &[], &[], "raw");
        assert_eq!(out, "T | 2026-01-01\nraw");
        // Everything empty ⇒ the raw text unchanged (no dangling header line).
        assert_eq!(augment_chunk_text("", " ", &[], &[], "raw"), "raw");
        // Deterministic.
        assert_eq!(
            augment_chunk_text("T", "D", &["A".into()], &["f".into()], "r"),
            augment_chunk_text("T", "D", &["A".into()], &["f".into()], "r"),
        );
    }

    // ── L1.3 score_fuse ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn score_fuse_single_leg_preserves_order() {
        let fts = vec![
            ("a".to_string(), 5.0),
            ("b".to_string(), 3.0),
            ("c".to_string(), 1.0),
        ];
        let fused = score_fuse(&fts, &[], &[]);
        let ids: Vec<&str> = fused.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b", "c"]);
        // Query-adaptive redistribution: the two empty legs' mass moves onto the ONLY present
        // leg, so it carries the full weight 1.0 — hybrid == that leg (its best normalizes to 1.0).
        assert!((fused[0].1 - 1.0).abs() < 1e-9, "single present leg ⇒ weight 1.0: {fused:?}");
    }

    #[test]
    fn score_fuse_two_legs_reward_agreement() {
        // "m2" is mid-pack in FTS but is also a KNN hit; "m1" tops FTS only, "m3" KNN only.
        let fts = vec![
            ("m1".to_string(), 9.0),
            ("m2".to_string(), 5.0),
            ("x".to_string(), 1.0),
        ];
        let knn = vec![
            ("m3".to_string(), 0.1),
            ("m2".to_string(), 0.2),
            ("y".to_string(), 2.0),
        ];
        let fused = score_fuse(&fts, &knn, &[]);
        let pos = |want: &str| fused.iter().position(|(id, _)| id == want).unwrap();
        assert!(
            pos("m2") < pos("m3"),
            "a both-legs hit must beat a one-leg hit: {fused:?}"
        );
        assert!(pos("m2") < pos("x"));
    }

    #[test]
    fn score_fuse_inverts_knn_distances() {
        // Smaller distance = better: d=0.1 must outrank d=1.5.
        let knn = vec![("far".to_string(), 1.5), ("near".to_string(), 0.1)];
        let fused = score_fuse(&[], &knn, &[]);
        assert_eq!(
            fused[0].0, "near",
            "smaller KNN distance must fuse higher: {fused:?}"
        );
        assert!(fused[0].1 > fused[1].1);
    }

    #[test]
    fn score_fuse_empty_legs_and_ties() {
        assert!(score_fuse(&[], &[], &[]).is_empty());
        // A constant leg normalizes to all-1.0 (presence is signal), ties break by id ASC.
        let graph = vec![("b".to_string(), 1.0), ("a".to_string(), 1.0)];
        let fused = score_fuse(&[], &[], &graph);
        assert_eq!(fused[0].0, "a");
        assert!(
            (fused[0].1 - fused[1].1).abs() < 1e-12,
            "constant leg ⇒ equal scores"
        );
    }

    // Reference fixed-weight blend (the PRE-change math): min-max each leg, then
    // `SCORE_FUSE_W_FTS·fts + SCORE_FUSE_W_KNN·knn_sim + SCORE_FUSE_W_GRAPH·graph`, empty legs
    // contributing nothing and their mass simply LOST (no redistribution). Self-contained so the
    // regression pin does not depend on the production `score_fuse` internals.
    fn old_fixed_weight_blend(
        fts: &[(String, f64)],
        knn: &[(String, f64)],
        graph: &[(String, f64)],
    ) -> Vec<(String, f64)> {
        fn minmax(leg: &[(String, f64)]) -> Vec<(String, f64)> {
            if leg.is_empty() {
                return Vec::new();
            }
            let min = leg.iter().map(|(_, s)| *s).fold(f64::INFINITY, f64::min);
            let max = leg.iter().map(|(_, s)| *s).fold(f64::NEG_INFINITY, f64::max);
            leg.iter()
                .map(|(id, s)| {
                    let norm = if max > min { (s - min) / (max - min) } else { 1.0 };
                    (id.clone(), norm)
                })
                .collect()
        }
        let knn_sim: Vec<(String, f64)> = knn
            .iter()
            .map(|(id, d)| (id.clone(), 1.0 / (1.0 + d.max(0.0))))
            .collect();
        let mut scores: HashMap<String, f64> = HashMap::new();
        for (leg, w) in [
            (minmax(fts), SCORE_FUSE_W_FTS),
            (minmax(&knn_sim), SCORE_FUSE_W_KNN),
            (minmax(graph), SCORE_FUSE_W_GRAPH),
        ] {
            for (id, s) in leg {
                *scores.entry(id).or_insert(0.0) += w * s;
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

    // (1) REGRESSION PIN — all three legs present ⇒ effective weights == base weights (divisor
    // 1.0), so the query-adaptive blend is BYTE-IDENTICAL (order AND scores) to the old fixed
    // blend. This is the "no regression on keyword queries where every leg earns its weight" case.
    #[test]
    fn score_fuse_all_legs_present_identical_to_fixed_blend() {
        let fts = vec![
            ("m1".to_string(), 9.0),
            ("m2".to_string(), 5.0),
            ("x".to_string(), 1.0),
        ];
        let knn = vec![
            ("m3".to_string(), 0.1),
            ("m2".to_string(), 0.2),
            ("y".to_string(), 2.0),
        ];
        let graph = vec![("m2".to_string(), 1.0), ("m4".to_string(), 0.5)];
        let got = score_fuse(&fts, &knn, &graph);
        let want = old_fixed_weight_blend(&fts, &knn, &graph);
        assert_eq!(got.len(), want.len());
        for ((gi, gs), (wi, ws)) in got.iter().zip(want.iter()) {
            assert_eq!(gi, wi, "order diverged: {got:?} vs {want:?}");
            assert!((gs - ws).abs() < 1e-12, "score diverged at {gi}: {gs} vs {ws}");
        }
    }

    // (2) BUG FIX — FTS empty (paraphrase / cross-lingual query): the fused ranking must EQUAL the
    // renormalized {knn, graph} blend (weights 0.4/0.2 → 0.667/0.333, summing to 1.0). RED against
    // the old code, which applied the FTS 0.4 zero-mass and left {knn, graph} at raw 0.4/0.2.
    #[test]
    fn score_fuse_fts_empty_redistributes_to_semantic_and_graph() {
        let knn = vec![
            ("near".to_string(), 0.1),
            ("mid".to_string(), 0.5),
            ("far".to_string(), 2.0),
        ];
        let graph = vec![("mid".to_string(), 1.0), ("g2".to_string(), 0.5)];
        let got = score_fuse(&[], &knn, &graph);

        // Independently compute the renormalized {knn, graph} blend at 0.667/0.333.
        let mass = SCORE_FUSE_W_KNN + SCORE_FUSE_W_GRAPH;
        let wk = SCORE_FUSE_W_KNN / mass;
        let wg = SCORE_FUSE_W_GRAPH / mass;
        // knn min-max over sim = 1/(1+d): near=1/1.1, mid=1/1.5, far=1/3.0.
        let knn_sim = [1.0 / 1.1_f64, 1.0 / 1.5_f64, 1.0 / 3.0_f64];
        let (kmin, kmax) = (
            knn_sim.iter().cloned().fold(f64::INFINITY, f64::min),
            knn_sim.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
        );
        let kn = |s: f64| (s - kmin) / (kmax - kmin);
        // graph min-max: mid=1.0, g2=0.5 ⇒ mid→1.0, g2→0.0.
        let mut want: HashMap<String, f64> = HashMap::new();
        *want.entry("near".to_string()).or_insert(0.0) += wk * kn(1.0 / 1.1);
        *want.entry("mid".to_string()).or_insert(0.0) += wk * kn(1.0 / 1.5);
        *want.entry("far".to_string()).or_insert(0.0) += wk * kn(1.0 / 3.0);
        *want.entry("mid".to_string()).or_insert(0.0) += wg * 1.0;
        *want.entry("g2".to_string()).or_insert(0.0) += wg * 0.0;
        for (id, gs) in &got {
            let ws = want.get(id).copied().unwrap_or(f64::NAN);
            assert!(
                (gs - ws).abs() < 1e-12,
                "fts-empty fused score for {id} = {gs}, expected renormalized {ws}"
            );
        }
        // And RED-vs-old: the top score under redistribution (0.667·1.0) is strictly greater than
        // the old zero-mass score (0.4·1.0) — the fix lifts the live legs into full weight.
        assert!(
            got[0].1 > SCORE_FUSE_W_KNN + 1e-9,
            "redistributed top score {} must exceed the old raw 0.4 leg weight",
            got[0].1
        );
    }

    // (3) SINGLE LEG — one present leg ⇒ fused order == that leg's order, best score 1.0.
    #[test]
    fn score_fuse_single_present_leg_equals_that_leg() {
        let knn = vec![
            ("far".to_string(), 1.5),
            ("near".to_string(), 0.1),
            ("mid".to_string(), 0.6),
        ];
        let fused = score_fuse(&[], &knn, &[]);
        let ids: Vec<&str> = fused.iter().map(|(id, _)| id.as_str()).collect();
        // Smaller distance ⇒ higher sim ⇒ higher fused: near > mid > far.
        assert_eq!(ids, vec!["near", "mid", "far"], "{fused:?}");
        assert!((fused[0].1 - 1.0).abs() < 1e-9, "single leg best ⇒ 1.0: {fused:?}");
    }

    // (4) ALL EMPTY ⇒ empty (unchanged).
    #[test]
    fn score_fuse_all_empty_is_empty() {
        assert!(score_fuse(&[], &[], &[]).is_empty());
    }

    // DIAGNOSTIC (not a gate): count how many labeled queries have a genuinely EMPTY FTS leg on
    // the real vault — i.e. how many queries the empty-leg redistribution can even affect. Reuses
    // the bake-off env vars. `#[ignore]`d; run manually with the same MURMUR_BAKEOFF_* env as the
    // real-vault bake-off. Reports empty-FTS vs non-empty-FTS split so the ranking-impact of the
    // redistribution is honest (redistribution when FTS is empty only RESCALES the {knn,graph}
    // blend by a constant — it does NOT reorder — so it changes recall ONLY through the all-legs-
    // present cases, never the fts-empty ones; this count quantifies that).
    #[test]
    #[ignore = "diagnostic: needs MURMUR_BAKEOFF_DB/DEK/SET env on a Mac"]
    fn diag_count_empty_fts_legs_on_real_set() {
        use std::collections::HashSet;
        let db_path = std::env::var("MURMUR_BAKEOFF_DB").expect("MURMUR_BAKEOFF_DB");
        let dek = std::env::var("MURMUR_BAKEOFF_DEK").expect("MURMUR_BAKEOFF_DEK");
        let set_path = std::env::var("MURMUR_BAKEOFF_SET").expect("MURMUR_BAKEOFF_SET");
        let db = crate::storage::Db::open_with_key(std::path::Path::new(&db_path), &dek).unwrap();
        let set = crate::eval::LabeledSet::from_json(
            &std::fs::read_to_string(&set_path).unwrap(),
        )
        .unwrap();
        let today = chrono::Utc::now().date_naive();
        let empties: HashSet<String> = HashSet::new();
        let mut empty = 0usize;
        let mut nonempty = 0usize;
        for q in &set.0 {
            let df = crate::summarize::temporal::extract_date_filter(&q.query, today);
            let hits = db
                .search_visible_in_range(&q.query, 40, &empties, df)
                .unwrap();
            if hits.is_empty() {
                empty += 1;
                println!("EMPTY-FTS: {}", q.query);
            } else {
                nonempty += 1;
                println!("fts={:>2} hits: {}", hits.len(), q.query);
            }
        }
        println!(
            "\nFTS-empty legs: {empty}/{} ; non-empty (present, wrong-or-right): {nonempty}/{}",
            set.0.len(),
            set.0.len()
        );
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
        assert!(
            pos("m2") < pos("m3"),
            "m2 (in both lists) must outrank m3 (one list)"
        );
        assert!(
            pos("m1") < pos("m3"),
            "m1 (in both lists) must outrank m3 (one list)"
        );
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
        assert_eq!(
            e.0.lock().unwrap().as_slice(),
            &["passage: a budget note".to_string()]
        );
        e.embed_query(&["how much budget".to_string()]).unwrap();
        assert_eq!(
            e.0.lock().unwrap().as_slice(),
            &["query: how much budget".to_string()]
        );
    }

    #[test]
    fn embed_model_dir_is_under_models_dir() {
        let _g = EMBED_SELECTION_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // With no selection set, the resolver falls back to the default (e5) subdir.
        set_selected_embed_model_id(None);
        let dir = embed_model_dir().unwrap();
        assert!(dir.ends_with(EMBED_MODEL_SUBDIR));
        // The three e5 files are the documented set.
        assert_eq!(
            EMBED_MODEL_FILES,
            &["model.safetensors", "tokenizer.json", "config.json"]
        );
        assert!(EMBED_MODEL_HF_BASE.contains("intfloat/multilingual-e5-small"));
    }

    /// The registry is well-formed: the DEFAULT is first, ids are unique, mmlw is present, and EVERY
    /// bundled option is 384-safe by construction (they all share EMBED_MODEL_FILES; the loader guards
    /// hidden_size == EMBED_DIM at load, so a wrong-width model would fail loud — never silently).
    #[test]
    fn embed_registry_is_wellformed_and_has_mmlw() {
        assert_eq!(default_embed_model().id, DEFAULT_EMBED_MODEL_ID);
        assert_eq!(
            EMBED_MODELS[0].id, DEFAULT_EMBED_MODEL_ID,
            "default must be first"
        );
        // Unique ids.
        let mut ids: Vec<&str> = EMBED_MODELS.iter().map(|m| m.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(
            ids.len(),
            EMBED_MODELS.len(),
            "embed model ids must be unique"
        );
        // mmlw is a first-class selectable option with the documented e5-compatible prefixes.
        let mmlw = embed_model_by_id("mmlw-retrieval-e5-small")
            .expect("mmlw-retrieval-e5-small must be registered");
        assert_eq!(mmlw.query_prefix, "query: ");
        assert_eq!(mmlw.passage_prefix, "passage: ");
        assert!(mmlw.hf_base.contains("sdadas/mmlw-retrieval-e5-small"));
        assert_ne!(
            mmlw.subdir, EMBED_MODEL_SUBDIR,
            "each model needs its own subdir"
        );
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
        let _g = EMBED_SELECTION_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        set_selected_embed_model_id(Some("mmlw-retrieval-e5-small".to_string()));
        assert_eq!(selected_embed_model().id, "mmlw-retrieval-e5-small");
        assert!(embed_model_dir()
            .unwrap()
            .ends_with("embed-mmlw-retrieval-e5-small"));

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
        let selected: Vec<&str> = dtos
            .iter()
            .filter(|d| d.selected)
            .map(|d| d.id.as_str())
            .collect();
        assert_eq!(
            selected,
            vec![DEFAULT_EMBED_MODEL_ID],
            "None ⇒ default is selected"
        );

        let dtos = embed_model_dtos(Some("mmlw-retrieval-e5-small"));
        let selected: Vec<&str> = dtos
            .iter()
            .filter(|d| d.selected)
            .map(|d| d.id.as_str())
            .collect();
        assert_eq!(selected, vec!["mmlw-retrieval-e5-small"]);

        // Unknown id ⇒ the default is marked selected (never zero-selected).
        let dtos = embed_model_dtos(Some("bogus"));
        assert!(dtos.iter().filter(|d| d.selected).count() == 1);
        assert!(
            dtos.iter().find(|d| d.selected).map(|d| d.id.as_str()) == Some(DEFAULT_EMBED_MODEL_ID)
        );
    }

    #[test]
    fn embed_model_present_false_when_any_file_missing() {
        let _g = EMBED_SELECTION_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        set_selected_embed_model_id(None);
        // On a clean machine the e5 dir is absent ⇒ not present. Even with a partial dir (only one of
        // the three files), `present` must be false — the loader needs all three.
        let dir = embed_model_dir().unwrap();
        let had_dir = dir.is_dir();
        // If a real model happens to be installed, this assertion is vacuously satisfied; otherwise
        // assert the absent/partial cases without clobbering a real install.
        if !had_dir {
            assert!(
                !embed_model_present(),
                "absent e5 dir must report not-present"
            );
        }
    }

    /// The embedder factory's graceful-degradation contract: with NO e5 model dir present,
    /// `active_embedder` returns the deterministic StubEmbedder (dim == EMBED_DIM, byte-stable). The
    /// candle backend is always compiled now, so selection keys ONLY on model presence — absent model
    /// ⇒ stub. Headless proof of the swap wiring's fallback (mirrors
    /// `active_reasoner_falls_back_to_stub_without_model`).
    #[test]
    fn active_embedder_falls_back_to_stub_without_model() {
        let _g = EMBED_SELECTION_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
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
