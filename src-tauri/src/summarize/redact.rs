//! Redaction firewall: scrub high-confidence PII (emails, card-like digit runs, phone numbers)
//! out of any text BEFORE it leaves for an LLM provider, then de-tokenize the reply so the
//! final note still reads with the real values. Wraps any provider at the make_provider seam.
//!
//! Honest scope: regex reliably catches emails / cards / phones. Personal NAMES are additionally
//! masked by the on-device NER name layer ([`NameRedactor`]) when the model is installed.
//!
//! Field coverage (the firewall's egress contract — kept in sync with the doc-comment on
//! [`RedactingProvider::summarize_with_meta`]): EVERY `SummarizeRequest` field that reaches the
//! inner provider is scrubbed there before egress — `transcript` / `related_context` / `user_notes`
//! (regex + NER), `template` / `meta.title_hint` (regex), and `vault_titles` (FILTERED: any title
//! the firewall would alter is dropped, since a masked wikilink target is useless). Only the
//! non-PII format flags (`meta.date_iso`, `meta.language`, `meta.duration_s`) pass verbatim. A new
//! string field is caught by `every_string_field_of_summarize_request_is_scrubbed_or_exempt`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use regex::Regex;

use crate::error::{AppError, Result};
use crate::summarize::egress_log::{EgressEntry, EgressSink, NoopEgressSink};
use crate::summarize::meta::{CallMeta, RedactionCounts};
use crate::summarize::provider::{Availability, SummarizeRequest, SummarizerProvider};
use serde_json::Value;

// ── Phase D model plumbing (shared by the runtime-selected redactor + the download command) ──────
// The real DebertaNameRedactor lives in the sibling `crate::summarize::ner_deberta` module (always
// compiled, declared in `summarize/mod.rs`); this file holds only the model resolver, the
// active-redactor factory, and the inbound-only downloader.

/// The stable `⟪NAME_` token prefix the [`NameRedactor`] family emits (closed by the index + `⟫`).
/// Centralized so the real redactor and the fixtures/tests agree byte-for-byte.
pub(crate) const NER_NAME_TOKEN_PREFIX: &str = "\u{27ea}NAME_";

/// Sub-directory under the shared models dir holding the multilingual DeBERTa NER files
/// (`model.safetensors` + `tokenizer.json` + `config.json`, the `config.json` carrying an `id2label`
/// with `B-PER`/`I-PER`).
pub const NER_MODEL_SUBDIR: &str = "ner-mdeberta-v3-multilingual";

/// The three Hugging Face files the real NER redactor needs, fetched INBOUND-ONLY by
/// [`download_ner_model`]. Each is downloaded into [`NER_MODEL_SUBDIR`].
pub const NER_MODEL_FILES: &[&str] = &["model.safetensors", "tokenizer.json", "config.json"];

/// Hugging Face `resolve/main` base for the chosen multilingual mDeBERTa-v3 token-classification NER.
/// INBOUND ONLY — fetched, never sent meeting content.
///
/// MODEL CHOICE (documented): a multilingual mDeBERTa-v3 NER that ships `model.safetensors` +
/// `tokenizer.json` + a `config.json` whose `id2label` uses CoNLL-style `B-PER`/`I-PER`. candle's
/// `DebertaV2NERModel` is fully driven by that `id2label`, so the decode is model-agnostic; only this
/// URL names the repo. Polish PERSON recall against this specific checkpoint is a @Mac eval (see the
/// module header of `ner_deberta.rs`). Swap this base + re-download to evaluate an alternative
/// loadable DeBERTa-v2 PER checkpoint with zero code change.
pub const NER_MODEL_HF_BASE: &str =
    "https://huggingface.co/Davlan/mdeberta-v3-base-ner-hrl/resolve/main";

/// Resolve the on-disk dir the real NER redactor loads from: `<models_dir>/ner-mdeberta-v3-multilingual/`.
/// Creating the models dir can fail (returns `Err`); the dir itself may not yet exist (that is fine —
/// the caller checks [`ner_model_present`]). NEVER panics.
pub fn ner_model_dir() -> Result<PathBuf> {
    Ok(crate::transcribe::models_dir()?.join(NER_MODEL_SUBDIR))
}

/// `true` when all three NER model files exist in [`ner_model_dir`]. Pure existence probe; a
/// models-dir resolution error is treated as "not present" (graceful — falls back to the no-op),
/// never propagated as a hard error.
pub fn ner_model_present() -> bool {
    match ner_model_dir() {
        Ok(dir) => NER_MODEL_FILES.iter().all(|f| dir.join(f).is_file()),
        Err(_) => false,
    }
}

/// The single active name-redactor used by [`RedactingProvider`] (mirrors
/// [`crate::embed::active_embedder`]). Graceful degradation, in priority order:
/// - the NER model dir is present at [`ner_model_dir`] ([`ner_model_present`]) → the real
///   [`crate::summarize::ner_deberta::DebertaNameRedactor`] (lazy: the model loads on first
///   `redact_names`, not here, so this never blocks startup and never panics);
/// - otherwise (no model, or a construction error) → the dependency-free [`NoopNameRedactor`]. Egress
///   is then byte-identical to before this seam existed (ZERO regression).
///
/// Selection keys ONLY on model presence — the candle NER backend is always compiled (no cargo
/// feature). NEVER panics and NEVER blocks. A NER miss leaks no more than the no-op (the redactor only
/// ever REMOVES content), so the worst case == the no-op behaviour.
pub fn active_name_redactor() -> Arc<dyn NameRedactor> {
    if ner_model_present() {
        match ner_model_dir().and_then(crate::summarize::ner_deberta::DebertaNameRedactor::new) {
            Ok(r) => {
                tracing::info!(target: "ner", "local NER name-redactor ready (lazy load)");
                return Arc::new(r);
            }
            Err(e) => {
                tracing::warn!(target: "ner", error = %e, "local NER init failed; using no-op name redactor");
            }
        }
    } else {
        tracing::info!(target: "ner", "no local NER model present; using no-op name redactor");
    }
    Arc::new(NoopNameRedactor)
}

/// Download the three NER model files into [`ner_model_dir`], INBOUND-ONLY, with progress.
///
/// Mirrors [`crate::embed::download_embed_model`]: each file streams to `<file>.part` then renames
/// atomically; `on_progress(file_index, downloaded, total)` fires as bytes arrive (`total` is `None`
/// when the server omits `Content-Length`). A file already present on disk is SKIPPED. INBOUND ONLY:
/// fetches model files and sends NO request body / NO meeting content (no egress). NO PII logged —
/// filenames + byte counts only.
pub async fn download_ner_model<F>(mut on_progress: F) -> Result<PathBuf>
where
    F: FnMut(usize, u64, Option<u64>),
{
    use crate::error::AppError;
    use tokio::io::AsyncWriteExt;

    let dir = ner_model_dir()?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| AppError::Storage(format!("create ner model dir: {e}")))?;

    for (idx, file) in NER_MODEL_FILES.iter().enumerate() {
        let dest = dir.join(file);
        if dest.is_file() {
            continue;
        }
        let url = format!("{NER_MODEL_HF_BASE}/{file}");
        tracing::info!(target: "ner", file = %file, "downloading ner model file");

        let mut resp = reqwest::get(&url)
            .await
            .map_err(|e| AppError::Storage(format!("ner model download request failed: {e}")))?;
        if !resp.status().is_success() {
            return Err(AppError::Storage(format!(
                "ner model download HTTP {} for {file}",
                resp.status()
            )));
        }
        let total = resp.content_length();

        let part = dest.with_extension("part");
        let mut out = tokio::fs::File::create(&part)
            .await
            .map_err(|e| AppError::Storage(format!("create ner model temp file: {e}")))?;
        let mut downloaded: u64 = 0;
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| AppError::Storage(format!("ner model download body failed: {e}")))?
        {
            out.write_all(&chunk)
                .await
                .map_err(|e| AppError::Storage(format!("write ner model chunk: {e}")))?;
            downloaded += chunk.len() as u64;
            on_progress(idx, downloaded, total);
        }
        out.flush()
            .await
            .map_err(|e| AppError::Storage(format!("flush ner model file: {e}")))?;
        drop(out);

        if downloaded == 0 {
            let _ = tokio::fs::remove_file(&part).await;
            return Err(AppError::Storage(format!(
                "ner model download returned empty body for {file}"
            )));
        }
        tokio::fs::rename(&part, &dest)
            .await
            .map_err(|e| AppError::Storage(format!("rename ner model file: {e}")))?;
        tracing::info!(target: "ner", file = %file, bytes = downloaded, "ner model file ready");
    }

    Ok(dir)
}

fn email_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}").unwrap())
}
fn card_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\b(?:\d[ -]?){13,19}\b").unwrap())
}
fn phone_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\+?\d[\d \-().]{7,}\d").unwrap())
}

/// Replace PII with stable placeholder tokens. Returns (redacted_text, token→original map).
pub fn redact(text: &str) -> (String, HashMap<String, String>) {
    let mut map = HashMap::new();
    let mut rev = HashMap::new();
    let out = redact_into(text, &mut map, &mut rev);
    (out, map)
}

/// Restore original values from placeholder tokens.
pub fn restore(text: &str, map: &HashMap<String, String>) -> String {
    let mut s = text.to_string();
    for (tok, orig) in map {
        s = s.replace(tok, orig);
    }
    s
}

/// Scrub a CONNECTOR (external-tool) query through the SAME two-layer firewall the cloud provider
/// path applies, and return the scrubbed query + the content-free redaction counts.
///
/// This is the FRAMEWORK-level scrub used by [`crate::connectors::ConnectorRegistry::search`] so an
/// outgoing web-search query is masked exactly as a cloud-bound prompt would be:
/// - regex layer (email/card/phone) via [`redact_into`], then
/// - the on-device NER name layer (`names`; the no-op when no model is installed → byte-identical),
///
/// mirroring the ordering in [`RedactingProvider::summarize_with_meta`] (regex map first, then
/// `redact_names`). Unlike the provider path, a connector query is NEVER de-tokenized (web results
/// are attributed to the source as-is), so BOTH restore maps are discarded here — the caller gets
/// only the scrubbed string plus the [`RedactionCounts`] for the content-free egress ledger.
pub(crate) fn redact_connector_query(
    query: &str,
    names: &dyn NameRedactor,
) -> (String, RedactionCounts) {
    let mut map = HashMap::new();
    let mut rev = HashMap::new();
    let regex_scrubbed = redact_into(query, &mut map, &mut rev);
    // NER name layer (no-op → unchanged when no model is present). The token→name pairs are
    // discarded: connector queries are never restored.
    let (scrubbed, name_pairs) = names.redact_names(&regex_scrubbed);
    let counts = count_redactions(&map, name_pairs.len());
    (scrubbed, counts)
}

/// Seam for on-device personal-NAME redaction (Phase 3a).
///
/// The regex scrubbers above catch emails / cards / phones with high confidence, but personal
/// NAMES need on-device NER (GLiNER, Phase 3b) — see the honest-scope note at the top of this
/// file. This trait IS that seam: an implementation scrubs names → stable placeholder tokens and
/// returns the token↔name pairs so the provider's reply can be de-tokenized, exactly mirroring the
/// email/card/phone round-trip (`redact` / `restore`).
///
/// The DEFAULT impl wired into [`RedactingProvider::new`] is [`NoopNameRedactor`] — names pass
/// through UNCHANGED, so production egress is byte-identical to before this seam existed (no risk
/// of a heuristic garbling prompts). A real `GlinerNameRedactor` is a drop-in replacement at the
/// `RedactingProvider::with_name_redactor` call site (Phase 3b).
///
/// Contract: an implementation SHOULD assign a STABLE token per distinct name, so the same name
/// redacted across the `system` and `user` prompts of one `complete` call restores consistently.
pub trait NameRedactor: Send + Sync {
    /// Scrub personal names out of `text`. Returns `(scrubbed_text, token→name pairs)`, where each
    /// token is a placeholder to be restored verbatim in the provider's reply.
    fn redact_names(&self, text: &str) -> (String, Vec<(String, String)>);

