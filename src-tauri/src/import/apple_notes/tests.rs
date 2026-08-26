//! Oracles for the Apple Notes parser.
//!
//! Only the PURE half is testable here — `osascript` and the TCC prompt cannot run headlessly, and
//! pretending otherwise with a mock would test the mock. So the split is deliberate: everything
//! below exercises the real parser against the exact byte shape the script emits, and the
//! unverifiable part is reduced to spawning a process and reading its stdout.

use super::*;

/// Assemble the script's wire format from `(id, name, folder, html body)` records.
fn wire(records: &[(&str, &str, &str, &str)]) -> String {
    let mut out = String::new();
    for (id, name, folder, body) in records {
        out.push_str(id);
        out.push(UNIT_SEP);
        out.push_str(name);
        out.push(UNIT_SEP);
        out.push_str(folder);
        out.push(UNIT_SEP);
        out.push_str(body);
        out.push(RECORD_SEP);
    }
    out
}

#[test]
fn parses_a_note_into_a_page() {
    let scan = parse_export(&wire(&[(
        "x-coredata://ABC/ICNote/p12",
        "Groceries",
        "Shopping",
        "<div><h1>Groceries</h1><div>milk</div></div>",
    )]));
    assert_eq!(scan.pages.len(), 1);
    let page = &scan.pages[0];
    assert_eq!(page.title, "Groceries");
    assert_eq!(page.parents, vec!["Shopping"]);
    assert_eq!(
        page.external_id.as_deref(),
        Some("x-coredata://ABC/ICNote/p12")
    );
    assert!(page.markdown.contains("milk"), "body rendered to text");
    assert!(
        !page.markdown.contains("<div>"),
        "HTML tags are rendered away, got: {}",
        page.markdown
    );
}

#[test]
fn a_note_with_no_folder_lands_unfiled() {
    // A note directly under an account has no container; the script emits an empty field.
    let scan = parse_export(&wire(&[("id-1", "Loose", "", "<div>body</div>")]));
    assert!(scan.pages[0].parents.is_empty());
}

#[test]
fn a_malformed_record_is_skipped_without_losing_the_others() {
    // One odd note must not cost the user the rest of the library.
    let mut raw = wire(&[("id-1", "First", "F", "<div>one</div>")]);
    raw.push_str("garbage-with-no-separators");
    raw.push(RECORD_SEP);
    raw.push_str(&wire(&[("id-2", "Second", "F", "<div>two</div>")]));
    let scan = parse_export(&raw);
    let titles: Vec<&str> = scan.pages.iter().map(|p| p.title.as_str()).collect();
    assert_eq!(titles, vec!["First", "Second"]);
}

#[test]
fn an_entirely_empty_note_is_dropped() {
    // Notes accumulates blank taps; importing them would add empty rows for nothing.
    let scan = parse_export(&wire(&[("id-1", "", "", "")]));
    assert!(scan.pages.is_empty());
}

#[test]
fn a_body_without_a_title_still_imports_under_the_untitled_sentinel() {
    let scan = parse_export(&wire(&[("id-1", "", "", "<div>real content</div>")]));
    assert_eq!(scan.pages.len(), 1);
    assert_eq!(scan.pages[0].title, crate::storage::db::UNTITLED_TITLE);
}

#[test]
fn a_body_containing_the_separators_does_not_break_the_next_record() {
    // `splitn(4)` is what makes this true: everything after the third separator is body, however
    // many separators it contains.
    let raw = wire(&[(
        "id-1",
        "Odd",
        "F",
        "<div>before\u{1F}after</div>",
    )]);
    let scan = parse_export(&raw);
    assert_eq!(scan.pages.len(), 1);
    assert!(scan.pages[0].markdown.contains("after"));
}

#[test]
fn duplicate_titles_are_reported_like_every_other_source() {
    let scan = parse_export(&wire(&[
        ("id-1", "Notes", "A", "<div>one</div>"),
        ("id-2", "Notes", "B", "<div>two</div>"),
    ]));
    assert_eq!(scan.title_collisions, vec!["Notes".to_string()]);
}

#[test]
fn empty_output_is_an_empty_plan_not_an_error() {
    // An empty Notes library is a legitimate answer, not a failure.
    let scan = parse_export("");
    assert!(scan.pages.is_empty());
    assert!(!scan.truncated);
}
