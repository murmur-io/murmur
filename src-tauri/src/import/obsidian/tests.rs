//! Oracles for the Obsidian vault normalizer. Synthetic folders in a temp dir — headless, no vault
//! required.

use super::*;

/// Materialize a temp vault from `(relative path, contents)` pairs. The caller owns cleanup.
fn make_vault(files: &[(&str, &str)]) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("murmur-vault-{}", uuid::Uuid::new_v4()));
    for (rel, body) in files {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, body).expect("write");
    }
    root
}

/// Build a temp vault from `(relative path, contents)` pairs and scan it.
fn vault(files: &[(&str, &str)]) -> ImportScan {
    let root = make_vault(files);
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
fn the_identity_is_the_vault_scoped_relative_path() {
    let root = make_vault(&[("Projects/Q4/Plan.md", "# Plan")]);
    let scan = scan_vault(&root).expect("scan");
    let expected = format!("{}:Projects/Q4/Plan", vault_scope(&root));
    assert_eq!(scan.pages[0].external_id.as_deref(), Some(expected.as_str()));
    assert_eq!(scan.pages[0].parents, vec!["Projects", "Q4"]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn two_vaults_sharing_a_relative_path_get_different_identities() {
    // CONTENT-LOSS ORACLE at the normalizer level. Keyed on the relative path ALONE, importing a
    // second vault that also holds `Area/Note.md` overwrote the first one's note.
    let a = make_vault(&[("Area/Note.md", "# Note\n\nvault A\n")]);
    let b = make_vault(&[("Area/Note.md", "# Note\n\nvault B\n")]);
    let id_a = scan_vault(&a).expect("scan a").pages[0]
        .external_id
        .clone()
        .expect("id a");
    let id_b = scan_vault(&b).expect("scan b").pages[0]
        .external_id
        .clone()
        .expect("id b");
    assert_ne!(id_a, id_b, "two vaults are two identities");
    assert!(
        id_a.ends_with(":Area/Note") && id_b.ends_with(":Area/Note"),
        "the relative path is still the readable half, got {id_a} / {id_b}"
    );
    let _ = std::fs::remove_dir_all(&a);
    let _ = std::fs::remove_dir_all(&b);
}

#[test]
fn the_same_vault_named_two_ways_keeps_one_identity() {
    // CONTROL. Scoping must not split a re-import of the SAME vault into a second identity just
    // because the picker handed us a trailing slash or a symlinked path.
    let root = make_vault(&[("Area/Note.md", "# Note")]);
    let slashed = std::path::PathBuf::from(format!("{}/", root.to_str().expect("path")));
    let link = std::env::temp_dir().join(format!("murmur-vault-link-{}", uuid::Uuid::new_v4()));
    std::os::unix::fs::symlink(&root, &link).expect("symlink");

    let plain_id = scan_vault(&root).expect("plain").pages[0].external_id.clone();
    let slashed_id = scan_vault(&slashed).expect("slashed").pages[0]
        .external_id
        .clone();
    let link_id = scan_vault(&link).expect("link").pages[0].external_id.clone();

    assert_eq!(plain_id, slashed_id, "a trailing slash is the same vault");
    assert_eq!(plain_id, link_id, "a symlink to it is the same vault");
    let _ = std::fs::remove_file(&link);
    let _ = std::fs::remove_dir_all(&root);
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