    /// `true` ONLY for the dependency-free no-op fallback ([`NoopNameRedactor`]) — i.e. NO real NER
    /// model is active and names pass through UNCHANGED. The vault-title filter in
    /// [`RedactingProvider::summarize_with_meta`] keys its conservative SYNTACTIC person-name
    /// fallback ([`title_looks_like_person_name`]) on this, so person-page titles are still dropped
    /// on installs without the NER model (Brain v2 P0.1). Default `false`: every REAL redactor
    /// (DeBERTa NER, test fixtures) keeps the unchanged NER-detector path.
    fn is_noop(&self) -> bool {
        false
    }
}

/// Default name redactor: a NO-OP. Returns the text unchanged and an empty map, so names egress
/// exactly as they do today. This keeps the firewall's name behaviour identical until a real NER
/// model (Phase 3b) is swapped in — zero regression, zero chance of a bad heuristic mangling a
/// prompt.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopNameRedactor;

impl NameRedactor for NoopNameRedactor {
    fn redact_names(&self, text: &str) -> (String, Vec<(String, String)>) {
        (text.to_string(), Vec::new())
    }

    /// The one redactor that detects NOTHING — the vault-title filter compensates with the
    /// syntactic person-name fallback (see [`NameRedactor::is_noop`]).
    fn is_noop(&self) -> bool {
        true
    }
}

// ── P0.1 (Brain v2) — the NO-NER person-name title fallback ─────────────────────────────────────

/// Words (case-insensitive) that mark a vault title as a MEETING-SHAPED title, not a person page.
/// Any title containing one of these is NEVER treated as a person name by
/// [`title_looks_like_person_name`] — the false-positive guard for the syntactic fallback.
/// EN + PL, matching the languages Murmur serves. Extend from observed dev false-positives
/// (conservative-ship decision, spec §P0.1).
const COMMON_TITLE_WORDS: &[&str] = &[
    // EN
    "meeting",
    "notes",
    "call",
    "sync",
    "review",
    "planning",
    "standup",
    "retrospective",
    "sprint",
    "kickoff",
    "workshop",
    "demo",
    "interview",
    "briefing",
    "agenda",
    "project",
    "roadmap",
    "update",
    "weekly",
    "daily",
    "monthly",
    // PL
    "spotkanie",
    "notatki",
    "raport",
    "przegląd",
    "planowanie",
    "synchronizacja",
    "projekt",
    "tygodniowy",
    "aktualizacja",
];

/// Conservative SYNTACTIC detector for a vault title that is (very likely) a bare PERSON NAME —
/// the shape of an auto-created `[[Anna Kowalska]].md` person page. Active ONLY as the vault-title
/// egress fallback when the real NER model is absent ([`NameRedactor::is_noop`]); with the model
/// present the NER detector governs, unchanged.
///
/// A title "looks like a person name" iff ALL hold:
/// - 2–4 whitespace-separated words (a single word or a long phrase is not a name shape);
/// - every word is name-like: starts uppercase, contains NO digits, uses only alphabetic chars
///   plus internal hyphens/apostrophes ("Anne-Marie", "O'Brien"), and is NOT an all-uppercase
///   acronym ("OKR", "CI");
/// - NO word (case-insensitive) is on the [`COMMON_TITLE_WORDS`] blocklist ("Meeting Notes",
///   "Atlas Project", "Spotkanie Zarządu" are titles, not people).
///
/// Deliberately conservative: a false POSITIVE only drops a `[[wikilink]]` target from the prompt
/// (never content); a false NEGATIVE leaks a name — so the blocklist errs toward dropping.
/// Unicode-aware (`char::is_uppercase` / `is_alphabetic`), so Polish diacritics work.
pub(crate) fn title_looks_like_person_name(title: &str) -> bool {
    let words: Vec<&str> = title.split_whitespace().collect();
    if words.len() < 2 || words.len() > 4 {
        return false;
    }
    for word in &words {
        if COMMON_TITLE_WORDS.contains(&word.to_lowercase().as_str()) {
            return false; // a known title word ⇒ this is a meeting-shaped title, not a person.
        }
        if !word_is_name_like(word) {
            return false;
        }
    }
    true
}

/// One word of a person-name shape: uppercase-first, digit-free, alphabetic (with internal
/// hyphen/apostrophe), and not an all-caps acronym. See [`title_looks_like_person_name`].
fn word_is_name_like(word: &str) -> bool {
    let Some(first) = word.chars().next() else {
        return false;
    };
    if !first.is_uppercase() {
        return false;
    }
    if !word
        .chars()
        .all(|c| c.is_alphabetic() || c == '-' || c == '\'' || c == '\u{2019}')
    {
        return false; // digits or punctuation ("Q3", "Offer:", "R&D") ⇒ not a name word.
    }
    // Reject an ALL-CAPS acronym ("OKR", "CI") — every alphabetic char uppercase, len ≥ 2.
    let alpha: Vec<char> = word.chars().filter(|c| c.is_alphabetic()).collect();
    if alpha.len() >= 2 && alpha.iter().all(|c| c.is_uppercase()) {
        return false;
    }
    true
}

/// The fixed, non-restoring placeholder the COMPLETE-path person-name-title scrub substitutes.
/// Deliberately NOT a `⟪…⟫` token: nothing enters the restore map (drop-only, the same hard
/// guarantee as the summarize-path vault-title FILTER — the name can never come back on egress).
const PERSON_TITLE_PLACEHOLDER: &str = "(person)";

/// R6 (P0.1 follow-up, 2026-07-10) — the NO-NER person-name-title scrub for the COMPLETE path.
///
/// The L3 lock-security review disclosed the gap: on installs WITHOUT the NER model, note/meeting
/// TITLES that are bare person names egress through [`RedactingProvider::complete_with_meta`] /
/// `complete_json_with_meta` untouched — the JIT meeting listing (`- <id> | <title> | <date>`
/// lines seeded into the Ask persona's SYSTEM prompt) and `[[Title]]` wikilinks in packed
/// corpora/citations both carry auto-created `[[Person Name]]` page titles. The summarize path's
/// syntactic fallback ([`title_looks_like_person_name`]) only filtered `vault_titles`.
///
/// This applies the SAME conservative predicate to the two STRUCTURAL title shapes free-form
/// prompts carry (free text is deliberately NOT touched — a general Title-Case scan would garble
/// prompts):
/// - `[[Target]]` wikilink targets — a person-shaped target becomes `[[(person)]]`;
/// - 3-field pipe listing lines (`… | <title> | …`, the JIT listing shape) — a person-shaped
///   middle field becomes `(person)` (id + date survive, so `get_meeting`-by-id still works).
///
/// DROP-ONLY like the summarize filter: the placeholder is a fixed literal, no restore token, so
/// the name cannot ride back out in any later prompt either. Text with no matching shape is
/// returned byte-identical. Callers gate on [`NameRedactor::is_noop`] — with a real NER layer the
/// mask+restore name firewall owns names and this scrub stays OFF.
pub(crate) fn scrub_person_name_titles(text: &str) -> String {
    scrub_wikilink_person_targets(&text_listing_scrub(text))
}

