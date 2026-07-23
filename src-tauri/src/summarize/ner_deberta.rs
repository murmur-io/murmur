//! Real on-device PERSON-name redaction (Phase D) — a [`NameRedactor`](crate::summarize::redact::NameRedactor)
//! backed by candle-transformers 0.10.2 token-classification NER (DeBERTa-v2, Metal). ALWAYS compiled;
//! [`active_name_redactor`](crate::summarize::redact::active_name_redactor) selects it at runtime when
//! the NER model dir is present, else ships the dependency-free
//! [`NoopNameRedactor`](crate::summarize::redact::NoopNameRedactor) instead.
//!
//! ## What this does (and why it is SAFE-BY-DESIGN)
//!
//! The redaction firewall ([`RedactingProvider`](crate::summarize::redact::RedactingProvider)) scrubs
//! emails/cards/phones with regex but, until now, let personal NAMES egress (the honest-scope note in
//! `redact.rs`). This module is the real NER that closes that gap: it runs a multilingual DeBERTa NER
//! over the prompt, decodes BIO **PERSON** spans (`B-PER`/`I-PER`), and replaces each DISTINCT person
//! name with a stable `⟪NAME_n⟫` token per the [`NameRedactor`] contract, so the provider's reply
//! de-tokenizes back to the real names.
//!
//! CRITICAL INVARIANT (lock-review): this redactor ONLY ever REMOVES/MASKS content (a matched PERSON
//! span → a placeholder token). It NEVER adds text to the prompt. Therefore a NER **miss** leaks NO
//! MORE than today's `NoopNameRedactor` — worst case == current production behaviour. It runs
//! ON-DEVICE inside the firewall, BEFORE egress; there is no network in this path
//! ([`download_ner_model`](crate::summarize::redact::download_ner_model) is the only I/O and is
//! inbound-only).
//!
//! ## Model choice (DOCUMENTED — Polish recall is a @Mac eval)
//!
//! candle's `DebertaV2NERModel::load(vb, &config, Some(id2label))` loads a `DebertaV2Model` + a
//! `classifier` linear head and is fully driven by the model's `id2label` (we detect PERSON purely by
//! the `*-PER` label suffix), so this code is model-AGNOSTIC — only the download URL names the repo.
//! The shipped target is a multilingual **mDeBERTa-v3 NER** (`MODEL_HF_BASE` in `redact.rs`) that
//! exposes `model.safetensors` + `tokenizer.json` + a `config.json` carrying an `id2label` with
//! `B-PER`/`I-PER`. If a perfectly turnkey safetensors+id2label mDeBERTa-v3 PER model is unavailable,
//! the closest loadable DeBERTa-v2 token-classifier is used — the decode logic is identical.
//!
//! ## Honest scope (READ THIS)
//!
//! Everything here is **COMPILE-proven only** in the headless CI loop (`cargo build --lib`). What can
//! ONLY be verified on a signed/dev build on a real Mac, with the NER
//! files actually present, is: real NER quality, **Polish PERSON recall**, the BIO decode against the
//! real tokenizer's subword offsets, and **Metal** correctness/perf (Metal-vs-CPU fallback). The pure
//! BIO→span decode + distinct-name→token mapping IS unit-tested headless via injected logits — that
//! is the part this module owns and the part most likely to regress.
//!
//! ## Graceful + crash-safe
//!
//! Every fallible step returns [`AppError`] — config parse, model load, tokenize, forward, argmax
//! NEVER panic or `unwrap`. The heavy safetensors+tokenizer load is LAZY (first `redact_names` call)
//! and cached once per canonical model directory behind a process-global shared slot, so this can
//! back independently-built providers without reloading or blocking app startup.
//! `Device::new_metal(0)` is tried first with a CPU fallback.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use candle_core::{Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::debertav2::{Config, DebertaV2NERModel, Id2Label};
use tokenizers::Tokenizer;

