//! Oracles for the Notion normalizer. Everything here is synthetic and in-memory, so the whole
//! file runs under `cargo test --lib` on any machine — no real Mac, no export ZIP, no network.

use std::collections::HashMap;
use std::io::Write;

use super::*;

// ── id stripping ──────────────────────────────────────────────────────────────

#[test]
fn strips_a_trailing_notion_id_and_keeps_the_title() {
    let (title, id) = strip_notion_id("Q4 Planning abc123def4567890abcdef1234567890");
    assert_eq!(title, "Q4 Planning");
    assert_eq!(id.as_deref(), Some("abc123def4567890abcdef1234567890"));
}

#[test]
fn strips_the_dashed_uuid_form_too() {
    // Notion also emits the hyphenated UUID shape; hyphens are removed before matching.
    let (title, id) = strip_notion_id("Roadmap abc123de-f456-7890-abcd-ef1234567890");
    assert_eq!(title, "Roadmap");
    assert_eq!(id.as_deref(), Some("abc123def4567890abcdef1234567890"));
}

#[test]
fn leaves_an_ordinary_filename_alone() {
    let (title, id) = strip_notion_id("Meeting notes");
    assert_eq!(title, "Meeting notes");
    assert_eq!(id, None);
}

#[test]
fn a_long_digitless_phrase_is_not_mistaken_for_an_id() {
    // 32 lowercase letters with no digit — without the digit requirement this would be eaten as an
    // id and the title would be destroyed.
    let stem = "abcdefghijklmnopqrstuvwxyzabcdef";
    assert_eq!(stem.len(), 32);
    let (title, id) = strip_notion_id(stem);
    assert_eq!(title, stem);
    assert_eq!(id, None);
}

#[test]
fn a_multibyte_tail_is_not_an_id() {
    // Guards the char-boundary split: a name ending in non-ASCII must never panic or match.
    let (title, id) = strip_notion_id("Spotkanie zespołu — podsumowanie miesiąca żółć");
    assert!(title.starts_with("Spotkanie"));
    assert_eq!(id, None);
}

// ── titles ────────────────────────────────────────────────────────────────────

#[test]
fn the_body_heading_beats_the_truncated_filename() {
    // Notion chops filenames mid-word; the H1 in the body is the full title. This is the whole
    // reason we read the body at all.
    let scan = scan_entries(vec![(
        "Quarterly plan for the platfo abc123def4567890abcdef1234567890.md",
        "# Quarterly plan for the platform team\n\nBody.",
    )]);
    assert_eq!(scan.pages.len(), 1);
    assert_eq!(scan.pages[0].title, "Quarterly plan for the platform team");
}

#[test]
fn a_page_with_no_heading_falls_back_to_the_stripped_stem() {
    let scan = scan_entries(vec![(
        "Standup abc123def4567890abcdef1234567890.md",
        "no heading here",
    )]);
    assert_eq!(scan.pages[0].title, "Standup");
}

#[test]
fn an_empty_title_becomes_the_untitled_sentinel() {
    // Coupled to the same sentinel the note picker and audit guards use, so they cannot drift.
    let scan = scan_entries(vec![("abc123def4567890abcdef1234567890.md", "")]);
    assert_eq!(scan.pages[0].title, crate::storage::db::UNTITLED_TITLE);
}

// ── link rewriting ────────────────────────────────────────────────────────────

fn one_title(id: &str, title: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    m.insert(id.to_string(), title.to_string());
    m
}

#[test]
fn rewrites_a_percent_encoded_relative_link_to_a_wikilink() {
    let map = one_title("abc123def4567890abcdef1234567890", "Other Page");
    let out = rewrite_notion_links(
        "See [the other one](Other%20Page%20abc123def4567890abcdef1234567890.md) today.",
        &map,
    );
    assert_eq!(out, "See [[Other Page|the other one]] today.");
}

#[test]
fn collapses_the_alias_when_the_label_already_is_the_title() {
    let map = one_title("abc123def4567890abcdef1234567890", "Other Page");
    let out = rewrite_notion_links(
        "[Other Page](Other%20Page%20abc123def4567890abcdef1234567890.md)",
        &map,
    );
    assert_eq!(out, "[[Other Page]]");
}