/// The pipe-listing half of [`scrub_person_name_titles`]: line-preserving (works on
/// `split_inclusive('\n')` segments, so byte-identity holds for untouched input).
fn text_listing_scrub(text: &str) -> String {
    if !text.contains(" | ") {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    for seg in text.split_inclusive('\n') {
        let (line, nl) = match seg.strip_suffix('\n') {
            Some(l) => (l, "\n"),
            None => (seg, ""),
        };
        let parts: Vec<&str> = line.split(" | ").collect();
        if parts.len() == 3 && title_looks_like_person_name(parts[1].trim()) {
            out.push_str(parts[0]);
            out.push_str(" | ");
            out.push_str(PERSON_TITLE_PLACEHOLDER);
            out.push_str(" | ");
            out.push_str(parts[2]);
        } else {
            out.push_str(line);
        }
        out.push_str(nl);
    }
    out
}

/// The wikilink half of [`scrub_person_name_titles`]: rewrites only `[[…]]` targets that trip the
/// person-name predicate; everything else (including a dangling `[[`) passes through byte-exact.
fn scrub_wikilink_person_targets(text: &str) -> String {
    if !text.contains("[[") {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("[[") {
        out.push_str(&rest[..start + 2]);
        rest = &rest[start + 2..];
        let Some(end) = rest.find("]]") else {
            break; // dangling open — the remainder is pushed verbatim below.
        };
        let target = &rest[..end];
        if title_looks_like_person_name(target.trim()) {
            out.push_str(PERSON_TITLE_PLACEHOLDER);
        } else {
            out.push_str(target);
        }
        out.push_str("]]");
        rest = &rest[end + 2..];
    }
    out.push_str(rest);
    out
}

/// Restore name placeholders produced by a [`NameRedactor`] back to the original names. Disjoint
/// from [`restore`] (the regex token namespace) so the two layers compose without collision.
fn restore_names(text: &str, pairs: &[(String, String)]) -> String {
    let mut s = text.to_string();
    for (tok, name) in pairs {
        s = s.replace(tok, name);
    }
    s
}

fn redact_into(
    text: &str,
    map: &mut HashMap<String, String>,
    rev: &mut HashMap<String, String>,
) -> String {
    // Order matters: cards before phones so long digit runs become CARD, not PHONE.
    let mut out = apply(text, email_re(), "EMAIL", map, rev);
    out = apply(&out, card_re(), "CARD", map, rev);
    out = apply(&out, phone_re(), "PHONE", map, rev);
    out
}

fn apply(
    text: &str,
    re: &Regex,
    kind: &str,
    map: &mut HashMap<String, String>,
    rev: &mut HashMap<String, String>,
) -> String {
    re.replace_all(text, |caps: &regex::Captures| {
        let m = caps.get(0).unwrap().as_str().to_string();
        if let Some(tok) = rev.get(&m) {
            return tok.clone();
        }
        let tok = format!("\u{27ea}{kind}_{}\u{27eb}", map.len() + 1); // ⟪EMAIL_1⟫
        map.insert(tok.clone(), m.clone());
        rev.insert(m, tok.clone());
        tok
    })
    .into_owned()
}

/// Count redaction placeholders by kind from the regex map and the name-pair count.
///
/// The regex map keys are of the form `⟪EMAIL_n⟫`, `⟪CARD_n⟫`, `⟪PHONE_n⟫` (one entry per
/// UNIQUE matched value — duplicates reuse the same token). Iterate once and bucket by prefix.
/// `name_count` is the number of name pairs returned by the `NameRedactor` (one per unique name).
fn count_redactions(map: &HashMap<String, String>, name_count: usize) -> RedactionCounts {
    let mut counts = RedactionCounts {
        name: name_count as u32,
        ..Default::default()
    };
    for key in map.keys() {
        // Keys are `⟪KIND_n⟫`; strip the leading `⟪` (U+27EA) and match on the suffix.
        let inner = key.trim_start_matches('\u{27ea}');
        if inner.starts_with("EMAIL") {
            counts.email += 1;
        } else if inner.starts_with("CARD") {
            counts.card += 1;
        } else if inner.starts_with("PHONE") {
            counts.phone += 1;
        }
    }
    counts
}

/// Provider decorator: redacts PII from inputs, restores it in outputs, and records a
/// content-free egress audit entry per call.
///
/// Two layers, both restored in the reply: the always-on regex scrubbers (emails/cards/phones,
/// via [`redact`]/[`restore`]) and the [`NameRedactor`] seam. The name layer defaults to
/// [`NoopNameRedactor`] (`new`), so production egress is byte-identical until a real NER model is
/// installed via [`with_name_redactor`](RedactingProvider::with_name_redactor).
///
/// The egress audit sink defaults to [`NoopEgressSink`] in `new`/`with_name_redactor`, preserving
/// byte-identical behaviour for all existing callers and tests. `with_name_redactor_and_sink` is
/// the full constructor used by `make_provider` to wire the live `DbEgressSink`.
pub struct RedactingProvider {
    inner: Arc<dyn SummarizerProvider>,
    names: Arc<dyn NameRedactor>,
    /// Stable provider id forwarded into every `EgressEntry`, e.g. `"anthropic"`.
    provider_id: String,
    /// Non-PII destination label, e.g. `"api.anthropic.com"` or `"claude_code (Anthropic CLI)"`.
    destination: String,
    /// Model id that was requested from config (may differ from `model_served` in the response).
    model_requested: String,
    /// Sink that receives one content-free audit row per call. `NoopEgressSink` by default.
    sink: Arc<dyn EgressSink>,
    /// Production-only NER admission seam. Test/fixture constructors leave this `None`; the provider
    /// factory supplies the shared heavy lane plus an optional exact recording token.
    ner_heavy: Option<Arc<tokio::sync::Semaphore>>,
    recording_token: Option<crate::perf::RecordingSessionToken>,
}

impl RedactingProvider {
    /// Wrap `inner` with the regex firewall and the DEFAULT (no-op) name redactor and NO-OP sink.
    /// Name egress and audit logging are unchanged — this is the back-compat constructor; all
    /// existing callers and tests are unaffected.
    pub fn new(inner: Arc<dyn SummarizerProvider>) -> Self {
        Self {
            provider_id: inner.id().to_string(),
            destination: String::new(),
            model_requested: String::new(),
            inner,
            names: Arc::new(NoopNameRedactor),
            sink: Arc::new(NoopEgressSink),
            ner_heavy: None,
            recording_token: None,
        }
    }

    /// Wrap `inner` with the regex firewall and an EXPLICIT name redactor (Phase 3b drop-in /
    /// tests). The name layer scrubs before egress and restores in the reply, alongside the regex
    /// layer. Sink defaults to no-op — back-compat for all existing call sites.
    pub fn with_name_redactor(
        inner: Arc<dyn SummarizerProvider>,
        names: Arc<dyn NameRedactor>,
    ) -> Self {
        Self {
            provider_id: inner.id().to_string(),
            destination: String::new(),
            model_requested: String::new(),
            inner,
            names,
            sink: Arc::new(NoopEgressSink),
            ner_heavy: None,
            recording_token: None,
        }
    }

    /// Full constructor used by `make_provider`: regex firewall + name redactor + egress sink.
    ///
    /// - `sink` receives one content-free [`EgressEntry`] per call (counts + meta, NO content).
    /// - `provider_id` / `destination` / `model_requested` are forwarded into every entry.
    ///
    /// Existing tests and callers that use `new`/`with_name_redactor` are byte-identical —
    /// only `make_provider` wires the live `DbEgressSink` through this path.
    pub fn with_name_redactor_and_sink(
        inner: Arc<dyn SummarizerProvider>,
        names: Arc<dyn NameRedactor>,
        sink: Arc<dyn EgressSink>,
        provider_id: String,
        destination: String,
        model_requested: String,
    ) -> Self {
        Self {
            inner,
            names,
            sink,
            provider_id,
            destination,
            model_requested,
            ner_heavy: None,
            recording_token: None,
        }
    }

    /// Production constructor with local NER lifecycle admission. The lease + heavy permit cover
    /// only regex/NER preparation and are dropped BEFORE the cloud await, so Record never waits on
    /// network I/O. If Start wins the race, unscoped admission fails and no plaintext egress occurs.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn with_name_redactor_sink_and_model_admission(
        inner: Arc<dyn SummarizerProvider>,
        names: Arc<dyn NameRedactor>,
        sink: Arc<dyn EgressSink>,
        provider_id: String,
        destination: String,
        model_requested: String,
        heavy: Arc<tokio::sync::Semaphore>,
        recording_token: Option<crate::perf::RecordingSessionToken>,
    ) -> Self {
        Self {
            inner,
            names,
            sink,
            provider_id,
            destination,
            model_requested,
            ner_heavy: Some(heavy),
            recording_token,
        }
    }

    async fn run_name_redactor<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&dyn NameRedactor) -> Result<T> + Send + 'static,
    {
        if self.names.is_noop() {
            return f(self.names.as_ref()); // no model, or an explicit fixture constructor.
        }
        let Some(heavy) = self.ner_heavy.as_ref() else {
            return f(self.names.as_ref()); // explicit fixture/back-compat constructor.
        };
        let names = Arc::clone(&self.names);
        crate::perf::run_heavy_with_admission(
            heavy,
            self.recording_token.clone(),
            crate::perf::ResidentModelKind::Ner,
            move || f(names.as_ref()),
        )
        .await
    }
}

#[async_trait]
impl SummarizerProvider for RedactingProvider {
    fn id(&self) -> &str {
        self.inner.id()
    }

    async fn availability(&self) -> Availability {
        self.inner.availability().await
    }

    async fn summarize(&self, req: &SummarizeRequest) -> Result<String> {
        let (text, _meta) = self.summarize_with_meta(req).await?;
        Ok(text)
    }

    async fn complete(&self, system: &str, user: &str) -> Result<String> {
        let (text, _meta) = self.complete_with_meta(system, user).await?;
        Ok(text)
    }

    /// Redact inputs, call inner provider (capturing `CallMeta`), restore outputs, record egress.
    ///
    /// FIREWALL CONTRACT — every `SummarizeRequest` field that EGRESSES to the inner (cloud)
    /// provider is scrubbed here before the `req.clone()` is forwarded. The classification:
    /// - `transcript`, `related_context`, `user_notes`, `live_bullets` — full firewall: regex
    ///   (email/card/phone) via the shared map + the NER name layer, tokens restored in the reply.
    /// - `template` (rides the SYSTEM prompt) and `meta.title_hint` (rides `render_user_content`) —
    ///   regex layer via the shared map, tokens restored in the reply (defense-in-depth; low-risk
    ///   instruction/label strings).
    /// - `vault_titles` (the `[[wikilink]]` target list embedded in `render_user_content`, incl.
    ///   auto-created `[[Person Name]].md` pages) — FILTERED: any title the firewall would alter
    ///   is DROPPED before egress (design B — see the inline rationale below). With NO NER model
    ///   installed the conservative syntactic fallback [`title_looks_like_person_name`] drops
    ///   bare person-name titles too (Brain v2 P0.1).
    /// - `meta.date_iso`, `meta.language`, `meta.duration_s` — deliberately UN-scrubbed non-PII
    ///   format flags (an ISO date / language code / integer; scrubbing a date would false-positive
    ///   as a PHONE and garble the note). Any NEW string field MUST be classified here and is
    ///   caught by `every_string_field_of_summarize_request_is_scrubbed_or_exempt`.
    async fn summarize_with_meta(&self, req: &SummarizeRequest) -> Result<(String, CallMeta)> {
        // Shared regex map across the transcript AND the Phase-4 `related_context` so a value
        // redacted in either restores consistently in the reply. With `related_context = None`
        // (the default + flag-OFF case) the map is built from the transcript alone, exactly as the
        // old `redact(&req.transcript)` did — egress stays byte-identical.
        let mut map = HashMap::new();
        let mut rev = HashMap::new();
        let red_transcript = redact_into(&req.transcript, &mut map, &mut rev);
        // The related-context corpus EGRESSES to the provider in the prompt — scrub it through the
        // SAME firewall as the transcript so emails/cards/phones never leave un-redacted.
        let red_related = req
            .related_context
            .as_ref()
            .map(|c| redact_into(c, &mut map, &mut rev));
        // ENHANCE-MY-NOTES: the typed notes ride the prompt in enhance mode — scrub them
        // through the SAME shared map as the transcript so a value redacted anywhere
        // restores consistently in the reply.
        let red_notes = req
            .user_notes
            .as_ref()
            .map(|c| redact_into(c, &mut map, &mut rev));
        // Brain v2 L4: the running live bullets ride the prompt as the "Live notes (auto)"
        // section — scrub them through the SAME shared map as the transcript they were derived
        // from, so a value redacted anywhere restores consistently in the reply.
        let red_bullets = req
            .live_bullets
            .as_ref()
            .map(|c| redact_into(c, &mut map, &mut rev));
        // DEFENSE-IN-DEPTH — the note-format `template` rides the prompt as the SYSTEM message
        // (anthropic/gateway/claude_code providers) and `meta.title_hint` rides
        // `render_user_content`; both previously egressed VERBATIM inside `req.clone()`. Scrub them
        // through the SAME shared regex map so any email/card/phone is tokenized before egress and
        // restored in the reply. (Regex layer only, per the firewall's honest-scope note — these are
        // low-risk instruction/label strings, not meeting-body text.) The production case (a clean
        // built-in template, `title_hint = None`) is byte-identical: `redact_into` leaves PII-free
        // text untouched and adds no map entry.
        let red_template = redact_into(&req.template, &mut map, &mut rev);
        let red_title_hint = req
            .meta
            .title_hint
            .as_ref()
            .map(|h| redact_into(h, &mut map, &mut rev));

        // VAULT-TITLE FIREWALL (design B — FILTER, not mask+restore). `vault_titles` is the list of
        // EXISTING NOTE TITLES the model may [[wikilink]] to; it is embedded verbatim into
        // `render_user_content` and so EGRESSES. It includes auto-created `[[Person Name]].md` pages,
        // whose stems are raw personal names — the SAME side-channel class as the (already-closed)
        // `user_notes` and speaker-tag leaks. We DROP any title the firewall would alter rather than
        // mask+restore it, because:
        //   - a title is only useful as a LINK TARGET, and a masked title (`Offer - ⟪NAME_1⟫`) is not
        //     a target the user wants linked; and
        //   - the name layer assigns `⟪NAME_n⟫` tokens PER `redact_names` call (see
        //     `ner_deberta::apply_person_spans` — numbering restarts each call), so masking each
        //     title in its own call would COLLIDE its tokens with the transcript's, and
        //     `restore_names` could then resolve a wikilink to the WRONG person's page. Filtering
        //     sidesteps that entirely and gives a hard guarantee: no title the firewall flags reaches
        //     the provider.
        // The predicate uses standalone `redact` + the active name layer purely as DETECTORS (their
        // scrubbed output is discarded — no title tokens enter the shared restore map). A clean vault
        // (no PII in any title) is byte-identical: every title survives the filter unchanged.
        //
        // P0.1 (Brain v2) — the NO-NER fallback: when the active name layer is the no-op (no NER
        // model on this install), the NER detector below flags NOTHING, so an auto-created
        // `[[Anna Kowalska]].md` person page would egress verbatim. The conservative SYNTACTIC
        // detector `title_looks_like_person_name` covers that gap — active ONLY under the no-op, so
        // model-present installs keep the unchanged NER behavior.
        let titles = req.vault_titles.clone();
        let (red_transcript, red_related, red_notes, red_bullets, name_pairs, red_titles) = self
            .run_name_redactor(move |names| {
                let (red_transcript, mut name_pairs) = names.redact_names(&red_transcript);
                let red_related = red_related.map(|c| {
                    let (c2, more) = names.redact_names(&c);
                    name_pairs.extend(more);
                    c2
                });
                let red_notes = red_notes.map(|c| {
                    let (c2, more) = names.redact_names(&c);
                    name_pairs.extend(more);
                    c2
                });
                let red_bullets = red_bullets.map(|c| {
                    let (c2, more) = names.redact_names(&c);
                    name_pairs.extend(more);
                    c2
                });
                let noop_ner = names.is_noop();
                let red_titles = titles
                    .into_iter()
                    .filter(|title| {
                        let (regex_scrubbed, _) = redact(title);
                        if regex_scrubbed.as_str() != title {
                            return false;
                        }
                        if noop_ner && title_looks_like_person_name(title) {
                            return false;
                        }
                        let (name_scrubbed, name_hits) = names.redact_names(title);
                        name_scrubbed.as_str() == title && name_hits.is_empty()
                    })
                    .collect();
                Ok((
                    red_transcript,
                    red_related,
                    red_notes,
                    red_bullets,
                    name_pairs,
                    red_titles,
                ))
            })
            .await?;

        // Byte sizes of the REDACTED content (sizes, never the text itself).
        let user_bytes = red_transcript.len()
            + red_related.as_ref().map(|c| c.len()).unwrap_or(0)
            + red_notes.as_ref().map(|c| c.len()).unwrap_or(0)
            + red_bullets.as_ref().map(|c| c.len()).unwrap_or(0);
        let mut r = req.clone();
        r.transcript = red_transcript;
        r.related_context = red_related;
        r.user_notes = red_notes;
        r.live_bullets = red_bullets;
        r.template = red_template;
        r.meta.title_hint = red_title_hint;
        r.vault_titles = red_titles;
        if self.recording_token.is_none()
            && self.ner_heavy.is_some()
            && !self.names.is_noop()
            && crate::perf::recording_has_priority()
        {
            return Err(AppError::Unavailable(
                "cloud dispatch deferred because recording started during local redaction".into(),
            ));
        }
        // NER/model work is complete. Never hold a local-model lease across the cloud await. The
        // affine external-egress lease, however, MUST cover the entire awaited provider dispatch:
        // either this call wins before Start/Draining and that transition waits, or the transition
        // wins and no network call starts.
        let _egress_lease =
            crate::perf::acquire_external_egress_lease(self.recording_token.as_ref())?;
        let (out, meta) = self.inner.summarize_with_meta(&r).await?;
        // Restore both layers in the reply (disjoint token namespaces; order-independent).
        let out = restore_names(&out, &name_pairs);
        let out = restore(&out, &map);
        // Count PII placeholders by prefix in the redaction map + name pairs length.
        let redactions = count_redactions(&map, name_pairs.len());
        self.sink.record(EgressEntry {
            provider_id: self.provider_id.clone(),
            destination: self.destination.clone(),
            model_requested: self.model_requested.clone(),
            call_kind: "summarize",
            meta: meta.clone(),
            redactions: redactions.clone(),
            system_bytes: 0, // summarize has no separate system prompt
            user_bytes,
            meeting_id: None,
        });
        // Tier 4c — surface the SAME scrub count to the CALLER so the per-note privacy receipt can
        // report how many PII items the firewall removed before this cloud call. A LOCAL provider
        // returns unwrapped upstream and never reaches this wrapper, so a local note's
        // `CallMeta.redactions` stays `None` (no firewall ran) — correctly, no PII key is stamped.
        let mut meta = meta;
        meta.redactions = Some(redactions);
        Ok((out, meta))
    }

