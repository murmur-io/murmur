//! Redaction firewall: scrub high-confidence PII (emails, card-like digit runs, phone numbers)
//! out of any text BEFORE it leaves for an LLM provider, then de-tokenize the reply so the
//! final note still reads with the real values. Wraps any provider at the make_provider seam.
//!
//! Honest scope: regex reliably catches emails / cards / phones. Personal NAMES need on-device
//! NER (not in this stack) and are therefore NOT redacted here — surfaced in the Settings copy.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use regex::Regex;

use crate::error::Result;
use crate::summarize::provider::{Availability, SummarizeRequest, SummarizerProvider};

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
