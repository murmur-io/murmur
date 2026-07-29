//! Deterministic, read-time extraction of historical decision/risk context from visible meeting
//! notes. This module owns no persistence and performs no inference: it only recognizes explicit
//! Markdown sections and preserves their list-item text verbatim.

use std::collections::HashSet;

use crate::error::Result;
use crate::storage::Db;

pub(crate) const NOTE_CONTEXT_MEETING_LIMIT: usize = 100;
pub(crate) const NOTE_CONTEXT_ENTRY_LIMIT: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NoteSectionKind {
    Decision,
    RiskOrOpenQuestion,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NoteSectionEntry {
    pub kind: NoteSectionKind,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HistoricalNoteEntry {
    pub kind: NoteSectionKind,
    pub text: String,
    pub meeting_id: String,
    pub meeting_title: String,
    pub started_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct HistoricalNoteContext {
    pub entries: Vec<HistoricalNoteEntry>,
    pub meetings_scanned: usize,
    pub meetings_truncated: bool,
    pub entries_truncated: bool,
}

/// Extract recognized list items from explicit decision/risk sections. This is intentionally a
/// small Markdown recognizer rather than a prose classifier: only ATX headings open a section, any
/// later ATX heading closes it, and fenced code is opaque. That keeps arbitrary note prose from
/// being promoted into decision-like historical context.
#[cfg(test)]
pub(crate) fn extract_note_sections(markdown: &str) -> Vec<NoteSectionEntry> {
    extract_note_sections_limited(markdown, usize::MAX)
}

/// Read the newest bounded set of VISIBLE meetings mentioning an already-resolved entity and parse
/// their CURRENT visible note Markdown. Both readers are existing lock gates. There is no cache or
/// write path, so edits/resummarization are reflected on the next call and relock immediately hides
/// the note again.
pub(crate) fn visible_entity_note_context(
    db: &Db,
    entity_id: &str,
    unlocked: &HashSet<String>,
) -> Result<HistoricalNoteContext> {
    // GATE 1: sealed-and-not-session-unlocked meetings are absent, including id/title/count.
    let meetings = db.entity_mentions_visible_limited(
        entity_id,
        unlocked,
        NOTE_CONTEXT_MEETING_LIMIT.saturating_add(1),
    )?;
    let meetings_scanned = meetings.len().min(NOTE_CONTEXT_MEETING_LIMIT);
    let meetings_truncated = meetings.len() > NOTE_CONTEXT_MEETING_LIMIT;
    let mut entries = Vec::new();
    let mut entries_truncated = false;

    for meeting in meetings.into_iter().take(NOTE_CONTEXT_MEETING_LIMIT) {
        if entries_truncated {
            // The bounded source row still belongs to the scanned window, but once the output cap
            // is proven exceeded there is no reason to read more note bodies.
            continue;
        }
        // GATE 2: re-read the current note only while it remains visible. A relocked/stale mention
        // therefore contributes nothing, even if it passed the first query.
        let Some(note) = db.get_note_if_visible(&meeting.meeting_id, unlocked)? else {
            continue;
        };
        let remaining = NOTE_CONTEXT_ENTRY_LIMIT.saturating_sub(entries.len());
        // Read one beyond the remaining output budget so truncation is explicit, never silent.
        let extracted = extract_note_sections_limited(&note.markdown, remaining.saturating_add(1));
        if extracted.len() > remaining {
            entries_truncated = true;
        }
        let title = if meeting.title.trim().is_empty() {
            "(untitled)".to_string()
        } else {
            meeting.title
        };
        entries.extend(
            extracted
                .into_iter()
                .take(remaining)
                .map(|entry| HistoricalNoteEntry {
                    kind: entry.kind,
                    text: entry.text,
                    meeting_id: meeting.meeting_id.clone(),
                    meeting_title: title.clone(),
                    started_at: meeting.started_at.clone(),
                }),
        );
    }

    Ok(HistoricalNoteContext {
        entries,
        meetings_scanned,
        meetings_truncated,
        entries_truncated,
    })
}

fn extract_note_sections_limited(markdown: &str, limit: usize) -> Vec<NoteSectionEntry> {
    let mut entries = Vec::new();
    let mut section = None;
    let mut fence: Option<(char, usize)> = None;

    for line in markdown.lines() {
        if let Some((marker, run_len)) = fence_marker(line) {
            match fence {
                Some((open_marker, open_len))
                    if marker == open_marker
                        && run_len >= open_len
                        && fence_tail(line, run_len).trim().is_empty() =>
                {
                    fence = None;
                }
                None => fence = Some((marker, run_len)),
                Some(_) => {}
            }
            continue;
        }
        if fence.is_some() {
            continue;
        }
        if let Some(heading) = atx_heading(line) {
            section = section_kind(heading);
            continue;
        }
        let Some(kind) = section else {
            continue;
        };
        let Some(item) = list_item(line) else {
            continue;
        };
        if is_checklist(item) || is_placeholder(item) {
            continue;
        }
        entries.push(NoteSectionEntry {
            kind,
            text: item.trim().to_string(),
        });
        if entries.len() >= limit {
            break;
        }
    }
    entries
}

/// A CommonMark-style fenced block marker (up to three leading spaces, 3+ backticks or tildes).
fn fence_marker(line: &str) -> Option<(char, usize)> {
    let bytes = line.as_bytes();
    let indent = bytes.iter().take_while(|byte| **byte == b' ').count();
    if indent > 3 || indent >= bytes.len() {
        return None;
    }
    let marker = bytes[indent] as char;
    if marker != '`' && marker != '~' {
        return None;
    }
    let run_len = bytes[indent..]
        .iter()
        .take_while(|byte| **byte == marker as u8)
        .count();
    (run_len >= 3).then_some((marker, run_len))
}

fn fence_tail(line: &str, run_len: usize) -> &str {
    let indent = line.as_bytes().iter().take_while(|b| **b == b' ').count();
    &line[indent + run_len..]
}

/// Parse an ATX heading and strip an optional closing `#` run. Setext headings are deliberately not
/// accepted: requiring an explicit ATX boundary keeps the recognizer deterministic and local.
fn atx_heading(line: &str) -> Option<&str> {
    let bytes = line.as_bytes();
    let indent = bytes.iter().take_while(|byte| **byte == b' ').count();
    if indent > 3 || indent >= bytes.len() {
        return None;
    }
    let hashes = bytes[indent..]
        .iter()
        .take_while(|byte| **byte == b'#')
        .count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let after = indent + hashes;
    if after < bytes.len() && !bytes[after].is_ascii_whitespace() {
        return None;
    }
    let heading = line[after..].trim();
    let without_hashes = heading.trim_end_matches('#');
    if without_hashes.len() != heading.len()
        && without_hashes
            .chars()
            .last()
            .map(char::is_whitespace)
            .unwrap_or(false)
    {
        Some(without_hashes.trim_end())
    } else {
        Some(heading)
    }
}

fn section_kind(heading: &str) -> Option<NoteSectionKind> {
    let normalized = normalize_heading(heading);
    match normalized.as_str() {
        "decision" | "decisions" | "decyzja" | "decyzje" => Some(NoteSectionKind::Decision),
        "risk"
        | "risks"
        | "ryzyko"
        | "ryzyka"
        | "open question"
        | "open questions"
        | "otwarte pytanie"
        | "otwarte pytania"
        | "risks and open questions"
        | "risks or open questions"
        | "risks and or open questions"
        | "ryzyka i otwarte pytania"
        | "ryzyka oraz otwarte pytania"
        | "ryzyka lub otwarte pytania"
        | "ryzyka i lub otwarte pytania"
        | "ryzyka i or otwarte pytania"
        | "ryzyka i or lub otwarte pytania"
        | "ryzyka and otwarte pytania"
        | "ryzyka or otwarte pytania" => Some(NoteSectionKind::RiskOrOpenQuestion),
        _ => None,
    }
}

/// Drop optional emoji/punctuation while preserving Unicode letters. `/` and `&` are normalized to
/// connector words so `Risks/Open Questions`, `Risks & Open Questions`, and `and/or` forms match.
fn normalize_heading(heading: &str) -> String {
    let mut normalized = String::new();
    for character in heading.chars() {
        if character.is_alphanumeric() {
            normalized.extend(character.to_lowercase());
        } else if character == '&' {
            normalized.push_str(" and ");
        } else if character == '/' {
            normalized.push_str(" or ");
        } else {
            normalized.push(' ');
        }
    }
    let mut words = Vec::new();
    for word in normalized.split_whitespace() {
        if words.last().copied() != Some(word) {
            words.push(word);
        }
    }
    words.join(" ")
}

fn list_item(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    let mut chars = trimmed.char_indices();
    let (_, first) = chars.next()?;
    if matches!(first, '-' | '*' | '+' | '•') {
        let after = &trimmed[first.len_utf8()..];
        return (after.is_empty()
            || after
                .chars()
                .next()
                .map(char::is_whitespace)
                .unwrap_or(false))
        .then(|| after.trim_start());
    }

    let digit_bytes = trimmed
        .as_bytes()
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    if digit_bytes == 0 || digit_bytes >= trimmed.len() {
        return None;
    }
    let marker = trimmed.as_bytes()[digit_bytes];
    if marker != b'.' && marker != b')' {
        return None;
    }
    let after = &trimmed[digit_bytes + 1..];
    (after
        .chars()
        .next()
        .map(char::is_whitespace)
        .unwrap_or(false))
    .then(|| after.trim_start())
}

fn is_checklist(item: &str) -> bool {
    let item = item.trim_start();
    let Some(after_open) = item.strip_prefix('[') else {
        return false;
    };
    let Some((marker, _)) = after_open.split_once(']') else {
        return false;
    };
    matches!(marker.trim(), "" | "x" | "X" | "-" | "~" | "✓" | "✔")
}

fn is_placeholder(item: &str) -> bool {
    let mut normalized = item.trim();
    if normalized.is_empty()
        || normalized.starts_with("<!--")
        || (normalized.starts_with("{{") && normalized.ends_with("}}"))
    {
        return true;
    }
    normalized = normalized.trim_matches(|c: char| {
        matches!(
            c,
            '*' | '_' | '~' | '`' | '"' | '\'' | '(' | ')' | '[' | ']'
        )
    });
    normalized = normalized.trim();
    normalized = normalized.trim_end_matches(['.', ',', ';', ':', '!', '?']);
    let normalized = normalized.trim().to_lowercase();
    matches!(
        normalized.as_str(),
        "" | "-"
            | "—"
            | "–"
            | "…"
            | "none"
            | "none recorded"
            | "nothing recorded"
            | "not recorded"
            | "not discussed"
            | "not mentioned"
            | "no decisions"
            | "no decisions recorded"
            | "no decisions were made"
            | "no risks"
            | "no risks recorded"
            | "no open questions"
            | "n/a"
            | "na"
            | "tbd"
            | "todo"
            | "brak"
            | "brak decyzji"
            | "brak ryzyk"
            | "brak otwartych pytań"
            | "nie odnotowano"
            | "nie zapisano"
            | "nie omówiono"
            | "nie wspomniano"
            | "nie dotyczy"
            | "do ustalenia"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(entries: &[NoteSectionEntry]) -> Vec<(NoteSectionKind, &str)> {
        entries
            .iter()
            .map(|entry| (entry.kind, entry.text.as_str()))
            .collect()
    }

    #[test]
    fn parses_english_polish_emoji_and_connector_heading_variants() {
        let markdown = "\
## ✅ Decisions
- Ship the Łódź rollout.
## ⚠️ Risks & Open Questions
- Will Zürich approve?
## Decyzje 🎯
- Wdrożyć żółty wariant.
## Ryzyka i/lub otwarte pytania
- Czy zespół ma budżet?
## Risks and/or Open Questions
- Supply-chain fallback?
";
        assert_eq!(
            texts(&extract_note_sections(markdown)),
            vec![
                (NoteSectionKind::Decision, "Ship the Łódź rollout."),
                (NoteSectionKind::RiskOrOpenQuestion, "Will Zürich approve?"),
                (NoteSectionKind::Decision, "Wdrożyć żółty wariant."),
                (NoteSectionKind::RiskOrOpenQuestion, "Czy zespół ma budżet?"),
                (
                    NoteSectionKind::RiskOrOpenQuestion,
                    "Supply-chain fallback?"
                ),
            ]
        );
    }

    #[test]
    fn respects_atx_boundaries_and_ignores_backtick_and_tilde_fences() {
        let markdown = "\
```markdown
## Decisions
- fenced backtick leak
```
## Decisions
- Keep this decision.
### Rationale
- outside the decision section
~~~md
## Ryzyka
- fenced tilde leak
~~~
## Risks / Open Questions
- Keep this risk.
## Summary
- outside the risk section
";
        assert_eq!(
            texts(&extract_note_sections(markdown)),
            vec![
                (NoteSectionKind::Decision, "Keep this decision."),
                (NoteSectionKind::RiskOrOpenQuestion, "Keep this risk."),
            ]
        );
    }

    #[test]
    fn omits_empty_placeholders_and_checklists_but_preserves_unicode_items() {
        let markdown = "\
## Decisions
-
- None recorded.
- Brak decyzji.
- N/A
- [ ] This is an action, not a decision.
- [x] Completed task is still a checklist.
- Zażółć gęślą jaźń — decyzja pozostaje.
## Otwarte pytania
- TBD
- Nie odnotowano.
1. Czy właścicielką jest Łucja?
";
        assert_eq!(
            texts(&extract_note_sections(markdown)),
            vec![
                (
                    NoteSectionKind::Decision,
                    "Zażółć gęślą jaźń — decyzja pozostaje."
                ),
                (
                    NoteSectionKind::RiskOrOpenQuestion,
                    "Czy właścicielką jest Łucja?"
                ),
            ]
        );
    }
}