    /// Redact inputs, call inner provider (capturing `CallMeta`), restore outputs, record egress.
    async fn complete_with_meta(&self, system: &str, user: &str) -> Result<(String, CallMeta)> {
        // Shared map so a value redacted in either prompt restores consistently.
        let mut map = HashMap::new();
        let mut rev = HashMap::new();
        let rsys = redact_into(system, &mut map, &mut rev);
        let ruser = redact_into(user, &mut map, &mut rev);
        // Byte sizes of the REDACTED content (sizes, never the text itself).
        let system_bytes = rsys.len();
        let user_bytes = ruser.len();
        // Name layer on each prompt (default no-op → unchanged). A stable-token NameRedactor maps
        // the same name to the same token across both prompts, so the merged pairs restore cleanly.
        let noop_ner = self.names.is_noop();
        let (rsys, ruser, name_pairs) = self
            .run_name_redactor(move |names| {
                let (rsys, mut name_pairs) = names.redact_names(&rsys);
                let (ruser, more) = names.redact_names(&ruser);
                name_pairs.extend(more);
                Ok((rsys, ruser, name_pairs))
            })
            .await?;
        // R6 (P0.1 follow-up) — with NO NER model the name layer detects nothing, so person-name
        // TITLES (the JIT listing lines / [[wikilinks]] in the prompt) would egress verbatim on
        // this COMPLETE path. Apply the drop-only syntactic title scrub — active ONLY under the
        // no-op layer, exactly like the summarize path's vault-title fallback.
        let (rsys, ruser) = if noop_ner {
            (
                scrub_person_name_titles(&rsys),
                scrub_person_name_titles(&ruser),
            )
        } else {
            (rsys, ruser)
        };
        if self.recording_token.is_none()
            && self.ner_heavy.is_some()
            && !noop_ner
            && crate::perf::recording_has_priority()
        {
            return Err(AppError::Unavailable(
                "cloud dispatch deferred because recording started during local redaction".into(),
            ));
        }
        // NER/model work is complete. Never hold a local-model lease across the cloud await; hold
        // only the affine egress lease across the actual provider future.
        let _egress_lease =
            crate::perf::acquire_external_egress_lease(self.recording_token.as_ref())?;
        let (out, meta) = self.inner.complete_with_meta(&rsys, &ruser).await?;
        let out = restore_names(&out, &name_pairs);
        let out = restore(&out, &map);
        // Count PII placeholders by prefix in the redaction map + name pairs length.
        let redactions = count_redactions(&map, name_pairs.len());
        self.sink.record(EgressEntry {
            provider_id: self.provider_id.clone(),
            destination: self.destination.clone(),
            model_requested: self.model_requested.clone(),
            call_kind: "complete",
            meta: meta.clone(),
            redactions: redactions.clone(),
            system_bytes,
            user_bytes,
            meeting_id: None,
        });
        // Tier 4c — surface the scrub count to the caller (see `summarize_with_meta`).
        let mut meta = meta;
        meta.redactions = Some(redactions);
        Ok((out, meta))
    }