use crate::error::{AppError, Result};
use crate::summarize::redact::{NameRedactor, NER_MODEL_FILES, NER_NAME_TOKEN_PREFIX};

/// Float dtype the NER weights load in. The candle DeBERTa NER example uses F32 on CPU/Metal; we mirror
/// that (the model is small enough that F32 is fine and avoids half-precision argmax surprises).
const NER_DTYPE: candle_core::DType = candle_core::DType::F32;

/// A [`NameRedactor`] running a multilingual DeBERTa NER in-process via candle (Metal, CPU fallback).
/// The model + tokenizer load lazily on the first `redact_names` and are cached behind an `Arc`. The
/// redact method tokenizes WITH char offsets, runs the classifier, argmaxes per token, decodes BIO
/// PERSON spans, and maps each distinct name to a stable `⟪NAME_n⟫` token.
pub struct DebertaNameRedactor {
    /// Directory holding `model.safetensors` + `tokenizer.json` + `config.json`.
    model_dir: PathBuf,
    /// Process-global per-model-dir lazy engine slot. `None` until the first `redact_names` call or
    /// after an explicit recording/residency-boundary eviction.
    inner: Arc<Mutex<Option<Arc<Loaded>>>>,
}

type CacheSlot<T> = Mutex<Option<Arc<T>>>;
type CacheSlots<T> = HashMap<PathBuf, Arc<CacheSlot<T>>>;
type LoadedSlot = CacheSlot<Loaded>;
type LoadedSlots = CacheSlots<Loaded>;

fn loaded_slots() -> &'static Mutex<LoadedSlots> {
    static SLOTS: OnceLock<Mutex<LoadedSlots>> = OnceLock::new();
    SLOTS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Return the ONE process-global lazy cache slot for `model_dir`. Providers built independently for
/// note generation, Ask, and connectors share the same warm model even across sequential calls and
/// cannot duplicate it in RAM. Serializing slot creation under the registry mutex closes the
/// concurrent-first-provider race; [`release_all_caches`] is the explicit residency-boundary eviction.
fn shared_loaded_slot(model_dir: &Path) -> Arc<LoadedSlot> {
    let mut slots = loaded_slots()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    slots
        .entry(model_dir.to_path_buf())
        .or_insert_with(|| Arc::new(Mutex::new(None)))
        .clone()
}

/// Clear every cached value while preserving slot identity for providers that survive the boundary.
/// Generic so the eviction contract can be unit-tested without constructing a real candle model or
/// touching Metal.
fn release_live_slots<T>(slots: &CacheSlots<T>) -> usize {
    let mut released = 0usize;
    for slot in slots.values() {
        if slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .is_some()
        {
            released += 1;
        }
    }
    released
}

/// Release every still-live provider's lazy NER weights at a recording model boundary. Provider
/// wrappers may outlive one redaction call (auto-organize can reuse them), so clear every shared
/// model-dir slot in place. The registry retains only empty slot identities after eviction — never a
/// provider, prompt, or name — so a surviving wrapper cannot split into a second cache generation.
pub(crate) fn release_all_caches() {
    let slots = loaded_slots()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let released = release_live_slots(&slots);
    if released > 0 {
        tracing::info!(target: "ner", caches = released, "released cached NER model weights");
    }
}

/// The loaded engine: the DeBERTa NER weights, its tokenizer (with offsets), the `id` → label map, and
/// the device they live on.
struct Loaded {
    model: DebertaV2NERModel,
    tokenizer: Tokenizer,
    /// `label_id` → label string (e.g. `1 -> "B-PER"`), from `config.json`'s `id2label`.
    id2label: Id2Label,
    device: Device,
}

