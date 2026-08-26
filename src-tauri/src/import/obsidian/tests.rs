//! Oracles for the Obsidian vault normalizer. Synthetic folders in a temp dir — headless, no vault
//! required.

use super::*;

/// Build a temp vault from `(relative path, contents)` pairs and scan it.
fn vault(files: &[(&str, &str)]) -> ImportScan {
    let root = std::env::temp_dir().join(format!("murmur-vault-{}", uuid::Uuid::new_v4()));
    for (rel, body) in files {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, body).expect("write");
    }
    let scan = scan_vault(&root).expect("scan");
    let _ = std::fs::remove_dir_all(&root);
    scan
}

#[test]
fn reads_notes_and_keeps_the_markdown_verbatim() {
    // The vault is ALREADY our target format. Touching the body would be the bug.
    let body = "# Plan\n\nsee [[Other Note]] and ![img](assets/a.png)\n";
    let scan = vault(&[("Plan.md", body)]);
    assert_eq!(scan.pages.len(), 1);
    assert_eq!(scan.pages[0].markdown, body);
    assert_eq!(scan.pages[0].title, "Plan");
}

#[test]
fn the_identity_is_the_vault_relative_path() {
    let scan = vault(&[("Projects/Q4/Plan.md", "# Plan")]);
    assert_eq!(
        scan.pages[0].external_id.as_deref(),
        Some("Projects/Q4/Plan")
    );
    assert_eq!(scan.pages[0].parents, vec!["Projects", "Q4"]);
}

#[test]
fn vault_plumbing_is_never_imported() {
    // `.obsidian` is config, `.trash` is what the user deleted. Importing either would be wrong in
    // a different way: noise, and resurrection.
    let scan = vault(&[
        ("Real.md", "# Real"),
        (".obsidian/workspace.json", "{}"),
        (".trash/Deleted.md", "# Deleted"),
        (".hidden.md", "# Hidden"),
    ]);
    let titles: Vec<&str> = scan.pages.iter().map(|p| p.title.as_str()).collect();
    assert_eq!(titles, vec!["Real"]);
}

#[test]
fn non_markdown_files_are_counted_as_attachments_not_imported() {
    let scan = vault(&[("Note.md", "# Note"), ("assets/shot.png", "0123456789")]);
    assert_eq!(scan.pages.len(), 1);
    assert_eq!(scan.attachments, 1);
    assert_eq!(scan.attachment_bytes, 10);
}

#[test]
fn the_body_heading_wins_over_the_filename() {
    let scan = vault(&[("2026-08-25.md", "# Retro with the platform team\n")]);
    assert_eq!(scan.pages[0].title, "Retro with the platform team");
}

#[test]
fn duplicate_titles_across_folders_are_reported() {
    let scan = vault(&[("a/Notes.md", "# Notes"), ("b/Notes.md", "# Notes")]);
    assert_eq!(scan.title_collisions, vec!["Notes".to_string()]);
}

#[test]
fn a_file_instead_of_a_folder_is_refused_with_guidance() {
    let path = std::env::temp_dir().join(format!("murmur-vault-{}.md", uuid::Uuid::new_v4()));
    std::fs::write(&path, "# Not a vault").expect("write");
    let err = scan_vault(&path).expect_err("must refuse a file");
    assert!(
        matches!(err, AppError::InvalidArg(ref m) if m.contains("FOLDER")),
        "the refusal should tell the user what to pick, got {err:?}"
    );
    let _ = std::fs::remove_file(&path);
}
