//! Self-assembling knowledge graph: ask the provider to resolve the people and projects in a
//! meeting note, so each can get a [[Person]] / [[Project]] page in the vault with a backlink.

use crate::error::Result;
use crate::summarize::provider::SummarizerProvider;

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphPayload {
    #[serde(default)]
    pub people: Vec<String>,
    #[serde(default)]
    pub projects: Vec<String>,
}

const SYSTEM: &str = "You extract named entities from a meeting note. Output STRICT JSON ONLY \
(no prose, no code fences): {\"people\":[\"Full Name\"],\"projects\":[\"Project Name\"]}.\n\
- people: distinct humans actually NAMED in the note (real names only — never roles, never \
\"User 1\"/\"Speaker 2\").\n\
- projects: distinct named projects / products / initiatives discussed.\n\
Use clean Title Case names usable as note titles. Empty arrays if none. Output ONLY the JSON.";

/// Ask the provider to extract people + projects from the note. Returns sanitized entities.
pub async fn extract_entities(
    provider: &dyn SummarizerProvider,
    title: &str,
    markdown: &str,
) -> Result<GraphPayload> {
    let excerpt: String = markdown.chars().take(8000).collect();
    let user = format!("MEETING: {title}\n\nNOTE:\n{excerpt}");
    // Minimal JSON schema for entity extraction — passed to the gateway for native constrained
    // decoding; the DEFAULT `complete_json` impl only stringifies it into the system prompt.
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "people":   {"type": "array", "items": {"type": "string"}},
            "projects": {"type": "array", "items": {"type": "string"}}
        },
        "required": ["people", "projects"],
        "additionalProperties": false
    });
    let v = provider.complete_json(SYSTEM, &user, &schema).await?;
    let raw: GraphPayload = serde_json::from_value(v).map_err(|e| {
        crate::error::AppError::Summarize(format!("graph: invalid JSON shape from provider: {e}"))
    })?;
    Ok(GraphPayload {
        people: clean(raw.people),
        projects: clean(raw.projects),
    })
}

/// Used directly in unit tests to validate the free-text extraction path (the same path the
/// DEFAULT `complete_json` impl uses). Production code now calls `complete_json` which
/// subsumes this step — so this function is compiled only in test mode.
#[cfg(test)]
fn parse(reply: &str) -> Result<GraphPayload> {
    // Recover the FIRST balanced top-level JSON object via the string/escape-aware extractor in
    // `reason.rs` instead of the brittle `find('{')..=rfind('}')` slice — the old slice swept up a
    // stray `}` in trailing prose (or a second object) and then failed to parse a valid reply.
    let mut p: GraphPayload = crate::reason::parse_first_json(reply)?;
    p.people = clean(p.people);
    p.projects = clean(p.projects);
    Ok(p)
}

/// Trim, drop empties/oversized, de-dup case-insensitively.
fn clean(v: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in v {
        let s = s.trim().to_string();
        if s.is_empty() || s.chars().count() > 80 {
            continue;
        }
        if !out.iter().any(|x| x.eq_ignore_ascii_case(&s)) {
            out.push(s);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_dedups() {
        let r = r#"junk {"people":["Anna Kowalska","anna kowalska",""],"projects":["Atlas"]} trailing"#;
        let p = parse(r).unwrap();
        assert_eq!(p.people, vec!["Anna Kowalska"]); // dedup + drop empty
        assert_eq!(p.projects, vec!["Atlas"]);
    }

    #[test]
    fn errors_without_json() {
        assert!(parse("no json").is_err());
    }

    /// RED-before-GREEN: the OLD `find('{')..=rfind('}')` slice swept up the stray `}` in the
    /// trailing prose, producing `{…} note: close the }` which `serde_json::from_str` rejected as
    /// trailing garbage. `parse_first_json` stops at the first balanced object and parses cleanly.
    /// (Verified RED: reverting `parse` to the rfind slice makes this `unwrap()` panic.)
    #[test]
    fn parses_despite_trailing_prose_with_stray_brace() {
        let r = r#"{"people":["Anna Kowalska"],"projects":["Atlas"]} note: close the } bracket"#;
        let p = parse(r).unwrap();
        assert_eq!(p.people, vec!["Anna Kowalska"]);
        assert_eq!(p.projects, vec!["Atlas"]);
    }
}
