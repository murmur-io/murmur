//! Parse the `- [ ]` / `- [x]` action-item lines that every generated note already contains,
//! and patch them into Obsidian Tasks-plugin format (append `📅 YYYY-MM-DD` due dates).
//! Pure functions, no new deps — dates are scanned manually (no regex crate).

use crate::storage::models::ActionItem;

/// Parse all action-item checklist lines from a note's markdown.
///
/// DEFENSE-IN-DEPTH against the assistant-command leak: a checklist line whose text is ADDRESSED TO
/// THE ASSISTANT ("Klaudku, sprawdź pogodę") is DROPPED — it is a voice command the user gave the
/// in-meeting assistant, not a real task. The primary fix excludes these lines from the summarizer
/// input (so they never become action items), but a model could still echo one; this guarantees the
/// owner-less "(właściciel nieokreślony) — Sprawdzić pogodę" item never survives into the task list.
pub fn parse_action_items(markdown: &str) -> Vec<ActionItem> {
    let mut out = Vec::new();
    let mut idx = 0usize;
    for line in markdown.lines() {
        if let Some((done, text)) = parse_item_line(line) {
            // Drop a task that is really an assistant command (vocative wake form).
            if crate::audio::wake::is_assistant_directed(&text) {
                continue;
            }
            let owner = extract_owner(&text);
            let due_date = find_date(&text);
            out.push(ActionItem {
                idx,
                done,
                text,
                owner,
                due_date,
            });
            idx += 1;
        }
    }
    out
}

/// Rewrite action-item lines into Tasks-plugin format: append ` 📅 YYYY-MM-DD` to any item
/// that has a detectable due date and doesn't already carry the 📅 marker. Idempotent.
pub fn patch_tasks_markdown(markdown: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    for line in markdown.lines() {
        if parse_item_line(line).is_some() && !line.contains('📅') {
            if let Some(date) = find_date(line) {
                lines.push(format!("{line} 📅 {date}"));
                continue;
            }
        }
        lines.push(line.to_string());
    }
    let mut s = lines.join("\n");
    if markdown.ends_with('\n') {
        s.push('\n');
    }
    s
}

/// If `line` is a checklist item, return (done, text-after-the-checkbox).
fn parse_item_line(line: &str) -> Option<(bool, String)> {
    let t = line.trim_start();
    let after = t.strip_prefix("- [").or_else(|| t.strip_prefix("* ["))?;
    let mut chars = after.chars();
    let state = chars.next()?;
    let remainder: String = chars.collect();
    let text = remainder
        .strip_prefix("] ")
        .or_else(|| remainder.strip_prefix(']'))?;
    let done = matches!(state, 'x' | 'X');
    Some((done, text.trim().to_string()))
}

/// Owner = the short name before an em/en/hyphen dash separator ("Anna — do X").
fn extract_owner(text: &str) -> Option<String> {
    for sep in [" — ", " – ", " - "] {
        if let Some((head, _)) = text.split_once(sep) {
            let head = head.trim();
            if !head.is_empty() && head.chars().count() <= 40 {
                return Some(head.to_string());
            }
        }
    }
    None
}

/// First `YYYY-MM-DD` substring in `s`, char-boundary safe. `pub(crate)` so the action-item RECALL
/// NET (`summarize::recall_net`) treats a spoken ISO date as a deadline cue using the SAME date
/// notion the checklist due-date patcher uses — one date scanner, not two.
pub(crate) fn find_date(s: &str) -> Option<String> {
    let len = s.len();
    if len < 10 {
        return None;
    }
    for i in 0..=len - 10 {
        if let Some(w) = s.get(i..i + 10) {
            if is_iso_date(w) {
                return Some(w.to_string());
            }
        }
    }
    None
}

fn is_iso_date(w: &str) -> bool {
    let b = w.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b[0..4].iter().all(u8::is_ascii_digit)
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[8..10].iter().all(u8::is_ascii_digit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_checklist_lines() {
        let md = "## Action items\n- [ ] Anna — ship the deck (due 2026-07-01)\n- [x] Bob — done thing\nnot an item\n";
        let items = parse_action_items(md);
        assert_eq!(items.len(), 2);
        assert!(!items[0].done);
        assert_eq!(items[0].owner.as_deref(), Some("Anna"));
        assert_eq!(items[0].due_date.as_deref(), Some("2026-07-01"));
        assert!(items[1].done);
    }

    #[test]
    fn patch_appends_due_emoji_idempotently() {
        let md = "- [ ] task by 2026-07-01\n";
        let p = patch_tasks_markdown(md);
        assert!(p.contains("📅 2026-07-01"));
        // idempotent
        assert_eq!(patch_tasks_markdown(&p), p);
    }

    #[test]
    fn no_date_no_change() {
        let md = "- [ ] just a task\n";
        assert_eq!(patch_tasks_markdown(md), md);
    }

    /// The assistant-command leak: a checklist line addressed to the assistant ("Klaudku, …") must be
    /// DROPPED, while a real action item ("Janek wyśle raport w piątek") is KEPT. This is the
    /// owner-less "(właściciel nieokreślony) — Sprawdzić pogodę" item the user reported.
    #[test]
    fn drops_assistant_directed_item_keeps_real_task() {
        let md = "## Action items\n\
                  - [ ] Klaudku, sprawdź jaka była pogoda\n\
                  - [ ] Janek wyśle raport w piątek\n\
                  - [ ] klaudku, jakie masz informacje w moich notatkach\n";
        let items = parse_action_items(md);
        assert_eq!(
            items.len(),
            1,
            "only the real task survives; assistant commands are dropped"
        );
        assert_eq!(items[0].text, "Janek wyśle raport w piątek");
    }
}