impl DebertaNameRedactor {
    /// Build a redactor for the NER files in `model_dir`. CHEAP + non-blocking: it only validates the
    /// three files exist and stores the dir — the safetensors/tokenizer load is deferred to first use,
    /// so this is safe to call from `active_name_redactor` on the startup path. Returns `Err` (never
    /// panics) if a required file is missing.
    pub fn new(model_dir: PathBuf) -> Result<Self> {
        for f in NER_MODEL_FILES {
            let p = model_dir.join(f);
            if !p.is_file() {
                return Err(AppError::Storage(format!(
                    "ner model missing required file: {f}"
                )));
            }
        }
        // Canonicalization makes syntactic aliases (for example a symlinked models root) share the
        // same process-global slot. Validation above already proved the directory is readable; retain
        // the original path only as a graceful fallback if canonicalization itself is unavailable.
        let model_dir = std::fs::canonicalize(&model_dir).unwrap_or(model_dir);
        let inner = shared_loaded_slot(&model_dir);
        Ok(Self { model_dir, inner })
    }

    #[cfg(test)]
    fn new_for_cache_test(model_dir: PathBuf) -> Self {
        let inner = shared_loaded_slot(&model_dir);
        Self { model_dir, inner }
    }

    /// Pick a compute device: Metal first (the Mac fast path), CPU as a graceful fallback. NEVER
    /// panics — a Metal-init failure logs (no PII) and falls back to CPU.
    fn pick_device() -> Device {
        match Device::new_metal(0) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(target: "ner", error = %e, "metal device init failed; falling back to CPU");
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
            .map_err(|_| AppError::Storage("ner model mutex poisoned".into()))?;
        if let Some(l) = guard.as_ref() {
            return Ok(l.clone());
        }

        let config_path = self.model_dir.join("config.json");
        let tokenizer_path = self.model_dir.join("tokenizer.json");
        let weights_path = self.model_dir.join("model.safetensors");

        let config_str = std::fs::read_to_string(&config_path)
            .map_err(|e| AppError::Storage(format!("read ner config.json: {e}")))?;
        let config: Config = serde_json::from_str(&config_str)
            .map_err(|e| AppError::Storage(format!("parse ner config.json: {e}")))?;

        // id2label MUST be present in the config — the NER head width AND our B-PER/I-PER decode both
        // depend on it. Fail LOUD here (no panic) rather than mis-decoding labels later.
        let id2label = config.id2label.clone().ok_or_else(|| {
            AppError::Storage("ner config.json missing id2label (need B-PER/I-PER labels)".into())
        })?;

        let mut tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| AppError::Storage(format!("load ner tokenizer.json: {e}")))?;
        // We need byte/char offsets to map a token span back to the exact substring to mask.
        tokenizer.with_padding(None).with_truncation(None).ok();

