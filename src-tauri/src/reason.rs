//! Local on-device reasoning seam (Phase 3a — PROD-INERT).
//!
//! [`LocalReasoner`] is the trait the heavy local reasoner (mistral.rs with grammar-constrained
//! decoding, Phase 3b) will implement. This increment ships ONLY the seam plus a deterministic
//! [`StubReasoner`] and a robust JSON extractor — NO ML crate, NO model download. The real impl is
//! a one-line swap at the construction site; nothing here is wired into a production path yet
//! (`graph.rs`/`timeline.rs` keep their existing brittle parse until a later increment).
//!
//! The [`LocalReasoner::structured`] method is the load-bearing seam: it is where reliable
//! tool-call / NER-classification JSON comes from. A real impl constrains decoding to the schema;
//! the stub fakes it but still routes its output through [`extract_first_json`] so the
//! recover-JSON-from-noisy-text path is exercised and testable.

use serde_json::Value;

use crate::error::{AppError, Result};

/// A local (on-device, no-egress) reasoning model. Synchronous: the real impl runs a local model
/// to completion on a worker thread; the stub is pure. All methods are deterministic for a given
/// input in the stub.
pub trait LocalReasoner: Send + Sync {
    /// Stable id of the backing model ("stub" until the real model lands).
    fn id(&self) -> &str;

    /// Free-form reasoning: run `system` + `user` and return the model's text.
    fn reason(&self, system: &str, user: &str) -> Result<String>;

    /// Structured reasoning: run `system` + `user` and return a JSON value. `json_schema` is the
    /// shape the output must conform to — a real impl constrains decoding to it; the stub ignores
    /// the constraint but still returns valid JSON recovered via [`extract_first_json`].
    fn structured(&self, system: &str, user: &str, json_schema: &Value) -> Result<Value>;
}

