//! Write [[Person]] / [[Project]] stub notes into the vault with a backlink to each meeting,
//! so the Obsidian graph self-assembles: open a teammate's page → every meeting with them.

use std::path::Path;

use crate::error::{AppError, Result};

/// Ensure `vault/{kind}/{name}.md` exists and lists a backlink to `meeting_title` under a
/// "## Meetings" section. `kind` is e.g. "People" or "Projects". Idempotent.
pub fn ensure_entity_backlink(
    vault_dir: &Path,
    kind: &str,
    name: &str,
    meeting_title: &str,
) -> Result<()> {
    let safe = sanitize_entity(name);
    if safe.is_empty() {
        return Ok(());
    }
    let dir = vault_dir.join(kind);
    std::fs::create_dir_all(&dir)
        .map_err(|e| AppError::Export(format!("create {kind} dir failed: {e}")))?;
    let path = dir.join(format!("{safe}.md"));

    let mut content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => format!("# {name}\n\n## Meetings\n"),
        Err(e) => return Err(AppError::Export(format!("read entity note failed: {e}"))),
    };

    let backlink = format!("- [[{meeting_title}]]");
    if content.contains(&backlink) {
        return Ok(());
    }
    if !content.contains("## Meetings") {
        if !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str("\n## Meetings\n");
    }
    if !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(&backlink);
    content.push('\n');

    crate::export::overwrite_note(&path, &content)
}

/// Strip Obsidian-reserved characters so the entity name is a safe note filename.
fn sanitize_entity(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '*' | '"' | '\\' | '/' | '<' | '>' | ':' | '|' | '?' | '#' | '^' | '[' | ']' => ' ',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches('.')
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_and_backlinks_idempotently() {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "murmur-entity-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        ensure_entity_backlink(&dir, "People", "Anna Kowalska", "2026-07-01 Sync").unwrap();
        ensure_entity_backlink(&dir, "People", "Anna Kowalska", "2026-07-01 Sync").unwrap();
        let p = dir.join("People").join("Anna Kowalska.md");
        let c = std::fs::read_to_string(&p).unwrap();
        assert_eq!(c.matches("- [[2026-07-01 Sync]]").count(), 1); // no duplicate
        assert!(c.contains("# Anna Kowalska"));
    }

    #[test]
    fn rejects_unsafe_empty_name() {
        let dir = std::env::temp_dir();
        assert!(ensure_entity_backlink(&dir, "People", "///", "M").is_ok());
    }
}