    /// Redact inputs, call the INNER provider's own `complete_json_with_meta` (not the trait
    /// default's free-text path), restore PII in the returned `Value` via a JSON string
    /// round-trip, record a content-free egress entry with the REAL `CallMeta` (so token counts
    /// for timeline/graph side-tasks are no longer zeroed in the ledger), and return both.
    ///
    /// Callers that only need the value use the inherited `complete_json` default, which
    /// delegates to this method and drops the meta — callers are unchanged.
    async fn complete_json_with_meta(
        &self,
        system: &str,
        user: &str,
        schema: &Value,
    ) -> Result<(Value, CallMeta)> {
        // Shared map so a value redacted in either prompt restores consistently in the reply.
        let mut map = HashMap::new();
        let mut rev = HashMap::new();
        let rsys = redact_into(system, &mut map, &mut rev);
        let ruser = redact_into(user, &mut map, &mut rev);
        // Byte sizes of the REDACTED content (sizes, never the text itself).
        let system_bytes = rsys.len();
        let user_bytes = ruser.len();
        // Name layer on each prompt (default no-op → unchanged). A stable-token NameRedactor maps
        // the same name to the same token across both prompts, so the merged pairs restore cleanly.
        let noop_ner = self.names.is_noop();
        let (rsys, ruser, name_pairs) = self
            .run_name_redactor(move |names| {
                let (rsys, mut name_pairs) = names.redact_names(&rsys);
                let (ruser, more) = names.redact_names(&ruser);
                name_pairs.extend(more);
                Ok((rsys, ruser, name_pairs))
            })
            .await?;
        // R6 — same no-NER-only person-name-title scrub as `complete_with_meta` (the structured
        // side-tasks embed the same listing/wikilink title shapes).
        let (rsys, ruser) = if noop_ner {
            (
                scrub_person_name_titles(&rsys),
                scrub_person_name_titles(&ruser),
            )
        } else {
            (rsys, ruser)
        };
        if self.recording_token.is_none()
            && self.ner_heavy.is_some()
            && !noop_ner
            && crate::perf::recording_has_priority()
        {
            return Err(AppError::Unavailable(
                "cloud dispatch deferred because recording started during local redaction".into(),
            ));
        }
        // Forward to the INNER's own complete_json_with_meta — dispatches to the gateway's native
        // json_schema+meta override, or the trait default for anthropic/claude_code/ollama.
        let _egress_lease =
            crate::perf::acquire_external_egress_lease(self.recording_token.as_ref())?;
        let (value, meta) = self
            .inner
            .complete_json_with_meta(&rsys, &ruser, schema)
            .await?;
        // Restore PII in the returned Value via a JSON string round-trip. The ⟪TOKEN⟫ placeholders
        // are embedded verbatim in the JSON string values, so serialization preserves them and
        // the regex replacements find them correctly.
        let serialized = serde_json::to_string(&value).map_err(|e| {
            AppError::Summarize(format!(
                "complete_json: failed to serialize value for PII restore: {e}"
            ))
        })?;
        // Restore both layers in the same order as complete_with_meta (names first, then regex).
        let restored = restore_names(&serialized, &name_pairs);
        let restored = restore(&restored, &map);
        let out = serde_json::from_str::<Value>(&restored).map_err(|e| {
            AppError::Summarize(format!("complete_json: restore produced invalid JSON: {e}"))
        })?;
        // Record a content-free audit entry with the REAL meta so timeline/graph calls
        // show actual token usage in the egress ledger (not the former CallMeta::default()).
        let redactions = count_redactions(&map, name_pairs.len());
        self.sink.record(EgressEntry {
            provider_id: self.provider_id.clone(),
            destination: self.destination.clone(),
            model_requested: self.model_requested.clone(),
            call_kind: "complete_json",
            meta: meta.clone(),
            redactions: redactions.clone(),
            system_bytes,
            user_bytes,
            meeting_id: None,
        });
        // Tier 4c — surface the scrub count to the caller (see `summarize_with_meta`).
        let mut meta = meta;
        meta.redactions = Some(redactions);
        Ok((out, meta))
    }
    // `complete_json` inherits the delegating default: calls `complete_json_with_meta` and
    // drops the meta — callers that only need the value are unchanged.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_and_restores_round_trip() {
        let text = "Email bob@acme.com and bob@acme.com again, call +1 415 555 1212.";
        let (red, map) = redact(text);
        assert!(!red.contains("bob@acme.com"));
        assert!(red.contains("\u{27ea}EMAIL_1\u{27eb}"));
        // same email reuses the same token
        assert_eq!(red.matches("\u{27ea}EMAIL_1\u{27eb}").count(), 2);
        let restored = restore(&red, &map);
        assert!(restored.contains("bob@acme.com"));
        assert!(!restored.contains("\u{27ea}EMAIL_1\u{27eb}"));
    }

    #[test]
    fn leaves_clean_text_untouched() {
        let (red, map) = redact("We decided to ship on Friday.");
        assert_eq!(red, "We decided to ship on Friday.");
        assert!(map.is_empty());
    }

    // ── name-redaction seam ──────────────────────────────────────────────────

    /// Deterministic, test-only NER stand-in: scrubs a FIXED known set of names → stable tokens.
    /// Stable per name (token derived from the name's index) so the same name maps to the same
    /// token across the `system` and `user` prompts, matching the trait contract.
    struct FixtureNameRedactor;

    impl NameRedactor for FixtureNameRedactor {
        fn redact_names(&self, text: &str) -> (String, Vec<(String, String)>) {
            let mut out = text.to_string();
            let mut pairs = Vec::new();
            for (i, name) in ["Anna Kowalska", "Bob Smith"].iter().enumerate() {
                if out.contains(name) {
                    let tok = format!("\u{27ea}NAME_{}\u{27eb}", i + 1);
                    out = out.replace(name, &tok);
                    pairs.push((tok, (*name).to_string()));
                }
            }
            (out, pairs)
        }
    }

    /// Inner provider that ECHOES the text it received, so a test can assert (a) the scrubbed text
    /// is what actually reached the provider and (b) the reply is de-tokenized on the way back.
    struct EchoProvider;

    #[async_trait]
    impl SummarizerProvider for EchoProvider {
        fn id(&self) -> &str {
            "echo"
        }
        async fn availability(&self) -> Availability {
            Availability::Available
        }
        async fn summarize(&self, req: &SummarizeRequest) -> Result<String> {
            Ok(req.transcript.clone())
        }
        async fn complete(&self, system: &str, user: &str) -> Result<String> {
            Ok(format!("{system}\n---\n{user}"))
        }
    }

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
    }

    struct ModelLifecycleTestGuard {
        _serial: std::sync::MutexGuard<'static, ()>,
    }

    impl Drop for ModelLifecycleTestGuard {
        fn drop(&mut self) {
            crate::perf::reset_model_lifecycle_for_test();
        }
    }

    fn model_lifecycle_test_guard() -> ModelLifecycleTestGuard {
        let serial = crate::perf::model_lifecycle_test_guard();
        crate::perf::reset_model_lifecycle_for_test();
        ModelLifecycleTestGuard { _serial: serial }
    }

    fn sample_req(transcript: &str) -> SummarizeRequest {
        use crate::summarize::provider::MeetingMeta;
        SummarizeRequest {
            transcript: transcript.to_string(),
            meta: MeetingMeta {
                date_iso: "2026-06-28".to_string(),
                title_hint: None,
                duration_s: 60,
                language: None,
            },
            template: String::new(),
            vault_titles: Vec::new(),
            related_context: None,
            user_notes: None,
            live_bullets: None,
        }
    }

    #[test]
    fn noop_name_redactor_is_identity() {
        // The no-regression proof: the default name redactor leaves text byte-identical and yields
        // an empty map, so nothing about name egress changes.
        let (out, pairs) = NoopNameRedactor.redact_names("Anna Kowalska met Bob Smith on Friday.");
        assert_eq!(out, "Anna Kowalska met Bob Smith on Friday.");
        assert!(pairs.is_empty());
    }

    #[test]
    fn default_provider_egress_is_byte_identical_for_names() {
        let _lifecycle = model_lifecycle_test_guard();
        // Through the PRODUCTION constructor (default no-op name layer), a transcript with names
        // and NO regex-PII reaches the inner provider verbatim — proving prod egress is unchanged.
        let captured = std::sync::Arc::new(std::sync::Mutex::new(String::new()));

        struct CaptureProvider(std::sync::Arc<std::sync::Mutex<String>>);
        #[async_trait]
        impl SummarizerProvider for CaptureProvider {
            fn id(&self) -> &str {
                "capture"
            }
            async fn availability(&self) -> Availability {
                Availability::Available
            }
            async fn summarize(&self, req: &SummarizeRequest) -> Result<String> {
                *self.0.lock().unwrap() = req.transcript.clone();
                Ok("done".to_string())
            }
            async fn complete(&self, _system: &str, _user: &str) -> Result<String> {
                Ok(String::new())
            }
        }

        let prov = RedactingProvider::new(Arc::new(CaptureProvider(captured.clone())));
        let transcript = "Anna Kowalska met Bob Smith on Friday to plan Atlas.";
        block_on(prov.summarize(&sample_req(transcript))).unwrap();
        // What egressed to the inner provider is byte-identical to the original (names intact).
        assert_eq!(*captured.lock().unwrap(), transcript);
    }

    #[test]
    fn name_seam_round_trips_through_provider() {
        let _lifecycle = model_lifecycle_test_guard();
        // With a real NameRedactor installed: names are scrubbed BEFORE the provider call and
        // restored in the reply. The EchoProvider returns exactly what it received, so the echoed
        // reply proves both halves of the round-trip.
        let prov = RedactingProvider::with_name_redactor(
            Arc::new(EchoProvider),
            Arc::new(FixtureNameRedactor),
        );
        let transcript = "Anna Kowalska briefed Bob Smith.";
        let out = block_on(prov.summarize(&sample_req(transcript))).unwrap();
        // Reply is de-tokenized back to the real names...
        assert_eq!(out, transcript);
        assert!(!out.contains("NAME_"));
    }

    #[test]
    fn name_seam_scrubs_before_egress() {
        let _lifecycle = model_lifecycle_test_guard();
        // Prove the SCRUB half independently: capture what the inner provider actually receives.
        let captured = std::sync::Arc::new(std::sync::Mutex::new(String::new()));

        struct CaptureProvider(std::sync::Arc<std::sync::Mutex<String>>);
        #[async_trait]
        impl SummarizerProvider for CaptureProvider {
            fn id(&self) -> &str {
                "capture"
            }
            async fn availability(&self) -> Availability {
                Availability::Available
            }
            async fn summarize(&self, req: &SummarizeRequest) -> Result<String> {
                *self.0.lock().unwrap() = req.transcript.clone();
                Ok(req.transcript.clone())
            }
            async fn complete(&self, _system: &str, _user: &str) -> Result<String> {
                Ok(String::new())
            }
        }

        let prov = RedactingProvider::with_name_redactor(
            Arc::new(CaptureProvider(captured.clone())),
            Arc::new(FixtureNameRedactor),
        );
        let out =
            block_on(prov.summarize(&sample_req("Anna Kowalska briefed Bob Smith."))).unwrap();
        let sent = captured.lock().unwrap().clone();
        // The provider saw TOKENS, not names — no name leaked off-device.
        assert!(!sent.contains("Anna Kowalska"));
        assert!(!sent.contains("Bob Smith"));
        assert!(sent.contains("\u{27ea}NAME_1\u{27eb}"));
        // ...but the caller still gets the real names back.
        assert!(out.contains("Anna Kowalska") && out.contains("Bob Smith"));
    }

    /// TIER 0 PII (lock-security): a real NAME that occupies a `(speaker)` tag rides `req.transcript`
    /// (via `pipeline::build_transcript_feed` → `summary_text`), so the SAME NameRedactor firewall
    /// that scrubs the body scrubs the tag before egress — the speaker labels use NO side channel
    /// (the analogue of the old un-scrubbed `user_notes` leak). RED-before-GREEN: the pre-redaction
    /// feed CONTAINS the raw name; what egresses must NOT.
    #[test]
    fn tier0_named_speaker_tag_is_scrubbed_before_egress() {
        let _lifecycle = model_lifecycle_test_guard();
        use crate::transcribe::types::Segment;
        let segs = vec![
            Segment {
                idx: 0,
                start_s: 0.0,
                end_s: 2.0,
                text: "let's begin".into(),
                speaker: Some("Anna Kowalska".into()),
                confidence: None,
            },
            Segment {
                idx: 1,
                start_s: 2.0,
                end_s: 6.0,
                text: "sounds good".into(),
                speaker: Some("me".into()),
                confidence: None,
            },
        ];
        let feed = crate::pipeline::build_transcript_feed(&segs);
        // Two distinct speakers ⇒ labeled ⇒ the raw name occupies a `(speaker)` tag pre-redaction.
        assert!(feed.labeled);
        assert!(
            feed.summary_text.contains("(Anna Kowalska)"),
            "raw name sits in the tag before redaction"
        );

        let captured = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        struct CaptureProvider(std::sync::Arc<std::sync::Mutex<String>>);
        #[async_trait]
        impl SummarizerProvider for CaptureProvider {
            fn id(&self) -> &str {
                "capture"
            }
            async fn availability(&self) -> Availability {
                Availability::Available
            }
            async fn summarize(&self, req: &SummarizeRequest) -> Result<String> {
                *self.0.lock().unwrap() = req.transcript.clone();
                Ok(req.transcript.clone())
            }
            async fn complete(&self, _system: &str, _user: &str) -> Result<String> {
                Ok(String::new())
            }
        }
        let prov = RedactingProvider::with_name_redactor(
            Arc::new(CaptureProvider(captured.clone())),
            Arc::new(FixtureNameRedactor),
        );
        block_on(prov.summarize(&sample_req(&feed.summary_text))).unwrap();
        let sent = captured.lock().unwrap().clone();
        // The name in the speaker tag was scrubbed to a token before egress; the labeled line survives.
        assert!(!sent.contains("Anna Kowalska"), "name must NOT egress");
        assert!(
            sent.contains("\u{27ea}NAME_1\u{27eb}"),
            "name replaced by the stable token"
        );
        assert!(
            sent.contains("[0.0-2.0] ("),
            "the labeled line shape survives redaction"
        );
    }

    // ── Phase D model plumbing + active factory ──────────────────────────────

    #[test]
    fn ner_model_dir_is_under_models_dir() {
        let dir = ner_model_dir().unwrap();
        assert!(dir.ends_with(NER_MODEL_SUBDIR));
        assert_eq!(
            NER_MODEL_FILES,
            &["model.safetensors", "tokenizer.json", "config.json"]
        );
        assert!(NER_MODEL_HF_BASE.contains("mdeberta"));
        // The token prefix the redactor emits matches the ⟪NAME_ tokens the tests assert on.
        assert_eq!(NER_NAME_TOKEN_PREFIX, "\u{27ea}NAME_");
    }

    #[test]
    fn ner_model_present_false_when_any_file_missing() {
        // On a clean machine the NER dir is absent ⇒ not present.
        let dir = ner_model_dir().unwrap();
        if !dir.is_dir() {
            assert!(
                !ner_model_present(),
                "absent NER dir must report not-present"
            );
        }
    }

    #[test]
    fn active_name_redactor_falls_back_to_noop_without_model() {
        // The graceful-degradation contract: with NO NER model present, `active_name_redactor`
        // returns a redactor that leaves text byte-identical (the no-op). The candle NER backend is
        // always compiled now, so selection keys ONLY on model presence — absent model ⇒ no-op.
        if !ner_model_present() {
            let r = active_name_redactor();
            let text = "Anna Kowalska met Bob Smith on Friday.";
            let (out, pairs) = r.redact_names(text);
            assert_eq!(out, text, "no-op fallback must be byte-identical");
            assert!(pairs.is_empty());
        }
    }

    #[test]
    fn default_make_provider_egress_byte_identical_with_active_redactor() {
        let _lifecycle = model_lifecycle_test_guard();
        // End-to-end through the PRODUCTION seam: `RedactingProvider::with_name_redactor(inner,
        // active_name_redactor())` (what `make_provider` now wires). On a clean machine the active
        // redactor is the no-op, so a names-only transcript reaches the inner provider verbatim —
        // proving the wire change is zero-regression when no model is installed.
        if !ner_model_present() {
            let captured = std::sync::Arc::new(std::sync::Mutex::new(String::new()));

            struct CaptureProvider(std::sync::Arc<std::sync::Mutex<String>>);
            #[async_trait]
            impl SummarizerProvider for CaptureProvider {
                fn id(&self) -> &str {
                    "capture"
                }
                async fn availability(&self) -> Availability {
                    Availability::Available
                }
                async fn summarize(&self, req: &SummarizeRequest) -> Result<String> {
                    *self.0.lock().unwrap() = req.transcript.clone();
                    Ok("done".to_string())
                }
                async fn complete(&self, _system: &str, _user: &str) -> Result<String> {
                    Ok(String::new())
                }
            }

            let prov = RedactingProvider::with_name_redactor(
                Arc::new(CaptureProvider(captured.clone())),
                active_name_redactor(),
            );
            let transcript = "Anna Kowalska met Bob Smith on Friday to plan Atlas.";
            block_on(prov.summarize(&sample_req(transcript))).unwrap();
            assert_eq!(*captured.lock().unwrap(), transcript);
        }
    }

    #[test]
    fn name_seam_consistent_across_complete_prompts() {
        let _lifecycle = model_lifecycle_test_guard();
        // The same name in both `system` and `user` restores consistently (stable-token contract).
        let prov = RedactingProvider::with_name_redactor(
            Arc::new(EchoProvider),
            Arc::new(FixtureNameRedactor),
        );
        let out =
            block_on(prov.complete("You assist Anna Kowalska.", "Anna Kowalska asks: status?"))
                .unwrap();
        assert!(!out.contains("NAME_"));
        assert_eq!(out.matches("Anna Kowalska").count(), 2);
    }

    #[test]
    fn production_ner_admission_blocks_background_but_accepts_exact_recording_token() {
        let _lifecycle = model_lifecycle_test_guard();
        let mut owner = crate::perf::begin_recording_session().unwrap();
        owner.transition_to_live().unwrap();
        let heavy = Arc::new(tokio::sync::Semaphore::new(1));

        let background = RedactingProvider::with_name_redactor_sink_and_model_admission(
            Arc::new(EchoProvider),
            Arc::new(FixtureNameRedactor),
            Arc::new(NoopEgressSink),
            "echo".into(),
            "test".into(),
            "m".into(),
            heavy.clone(),
            None,
        );
        assert!(matches!(
            block_on(background.complete("s", "Anna Kowalska")),
            Err(AppError::Unavailable(_))
        ));
        assert!(matches!(
            block_on(background.complete_json_with_meta(
                "s",
                "Anna Kowalska",
                &serde_json::json!({"type": "object"}),
            )),
            Err(AppError::Unavailable(_))
        ));

        let recording = RedactingProvider::with_name_redactor_sink_and_model_admission(
            Arc::new(EchoProvider),
            Arc::new(FixtureNameRedactor),
            Arc::new(NoopEgressSink),
            "echo".into(),
            "test".into(),
            "m".into(),
            heavy,
            Some(owner.token()),
        );
        let out = block_on(recording.complete("s", "Anna Kowalska")).unwrap();
        assert!(out.contains("Anna Kowalska"));

        owner.transition_to_draining().unwrap();
        owner.transition_to_postprocess().unwrap();
        owner.finish().unwrap();
    }

    // ── Phase 2b — egress ledger ─────────────────────────────────────────────

    /// `CaptureEgressSink` captures every `EgressEntry` for assertion in tests.
    struct CaptureEgressSink(
        std::sync::Arc<std::sync::Mutex<Vec<crate::summarize::egress_log::EgressEntry>>>,
    );

    impl crate::summarize::egress_log::EgressSink for CaptureEgressSink {
        fn record(&self, entry: crate::summarize::egress_log::EgressEntry) {
            self.0.lock().unwrap().push(entry);
        }
    }

    /// `EchoMetaProvider` — like `EchoProvider` but returns a fixed `CallMeta` from `*_with_meta`.
    struct EchoMetaProvider;

    #[async_trait]
    impl SummarizerProvider for EchoMetaProvider {
        fn id(&self) -> &str {
            "echo-meta"
        }
        async fn availability(&self) -> Availability {
            Availability::Available
        }
        async fn summarize(&self, req: &SummarizeRequest) -> Result<String> {
            Ok(req.transcript.clone())
        }
        async fn complete(&self, system: &str, user: &str) -> Result<String> {
            Ok(format!("{system}\n---\n{user}"))
        }
        async fn summarize_with_meta(
            &self,
            req: &SummarizeRequest,
        ) -> Result<(String, crate::summarize::meta::CallMeta)> {
            use crate::summarize::meta::CallMeta;
            Ok((
                req.transcript.clone(),
                CallMeta {
                    model_served: Some("claude-opus-4-8-test".to_string()),
                    prompt_tokens: Some(42),
                    completion_tokens: Some(13),
                    total_tokens: Some(55),
                    cached_tokens: None,
                    redactions: None,
                },
            ))
        }
        async fn complete_with_meta(
            &self,
            system: &str,
            user: &str,
        ) -> Result<(String, crate::summarize::meta::CallMeta)> {
            use crate::summarize::meta::CallMeta;
            Ok((
                format!("{system}\n---\n{user}"),
                CallMeta {
                    model_served: Some("claude-opus-4-8-test".to_string()),
                    prompt_tokens: Some(10),
                    completion_tokens: Some(5),
                    total_tokens: Some(15),
                    cached_tokens: None,
                    redactions: None,
                },
            ))
        }
    }

    /// The content-free proof: an EgressEntry records counts + meta but NOT the input content.
    ///
    /// Build a `RedactingProvider::with_name_redactor_and_sink` wrapping `EchoMetaProvider`;
    /// feed input containing one email + one phone. Assert:
    /// - ONE entry was recorded with `redactions.email == 1`, `redactions.phone == 1`.
    /// - `CallMeta` propagated (prompt_tokens == 10 for `complete`).
    /// - `format!("{:?}", entry)` contains NEITHER the email string NOR the note text.
    #[test]
    fn egress_entry_is_content_free_and_captures_meta_and_counts() {
        let _lifecycle = model_lifecycle_test_guard();
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = Arc::new(CaptureEgressSink(captured.clone()));
        let prov = RedactingProvider::with_name_redactor_and_sink(
            Arc::new(EchoMetaProvider),
            Arc::new(NoopNameRedactor),
            sink,
            "anthropic".to_string(),
            "api.anthropic.com".to_string(),
            "claude-opus-4-8".to_string(),
        );
        let note_text = "Meeting about the Atlas project — please contact alice@corp.example and call +1 800 555 0100.";
        block_on(prov.complete("You are a helpful assistant.", note_text)).unwrap();

        let entries = captured.lock().unwrap();
        assert_eq!(entries.len(), 1, "exactly one egress entry per call");
        let entry = &entries[0];

        // Counts correct:
        assert_eq!(entry.redactions.email, 1, "one email was scrubbed");
        assert_eq!(entry.redactions.phone, 1, "one phone was scrubbed");
        assert_eq!(entry.redactions.card, 0);
        assert_eq!(entry.redactions.name, 0); // no-op name redactor

        // CallMeta propagated:
        assert_eq!(entry.meta.prompt_tokens, Some(10));
        assert_eq!(
            entry.meta.model_served.as_deref(),
            Some("claude-opus-4-8-test")
        );

        // call_kind:
        assert_eq!(entry.call_kind, "complete");

        // THE CONTENT-FREE INVARIANT: the Debug output must contain neither the email nor note text.
        let debug = format!("{:?}", entry);
        assert!(
            !debug.contains("alice@corp.example"),
            "email must NOT appear in egress entry debug: {debug}"
        );
        assert!(
            !debug.contains("Atlas project"),
            "note text must NOT appear in egress entry debug: {debug}"
        );
        // Only non-PII metadata present:
        assert!(
            debug.contains("api.anthropic.com"),
            "destination label is non-PII"
        );
    }

    /// Tier 4c (v1.1) — RED-before-GREEN: `summarize_with_meta` must SURFACE the firewall's scrub
    /// count to the CALLER via `CallMeta.redactions`, so the per-note privacy receipt reports a
    /// REAL number equal to what actually left the device redacted. RED on the pre-change code
    /// (the field did not exist / stayed `None`); GREEN once `RedactingProvider` sets
    /// `meta.redactions = Some(count)` before returning.
    #[test]
    fn summarize_with_meta_surfaces_redaction_count_to_caller() {
        let _lifecycle = model_lifecycle_test_guard();
        let prov = RedactingProvider::with_name_redactor(
            Arc::new(EchoMetaProvider),
            Arc::new(NoopNameRedactor),
        );
        // One email in the transcript → the firewall scrubs exactly one EMAIL placeholder.
        let (_out, meta) = block_on(
            prov.summarize_with_meta(&sample_req("Ping alice@corp.example about the roadmap.")),
        )
        .unwrap();
        let counts = meta
            .redactions
            .expect("scrub count surfaced to the caller (Some), not dropped");
        assert_eq!(counts.email, 1, "one email scrubbed");
        assert_eq!(counts.card, 0);
        assert_eq!(counts.phone, 0);
        assert_eq!(counts.name, 0, "no-op name redactor → zero names");
    }

    /// The default `with_name_redactor` path records to a NoopEgressSink — no panic, no row.
    #[test]
    fn default_constructor_records_to_noop_sink_no_panic() {
        let _lifecycle = model_lifecycle_test_guard();
        let prov = RedactingProvider::with_name_redactor(
            Arc::new(EchoProvider),
            Arc::new(NoopNameRedactor),
        );
        // Must not panic; sink is no-op so nothing is written anywhere.
        block_on(prov.complete("sys", "user content alice@example.com")).unwrap();
    }

    /// `count_redactions` correctly buckets email/card/phone from the redaction map keys.
    #[test]
    fn count_redactions_buckets_by_kind() {
        let (_, map) = redact("Email bob@acme.com, card 4111111111111111, phone +1-800-555-0100.");
        let counts = count_redactions(&map, 3 /* simulate 3 name pairs */);
        assert_eq!(counts.email, 1);
        assert_eq!(counts.card, 1);
        assert_eq!(counts.phone, 1);
        assert_eq!(counts.name, 3);
    }

    // ── Task 8 fix — complete_json override ─────────────────────────────────

    /// RED-before-GREEN: `RedactingProvider::complete_json` must (1) call the INNER provider's
    /// own `complete_json` override (not the trait default, which would call `complete()`),
    /// (2) pass REDACTED inputs, (3) RESTORE PII in the returned `Value`, and (4) record a
    /// content-free egress entry with `call_kind == "complete_json"`.
    ///
    /// RED state (before the override): the trait default fires → it calls `self.complete()` →
    /// the inner's `complete_json` override is NEVER reached → `complete_json_called` stays false.
    #[test]
    fn complete_json_redacts_forwards_restores_and_records() {
        let _lifecycle = model_lifecycle_test_guard();
        use std::sync::atomic::{AtomicBool, Ordering};

        let captured_user = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let complete_json_called = std::sync::Arc::new(AtomicBool::new(false));

        /// Inner that (a) flags when its `complete_json_with_meta` is called, (b) captures the
        /// user prompt it received, and (c) echoes the ⟪EMAIL_1⟫ token back inside a JSON Value
        /// paired with a KNOWN non-default `CallMeta` — so we can verify the `RedactingProvider`
        /// restores the PII and forwards the REAL meta to the egress ledger.
        struct RecordingJsonProvider {
            captured_user: std::sync::Arc<std::sync::Mutex<String>>,
            called: std::sync::Arc<AtomicBool>,
        }

        #[async_trait]
        impl SummarizerProvider for RecordingJsonProvider {
            fn id(&self) -> &str {
                "recording-json"
            }
            async fn availability(&self) -> Availability {
                Availability::Available
            }
            async fn summarize(&self, _req: &SummarizeRequest) -> Result<String> {
                Ok(String::new())
            }
            async fn complete(&self, _system: &str, _user: &str) -> Result<String> {
                // Must NOT be called when complete_json_with_meta override is wired correctly.
                Ok(String::new())
            }
            async fn complete_json_with_meta(
                &self,
                _system: &str,
                user: &str,
                _schema: &serde_json::Value,
            ) -> Result<(serde_json::Value, crate::summarize::meta::CallMeta)> {
                self.called.store(true, Ordering::SeqCst);
                *self.captured_user.lock().unwrap() = user.to_string();
                // Return a Value that embeds the ⟪EMAIL_1⟫ placeholder (proves the
                // RedactingProvider restores it to `alice@corp.example`) together with a
                // KNOWN non-default CallMeta (proves the meta reaches the egress ledger).
                Ok((
                    serde_json::json!({ "note": "contact \u{27ea}EMAIL_1\u{27eb}" }),
                    crate::summarize::meta::CallMeta {
                        model_served: Some("test-gateway-model".to_string()),
                        prompt_tokens: Some(42),
                        completion_tokens: Some(7),
                        total_tokens: Some(49),
                        cached_tokens: None,
                        redactions: None,
                    },
                ))
            }
        }

        let inner = Arc::new(RecordingJsonProvider {
            captured_user: captured_user.clone(),
            called: complete_json_called.clone(),
        });
        let captured_entries = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = Arc::new(CaptureEgressSink(captured_entries.clone()));

        let prov = RedactingProvider::with_name_redactor_and_sink(
            inner,
            Arc::new(NoopNameRedactor),
            sink,
            "gateway".to_string(),
            "localhost:4000".to_string(),
            "gpt-4o".to_string(),
        );

        let schema = serde_json::json!({"type": "object"});
        let value = block_on(prov.complete_json(
            "You are a helper.",
            "Please summarize for alice@corp.example",
            &schema,
        ))
        .unwrap();

        // 1. The inner's complete_json was called (not the trait default via complete()).
        assert!(
            complete_json_called.load(Ordering::SeqCst),
            "inner.complete_json must be invoked; RED: before the override the trait default \
             fires self.complete() instead, so this flag stays false"
        );

        // 2. The inner received REDACTED user — email was scrubbed before egress.
        let sent_user = captured_user.lock().unwrap().clone();
        assert!(
            !sent_user.contains("alice@corp.example"),
            "inner must receive REDACTED user (email must not egress), got: {sent_user}"
        );
        assert!(
            sent_user.contains("\u{27ea}EMAIL_"),
            "inner must see the ⟪EMAIL_n⟫ placeholder token, got: {sent_user}"
        );

        // 3. The returned Value has PII restored — the note field has the original email.
        let note = value["note"].as_str().unwrap_or("");
        assert!(
            note.contains("alice@corp.example"),
            "PII must be RESTORED in the returned Value (JSON string round-trip), note: {note}"
        );
        assert!(
            !note.contains("\u{27ea}EMAIL_"),
            "⟪EMAIL_n⟫ token must be absent from the restored Value, note: {note}"
        );

        // 4. Egress sink: one entry, correct call_kind, correct redaction count, REAL meta,
        //    and content-free.
        let entries = captured_entries.lock().unwrap();
        assert_eq!(
            entries.len(),
            1,
            "exactly one egress entry per complete_json call"
        );
        let entry = &entries[0];
        assert_eq!(
            entry.call_kind, "complete_json",
            "call_kind must be 'complete_json'"
        );
        assert_eq!(entry.redactions.email, 1, "one email was redacted");

        // 5. The REAL CallMeta propagated — no longer CallMeta::default().
        assert_eq!(
            entry.meta.prompt_tokens,
            Some(42),
            "real prompt_tokens from inner must be recorded in the egress entry (was CallMeta::default() before FIX 1)"
        );
        assert_eq!(
            entry.meta.model_served.as_deref(),
            Some("test-gateway-model"),
            "real model_served from inner must be recorded in the egress entry"
        );

        let debug = format!("{:?}", entry);
        assert!(
            !debug.contains("alice@corp.example"),
            "email must NOT appear in the egress entry debug output: {debug}"
        );
        assert!(
            !debug.contains("contact"),
            "response content must NOT appear in the egress entry debug output: {debug}"
        );
    }

    // ── ENHANCE-MY-NOTES: user_notes redaction firewall ─────────────────────

    /// Captures the exact SummarizeRequest the wrapped (i.e. EGRESSING) provider receives.
    struct CapturingInner(std::sync::Mutex<Option<SummarizeRequest>>);

    #[async_trait]
    impl SummarizerProvider for CapturingInner {
        fn id(&self) -> &str {
            "capture-full"
        }
        async fn availability(&self) -> Availability {
            Availability::Available
        }
        async fn summarize(&self, req: &SummarizeRequest) -> Result<String> {
            *self.0.lock().unwrap() = Some(req.clone());
            Ok("---\ntitle: T\n---\n# T\n".to_string())
        }
        async fn complete(&self, _system: &str, _user: &str) -> Result<String> {
            Ok(String::new())
        }
    }

    /// ENHANCE-MY-NOTES: user_notes EGRESSES in enhance mode, so it MUST pass the same
    /// redaction firewall as the transcript — emails/phones never leave un-scrubbed.
    ///
    /// RED evidence (before fix): `r.user_notes = red_notes;` was absent; `req.clone()` forwarded
    /// the raw `user_notes` verbatim to the inner provider → email present in egressed request.
    /// GREEN after fix: the same shared map + name-layer pass applied, email replaced with token.
    #[test]
    fn user_notes_are_redacted_before_egress() {
        let _lifecycle = model_lifecycle_test_guard();
        let mut req = sample_req("no PII in transcript");
        req.user_notes = Some("ping bob@corp.com about the deck".to_string());
        let inner = std::sync::Arc::new(CapturingInner(std::sync::Mutex::new(None)));
        let provider = RedactingProvider::new(inner.clone());
        block_on(provider.summarize(&req)).unwrap();
        let egressed = inner.0.lock().unwrap().clone().expect("inner was called");
        let notes = egressed.user_notes.expect("user_notes forwarded to inner");
        assert!(
            !notes.contains("bob@corp.com"),
            "email must not egress un-redacted: {notes}"
        );
        assert!(
            notes.contains("about the deck"),
            "non-PII text must pass through: {notes}"
        );
    }

    // ── VAULT-TITLES: the [[wikilink]] side-channel (design B — FILTER) ───────

    /// Render exactly what a provider sends: the SYSTEM prompt (`req.template`) plus the USER
    /// content (`render_user_content`, which embeds vault titles / title_hint / related / notes /
    /// transcript). This is the faithful egress surface the firewall must keep PII-free.
    fn rendered_egress(req: &SummarizeRequest) -> String {
        format!(
            "{}\n{}",
            req.template,
            crate::summarize::template::render_user_content(req)
        )
    }

    /// VAULT-TITLE LEAK (regex layer). `vault_titles` egresses via `render_user_content`
    /// ("EXISTING NOTE TITLES …"), so a regex-detectable PII value in a title — e.g. an
    /// auto-created contact page whose stem carries an email — must NOT reach the inner provider.
    ///
    /// RED-before-GREEN: on the UNPATCHED code (`req.clone()` forwarded `vault_titles` verbatim)
    /// this same body with the assertion flipped to `contains(...)` PASSED — the email leaked.
    /// After the design-B filter it is ABSENT: the offending title is dropped, a clean title
    /// survives as a valid link target.
    #[test]
    fn vault_title_with_regex_pii_is_filtered_before_egress() {
        let _lifecycle = model_lifecycle_test_guard();
        let mut req = sample_req("no PII in transcript");
        req.vault_titles = vec![
            "Offer - jane@doe.example".to_string(), // email in the title → must be dropped
            "Q3 Roadmap".to_string(),               // clean title → must survive as a link target
        ];
        let inner = std::sync::Arc::new(CapturingInner(std::sync::Mutex::new(None)));
        let provider = RedactingProvider::new(inner.clone());
        block_on(provider.summarize(&req)).unwrap();
        let egressed = inner.0.lock().unwrap().clone().expect("inner was called");
        let egress = rendered_egress(&egressed);
        assert!(
            !egress.contains("jane@doe.example"),
            "email-bearing vault title must NOT egress: {egress}"
        );
        assert!(
            !egressed
                .vault_titles
                .iter()
                .any(|t| t == "Offer - jane@doe.example"),
            "the PII title must be filtered out of the egressed list"
        );
        assert!(
            egress.contains("Q3 Roadmap"),
            "a clean title must still egress as a valid [[wikilink]] target: {egress}"
        );
    }

    /// VAULT-TITLE NAME LEAK — the precise falsification of the Settings copy ("NAMES are
    /// additionally masked before any redacted text leaves this Mac"). With the NER name layer
    /// ACTIVE (the `FixtureNameRedactor` seam the other name-seam tests use), a personal NAME
    /// sitting in a vault title (an auto-created `[[Person Name]].md` page) used to bypass the name
    /// firewall entirely, because `summarize_with_meta` overwrote only transcript/related/notes and
    /// forwarded `vault_titles` verbatim.
    ///
    /// RED-before-GREEN: on the UNPATCHED code this body with the assertion flipped to
    /// `contains("Anna Kowalska")` PASSED even with the redactor active (proving the bypass). After
    /// the design-B filter the flagged title is dropped, so the name is ABSENT; a clean title stays.
    #[test]
    fn vault_title_with_person_name_is_filtered_when_ner_active() {
        let _lifecycle = model_lifecycle_test_guard();
        let mut req = sample_req("no PII in transcript");
        req.vault_titles = vec![
            "Meeting with Anna Kowalska".to_string(), // person page the NER layer detects → dropped
            "Roadmap".to_string(),                    // clean title → survives
        ];
        let inner = std::sync::Arc::new(CapturingInner(std::sync::Mutex::new(None)));
        let provider =
            RedactingProvider::with_name_redactor(inner.clone(), Arc::new(FixtureNameRedactor));
        block_on(provider.summarize(&req)).unwrap();
        let egressed = inner.0.lock().unwrap().clone().expect("inner was called");
        let egress = rendered_egress(&egressed);
        assert!(
            !egress.contains("Anna Kowalska"),
            "a NAME in a vault title must NOT egress when the name layer is active: {egress}"
        );
        assert!(
            egress.contains("Roadmap"),
            "a clean title must still egress as a link target: {egress}"
        );
    }

    /// P0.1 (Brain v2) — the NO-NER person-name fallback. On an install WITHOUT the NER model the
    /// active name layer is the [`NoopNameRedactor`] (`name_hits` always empty), so an auto-created
    /// `[[Anna Kowalska]].md` person page rode `vault_titles` to the cloud VERBATIM — the exact
    /// side-channel the NER title filter closes on model-present installs. The fix is a conservative
    /// SYNTACTIC fallback ([`title_looks_like_person_name`]) active ONLY when the name layer is the
    /// no-op: a Title-Case 2–4-word, digit-free, non-blocklisted title is dropped before egress.
    ///
    /// RED-before-GREEN: on the unpatched code this test FAILED — "Anna Kowalska" reached the inner
    /// provider (the no-op detector flags nothing). Confirmed failing before the fix.
    #[test]
    fn person_name_title_is_dropped_when_no_ner_model_present() {
        let _lifecycle = model_lifecycle_test_guard();
        let mut req = sample_req("no PII in transcript");
        req.vault_titles = vec![
            "Anna Kowalska".to_string(), // person-page stem → MUST be dropped under the no-op NER
            "Meeting Notes".to_string(), // blocklisted common title words → survives
            "Q3 OKRs".to_string(),       // digits/acronym → survives
        ];
        let inner = std::sync::Arc::new(CapturingInner(std::sync::Mutex::new(None)));
        // The PRODUCTION no-model shape: the explicit NoopNameRedactor (what `active_name_redactor`
        // returns when the NER model is absent).
        let provider =
            RedactingProvider::with_name_redactor(inner.clone(), Arc::new(NoopNameRedactor));
        block_on(provider.summarize(&req)).unwrap();
        let egressed = inner.0.lock().unwrap().clone().expect("inner was called");
        assert!(
            !egressed.vault_titles.iter().any(|t| t == "Anna Kowalska"),
            "a person-name title must NOT egress when no NER model is present: {:?}",
            egressed.vault_titles
        );
        assert!(
            egressed.vault_titles.iter().any(|t| t == "Meeting Notes"),
            "a common-words title must survive as a link target: {:?}",
            egressed.vault_titles
        );
        assert!(
            egressed.vault_titles.iter().any(|t| t == "Q3 OKRs"),
            "a digit/acronym title must survive as a link target: {:?}",
            egressed.vault_titles
        );
        // The rendered egress surface carries no trace of the person name either.
        let egress = rendered_egress(&egressed);
        assert!(
            !egress.contains("Anna Kowalska"),
            "the person name must not appear anywhere in the egressed content: {egress}"
        );
    }

    /// R6 (P0.1 follow-up, 2026-07-10) — the COMPLETE-path gap the L3 lock-security review
    /// disclosed: the JIT meeting listing (`- <id> | <title> | <date>` lines riding the SYSTEM
    /// prompt) and `[[Title]]` corpus/citation links egress through
    /// `RedactingProvider::complete_with_meta`, where the vault-title syntactic person-name
    /// fallback did NOT apply — on a no-NER install an auto-created person-page title
    /// ("Anna Kowalska") rode to the cloud verbatim. The fix applies the SAME
    /// [`title_looks_like_person_name`] fallback (drop-only, no restore tokens) to those two
    /// structural title shapes on the complete path, ACTIVE ONLY under the no-op name layer.
    ///
    /// RED-before-GREEN via EchoProvider: on the unpatched code the echoed prompt contained
    /// "Anna Kowalska" (captured failing run); after the fix the name is absent while
    /// meeting-shaped titles survive untouched.
    #[test]
    fn person_name_titles_scrubbed_on_complete_path_when_no_ner() {
        let _lifecycle = model_lifecycle_test_guard();
        let prov = RedactingProvider::with_name_redactor(
            Arc::new(EchoProvider),
            Arc::new(NoopNameRedactor), // the production no-NER-model shape
        );
        let system = "Meetings you can read (id | title | date):\n\
                      - 3f2b1a | Anna Kowalska | 2026-07-01\n\
                      - 9c8d7e | Weekly Sync | 2026-07-02";
        let user = "Ground your answer in [[Anna Kowalska]] and [[Roadmap Review]].";
        let out = block_on(prov.complete(system, user)).unwrap();
        assert!(
            !out.contains("Anna Kowalska"),
            "a person-name title must NOT egress on the complete path with no NER model: {out}"
        );
        assert!(
            out.contains("Weekly Sync") && out.contains("Roadmap Review"),
            "meeting-shaped titles survive as usable targets: {out}"
        );
        assert!(
            out.contains("- 3f2b1a |") && out.contains("| 2026-07-01"),
            "the listing line structure (id + date) survives so get_meeting-by-id still works: {out}"
        );
    }

    /// R6 counter-proof: with a REAL name layer active (`is_noop() == false`) the syntactic
    /// complete-path fallback stays OFF — the NER layer owns names (mask + restore), so the
    /// prompt text is not additionally rewritten by the drop-only scrub.
    #[test]
    fn complete_path_syntactic_scrub_inactive_when_ner_present() {
        let _lifecycle = model_lifecycle_test_guard();
        let prov = RedactingProvider::with_name_redactor(
            Arc::new(EchoProvider),
            Arc::new(FixtureNameRedactor), // a real (non-noop) name layer
        );
        // A listing-shaped line whose title the FIXTURE does not detect: with NER active the
        // syntactic fallback must NOT fire, so the title passes through (the NER layer's job).
        let system = "- 3f2b1a | Tomasz Nowak | 2026-07-01";
        let out = block_on(prov.complete(system, "question")).unwrap();
        assert!(
            out.contains("Tomasz Nowak"),
            "with a real NER layer the syntactic complete-path fallback stays off: {out}"
        );
    }

    /// P0.1 — the conservative syntactic person-name predicate, tested directly. Positives are the
    /// bare person-page shapes; negatives cover blocklisted title words, digits, acronyms, single
    /// words, lowercase words, and long phrases.
    #[test]
    fn title_looks_like_person_name_predicate() {
        // POSITIVE — person-page shapes (2–4 Title-Case, digit-free, non-blocklisted words).
        assert!(title_looks_like_person_name("Anna Kowalska"));
        assert!(title_looks_like_person_name("Jan Maria Rokita"));
        assert!(title_looks_like_person_name("Anne-Marie O'Brien")); // internal hyphen/apostrophe
        assert!(title_looks_like_person_name("Łukasz Gawroński")); // Unicode uppercase + diacritics

        // NEGATIVE — meeting-shaped / non-name titles must survive the filter.
        assert!(!title_looks_like_person_name("Meeting Notes")); // EN blocklist
        assert!(!title_looks_like_person_name("Spotkanie Zarządu")); // PL blocklist
        assert!(!title_looks_like_person_name("Przegląd Kwartalny")); // PL blocklist (diacritics)
        assert!(!title_looks_like_person_name("Q3 OKRs")); // digit word
        assert!(!title_looks_like_person_name("Atlas Project")); // blocklisted "project"
        assert!(!title_looks_like_person_name("Budget 2026")); // digits
        assert!(!title_looks_like_person_name("Anna")); // single word
        assert!(!title_looks_like_person_name("CI CD Pipeline")); // all-caps acronyms
        assert!(!title_looks_like_person_name("Clean retained title")); // lowercase words
        assert!(!title_looks_like_person_name("Five Word Long Phrase Here")); // > 4 words
        assert!(!title_looks_like_person_name("")); // empty
    }

    /// DEFENSE-IN-DEPTH: a user-authored custom `template` (rides the SYSTEM prompt) and
    /// `meta.title_hint` (rides `render_user_content`) previously carried any email/card/phone the
    /// user typed straight past the firewall inside `req.clone()`. Both are now scrubbed through the
    /// shared regex map before egress.
    ///
    /// RED-before-GREEN: on the UNPATCHED code the emails were PRESENT in the egressed content;
    /// after the fix they are ABSENT while the non-PII instruction text still passes through.
    #[test]
    fn template_and_title_hint_are_scrubbed_before_egress() {
        let _lifecycle = model_lifecycle_test_guard();
        let mut req = sample_req("no PII in transcript");
        req.template = "Custom recipe — ping ops@corp.example for context.".to_string();
        req.meta.title_hint = Some("Sync re carl@corp.example".to_string());
        let inner = std::sync::Arc::new(CapturingInner(std::sync::Mutex::new(None)));
        let provider = RedactingProvider::new(inner.clone());
        block_on(provider.summarize(&req)).unwrap();
        let egressed = inner.0.lock().unwrap().clone().expect("inner was called");
        let egress = rendered_egress(&egressed);
        assert!(
            !egress.contains("ops@corp.example"),
            "template email must not egress: {egress}"
        );
        assert!(
            !egress.contains("carl@corp.example"),
            "title_hint email must not egress: {egress}"
        );
        // Non-PII instruction text still passes through (the template stays usable).
        assert!(
            egressed.template.contains("Custom recipe"),
            "template instruction text preserved"
        );
    }

    /// ROOT-CAUSE / FUTURE-PROOFING. `RedactingProvider` scrubs an ALLOWLIST of fields — the exact
    /// design that let `vault_titles` (and, before it, `user_notes`) slip through. This test places
    /// a UNIQUE sentinel PII string in EVERY PII-bearing String / `Vec<String>` field of
    /// `SummarizeRequest` and asserts none reaches the inner provider un-scrubbed. It enumerates the
    /// whole struct via a literal (there is no `Default`), so a NEWLY-ADDED string field forces a
    /// compile error here until it is explicitly classified — scrubbed (add a sentinel below) or
    /// exempt (a documented non-PII format flag). That is the guard the allowlist bug needed.
    #[test]
    fn every_string_field_of_summarize_request_is_scrubbed_or_exempt() {
        let _lifecycle = model_lifecycle_test_guard();
        use crate::summarize::provider::MeetingMeta;
        // Distinct email-shaped sentinels — deterministically caught by the regex layer (no model
        // needed), so this test is stable in the headless loop.
        let req = SummarizeRequest {
            transcript: "s-transcript@leak.example".to_string(), // SCRUBBED (regex + NER)
            meta: MeetingMeta {
                // EXEMPT — non-PII format flags, deliberately forwarded verbatim. NOTE: an ISO date
                // WOULD false-positive as a PHONE if it went through the firewall, so scrubbing it
                // would garble the note's date — a concrete reason it must stay exempt.
                date_iso: "2026-07-04".to_string(),
                title_hint: Some("s-hint@leak.example".to_string()), // SCRUBBED (regex)
                duration_s: 60,                                      // not a string
                language: Some("pl".to_string()),                    // EXEMPT (enum-like flag)
            },
            template: "recipe s-template@leak.example".to_string(), // SCRUBBED (regex, system prompt)
            vault_titles: vec![
                "Offer - s-title@leak.example".to_string(), // SCRUBBED (design B filter → dropped)
                // Control: a clean title must survive. NOT Title-Case-throughout on purpose — a
                // 2–4-word all-Title-Case title now trips the P0.1 no-NER person-name fallback
                // (this test runs the production no-op name layer), which would be CORRECT
                // filtering, not a leak; the control pins that ordinary titles still pass.
                "Clean retained title".to_string(),
            ],
            related_context: Some("s-related@leak.example".to_string()), // SCRUBBED (regex + NER)
            user_notes: Some("s-notes@leak.example".to_string()),        // SCRUBBED (regex + NER)
            live_bullets: Some("s-bullets@leak.example".to_string()),    // SCRUBBED (regex + NER)
        };
        let inner = std::sync::Arc::new(CapturingInner(std::sync::Mutex::new(None)));
        let provider = RedactingProvider::new(inner.clone());
        block_on(provider.summarize(&req)).unwrap();
        let egressed = inner.0.lock().unwrap().clone().expect("inner was called");
        let egress = rendered_egress(&egressed);
        for sentinel in [
            "s-transcript@leak.example",
            "s-hint@leak.example",
            "s-template@leak.example",
            "s-title@leak.example",
            "s-related@leak.example",
            "s-notes@leak.example",
            "s-bullets@leak.example",
        ] {
            assert!(
                !egress.contains(sentinel),
                "PII sentinel {sentinel} leaked to the inner provider: {egress}"
            );
        }
        // The two DELIBERATE exemptions still pass verbatim (non-PII format flags the note needs).
        assert!(
            egress.contains("2026-07-04"),
            "date_iso is a non-PII format flag, forwarded verbatim"
        );
        assert!(
            egress.contains("- language: pl"),
            "language is a non-PII enum-like flag, forwarded verbatim"
        );
        // A clean vault title must NOT be over-filtered.
        assert!(
            egress.contains("Clean retained title"),
            "clean titles must survive the filter as valid link targets"
        );
    }
}
