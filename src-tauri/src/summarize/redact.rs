//! Redaction firewall: scrub high-confidence PII (emails, card-like digit runs, phone numbers)
//! out of any text BEFORE it leaves for an LLM provider, then de-tokenize the reply so the
//! final note still reads with the real values. Wraps any provider at the make_provider seam.
//!
//! Honest scope: regex reliably catches emails / cards / phones. Personal NAMES are additionally
//! masked by the on-device NER name layer ([`NameRedactor`]) when the model is installed.
//!
//! Field coverage (the firewall's egress contract — kept in sync with the doc-comment on
//! [`RedactingProvider::summarize_with_meta`]): EVERY `SummarizeRequest` field that reaches the
//! inner provider is scrubbed there before egress — `transcript` / `related_context` /
//! `user_notes` / `live_bullets` / `glossary` / `template` / `meta.title_hint` use one shared
//! regex map and one global NER batch; `vault_titles` are FILTERED when the firewall would alter
//! them (a masked wikilink target is useless). Only the non-PII format flags (`meta.date_iso`,
//! `meta.language`, `meta.duration_s`) pass verbatim. A new string field is caught by
//! `every_string_field_of_summarize_request_is_scrubbed_or_exempt`.

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

/// Run the NER layer ONCE for all fields of one provider call. Real NER token numbering starts at
/// `NAME_1` per invocation, so redacting fields separately can assign the same token to different
/// names and restore the wrong person. A deterministic boundary keeps field identity without
/// influencing token numbering; if a redactor alters it, fail closed before egress.
type RedactedNameBatch = (Vec<Option<String>>, Vec<(String, String)>);

fn redact_names_batch(
    names: &dyn NameRedactor,
    fields: Vec<Option<String>>,
) -> Result<RedactedNameBatch> {
    let present: Vec<&String> = fields.iter().filter_map(Option::as_ref).collect();
    if present.is_empty() {
        return Ok((fields, Vec::new()));
    }

    let boundary = (0_u16..=u16::MAX)
        .map(|nonce| format!("\u{241e}MURMUR_NAME_FIELD_{nonce}\u{241f}"))
        .find(|candidate| present.iter().all(|field| !field.contains(candidate)))
        .ok_or_else(|| {
            AppError::Summarize("unable to allocate a safe name-redaction boundary".into())
        })?;
    let joined = present
        .iter()
        .map(|field| field.as_str())
        .collect::<Vec<_>>()
        .join(&boundary);

    let (redacted, pairs) = names.redact_names(&joined);
    let pieces: Vec<String> = redacted.split(&boundary).map(str::to_string).collect();
    if pieces.len() != present.len() {
        return Err(AppError::Summarize(
            "name redactor altered the field boundary; cloud dispatch refused".into(),
        ));
    }

    let mut token_names: HashMap<&str, &str> = HashMap::new();
    for (token, name) in &pairs {
        if joined.contains(token) {
            return Err(AppError::Summarize(
                "prompt contained a reserved name-redaction token; cloud dispatch refused".into(),
            ));
        }
        if let Some(existing) = token_names.insert(token.as_str(), name.as_str()) {
            if existing != name {
                return Err(AppError::Summarize(
                    "name redactor emitted a colliding restore token; cloud dispatch refused"
                        .into(),
                ));
            }
        }
    }

    let mut pieces = pieces.into_iter();
    let restored_shape = fields
        .into_iter()
        .map(|field| field.map(|_| pieces.next().unwrap_or_default()))
        .collect();
    Ok((restored_shape, pairs))
}

/// Exhaustive rendering class for every provider the redaction wrapper is allowed to dispatch.
///
/// This deliberately has no catch-all variant. A newly added cloud provider cannot silently inherit
/// another provider's prompt-byte accounting; it must be classified here before it can dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProviderEgressClass {
    SplitSystemUser,
    Ollama,
}

fn provider_egress_class(provider_id: &str) -> Result<ProviderEgressClass> {
    match provider_id {
        crate::summarize::PROVIDER_CLAUDE_CODE
        | crate::summarize::PROVIDER_ANTHROPIC
        | crate::summarize::PROVIDER_GATEWAY => Ok(ProviderEgressClass::SplitSystemUser),
        crate::summarize::PROVIDER_OLLAMA => Ok(ProviderEgressClass::Ollama),
        _ => Err(AppError::InvalidArg(
            "cloud provider has no registered egress rendering class; dispatch refused".into(),
        )),
    }
}

/// Render the exact two byte streams the wrapped provider sends for a note summary. Anthropic,
/// Gateway, and Claude Code use a system/user split; remote Ollama sends the same combined prompt
/// its provider implementation builds with `render_prompt`.
fn rendered_summarize_egress(
    class: ProviderEgressClass,
    req: &SummarizeRequest,
) -> (String, String) {
    match class {
        ProviderEgressClass::Ollama => (
            String::new(),
            crate::summarize::template::render_prompt(req),
        ),
        ProviderEgressClass::SplitSystemUser => {
            let system = if req.template.trim().is_empty() {
                crate::summarize::template::default_template()
            } else {
                req.template.clone()
            };
            let user = crate::summarize::template::render_user_content(req);
            (system, user)
        }
    }
}

/// Return the exact two text fields sent by a raw completion.
///
/// Unlike note summarization, Ollama completion does **not** concatenate the channels:
/// `OllamaProvider::complete` serializes them as distinct `/api/generate` JSON fields
/// (`system` and `prompt`). Keeping this provider-class match explicit prevents summary rendering
/// semantics from being incorrectly reused for completion receipts.
fn rendered_complete_egress<'a>(
    class: ProviderEgressClass,
    system: &'a str,
    user: &'a str,
) -> (&'a str, &'a str) {
    match class {
        ProviderEgressClass::SplitSystemUser => (system, user),
        ProviderEgressClass::Ollama => {
            crate::summarize::ollama::completion_prompt_parts(system, user)
        }
    }
}

/// Turn a provider failure into the only diagnostic that may cross the firewall back to callers.
///
/// Provider errors can embed response bodies, reflected prompts, URLs, or SDK debug output. The
/// application logs propagated errors in several outer lifecycle paths, so returning the original
/// value would make the ledger content-free while still leaking through ordinary diagnostics. Keep
/// only the typed failure category; the exact call outcome remains in the `summarize_error` receipt.
fn ollama_http_status(error: &AppError) -> Option<u16> {
    let AppError::Summarize(message) = error else {
        return None;
    };
    let suffix = message.strip_prefix("Ollama API returned HTTP ")?;
    let digits: String = suffix.chars().take_while(char::is_ascii_digit).collect();
    let status = digits.parse::<u16>().ok()?;
    (100..=599).contains(&status).then_some(status)
}