        let device = Self::pick_device();
        // SAFETY: `from_mmaped_safetensors` mmaps the weights read-only; the file is a trusted model
        // artifact we downloaded ourselves into the app models dir. The `VarBuilder` borrows the mmap
        // for the lifetime of the load (the tensors are copied onto `device`).
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(&[&weights_path], NER_DTYPE, &device)
                .map_err(|e| AppError::Storage(format!("mmap ner safetensors: {e}")))?
        };
        let model = DebertaV2NERModel::load(vb, &config, Some(id2label.clone()))
            .map_err(|e| AppError::Storage(format!("load ner DeBERTa weights: {e}")))?;

        let arc = Arc::new(Loaded {
            model,
            tokenizer,
            id2label,
            device,
        });
        *guard = Some(arc.clone());
        tracing::info!(target: "ner", labels = id2label_will_log(&arc.id2label), "ner model loaded");
        Ok(arc)
    }

    /// The real work: run the NER over `text` and return `(scrubbed, pairs)`. Any model/tensor error
    /// surfaces as `Err` (the caller treats `Err` as "no-op, leak nothing extra" — see
    /// [`NameRedactor::redact_names`] impl). NEVER panics.
    fn run(&self, text: &str) -> Result<(String, Vec<(String, String)>)> {
        if text.trim().is_empty() {
            return Ok((text.to_string(), Vec::new()));
        }
        let loaded = self.loaded()?;
        let device = &loaded.device;

        // Tokenize WITH offsets so each subword token knows its [start,end) char span in `text`.
        let encoding = loaded
            .tokenizer
            .encode(text, true)
            .map_err(|e| AppError::Storage(format!("ner tokenize failed: {e}")))?;
        let ids = encoding.get_ids();
        if ids.is_empty() {
            return Ok((text.to_string(), Vec::new()));
        }
        let offsets = encoding.get_offsets(); // &[(usize, usize)] char spans into `text`
        let special = encoding.get_special_tokens_mask(); // 1 for [CLS]/[SEP]/pad

        let seq = ids.len();
        // See the matching autoreleasepool wrap + rationale comment in embed/candle_bert.rs — same
        // Metal-autorelease-drain concern, same zero-new-crate objc2 fix.
        let label_ids: Vec<u32> = objc2::rc::autoreleasepool(|_pool| -> Result<Vec<u32>> {
            let input_ids = Tensor::from_vec(ids.to_vec(), (1, seq), device)
                .map_err(|e| AppError::Storage(format!("ner input tensor: {e}")))?;
            let attention_mask = Tensor::ones((1, seq), candle_core::DType::U32, device)
                .map_err(|e| AppError::Storage(format!("ner mask tensor: {e}")))?;

            // logits: [1, seq, num_labels]
            let logits = loaded
                .model
                .forward(&input_ids, None, Some(attention_mask))
                .map_err(|e| AppError::Storage(format!("ner forward failed: {e}")))?;
            let logits = logits
                .to_dtype(candle_core::DType::F32)
                .and_then(|t| t.squeeze(0)) // [seq, num_labels]
                .map_err(|e| AppError::Storage(format!("ner logits reshape: {e}")))?;
            logits
                .argmax(candle_core::D::Minus1)
                .and_then(|t| t.to_vec1::<u32>())
                .map_err(|e| AppError::Storage(format!("ner argmax failed: {e}")))
        })?;

        // Decode BIO PERSON spans into char ranges, then map distinct names → stable tokens.
        let spans = decode_person_spans(&label_ids, offsets, special, &loaded.id2label);
        Ok(apply_person_spans(text, &spans))
    }
}

impl NameRedactor for DebertaNameRedactor {
    fn redact_names(&self, text: &str) -> (String, Vec<(String, String)>) {
        // Fail-SAFE: a model error must NEVER add content or panic — it degrades to the no-op (text
        // unchanged, empty pairs), which leaks no more than today's NoopNameRedactor. We only ever
        // REMOVE content on the happy path.
        match self.run(text) {
            Ok(out) => out,
            Err(e) => {
                tracing::warn!(target: "ner", error = %e, "ner redact failed; passing text through unchanged (no-op)");
                (text.to_string(), Vec::new())
            }
        }
    }
}

/// A decoded PERSON span: the inclusive char range `[start, end)` in the source text and the exact
/// substring it covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PersonSpan {
    pub start: usize,
    pub end: usize,
}

/// True iff `label` denotes a PERSON tag (`B-PER`/`I-PER`, case-insensitive, also `PERSON`/`B-PERSON`).
/// We match on the SUFFIX so the scheme works across `PER`/`PERSON` label vocabularies.
fn is_person_label(label: &str) -> bool {
    let up = label.to_ascii_uppercase();
    up.ends_with("PER") || up.ends_with("PERSON")
}

/// True iff `label` BEGINS a new entity (`B-`). Anything else inside a PERSON run (`I-PER`) continues.
fn is_begin(label: &str) -> bool {
    label.to_ascii_uppercase().starts_with("B-")
}

