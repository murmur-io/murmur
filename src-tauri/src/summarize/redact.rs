//! Redaction firewall: scrub high-confidence PII (emails, card-like digit runs, phone numbers)
//! out of any text BEFORE it leaves for an LLM provider, then de-tokenize the reply so the
//! final note still reads with the real values. Wraps any provider at the make_provider seam.
//!
//! Honest scope: regex reliably catches emails / cards / phones. Personal NAMES need on-device
//! NER (not in this stack) and are therefore NOT redacted here — surfaced in the Settings copy.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use regex::Regex;

use crate::error::Result;
use crate::summarize::provider::{Availability, SummarizeRequest, SummarizerProvider};

// ── Phase D model plumbing (shared by the feature-gated redactor + the download command) ──────────
// The real DebertaNameRedactor lives in the sibling `crate::summarize::ner_deberta` module (declared
// in `summarize/mod.rs`, `#[cfg(feature = "local-ner")]`); this file holds only the model resolver,
// the active-redactor factory, and the inbound-only downloader.

/// The stable `⟪NAME_` token prefix the [`NameRedactor`] family emits (closed by the index + `⟫`).
/// Centralized so the real redactor and the fixtures/tests agree byte-for-byte. The sole runtime
/// consumer (`ner_deberta`) is feature-gated behind `local-ner`, so the default build sees it as
/// "unused" outside of `#[cfg(test)]` — allow that rather than feature-gate the constant itself.
#[allow(dead_code)]
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
/// - the `local-ner` feature is ON **and** the NER model dir is present at [`ner_model_dir`]
///   ([`ner_model_present`]) → the real [`ner_deberta::DebertaNameRedactor`] (lazy: the model loads on
///   first `redact_names`, not here, so this never blocks startup and never panics);
/// - otherwise (feature off, no model, or a construction error) → the dependency-free
///   [`NoopNameRedactor`]. Egress is then byte-identical to before this seam existed (ZERO regression).
///
/// NEVER panics and NEVER blocks. A NER miss leaks no more than the no-op (the redactor only ever
/// REMOVES content), so the worst case == today's behaviour.
pub fn active_name_redactor() -> Arc<dyn NameRedactor> {
    #[cfg(feature = "local-ner")]
    {
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

/// Provider decorator: redacts PII from inputs, restores it in outputs.
///
/// Two layers, both restored in the reply: the always-on regex scrubbers (emails/cards/phones,
/// via [`redact`]/[`restore`]) and the [`NameRedactor`] seam. The name layer defaults to
/// [`NoopNameRedactor`] (`new`), so production egress is byte-identical until a real NER model is
/// installed via [`with_name_redactor`](RedactingProvider::with_name_redactor).
pub struct RedactingProvider {
    inner: Arc<dyn SummarizerProvider>,
    names: Arc<dyn NameRedactor>,
}

impl RedactingProvider {
    /// Wrap `inner` with the regex firewall and the DEFAULT (no-op) name redactor. Name egress is
    /// unchanged — this is the production constructor.
    pub fn new(inner: Arc<dyn SummarizerProvider>) -> Self {
        Self {
            inner,
            names: Arc::new(NoopNameRedactor),
        }
    }

    /// Wrap `inner` with the regex firewall and an EXPLICIT name redactor (Phase 3b drop-in /
    /// tests). The name layer scrubs before egress and restores in the reply, alongside the regex
    /// layer.
    pub fn with_name_redactor(
        inner: Arc<dyn SummarizerProvider>,
        names: Arc<dyn NameRedactor>,
    ) -> Self {
        Self { inner, names }
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
        // Name layer (default no-op → text unchanged, `name_pairs` empty → byte-identical egress).
        let (red_transcript, mut name_pairs) = self.names.redact_names(&red_transcript);
        let red_related = red_related.map(|c| {
            let (c2, more) = self.names.redact_names(&c);
            name_pairs.extend(more);
            c2
        });
        let mut r = req.clone();
        r.transcript = red_transcript;
        r.related_context = red_related;
        let out = self.inner.summarize(&r).await?;
        // Restore both layers in the reply (disjoint token namespaces; order-independent).
        let out = restore_names(&out, &name_pairs);
        Ok(restore(&out, &map))
    }

    async fn complete(&self, system: &str, user: &str) -> Result<String> {
        // Shared map so a value redacted in either prompt restores consistently.
        let mut map = HashMap::new();
        let mut rev = HashMap::new();
        let rsys = redact_into(system, &mut map, &mut rev);
        let ruser = redact_into(user, &mut map, &mut rev);
        // Name layer on each prompt (default no-op → unchanged). A stable-token NameRedactor maps
        // the same name to the same token across both prompts, so the merged pairs restore cleanly.
        let (rsys, mut name_pairs) = self.names.redact_names(&rsys);
        let (ruser, more) = self.names.redact_names(&ruser);
        name_pairs.extend(more);
        let out = self.inner.complete(&rsys, &ruser).await?;
        let out = restore_names(&out, &name_pairs);
        Ok(restore(&out, &map))
    }
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
        let out = block_on(prov.summarize(&sample_req("Anna Kowalska briefed Bob Smith."))).unwrap();
        let sent = captured.lock().unwrap().clone();
        // The provider saw TOKENS, not names — no name leaked off-device.
        assert!(!sent.contains("Anna Kowalska"));
        assert!(!sent.contains("Bob Smith"));
        assert!(sent.contains("\u{27ea}NAME_1\u{27eb}"));
        // ...but the caller still gets the real names back.
        assert!(out.contains("Anna Kowalska") && out.contains("Bob Smith"));
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
            assert!(!ner_model_present(), "absent NER dir must report not-present");
        }
    }

    #[test]
    fn active_name_redactor_falls_back_to_noop_without_model() {
        // The graceful-degradation contract: with NO NER model present, `active_name_redactor`
        // returns a redactor that leaves text byte-identical (the no-op). With `local-ner` OFF this
        // is unconditional; with it ON it still holds whenever the model dir is absent.
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
        // The same name in both `system` and `user` restores consistently (stable-token contract).
        let prov = RedactingProvider::with_name_redactor(
            Arc::new(EchoProvider),
            Arc::new(FixtureNameRedactor),
        );
        let out = block_on(prov.complete("You assist Anna Kowalska.", "Anna Kowalska asks: status?"))
            .unwrap();
        assert!(!out.contains("NAME_"));
        assert_eq!(out.matches("Anna Kowalska").count(), 2);
    }
}