fn content_free_dispatch_error(provider_id: &str, error: &AppError) -> AppError {
    if provider_id == crate::summarize::PROVIDER_OLLAMA {
        if let Some(status) = ollama_http_status(error) {
            return AppError::Summarize(format!(
                "remote Ollama returned HTTP {status}; response details omitted"
            ));
        }
        if matches!(
            error,
            AppError::Summarize(message)
                if message.starts_with("failed to parse Ollama response")
        ) {
            return AppError::Summarize(
                "remote Ollama returned a malformed response; details omitted".into(),
            );
        }
        if matches!(
            error,
            AppError::Summarize(message)
                if message.starts_with("Ollama response contained no text")
        ) {
            return AppError::Summarize(
                "remote Ollama returned an empty response; details omitted".into(),
            );
        }
    }

    match error {
        AppError::Auth(_) => AppError::Auth(
            "cloud provider authentication failed after protected dispatch; details omitted".into(),
        ),
        AppError::Unavailable(_) => AppError::Unavailable(
            "cloud provider was unavailable after protected dispatch; details omitted".into(),
        ),
        AppError::Summarize(_) => AppError::Summarize(
            "cloud provider response failed after protected dispatch; details omitted".into(),
        ),
        _ => AppError::Summarize(
            "cloud provider dispatch failed after protected dispatch; details omitted".into(),
        ),
    }
}

/// Count only placeholders that are present in the exact rendered prompt streams. Fields retained
/// on `SummarizeRequest` but not rendered today (for example `related_context`) and filtered vault
/// titles therefore cannot inflate the privacy receipt.
fn count_rendered_redactions(
    map: &HashMap<String, String>,
    name_pairs: &[(String, String)],
    system: &str,
    user: &str,
) -> RedactionCounts {
    let mut rendered_map = HashMap::new();
    for (token, original) in map {
        if system.contains(token) || user.contains(token) {
            rendered_map.insert(token.clone(), original.clone());
        }
    }
    let rendered_names = name_pairs
        .iter()
        .filter(|(token, _)| system.contains(token) || user.contains(token))
        .count();
    count_redactions(&rendered_map, rendered_names)
}

