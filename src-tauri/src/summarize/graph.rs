//! Self-assembling knowledge graph: ask the provider to resolve the people and projects in a
//! meeting note, so each can get a [[Person]] / [[Project]] page in the vault with a backlink.

use crate::error::{AppError, Result};
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
    let reply = provider.complete(SYSTEM, &user).await?;
    parse(&reply)
}

fn parse(reply: &str) -> Result<GraphPayload> {
    let json = match (reply.find('{'), reply.rfind('}')) {
        (Some(s), Some(e)) if e > s => &reply[s..=e],
        _ => {
            return Err(AppError::Summarize(
                "graph: model did not return JSON".into(),
            ))
        }
    };
    let mut p: GraphPayload = serde_json::from_str(json)
        .map_err(|e| AppError::Summarize(format!("graph: invalid JSON ({e})")))?;
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
}