#[test]
fn preserves_a_section_anchor() {
    let map = one_title("abc123def4567890abcdef1234567890", "Other Page");
    let out = rewrite_notion_links(
        "[jump](Other%20Page%20abc123def4567890abcdef1234567890.md#section-2)",
        &map,
    );
    assert_eq!(out, "[[Other Page#section-2|jump]]");
}

#[test]
fn an_unresolved_target_is_left_exactly_as_it_was() {
    // A page outside this export must NOT become a wikilink — that would invent a broken edge in
    // the graph. A dangling relative link is the honest outcome.
    let out = rewrite_notion_links(
        "[gone](Missing%20Page%20ffffffffffffffffffffffffffffff11.md)",
        &HashMap::new(),
    );
    assert_eq!(out, "[gone](Missing%20Page%20ffffffffffffffffffffffffffffff11.md)");
}

#[test]
fn external_urls_and_images_are_untouched() {
    let map = one_title("abc123def4567890abcdef1234567890", "Other Page");
    let src = "[site](https://example.com) and ![shot](Other%20Page%20abc123def4567890abcdef1234567890.md)";
    assert_eq!(rewrite_notion_links(src, &map), src);
}

#[test]
fn a_label_containing_brackets_survives() {
    // The reason this is a hand-written parser and not a regex.
    let map = one_title("abc123def4567890abcdef1234567890", "Other Page");
    let out = rewrite_notion_links(
        "[see [1] here](Other%20Page%20abc123def4567890abcdef1234567890.md)",
        &map,
    );
    assert_eq!(out, "[[Other Page|see [1] here]]");
}

#[test]
fn an_existing_wikilink_is_not_touched() {
    let map = one_title("abc123def4567890abcdef1234567890", "Other Page");
    assert_eq!(
        rewrite_notion_links("already [[Other Page]] linked", &map),
        "already [[Other Page]] linked"
    );
}

// ── directory scans ───────────────────────────────────────────────────────────

/// Build a temp export directory from `(relative path, contents)` pairs and scan it.
fn scan_entries(files: Vec<(&str, &str)>) -> ImportScan {
    let root = std::env::temp_dir().join(format!("murmur-notion-{}", uuid::Uuid::new_v4()));
    for (rel, body) in &files {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(&path, body).expect("write");
    }
    let scan = scan_export(&root).expect("scan");
    let _ = std::fs::remove_dir_all(&root);
    scan
}

#[test]
fn the_directory_tree_becomes_the_page_path() {
    let scan = scan_entries(vec![(
        "Workspace abc123def4567890abcdef1234567891/Team abc123def4567890abcdef1234567892/Note abc123def4567890abcdef1234567893.md",
        "# Note",
    )]);
    assert_eq!(scan.pages.len(), 1);
    assert_eq!(scan.pages[0].parents, vec!["Workspace", "Team"]);
}

#[test]
fn duplicate_titles_are_reported_as_collisions() {
    // 865 of these in one real export — the caller must disambiguate, so the plan must say so.
    let scan = scan_entries(vec![
        ("a/Notes abc123def4567890abcdef1234567891.md", "# Notes"),
        ("b/Notes abc123def4567890abcdef1234567892.md", "# Notes"),
    ]);
    assert_eq!(scan.pages.len(), 2);
    assert_eq!(scan.title_collisions, vec!["Notes".to_string()]);
}

#[test]
fn csv_all_twins_are_counted_separately_from_real_databases() {
    let scan = scan_entries(vec![
        ("Tasks abc123def4567890abcdef1234567891.csv", "a,b"),
        ("Tasks abc123def4567890abcdef1234567891_all.csv", "a,b"),
        ("Page abc123def4567890abcdef1234567892.md", "# Page"),
    ]);
    assert_eq!(scan.databases, 1);
    assert_eq!(scan.csv_all_duplicates, 1);
    assert_eq!(scan.pages.len(), 1);
}

