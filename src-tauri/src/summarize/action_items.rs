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
            let head = normalize_owner(head);
            if !head.is_empty() && head.chars().count() <= 40 {
                return Some(head);
            }
        }
    }
    None
}

/// Strip the SCAFFOLDING a diarization tag or a wikilink leaves around an owner name.
///
/// The measured symptom: `get_open_commitments(owner="Miles")` returned 4 of the 7 items Miles
/// actually owns — 57% recall — because the filter is exact equality on the raw string, so
/// `"Miles (others-9)"` and `"others-10 -> Miles"` never matched `"Miles"`.
///
/// The residue is the diarization CLUSTER TAG leaking through the note prompt: the attribution
/// directive tells the model to "keep the tag label" when it cannot name a speaker, and
/// [`extract_owner`] then took the head before the em-dash verbatim.
///
/// DELIBERATELY CONSERVATIVE — this normalizes SCAFFOLDING ONLY, never identity:
/// - `(others-N)` and `(me)` are stripped because they are machine tags, but ANY OTHER
///   parenthetical is KEPT. `me (Ali)` and `me (YuYakob)` are different people in org-shared
///   notes, and collapsing them to `me` would merge three people's commitments into one rollup —
///   the opposite of the bug being fixed here.
/// - matching stays case-insensitive EQUALITY on the normalized form; no fuzzy/prefix matching,
///   which would start merging distinct people named similarly.
pub(crate) fn normalize_owner(raw: &str) -> String {
    let mut s = raw.trim();

    // `others-10 -> Miles` / `others-10: Miles` — the cluster tag prefixes the real name.
    for sep in ["->", "→", ":"] {
        if let Some((head, tail)) = s.split_once(sep) {
            if is_cluster_tag(head.trim()) {
                s = tail.trim();
            }
        }
    }

    // `[[Miles]]` / `[[Miles|Miles Davis]]` — an Obsidian wikilink around the owner.
    if let Some(inner) = s.strip_prefix("[[").and_then(|r| r.strip_suffix("]]")) {
        s = inner.split('|').next().unwrap_or(inner).trim();
    }

    // Trailing machine tag: `Miles (others-9)`, `Miles (me)`.
    if let Some(open) = s.rfind('(') {
        if s.trim_end().ends_with(')') {
            let inside = s[open + 1..s.trim_end().len() - 1].trim();
            if is_cluster_tag(inside) || inside.eq_ignore_ascii_case("me") {
                s = s[..open].trim();
            }
        }
    }

    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether `s` is a raw diarization lane tag (`others`, `others-3`, `me`) rather than a person.
fn is_cluster_tag(s: &str) -> bool {
    let s = s.trim().to_lowercase();
    if s == "me" || s == "others" {
        return true;
    }
    s.strip_prefix("others-")
        .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
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

    /// R5/#16 (regression). The three owner spellings a real vault produced for ONE person must
    /// all normalize to the same key, so an owner filter stops losing 43% of his items.
    ///
    /// RED against the previous behavior: `extract_owner` returned the head before the em-dash
    /// verbatim, and the filter was exact equality, so only the bare `Miles` form ever matched.
    #[test]
    fn the_diarization_residue_around_an_owner_is_normalized_away() {
        // Parameterized over unrelated names (incl. a two-word one and a non-ASCII one) so the
        // normalization is demonstrably STRUCTURAL — it strips machine scaffolding, and nothing in
        // it is tied to any particular person.
        for name in ["Miles", "Anna Kowalska", "Łukasz", "O'Brien"] {
            for raw in [
                name.to_string(),
                format!("{name} (others-9)"),
                format!("others-10 -> {name}"),
                format!("others-10: {name}"),
                format!("[[{name}]]"),
                format!("[[{name}|{name} Jr]]"),
                format!("{name}  (me)"),
                format!("  {name}  "),
            ] {
                assert_eq!(
                    normalize_owner(&raw),
                    name,
                    "{raw:?} must normalize to the bare owner name"
                );
            }
        }
    }

    /// The normalization must stay SCAFFOLDING-ONLY. `me (Ali)` and `me (YuYakob)` are DIFFERENT
    /// people in org-shared notes; collapsing either to `me` would merge several people's
    /// commitments into one "what did I promise" rollup — the opposite of the bug being fixed.
    #[test]
    fn normalize_owner_never_collapses_distinct_identities() {
        assert_eq!(normalize_owner("me (Ali)"), "me (Ali)");
        assert_eq!(normalize_owner("me (YuYakob)"), "me (YuYakob)");
        assert_ne!(normalize_owner("me (Ali)"), normalize_owner("me (YuYakob)"));
        // A real name in parentheses is identity, not scaffolding.
        assert_eq!(normalize_owner("Anna (QA)"), "Anna (QA)");
    }

    /// The owner survives the round trip through the real parser, not just the helper.
    #[test]
    fn extract_owner_strips_the_cluster_tag_end_to_end() {
        let md = "## Action items\n\
                  - [ ] Miles — ship the deck\n\
                  - [ ] Miles (others-9) — review the PRD\n\
                  - [ ] others-10 -> Miles — file the ticket\n";
        let owners: Vec<_> = parse_action_items(md)
            .into_iter()
            .filter_map(|i| i.owner)
            .collect();
        assert_eq!(
            owners,
            vec!["Miles", "Miles", "Miles"],
            "all three spellings resolve to one owner key"
        );
    }

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