/// Decode argmax per-token label ids into PERSON char spans using BIO. Special tokens (CLS/SEP/pad)
/// and `O` break any open span. A `B-PER` always starts a fresh span; an `I-PER` extends the current
/// PERSON span (or, leniently, starts one if a model emits `I-PER` without a preceding `B-PER`).
/// Pure + deterministic — this is the unit-tested core.
pub(crate) fn decode_person_spans(
    label_ids: &[u32],
    offsets: &[(usize, usize)],
    special: &[u32],
    id2label: &Id2Label,
) -> Vec<PersonSpan> {
    let mut spans: Vec<PersonSpan> = Vec::new();
    let mut open: Option<PersonSpan> = None;

    let n = label_ids.len().min(offsets.len());
    for i in 0..n {
        // A special token (CLS/SEP/pad) is not real text — it ends any open span and is skipped.
        if special.get(i).copied().unwrap_or(0) == 1 {
            if let Some(s) = open.take() {
                spans.push(s);
            }
            continue;
        }
        let label = id2label
            .get(&label_ids[i])
            .map(String::as_str)
            .unwrap_or("O");
        let (tok_start, tok_end) = offsets[i];
        // A zero-width offset (some tokenizers emit (0,0) for added tokens) carries no text → skip,
        // but do NOT break an open span on it.
        let is_empty_offset = tok_start == tok_end;

        if is_person_label(label) && !is_empty_offset {
            match (&mut open, is_begin(label)) {
                // New entity boundary, or a continuation with no span open → start a fresh span.
                (None, _) | (Some(_), true) => {
                    if is_begin(label) {
                        if let Some(s) = open.take() {
                            spans.push(s);
                        }
                    }
                    open = Some(PersonSpan {
                        start: tok_start,
                        end: tok_end,
                    });
                }
                // I-PER continuing the open span → extend its end.
                (Some(s), false) => {
                    s.end = tok_end;
                }
            }
        } else if !is_empty_offset {
            // A real, non-PERSON token (O / other entity) closes any open span.
            if let Some(s) = open.take() {
                spans.push(s);
            }
        }
    }
    if let Some(s) = open.take() {
        spans.push(s);
    }
    spans
}

/// Replace each PERSON span's substring with a stable `⟪NAME_n⟫` token, mapping DISTINCT names to
/// distinct stable tokens (so the same name anywhere in the text → the same token, satisfying the
/// [`NameRedactor`] stable-token contract). Returns `(scrubbed_text, token→name pairs)`. The pairs
/// are de-duplicated (one entry per distinct name) so [`restore_names`](crate::summarize::redact) maps
/// cleanly. Pure + deterministic.
///
/// Rebuilds the string by walking the ORIGINAL text and substituting only the masked ranges, so
/// non-PERSON text is byte-identical to the input (we only ever REMOVE PERSON spans).
pub(crate) fn apply_person_spans(
    text: &str,
    spans: &[PersonSpan],
) -> (String, Vec<(String, String)>) {
    if spans.is_empty() {
        return (text.to_string(), Vec::new());
    }
    // Sort + drop overlaps (defensive: a well-formed BIO decode yields disjoint ascending spans).
    let mut spans: Vec<PersonSpan> = spans.to_vec();
    spans.sort_by_key(|s| (s.start, s.end));
    let mut clean: Vec<PersonSpan> = Vec::with_capacity(spans.len());
    let mut last_end = 0usize;
    for s in spans {
        if s.start >= last_end && s.end > s.start && s.end <= text.len() {
            last_end = s.end;
            clean.push(s);
        }
    }

    // Assign a stable token per DISTINCT name (by exact surface string). Insertion order → NAME_1,
    // NAME_2, … in first-appearance order.
    let mut name_to_tok: Vec<(String, String)> = Vec::new(); // name → token (preserves order)
    let mut tok_to_name: Vec<(String, String)> = Vec::new(); // token → name (the returned pairs)

    let mut out = String::with_capacity(text.len());
    let mut cursor = 0usize;
    for s in &clean {
        // Char-boundary safety: only mask when both ends land on UTF-8 boundaries; otherwise skip this
        // span (leave the text intact) rather than panic on a mid-char slice.
        if !text.is_char_boundary(s.start) || !text.is_char_boundary(s.end) {
            continue;
        }
        if s.start < cursor {
            continue;
        }
        out.push_str(&text[cursor..s.start]);
        let name = text[s.start..s.end].to_string();
        let tok = match name_to_tok.iter().find(|(n, _)| n == &name) {
            Some((_, t)) => t.clone(),
            None => {
                let t = format!("{NER_NAME_TOKEN_PREFIX}{}\u{27eb}", name_to_tok.len() + 1); // ⟪NAME_n⟫
                name_to_tok.push((name.clone(), t.clone()));
                tok_to_name.push((t.clone(), name.clone()));
                t
            }
        };
        out.push_str(&tok);
        cursor = s.end;
    }
    out.push_str(&text[cursor..]);
    (out, tok_to_name)
}

