use super::*;

fn meeting_with_audio(audio_path: Option<&str>) -> Meeting {
    Meeting {
        id: "m1".to_string(),
        started_at: "2026-06-27T09:00:00Z".to_string(),
        ended_at: None,
        title: Some("Quarterly board strategy".to_string()),
        duration_s: 1800,
        audio_path: audio_path.map(|s| s.to_string()),
        status: MeetingStatus::Summarized,
        folder_id: Some("secret-folder".to_string()),
    }
}

/// The master seal stages (`seal_audio_at_rest` → `permanent_unseal_audio`) round-trip a file
/// byte-identical with verify-before-destroy: the plaintext is removed only after a verified
/// `.enc` exists. Permanent-unseal preparation durably restores plaintext but deliberately
/// retains `.enc` until the enclosing folder transaction commits `locked=0`; this closes the
/// crash window where a still-locked row could point at already-destroyed ciphertext.
#[test]
fn master_seal_stage_round_trips_byte_identical() {
    let ck = [7u8; 32];
    // Seal binds the ROLE form (mic master); unseal goes through the role→role-less ladder. A
    // mismatch would fail the AES-GCM tag check, so this exercises the real bound round-trip
    // under the stream-role hardening.
    let mic_aad = aad_audio_role("m-master", "f-master", StreamRole::Mic);
    let (mic_role, mic_less) = audio_decrypt_ladder("m-master", "f-master", StreamRole::Mic);
    let plain = std::env::temp_dir().join(format!("murmur-seal-stage-{}.bin", std::process::id()));
    let original = b"RIFF\x00\x01\x02\xfffake-master-pcm....\x10\x20".to_vec();
    std::fs::write(&plain, &original).unwrap();
    let plain_s = plain.to_string_lossy().to_string();

    let enc = seal_audio_at_rest(&ck, Some(plain_s.clone()), &mic_aad)
        .unwrap()
        .expect("a fresh plaintext path seals");
    assert!(enc.ends_with(ENC_SUFFIX));
    assert!(
        !std::path::Path::new(&plain_s).exists(),
        "plaintext removed only after a verified .enc"
    );
    assert!(std::path::Path::new(&enc).exists(), ".enc written");
    // Idempotent: an already-sealed path is a no-op (never double-encrypts).
    assert!(seal_audio_at_rest(&ck, Some(enc.clone()), &mic_aad)
        .unwrap()
        .is_none());

    // A mic master must NOT decrypt under the SYS ladder (no cross-stream swap within a meeting).
    let (sys_role, sys_less) = audio_decrypt_ladder("m-master", "f-master", StreamRole::Sys);
    assert!(
        permanent_unseal_audio(&ck, Some(enc.clone()), &[&sys_role, &sys_less]).is_err(),
        "the mic master must not unseal under the sys role ladder"
    );

    let restored = permanent_unseal_audio(&ck, Some(enc.clone()), &[&mic_role, &mic_less])
        .unwrap()
        .expect("a .enc path unseals");
    assert_eq!(restored.plaintext_path, plain_s);
    assert_eq!(restored.sealed_path, enc);
    assert!(
        std::path::Path::new(&enc).exists(),
        ".enc retained until the enclosing folder commits permanently open"
    );
    let back = std::fs::read(&restored.plaintext_path).unwrap();
    let _ = std::fs::remove_file(&restored.plaintext_path);
    let _ = std::fs::remove_file(&enc);
    assert_eq!(
        back, original,
        "master survives seal -> unseal byte-identical"
    );
}