#[test]
fn attachments_are_counted_and_weighed_but_never_imported() {
    let scan = scan_entries(vec![
        ("Page abc123def4567890abcdef1234567890.md", "# Page"),
        ("Page abc123def4567890abcdef1234567890/shot.png", "0123456789"),
    ]);
    assert_eq!(scan.pages.len(), 1);
    assert_eq!(scan.attachments, 1);
    assert_eq!(scan.attachment_bytes, 10);
}

// ── archive scans ─────────────────────────────────────────────────────────────

/// Build an in-memory zip from `(name, bytes)` pairs.
fn zip_of(entries: Vec<(&str, Vec<u8>)>) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, bytes) in entries {
            w.start_file(name, opts).expect("start");
            w.write_all(&bytes).expect("write");
        }
        w.finish().expect("finish");
    }
    buf
}

/// Write `bytes` to a temp `.zip` and scan it.
fn scan_zip(bytes: Vec<u8>) -> Result<ImportScan> {
    let path = std::env::temp_dir().join(format!("murmur-notion-{}.zip", uuid::Uuid::new_v4()));
    std::fs::write(&path, bytes).expect("write zip");
    let out = scan_export(&path);
    let _ = std::fs::remove_file(&path);
    out
}

#[test]
fn scans_a_plain_export_archive() {
    let zip = zip_of(vec![(
        "Export/Page abc123def4567890abcdef1234567890.md",
        b"# Page\n".to_vec(),
    )]);
    let scan = scan_zip(zip).expect("scan");
    assert_eq!(scan.pages.len(), 1);
    assert_eq!(scan.pages[0].title, "Page");
}

#[test]
fn descends_into_nested_part_archives() {
    // The multi-part export Obsidian still tells users to unzip by hand. We do it for them.
    let inner = zip_of(vec![(
        "Inner abc123def4567890abcdef1234567890.md",
        b"# Inner\n".to_vec(),
    )]);
    let outer = zip_of(vec![("Export-abc-Part-1.zip", inner)]);
    let scan = scan_zip(outer).expect("scan");
    assert_eq!(scan.nested_archives, 1);
    assert_eq!(scan.pages.len(), 1);
    assert_eq!(scan.pages[0].title, "Inner");
}

#[test]
fn a_traversal_entry_name_is_dropped() {
    // Nothing is ever written to disk, but the hostile entry must not even reach the page list.
    let zip = zip_of(vec![
        ("../../etc/passwd.md", b"# pwned\n".to_vec()),
        ("Ok abc123def4567890abcdef1234567890.md", b"# Ok\n".to_vec()),
    ]);
    let scan = scan_zip(zip).expect("scan");
    let titles: Vec<&str> = scan.pages.iter().map(|p| p.title.as_str()).collect();
    assert_eq!(titles, vec!["Ok"]);
}

#[test]
fn a_decompression_bomb_fails_closed() {
    // One highly compressible entry larger than the shared ceiling. The guard must refuse BEFORE
    // materializing it — the bounded `take` is what makes that true.
    let huge = vec![b'a'; (MAX_EXPORT_DECOMPRESSED_BYTES + 1024) as usize];
    let zip = zip_of(vec![("Bomb abc123def4567890abcdef1234567890.md", huge)]);
    let err = scan_zip(zip).expect_err("must refuse");
    assert!(
        matches!(err, AppError::InvalidArg(ref m) if m.contains(crate::errcode::DOC_TOO_LARGE)),
        "expected a doc-too-large refusal, got {err:?}"
    );
}

#[test]
fn titles_by_id_maps_only_pages_that_carried_an_id() {
    let pages = vec![
        ImportedPage {
            external_id: Some("abc123def4567890abcdef1234567890".into()),
            title: "With id".into(),
            parents: vec![],
            markdown: String::new(),
        },
        ImportedPage {
            external_id: None,
            title: "No id".into(),
            parents: vec![],
            markdown: String::new(),
        },
    ];
    let map = titles_by_id(&pages);
    assert_eq!(map.len(), 1);
    assert_eq!(
        map.get("abc123def4567890abcdef1234567890").map(String::as_str),
        Some("With id")
    );
}
