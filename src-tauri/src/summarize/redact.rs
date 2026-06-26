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
pub struct RedactingProvider {
    inner: Arc<dyn SummarizerProvider>,
}

impl RedactingProvider {
    pub fn new(inner: Arc<dyn SummarizerProvider>) -> Self {
        Self { inner }
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
        let (red, map) = redact(&req.transcript);
        let mut r = req.clone();
        r.transcript = red;
        let out = self.inner.summarize(&r).await?;
        Ok(restore(&out, &map))
    }

    async fn complete(&self, system: &str, user: &str) -> Result<String> {
        // Shared map so a value redacted in either prompt restores consistently.
        let mut map = HashMap::new();
        let mut rev = HashMap::new();
        let rsys = redact_into(system, &mut map, &mut rev);
        let ruser = redact_into(user, &mut map, &mut rev);
        let out = self.inner.complete(&rsys, &ruser).await?;
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
}