/// Provider decorator: redacts PII from inputs, restores it in outputs, and records one
/// content-free egress audit entry per provider dispatch, including failed summary responses.
///
/// Two layers, both restored in the reply: the always-on regex scrubbers (emails/cards/phones,
/// via [`redact`]/[`restore`]) and the [`NameRedactor`] seam. The name layer defaults to
/// [`NoopNameRedactor`] (`new`), so production egress is byte-identical until a real NER model is
/// installed via [`with_name_redactor`](RedactingProvider::with_name_redactor).
///
/// The egress audit sink defaults to [`NoopEgressSink`] in `new`/`with_name_redactor`, preserving
/// byte-identical behaviour for callers that expose a registered provider identity.
/// `with_name_redactor_and_sink` is the full constructor used by `make_provider` to wire the live
/// `DbEgressSink`; unknown or mismatched identities fail closed before dispatch.
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
    /// Validate both the configured audit identity and the inner provider identity before any
    /// content preparation, egress lease, or provider dispatch. This closes two fail-open shapes:
    /// an unknown provider inheriting split-prompt accounting, and a configured id that does not
    /// describe the inner provider whose bytes actually leave the device.
    fn validated_egress_class(&self) -> Result<ProviderEgressClass> {
        let class = provider_egress_class(&self.provider_id)?;
        if self.provider_id != self.inner.id() {
            return Err(AppError::InvalidArg(
                "redaction wrapper provider identity mismatch; dispatch refused".into(),
            ));
        }
        Ok(class)
    }

    /// Wrap `inner` with the regex firewall and the DEFAULT (no-op) name redactor and NO-OP sink.
    /// Name egress and audit logging are unchanged. Content calls still require the inner to expose
    /// one of the registered cloud-provider ids so prompt accounting cannot fail open.
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
    /// layer. Sink defaults to no-op for registered-provider call sites.
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
    /// - `provider_id` must be a registered cloud provider and match `inner.id()`; every content
    ///   operation checks this before dispatch.
    /// - `provider_id` / `destination` / `model_requested` are forwarded into every entry.
    ///
    /// Existing registered-provider callers that use `new`/`with_name_redactor` remain
    /// byte-identical; only `make_provider` wires the live `DbEgressSink` through this path.
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
    /// - `transcript`, `related_context`, `user_notes`, `live_bullets`, `glossary`, `template`,
    ///   and `meta.title_hint` — full firewall: one shared regex map followed by ONE coherent NER
    ///   batch for the entire call, so per-call `NAME_n` numbering cannot collide across fields.
    ///   Both token layers are restored in the reply.
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
        let egress_class = self.validated_egress_class()?;

        // One regex map for every PII-bearing field in the request, followed below by one global
        // name-redactor batch. A repeated value therefore receives one stable token everywhere.
        let mut map = HashMap::new();
        let mut rev = HashMap::new();
        let red_transcript = redact_into(&req.transcript, &mut map, &mut rev);
        let red_related = req
            .related_context
            .as_ref()
            .map(|c| redact_into(c, &mut map, &mut rev));
        let red_notes = req
            .user_notes
            .as_ref()
            .map(|c| redact_into(c, &mut map, &mut rev));
        let red_bullets = req
            .live_bullets
            .as_ref()
            .map(|c| redact_into(c, &mut map, &mut rev));
        let red_glossary = req
            .glossary
            .as_ref()
            .map(|c| redact_into(c, &mut map, &mut rev));
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
        // Regex-detectable PII titles are filtered before the shared NER batch. The remaining titles
        // join that ONE batch as detectors; any title whose output changes is dropped.
        //
        // P0.1 (Brain v2) — the NO-NER fallback: when the active name layer is the no-op (no NER
        // model on this install), the NER detector below flags NOTHING, so an auto-created
        // `[[Anna Kowalska]].md` person page would egress verbatim. The conservative SYNTACTIC
        // detector `title_looks_like_person_name` covers that gap — active ONLY under the no-op, so
        // model-present installs keep the unchanged NER behavior.
        let titles: Vec<String> = req
            .vault_titles
            .iter()
            .filter(|title| redact(title.as_str()).0 == title.as_str())
            .cloned()
            .collect();
        let (
            red_transcript,
            red_related,
            red_notes,
            red_bullets,
            red_glossary,
            red_template,
            red_title_hint,
            name_pairs,
            red_titles,
        ) = self
            .run_name_redactor(move |names| {
                let mut fields = vec![
                    Some(red_transcript),
                    red_related,
                    red_notes,
                    red_bullets,
                    red_glossary,
                    Some(red_template),
                    red_title_hint,
                ];
                fields.extend(titles.iter().cloned().map(Some));
                let (fields, name_pairs) = redact_names_batch(names, fields)?;
                let mut fields = fields.into_iter();
                let red_transcript = fields.next().flatten().unwrap_or_default();
                let red_related = fields.next().flatten();
                let red_notes = fields.next().flatten();
                let red_bullets = fields.next().flatten();
                let red_glossary = fields.next().flatten();
                let red_template = fields.next().flatten().unwrap_or_default();
                let red_title_hint = fields.next().flatten();
                let noop_ner = names.is_noop();
                let red_titles = titles
                    .into_iter()
                    .zip(fields)
                    .filter_map(|(original, redacted)| {
                        let redacted = redacted.unwrap_or_default();
                        if (noop_ner && title_looks_like_person_name(&original))
                            || redacted != original
                        {
                            None
                        } else {
                            Some(original)
                        }
                    })
                    .collect();
                Ok((
                    red_transcript,
                    red_related,
                    red_notes,
                    red_bullets,
                    red_glossary,
                    red_template,
                    red_title_hint,
                    name_pairs,
                    red_titles,
                ))
            })
            .await?;

        let mut r = req.clone();
        r.transcript = red_transcript;
        r.related_context = red_related;
        r.user_notes = red_notes;
        r.live_bullets = red_bullets;
        r.glossary = red_glossary;
        r.template = red_template;
        r.meta.title_hint = red_title_hint;
        r.vault_titles = red_titles;
        let (rendered_system, rendered_user) = rendered_summarize_egress(egress_class, &r);
        let system_bytes = rendered_system.len();
        let user_bytes = rendered_user.len();
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
        // Count only tokens present in the exact rendered bytes handed to the provider. Compute
        // this BEFORE dispatch so a failed HTTP response, malformed body, or empty provider reply
        // still leaves one truthful, content-free receipt for the bytes that were attempted.
        let redactions =
            count_rendered_redactions(&map, &name_pairs, &rendered_system, &rendered_user);
        let (out, meta) = match self.inner.summarize_with_meta(&r).await {
            Ok(result) => result,
            Err(error) => {
                // The error text may contain a remote response body (or even reflected content), so
                // never put it in the ledger OR propagate it to an outer caller that may log it.
                // The static call kind is the audit outcome; the returned error preserves only a
                // typed, content-free category.
                self.sink.record(EgressEntry {
                    provider_id: self.provider_id.clone(),
                    destination: self.destination.clone(),
                    model_requested: self.model_requested.clone(),
                    call_kind: "summarize_error",
                    meta: CallMeta::default(),
                    redactions: redactions.clone(),
                    system_bytes,
                    user_bytes,
                    meeting_id: None,
                });
                return Err(content_free_dispatch_error(&self.provider_id, &error));
            }
        };
        // Restore both layers in the reply (disjoint token namespaces; order-independent).
        let out = restore_names(&out, &name_pairs);
        let out = restore(&out, &map);
        self.sink.record(EgressEntry {
            provider_id: self.provider_id.clone(),
            destination: self.destination.clone(),
            model_requested: self.model_requested.clone(),
            call_kind: "summarize",
            meta: meta.clone(),
            redactions: redactions.clone(),
            system_bytes,
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
        let egress_class = self.validated_egress_class()?;

        // Shared map so a value redacted in either prompt restores consistently.
        let mut map = HashMap::new();
        let mut rev = HashMap::new();
        let rsys = redact_into(system, &mut map, &mut rev);
        let ruser = redact_into(user, &mut map, &mut rev);
        // One coherent NER batch across both channels prevents NAME_n collisions when the same
        // implementation restarts numbering per call.
        let noop_ner = self.names.is_noop();
        let (rsys, ruser, name_pairs) = self
            .run_name_redactor(move |names| {
                let (mut fields, name_pairs) =
                    redact_names_batch(names, vec![Some(rsys), Some(ruser)])?;
                let rsys = fields.remove(0).unwrap_or_default();
                let ruser = fields.remove(0).unwrap_or_default();
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
        // Measure the exact final provider fields, after both NER and the no-NER title fallback.
        // Remote Ollama sends these as separate `system` and `prompt` JSON values; only its
        // summarize surface uses one combined prompt.
        let (rendered_system, rendered_user) =
            rendered_complete_egress(egress_class, &rsys, &ruser);
        let system_bytes = rendered_system.len();
        let user_bytes = rendered_user.len();
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
        // Compute this before dispatch: even a provider failure must leave one content-free receipt
        // bound to the exact scrubbed bytes that were attempted.
        let redactions =
            count_rendered_redactions(&map, &name_pairs, rendered_system, rendered_user);
        let _egress_lease =
            crate::perf::acquire_external_egress_lease(self.recording_token.as_ref())?;
        let (out, meta) = match self.inner.complete_with_meta(&rsys, &ruser).await {
            Ok(result) => result,
            Err(error) => {
                self.sink.record(EgressEntry {
                    provider_id: self.provider_id.clone(),
                    destination: self.destination.clone(),
                    model_requested: self.model_requested.clone(),
                    call_kind: "complete_error",
                    meta: CallMeta::default(),
                    redactions: redactions.clone(),
                    system_bytes,
                    user_bytes,
                    meeting_id: None,
                });
                return Err(content_free_dispatch_error(&self.provider_id, &error));
            }
        };
        let out = restore_names(&out, &name_pairs);
        let out = restore(&out, &map);
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
        let egress_class = self.validated_egress_class()?;

        // Shared map so a value redacted in either prompt restores consistently in the reply.
        let mut map = HashMap::new();
        let mut rev = HashMap::new();
        let rsys = redact_into(system, &mut map, &mut rev);
        let ruser = redact_into(user, &mut map, &mut rev);
        // One coherent NER batch across both channels, identical to `complete_with_meta`.
        let noop_ner = self.names.is_noop();
        let (rsys, ruser, name_pairs) = self
            .run_name_redactor(move |names| {
                let (mut fields, name_pairs) =
                    redact_names_batch(names, vec![Some(rsys), Some(ruser)])?;
                let rsys = fields.remove(0).unwrap_or_default();
                let ruser = fields.remove(0).unwrap_or_default();
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
        // Gateway sends `system` as-is and carries the schema in response_format. Every provider
        // on the trait default appends the schema instruction to the actual system prompt.
        let rendered_system = if self.inner.supports_native_json() {
            rsys.clone()
        } else {
            crate::summarize::provider::default_json_system_prompt(&rsys, schema)
        };
        let (rendered_system, rendered_user) =
            rendered_complete_egress(egress_class, &rendered_system, &ruser);
        let system_bytes = rendered_system.len();
        let user_bytes = rendered_user.len();
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
        // Count before dispatch so the error receipt remains exact even when no response meta exists.
        let redactions =
            count_rendered_redactions(&map, &name_pairs, rendered_system, rendered_user);
        let _egress_lease =
            crate::perf::acquire_external_egress_lease(self.recording_token.as_ref())?;
        let (value, meta) = match self
            .inner
            .complete_json_with_meta(&rsys, &ruser, schema)
            .await
        {
            Ok(result) => result,
            Err(error) => {
                self.sink.record(EgressEntry {
                    provider_id: self.provider_id.clone(),
                    destination: self.destination.clone(),
                    model_requested: self.model_requested.clone(),
                    call_kind: "complete_json_error",
                    meta: CallMeta::default(),
                    redactions: redactions.clone(),
                    system_bytes,
                    user_bytes,
                    meeting_id: None,
                });
                return Err(content_free_dispatch_error(&self.provider_id, &error));
            }
        };
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

    /// Mimics the real redactor's per-invocation numbering: the first name in EACH call is
    /// `NAME_1`. Separate field calls would therefore collide; one batch yields NAME_1 + NAME_2.
    struct ResettingNumberNameRedactor(std::sync::Arc<std::sync::atomic::AtomicUsize>);

    impl NameRedactor for ResettingNumberNameRedactor {
        fn redact_names(&self, text: &str) -> (String, Vec<(String, String)>) {
            use std::sync::atomic::Ordering;
            self.0.fetch_add(1, Ordering::SeqCst);
            let mut hits: Vec<(usize, &str)> = ["Anna Kowalska", "Bob Smith"]
                .into_iter()
                .filter_map(|name| text.find(name).map(|offset| (offset, name)))
                .collect();
            hits.sort_by_key(|(offset, _)| *offset);
            let mut out = text.to_string();
            let mut pairs = Vec::new();
            for (idx, (_, name)) in hits.into_iter().enumerate() {
                let token = format!("\u{27ea}NAME_{}\u{27eb}", idx + 1);
                out = out.replace(name, &token);
                pairs.push((token, name.to_string()));
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
            crate::summarize::PROVIDER_ANTHROPIC
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

    struct DispatchCounterProvider {
        provider_id: &'static str,
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait]
    impl SummarizerProvider for DispatchCounterProvider {
        fn id(&self) -> &str {
            self.provider_id
        }

        async fn availability(&self) -> Availability {
            Availability::Available
        }

        async fn summarize(&self, _req: &SummarizeRequest) -> Result<String> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(String::new())
        }

        async fn complete(&self, _system: &str, _user: &str) -> Result<String> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(String::new())
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

    static PROXY_ENV_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Temporarily route reqwest's system HTTP proxy to a controlled loopback listener.
    ///
    /// The production factory must receive a genuinely remote-classified URL, while the regression
    /// must never use external DNS/network. Environment mutation is serialized and restored on
    /// every exit path, including panic unwinding.
    struct ScopedProxyEnv {
        _lock: std::sync::MutexGuard<'static, ()>,
        saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
    }

    impl ScopedProxyEnv {
        fn install(proxy_url: &str) -> Self {
            const KEYS: [&str; 6] = [
                "HTTP_PROXY",
                "http_proxy",
                "ALL_PROXY",
                "all_proxy",
                "NO_PROXY",
                "no_proxy",
            ];
            let lock = PROXY_ENV_TEST_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let saved = KEYS
                .into_iter()
                .map(|key| (key, std::env::var_os(key)))
                .collect();
            for key in ["HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"] {
                std::env::set_var(key, proxy_url);
            }
            for key in ["NO_PROXY", "no_proxy"] {
                std::env::remove_var(key);
            }
            Self { _lock: lock, saved }
        }
    }

    impl Drop for ScopedProxyEnv {
        fn drop(&mut self) {
            for (key, value) in &self.saved {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
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
            glossary: None,
        }
    }

    #[test]
    fn unknown_provider_class_refuses_before_dispatch() {
        let _lifecycle = model_lifecycle_test_guard();
        use std::sync::atomic::{AtomicUsize, Ordering};

        let calls = Arc::new(AtomicUsize::new(0));
        let entries = Arc::new(std::sync::Mutex::new(Vec::new()));
        let provider = RedactingProvider::with_name_redactor_and_sink(
            Arc::new(DispatchCounterProvider {
                provider_id: "future-cloud-provider",
                calls: calls.clone(),
            }),
            Arc::new(NoopNameRedactor),
            Arc::new(CaptureEgressSink(entries.clone())),
            "future-cloud-provider".to_string(),
            "future.example".to_string(),
            "future-model".to_string(),
        );

        let error =
            block_on(provider.summarize_with_meta(&sample_req("must-not-dispatch@corp.example")))
                .expect_err("an unclassified provider must fail closed");
        assert!(matches!(error, AppError::InvalidArg(_)), "{error:?}");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "unknown provider must be rejected before inner dispatch"
        );
        assert!(
            entries.lock().unwrap().is_empty(),
            "a refused pre-dispatch call is not an egress event"
        );
    }

    #[test]
    fn configured_and_inner_provider_id_mismatch_refuses_before_dispatch() {
        let _lifecycle = model_lifecycle_test_guard();
        use std::sync::atomic::{AtomicUsize, Ordering};

        let calls = Arc::new(AtomicUsize::new(0));
        let entries = Arc::new(std::sync::Mutex::new(Vec::new()));
        let provider = RedactingProvider::with_name_redactor_and_sink(
            Arc::new(DispatchCounterProvider {
                provider_id: crate::summarize::PROVIDER_ANTHROPIC,
                calls: calls.clone(),
            }),
            Arc::new(NoopNameRedactor),
            Arc::new(CaptureEgressSink(entries.clone())),
            crate::summarize::PROVIDER_OLLAMA.to_string(),
            "remote-ollama.example".to_string(),
            "remote-model".to_string(),
        );

        let error =
            block_on(provider.summarize_with_meta(&sample_req("must-not-dispatch@corp.example")))
                .expect_err("a configured/inner provider mismatch must fail closed");
        assert!(matches!(error, AppError::InvalidArg(_)), "{error:?}");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "provider identity mismatch must be rejected before inner dispatch"
        );
        assert!(
            entries.lock().unwrap().is_empty(),
            "a refused pre-dispatch call is not an egress event"
        );
    }

    #[test]
    fn remote_glossary_provider_requires_factory_consent_before_dispatch() {
        let _lifecycle = model_lifecycle_test_guard();
        let config = crate::settings::AppConfig {
            ollama_base_url: "https://remote-ollama.example".to_string(),
            cloud_egress_consented: false,
            ..crate::settings::AppConfig::default()
        };
        let mut request = sample_req("consent-gate-prompt@corp.example");
        request.glossary =
            Some("- {\"canonical\":\"Murmur\",\"aliases\":[\"MeetNotes\"]}\n".to_string());

        let error = crate::summarize::make_provider(
            crate::summarize::PROVIDER_OLLAMA,
            &config,
            &Arc::new(tokio::sync::Semaphore::new(1)),
        )
        .map(|_| ())
        .expect_err("remote Ollama must not even be constructed without cloud consent");
        assert!(matches!(error, AppError::Unavailable(_)), "{error:?}");
        assert!(
            error.to_string().contains(crate::errcode::CLOUD_CONSENT),
            "the factory must fail at the explicit consent seam: {error}"
        );
        assert!(
            request
                .glossary
                .as_deref()
                .unwrap()
                .contains("\"canonical\":\"Murmur\""),
            "the fixture proves a real glossary-bearing request was pending when construction failed"
        );
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
                crate::summarize::PROVIDER_ANTHROPIC
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
                crate::summarize::PROVIDER_ANTHROPIC
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

    #[test]
    fn summarize_redacts_all_fields_in_one_collision_free_name_batch() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let _lifecycle = model_lifecycle_test_guard();
        let calls = Arc::new(AtomicUsize::new(0));
        let captured = Arc::new(std::sync::Mutex::new(None));

        struct CaptureGlossaryEcho(Arc<std::sync::Mutex<Option<SummarizeRequest>>>);
        #[async_trait]
        impl SummarizerProvider for CaptureGlossaryEcho {
            fn id(&self) -> &str {
                "anthropic"
            }
            async fn availability(&self) -> Availability {
                Availability::Available
            }
            async fn summarize(&self, req: &SummarizeRequest) -> Result<String> {
                *self.0.lock().unwrap() = Some(req.clone());
                Ok(format!(
                    "{}\n{}",
                    req.transcript,
                    req.glossary.as_deref().unwrap_or("")
                ))
            }
            async fn complete(&self, _system: &str, _user: &str) -> Result<String> {
                Ok(String::new())
            }
        }

        let provider = RedactingProvider::with_name_redactor(
            Arc::new(CaptureGlossaryEcho(captured.clone())),
            Arc::new(ResettingNumberNameRedactor(calls.clone())),
        );
        let mut req = sample_req("Anna Kowalska owns the rollout.");
        req.glossary = Some("- canonical: Bob Smith\n".to_string());
        let restored = block_on(provider.summarize(&req)).unwrap();

        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "all request fields must share one NER invocation"
        );
        let sent = captured.lock().unwrap().clone().unwrap();
        assert!(sent.transcript.contains("\u{27ea}NAME_1\u{27eb}"));
        assert!(
            sent.glossary
                .as_deref()
                .unwrap()
                .contains("\u{27ea}NAME_2\u{27eb}"),
            "a second field must not restart at NAME_1"
        );
        assert!(!sent.transcript.contains("Anna Kowalska"));
        assert!(!sent.glossary.unwrap().contains("Bob Smith"));
        assert!(restored.contains("Anna Kowalska"));
        assert!(restored.contains("Bob Smith"));
        assert!(!restored.contains("NAME_"));
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
                crate::summarize::PROVIDER_ANTHROPIC
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
                    crate::summarize::PROVIDER_ANTHROPIC
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
            crate::summarize::PROVIDER_ANTHROPIC.into(),
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
            crate::summarize::PROVIDER_ANTHROPIC.into(),
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
            crate::summarize::PROVIDER_ANTHROPIC
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

    /// A provider can fail only after it accepted the scrubbed prompt bytes. Both completion
    /// surfaces must therefore write exactly one error receipt before returning a content-free
    /// diagnostic; success-only ledger writes would silently lose evidence of real egress.
    #[test]
    fn failed_complete_surfaces_record_exact_content_free_receipts() {
        let _lifecycle = model_lifecycle_test_guard();

        struct FailingCompletionProvider;

        #[async_trait]
        impl SummarizerProvider for FailingCompletionProvider {
            fn id(&self) -> &str {
                crate::summarize::PROVIDER_ANTHROPIC
            }

            async fn availability(&self) -> Availability {
                Availability::Available
            }

            async fn summarize(&self, _req: &SummarizeRequest) -> Result<String> {
                Ok(String::new())
            }

            async fn complete(&self, _system: &str, _user: &str) -> Result<String> {
                Err(AppError::Summarize("provider-response-body-secret".into()))
            }

            async fn complete_json_with_meta(
                &self,
                _system: &str,
                _user: &str,
                _schema: &serde_json::Value,
            ) -> Result<(serde_json::Value, CallMeta)> {
                Err(AppError::Summarize(
                    "provider-json-response-body-secret".into(),
                ))
            }
        }

        let captured = Arc::new(std::sync::Mutex::new(Vec::new()));
        let provider = RedactingProvider::with_name_redactor_and_sink(
            Arc::new(FailingCompletionProvider),
            Arc::new(NoopNameRedactor),
            Arc::new(CaptureEgressSink(captured.clone())),
            crate::summarize::PROVIDER_ANTHROPIC.to_string(),
            "api.anthropic.com".to_string(),
            "test-model".to_string(),
        );
        let system = "System contact system@corp.example.";
        let user = "Bounded user prompt.";
        let (redacted_system, redaction_map) = redact(system);

        let complete_error = block_on(provider.complete_with_meta(system, user))
            .expect_err("the fixture provider must fail after dispatch");
        let complete_diagnostic = format!("{complete_error:?} / {complete_error}");
        assert!(
            !complete_diagnostic.contains("provider-response-body-secret"),
            "{complete_diagnostic}"
        );
        assert!(
            complete_diagnostic.contains("details omitted"),
            "{complete_diagnostic}"
        );
        {
            let entries = captured.lock().unwrap();
            assert_eq!(entries.len(), 1, "one failed complete, one receipt");
            let entry = &entries[0];
            assert_eq!(entry.call_kind, "complete_error");
            assert_eq!(entry.meta, CallMeta::default());
            assert_eq!(entry.system_bytes, redacted_system.len());
            assert_eq!(entry.user_bytes, user.len());
            assert_eq!(entry.redactions, count_redactions(&redaction_map, 0));
            let debug = format!("{entry:?}");
            assert!(!debug.contains(system), "{debug}");
            assert!(!debug.contains("provider-response-body-secret"), "{debug}");
        }

        let schema = serde_json::json!({
            "type": "object",
            "properties": {"answer": {"type": "string"}}
        });
        let json_error = block_on(provider.complete_json_with_meta(system, user, &schema))
            .expect_err("the JSON fixture provider must fail after dispatch");
        let json_diagnostic = format!("{json_error:?} / {json_error}");
        assert!(
            !json_diagnostic.contains("provider-json-response-body-secret"),
            "{json_diagnostic}"
        );
        assert!(
            json_diagnostic.contains("details omitted"),
            "{json_diagnostic}"
        );
        let entries = captured.lock().unwrap();
        assert_eq!(entries.len(), 2, "two failed dispatches, two receipts");
        let entry = &entries[1];
        assert_eq!(entry.call_kind, "complete_json_error");
        assert_eq!(entry.meta, CallMeta::default());
        assert_eq!(
            entry.system_bytes,
            crate::summarize::provider::default_json_system_prompt(&redacted_system, &schema).len()
        );
        assert_eq!(entry.user_bytes, user.len());
        assert_eq!(entry.redactions, count_redactions(&redaction_map, 0));
        let debug = format!("{entry:?}");
        assert!(!debug.contains(system), "{debug}");
        assert!(
            !debug.contains("provider-json-response-body-secret"),
            "{debug}"
        );
    }

    #[test]
    fn summarize_receipt_measures_exact_rendered_prompts_and_visible_redactions() {
        let _lifecycle = model_lifecycle_test_guard();
        let sent = Arc::new(std::sync::Mutex::new(None));
        let entries = Arc::new(std::sync::Mutex::new(Vec::new()));

        struct CaptureSummaryMeta(Arc<std::sync::Mutex<Option<SummarizeRequest>>>);
        #[async_trait]
        impl SummarizerProvider for CaptureSummaryMeta {
            fn id(&self) -> &str {
                "anthropic"
            }
            async fn availability(&self) -> Availability {
                Availability::Available
            }
            async fn summarize(&self, req: &SummarizeRequest) -> Result<String> {
                Ok(req.transcript.clone())
            }
            async fn complete(&self, _system: &str, _user: &str) -> Result<String> {
                Ok(String::new())
            }
            async fn summarize_with_meta(
                &self,
                req: &SummarizeRequest,
            ) -> Result<(String, CallMeta)> {
                *self.0.lock().unwrap() = Some(req.clone());
                Ok((req.transcript.clone(), CallMeta::default()))
            }
        }

        let provider = RedactingProvider::with_name_redactor_and_sink(
            Arc::new(CaptureSummaryMeta(sent.clone())),
            Arc::new(NoopNameRedactor),
            Arc::new(CaptureEgressSink(entries.clone())),
            "anthropic".to_string(),
            "api.anthropic.com".to_string(),
            "test-model".to_string(),
        );
        let mut req = sample_req("Ping transcript@corp.example.");
        req.template = "System contact system@corp.example.".to_string();
        req.glossary = Some("- canonical: glossary@corp.example; aliases: Glossary\n".to_string());
        // Retained on the request but intentionally NOT rendered in the Stage-1 prompt.
        req.related_context = Some("hidden@corp.example".to_string());
        // Filtered entirely, so it must neither egress nor inflate the rendered-redaction count.
        req.vault_titles = vec!["Offer title@corp.example".to_string()];

        let (_, meta) = block_on(provider.summarize_with_meta(&req)).unwrap();
        let sent = sent.lock().unwrap().clone().unwrap();
        let expected_system = sent.template.clone();
        let expected_user = crate::summarize::template::render_user_content(&sent);
        let entries = entries.lock().unwrap();
        let entry = entries.first().expect("one summarize receipt");

        assert_eq!(entry.system_bytes, expected_system.len());
        assert_eq!(entry.user_bytes, expected_user.len());
        assert!(
            entry.user_bytes > sent.transcript.len(),
            "receipt measures the rendered metadata/glossary/transcript prompt, not raw fields"
        );
        assert_eq!(
            entry.redactions.email, 3,
            "system + transcript + glossary egress; hidden related context and dropped title do not"
        );
        assert_eq!(meta.redactions.as_ref(), Some(&entry.redactions));
        for pii in [
            "system@corp.example",
            "transcript@corp.example",
            "glossary@corp.example",
            "hidden@corp.example",
            "title@corp.example",
        ] {
            assert!(
                !format!("{expected_system}\n{expected_user}").contains(pii),
                "{pii} must not appear in the exact rendered egress"
            );
        }
    }

    /// Remote Ollama is cloud egress but uses one combined `/api/generate` prompt rather than the
    /// system/user split used by Anthropic, Gateway, and Claude Code. The content-free receipt must
    /// therefore bind its byte count and scrub count to that exact combined prompt.
    #[test]
    fn remote_ollama_receipt_binds_exact_combined_prompt() {
        let _lifecycle = model_lifecycle_test_guard();
        let sent = Arc::new(std::sync::Mutex::new(None));
        let entries = Arc::new(std::sync::Mutex::new(Vec::new()));

        struct CaptureRemoteOllama(Arc<std::sync::Mutex<Option<SummarizeRequest>>>);
        #[async_trait]
        impl SummarizerProvider for CaptureRemoteOllama {
            fn id(&self) -> &str {
                crate::summarize::PROVIDER_OLLAMA
            }

            async fn availability(&self) -> Availability {
                Availability::Available
            }

            async fn summarize(&self, req: &SummarizeRequest) -> Result<String> {
                Ok(req.transcript.clone())
            }

            async fn complete(&self, _system: &str, _user: &str) -> Result<String> {
                Ok(String::new())
            }

            async fn summarize_with_meta(
                &self,
                req: &SummarizeRequest,
            ) -> Result<(String, CallMeta)> {
                *self.0.lock().unwrap() = Some(req.clone());
                Ok((req.transcript.clone(), CallMeta::default()))
            }
        }

        let provider = RedactingProvider::with_name_redactor_and_sink(
            Arc::new(CaptureRemoteOllama(sent.clone())),
            Arc::new(NoopNameRedactor),
            Arc::new(CaptureEgressSink(entries.clone())),
            crate::summarize::PROVIDER_OLLAMA.to_string(),
            "ollama.example".to_string(),
            "remote-test-model".to_string(),
        );
        let mut req = sample_req("Ping transcript@corp.example.");
        req.template = "System contact system@corp.example.".to_string();
        req.glossary = Some("- canonical: glossary@corp.example; aliases: Glossary\n".to_string());
        req.related_context = Some("hidden@corp.example".to_string());
        req.vault_titles = vec!["Offer title@corp.example".to_string()];

        let (_, meta) = block_on(provider.summarize_with_meta(&req)).unwrap();
        let sent = sent.lock().unwrap().clone().expect("inner was called");
        let combined = crate::summarize::template::render_prompt(&sent);
        let entries = entries.lock().unwrap();
        let entry = entries.first().expect("one remote Ollama receipt");

        assert_eq!(entries.len(), 1, "one receipt for one remote Ollama call");
        assert_eq!(entry.provider_id, crate::summarize::PROVIDER_OLLAMA);
        assert_eq!(entry.destination, "ollama.example");
        assert_eq!(entry.call_kind, "summarize");
        assert_eq!(
            entry.system_bytes, 0,
            "Ollama has no separate system channel"
        );
        assert_eq!(
            entry.user_bytes,
            combined.len(),
            "receipt bytes must equal the exact combined prompt sent by OllamaProvider"
        );
        assert!(combined.contains("System contact"));
        assert!(combined.contains("WORKSPACE GLOSSARY"));
        assert!(combined.contains("Ping "));
        assert_eq!(
            entry.redactions.email, 3,
            "combined template + transcript + glossary egress; hidden context and dropped title do not"
        );
        assert_eq!(meta.redactions.as_ref(), Some(&entry.redactions));
        for pii in [
            "system@corp.example",
            "transcript@corp.example",
            "glossary@corp.example",
            "hidden@corp.example",
            "title@corp.example",
        ] {
            assert!(
                !combined.contains(pii),
                "{pii} must not appear in the exact combined remote Ollama egress"
            );
        }
    }

    /// RED-before-GREEN through the PRODUCTION egress seam: once the remote Ollama server has
    /// received the combined prompt, an HTTP failure, malformed body, or empty completion must
    /// still produce exactly one durable, content-free error receipt. This deliberately uses
    /// `make_provider` with consent enabled plus the real `DbEgressSink`; constructing the wrapper
    /// or an in-memory capture sink directly would not prove the production factory wiring.
    /// Before the fix, the `?` on the inner provider result returned before `sink.record`, leaving
    /// no evidence that bytes had left the device.
    #[test]
    fn remote_ollama_failures_record_one_exact_content_free_error_receipt() {
        use std::io::{Read, Write};

        let _lifecycle = model_lifecycle_test_guard();
        let db = Arc::new(
            crate::storage::Db::open_with_key(
                std::path::Path::new(":memory:"),
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            )
            .unwrap(),
        );
        crate::summarize::egress_log::set_egress_sink(Arc::new(
            crate::summarize::egress_log::DbEgressSink::new(db.clone()),
        ));
        let cases: [(&str, &str, &str, &[u8]); 3] = [
            (
                "http-status",
                "503 Service Unavailable",
                "provider-http-body-secret",
                br#"{"error":"provider-http-body-secret"}"#,
            ),
            (
                "malformed-body",
                "200 OK",
                "provider-malformed-body-secret",
                b"provider-malformed-body-secret",
            ),
            (
                "empty-completion",
                "200 OK",
                "provider-empty-body-secret",
                br#"{"response":"   ","diagnostic":"provider-empty-body-secret"}"#,
            ),
        ];

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            for (case, status, body_sentinel, response_body) in cases {
                let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
                listener.set_nonblocking(true).unwrap();
                let addr = listener.local_addr().unwrap();
                let status = status.to_string();
                let response_body = response_body.to_vec();
                let server = std::thread::spawn(move || {
                    let started = std::time::Instant::now();
                    let (mut socket, _) = loop {
                        match listener.accept() {
                            Ok(accepted) => break accepted,
                            Err(error)
                                if error.kind() == std::io::ErrorKind::WouldBlock
                                    && started.elapsed() < std::time::Duration::from_secs(10) =>
                            {
                                std::thread::sleep(std::time::Duration::from_millis(10));
                            }
                            Err(error) => panic!("controlled Ollama fixture did not connect: {error}"),
                        }
                    };
                    socket.set_nonblocking(false).unwrap();
                    socket
                        .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                        .unwrap();
                    let mut request = Vec::new();
                    let mut buf = [0u8; 4096];
                    loop {
                        let read = socket.read(&mut buf).unwrap();
                        if read == 0 {
                            break;
                        }
                        request.extend_from_slice(&buf[..read]);
                        let Some(header_end) =
                            request.windows(4).position(|window| window == b"\r\n\r\n")
                        else {
                            continue;
                        };
                        let headers = String::from_utf8_lossy(&request[..header_end]);
                        let content_len = headers
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().ok())
                                    .flatten()
                            })
                            .unwrap_or(0);
                        if request.len() >= header_end + 4 + content_len {
                            break;
                        }
                    }
                    write!(
                        socket,
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        response_body.len()
                    )
                    .unwrap();
                    socket.write_all(&response_body).unwrap();
                    String::from_utf8(request).unwrap()
                });

                // The production target stays genuinely remote-classified. Reqwest reaches it only
                // through this case's scoped, loopback-only HTTP proxy, so the test uses neither
                // external DNS nor network and does not need a test-only classification bypass.
                let model = format!("factory-integration-{case}");
                let config = crate::settings::AppConfig {
                    ollama_base_url: "http://ollama.remote.invalid".to_string(),
                    ollama_model: model.clone(),
                    cloud_egress_consented: true,
                    ..crate::settings::AppConfig::default()
                };
                assert!(
                    crate::summarize::egress_is_cloud(crate::summarize::PROVIDER_OLLAMA, &config),
                    "{case}: fixture URL must traverse the production remote-Ollama branch"
                );
                let _proxy =
                    ScopedProxyEnv::install(&format!("http://127.0.0.1:{}", addr.port()));
                let provider = crate::summarize::make_provider(
                    crate::summarize::PROVIDER_OLLAMA,
                    &config,
                    &Arc::new(tokio::sync::Semaphore::new(1)),
                )
                .unwrap();
                let prompt_sentinel = format!("{case}-prompt@corp.example");
                let mut req = sample_req(&format!("Ping {prompt_sentinel}."));
                req.template = "System contact system@corp.example.".to_string();
                req.glossary =
                    Some("- canonical: glossary@corp.example; aliases: Glossary\n".to_string());
                req.related_context = Some("hidden@corp.example".to_string());
                req.vault_titles = vec!["Offer title@corp.example".to_string()];

                let error = tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    provider.summarize_with_meta(&req),
                )
                .await
                .expect("the controlled remote-Ollama fixture must respond within 10 seconds")
                .expect_err("each fixture must fail after the request was accepted");
                let request = server.join().unwrap();
                let sent_body = request.split("\r\n\r\n").nth(1).expect("HTTP request body");
                let sent: serde_json::Value = serde_json::from_str(sent_body).unwrap();
                let combined = sent
                    .get("prompt")
                    .and_then(serde_json::Value::as_str)
                    .expect("Ollama combined prompt");

                let rows = {
                    let conn = db.lock();
                    let mut statement = conn
                        .prepare(
                            "SELECT provider_id, destination, model_requested, call_kind, model_served,
                                    prompt_tokens, completion_tokens, total_tokens, cached_tokens,
                                    redactions_email, redactions_card, redactions_phone, redactions_name,
                                    system_bytes, user_bytes, meeting_id
                               FROM egress_log
                              WHERE model_requested = ?1",
                        )
                        .unwrap();
                    statement
                        .query_map(rusqlite::params![model], |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, String>(2)?,
                                row.get::<_, String>(3)?,
                                row.get::<_, Option<String>>(4)?,
                                row.get::<_, Option<i64>>(5)?,
                                row.get::<_, Option<i64>>(6)?,
                                row.get::<_, Option<i64>>(7)?,
                                row.get::<_, Option<i64>>(8)?,
                                row.get::<_, i64>(9)?,
                                row.get::<_, i64>(10)?,
                                row.get::<_, i64>(11)?,
                                row.get::<_, i64>(12)?,
                                row.get::<_, i64>(13)?,
                                row.get::<_, i64>(14)?,
                                row.get::<_, Option<String>>(15)?,
                            ))
                        })
                        .unwrap()
                        .collect::<std::result::Result<Vec<_>, _>>()
                        .unwrap()
                };
                assert_eq!(
                    rows.len(),
                    1,
                    "{case}: one failed production dispatch must yield exactly one durable receipt"
                );
                let entry = &rows[0];
                assert_eq!(entry.0, crate::summarize::PROVIDER_OLLAMA, "{case}");
                assert_eq!(entry.2, model, "{case}");
                assert_eq!(entry.3, "summarize_error", "{case}");
                assert_eq!(
                    (&entry.4, entry.5, entry.6, entry.7, entry.8),
                    (&None, None, None, None, None),
                    "{case}: a failed response has no provider metadata"
                );
                assert_eq!(entry.13, 0, "{case}");
                assert_eq!(
                    entry.14,
                    combined.len() as i64,
                    "{case}: receipt must measure the exact combined prompt actually sent"
                );
                assert_eq!(entry.9, 3, "{case}");
                assert_eq!((entry.10, entry.11, entry.12), (0, 0, 0), "{case}");
                assert_eq!(entry.1, "ollama.remote.invalid", "{case}");
                assert!(entry.15.is_none(), "{case}");
                let durable_text_fields = format!(
                    "{} {} {} {} {:?} {:?}",
                    entry.0, entry.1, entry.2, entry.3, entry.4, entry.15
                );

                for secret in [
                    "system@corp.example",
                    "glossary@corp.example",
                    "hidden@corp.example",
                    "title@corp.example",
                    prompt_sentinel.as_str(),
                    body_sentinel,
                ] {
                    assert!(
                        !durable_text_fields.contains(secret),
                        "{case}: durable receipt must stay content-free"
                    );
                    assert!(
                        !combined.contains(secret),
                        "{case}: exact remote prompt must be redacted"
                    );
                }
                // Outer pipeline/command paths log propagated errors with `%error`. This is the exact
                // Display boundary they observe: it must retain only a typed/category/status diagnostic,
                // never the untrusted provider body or any prompt field.
                let caller_log = format!("summarize failed: {error}");
                for secret in [prompt_sentinel.as_str(), body_sentinel] {
                    assert!(
                        !error.to_string().contains(secret),
                        "{case}: returned diagnostic must be content-free"
                    );
                    assert!(
                        !format!("{error:?}").contains(secret),
                        "{case}: Debug diagnostic must be content-free"
                    );
                    assert!(
                        !caller_log.contains(secret),
                        "{case}: caller logging boundary must be content-free"
                    );
                }
                match case {
                    "http-status" => assert!(caller_log.contains("HTTP 503"), "{caller_log}"),
                    "malformed-body" => assert!(caller_log.contains("malformed"), "{caller_log}"),
                    "empty-completion" => assert!(caller_log.contains("empty"), "{caller_log}"),
                    _ => unreachable!(),
                }
                assert!(
                    caller_log.contains("details omitted"),
                    "{case}: diagnostic must make suppression explicit"
                );
            }
        });
    }

    /// Ollama has two different production renderings at `/api/generate`: summaries use one
    /// locally combined `prompt`, while raw completions use distinct JSON `system` and `prompt`
    /// fields. Bind both completion receipt surfaces to the fields observed at the real HTTP
    /// boundary on success and failure; inventing a combined completion stream would make the
    /// audit row disagree with the request Murmur actually sent.
    #[test]
    fn ollama_completion_receipts_match_exact_generate_fields_on_success_and_failure() {
        use std::io::{Read, Write};

        #[derive(Clone, Copy)]
        enum Surface {
            Complete,
            CompleteJson,
        }

        fn start_generate_fixture(
            status: &str,
            response_body: &[u8],
        ) -> (String, std::thread::JoinHandle<serde_json::Value>) {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let status = status.to_string();
            let response_body = response_body.to_vec();
            let server = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                    .unwrap();
                let mut request = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    let read = socket.read(&mut buf).unwrap();
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buf[..read]);
                    let Some(header_end) =
                        request.windows(4).position(|window| window == b"\r\n\r\n")
                    else {
                        continue;
                    };
                    let headers = String::from_utf8_lossy(&request[..header_end]);
                    let content_len = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .unwrap_or(0);
                    if request.len() >= header_end + 4 + content_len {
                        break;
                    }
                }
                write!(
                    socket,
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    response_body.len()
                )
                .unwrap();
                socket.write_all(&response_body).unwrap();
                let body = request
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                    .map(|offset| &request[offset + 4..])
                    .expect("HTTP request body");
                serde_json::from_slice(body).unwrap()
            });
            (format!("http://{addr}"), server)
        }

        let _lifecycle = model_lifecycle_test_guard();
        let schema = serde_json::json!({
            "type": "object",
            "properties": {"answer": {"type": "string"}},
            "required": ["answer"]
        });
        let cases = [
            ("complete-success", Surface::Complete, true),
            ("complete-failure", Surface::Complete, false),
            ("json-success", Surface::CompleteJson, true),
            ("json-failure", Surface::CompleteJson, false),
        ];

        for (label, surface, succeeds) in cases {
            let response_body: &[u8] = match (surface, succeeds) {
                (Surface::Complete, true) => br#"{"response":"ok"}"#,
                (Surface::CompleteJson, true) => br#"{"response":"{\"answer\":\"ok\"}"}"#,
                (_, false) => br#"{"error":"provider-response-body-secret"}"#,
            };
            let status = if succeeds {
                "200 OK"
            } else {
                "503 Service Unavailable"
            };
            let (base_url, server) = start_generate_fixture(status, response_body);
            let entries = Arc::new(std::sync::Mutex::new(Vec::new()));
            let provider = RedactingProvider::with_name_redactor_and_sink(
                Arc::new(crate::summarize::ollama::OllamaProvider::new(
                    base_url,
                    format!("boundary-{label}"),
                )),
                Arc::new(NoopNameRedactor),
                Arc::new(CaptureEgressSink(entries.clone())),
                crate::summarize::PROVIDER_OLLAMA.to_string(),
                "controlled-loopback-fixture".to_string(),
                format!("boundary-{label}"),
            );
            let system = format!("System {label} contact system@corp.example.");
            let user = format!("User {label} contact user@corp.example.");

            let error = match surface {
                Surface::Complete => match block_on(provider.complete_with_meta(&system, &user)) {
                    Ok(_) => None,
                    Err(error) => Some(error),
                },
                Surface::CompleteJson => {
                    match block_on(provider.complete_json_with_meta(&system, &user, &schema)) {
                        Ok(_) => None,
                        Err(error) => Some(error),
                    }
                }
            };
            assert_eq!(
                error.is_none(),
                succeeds,
                "{label}: fixture outcome must match the case"
            );

            let sent = server.join().unwrap();
            let sent_system = sent
                .get("system")
                .and_then(serde_json::Value::as_str)
                .expect("completion system field");
            let sent_prompt = sent
                .get("prompt")
                .and_then(serde_json::Value::as_str)
                .expect("completion prompt field");
            assert_eq!(
                sent.get("stream"),
                Some(&serde_json::Value::Bool(false)),
                "{label}"
            );
            assert_eq!(
                sent.get("keep_alive"),
                Some(&serde_json::json!(0)),
                "{label}"
            );
            assert!(
                !sent_system.contains("system@corp.example")
                    && !sent_prompt.contains("user@corp.example"),
                "{label}: exact HTTP fields must be redacted"
            );
            match surface {
                Surface::Complete => assert!(
                    !sent_system.contains("Respond with ONLY"),
                    "{label}: free-text completion must not gain a JSON instruction"
                ),
                Surface::CompleteJson => assert!(
                    sent_system.contains("Respond with ONLY a single JSON object"),
                    "{label}: JSON fallback schema instruction belongs in the system field"
                ),
            }

            let entries = entries.lock().unwrap();
            assert_eq!(entries.len(), 1, "{label}: one dispatch, one receipt");
            let entry = &entries[0];
            assert_eq!(
                entry.call_kind,
                match (surface, succeeds) {
                    (Surface::Complete, true) => "complete",
                    (Surface::Complete, false) => "complete_error",
                    (Surface::CompleteJson, true) => "complete_json",
                    (Surface::CompleteJson, false) => "complete_json_error",
                },
                "{label}"
            );
            assert_eq!(
                entry.system_bytes,
                sent_system.len(),
                "{label}: system_bytes must equal the actual JSON system field"
            );
            assert_eq!(
                entry.user_bytes,
                sent_prompt.len(),
                "{label}: user_bytes must equal the actual JSON prompt field"
            );
            assert_eq!(
                entry.redactions.email, 2,
                "{label}: both exact HTTP fields contain one scrubbed email"
            );
            assert_eq!(
                (
                    entry.redactions.card,
                    entry.redactions.phone,
                    entry.redactions.name
                ),
                (0, 0, 0),
                "{label}"
            );
            let receipt_debug = format!("{entry:?}");
            for secret in [
                "system@corp.example",
                "user@corp.example",
                "provider-response-body-secret",
            ] {
                assert!(
                    !receipt_debug.contains(secret),
                    "{label}: receipt must remain content-free"
                );
            }
            if let Some(error) = error {
                let diagnostic = format!("{error:?} / {error}");
                assert!(diagnostic.contains("HTTP 503"), "{label}: {diagnostic}");
                assert!(
                    diagnostic.contains("details omitted")
                        && !diagnostic.contains("provider-response-body-secret"),
                    "{label}: {diagnostic}"
                );
            }
        }
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
                crate::summarize::PROVIDER_GATEWAY
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
            crate::summarize::PROVIDER_ANTHROPIC
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
            glossary: Some("- canonical: s-glossary@leak.example; aliases: Alias".to_string()), // SCRUBBED (regex + NER)
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
            "s-glossary@leak.example",
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
