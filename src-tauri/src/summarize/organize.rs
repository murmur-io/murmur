//! AI thematic filing: ask the provider to choose a vault subfolder for a meeting so notes
//! self-organize (e.g. "Standups", "1-1s", "Acme Project") instead of piling into one dir.

use crate::summarize::provider::SummarizerProvider;

const SYSTEM: &str = "You file meeting notes into an Obsidian vault. Given a note title + \
summary and the list of EXISTING folders, choose ONE short thematic subfolder for this \
note (e.g. \"Standups\", \"1-1s\", \"Acme Project\", \"Hiring\"). Strongly prefer reusing an \
existing folder when it fits; otherwise propose a new concise Title Case name (1-3 words). \
Reply with ONLY the folder name — no path, no slashes, no quotes, no explanation.";

/// Ask the provider for a single thematic subfolder name. Returns a sanitized folder name,
/// or `None` if the model declines / errors / returns something unusable (caller then falls
/// back to the configured subfolder).
pub async fn classify_subfolder(
    provider: &dyn SummarizerProvider,
    title: &str,
    summary_excerpt: &str,
    existing: &[String],
) -> Option<String> {
    let existing_list = if existing.is_empty() {
        "(none yet)".to_string()
    } else {
        existing.join(", ")
    };
    let user = format!(
        "EXISTING folders: {existing_list}\n\nNOTE title: {title}\n\nSUMMARY:\n{excerpt}",
        excerpt = summary_excerpt.chars().take(1200).collect::<String>(),
    );
    let reply = provider.complete(SYSTEM, &user).await.ok()?;
    sanitize_folder(&reply)
}

/// Reduce a model reply (or user input) to a single safe folder name (first non-empty line,
/// reserved path/link chars stripped). Returns `None` for empty/oversized results. Reused by the
/// folder commands to sanitize user-supplied folder names into vault-safe path segments.
pub fn sanitize_folder(reply: &str) -> Option<String> {
    let first = reply.lines().find(|l| !l.trim().is_empty())?.trim();
    let cleaned: String = first
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '#' | '[' | ']' => ' ',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    let cleaned = cleaned
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches('.')
        .trim()
        .to_string();
    if cleaned.is_empty() || cleaned.chars().count() > 64 {
        None
    } else {
        Some(cleaned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_plain_name() {
        assert_eq!(sanitize_folder("Standups").as_deref(), Some("Standups"));
    }

    #[test]
    fn strips_slashes_and_quotes() {
        assert_eq!(
            sanitize_folder("\"Acme/Project\"").as_deref(),
            Some("Acme Project")
        );
    }

    #[test]
    fn takes_first_nonempty_line() {
        assert_eq!(sanitize_folder("\n  1-1s  \nblah").as_deref(), Some("1-1s"));
    }

    #[test]
    fn rejects_empty_or_punctuation_only() {
        assert!(sanitize_folder("   ").is_none());
        assert!(sanitize_folder("###").is_none());
    }
}