/// Extract the first BALANCED top-level JSON object `{...}` from `text`.
///
/// Honors braces that appear inside JSON string literals (and `\"` escapes), so it is robust where
/// the `find('{')..=rfind('}')` slice in `graph.rs`/`timeline.rs` mis-cuts — e.g. when prose after
/// the object contains a stray `}`, when the model emits two objects, or when a string value itself
/// contains braces. Returns the matched substring, or `None` if no balanced object is present.
///
/// Byte-scanning is UTF-8-safe: the structural bytes `{` `}` `"` `\` are all ASCII, and UTF-8
/// continuation bytes (>= 0x80) never collide with them, so multibyte content between the braces is
/// passed through untouched and the returned slice always lands on char boundaries.
pub fn extract_first_json(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    let start = text.find('{')?;
    let mut depth: usize = 0;
    let mut in_str = false;
    let mut escaped = false;
    let mut i = start;
    while i < bytes.len() {
        let c = bytes[i];
        if in_str {
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                in_str = false;
            }
        } else {
            match c {
                b'"' => in_str = true,
                b'{' => depth += 1,
                b'}' => {
                    // `start` is at a '{', so depth is >= 1 here; the saturating guard is defensive.
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return Some(&text[start..=i]);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

/// Recover the first balanced JSON object from a (possibly noisy / fenced) reply and deserialize
/// it into `T`. The robust counterpart to the brittle slice in `graph.rs`/`timeline.rs`.
pub fn parse_first_json<T: serde::de::DeserializeOwned>(text: &str) -> Result<T> {
    let json = extract_first_json(text)
        .ok_or_else(|| AppError::Summarize("reasoner: no JSON object in reply".to_string()))?;
    serde_json::from_str(json)
        .map_err(|e| AppError::Summarize(format!("reasoner: invalid JSON ({e})")))
}

/// Deterministic, dependency-free stand-in for the real local reasoner (Phase 3b). Produces stable
/// output for a given input so the seam + the JSON-recovery path can be unit-tested without a model.
#[derive(Debug, Default, Clone, Copy)]
pub struct StubReasoner;

impl LocalReasoner for StubReasoner {
    fn id(&self) -> &str {
        "stub"
    }

    fn reason(&self, system: &str, user: &str) -> Result<String> {
        // Deterministic echo-shape: stable for a given (system, user) so tests can assert equality.
        Ok(format!(
            "[stub-reason] system={} chars user={} chars",
            system.chars().count(),
            user.chars().count()
        ))
    }

    fn structured(&self, _system: &str, user: &str, _json_schema: &Value) -> Result<Value> {
        // Build a deterministic object, then SIMULATE a real model emitting it wrapped in prose +
        // code fences, and recover it through the robust extractor — exercising the exact
        // noisy-reply → JSON path the grammar-constrained model output will take in Phase 3b.
        let obj = serde_json::json!({
            "stub": true,
            "echo": user,
            "chars": user.chars().count(),
        });
        let noisy = format!("Sure, here is the JSON:\n```json\n{obj}\n```\nHope that helps!");
        parse_first_json(&noisy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_object_from_surrounding_prose() {
        let got = extract_first_json("blah blah {\"a\":1} trailing").unwrap();
        assert_eq!(got, "{\"a\":1}");
    }

    #[test]
    fn handles_braces_inside_strings() {
        // A naive rfind('}') would over-slice here; the string-aware scanner stops at the real end.
        let got = extract_first_json("noise {\"text\":\"a } b { c\"} more } junk").unwrap();
        assert_eq!(got, "{\"text\":\"a } b { c\"}");
    }

    #[test]
    fn handles_nested_objects() {
        let got = extract_first_json("x {\"o\":{\"i\":2}} y").unwrap();
        assert_eq!(got, "{\"o\":{\"i\":2}}");
    }

    #[test]
    fn returns_first_of_two_objects() {
        let got = extract_first_json("{\"first\":1} {\"second\":2}").unwrap();
        assert_eq!(got, "{\"first\":1}");
    }

    #[test]
    fn respects_escaped_quote_inside_string() {
        let got = extract_first_json("{\"q\":\"he said \\\"hi}\\\"\"}").unwrap();
        assert_eq!(got, "{\"q\":\"he said \\\"hi}\\\"\"}");
    }

    #[test]
    fn none_when_no_json() {
        assert!(extract_first_json("no json here").is_none());
        assert!(extract_first_json("unbalanced {\"a\":1").is_none());
    }

    #[test]
    fn parse_first_json_deserializes() {
        #[derive(serde::Deserialize)]
        struct P {
            a: i32,
        }
        let p: P = parse_first_json("prefix {\"a\":7} suffix").unwrap();
        assert_eq!(p.a, 7);
        assert!(parse_first_json::<P>("no json").is_err());
    }

    #[test]
    fn stub_reason_is_deterministic() {
        let r = StubReasoner;
        assert_eq!(r.id(), "stub");
        let a = r.reason("sys", "hello").unwrap();
        let b = r.reason("sys", "hello").unwrap();
        assert_eq!(a, b);
        // Different input → different output (the length fields move).
        assert_ne!(r.reason("sys", "hello").unwrap(), r.reason("sys", "hi").unwrap());
    }

    #[test]
    fn stub_structured_returns_valid_json_via_extractor() {
        let r = StubReasoner;
        let schema = serde_json::json!({ "type": "object" });
        let v = r.structured("sys", "find Atlas", &schema).unwrap();
        assert_eq!(v["stub"], serde_json::json!(true));
        assert_eq!(v["echo"], serde_json::json!("find Atlas"));
        assert_eq!(v["chars"], serde_json::json!("find Atlas".chars().count()));
    }

    #[test]
    fn stub_structured_survives_braces_in_user_text() {
        // The echoed user text contains braces; the extractor must still recover the whole object.
        let r = StubReasoner;
        let schema = serde_json::json!({});
        let v = r.structured("sys", "json like {a:1} please", &schema).unwrap();
        assert_eq!(v["echo"], serde_json::json!("json like {a:1} please"));
    }
}