/// Non-PII log helper: the COUNT of labels in the model's id2label (never the labels themselves, in
/// case a future model carries person-ish label text). Keeps the "no PII in logs" rule.
fn id2label_will_log(id2label: &Id2Label) -> usize {
    id2label.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Barrier;

    fn cache_test_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("murmur-ner-cache-{tag}-{}", std::process::id()))
    }

    #[test]
    fn providers_for_same_model_dir_share_one_lazy_slot() {
        let dir = cache_test_path("same-dir");
        let first = DebertaNameRedactor::new_for_cache_test(dir.clone());
        let second = DebertaNameRedactor::new_for_cache_test(dir);

        assert!(
            Arc::ptr_eq(&first.inner, &second.inner),
            "independently-built providers must share one lazy model slot"
        );
    }

    #[test]
    fn sequential_providers_reuse_the_process_global_slot() {
        let dir = cache_test_path("sequential");
        let first_slot = {
            let first = DebertaNameRedactor::new_for_cache_test(dir.clone());
            Arc::downgrade(&first.inner)
        };
        let retained = first_slot
            .upgrade()
            .expect("the process-global registry must retain the warm slot");
        let second = DebertaNameRedactor::new_for_cache_test(dir);

        assert!(
            Arc::ptr_eq(&retained, &second.inner),
            "dropping one provider must not force the next provider to reload NER"
        );
    }

    #[test]
    fn different_model_dirs_keep_distinct_lazy_slots() {
        let first = DebertaNameRedactor::new_for_cache_test(cache_test_path("model-a"));
        let second = DebertaNameRedactor::new_for_cache_test(cache_test_path("model-b"));

        assert!(
            !Arc::ptr_eq(&first.inner, &second.inner),
            "cache identity must include the model directory"
        );
    }

    #[test]
    fn concurrent_provider_construction_deduplicates_the_lazy_slot() {
        const PROVIDERS: usize = 8;
        let dir = cache_test_path("concurrent");
        let gate = Arc::new(Barrier::new(PROVIDERS));
        let workers: Vec<_> = (0..PROVIDERS)
            .map(|_| {
                let dir = dir.clone();
                let gate = gate.clone();
                std::thread::spawn(move || {
                    gate.wait();
                    DebertaNameRedactor::new_for_cache_test(dir).inner
                })
            })
            .collect();
        let slots: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().expect("cache-test worker must not panic"))
            .collect();

        assert!(
            slots
                .iter()
                .skip(1)
                .all(|slot| Arc::ptr_eq(&slots[0], slot)),
            "concurrent first providers must not allocate duplicate load slots"
        );
    }

    #[test]
    fn boundary_release_evicts_values_but_preserves_slot_identity() {
        type MarkerSlot = CacheSlot<usize>;

        let first = Arc::new(Mutex::new(Some(Arc::new(7usize))));
        let second = Arc::new(Mutex::new(Some(Arc::new(9usize))));
        let first_key = cache_test_path("release-first");
        let second_key = cache_test_path("release-second");
        let mut slots: HashMap<PathBuf, Arc<MarkerSlot>> = HashMap::new();
        slots.insert(first_key.clone(), first.clone());
        slots.insert(second_key.clone(), second.clone());

        assert_eq!(release_live_slots(&slots), 2);
        assert!(
            first
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_none(),
            "recording boundary must evict the shared cached model"
        );
        assert!(
            second
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_none(),
            "every model-dir cache must be evicted"
        );
        assert!(
            Arc::ptr_eq(slots.get(&first_key).expect("first slot retained"), &first),
            "eviction must preserve slot identity for surviving providers"
        );
        assert!(
            Arc::ptr_eq(
                slots.get(&second_key).expect("second slot retained"),
                &second
            ),
            "eviction must preserve every keyed slot identity"
        );
    }

    fn id2label() -> Id2Label {
        // A canonical CoNLL-style PER scheme: 0=O, 1=B-PER, 2=I-PER, 3=B-ORG, 4=I-ORG.
        let mut m: HashMap<u32, String> = HashMap::new();
        m.insert(0, "O".into());
        m.insert(1, "B-PER".into());
        m.insert(2, "I-PER".into());
        m.insert(3, "B-ORG".into());
        m.insert(4, "I-ORG".into());
        m
    }

    /// Build (label_ids, offsets, special) for a sequence of (token_text, label_id) over a source
    /// string, prepending a CLS and appending a SEP special token (offsets (0,0), special=1) like a
    /// real DeBERTa encoding. Token offsets are located by scanning the source left-to-right.
    fn fixture(src: &str, toks: &[(&str, u32)]) -> (Vec<u32>, Vec<(usize, usize)>, Vec<u32>) {
        let mut labels = vec![0u32]; // CLS
        let mut offsets = vec![(0usize, 0usize)];
        let mut special = vec![1u32];
        let mut search_from = 0usize;
        for (tok, lab) in toks {
            let rel = src[search_from..]
                .find(tok)
                .expect("token must occur in src");
            let start = search_from + rel;
            let end = start + tok.len();
            labels.push(*lab);
            offsets.push((start, end));
            special.push(0);
            search_from = end;
        }
        labels.push(0); // SEP
        offsets.push((0, 0));
        special.push(1);
        (labels, offsets, special)
    }

    #[test]
    fn decodes_single_person_span() {
        let src = "Anna met Carol";
        // "Anna" = B-PER (split into subwords An/na to exercise span extension), rest O.
        let (labels, offsets, special) =
            fixture(src, &[("Ann", 1), ("a", 2), ("met", 0), ("Carol", 1)]);
        let spans = decode_person_spans(&labels, &offsets, &special, &id2label());
        assert_eq!(spans.len(), 2, "two distinct PERSON spans");
        let (out, pairs) = apply_person_spans(src, &spans);
        assert!(!out.contains("Anna"));
        assert!(!out.contains("Carol"));
        assert!(out.contains("\u{27ea}NAME_1\u{27eb}"));
        assert!(out.contains("\u{27ea}NAME_2\u{27eb}"));
        assert_eq!(pairs.len(), 2);
        // Round-trip: restoring the pairs yields the original.
        let restored = restore(&out, &pairs);
        assert_eq!(restored, src);
    }

    #[test]
    fn distinct_name_maps_to_stable_token_when_repeated() {
        let src = "Bob called Bob again";
        let (labels, offsets, special) =
            fixture(src, &[("Bob", 1), ("called", 0), ("Bob", 1), ("again", 0)]);
        let spans = decode_person_spans(&labels, &offsets, &special, &id2label());
        let (out, pairs) = apply_person_spans(src, &spans);
        // The SAME name → the SAME token both times (stable-token contract).
        assert_eq!(out.matches("\u{27ea}NAME_1\u{27eb}").count(), 2);
        assert!(!out.contains("\u{27ea}NAME_2\u{27eb}"));
        assert_eq!(pairs.len(), 1, "one DISTINCT name → one pair");
        assert_eq!(restore(&out, &pairs), src);
    }

    #[test]
    fn multiword_person_is_one_span() {
        let src = "Anna Kowalska spoke";
        // B-PER "Anna", I-PER "Kowalska" → one merged span "Anna Kowalska".
        let (labels, offsets, special) =
            fixture(src, &[("Anna", 1), ("Kowalska", 2), ("spoke", 0)]);
        let spans = decode_person_spans(&labels, &offsets, &special, &id2label());
        assert_eq!(spans.len(), 1);
        let (out, pairs) = apply_person_spans(src, &spans);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].1, "Anna Kowalska");
        assert!(out.starts_with("\u{27ea}NAME_1\u{27eb} spoke"));
        assert_eq!(restore(&out, &pairs), src);
    }

    #[test]
    fn non_person_entities_are_not_masked() {
        let src = "Acme shipped Atlas";
        // Both ORG/O — NO person → text unchanged, empty pairs (leaks nothing extra, masks nothing).
        let (labels, offsets, special) = fixture(src, &[("Acme", 3), ("shipped", 0), ("Atlas", 4)]);
        let spans = decode_person_spans(&labels, &offsets, &special, &id2label());
        assert!(spans.is_empty());
        let (out, pairs) = apply_person_spans(src, &spans);
        assert_eq!(out, src, "no PERSON → byte-identical text");
        assert!(pairs.is_empty());
    }

    #[test]
    fn b_per_boundary_splits_adjacent_people() {
        // Two people back-to-back, each B-PER → TWO spans, not one merged span.
        let src = "Anna Bob";
        let (labels, offsets, special) = fixture(src, &[("Anna", 1), ("Bob", 1)]);
        let spans = decode_person_spans(&labels, &offsets, &special, &id2label());
        assert_eq!(spans.len(), 2);
        let (out, pairs) = apply_person_spans(src, &spans);
        assert_eq!(pairs.len(), 2);
        assert_eq!(restore(&out, &pairs), src);
    }

    /// Mirror of `restore_names` (private in redact.rs) for round-trip assertions here.
    fn restore(text: &str, pairs: &[(String, String)]) -> String {
        let mut s = text.to_string();
        for (tok, name) in pairs {
            s = s.replace(tok, name);
        }
        s
    }
}