/// A permanent unlock can start from the SESSION-unlocked shape: the DB already points at the
/// plaintext while its authenticated `.enc` sibling remains. The preparation step must return
/// both paths so the transaction persists plaintext first and retires the exact ciphertext only
/// after commit; treating the plaintext pointer as "not sealed" strands multi-GB residue.
#[test]
fn permanent_unseal_recognizes_session_plaintext_and_retained_ciphertext() {
    let ck = [17u8; 32];
    let meeting_id = "session-to-permanent";
    let folder_id = "session-folder";
    let aad = aad_audio_role(meeting_id, folder_id, StreamRole::Mic);
    let (role, role_less) = audio_decrypt_ladder(meeting_id, folder_id, StreamRole::Mic);
    let base = std::env::temp_dir().join(format!(
        "murmur-session-permanent-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let plain = base.to_string_lossy().to_string();
    let original = b"session-unlocked-audio".to_vec();
    std::fs::write(&plain, &original).unwrap();
    let enc = seal_audio_at_rest(&ck, Some(plain.clone()), &aad)
        .unwrap()
        .expect("plaintext seals");
    let session_plain = session_unseal_audio(&ck, Some(enc.clone()), &[&role, &role_less])
        .unwrap()
        .expect("session unlock restores plaintext");
    assert_eq!(session_plain, plain);
    assert!(std::path::Path::new(&plain).exists() && std::path::Path::new(&enc).exists());

    let prepared = permanent_unseal_audio(&ck, Some(session_plain), &[&role, &role_less])
        .unwrap()
        .expect("session-unlocked pair is prepared for permanent unlock");

    assert_eq!(prepared.plaintext_path, plain);
    assert_eq!(prepared.sealed_path, enc);
    assert_eq!(std::fs::read(&prepared.plaintext_path).unwrap(), original);
    assert!(std::path::Path::new(&prepared.sealed_path).exists());
    let _ = std::fs::remove_file(&prepared.plaintext_path);
    let _ = std::fs::remove_file(&prepared.sealed_path);
}

/// If the pre-existing plaintext no longer matches the retained authenticated ciphertext,
/// permanent unlock fails before either copy is retired. This is the verify-before-destroy
/// boundary for a session edit/corruption race.
#[test]
fn permanent_unseal_rejects_mismatched_session_plaintext_without_loss() {
    let ck = [23u8; 32];
    let meeting_id = "session-mismatch";
    let folder_id = "session-folder";
    let aad = aad_audio_role(meeting_id, folder_id, StreamRole::Mic);
    let (role, role_less) = audio_decrypt_ladder(meeting_id, folder_id, StreamRole::Mic);
    let base = std::env::temp_dir().join(format!(
        "murmur-session-mismatch-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let plain = base.to_string_lossy().to_string();
    let original = b"authenticated-audio".to_vec();
    std::fs::write(&plain, &original).unwrap();
    let enc = seal_audio_at_rest(&ck, Some(plain.clone()), &aad)
        .unwrap()
        .expect("plaintext seals");
    session_unseal_audio(&ck, Some(enc.clone()), &[&role, &role_less])
        .unwrap()
        .expect("session unlock restores plaintext");
    let changed = b"different-plaintext".to_vec();
    std::fs::write(&plain, &changed).unwrap();

    assert!(permanent_unseal_audio(&ck, Some(plain.clone()), &[&role, &role_less],).is_err());
    assert_eq!(std::fs::read(&plain).unwrap(), changed);
    assert!(std::path::Path::new(&enc).exists());

    let _ = std::fs::remove_file(&plain);
    let _ = std::fs::remove_file(&enc);
}

/// C4 (RED-before-GREEN): `delete_meeting` removed only the plaintext form of the primary
/// playback audio (`std::fs::remove_file(audio_path)`), but during a session-unlock the sealed
/// `.enc` coexists on disk (`session_unseal` decrypts to a plaintext WAV for playback yet KEEPS
/// the `.enc`). So record→lock→Touch-ID-unlock→delete left the `<file>.enc` orphaned. This is a
/// disk-residue leak, NOT a security leak (the plaintext IS removed). The fix routes the primary
/// audio through `remove_meeting_audio_files`, which clears both forms (matching the masters).
///
/// RED: the assertion for the `.enc` fails under the OLD single-form removal (modeled inline
/// below to prove it). GREEN: `remove_meeting_audio_files` clears both.
#[test]
fn delete_meeting_removes_both_plaintext_and_enc_playback_audio() {
    let base = std::env::temp_dir().join(format!(
        "murmur-c4-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    // The session-unlock shape: audio_path points at the PLAINTEXT WAV, and its `.enc` twin
    // (the durable sealed copy) coexists on disk.
    let plaintext = format!("{}.wav", base.to_string_lossy());
    let enc = format!("{plaintext}{ENC_SUFFIX}");
    std::fs::write(&plaintext, b"PLAYBACK-WAV").unwrap();
    std::fs::write(&enc, b"SEALED-ENC").unwrap();

    // Proof of the OLD bug: removing ONLY the plaintext form (the pre-fix behavior) leaves the
    // `.enc` orphaned — this is exactly the residue the fix eliminates.
    let _ = std::fs::remove_file(&plaintext);
    assert!(
        std::path::Path::new(&enc).exists(),
        "RED: the pre-fix single-form delete strands the sealed .enc"
    );

    // Re-create the plaintext to model the real pre-delete on-disk state, then run the FIX.
    std::fs::write(&plaintext, b"PLAYBACK-WAV").unwrap();
    remove_meeting_audio_files(Some(&plaintext));

    assert!(
        !std::path::Path::new(&plaintext).exists(),
        "the plaintext playback WAV is removed"
    );
    assert!(
        !std::path::Path::new(&enc).exists(),
        "GREEN: the sealed .enc twin is ALSO removed (no orphan)"
    );

    // Symmetric: when audio_path is recorded as the `.enc` form, both twins are cleared too.
    std::fs::write(&plaintext, b"PLAYBACK-WAV").unwrap();
    std::fs::write(&enc, b"SEALED-ENC").unwrap();
    remove_meeting_audio_files(Some(&enc));
    assert!(
        !std::path::Path::new(&enc).exists(),
        ".enc form removes the .enc"
    );
    assert!(
        !std::path::Path::new(&plaintext).exists(),
        ".enc form also removes the plaintext twin"
    );
}

/// REGRESSION (audio asset-protocol leak): `get_meeting_detail`'s masked DTO for a sealed-and-
/// not-session-unlocked meeting MUST null `audio_path`. The FE feeds `audio_path` straight into
/// `convertFileSrc` (the Tauri `asset:` protocol, scoped to the audio dir) which serves the
/// file to the webview WITHOUT going through the `export_audio` command or `meeting_is_unlocked`
/// — the one audio read path outside the command gate. Before the fix the masked DTO kept
/// `audio_path` via `..meeting`; if a PLAINTEXT WAV lived in the scoped dir (e.g. a recording
/// auto-filed / moved into an already-sealed folder, where the pipeline writes
/// `<audio>/{id}.wav` with no seal-awareness, or a crash window before re-seal) the locked
/// view would serve raw audio. Nulling the path closes the bypass regardless of on-disk state.
#[test]
fn masked_detail_nulls_audio_path_so_asset_protocol_cannot_serve_a_locked_recording() {
    // The dangerous case: a PLAINTEXT WAV still on disk in the scoped audio dir.
    let plaintext_wav = "/Users/x/Library/Application Support/MeetNotes/audio/m1.wav";
    let masked = masked_detail(meeting_with_audio(Some(plaintext_wav)));

    // The single load-bearing assertion: no path for `convertFileSrc` to serve.
    assert_eq!(
            masked.meeting.audio_path, None,
            "masked detail must NULL audio_path — the FE asset-protocol serve path bypasses the command gate"
        );
    // And the rest of the mask: title hidden, no note, no segments, locked flag set.
    assert_eq!(masked.meeting.title.as_deref(), Some("🔒 Locked"));
    assert!(masked.note.is_none(), "no note while locked");
    assert!(masked.segments.is_empty(), "no segments while locked");
    assert!(
        masked.locked,
        "locked flag set so the FE renders the unlock affordance"
    );
    // Non-content metadata is preserved so the FE can offer "unlock this folder".
    assert_eq!(masked.meeting.id, "m1");
    assert_eq!(masked.meeting.folder_id.as_deref(), Some("secret-folder"));
}

/// Even with NO audio (already `.enc`-renamed or never recorded), the masked DTO is `None` —
/// the mask is unconditional, not dependent on the on-disk seal state.
#[test]
fn masked_detail_nulls_audio_path_even_when_already_absent() {
    let masked = masked_detail(meeting_with_audio(None));
    assert_eq!(masked.meeting.audio_path, None);
    assert!(masked.locked);
}

/// Phase 5 — provenance lock-gate: a LOCKED (sealed-not-unlocked) meeting's masked DTO MUST
/// have ALL three provenance fields set to `None`. A model name / gateway host could reveal
/// which AI service processed the note content — the same sensitivity as the note text itself.
#[test]
fn masked_detail_nulls_all_provenance_fields() {
    let masked = masked_detail(meeting_with_audio(None));
    assert!(
        masked.ai_provider.is_none(),
        "masked detail must NULL ai_provider (provenance leak)"
    );
    assert!(
        masked.ai_model.is_none(),
        "masked detail must NULL ai_model (provenance leak)"
    );
    assert!(
        masked.model_served.is_none(),
        "masked detail must NULL model_served (provenance leak)"
    );
    assert!(masked.locked, "locked flag set");
}

// ── D5 vault-containment (`assert_in_vault`) ────────────────────────────────────────────────

fn tmp_vault(tag: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!(
        "murmur-vault-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

#[test]
fn assert_in_vault_accepts_legit_relative_and_nonexistent_leaf() {
    let vault = tmp_vault("ok");
    // A not-yet-existing nested target inside the vault is allowed (it's about to be created).
    let resolved = assert_in_vault(&vault, std::path::Path::new("Projects/Q3/note.md")).unwrap();
    assert!(
        resolved.starts_with(vault.canonicalize().unwrap()),
        "stays inside the vault root"
    );
    // The empty path resolves to the vault root itself.
    let root = assert_in_vault(&vault, std::path::Path::new("")).unwrap();
    assert_eq!(root, vault.canonicalize().unwrap());
    let _ = std::fs::remove_dir_all(&vault);
}

#[test]
fn assert_in_vault_rejects_parent_dir_traversal_and_absolute() {
    let vault = tmp_vault("escape");
    // `..` traversal that would climb out of the vault.
    assert!(
        assert_in_vault(&vault, std::path::Path::new("../../etc/passwd")).is_err(),
        "must reject a '..' traversal"
    );
    // A `..` even mid-path is rejected outright.
    assert!(
        assert_in_vault(&vault, std::path::Path::new("Projects/../../secret")).is_err(),
        "must reject any embedded '..'"
    );
    // An absolute path is rejected (re-anchors outside the vault).
    assert!(
        assert_in_vault(&vault, std::path::Path::new("/etc/passwd")).is_err(),
        "must reject an absolute path"
    );
    let _ = std::fs::remove_dir_all(&vault);
}

#[test]
fn assert_in_vault_rejects_symlink_escape() {
    let vault = tmp_vault("symlink");
    // A symlink INSIDE the vault that points OUTSIDE must not let a write escape.
    let outside = std::env::temp_dir().join(format!("murmur-outside-{}", std::process::id()));
    std::fs::create_dir_all(&outside).unwrap();
    let link = vault.join("escape-link");
    #[cfg(unix)]
    {
        // Best-effort: if symlink creation fails (e.g. sandbox), skip the assertion.
        if std::os::unix::fs::symlink(&outside, &link).is_ok() {
            let res = assert_in_vault(&vault, std::path::Path::new("escape-link/evil.md"));
            assert!(
                res.is_err(),
                "a symlink that points outside the vault must be rejected"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&vault);
    let _ = std::fs::remove_dir_all(&outside);
}

fn marker_cleanup_test_db() -> (Db, std::path::PathBuf) {
    const TEST_DEK: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let path = crate::storage::db::unique_temp_path("murmur-marker-outbox", "sqlite");
    (Db::open_with_key(&path, TEST_DEK).unwrap(), path)
}

fn enqueue_marker_cleanup(db: &Db, source_id: &str, exported_path: &std::path::Path, title: &str) {
    db.lock()
        .execute(
            "INSERT INTO lock_marker_export_cleanup(
                   source_kind, source_id, provider_id, exported_path, sealed_title
                 ) VALUES ('note', ?1, '', ?2, ?3)",
            rusqlite::params![source_id, exported_path.to_string_lossy(), title],
        )
        .unwrap();
}

/// The durable drainer edits only Murmur's managed block at the exact path: user text and an
/// inline wikilink survive, no external-edit sibling is created, and acknowledgement happens
/// only after the file reached the scrubbed state.
#[test]
fn marker_cleanup_outbox_scrubs_exact_file_without_sibling_or_user_edit_loss() {
    let (db, db_path) = marker_cleanup_test_db();
    let dir = std::env::temp_dir().join(format!(
        "murmur-marker-drain-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    db.set_setting("vault_path", dir.to_str().unwrap()).unwrap();
    let path = dir.join("note.md");
    let body = crate::enrich::apply_link_markers(
        "external edit stays\nuser keeps [[Secret Neighbour]] inline",
        &[crate::enrich::ContextHit {
            source: "note".into(),
            detail: "[[Secret Neighbour]]".into(),
            url: None,
        }],
    );
    std::fs::write(&path, body).unwrap();
    enqueue_marker_cleanup(&db, "source-note", &path, "Secret Neighbour");

    drain_lock_marker_export_cleanup(&db).unwrap();

    let written = std::fs::read_to_string(&path).unwrap();
    assert!(written.contains("external edit stays"));
    assert!(written.contains("user keeps [[Secret Neighbour]] inline"));
    assert!(!written.contains("> - [[Secret Neighbour]]"));
    assert!(db.pending_lock_marker_export_cleanup().unwrap().is_empty());
    assert_eq!(
        std::fs::read_dir(&dir).unwrap().count(),
        1,
        "no leak-carrying sibling or stage remains"
    );

    drop(db);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_file(&db_path);
}

/// Crash after durable file publish but before SQL acknowledgement: replay sees an already-safe
/// file, durably reasserts that state, and clears the still-pending row idempotently.
#[test]
fn marker_cleanup_outbox_replays_after_file_write_before_ack() {
    let (db, db_path) = marker_cleanup_test_db();
    let dir = std::env::temp_dir().join(format!(
        "murmur-marker-replay-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    db.set_setting("vault_path", dir.to_str().unwrap()).unwrap();
    let path = dir.join("note.md");
    let safe = "external text, managed marker already scrubbed";
    std::fs::write(&path, safe).unwrap();
    enqueue_marker_cleanup(&db, "source-note", &path, "Secret Neighbour");

    drain_lock_marker_export_cleanup(&db).unwrap();

    assert_eq!(std::fs::read_to_string(&path).unwrap(), safe);
    assert!(db.pending_lock_marker_export_cleanup().unwrap().is_empty());
    drop(db);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_file(&db_path);
}

/// A symlink is never accepted as cleanup authority. Failure retains the SQLCipher row and
/// leaves the symlink target byte-identical so startup can fail closed and retry safely.
#[cfg(unix)]
#[test]
fn marker_cleanup_outbox_rejects_symlink_and_retains_journal() {
    let (db, db_path) = marker_cleanup_test_db();
    let dir = std::env::temp_dir().join(format!(
        "murmur-marker-symlink-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    db.set_setting("vault_path", dir.to_str().unwrap()).unwrap();
    let target = dir.join("outside.md");
    let link = dir.join("note.md");
    let original = "target [[Secret Neighbour]] must stay untouched";
    std::fs::write(&target, original).unwrap();
    std::os::unix::fs::symlink(&target, &link).unwrap();
    enqueue_marker_cleanup(&db, "source-note", &link, "Secret Neighbour");

    assert!(drain_lock_marker_export_cleanup(&db).is_err());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), original);
    assert_eq!(db.pending_lock_marker_export_cleanup().unwrap().len(), 1);

    drop(db);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_file(&db_path);
}

/// Every vault-relative ancestor is opened one component at a time with O_NOFOLLOW. A symlinked
/// directory therefore cannot redirect the exact outbox capability to an outside note.
#[cfg(target_os = "macos")]
#[test]
fn marker_cleanup_outbox_rejects_symlinked_ancestor_and_preserves_outside_file() {
    let (db, db_path) = marker_cleanup_test_db();
    let root = std::env::temp_dir().join(format!(
        "murmur-marker-ancestor-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let vault_raw = root.join("vault");
    let outside_raw = root.join("outside");
    std::fs::create_dir_all(&vault_raw).unwrap();
    std::fs::create_dir_all(&outside_raw).unwrap();
    let vault = vault_raw.canonicalize().unwrap();
    let outside = outside_raw.canonicalize().unwrap();
    db.set_setting("vault_path", vault.to_str().unwrap())
        .unwrap();
    let outside_note = outside.join("note.md");
    let original = "outside [[Secret Neighbour]] must remain byte-identical";
    std::fs::write(&outside_note, original).unwrap();
    let linked_parent = vault.join("linked");
    std::os::unix::fs::symlink(&outside, &linked_parent).unwrap();
    enqueue_marker_cleanup(
        &db,
        "source-note",
        &linked_parent.join("note.md"),
        "Secret Neighbour",
    );

    assert!(drain_lock_marker_export_cleanup(&db).is_err());
    assert_eq!(std::fs::read_to_string(&outside_note).unwrap(), original);
    assert_eq!(db.pending_lock_marker_export_cleanup().unwrap().len(), 1);

    drop(db);
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_file(&db_path);
}

/// An absolute path outside the configured vault is never accepted merely because it is stored
/// in the encrypted outbox; the row remains pending and the outside file is untouched.
#[cfg(target_os = "macos")]
#[test]
fn marker_cleanup_outbox_rejects_path_outside_configured_vault() {
    let (db, db_path) = marker_cleanup_test_db();
    let root = std::env::temp_dir().join(format!(
        "murmur-marker-outside-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let vault_raw = root.join("vault");
    let outside_raw = root.join("outside.md");
    std::fs::create_dir_all(&vault_raw).unwrap();
    let vault = vault_raw.canonicalize().unwrap();
    db.set_setting("vault_path", vault.to_str().unwrap())
        .unwrap();
    let original = "outside [[Secret Neighbour]] remains";
    std::fs::write(&outside_raw, original).unwrap();
    let outside = outside_raw.canonicalize().unwrap();
    enqueue_marker_cleanup(&db, "source-note", &outside, "Secret Neighbour");

    assert!(drain_lock_marker_export_cleanup(&db).is_err());
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), original);
    assert_eq!(db.pending_lock_marker_export_cleanup().unwrap().len(), 1);

    drop(db);
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_file(&db_path);
}

// ── B7/B8 AAD context-binding regression at the helper level (defense-in-depth over crypto::) ──

#[test]
fn content_aad_distinguishes_every_context_axis() {
    // The five axes (folder, meeting, provider, record-type, schema-version) must each change
    // the AAD so a blob cannot be swapped across any of them.
    let base = aad_content("f", "m", "p", "note");
    assert_ne!(
        base,
        aad_content("F", "m", "p", "note"),
        "folder axis binds"
    );
    assert_ne!(
        base,
        aad_content("f", "M", "p", "note"),
        "meeting axis binds"
    );
    assert_ne!(
        base,
        aad_content("f", "m", "P", "note"),
        "provider axis binds"
    );
    assert_ne!(
        base,
        aad_content("f", "m", "p", "segment"),
        "record-type axis binds"
    );
    // wrapped-CK and audio AADs are distinct namespaces from content.
    assert_ne!(
        aad_wrapped_ck("f"),
        aad_content("f", "m", AAD_NO_PROVIDER, "note")
    );
    assert_ne!(
        aad_audio("m", "f"),
        aad_content("f", "m", AAD_NO_PROVIDER, "note")
    );
}

/// Stream-role hardening: each of the three per-meeting audio roles produces a DISTINCT AAD, and
/// each differs from the historical role-LESS form — so within ONE meeting a mic master can't be
/// swapped for the sys master or the playback WAV. The role-less form is retained verbatim as the
/// backward-compat decrypt rung (it must equal the v1 string an existing master was sealed with).
#[test]
fn audio_role_aad_distinguishes_each_stream_and_keeps_legacy_form() {
    let pb = aad_audio_role("m", "f", StreamRole::Playback);
    let mic = aad_audio_role("m", "f", StreamRole::Mic);
    let sys = aad_audio_role("m", "f", StreamRole::Sys);
    assert_ne!(pb, mic, "playback vs mic binds");
    assert_ne!(pb, sys, "playback vs sys binds");
    assert_ne!(mic, sys, "mic vs sys binds");

    let role_less = aad_audio("m", "f");
    assert_ne!(
        role_less, mic,
        "the role form differs from the role-less form"
    );
    // Each role form is the role-less string PLUS a |stream=… suffix → a role-less blob can never
    // match a role AAD, which is exactly why the decrypt ladder must also try the role-less rung.
    assert!(
        mic.starts_with(&role_less),
        "role AAD extends the role-less form"
    );
    // The role-less form is the EXACT v1 string existing masters carry (no drift = no data loss).
    assert_eq!(role_less, b"murmur:audio:v1|meeting=m|folder=f".to_vec());
}