/// On-Mac smoke test — does a real multilingual DeBERTa NER actually LOAD via candle + mask a Polish
/// PERSON name before egress? `#[ignore]`d (needs the model on disk + Metal), so it never runs in the
/// normal `cargo test` loop. Run:
///
/// ```text
/// cargo test summarize::ner_deberta::smoke -- --ignored --nocapture
/// ```
#[cfg(test)]
mod smoke {
    use super::DebertaNameRedactor;
    use crate::summarize::redact::{ner_model_dir, NameRedactor};

    #[test]
    #[ignore = "needs the NER model on disk + Metal; run manually on a Mac"]
    fn deberta_masks_polish_person() {
        let dir = ner_model_dir().expect("ner_model_dir");
        assert!(
            dir.join("model.safetensors").is_file(),
            "ner model not found at {dir:?}"
        );
        let r = DebertaNameRedactor::new(dir).expect("construct DebertaNameRedactor");
        let text = "Anna Kowalska spotkała się z Bobem w piątek.";
        let (scrubbed, pairs) = r.redact_names(text);
        println!("scrubbed={scrubbed:?} pairs={pairs:?}");
        assert!(!scrubbed.contains("Anna Kowalska"), "PERSON must be masked");
        assert!(!pairs.is_empty(), "at least one name detected");
        // Round-trip restores the original.
        let mut restored = scrubbed.clone();
        for (tok, name) in &pairs {
            restored = restored.replace(tok, name);
        }
        assert!(restored.contains("Anna Kowalska"));
    }
}
