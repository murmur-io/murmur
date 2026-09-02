//! File-backed (tempfile via `open_with_key` + a FIXED test key) tests for the per-folder
//! seal/unseal lifecycle. These NEVER touch the real Keychain — both the SQLCipher DEK and
//! the lock KEK are explicit literals here. They reproduce the exact seal/unseal/remove
//! sequence the Stage-C commands run (db helpers + `crate::crypto`), so a regression in the
//! lifecycle fails here even though the command wrappers need a Tauri `State`.

use super::*;
use crate::storage::models::{AskConversationScope, Folder, Meeting, MeetingStatus, NoteRecord};

/// Fixed SQLCipher key for file-backed test DBs (NOT the Keychain DEK).
const TEST_DEK: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn temp_db_path(label: &str) -> std::path::PathBuf {
    super::unique_temp_path(&format!("meetnotes-lock-test-{label}"), "sqlite")
}

fn file_db(label: &str) -> Db {
    Db::open_with_key(&temp_db_path(label), TEST_DEK).unwrap()
}

/// #5 (0.7 security fast-follow): the durable accept-resume record round-trips (insert → get →
/// idempotent overwrite → delete → None). Proves the additive `pending_share_accepts` table +
/// its methods — the persistence that makes a post-flip accept RECOVERABLE, not stranded.
#[test]
fn pending_share_accept_round_trips_and_deletes() {
    let db = file_db("pending-accept");
    assert!(db.get_pending_share_accept("s-1").unwrap().is_none());
    let p = PendingShareAccept {
        share_id: "s-1".to_string(),
        blob_id: "blob-9".to_string(),
        target_folder_id: "f-shared".to_string(),
        sender_user_id: "u-sender".to_string(),
        sender_fingerprint: "ABCDE-FGHIJ".to_string(),
        wrapped_key: vec![1, 2, 3, 4],
        grant_sig: vec![9, 8, 7],
        rev: 2,
        key_generation: 3,
        created_at: "2026-07-04T10:00:00Z".to_string(),
    };
    db.insert_pending_share_accept(&p).unwrap();
    let got = db.get_pending_share_accept("s-1").unwrap().unwrap();
    assert_eq!(got.blob_id, "blob-9");
    assert_eq!(got.target_folder_id, "f-shared");
    assert_eq!(got.sender_user_id, "u-sender");
    assert_eq!(got.sender_fingerprint, "ABCDE-FGHIJ");
    assert_eq!(got.wrapped_key, vec![1, 2, 3, 4]);
    assert_eq!(got.grant_sig, vec![9, 8, 7]);
    assert_eq!(got.rev, 2);
    assert_eq!(got.key_generation, 3);
    // Idempotent overwrite on the same share_id (a retry that re-flips is harmless).
    db.insert_pending_share_accept(&p).unwrap();
    assert!(db.get_pending_share_accept("s-1").unwrap().is_some());
    // Drop it (commit) → gone; delete is idempotent.
    db.delete_pending_share_accept("s-1").unwrap();
    assert!(db.get_pending_share_accept("s-1").unwrap().is_none());
    db.delete_pending_share_accept("s-1").unwrap();
}

fn seed_folder(db: &Db, id: &str, name: &str) -> Folder {
    let f = Folder {
        id: id.to_string(),
        name: name.to_string(),
        path: name.to_string(),
        parent_id: None,
        locked: false,
        created_at: "2026-06-26T00:00:00Z".to_string(),
    };
    db.insert_folder(&f).unwrap();
    f
}

fn seed_note(db: &Db, meeting_id: &str, markdown: &str, folder_id: Option<&str>) {
    db.insert_meeting(&Meeting {
        id: meeting_id.to_string(),
        started_at: "2026-06-26T09:00:00Z".to_string(),
        ended_at: None,
        title: Some(format!("title-{meeting_id}")),
        duration_s: 60,
        audio_path: None,
        status: MeetingStatus::Summarized,
        folder_id: None,
    })
    .unwrap();
    db.upsert_note(&NoteRecord {
        meeting_id: meeting_id.to_string(),
        provider_id: "claude_code".to_string(),
        markdown: markdown.to_string(),
        created_at: "2026-06-26T09:05:00Z".to_string(),
        exported_path: Some(format!("/vault/{meeting_id}.md")),
        model_requested: None,
        model_served: None,
        gateway_host: None,
    })
    .unwrap();
    db.set_note_folder(meeting_id, folder_id).unwrap();
}

/// Add a second provider row (distinct markdown) to an existing meeting — models the
/// re-summarize-with-another-provider state (e.g. `ollama` then `anthropic`).
fn add_provider_note(db: &Db, meeting_id: &str, provider_id: &str, markdown: &str) {
    db.upsert_note(&NoteRecord {
        meeting_id: meeting_id.to_string(),
        provider_id: provider_id.to_string(),
        markdown: markdown.to_string(),
        created_at: "2026-06-26T09:06:00Z".to_string(),
        exported_path: Some(format!("/vault/{meeting_id}-{provider_id}.md")),
        model_requested: None,
        model_served: None,
        gateway_host: None,
    })
    .unwrap();
    // Keep the new row in the same folder as its siblings.
    let folder_id = db
        .lock()
        .query_row(
            "SELECT folder_id FROM notes WHERE meeting_id = ?1 AND folder_id IS NOT NULL LIMIT 1",
            rusqlite::params![meeting_id],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()
        .unwrap()
        .flatten();
    db.set_note_folder(meeting_id, folder_id.as_deref())
        .unwrap();
}

/// Mirror of `lock_folder`: generate CK, KEK-wrap, encrypt+verify each note (PER provider
/// row), seal. One blob per (meeting, provider) — distinct provider markdown must not collide.
fn seal_folder(db: &Db, folder_id: &str, kek: &[u8; 32]) {
    let ck = crate::crypto::random_key().unwrap();
    let wrapped = crate::crypto::encrypt(kek, &ck, b"").unwrap();
    let notes = db.notes_in_folder(folder_id).unwrap();
    let mut blobs = Vec::new();
    for n in &notes {
        let blob = crate::crypto::encrypt(&ck, n.markdown.as_bytes(), b"").unwrap();
        // Verify decryptable BEFORE blanking (the command's atomicity rule).
        assert_eq!(
            crate::crypto::decrypt(&ck, &blob, b"").unwrap(),
            n.markdown.as_bytes()
        );
        blobs.push((n.meeting_id.clone(), n.provider_id.clone(), blob));
    }
    db.set_folder_locked(folder_id, true, Some(&wrapped))
        .unwrap();
    for (mid, pid, blob) in &blobs {
        db.seal_note(mid, pid, blob).unwrap();
    }
}

/// Mirror of `unlock_folder`: KEK→unwrap CK→decrypt each blob back into ITS OWN row.
fn session_unlock(db: &Db, folder_id: &str, kek: &[u8; 32]) {
    let wrapped = db.folder_wrapped_key(folder_id).unwrap().unwrap();
    let ck_bytes = crate::crypto::decrypt(kek, &wrapped, b"").unwrap();
    let ck: [u8; 32] = ck_bytes.as_slice().try_into().unwrap();
    let notes = db.notes_in_folder(folder_id).unwrap();
    for n in &notes {
        let blob = n.content_blob.as_ref().unwrap();
        let pt = crate::crypto::decrypt(&ck, blob, b"").unwrap();
        db.restore_note_markdown(
            &n.meeting_id,
            &n.provider_id,
            &String::from_utf8(pt).unwrap(),
        )
        .unwrap();
    }
}

#[test]
fn list_open_commitments_aggregates_attaches_context_and_gates() {
    let db = file_db("commitments");
    seed_folder(&db, "f-lock", "Secret");
    // open1: one open item w/ owner+due, one DONE item, one loose open item (no owner/date).
    seed_note(
            &db,
            "open1",
            "## Action items\n- [ ] Anna — ship the deck 2026-07-01\n- [x] Bob — done thing\n- [ ] just a loose task\n",
            None,
        );
    // open2: one open item, an earlier due date (sorts first).
    seed_note(&db, "open2", "- [ ] Carol — review 2026-06-15\n", None);
    // sealed: in a folder we lock → must contribute NOTHING until session-unlocked.
    seed_note(
        &db,
        "sealed",
        "- [ ] Dave — secret task 2026-07-02\n",
        Some("f-lock"),
    );
    db.set_folder_locked("f-lock", true, None).unwrap();

    // GATED: folder locked, not session-unlocked → sealed meeting excluded; DONE item excluded.
    let open = db.list_open_commitments(&HashSet::new(), None).unwrap();
    assert!(
        open.iter().all(|c| c.meeting_id != "sealed"),
        "sealed-not-unlocked meeting leaked into the rollup (gate violation)"
    );
    assert!(
        open.iter().all(|c| !c.text.contains("done thing")),
        "checked `- [x]` item must be excluded"
    );
    assert_eq!(
        open.len(),
        3,
        "two open meetings → 3 open items (Carol, Anna, loose)"
    );

    // Sort: due dates ascending, then None last.
    assert_eq!(open[0].due_date.as_deref(), Some("2026-06-15"));
    assert_eq!(open[0].owner.as_deref(), Some("Carol"));
    assert_eq!(
        open[0].meeting_title, "title-open2",
        "meeting context attached"
    );
    assert_eq!(open[1].due_date.as_deref(), Some("2026-07-01"));
    assert_eq!(open[1].owner.as_deref(), Some("Anna"));
    assert_eq!(open[2].due_date, None, "the dateless loose task sorts last");
    assert_eq!(open[2].owner, None);

    // Session-unlock → the sealed meeting's open commitment reappears.
    let mut unlocked = HashSet::new();
    unlocked.insert("f-lock".to_string());
    let all = db.list_open_commitments(&unlocked, None).unwrap();
    assert!(
        all.iter()
            .any(|c| c.meeting_id == "sealed" && c.text.contains("secret task")),
        "unlocked folder's commitment must reappear"
    );

    // Owner filter (case-insensitive) keeps only Anna's item.
    let anna = db.list_open_commitments(&unlocked, Some("ANNA")).unwrap();
    assert_eq!(anna.len(), 1);
    assert!(anna[0].text.contains("ship the deck"));
    assert_eq!(anna[0].meeting_title, "title-open1");
}

/// Feature B saved-views CRUD: upsert → list returns it; edit updates in place; delete removes
/// it; reorder rewrites `sort_order` and the list respects the new order. Content-free metadata
/// (no gate) — the `config` blob is stored opaquely and round-trips byte-identical.
#[test]
fn saved_views_crud_round_trip() {
    let db = file_db("saved-views-crud");
    let mk = |id: &str, name: &str, order: i64| SavedView {
        id: id.to_string(),
        scope: "meetings".to_string(),
        name: name.to_string(),
        layout: "list".to_string(),
        config:
            r#"{"filters":[{"field":"status","op":"eq","value":"summarized"}],"sort":"started_at"}"#
                .to_string(),
        sort_order: order,
        created_at: "2026-07-14T09:00:00Z".to_string(),
        updated_at: "2026-07-14T09:00:00Z".to_string(),
    };

    // Empty to start; a different scope never leaks in.
    assert!(db.list_saved_views("meetings").unwrap().is_empty());

    // Upsert two → list returns both in sort_order.
    db.upsert_saved_view(&mk("v-a", "Recent", 0)).unwrap();
    db.upsert_saved_view(&mk("v-b", "Untitled", 1)).unwrap();
    let views = db.list_saved_views("meetings").unwrap();
    assert_eq!(views.len(), 2);
    assert_eq!(views[0].id, "v-a");
    assert_eq!(views[1].id, "v-b");
    // The opaque config round-trips byte-identical (never parsed/mutated by the backend).
    assert!(views[0].config.contains(r#""field":"status""#));

    // Edit v-a in place (INSERT OR REPLACE): name changes, count stays 2.
    let mut edited = mk("v-a", "Renamed", 0);
    edited.updated_at = "2026-07-14T10:00:00Z".to_string();
    db.upsert_saved_view(&edited).unwrap();
    let views = db.list_saved_views("meetings").unwrap();
    assert_eq!(views.len(), 2, "edit must not create a duplicate row");
    assert_eq!(
        views.iter().find(|v| v.id == "v-a").unwrap().name,
        "Renamed"
    );

    // Reorder: v-b before v-a → sort_order rewritten, list order follows.
    db.reorder_saved_views("meetings", &["v-b".to_string(), "v-a".to_string()])
        .unwrap();
    let views = db.list_saved_views("meetings").unwrap();
    assert_eq!(views[0].id, "v-b");
    assert_eq!(views[0].sort_order, 0);
    assert_eq!(views[1].id, "v-a");
    assert_eq!(views[1].sort_order, 1);

    // Delete v-a → only v-b remains; delete is idempotent.
    db.delete_saved_view("v-a").unwrap();
    let views = db.list_saved_views("meetings").unwrap();
    assert_eq!(views.len(), 1);
    assert_eq!(views[0].id, "v-b");
    db.delete_saved_view("v-a").unwrap(); // no-op, no error
    assert_eq!(db.list_saved_views("meetings").unwrap().len(), 1);
}

/// GATE regression (RED against a naive impl that reads note markdown without the
/// `get_note_if_visible`/`visibility` gate): a sealed-and-not-session-unlocked meeting must
/// contribute ZERO rows to the per-meeting action-item summary (aggregate posture — NOT a
/// masked/zeroed row). After a session unlock its real open/done counts appear.
#[test]
fn list_meeting_action_summaries_excludes_sealed_meeting() {
    let db = file_db("action-summaries-gate");
    seed_folder(&db, "f-lock", "Secret");
    // An OPEN meeting (vault root): 2 open + 1 done.
    seed_note(
            &db,
            "open1",
            "## Action items\n- [ ] Anna — ship the deck 2026-07-01\n- [ ] loose task\n- [x] Bob — done thing\n",
            None,
        );
    // A meeting we will SEAL (folder locked): 1 open + 1 done — must contribute NOTHING until
    // session-unlocked. Uses a distinct owner/text so a leak would be unambiguous.
    seed_note(
        &db,
        "sealed",
        "## Action items\n- [ ] Dave — secret task 2026-07-02\n- [x] Eve — secret done\n",
        Some("f-lock"),
    );
    db.set_folder_locked("f-lock", true, None).unwrap();

    // GATED (folder locked, not session-unlocked): the sealed meeting is absent entirely.
    let locked = db.list_meeting_action_summaries(&HashSet::new()).unwrap();
    assert!(
        locked.iter().all(|s| s.meeting_id != "sealed"),
        "sealed-not-unlocked meeting leaked into the action summary (gate violation)"
    );
    let open1 = locked
        .iter()
        .find(|s| s.meeting_id == "open1")
        .expect("open meeting must be summarized");
    assert_eq!(open1.open_count, 2, "open1 has 2 open items");
    assert_eq!(open1.done_count, 1, "open1 has 1 done item");
    assert_eq!(
        locked.len(),
        1,
        "only the visible open meeting contributes a row while the folder is sealed"
    );

    // SESSION-UNLOCK → the sealed meeting's real counts appear (reversible).
    let mut unlocked = HashSet::new();
    unlocked.insert("f-lock".to_string());
    let all = db.list_meeting_action_summaries(&unlocked).unwrap();
    let sealed = all
        .iter()
        .find(|s| s.meeting_id == "sealed")
        .expect("unlocked sealed meeting must now contribute a row");
    assert_eq!(sealed.open_count, 1, "sealed meeting has 1 open item");
    assert_eq!(sealed.done_count, 1, "sealed meeting has 1 done item");
    assert_eq!(all.len(), 2, "both meetings visible after unlock");
}

#[test]
fn lock_unlock_round_trips_byte_identical() {
    let db = file_db("roundtrip");
    let kek = crate::crypto::random_key().unwrap();
    seed_folder(&db, "f1", "Secret");
    let md_a = "# Strategy\n\nbudget: 1_000_000 EUR\n- hire 3\n";
    let md_b = "## 1:1 with Sarah\n\nshe wants a raise — zażółć gęślą jaźń 🔒\n";
    seed_note(&db, "m1", md_a, Some("f1"));
    seed_note(&db, "m2", md_b, Some("f1"));

    // SEAL.
    seal_folder(&db, "f1", &kek);

    // After sealing: markdown column blank, content_blob present, exported_path NULL.
    let sealed = db.notes_in_folder("f1").unwrap();
    assert_eq!(sealed.len(), 2);
    for n in &sealed {
        assert_eq!(n.markdown, "", "markdown column must be blanked");
        assert!(n.content_blob.is_some(), "content_blob must be present");
        assert!(n.exported_path.is_none(), "exported_path must be cleared");
    }
    // The raw blob must NOT contain the plaintext (not recoverable without the CK).
    for (n, expected) in sealed.iter().zip([md_a, md_b]) {
        let blob = n.content_blob.as_ref().unwrap();
        assert!(
            !contains_subslice(blob, expected.as_bytes()),
            "ciphertext must not leak plaintext"
        );
    }
    // Folder is locked + carries a wrapped key.
    assert!(db.folder_by_id("f1").unwrap().unwrap().locked);
    assert!(db.folder_wrapped_key("f1").unwrap().is_some());

    // SESSION-UNLOCK → markdown byte-identical.
    session_unlock(&db, "f1", &kek);
    let unlocked = db.notes_in_folder("f1").unwrap();
    let by_id: std::collections::HashMap<_, _> = unlocked
        .iter()
        .map(|n| (n.meeting_id.as_str(), n))
        .collect();
    assert_eq!(
        by_id["m1"].markdown, md_a,
        "m1 markdown must round-trip byte-identical"
    );
    assert_eq!(
        by_id["m2"].markdown, md_b,
        "m2 markdown must round-trip byte-identical"
    );
    // content_blob still present (folder is still locked on disk during a session unlock).
    assert!(by_id["m1"].content_blob.is_some());
}

#[test]
fn multi_provider_seal_unlock_preserves_each_providers_markdown() {
    // REGRESSION: a meeting with TWO provider notes (re-summarized with a second provider)
    // must NOT collapse to a single shared blob on seal. Each (meeting, provider) row carries
    // its OWN distinct markdown; sealing then unlocking must round-trip BOTH byte-identical.
    // (Pre-fix: seal dedup'd by meeting → only the first provider's markdown was encrypted,
    //  then blanked + restored to all rows, destroying the second provider's content.)
    let db = file_db("multi-provider");
    let kek = crate::crypto::random_key().unwrap();
    seed_folder(&db, "f1", "Secret");
    let md_claude = "# Claude note\n\nstructured summary with action items\n";
    let md_ollama = "# Ollama note\n\nDIFFERENT local-model summary — must survive 🔒\n";
    seed_note(&db, "m1", md_claude, Some("f1")); // provider = claude_code
    add_provider_note(&db, "m1", "ollama", md_ollama);

    // Sanity: two distinct provider rows before sealing.
    let before = db.notes_in_folder("f1").unwrap();
    assert_eq!(before.len(), 2, "two provider rows expected");

    seal_folder(&db, "f1", &kek);

    // Each provider row sealed independently: markdown blanked, its own blob present.
    let sealed = db.notes_in_folder("f1").unwrap();
    assert_eq!(sealed.len(), 2);
    for n in &sealed {
        assert_eq!(n.markdown, "", "markdown blanked");
        assert!(
            n.content_blob.is_some(),
            "each provider row keeps its own blob"
        );
    }
    // The two blobs must differ (distinct plaintext → distinct ciphertext).
    let blob_claude = sealed
        .iter()
        .find(|n| n.provider_id == "claude_code")
        .unwrap();
    let blob_ollama = sealed.iter().find(|n| n.provider_id == "ollama").unwrap();
    assert_ne!(
        blob_claude.content_blob, blob_ollama.content_blob,
        "distinct provider markdown must NOT share one blob (content-loss guard)"
    );

    // Unlock → BOTH providers' markdown returns byte-identical.
    session_unlock(&db, "f1", &kek);
    let unlocked = db.notes_in_folder("f1").unwrap();
    let by_provider: std::collections::HashMap<_, _> = unlocked
        .iter()
        .map(|n| (n.provider_id.as_str(), n))
        .collect();
    assert_eq!(
        by_provider["claude_code"].markdown, md_claude,
        "claude_code markdown must round-trip"
    );
    assert_eq!(
        by_provider["ollama"].markdown, md_ollama,
        "ollama markdown must round-trip (NOT overwritten by the sibling provider)"
    );
}

#[test]
fn mcp_visibility_filter() {
    let db = file_db("visibility");
    let kek = crate::crypto::random_key().unwrap();
    seed_folder(&db, "secret", "Secret");
    seed_note(&db, "open1", "# open note about apples", None); // root, always visible
    seed_note(
        &db,
        "sealed1",
        "# secret note about bananas",
        Some("secret"),
    );

    let empty: HashSet<String> = HashSet::new();

    // Before sealing both are visible.
    assert!(db
        .list_meetings_visible(50, &empty)
        .unwrap()
        .iter()
        .any(|m| m.id == "sealed1"));

    // SEAL → the sealed note is invisible to MCP, the open one stays visible.
    seal_folder(&db, "secret", &kek);
    let visible_ids: HashSet<String> = db
        .list_meetings_visible(50, &empty)
        .unwrap()
        .into_iter()
        .map(|m| m.id)
        .collect();
    assert!(visible_ids.contains("open1"), "open note stays visible");
    assert!(!visible_ids.contains("sealed1"), "sealed note is hidden");

    // search_visible: a query that ONLY matches the sealed note's content returns nothing
    // (its markdown is blanked + folder hidden), while the open note is found.
    assert!(db.search_visible("bananas", 20, &empty).unwrap().is_empty());
    assert!(!db.search_visible("apples", 20, &empty).unwrap().is_empty());

    // get_meeting visibility gate.
    assert!(!db.meeting_is_visible("sealed1", &empty).unwrap());
    assert!(db.get_note_if_visible("sealed1", &empty).unwrap().is_none());
    assert!(db.meeting_is_visible("open1", &empty).unwrap());

    // SESSION-UNLOCK → the sealed note becomes visible again.
    session_unlock(&db, "secret", &kek);
    let mut unlocked = HashSet::new();
    unlocked.insert("secret".to_string());
    assert!(db.meeting_is_visible("sealed1", &unlocked).unwrap());
    assert!(db
        .get_note_if_visible("sealed1", &unlocked)
        .unwrap()
        .is_some());
    assert!(!db
        .search_visible("bananas", 20, &unlocked)
        .unwrap()
        .is_empty());
    let visible_after: HashSet<String> = db
        .list_meetings_visible(50, &unlocked)
        .unwrap()
        .into_iter()
        .map(|m| m.id)
        .collect();
    assert!(visible_after.contains("sealed1"));
}

/// A NULL canonical `meetings.folder_id` leaves a live/crash-recovery meeting in the unfiled inbox.
/// Once assigned to a sealed folder, the same meeting must disappear atomically from every
/// meeting-row resolver; provider notes are synchronized but do not establish canonical ownership.
#[test]
fn unfiled_live_meeting_is_visible_until_canonical_locked_ownership_is_assigned() {
    let db = file_db("no-note-root-ownership");
    let empty: HashSet<String> = HashSet::new();
    let title = "live root visibility sentinel";

    db.insert_meeting(&Meeting {
        id: "live-root".to_string(),
        started_at: "2026-08-13T10:00:00Z".to_string(),
        ended_at: None,
        title: Some(title.to_string()),
        duration_s: 17,
        audio_path: Some("/tmp/live-root.wav".to_string()),
        status: MeetingStatus::Recording,
        folder_id: None,
    })
    .unwrap();

    let live = db
        .get_meeting_if_visible("live-root", &empty)
        .unwrap()
        .expect("a no-note live recording is canonically unfiled and visible");
    assert_eq!(live.title.as_deref(), Some(title));
    assert_eq!(live.audio_path.as_deref(), Some("/tmp/live-root.wav"));
    assert_eq!(live.folder_id, None);
    assert_eq!(live.status, MeetingStatus::Recording);
    assert!(db.meeting_is_visible("live-root", &empty).unwrap());
    assert!(db
        .meeting_by_title_visible(title, &empty)
        .unwrap()
        .is_some());
    assert!(db
        .list_meetings_visible(50, &empty)
        .unwrap()
        .iter()
        .any(|meeting| meeting.id == "live-root"));

    seed_folder(&db, "secret-live", "Secret live");
    db.upsert_note(&NoteRecord {
        meeting_id: "live-root".to_string(),
        provider_id: "claude_code".to_string(),
        markdown: "# finalized\n".to_string(),
        created_at: "2026-08-13T10:01:00Z".to_string(),
        exported_path: None,
        model_requested: None,
        model_served: None,
        gateway_host: None,
    })
    .unwrap();
    db.set_meeting_folder("live-root", Some("secret-live"))
        .unwrap();
    db.update_meeting_status("live-root", MeetingStatus::Summarized)
        .unwrap();
    let kek = crate::crypto::random_key().unwrap();
    seal_folder(&db, "secret-live", &kek);

    assert!(db
        .get_meeting_if_visible("live-root", &empty)
        .unwrap()
        .is_none());
    assert!(!db.meeting_is_visible("live-root", &empty).unwrap());
    assert!(db
        .meeting_by_title_visible(title, &empty)
        .unwrap()
        .is_none());
    assert!(!db
        .list_meetings_visible(50, &empty)
        .unwrap()
        .iter()
        .any(|meeting| meeting.id == "live-root"));

    session_unlock(&db, "secret-live", &kek);
    let unlocked = HashSet::from(["secret-live".to_string()]);
    let restored = db
        .get_meeting_if_visible("live-root", &unlocked)
        .unwrap()
        .expect("session unlock restores the canonically owned meeting row");
    assert_eq!(restored.title.as_deref(), Some(title));
    assert_eq!(restored.audio_path.as_deref(), Some("/tmp/live-root.wav"));
    assert_eq!(restored.folder_id.as_deref(), Some("secret-live"));
}

#[test]
fn remove_lock_re_plaintexts() {
    let db = file_db("remove");
    let kek = crate::crypto::random_key().unwrap();
    seed_folder(&db, "f1", "Secret");
    let md = "# permanent\n\nback to plaintext\n";
    seed_note(&db, "m1", md, Some("f1"));
    seal_folder(&db, "f1", &kek);

    // Sanity: sealed.
    assert!(db.folder_by_id("f1").unwrap().unwrap().locked);
    assert_eq!(db.notes_in_folder("f1").unwrap()[0].markdown, "");

    // Mirror of remove_lock: KEK→unwrap CK→decrypt→restore plaintext→clear blob→unlock folder.
    let wrapped = db.folder_wrapped_key("f1").unwrap().unwrap();
    let ck_bytes = crate::crypto::decrypt(&kek, &wrapped, b"").unwrap();
    let ck: [u8; 32] = ck_bytes.as_slice().try_into().unwrap();
    for n in db.notes_in_folder("f1").unwrap() {
        let pt = crate::crypto::decrypt(&ck, n.content_blob.as_ref().unwrap(), b"").unwrap();
        let markdown = String::from_utf8(pt).unwrap();
        db.restore_note_markdown(&n.meeting_id, &n.provider_id, &markdown)
            .unwrap();
        db.clear_note_content_blob(&n.meeting_id).unwrap();
    }
    db.set_folder_locked("f1", false, None).unwrap();

    // Now: plaintext back, blob NULL, locked=0, wrapped_key NULL.
    let after = db.notes_in_folder("f1").unwrap();
    assert_eq!(after[0].markdown, md, "markdown restored byte-identical");
    assert!(after[0].content_blob.is_none(), "content_blob cleared");
    assert!(!db.folder_by_id("f1").unwrap().unwrap().locked);
    assert!(db.folder_wrapped_key("f1").unwrap().is_none());

    // Visible to MCP again with an empty session set.
    assert!(db.meeting_is_visible("m1", &HashSet::new()).unwrap());
}

// ── Phase 0.5 full-lock helpers (transcript + timeline + audio) ──────────────

use crate::transcribe::types::Segment;

/// Seed transcript segments + a cached timeline JSON for a meeting (open state).
fn seed_transcript_and_timeline(db: &Db, meeting_id: &str, texts: &[&str], timeline_json: &str) {
    let segs: Vec<Segment> = texts
        .iter()
        .enumerate()
        .map(|(i, t)| Segment {
            idx: i as i64,
            start_s: i as f64,
            end_s: (i + 1) as f64,
            text: t.to_string(),
            speaker: if i % 2 == 0 {
                Some("me".into())
            } else {
                Some("others".into())
            },
            confidence: None,
        })
        .collect();
    db.insert_segments(meeting_id, &segs).unwrap();
    db.set_timeline_data(meeting_id, timeline_json).unwrap();
}

/// Mirror of `seal_folder_extras`: seal every governed meeting's transcript + timeline under CK
/// (verify-before-blank), and the audio file at `audio_path` → `<file>.enc` (then "remove" the
/// plaintext + re-point audio_path), exactly like the command.
fn seal_extras(db: &Db, folder_id: &str, ck: &[u8; 32]) {
    for mid in db.meeting_ids_in_folder(folder_id).unwrap() {
        // transcript
        let segs = db.raw_segments(&mid).unwrap();
        for s in &segs {
            if s.text_blob.is_some() && s.text.is_empty() {
                continue;
            }
            let blob = crate::crypto::encrypt(ck, s.text.as_bytes(), b"").unwrap();
            assert_eq!(
                crate::crypto::decrypt(ck, &blob, b"").unwrap(),
                s.text.as_bytes()
            );
            db.seal_segment(&mid, s.idx, &blob).unwrap();
        }
        // timeline
        if let Some(tl) = db.raw_timeline(&mid).unwrap() {
            if !(tl.data_blob.is_some() && tl.data.is_empty()) {
                let blob = crate::crypto::encrypt(ck, tl.data.as_bytes(), b"").unwrap();
                db.seal_timeline(&mid, &blob).unwrap();
            }
        }
        // audio
        if let Some(path) = db.get_meeting(&mid).unwrap().and_then(|m| m.audio_path) {
            if !path.ends_with(".enc") && std::path::Path::new(&path).exists() {
                let enc = format!("{path}.enc");
                crate::crypto::encrypt_file(
                    ck,
                    std::path::Path::new(&path),
                    std::path::Path::new(&enc),
                    b"",
                )
                .unwrap();
                std::fs::remove_file(&path).unwrap();
                db.set_meeting_audio_path(&mid, Some(&enc)).unwrap();
            }
        }
    }
}

/// Mirror of `unseal_folder_extras`: decrypt transcript + timeline back into plaintext columns
/// and materialize a playable WAV for the session.
fn unseal_extras(db: &Db, folder_id: &str, ck: &[u8; 32]) {
    for mid in db.meeting_ids_in_folder(folder_id).unwrap() {
        for s in db.raw_segments(&mid).unwrap() {
            if let Some(blob) = &s.text_blob {
                let text =
                    String::from_utf8(crate::crypto::decrypt(ck, blob, b"").unwrap()).unwrap();
                db.restore_segment_text(&mid, s.idx, &text).unwrap();
            }
        }
        if let Some(tl) = db.raw_timeline(&mid).unwrap() {
            if let Some(blob) = &tl.data_blob {
                let data =
                    String::from_utf8(crate::crypto::decrypt(ck, blob, b"").unwrap()).unwrap();
                db.restore_timeline_data(&mid, &data).unwrap();
            }
        }
        if let Some(enc) = db.get_meeting(&mid).unwrap().and_then(|m| m.audio_path) {
            if enc.ends_with(".enc") {
                let plain = enc.trim_end_matches(".enc").to_string();
                crate::crypto::decrypt_file(
                    ck,
                    std::path::Path::new(&enc),
                    std::path::Path::new(&plain),
                    b"",
                )
                .unwrap();
                db.set_meeting_audio_path(&mid, Some(&plain)).unwrap();
            }
        }
    }
}

/// The READ-GATE predicate (`meeting_is_unlocked`): folder open/NULL OR folder id in the
/// session set. The gated commands return masked/empty content when this is false.
fn meeting_unlocked(db: &Db, meeting_id: &str, unlocked: &HashSet<String>) -> bool {
    match db.folder_for_meeting(meeting_id).unwrap() {
        None => true,
        Some(fid) => match db.folder_by_id(&fid).unwrap() {
            None => true,
            Some(f) => !f.locked || unlocked.contains(&fid),
        },
    }
}

#[test]
fn seal_transcript_timeline_round_trips_byte_identical() {
    let db = file_db("extras-roundtrip");
    let kek = crate::crypto::random_key().unwrap();
    seed_folder(&db, "f1", "Secret");
    seed_note(&db, "m1", "# note", Some("f1"));
    let texts = [
        "zażółć gęślą jaźń 🔒",
        "second segment with budget 1_000_000 EUR",
        "",
    ];
    let timeline =
        r#"{"turns":[{"speaker":"me","topic":"secret topic","start_s":0.0,"end_s":1.0}]}"#;
    seed_transcript_and_timeline(&db, "m1", &texts, timeline);
    let ck = crate::crypto::random_key().unwrap();
    // Wrap CK so the folder carries a real wrapped_key (parity with the command).
    let wrapped = crate::crypto::encrypt(&kek, &ck, b"").unwrap();
    db.set_folder_locked("f1", true, Some(&wrapped)).unwrap();

    // SEAL transcript + timeline.
    seal_extras(&db, "f1", &ck);

    // At rest while sealed: plaintext blanked, blobs present, ciphertext does NOT leak.
    let sealed = db.raw_segments("m1").unwrap();
    assert_eq!(sealed.len(), 3);
    for (s, expected) in sealed.iter().zip(texts) {
        assert_eq!(s.text, "", "segment text blanked while sealed");
        assert!(s.text_blob.is_some(), "segment text_blob present");
        if !expected.is_empty() {
            assert!(
                !contains_subslice(s.text_blob.as_ref().unwrap(), expected.as_bytes()),
                "segment ciphertext must not leak plaintext"
            );
        }
    }
    let raw_tl = db.raw_timeline("m1").unwrap().unwrap();
    assert_eq!(raw_tl.data, "", "timeline data blanked while sealed");
    assert!(raw_tl.data_blob.is_some());
    assert!(
        !contains_subslice(raw_tl.data_blob.as_ref().unwrap(), timeline.as_bytes()),
        "timeline ciphertext must not leak plaintext"
    );
    // The user-facing reads see blank while sealed.
    assert!(db
        .get_segments("m1")
        .unwrap()
        .iter()
        .all(|s| s.text.is_empty()));
    assert_eq!(db.get_timeline_data("m1").unwrap().as_deref(), Some(""));

    // UNLOCK → byte-identical round-trip of EVERY segment + the timeline.
    unseal_extras(&db, "f1", &ck);
    let restored = db.get_segments("m1").unwrap();
    let restored_texts: Vec<&str> = restored.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(
        restored_texts, texts,
        "transcript round-trips byte-identical"
    );
    assert_eq!(
        db.get_timeline_data("m1").unwrap().as_deref(),
        Some(timeline),
        "timeline round-trips byte-identical"
    );
    // speaker attribution survives (it is not sealed — only text is).
    assert_eq!(restored[0].speaker.as_deref(), Some("me"));
    assert_eq!(restored[1].speaker.as_deref(), Some("others"));
}

#[test]
fn audio_encrypt_decrypt_round_trips_byte_identical() {
    // Encrypt a temp WAV under the CK, assert the plaintext is removed while sealed, decrypt,
    // assert byte-identical (mirrors the audio-at-rest seal lifecycle through the DB helpers).
    let db = file_db("extras-audio");
    let kek = crate::crypto::random_key().unwrap();
    seed_folder(&db, "f1", "Secret");
    seed_note(&db, "m1", "# note", Some("f1"));

    // A temp "WAV" file (opaque bytes — the crypto layer is content-agnostic).
    let wav = temp_db_path("audio").with_extension("wav");
    let payload: Vec<u8> = (0..8192u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(&wav, &payload).unwrap();
    db.set_meeting_audio_path("m1", Some(&wav.to_string_lossy()))
        .unwrap();

    let ck = crate::crypto::random_key().unwrap();
    let wrapped = crate::crypto::encrypt(&kek, &ck, b"").unwrap();
    db.set_folder_locked("f1", true, Some(&wrapped)).unwrap();

    // SEAL → .enc written, plaintext removed, audio_path re-pointed at the .enc.
    seal_extras(&db, "f1", &ck);
    assert!(!wav.exists(), "plaintext WAV removed while sealed");
    let enc_path = db.get_meeting("m1").unwrap().unwrap().audio_path.unwrap();
    assert!(
        enc_path.ends_with(".enc"),
        "audio_path points at the encrypted file"
    );
    assert!(std::path::Path::new(&enc_path).exists(), ".enc exists");
    let blob = std::fs::read(&enc_path).unwrap();
    assert!(
        !contains_subslice(&blob, &payload),
        "encrypted audio must not leak plaintext"
    );

    // UNLOCK → plaintext WAV materialized again, byte-identical.
    unseal_extras(&db, "f1", &ck);
    let plain_path = db.get_meeting("m1").unwrap().unwrap().audio_path.unwrap();
    assert!(
        !plain_path.ends_with(".enc"),
        "audio_path re-points at the plaintext WAV"
    );
    assert_eq!(
        std::fs::read(&plain_path).unwrap(),
        payload,
        "audio round-trips byte-identical"
    );

    let _ = std::fs::remove_file(&enc_path);
    let _ = std::fs::remove_file(&plain_path);
}

#[test]
fn locked_meeting_detail_is_masked() {
    // get_meeting_detail / get_segments / get_timeline return MASKED/EMPTY + the gate says
    // "locked" when the folder is sealed-and-not-unlocked; full content after the folder id is
    // added to the session unlock set.
    let db = file_db("masked-read");
    let kek = crate::crypto::random_key().unwrap();
    seed_folder(&db, "f1", "Secret");
    seed_note(&db, "m1", "# secret note", Some("f1"));
    seed_transcript_and_timeline(&db, "m1", &["secret words"], r#"{"turns":[]}"#);
    let ck = crate::crypto::random_key().unwrap();
    let wrapped = crate::crypto::encrypt(&kek, &ck, b"").unwrap();
    db.set_folder_locked("f1", true, Some(&wrapped)).unwrap();
    seal_folder(&db, "f1", &kek); // seals the note (markdown)
                                  // Re-seal extras under the folder's own CK (unwrap the wrapped we just set is the SAME CK
                                  // the note seal used? No — seal_folder mints its OWN CK). Use the folder's wrapped CK.
    let folder_wrapped = db.folder_wrapped_key("f1").unwrap().unwrap();
    let folder_ck: [u8; 32] = crate::crypto::decrypt(&kek, &folder_wrapped, b"")
        .unwrap()
        .as_slice()
        .try_into()
        .unwrap();
    seal_extras(&db, "f1", &folder_ck);

    let empty: HashSet<String> = HashSet::new();

    // SEALED-not-unlocked → masked: gate says locked, plaintext columns blank.
    assert!(
        !meeting_unlocked(&db, "m1", &empty),
        "gate: meeting is locked"
    );
    assert!(
        db.get_segments("m1")
            .unwrap()
            .iter()
            .all(|s| s.text.is_empty()),
        "transcript empty while locked"
    );
    assert_eq!(db.get_timeline_data("m1").unwrap().as_deref(), Some(""));
    assert!(
        db.get_latest_note_for_meeting("m1")
            .unwrap()
            .unwrap()
            .markdown
            .is_empty(),
        "note markdown blank while locked"
    );

    // SESSION-UNLOCK (add folder id to the set + decrypt back) → full content.
    let mut unlocked = HashSet::new();
    unlocked.insert("f1".to_string());
    session_unlock(&db, "f1", &kek); // note markdown
    unseal_extras(&db, "f1", &folder_ck); // transcript + timeline
    assert!(
        meeting_unlocked(&db, "m1", &unlocked),
        "gate: meeting unlocked"
    );
    assert_eq!(db.get_segments("m1").unwrap()[0].text, "secret words");
    assert_eq!(
        db.get_timeline_data("m1").unwrap().as_deref(),
        Some(r#"{"turns":[]}"#)
    );
    assert_eq!(
        db.get_latest_note_for_meeting("m1")
            .unwrap()
            .unwrap()
            .markdown,
        "# secret note"
    );
}

#[test]
fn export_audio_refused_when_locked() {
    // The export_audio gate: refuse (Locked) while sealed-not-unlocked; allowed once the
    // folder id is in the session set. Mirrors the `meeting_is_unlocked` early-return.
    let db = file_db("export-audio-gate");
    let kek = crate::crypto::random_key().unwrap();
    seed_folder(&db, "f1", "Secret");
    seed_note(&db, "m1", "# note", Some("f1"));
    let wav = temp_db_path("export-audio").with_extension("wav");
    std::fs::write(&wav, b"RIFF....WAVEfmt fake-pcm").unwrap();
    db.set_meeting_audio_path("m1", Some(&wav.to_string_lossy()))
        .unwrap();
    let ck = crate::crypto::random_key().unwrap();
    let wrapped = crate::crypto::encrypt(&kek, &ck, b"").unwrap();
    db.set_folder_locked("f1", true, Some(&wrapped)).unwrap();
    seal_extras(&db, "f1", &ck);

    let empty: HashSet<String> = HashSet::new();
    // LOCKED → export refused (the command early-returns AppError::Locked when the gate is
    // false). There is also no plaintext WAV on disk to copy.
    assert!(
        !meeting_unlocked(&db, "m1", &empty),
        "export refused while locked"
    );
    let enc = db.get_meeting("m1").unwrap().unwrap().audio_path.unwrap();
    assert!(enc.ends_with(".enc"));
    assert!(!std::path::Path::new(enc.trim_end_matches(".enc")).exists());

    // UNLOCKED → allowed.
    unseal_extras(&db, "f1", &ck);
    let mut unlocked = HashSet::new();
    unlocked.insert("f1".to_string());
    assert!(
        meeting_unlocked(&db, "m1", &unlocked),
        "export allowed once unlocked"
    );
    let plain = db.get_meeting("m1").unwrap().unwrap().audio_path.unwrap();
    assert!(
        std::path::Path::new(&plain).exists(),
        "plaintext WAV available for export"
    );

    let _ = std::fs::remove_file(&enc);
    let _ = std::fs::remove_file(&plain);
}

/// Naive subslice search for the leak assertion.
fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || needle.len() > haystack.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|w| w == needle)
}

// ── brain2 R2 bitemporal facts: persistence + gating + purge ──────────────

use crate::facts::{FactCandidate, FactOp, NewFact};

fn add_op(
    entity_id: &str,
    predicate: &str,
    object: &str,
    valid_from: &str,
    meeting_id: &str,
) -> FactOp {
    FactOp::Add(NewFact {
        entity_id: entity_id.to_string(),
        subject: "Atlas".to_string(),
        predicate: predicate.to_string(),
        object: object.to_string(),
        valid_from: valid_from.to_string(),
        recorded_at: valid_from.to_string(),
        confidence: 1.0,
        meeting_id: Some(meeting_id.to_string()),
    })
}

// ── Brain v2 L1.1/L1.2: topic chunks under the lock model ─────────────────

fn topic_counts(db: &Db, meeting_id: &str) -> (i64, i64) {
    let conn = db.lock();
    let chunks: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM topic_chunks WHERE meeting_id = ?1",
            rusqlite::params![meeting_id],
            |r| r.get(0),
        )
        .unwrap();
    let vecs: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM topic_vec_chunks v
                   JOIN topic_chunks tc ON tc.id = v.chunk_id
                  WHERE tc.meeting_id = ?1",
            rusqlite::params![meeting_id],
            |r| r.get(0),
        )
        .unwrap();
    (chunks, vecs)
}

/// Raw fts_topic_chunks MATCH count for a term — proves the `_ad` trigger purged the sealed
/// content's tokens from the FTS index, not just the base rows.
fn topic_fts_matches(db: &Db, term: &str) -> i64 {
    let conn = db.lock();
    conn.query_row(
        "SELECT COUNT(*) FROM fts_topic_chunks WHERE fts_topic_chunks MATCH ?1",
        rusqlite::params![format!("\"{term}\"")],
        |r| r.get(0),
    )
    .unwrap()
}

fn one_topic_segment(text: &str) -> Vec<Segment> {
    vec![Segment {
        idx: 0,
        start_s: 0.0,
        end_s: 120.0,
        text: text.to_string(),
        speaker: Some("me".to_string()),
        confidence: None,
    }]
}

/// `(note_chunks, vec_chunks)` currently indexed for a meeting — the `index_meeting_chunks`
/// analogue of `topic_counts`, so the seal-TOCTOU test can assert ZERO rows survive a refused
/// write.
fn meeting_chunk_counts(db: &Db, meeting_id: &str) -> (i64, i64) {
    let conn = db.lock();
    let chunks: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM note_chunks WHERE meeting_id = ?1",
            rusqlite::params![meeting_id],
            |r| r.get(0),
        )
        .unwrap();
    let vecs: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM vec_chunks v
                   JOIN note_chunks nc ON nc.id = v.chunk_id
                  WHERE nc.meeting_id = ?1",
            rusqlite::params![meeting_id],
            |r| r.get(0),
        )
        .unwrap();
    (chunks, vecs)
}

/// PURGE-ON-SEAL (L1.1, lock-critical): sealing a folder via `blank_sealed_notes_in_folders`
/// (the same tx that blanks the plaintext) must drop the meeting's topic_chunks AND their
/// topic_vec_chunks rows AND the aug_text tokens from fts_topic_chunks. RED-before-GREEN:
/// without the topic deletes in `purge_chunks_tx` the rows + tokens survive the seal.
#[test]
fn seal_purges_topic_chunks_vectors_and_fts_tokens() {
    let db = file_db("topic-purge-seal");
    seed_folder(&db, "f-lock", "Secret");
    seed_note(&db, "m1", "sealed content", Some("f-lock"));
    let segs = one_topic_segment("tajny wątek fuzji zebra omawiany szczegółowo");
    db.insert_segments("m1", &segs).unwrap();
    let nothing = HashSet::new();
    db.index_meeting_topic_chunks("m1", &segs, &crate::embed::StubEmbedder, &nothing)
        .unwrap();
    let (chunks, vecs) = topic_counts(&db, "m1");
    assert!(chunks > 0 && vecs == chunks, "indexed before seal");
    assert!(
        topic_fts_matches(&db, "zebra") > 0,
        "aug tokens indexed before seal"
    );

    // Seal: flip the folder + run the shared blank/purge tx (the lock_folder path).
    db.set_folder_locked("f-lock", true, None).unwrap();
    let mut folders = HashSet::new();
    folders.insert("f-lock".to_string());
    db.blank_sealed_notes_in_folders(&folders).unwrap();

    let (chunks, vecs) = topic_counts(&db, "m1");
    assert_eq!(chunks, 0, "topic_chunks must be purged on seal");
    assert_eq!(vecs, 0, "topic_vec_chunks must be purged on seal");
    assert_eq!(
        topic_fts_matches(&db, "zebra"),
        0,
        "sealed aug_text tokens must be purged from fts_topic_chunks"
    );
}

/// STARTUP RECONCILE (crash-while-unlocked): `reblank_locked_folders_at_rest` must purge topic
/// rows of every locked folder's meeting — a crash after a session re-index cannot leave a
/// sealed meeting's topic vectors at rest. Mirrors the note_chunks reconcile test.
#[test]
fn reblank_at_rest_purges_topic_chunks() {
    let db = file_db("topic-purge-reblank");
    seed_folder(&db, "f-lock", "Secret");
    seed_note(&db, "m1", "sealed content", Some("f-lock"));
    let segs = one_topic_segment("poufny plan przejęcia gepard w toku");
    db.insert_segments("m1", &segs).unwrap();
    let nothing = HashSet::new();
    db.index_meeting_topic_chunks("m1", &segs, &crate::embed::StubEmbedder, &nothing)
        .unwrap();
    assert!(topic_counts(&db, "m1").0 > 0);

    // Simulate the crash-while-unlocked shape: folder locked on disk, derived rows present.
    db.set_folder_locked("f-lock", true, None).unwrap();
    db.reblank_locked_folders_at_rest().unwrap();

    let (chunks, vecs) = topic_counts(&db, "m1");
    assert_eq!(
        chunks, 0,
        "startup reconcile must purge sealed topic chunks"
    );
    assert_eq!(vecs, 0, "startup reconcile must purge sealed topic vectors");
    assert_eq!(topic_fts_matches(&db, "gepard"), 0);
}

/// L1.2 GATING: the augmentation header carries the meeting's VISIBLE attendees + facts; a
/// sealed-and-not-unlocked meeting is a NO-OP for the topic indexer (nothing is chunked at
/// all — the visibility gate fires before any read).
#[test]
fn topic_aug_header_is_gated_and_sealed_meeting_is_never_indexed() {
    let db = file_db("topic-aug-gate");
    seed_folder(&db, "f-lock", "Secret");
    seed_note(&db, "m1", "atlas planning", Some("f-lock"));
    let anna = db.upsert_entity("Anna Nowak", EntityKind::Person).unwrap();
    db.add_mention(&anna, "m1").unwrap();
    db.apply_fact_ops(&[add_op(
        &anna,
        "deadline",
        "Q3",
        "2026-06-01T00:00:00Z",
        "m1",
    )])
    .unwrap();
    let segs = one_topic_segment("omawiamy harmonogram projektu atlas");
    db.insert_segments("m1", &segs).unwrap();
    let nothing = HashSet::new();

    // OPEN folder: the header carries the gated attendee + fact.
    db.index_meeting_topic_chunks("m1", &segs, &crate::embed::StubEmbedder, &nothing)
        .unwrap();
    let aug: String = {
        let conn = db.lock();
        conn.query_row(
            "SELECT aug_text FROM topic_chunks WHERE meeting_id = 'm1' LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    let header = aug.lines().next().unwrap_or_default().to_string();
    assert!(
        header.contains("Anna Nowak"),
        "attendee missing from header: {header:?}"
    );
    assert!(
        header.contains("deadline: Q3"),
        "fact missing from header: {header:?}"
    );

    // Seal + purge, then try to index again while NOT unlocked: must stay empty (gate no-op).
    db.set_folder_locked("f-lock", true, None).unwrap();
    db.purge_chunks_for_meetings(&["m1".to_string()]).unwrap();
    assert_eq!(topic_counts(&db, "m1").0, 0);
    db.index_meeting_topic_chunks("m1", &segs, &crate::embed::StubEmbedder, &nothing)
        .unwrap();
    assert_eq!(
        topic_counts(&db, "m1").0,
        0,
        "a sealed-not-unlocked meeting must never be topic-indexed"
    );

    // Session-unlocked: indexing is allowed again (the unlock re-index path).
    let mut unlocked = HashSet::new();
    unlocked.insert("f-lock".to_string());
    db.index_meeting_topic_chunks("m1", &segs, &crate::embed::StubEmbedder, &unlocked)
        .unwrap();
    assert!(
        topic_counts(&db, "m1").0 > 0,
        "session-unlocked meeting indexes again"
    );
}

/// TOCTOU (lock-security finding, PR 3): a `lock_folder` committing BETWEEN the indexer's
/// pre-embed visibility gate and its write transaction must not leave freshly-written
/// sealed-meeting plaintext at rest. Simulated by sealing the note at rest (markdown blanked,
/// `content_blob` kept — exactly what `blank_sealed_notes_in_folders` leaves) while the
/// CALLER'S unlocked snapshot is stale and still names the folder: the pre-gate passes, and
/// only the in-tx sealed-at-rest re-check can refuse the write.
#[test]
fn topic_index_refuses_write_when_sealed_at_rest_mid_flight() {
    let db = file_db("topic-toctou");
    seed_folder(&db, "f-lock", "Secret");
    seed_note(&db, "m1", "atlas planning", Some("f-lock"));
    let segs = one_topic_segment("omawiamy harmonogram projektu atlas");
    db.insert_segments("m1", &segs).unwrap();

    // "lock_folder committed mid-embed": folder locked + note sealed at rest…
    db.set_folder_locked("f-lock", true, None).unwrap();
    {
        let conn = db.lock();
        conn.execute(
            "UPDATE notes SET markdown = '', content_blob = X'00' WHERE meeting_id = 'm1'",
            [],
        )
        .unwrap();
    }
    // …while the caller's snapshot is STALE and still says the folder is unlocked.
    let mut stale = HashSet::new();
    stale.insert("f-lock".to_string());
    db.index_meeting_topic_chunks("m1", &segs, &crate::embed::StubEmbedder, &stale)
        .unwrap();
    assert_eq!(
        topic_counts(&db, "m1").0,
        0,
        "the in-tx sealed-at-rest re-check must refuse writing topic plaintext"
    );
}

/// TOCTOU (lock-security finding, PR-1): the SAME race for `index_meeting_chunks` (note-summary +
/// transcript chunks/vectors). A `lock_folder` committing between the caller's pre-check and this
/// indexer's write tx is simulated by sealing the note at rest (markdown blanked, `content_blob`
/// kept). The indexer must refuse to write the meeting's derived plaintext chunks/vectors.
/// RED-before-GREEN: remove the in-tx `sealed_at_rest` re-check in `index_meeting_chunks` and this
/// asserts non-zero rows (a sealed folder's plaintext chunks/vectors persisted at rest — the leak).
#[test]
fn meeting_index_refuses_write_when_sealed_at_rest_mid_flight() {
    let db = file_db("meeting-toctou");
    seed_folder(&db, "f-lock", "Secret");
    seed_note(&db, "m1", "atlas planning notes", Some("f-lock"));
    let segs = one_topic_segment("omawiamy harmonogram projektu atlas i budżet");
    db.insert_segments("m1", &segs).unwrap();

    // "lock_folder committed mid-embed": note sealed at rest (markdown='', content_blob kept) —
    // exactly what `blank_sealed_notes_in_folders` leaves.
    db.set_folder_locked("f-lock", true, None).unwrap();
    {
        let conn = db.lock();
        conn.execute(
            "UPDATE notes SET markdown = '', content_blob = X'00' WHERE meeting_id = 'm1'",
            [],
        )
        .unwrap();
    }
    db.index_meeting_chunks("m1", &segs, &crate::embed::StubEmbedder)
        .unwrap();
    assert_eq!(
        meeting_chunk_counts(&db, "m1"),
        (0, 0),
        "the in-tx sealed-at-rest re-check must refuse writing meeting chunks/vectors"
    );

    // Session-unlock un-blanks the plaintext BEFORE re-indexing → the write proceeds.
    {
        let conn = db.lock();
        conn.execute(
            "UPDATE notes SET markdown = 'atlas planning notes' WHERE meeting_id = 'm1'",
            [],
        )
        .unwrap();
    }
    db.index_meeting_chunks("m1", &segs, &crate::embed::StubEmbedder)
        .unwrap();
    let (chunks, vecs) = meeting_chunk_counts(&db, "m1");
    assert!(
        chunks > 0 && vecs == chunks,
        "an unsealed (un-blanked) meeting indexes again"
    );
}

/// apply_fact_ops persists an open fact; a later reconcile of a CHANGED object closes the old
/// (valid_to set) and opens the new — both rows survive (bitemporal history). RED-before-GREEN:
/// without the Invalidate UPDATE the old fact stays open (two open rows), failing the assertions.
#[test]
fn facts_apply_and_bitemporal_history_round_trips() {
    let db = file_db("facts-bitemporal");
    seed_note(&db, "m1", "Atlas is in progress", None);
    let atlas = db.upsert_entity("Atlas", EntityKind::Project).unwrap();
    db.add_mention(&atlas, "m1").unwrap();

    // First meeting records: status = in-progress (open).
    db.apply_fact_ops(&[add_op(
        &atlas,
        "status",
        "in-progress",
        "2026-06-01T00:00:00Z",
        "m1",
    )])
    .unwrap();

    // Second meeting says: status = shipped → reconcile.
    let existing = db.facts_for_entities(std::slice::from_ref(&atlas)).unwrap();
    assert_eq!(existing.len(), 1);
    let cands = vec![FactCandidate {
        entity_id: atlas.clone(),
        subject: "Atlas".to_string(),
        predicate: "status".to_string(),
        object: "shipped".to_string(),
        confidence: 1.0,
    }];
    let at = "2026-06-20T00:00:00Z";
    let mut ops = crate::facts::reconcile_facts(&existing, &cands, at);
    crate::facts::set_meeting_id(&mut ops, "m1");
    db.apply_fact_ops(&ops).unwrap();

    // Both rows present: old closed at `at`, new open.
    let all = db.facts_for_entities(std::slice::from_ref(&atlas)).unwrap();
    assert_eq!(
        all.len(),
        2,
        "history preserved — old fact kept, not overwritten"
    );
    let open: Vec<_> = all.iter().filter(|f| f.valid_to.is_none()).collect();
    let closed: Vec<_> = all.iter().filter(|f| f.valid_to.is_some()).collect();
    assert_eq!(open.len(), 1, "exactly one currently-valid fact");
    assert_eq!(open[0].object, "shipped");
    assert_eq!(open[0].valid_from, at);
    assert_eq!(closed.len(), 1, "exactly one superseded fact");
    assert_eq!(closed[0].object, "in-progress");
    assert_eq!(
        closed[0].valid_to.as_deref(),
        Some(at),
        "old fact closed at the supersession instant"
    );

    // The gated read returns both (open first), since m1 is in an open folder.
    let facts = db.list_facts_visible(&atlas, &HashSet::new()).unwrap();
    assert_eq!(facts.len(), 2);
    assert!(
        facts[0].valid_to.is_none(),
        "open (current) fact ordered first"
    );
}

/// list_facts_visible GATE: a fact whose source meeting is in a sealed-and-not-unlocked folder is
/// INVISIBLE, and reappears once the folder is session-unlocked. Uses set_folder_locked directly
/// (NOT lock_folder) so the row survives at rest — this proves the READ GATE, independent of the
/// purge-on-seal. RED-before-GREEN: drop the meetings-JOIN visibility predicate → the sealed
/// fact leaks.
#[test]
fn list_facts_visible_excludes_sealed_meeting() {
    let db = file_db("facts-gate");
    seed_folder(&db, "f-lock", "Secret");
    seed_note(&db, "secret1", "Atlas acquisition", Some("f-lock"));
    let atlas = db.upsert_entity("Atlas", EntityKind::Project).unwrap();
    db.add_mention(&atlas, "secret1").unwrap();
    db.apply_fact_ops(&[add_op(
        &atlas,
        "price",
        "10M",
        "2026-06-01T00:00:00Z",
        "secret1",
    )])
    .unwrap();

    // Open folder → fact visible.
    assert_eq!(
        db.list_facts_visible(&atlas, &HashSet::new())
            .unwrap()
            .len(),
        1
    );

    // Seal the folder flag directly (no purge) → the row survives at rest but must be GATED OUT.
    db.set_folder_locked("f-lock", true, None).unwrap();
    assert!(
        db.list_facts_visible(&atlas, &HashSet::new())
            .unwrap()
            .is_empty(),
        "a sealed-not-unlocked meeting's facts must not surface (gate violation)"
    );

    // Session-unlock → the fact reappears.
    let mut unlocked = HashSet::new();
    unlocked.insert("f-lock".to_string());
    assert_eq!(
        db.list_facts_visible(&atlas, &unlocked).unwrap().len(),
        1,
        "facts reappear once the folder is session-unlocked"
    );
}

/// PURGE-ON-SEAL: the same atomic tx that purges chunks/corrections/assistant-interactions also
/// DELETES the meeting's facts (purge_facts_tx). RED-before-GREEN: without the purge_facts_tx
/// call the fact row survives the seal.
#[test]
fn seal_purges_facts() {
    let db = file_db("facts-purge");
    seed_note(&db, "m1", "Atlas shipped", None);
    let atlas = db.upsert_entity("Atlas", EntityKind::Project).unwrap();
    db.add_mention(&atlas, "m1").unwrap();
    db.apply_fact_ops(&[add_op(
        &atlas,
        "status",
        "shipped",
        "2026-06-01T00:00:00Z",
        "m1",
    )])
    .unwrap();
    assert_eq!(
        db.facts_for_entities(std::slice::from_ref(&atlas))
            .unwrap()
            .len(),
        1
    );

    // The seal purge (chunks + corrections + assistant interactions + FACTS) in one tx.
    db.purge_chunks_for_meetings(&["m1".to_string()]).unwrap();
    assert!(
        db.facts_for_entities(std::slice::from_ref(&atlas))
            .unwrap()
            .is_empty(),
        "facts must be purged on seal (drop-on-seal, like correction_log / note_chunks)"
    );
}

/// delete_meeting cascades to facts (FK ON DELETE CASCADE).
#[test]
fn delete_meeting_cascades_to_facts() {
    let db = file_db("facts-cascade");
    seed_note(&db, "m1", "Atlas", None);
    let atlas = db.upsert_entity("Atlas", EntityKind::Project).unwrap();
    db.add_mention(&atlas, "m1").unwrap();
    db.apply_fact_ops(&[add_op(
        &atlas,
        "status",
        "shipped",
        "2026-06-01T00:00:00Z",
        "m1",
    )])
    .unwrap();
    db.delete_meeting("m1").unwrap();
    assert!(
        db.facts_for_entities(&[atlas]).unwrap().is_empty(),
        "FK CASCADE drops facts"
    );
}

// ── Re-Truth (supersessions): record/dedup/lifecycle + purge-on-delete + purge-on-seal ──

fn supersession_row(
    id: &str,
    superseding: &str,
    source: &str,
) -> crate::storage::models::SupersessionRow {
    crate::storage::models::SupersessionRow {
        id: id.to_string(),
        superseding_meeting_id: superseding.to_string(),
        source_meeting_id: source.to_string(),
        entity: "Atlas".to_string(),
        predicate: "status".to_string(),
        old_value: "in-progress".to_string(),
        new_value: "shipped".to_string(),
        created_at: "2026-06-20T00:00:00Z".to_string(),
        applied_at: None,
        source_pre_image: None,
        superseding_pre_image: None,
    }
}

/// record → unapplied read → DEDUP on natural key → mark applied (with pre-images) → applied row
/// leaves the unapplied set → clear (undo) restores it as pending + drops the pre-images.
#[test]
fn supersessions_record_dedup_and_lifecycle() {
    let db = file_db("retruth-lifecycle");
    assert_eq!(
        db.record_supersessions(&[supersession_row("s1", "m2", "m1")])
            .unwrap(),
        1
    );
    // Re-record the SAME natural key → deduped (0 new), one row total.
    assert_eq!(
        db.record_supersessions(&[supersession_row("s1-dup", "m2", "m1")])
            .unwrap(),
        0,
        "identical natural key is deduped"
    );
    let pending = db.unapplied_supersessions_for("m2").unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, "s1");
    assert!(pending[0].applied_at.is_none());

    // Durably store a pre-image (undo scratch), THEN mark applied (applied_at is the last write).
    db.store_supersession_pre_images("s1", Some(&b"ORIGINAL"[..]), None)
        .unwrap();
    db.mark_supersession_applied("s1", "2026-06-21T00:00:00Z")
        .unwrap();
    assert!(
        db.unapplied_supersessions_for("m2").unwrap().is_empty(),
        "an applied row is no longer pending"
    );
    let got = db.get_supersession("s1").unwrap().unwrap();
    assert_eq!(got.applied_at.as_deref(), Some("2026-06-21T00:00:00Z"));
    assert_eq!(got.source_pre_image.as_deref(), Some(&b"ORIGINAL"[..]));

    // Clear (undo) → pending again, pre-image dropped.
    db.clear_supersession_applied("s1").unwrap();
    let after = db.get_supersession("s1").unwrap().unwrap();
    assert!(after.applied_at.is_none());
    assert!(after.source_pre_image.is_none());
    assert_eq!(db.unapplied_supersessions_for("m2").unwrap().len(), 1);
}

/// delete_meeting purges supersessions referencing that meeting on EITHER side (no FK — the
/// explicit `purge_supersessions_tx`). RED-before-GREEN: without the purge the row survives.
#[test]
fn delete_meeting_purges_supersessions_either_side() {
    let db = file_db("retruth-delete");
    // Row where the deleted meeting is the SOURCE.
    db.record_supersessions(&[supersession_row("s-src", "m2", "m1")])
        .unwrap();
    db.delete_meeting("m1").unwrap();
    assert!(
        db.get_supersession("s-src").unwrap().is_none(),
        "deleting the source meeting purges its supersession"
    );
    // Row where the deleted meeting is the SUPERSEDING side.
    db.record_supersessions(&[supersession_row("s-sup", "m3", "m4")])
        .unwrap();
    db.delete_meeting("m3").unwrap();
    assert!(
        db.get_supersession("s-sup").unwrap().is_none(),
        "deleting the superseding meeting purges its supersession"
    );
}

/// PURGE-ON-SEAL: the seal purge tx (`purge_chunks_for_meetings`) also drops supersessions
/// referencing the sealed meeting — an applied row's PLAINTEXT note pre-image must not linger at
/// rest for a sealed folder. RED-before-GREEN: without the `purge_supersessions_tx` call the row
/// (and its plaintext pre-image) survives the seal.
#[test]
fn seal_purges_supersessions() {
    let db = file_db("retruth-seal");
    db.record_supersessions(&[supersession_row("s1", "m2", "m1")])
        .unwrap();
    // Simulate apply having stored the plaintext pre-image + marked applied.
    db.store_supersession_pre_images("s1", Some(&b"PLAINTEXT NOTE"[..]), None)
        .unwrap();
    db.mark_supersession_applied("s1", "2026-06-21T00:00:00Z")
        .unwrap();
    // Seal the source meeting's folder → the seal purge tx runs for m1.
    db.purge_chunks_for_meetings(&["m1".to_string()]).unwrap();
    assert!(
        db.get_supersession("s1").unwrap().is_none(),
        "a sealed meeting's supersession (+ its plaintext pre-image) must be purged on seal"
    );
}

/// STARTUP crash-while-unlocked reconcile: `reblank_locked_folders_at_rest` must purge every
/// supersession referencing a locked meeting on EITHER side — so an applied row's plaintext note
/// pre-images + fact-value strings do not linger at rest behind the lock after a restart. Mirrors
/// `seal_purges_supersessions` but drives the STARTUP path. RED-before-GREEN: without the
/// supersessions DELETE in that fn, both rows survive the reblank.
#[test]
fn reblank_at_rest_purges_supersessions() {
    let db = file_db("retruth-reblank");
    seed_folder(&db, "f-src", "Work");
    seed_folder(&db, "f-open", "Reviews");
    seed_note(&db, "m1", "source note", Some("f-src"));
    seed_note(&db, "m2", "other note", Some("f-open"));
    // Row A: the to-be-locked meeting m1 is the SOURCE. Row B: m1 is the SUPERSEDING side.
    db.record_supersessions(&[supersession_row("s-src", "m2", "m1")])
        .unwrap();
    db.record_supersessions(&[supersession_row("s-sup", "m1", "m2")])
        .unwrap();
    // Simulate both APPLIED with plaintext pre-images (the leak-at-rest scratch).
    for id in ["s-src", "s-sup"] {
        db.store_supersession_pre_images(id, Some(&b"PLAINTEXT NOTE"[..]), None)
            .unwrap();
        db.mark_supersession_applied(id, "2026-06-21T00:00:00Z")
            .unwrap();
    }
    assert!(db.get_supersession("s-src").unwrap().is_some());
    assert!(db.get_supersession("s-sup").unwrap().is_some());

    // Lock m1's folder on disk, then run the startup at-rest reblank (crash-while-unlocked path).
    db.set_folder_locked("f-src", true, Some(&b"wrapped"[..]))
        .unwrap();
    db.reblank_locked_folders_at_rest().unwrap();

    assert!(
        db.get_supersession("s-src").unwrap().is_none(),
        "source-side supersession (+ plaintext pre-image) purged at rest"
    );
    assert!(
        db.get_supersession("s-sup").unwrap().is_none(),
        "superseding-side supersession (+ plaintext pre-image) purged at rest"
    );
}

// ── Phase 3 CROSS-MEETING USER MEMORY: persistence + reconcile + gating + forget + purge ──

fn user_add_op(predicate: &str, object: &str, valid_from: &str, meeting_id: &str) -> FactOp {
    FactOp::Add(NewFact {
        entity_id: crate::user_memory::USER_SCOPE.to_string(),
        subject: "You".to_string(),
        predicate: predicate.to_string(),
        object: object.to_string(),
        valid_from: valid_from.to_string(),
        recorded_at: valid_from.to_string(),
        confidence: 1.0,
        meeting_id: Some(meeting_id.to_string()),
    })
}

/// ROUND-TRIP persist/reconcile (task C5.ii): apply an open user fact; a later reconcile of a
/// CHANGED object for the SAME predicate closes the old (valid_to set) and opens the new — both
/// rows survive (bitemporal history), and only the current one is visible. RED-before-GREEN:
/// without the Invalidate UPDATE two open rows survive, failing the single-open assertion.
#[test]
fn user_facts_apply_and_reconcile_round_trips() {
    let db = file_db("user-facts-roundtrip");
    seed_note(&db, "m1", "note", None);
    db.apply_user_fact_ops(&[user_add_op(
        "prefer",
        "English replies",
        "2026-06-01T00:00:00Z",
        "m1",
    )])
    .unwrap();

    // A later meeting supersedes the preference: prefer = Polish replies.
    seed_note(&db, "m2", "note2", None);
    let existing = db.user_facts_all().unwrap();
    assert_eq!(existing.len(), 1);
    let cands = vec![FactCandidate {
        entity_id: crate::user_memory::USER_SCOPE.to_string(),
        subject: "You".to_string(),
        predicate: "prefer".to_string(),
        object: "Polish replies".to_string(),
        confidence: 1.0,
    }];
    let at = "2026-06-20T00:00:00Z";
    let mut ops = crate::facts::reconcile_facts(&existing, &cands, at);
    crate::facts::set_meeting_id(&mut ops, "m2");
    db.apply_user_fact_ops(&ops).unwrap();

    let all = db.user_facts_all().unwrap();
    assert_eq!(
        all.len(),
        2,
        "history preserved — old user fact kept, not overwritten"
    );
    let open: Vec<_> = all.iter().filter(|f| f.valid_to.is_none()).collect();
    assert_eq!(open.len(), 1, "exactly one currently-valid user fact");
    assert_eq!(open[0].object, "Polish replies");

    // The gated read returns only the OPEN, VISIBLE fact.
    let visible = db.list_user_facts_visible(&HashSet::new()).unwrap();
    assert_eq!(visible.len(), 1, "only the current preference is visible");
    assert_eq!(visible[0].object, "Polish replies");
    assert_eq!(
        visible[0].meeting_id.as_deref(),
        Some("m2"),
        "provenance = the source meeting"
    );
}

/// GATE (task C5.i, DB layer): a user fact whose source meeting is sealed-and-not-unlocked is
/// INVISIBLE and reappears once the folder is session-unlocked. Uses set_folder_locked directly
/// (NOT lock_folder) so the row survives at rest — this proves the READ GATE, independent of the
/// purge-on-seal. RED-before-GREEN: drop the meetings-JOIN visibility predicate → the sealed user
/// fact leaks into the audit view AND the brief.
#[test]
fn list_user_facts_visible_excludes_sealed_meeting() {
    let db = file_db("user-facts-gate");
    seed_folder(&db, "f-lock", "Secret");
    seed_note(&db, "secret1", "private", Some("f-lock"));
    db.apply_user_fact_ops(&[user_add_op(
        "salary",
        "confidential",
        "2026-06-01T00:00:00Z",
        "secret1",
    )])
    .unwrap();

    assert_eq!(
        db.list_user_facts_visible(&HashSet::new()).unwrap().len(),
        1
    );
    db.set_folder_locked("f-lock", true, None).unwrap();
    assert!(
        db.list_user_facts_visible(&HashSet::new())
            .unwrap()
            .is_empty(),
        "a sealed-not-unlocked meeting's user facts must not surface (gate violation)"
    );
    let mut unlocked = HashSet::new();
    unlocked.insert("f-lock".to_string());
    assert_eq!(
        db.list_user_facts_visible(&unlocked).unwrap().len(),
        1,
        "user facts reappear once the folder is session-unlocked"
    );
}

/// R4/#17 (regression). An ENTITY fact must be FORGETTABLE.
///
/// Until `forget_entity_fact` existed, the store exposed forget/clear for USER facts only, so the
/// only way to close a wrong entity fact was for a later meeting to assert a different object for
/// the same key. A junk row nobody would ever restate — the real `owner: claude_code` that reached
/// a dossier as a current fact — therefore stayed CURRENT forever.
///
/// Same bitemporal contract as the user-fact twin: CLOSE, never delete, and idempotent.
#[test]
fn forget_entity_fact_closes_the_row_and_is_idempotent() {
    use crate::facts::{FactOp, NewFact};
    use crate::storage::models::EntityKind;
    let db = file_db("entity-facts-forget");
    seed_note(&db, "m1", "note", None);
    // `facts.entity_id` is a real FK — the entity has to exist before a fact can reference it.
    let entity = db
        .upsert_entity("M1 Advanced Mode", EntityKind::Project)
        .unwrap();
    let new_fact = |predicate: &str, object: &str| {
        FactOp::Add(NewFact {
            entity_id: entity.clone(),
            subject: "M1 Advanced Mode".into(),
            predicate: predicate.into(),
            object: object.into(),
            valid_from: "2026-06-01T00:00:00Z".into(),
            recorded_at: "2026-06-01T00:00:00Z".into(),
            confidence: 1.0,
            meeting_id: Some("m1".into()),
        })
    };
    db.apply_fact_ops(&[new_fact("owner", "claude_code"), new_fact("status", "open")])
        .unwrap();

    let visible = db.list_facts_visible(&entity, &HashSet::new()).unwrap();
    assert_eq!(visible.len(), 2, "both facts start current");
    let junk = visible
        .iter()
        .find(|f| f.predicate == "owner")
        .expect("the junk owner row")
        .id
        .clone();
    db.persist_ask_exchange(
        &AskConversationScope::Vault,
        None,
        "Who owns the project?",
        "claude_code owns it.",
        &[],
        &[],
        &[],
        &[],
        "2026-06-01T12:00:00Z",
    )
    .unwrap();

    assert!(
        db.forget_entity_fact(&junk, "2026-06-02T00:00:00Z").unwrap(),
        "closed the open junk fact"
    );
    assert!(
        !db.forget_entity_fact(&junk, "2026-06-02T00:00:00Z").unwrap(),
        "re-forget is a no-op (idempotent)"
    );
    assert!(db
        .list_ask_conversation_ids(&AskConversationScope::Vault, &HashSet::new())
        .unwrap()
        .is_empty());

    // `list_facts_visible` deliberately returns the FULL bitemporal ledger (open + closed), because
    // knowledge_diff needs the history. What must change is which facts are still OPEN — that is
    // what `dossier::render_structured` filters on (`valid_to.is_none()`) to print CURRENT FACTS,
    // and therefore what an agent reads as true.
    let after = db.list_facts_visible(&entity, &HashSet::new()).unwrap();
    assert_eq!(after.len(), 2, "history is preserved, never deleted");
    let open: Vec<_> = after.iter().filter(|f| f.valid_to.is_none()).collect();
    assert_eq!(open.len(), 1, "only one fact is still current");
    assert_eq!(
        open[0].predicate, "status",
        "the SIBLING stays current; the junk owner is no longer a current fact"
    );
    let closed = after
        .iter()
        .find(|f| f.predicate == "owner")
        .expect("the owner row still exists as history");
    assert_eq!(
        closed.valid_to.as_deref(),
        Some("2026-06-02T00:00:00Z"),
        "closed AT the requested instant, not deleted"
    );
}

/// FORGET (task C5.iii): forget_user_fact bitemporally CLOSES the row (never deletes) so it drops
/// out of the gated read; a second forget is a no-op. clear_user_facts closes ALL open facts.
#[test]
fn forget_and_clear_user_facts() {
    let db = file_db("user-facts-forget");
    seed_note(&db, "m1", "note", None);
    db.apply_user_fact_ops(&[
        user_add_op("prefer", "Polish", "2026-06-01T00:00:00Z", "m1"),
        user_add_op("role", "PM", "2026-06-01T00:00:00Z", "m1"),
    ])
    .unwrap();
    let visible = db.list_user_facts_visible(&HashSet::new()).unwrap();
    assert_eq!(visible.len(), 2);
    let target = visible[0].id.clone();
    db.persist_ask_exchange(
        &AskConversationScope::Vault,
        None,
        "What do you remember?",
        "You prefer Polish.",
        &[],
        &[],
        &[],
        &[],
        "2026-06-01T12:00:00Z",
    )
    .unwrap();

    // Forget one → it is closed and drops out.
    assert!(
        db.forget_user_fact(&target, "2026-06-02T00:00:00Z")
            .unwrap(),
        "closed one open fact"
    );
    assert!(
        !db.forget_user_fact(&target, "2026-06-02T00:00:00Z")
            .unwrap(),
        "re-forget is a no-op"
    );
    assert!(db
        .list_ask_conversation_ids(&AskConversationScope::Vault, &HashSet::new())
        .unwrap()
        .is_empty());
    let after = db.list_user_facts_visible(&HashSet::new()).unwrap();
    assert_eq!(
        after.len(),
        1,
        "the forgotten fact drops out of the gated read"
    );
    assert!(after.iter().all(|f| f.id != target));
    // The row still EXISTS (closed, not deleted) — history preserved.
    assert_eq!(
        db.user_facts_all().unwrap().len(),
        2,
        "forget is an invalidate, not a delete"
    );

    // Clear all → nothing visible; every open fact closed.
    db.persist_ask_exchange(
        &AskConversationScope::Vault,
        None,
        "What is my role?",
        "You are a PM.",
        &[],
        &[],
        &[],
        &[],
        "2026-06-02T12:00:00Z",
    )
    .unwrap();
    let n = db.clear_user_facts("2026-06-03T00:00:00Z").unwrap();
    assert_eq!(n, 1, "one remaining open fact closed");
    assert!(
        db.list_user_facts_visible(&HashSet::new())
            .unwrap()
            .is_empty(),
        "no user memory after clear"
    );
    assert!(db
        .list_ask_conversation_ids(&AskConversationScope::Vault, &HashSet::new())
        .unwrap()
        .is_empty());
}

/// PURGE-ON-SEAL (task C2.a, DB layer): the same atomic seal tx that purges facts also DELETES the
/// meeting's user facts (purge_user_facts_tx). RED-before-GREEN: without the purge_user_facts_tx
/// call the user-fact row survives the seal at rest.
#[test]
fn seal_purges_user_facts() {
    let db = file_db("user-facts-purge");
    seed_note(&db, "m1", "note", None);
    db.apply_user_fact_ops(&[user_add_op(
        "prefer",
        "Polish",
        "2026-06-01T00:00:00Z",
        "m1",
    )])
    .unwrap();
    assert_eq!(db.user_facts_all().unwrap().len(), 1);
    // The seal purge (chunks + corrections + assistant interactions + facts + USER facts) in one tx.
    db.purge_chunks_for_meetings(&["m1".to_string()]).unwrap();
    assert!(
        db.user_facts_all().unwrap().is_empty(),
        "user facts must be purged on seal (drop-on-seal, like facts / note_chunks)"
    );
}

/// delete_meeting cascades to user_facts (FK ON DELETE CASCADE).
#[test]
fn delete_meeting_cascades_to_user_facts() {
    let db = file_db("user-facts-cascade");
    seed_note(&db, "m1", "note", None);
    db.apply_user_fact_ops(&[user_add_op(
        "prefer",
        "Polish",
        "2026-06-01T00:00:00Z",
        "m1",
    )])
    .unwrap();
    db.delete_meeting("m1").unwrap();
    assert!(
        db.user_facts_all().unwrap().is_empty(),
        "FK CASCADE drops user facts"
    );
}

/// Brain v2 L2.2 (RED-first): `search_user_facts_visible` is BM25-relevance-filtered AND runs
/// the SAME visibility gate as `list_user_facts_visible` — a sealed-not-unlocked source
/// meeting's fact never matches, a session unlock re-admits it, only OPEN facts return, and an
/// empty/punctuation-only query returns nothing (the caller's fallback owns that case).
#[test]
fn search_user_facts_visible_is_relevance_filtered_and_gated() {
    let db = file_db("user-facts-search");
    seed_folder(&db, "f-s", "Secret");
    seed_note(&db, "m-open", "open note", None);
    seed_note(&db, "m-priv", "private note", Some("f-s"));
    db.apply_user_fact_ops(&[
        user_add_op(
            "works on",
            "Project Atlas",
            "2026-07-01T00:00:00Z",
            "m-open",
        ),
        user_add_op("prefer", "Polish replies", "2026-07-01T00:00:00Z", "m-open"),
        user_add_op("salary", "Atlas bonus", "2026-07-01T00:00:00Z", "m-priv"),
    ])
    .unwrap();
    let none = HashSet::new();

    // Relevance: an "atlas" query matches the two Atlas facts, not the Polish-replies one.
    let hits = db
        .search_user_facts_visible("what about atlas?", 8, &none)
        .unwrap();
    assert_eq!(
        hits.len(),
        2,
        "both atlas facts match while all folders are open"
    );
    assert!(hits.iter().all(|f| f.object.contains("Atlas")));

    // Empty / punctuation-only query ⇒ no hits (fallback belongs to the caller).
    assert!(db
        .search_user_facts_visible("", 8, &none)
        .unwrap()
        .is_empty());
    assert!(db
        .search_user_facts_visible("?!():", 8, &none)
        .unwrap()
        .is_empty());
    // FIX 3: a STOPWORD-ONLY query is no query at all — it must return zero hits (so the
    // caller's full-list fallback owns it), never a stopword-overlap "match".
    assert!(db
        .search_user_facts_visible("what about the", 8, &none)
        .unwrap()
        .is_empty());

    // GATE: seal the private folder — its fact drops out of the SAME query.
    db.set_folder_locked("f-s", true, None).unwrap();
    let hits = db.search_user_facts_visible("atlas", 8, &none).unwrap();
    assert_eq!(hits.len(), 1, "the sealed source's fact must not match");
    assert_eq!(hits[0].meeting_id.as_deref(), Some("m-open"));

    // A session unlock re-admits it (reversible gate).
    let mut unlocked = HashSet::new();
    unlocked.insert("f-s".to_string());
    assert_eq!(
        db.search_user_facts_visible("atlas", 8, &unlocked)
            .unwrap()
            .len(),
        2
    );

    // Only OPEN facts: forget the open-folder Atlas fact → it stops matching.
    let open_fact_id = db
        .list_user_facts_visible(&none)
        .unwrap()
        .into_iter()
        .find(|f| f.object == "Project Atlas")
        .unwrap()
        .id;
    db.forget_user_fact(&open_fact_id, "2026-07-02T00:00:00Z")
        .unwrap();
    assert!(
        db.search_user_facts_visible("atlas", 8, &none)
            .unwrap()
            .is_empty(),
        "a closed (forgotten) fact must not match"
    );
}

/// Brain v2 L2.2 LOCK-AT-REST: the purge-on-seal DELETE and `delete_meeting` both remove the
/// fact's tokens from the `fts_user_facts` index (the `_ad` trigger fires on the direct
/// DELETE) — no sealed/deleted fact text survives as a searchable token. Mirrors
/// `sealed_tokens_purged_from_fts_after_blank` for the user-facts index.
#[test]
fn user_fact_tokens_purged_from_fts_on_seal_and_delete() {
    let db = file_db("user-facts-fts-purge");
    seed_note(&db, "m1", "note", None);
    seed_note(&db, "m2", "note2", None);
    db.apply_user_fact_ops(&[
        user_add_op("codename", "Zenith", "2026-07-01T00:00:00Z", "m1"),
        user_add_op("codename2", "Quasar", "2026-07-01T00:00:00Z", "m2"),
    ])
    .unwrap();
    let fts_count = |term: &str| -> i64 {
        let conn = db.lock();
        conn.query_row(
            "SELECT count(*) FROM fts_user_facts WHERE fts_user_facts MATCH ?1",
            rusqlite::params![format!("\"{term}\"")],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(fts_count("zenith"), 1);
    assert_eq!(fts_count("quasar"), 1);

    // Purge-on-seal path (the direct DELETE inside the seal tx).
    db.purge_chunks_for_meetings(&["m1".to_string()]).unwrap();
    assert_eq!(
        fts_count("zenith"),
        0,
        "sealed fact tokens must leave the index"
    );

    // delete_meeting path (explicit purge in-tx).
    db.delete_meeting("m2").unwrap();
    assert_eq!(
        fts_count("quasar"),
        0,
        "deleted fact tokens must leave the index"
    );
}

/// FIX 1a (CRITICAL leak — the DB half): every seal/delete transaction purges ALL
/// `memory_rollups` rows and returns their `exported_path`s for the caller's file deletion —
/// `purge_chunks_for_meetings` (lock_folder / move-into-locked), `blank_sealed_notes_in_folders`
/// (relock), `reblank_locked_folders_at_rest` (startup reconcile — purges ONLY when a locked
/// folder exists, so an open-only DB keeps its rollups across restarts), and `delete_meeting`.
/// RED on the pre-fix code: none of these touched `memory_rollups`.
#[test]
fn seal_paths_purge_memory_rollup_rows_and_return_export_paths() {
    let db = file_db("rollup-purge-paths");
    seed_folder(&db, "f1", "Secret");
    seed_note(&db, "m1", "note", Some("f1"));
    let seed_rollup = |db: &Db| {
        db.upsert_memory_rollup("entity:e1", "synth", "h", "2026-07-09T12:00:00Z")
            .unwrap();
        db.set_memory_rollup_exported("entity:e1", "/vault/brain/memory/entity-e1.md")
            .unwrap();
    };

    // lock_folder / move-into-locked chain.
    seed_rollup(&db);
    let paths = db.purge_chunks_for_meetings(&["m1".to_string()]).unwrap();
    assert_eq!(paths, vec!["/vault/brain/memory/entity-e1.md".to_string()]);
    assert!(db.list_memory_rollups().unwrap().is_empty());

    // relock chain.
    seed_rollup(&db);
    let mut folders = HashSet::new();
    folders.insert("f1".to_string());
    let paths = db.blank_sealed_notes_in_folders(&folders).unwrap();
    assert_eq!(paths.len(), 1);
    assert!(db.list_memory_rollups().unwrap().is_empty());

    // Startup reconcile with NO locked folder ⇒ rollups SURVIVE (nothing sealed, no leak).
    seed_rollup(&db);
    let (_, paths, _) = db.reblank_locked_folders_at_rest().unwrap();
    assert!(paths.is_empty());
    assert_eq!(
        db.list_memory_rollups().unwrap().len(),
        1,
        "an open-only DB keeps its rollups across restarts"
    );

    // Startup reconcile WITH a locked folder ⇒ purged.
    db.set_folder_locked("f1", true, None).unwrap();
    let (_, paths, _) = db.reblank_locked_folders_at_rest().unwrap();
    assert_eq!(paths.len(), 1);
    assert!(db.list_memory_rollups().unwrap().is_empty());

    // delete_meeting.
    seed_rollup(&db);
    let paths = db.delete_meeting("m1").unwrap();
    assert_eq!(paths.len(), 1);
    assert!(db.list_memory_rollups().unwrap().is_empty());
}

// ── Voiceprints: at-rest storage + LOCK invariants (mirror the user_facts tests exactly) ──────

/// A voiceprint stored via `insert_voiceprint` round-trips byte-exact through the BLOB and reads
/// back through the gated reader with its embedding, cluster index, and NULL label intact.
#[test]
fn voiceprint_round_trips_through_gated_reader() {
    let db = file_db("voiceprint-round-trip");
    seed_note(&db, "m1", "note", None);
    let emb = vec![0.1f32, -0.2, 0.3, 0.4, -0.5];
    db.insert_voiceprint("vp1", "m1", 0, None, &emb, "2026-07-01T00:00:00Z")
        .unwrap();

    let got = db.list_voiceprints_visible(&HashSet::new()).unwrap();
    assert_eq!(
        got.len(),
        1,
        "the voiceprint is visible for an open meeting"
    );
    assert_eq!(got[0].id, "vp1");
    assert_eq!(got[0].meeting_id, "m1");
    assert_eq!(got[0].cluster_index, 0);
    assert_eq!(got[0].dim, emb.len() as i64);
    assert!(got[0].label.is_none(), "label is NULL until enrolled");
    assert_eq!(
        got[0].embedding, emb,
        "embedding round-trips byte-exact through the BLOB"
    );
}

/// GATE: a voiceprint whose source meeting is SEALED (its folder locked, not session-unlocked)
/// must NOT surface from `list_voiceprints_visible` — a voice biometric of a locked speaker stays
/// invisible. RED-before-GREEN: with an ungated SELECT the row would surface while sealed.
#[test]
fn list_voiceprints_visible_excludes_sealed_meeting() {
    let db = file_db("voiceprint-gate");
    seed_folder(&db, "f-lock", "Secret");
    seed_note(&db, "secret1", "private", Some("f-lock"));
    db.insert_voiceprint(
        "vp1",
        "secret1",
        1,
        None,
        &[0.5f32, 0.5, 0.5, 0.5],
        "2026-07-01T00:00:00Z",
    )
    .unwrap();

    assert_eq!(
        db.list_voiceprints_visible(&HashSet::new()).unwrap().len(),
        1
    );
    db.set_folder_locked("f-lock", true, None).unwrap();
    assert!(
        db.list_voiceprints_visible(&HashSet::new())
            .unwrap()
            .is_empty(),
        "a sealed-not-unlocked meeting's voiceprint must not surface (gate violation)"
    );
    let mut unlocked = HashSet::new();
    unlocked.insert("f-lock".to_string());
    assert_eq!(
        db.list_voiceprints_visible(&unlocked).unwrap().len(),
        1,
        "the voiceprint reappears once the folder is session-unlocked"
    );
}

/// PURGE-ON-SEAL (DB layer): the same atomic seal tx that purges user facts also DELETES the
/// meeting's voiceprints (purge_speaker_voiceprints_tx). RED-before-GREEN: without the purge call
/// the voiceprint row survives the seal at rest.
#[test]
fn seal_purges_voiceprints() {
    let db = file_db("voiceprint-purge");
    seed_note(&db, "m1", "note", None);
    db.insert_voiceprint(
        "vp1",
        "m1",
        0,
        None,
        &[0.1f32, 0.2, 0.3],
        "2026-07-01T00:00:00Z",
    )
    .unwrap();
    // Present before the seal (visible for the open meeting).
    assert_eq!(
        db.list_voiceprints_visible(&HashSet::new()).unwrap().len(),
        1
    );
    // The seal purge (chunks + corrections + assistant interactions + facts + user facts +
    // VOICEPRINTS) in one tx.
    db.purge_chunks_for_meetings(&["m1".to_string()]).unwrap();
    assert!(
        db.list_voiceprints_visible(&HashSet::new())
            .unwrap()
            .is_empty(),
        "voiceprints must be purged on seal (drop-on-seal, like user facts)"
    );
}

/// At-rest reconcile (crash-while-unlocked recovery): `reblank_locked_folders_at_rest` purges the
/// voiceprints of every meeting in a LOCKED folder in the same reconciliation tx. RED-before-GREEN:
/// without the reconcile DELETE a voiceprint re-derived while unlocked would survive a restart.
#[test]
fn reconcile_purges_voiceprints_in_locked_folder() {
    let db = file_db("voiceprint-reconcile");
    seed_folder(&db, "f-lock", "Secret");
    seed_note(&db, "secret1", "private", Some("f-lock"));
    db.set_folder_locked("f-lock", true, None).unwrap();
    // Simulate a crash-while-unlocked leftover: a voiceprint persisted against a since-locked
    // meeting (the folder is locked at rest, so this row must not survive the reconcile).
    db.insert_voiceprint(
        "vp1",
        "secret1",
        0,
        None,
        &[0.9f32, 0.1],
        "2026-07-01T00:00:00Z",
    )
    .unwrap();

    db.reblank_locked_folders_at_rest().unwrap();
    // Even with the folder session-unlocked, the row is GONE (reconcile deleted it at rest).
    let mut unlocked = HashSet::new();
    unlocked.insert("f-lock".to_string());
    assert!(
        db.list_voiceprints_visible(&unlocked).unwrap().is_empty(),
        "the at-rest reconcile must purge a locked folder's voiceprints"
    );
}

/// delete_meeting cascades to speaker_voiceprints (FK ON DELETE CASCADE).
#[test]
fn delete_meeting_cascades_to_voiceprints() {
    let db = file_db("voiceprint-cascade");
    seed_note(&db, "m1", "note", None);
    db.insert_voiceprint("vp1", "m1", 0, None, &[0.1f32, 0.2], "2026-07-01T00:00:00Z")
        .unwrap();
    db.delete_meeting("m1").unwrap();
    assert!(
        db.list_voiceprints_visible(&HashSet::new())
            .unwrap()
            .is_empty(),
        "FK CASCADE drops voiceprints"
    );
}

/// ENROLL (Phase 2): binding a person label to a cluster's voiceprint sets `label` on exactly
/// that (meeting, cluster) row, leaves others untouched, and is idempotent (overwrite).
#[test]
fn set_voiceprint_label_for_cluster_enrolls_one_row() {
    let db = file_db("voiceprint-enroll");
    seed_note(&db, "m1", "note", None);
    db.insert_voiceprint("vp0", "m1", 0, None, &[1.0f32, 0.0], "2026-07-01T00:00:00Z")
        .unwrap();
    db.insert_voiceprint("vp1", "m1", 1, None, &[0.0f32, 1.0], "2026-07-01T00:00:00Z")
        .unwrap();

    let n = db
        .set_voiceprint_label_for_cluster("m1", 0, "Sarah")
        .unwrap();
    assert_eq!(n, 1, "exactly the cluster-0 row is labeled");

    let got = db.list_voiceprints_visible(&HashSet::new()).unwrap();
    let c0 = got.iter().find(|v| v.cluster_index == 0).unwrap();
    let c1 = got.iter().find(|v| v.cluster_index == 1).unwrap();
    assert_eq!(c0.label.as_deref(), Some("Sarah"), "enroll bound the label");
    assert!(c1.label.is_none(), "the other cluster is untouched");

    // Idempotent overwrite.
    assert_eq!(
        db.set_voiceprint_label_for_cluster("m1", 0, "Sara")
            .unwrap(),
        1
    );
    let got2 = db.list_voiceprints_visible(&HashSet::new()).unwrap();
    assert_eq!(
        got2.iter()
            .find(|v| v.cluster_index == 0)
            .unwrap()
            .label
            .as_deref(),
        Some("Sara")
    );

    // No voiceprint for that cluster → no-op (0 rows), never an error (pre-opt-in recordings).
    assert_eq!(
        db.set_voiceprint_label_for_cluster("m1", 9, "Nobody")
            .unwrap(),
        0
    );
}

/// FORGET one voiceprint by id removes exactly that row; deleting a missing id is a no-op.
#[test]
fn delete_voiceprint_removes_one_row() {
    let db = file_db("voiceprint-forget-one");
    seed_note(&db, "m1", "note", None);
    db.insert_voiceprint("vp0", "m1", 0, None, &[1.0f32, 0.0], "2026-07-01T00:00:00Z")
        .unwrap();
    db.insert_voiceprint("vp1", "m1", 1, None, &[0.0f32, 1.0], "2026-07-01T00:00:00Z")
        .unwrap();

    assert!(db.delete_voiceprint("vp0").unwrap(), "removed");
    let got = db.list_voiceprints_visible(&HashSet::new()).unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].id, "vp1", "only the requested row is gone");
    assert!(
        !db.delete_voiceprint("missing").unwrap(),
        "no-op on a missing id"
    );
}

/// CLEAR removes every voiceprint (the "forget all captured voices" affordance).
#[test]
fn clear_voiceprints_removes_all() {
    let db = file_db("voiceprint-clear");
    seed_note(&db, "m1", "note", None);
    seed_note(&db, "m2", "note2", None);
    db.insert_voiceprint("vp0", "m1", 0, None, &[1.0f32, 0.0], "2026-07-01T00:00:00Z")
        .unwrap();
    db.insert_voiceprint("vp1", "m2", 0, None, &[0.0f32, 1.0], "2026-07-01T00:00:00Z")
        .unwrap();
    assert_eq!(db.clear_voiceprints().unwrap(), 2, "both rows cleared");
    assert!(db
        .list_voiceprints_visible(&HashSet::new())
        .unwrap()
        .is_empty());
}

/// Helper for the scoped-Ask-purge oracle: how many durable Ask conversation rows survive AT REST
/// (not "are visible" — the row count, so a merely-hidden row still counts as surviving).
fn ask_rows_at_rest(db: &Db) -> i64 {
    db.lock()
        .query_row("SELECT COUNT(*) FROM ask_conversations", [], |r| r.get(0))
        .unwrap()
}

/// RELEASE BLOCKER (2.0) — durable Ask history must not be globally erased by a session-visibility
/// WITHDRAWAL. `blank_sealed_notes_in_folders` (relock / relock-all — the app-quit, window-close and
/// screen-share auto-relock hooks) and `reblank_locked_folders_at_rest` (EVERY launch, guarded only
/// on "some folder is locked") used to run the GLOBAL
/// `purge_all_ask_conversations_tx`, whose predicate `provenance_mode = 'globalDerived'` matches
/// every row the schema CHECK allows — a total wipe for any user with one locked folder.
///
/// Both directions, one oracle:
///   * LEAK leg (fail-closed CONTROL): a conversation whose conservative dependency snapshot
///     contains the folder being sealed MUST be destroyed at rest by BOTH paths.
///   * LOSS leg: a conversation that only ever saw an unrelated, still-open folder MUST survive
///     BOTH paths and MUST STAY READABLE afterwards (a surviving-but-invisible row — e.g. one
///     orphaned by a bumped `visibility_generation` — is the same data loss to the user).
#[test]
fn ask_history_purge_on_visibility_withdrawal_is_scoped_to_the_sealed_folders() {
    let db = file_db("ask-scoped-purge");
    seed_folder(&db, "f-open", "Open");
    seed_folder(&db, "f-sealed", "Sealed");
    // `f-sealed` is locked AT REST and session-unlocked right now — the exact state both paths
    // reconcile (a relock withdraws the session unlock; the startup reconcile re-asserts at rest).
    db.lock()
        .execute("UPDATE folders SET locked = 1 WHERE id = 'f-sealed'", [])
        .unwrap();
    let session_unlocked: HashSet<String> = ["f-sealed".to_string()].into_iter().collect();

    let unrelated = db
        .persist_ask_exchange(
            &AskConversationScope::Vault,
            None,
            "What did we decide about the open folder?",
            "Derived only from content that stays visible.",
            &[],
            &[],
            &[],
            &["f-open".to_string()],
            "2026-08-26T09:00:00Z",
        )
        .unwrap();
    let derived = db
        .persist_ask_exchange(
            &AskConversationScope::Vault,
            None,
            "What did we decide in the private folder?",
            "Derived from content that is about to be sealed.",
            &[],
            &[],
            &[],
            &["f-open".to_string(), "f-sealed".to_string()],
            "2026-08-26T09:01:00Z",
        )
        .unwrap();

    let visible_now = db
        .list_ask_conversation_ids(&AskConversationScope::Vault, &session_unlocked)
        .unwrap();
    assert!(
        visible_now.contains(&unrelated) && visible_now.contains(&derived),
        "both threads start readable while the sealed folder is session-unlocked"
    );
    assert_eq!(ask_rows_at_rest(&db), 2);

    // ── RELOCK (app quit / window close / screen-share auto-relock) ──────────────────────────────
    db.blank_sealed_notes_in_folders(&session_unlocked).unwrap();
    assert_eq!(
        ask_rows_at_rest(&db),
        1,
        "the thread derived from the sealed folder must be DESTROYED at rest by the relock"
    );
    let after_relock = db
        .list_ask_conversation_ids(&AskConversationScope::Vault, &HashSet::new())
        .unwrap();
    assert_eq!(
        after_relock,
        vec![unrelated.clone()],
        "the unrelated thread must survive the relock AND stay readable"
    );

    // ── STARTUP RECONCILE (runs at EVERY launch while any folder is locked) ──────────────────────
    db.reblank_locked_folders_at_rest().unwrap();
    assert_eq!(
        ask_rows_at_rest(&db),
        1,
        "the startup reconcile must not erase an unrelated thread"
    );
    let after_launch = db
        .list_ask_conversation_ids(&AskConversationScope::Vault, &HashSet::new())
        .unwrap();
    assert_eq!(
        after_launch,
        vec![unrelated.clone()],
        "durable Ask history must survive relaunch when nothing it depended on was sealed"
    );

    // ── LEAK leg for the startup path on its own (crash-while-unlocked recovery) ─────────────────
    let crashed = db
        .persist_ask_exchange(
            &AskConversationScope::Vault,
            None,
            "What is in the private folder?",
            "Derived during a session that crashed before relocking.",
            &[],
            &[],
            &[],
            &["f-open".to_string(), "f-sealed".to_string()],
            "2026-08-26T09:02:00Z",
        )
        .unwrap();
    assert_eq!(ask_rows_at_rest(&db), 2);
    db.reblank_locked_folders_at_rest().unwrap();
    assert_eq!(
        ask_rows_at_rest(&db),
        1,
        "startup reconcile must DESTROY a thread that depended on a folder locked at rest"
    );
    assert!(
        !db.list_ask_conversation_ids(&AskConversationScope::Vault, &HashSet::new())
            .unwrap()
            .contains(&crashed),
        "the crash-derived thread is gone, not merely hidden"
    );
}

// ── The fact ledger survives a lock/unlock cycle ────────────────────────────────────────────────

/// THE ROUND TRIP. A seal still DELETES the meeting's facts, user facts and supersessions — their
/// subject/predicate/object are plaintext derived from the meeting, and leaving them readable at
/// rest would defeat the lock. What was missing is the other half of the contract: the ciphertext
/// that lets an unlock put them back.
///
/// Re-extraction is not a recovery. It needs a provider call, and it cannot reconstruct the
/// bitemporal history — `valid_to` records when a fact STOPPED being true, and nothing in the
/// current note text says that. Knowledge diff, dossiers and the entity timeline are built on
/// exactly that history, so this test asserts the CLOSED fact comes back closed.
#[test]
fn a_sealed_fact_ledger_round_trips_through_lock_and_unlock() {
    let db = file_db("fact-ledger-round-trip");
    seed_folder(&db, "f-secret", "Secret");
    seed_note(&db, "m1", "Atlas shipped", None);
    db.set_note_folder("m1", Some("f-secret")).unwrap();
    let atlas = db.upsert_entity("Atlas", EntityKind::Project).unwrap();
    db.add_mention(&atlas, "m1").unwrap();

    // Two assertions about the same predicate, the second superseding the first: the reconcile
    // CLOSES the old row (`valid_to`) instead of deleting it. That closure is the history.
    db.apply_fact_ops(&[add_op(
        &atlas,
        "status",
        "in progress",
        "2026-06-01T00:00:00Z",
        "m1",
    )])
    .unwrap();
    db.apply_fact_ops(&[add_op(
        &atlas,
        "status",
        "shipped",
        "2026-07-01T00:00:00Z",
        "m1",
    )])
    .unwrap();
    // Close the earlier assertion, which is what the reconcile does when a later meeting
    // contradicts an open fact: invalidate-not-delete, so the row stays with a `valid_to`.
    let open_id = db
        .facts_for_entities(std::slice::from_ref(&atlas))
        .unwrap()
        .into_iter()
        .find(|f| f.object == "in progress")
        .expect("the superseded assertion")
        .id;
    db.apply_fact_ops(&[FactOp::Invalidate {
        id: open_id,
        valid_to: "2026-07-01T00:00:00Z".to_string(),
    }])
    .unwrap();

    let before = db.facts_for_entities(std::slice::from_ref(&atlas)).unwrap();
    assert!(
        before.len() >= 2,
        "expected the superseded row to survive as history, got {before:?}"
    );
    assert!(
        before.iter().any(|f| f.valid_to.is_some()),
        "the point of the ledger is the CLOSED row — got {before:?}"
    );

    let ck = [7u8; 32];
    crate::commands::seal_fact_ledger_for_meeting(&db, "f-secret", "m1", &ck).unwrap();

    // The seal's own purge, unchanged: the rows leave the database.
    db.purge_chunks_for_meetings(&["m1".to_string()]).unwrap();
    assert!(
        db.facts_for_entities(std::slice::from_ref(&atlas))
            .unwrap()
            .is_empty(),
        "the at-rest guarantee is unchanged — a sealed meeting's facts must not be readable"
    );

    // Unlock: the ledger comes back exactly as it was, closure and all.
    crate::commands::restore_fact_ledger_for_meeting(&db, "f-secret", "m1", &ck).unwrap();
    let after = db.facts_for_entities(std::slice::from_ref(&atlas)).unwrap();

    let key = |f: &crate::facts::Fact| {
        (
            f.id.clone(),
            f.subject.clone(),
            f.predicate.clone(),
            f.object.clone(),
            f.valid_from.clone(),
            f.valid_to.clone(),
            f.recorded_at.clone(),
        )
    };
    let mut want: Vec<_> = before.iter().map(key).collect();
    let mut got: Vec<_> = after.iter().map(key).collect();
    want.sort();
    got.sort();
    assert_eq!(
        got, want,
        "every fact must return identical, including the valid_to that records when it stopped \
         being true — re-extraction could never reconstruct that"
    );
}

/// A blob sealed for one meeting must not open for another, and a wrong key must not open it at
/// all. The AAD binds folder AND meeting; without that, a ledger could be swapped between meetings
/// in the same folder and restore somebody else's facts onto this meeting.
#[test]
fn a_sealed_fact_ledger_is_bound_to_its_folder_and_meeting() {
    let db = file_db("fact-ledger-aad");
    seed_folder(&db, "f-secret", "Secret");
    seed_note(&db, "m1", "Atlas", None);
    seed_note(&db, "m2", "Other", None);
    db.set_note_folder("m1", Some("f-secret")).unwrap();
    db.set_note_folder("m2", Some("f-secret")).unwrap();
    let atlas = db.upsert_entity("Atlas", EntityKind::Project).unwrap();
    db.add_mention(&atlas, "m1").unwrap();
    db.apply_fact_ops(&[add_op(
        &atlas,
        "status",
        "shipped",
        "2026-06-01T00:00:00Z",
        "m1",
    )])
    .unwrap();

    let ck = [7u8; 32];
    crate::commands::seal_fact_ledger_for_meeting(&db, "f-secret", "m1", &ck).unwrap();
    let blob = db.fact_ledger_blob("m1").unwrap().expect("sealed");

    // The same ciphertext, filed under a different meeting, must not open.
    db.seal_fact_ledger("m2", &blob).unwrap();
    assert!(
        crate::commands::restore_fact_ledger_for_meeting(&db, "f-secret", "m2", &ck)
            .is_err(),
        "a ledger must not open under another meeting's identity"
    );
    // Nor under a different key.
    assert!(
        crate::commands::restore_fact_ledger_for_meeting(&db, "f-secret", "m1", &[9u8; 32])
            .is_err(),
        "a ledger must not open under a different content key"
    );
}

/// The permanent unlock hands the rows back for good and drops the ciphertext — after the rows are
/// durably written, never before.
#[test]
fn removing_a_lock_restores_the_ledger_and_then_drops_its_ciphertext() {
    let db = file_db("fact-ledger-permanent");
    seed_folder(&db, "f-secret", "Secret");
    seed_note(&db, "m1", "Atlas", None);
    db.set_note_folder("m1", Some("f-secret")).unwrap();
    let atlas = db.upsert_entity("Atlas", EntityKind::Project).unwrap();
    db.add_mention(&atlas, "m1").unwrap();
    db.apply_fact_ops(&[add_op(
        &atlas,
        "status",
        "shipped",
        "2026-06-01T00:00:00Z",
        "m1",
    )])
    .unwrap();

    let ck = [7u8; 32];
    crate::commands::seal_fact_ledger_for_meeting(&db, "f-secret", "m1", &ck).unwrap();
    db.purge_chunks_for_meetings(&["m1".to_string()]).unwrap();

    crate::commands::restore_fact_ledger_for_meeting(&db, "f-secret", "m1", &ck).unwrap();
    assert_eq!(
        db.facts_for_entities(std::slice::from_ref(&atlas))
            .unwrap()
            .len(),
        1,
        "the rows are back"
    );
    assert!(
        db.fact_ledger_blob("m1").unwrap().is_some(),
        "the restore itself must NOT drop the ciphertext — an interruption here has to leave \
         something recoverable"
    );

    // The ciphertext retires in the ATOMIC folder-open commit, with every other blob. Driving that
    // real function is the point: an earlier version of this test passed a `clear_blob` flag no
    // production caller ever set, so the actual retire was pinned by nothing.
    db.commit_folder_permanent_unlock("f-secret").unwrap();
    assert!(
        db.fact_ledger_blob("m1").unwrap().is_none(),
        "and after the commit the rows are the only copy again"
    );
}

/// RE-SEALING must not lose what an unlocked session learned. A relock encrypts the CURRENT rows,
/// so a fact extracted while the folder was open has to end up in the new ciphertext — a stale blob
/// would silently drop it.
#[test]
fn a_relock_seals_the_facts_the_unlocked_session_added() {
    let db = file_db("fact-ledger-reseal");
    seed_folder(&db, "f-secret", "Secret");
    seed_note(&db, "m1", "Atlas", None);
    db.set_note_folder("m1", Some("f-secret")).unwrap();
    let atlas = db.upsert_entity("Atlas", EntityKind::Project).unwrap();
    db.add_mention(&atlas, "m1").unwrap();
    db.apply_fact_ops(&[add_op(
        &atlas,
        "status",
        "shipped",
        "2026-06-01T00:00:00Z",
        "m1",
    )])
    .unwrap();

    let ck = [7u8; 32];
    crate::commands::seal_fact_ledger_for_meeting(&db, "f-secret", "m1", &ck).unwrap();
    db.purge_chunks_for_meetings(&["m1".to_string()]).unwrap();
    crate::commands::restore_fact_ledger_for_meeting(&db, "f-secret", "m1", &ck).unwrap();

    // The unlocked session learns one more thing.
    db.apply_fact_ops(&[add_op(
        &atlas,
        "owner",
        "Ada",
        "2026-08-01T00:00:00Z",
        "m1",
    )])
    .unwrap();

    // Relock: re-seal, then purge.
    crate::commands::seal_fact_ledger_for_meeting(&db, "f-secret", "m1", &ck).unwrap();
    db.purge_chunks_for_meetings(&["m1".to_string()]).unwrap();
    crate::commands::restore_fact_ledger_for_meeting(&db, "f-secret", "m1", &ck).unwrap();

    let after = db.facts_for_entities(std::slice::from_ref(&atlas)).unwrap();
    assert!(
        after.iter().any(|f| f.predicate == "owner"),
        "the fact the unlocked session added must survive the relock — got {after:?}"
    );
    assert!(
        after.iter().any(|f| f.predicate == "status"),
        "and so must the one that was already sealed — got {after:?}"
    );
}

/// THE WIRING, not just the helpers. The four tests above call `seal_fact_ledger_for_meeting`
/// directly, so deleting the line that calls it from the real seal would leave every one of them
/// green — a lock-security review made exactly that point. This one goes through
/// `seal_folder_extras`, the function `lock_folder`, the move-into-locked path and the startup
/// repair all share, and it carries the two families the earlier tests never exercised: a USER fact
/// and a SUPERSESSION. A slip in either of those INSERTs is invisible to a test that only checks
/// entity facts.
#[test]
fn the_real_folder_seal_captures_user_facts_and_supersessions_too() {
    let db = file_db("fact-ledger-wiring");
    seed_folder(&db, "f-secret", "Secret");
    seed_note(&db, "m1", "Atlas", None);
    seed_note(&db, "m2", "Atlas again", None);
    db.set_note_folder("m1", Some("f-secret")).unwrap();
    db.set_note_folder("m2", Some("f-secret")).unwrap();
    let atlas = db.upsert_entity("Atlas", EntityKind::Project).unwrap();
    db.add_mention(&atlas, "m1").unwrap();
    db.apply_fact_ops(&[add_op(
        &atlas,
        "status",
        "shipped",
        "2026-06-01T00:00:00Z",
        "m1",
    )])
    .unwrap();
    db.apply_user_fact_ops(&[user_add_op(
        "prefer",
        "Polish replies",
        "2026-06-01T00:00:00Z",
        "m1",
    )])
    .unwrap();
    // Both anchors sit in the SAME folder, so unlocking it makes the row legitimately restorable.
    db.record_supersessions(&[supersession_row("s1", "m2", "m1")])
        .unwrap();

    let ck = [7u8; 32];
    crate::commands::seal_folder_extras(&db, "f-secret", &ck).unwrap();
    db.purge_chunks_for_meetings(&["m1".to_string(), "m2".to_string()])
        .unwrap();
    assert!(db.user_facts_all().unwrap().is_empty(), "purged on seal");
    assert!(db.unapplied_supersessions_for("m2").unwrap().is_empty());

    for mid in ["m1", "m2"] {
        crate::commands::restore_fact_ledger_for_meeting(&db, "f-secret", mid, &ck).unwrap();
    }
    assert_eq!(
        db.facts_for_entities(std::slice::from_ref(&atlas))
            .unwrap()
            .len(),
        1,
        "the entity fact came back"
    );
    assert_eq!(
        db.user_facts_all().unwrap().len(),
        1,
        "so did the USER fact — a separate table with its own INSERT"
    );
    assert_eq!(
        db.unapplied_supersessions_for("m2").unwrap().len(),
        1,
        "and the supersession, whose eleven columns nothing else checks"
    );
}

/// A DISCARDED seal must leave no ciphertext behind, or the next lock cannot succeed.
///
/// `discard_folder_seal` rolls a folder back to open. It purges every derived family but used not to
/// touch the sealed ledger, and a re-lock mints a NEW content key — so the seal would find a blob
/// from the discarded key, fail to authenticate it, and abort AFTER `locked=1` was already durable.
/// The startup repair then hit the same wall on every launch. Found by a lock-security review; RED
/// before `sealed_fact_ledgers` joined the discard purge.
#[test]
fn a_discarded_seal_leaves_no_ledger_ciphertext_to_break_the_next_lock() {
    let db = file_db("fact-ledger-discard");
    seed_folder(&db, "f-secret", "Secret");
    seed_note(&db, "m1", "Atlas", None);
    db.set_note_folder("m1", Some("f-secret")).unwrap();
    let atlas = db.upsert_entity("Atlas", EntityKind::Project).unwrap();
    db.add_mention(&atlas, "m1").unwrap();
    db.apply_fact_ops(&[add_op(
        &atlas,
        "status",
        "shipped",
        "2026-06-01T00:00:00Z",
        "m1",
    )])
    .unwrap();

    let first_ck = [7u8; 32];
    crate::commands::seal_fact_ledger_for_meeting(&db, "f-secret", "m1", &first_ck).unwrap();
    assert!(db.fact_ledger_blob("m1").unwrap().is_some());

    db.discard_folder_seal("f-secret").unwrap();
    assert!(
        db.fact_ledger_blob("m1").unwrap().is_none(),
        "a discarded seal must not leave a ciphertext only the abandoned key can open"
    );

    // The re-lock mints a different key. It must succeed.
    let second_ck = [9u8; 32];
    crate::commands::seal_fact_ledger_for_meeting(&db, "f-secret", "m1", &second_ck)
        .expect("re-locking a discarded folder must not fail on a stale ledger ciphertext");
}

/// A supersession spans two meetings. Restoring one of them must NOT put the row back while the
/// OTHER one's folder is still sealed — its old/new values and note pre-images are that meeting's
/// plaintext, and purge-on-seal exists precisely to keep them out of a live table.
#[test]
fn a_cross_folder_supersession_waits_for_its_other_anchor() {
    let db = file_db("fact-ledger-cross-folder");
    seed_folder(&db, "f-open", "Open");
    seed_folder(&db, "f-secret", "Secret");
    seed_note(&db, "m-open", "Open side", None);
    seed_note(&db, "m-secret", "Secret side", None);
    db.set_note_folder("m-open", Some("f-open")).unwrap();
    db.set_note_folder("m-secret", Some("f-secret")).unwrap();
    db.record_supersessions(&[supersession_row("s1", "m-open", "m-secret")])
        .unwrap();

    let ck = [7u8; 32];
    crate::commands::seal_fact_ledger_for_meeting(&db, "f-open", "m-open", &ck).unwrap();

    // The OTHER anchor is SEALED FOR REAL — its note markdown replaced by ciphertext, not merely
    // filed in a locked folder. That distinction is the predicate's whole point: during a session
    // unlock the markdown is back, and the row is legitimately restorable again.
    db.seal_note("m-secret", "claude_code", &[1u8, 2, 3]).unwrap();
    db.set_folder_locked("f-secret", true, None).unwrap();
    let mut sealed = HashSet::new();
    sealed.insert("f-secret".to_string());
    db.blank_sealed_notes_in_folders(&sealed).unwrap();
    assert!(db.unapplied_supersessions_for("m-open").unwrap().is_empty());

    // Unlocking the OPEN side must not resurrect a row carrying the sealed side's content.
    crate::commands::restore_fact_ledger_for_meeting(&db, "f-open", "m-open", &ck).unwrap();
    assert!(
        db.unapplied_supersessions_for("m-open").unwrap().is_empty(),
        "a supersession must wait until BOTH of its meetings are readable"
    );
}

/// A RE-SEAL MUST NEVER SHRINK A CIPHERTEXT.
///
/// This is the loss an adversarial review reproduced, and it was caused by the cross-folder skip
/// directly above. A supersession spans meetings in two folders. Lock both; the rows are purged and
/// only A's ciphertext still holds the row. Unlock A alone: the restore correctly SKIPS the row,
/// because B is still sealed and the row carries B's plaintext. Relock A: the re-seal reads the live
/// rows, which no longer contain it, and overwrites the one copy that did.
///
/// Nothing surfaced it. The row is not leaked, not logged, not counted anywhere — it is simply gone
/// from both ciphertexts, and the next time both folders are open it does not come back.
#[test]
fn a_re_seal_carries_forward_what_the_restore_had_to_skip() {
    let db = file_db("fact-ledger-no-shrink");
    seed_folder(&db, "f-a", "A");
    seed_folder(&db, "f-b", "B");
    seed_note(&db, "m-a", "Side A", None);
    seed_note(&db, "m-b", "Side B", None);
    db.set_note_folder("m-a", Some("f-a")).unwrap();
    db.set_note_folder("m-b", Some("f-b")).unwrap();
    // A fact of its own, so the re-seal does not take the empty-ledger early return (which keeps
    // the old blob by accident and would hide the hazard).
    let atlas = db.upsert_entity("Atlas", EntityKind::Project).unwrap();
    db.add_mention(&atlas, "m-a").unwrap();
    db.apply_fact_ops(&[add_op(
        &atlas,
        "status",
        "shipped",
        "2026-06-01T00:00:00Z",
        "m-a",
    )])
    .unwrap();
    db.record_supersessions(&[supersession_row("s1", "m-a", "m-b")])
        .unwrap();

    let ck_a = [7u8; 32];
    crate::commands::seal_fact_ledger_for_meeting(&db, "f-a", "m-a", &ck_a).unwrap();
    db.purge_chunks_for_meetings(&["m-a".to_string()]).unwrap();

    // B seals for real too, so it reads as sealed-at-rest.
    db.seal_note("m-b", "claude_code", &[1u8, 2, 3]).unwrap();
    db.set_folder_locked("f-b", true, None).unwrap();

    // Unlock A alone: the row is skipped, correctly — B's content must not come back yet.
    crate::commands::restore_fact_ledger_for_meeting(&db, "f-a", "m-a", &ck_a).unwrap();
    assert!(
        db.unapplied_supersessions_for("m-a").unwrap().is_empty(),
        "the skip itself is right — B is still sealed"
    );

    // Relock A. The re-seal reads live rows that no longer hold the supersession.
    crate::commands::seal_fact_ledger_for_meeting(&db, "f-a", "m-a", &ck_a).unwrap();
    db.purge_chunks_for_meetings(&["m-a".to_string()]).unwrap();

    // Now B opens. The row must still be recoverable — it was never anyone's to delete.
    db.restore_note_markdown("m-b", "claude_code", "Side B")
        .unwrap();
    db.set_folder_locked("f-b", false, None).unwrap();
    crate::commands::restore_fact_ledger_for_meeting(&db, "f-a", "m-a", &ck_a).unwrap();
    assert_eq!(
        db.unapplied_supersessions_for("m-a").unwrap().len(),
        1,
        "a re-seal must carry forward what the restore skipped — otherwise unlocking one folder and \
         locking it again silently destroys a row spanning two"
    );
    assert_eq!(
        db.facts_for_entities(std::slice::from_ref(&atlas))
            .unwrap()
            .len(),
        1,
        "and the meeting's own fact is still there"
    );
}

/// The ciphertext is bound to its FOLDER as well as its meeting. Per-folder keys already make a
/// cross-folder swap fail, so this is defence in depth — but the earlier binding test could not
/// tell the two apart, and a mutation that dropped `folder_id` from the AAD survived it.
#[test]
fn a_sealed_fact_ledger_does_not_open_under_another_folders_identity() {
    let db = file_db("fact-ledger-folder-aad");
    seed_folder(&db, "f-a", "A");
    seed_note(&db, "m1", "Atlas", None);
    db.set_note_folder("m1", Some("f-a")).unwrap();
    let atlas = db.upsert_entity("Atlas", EntityKind::Project).unwrap();
    db.add_mention(&atlas, "m1").unwrap();
    db.apply_fact_ops(&[add_op(
        &atlas,
        "status",
        "shipped",
        "2026-06-01T00:00:00Z",
        "m1",
    )])
    .unwrap();

    let ck = [7u8; 32];
    crate::commands::seal_fact_ledger_for_meeting(&db, "f-a", "m1", &ck).unwrap();
    assert!(
        crate::commands::restore_fact_ledger_for_meeting(&db, "f-b", "m1", &ck).is_err(),
        "the same key and meeting under a different folder must not open the ledger"
    );
}

/// `facts.importance` rides along. It is not on `crate::facts::Fact`, so it had to be carried
/// separately — and a mutation dropping it passed every other test.
#[test]
fn the_sealed_ledger_carries_fact_importance() {
    let db = file_db("fact-ledger-importance");
    seed_folder(&db, "f-secret", "Secret");
    seed_note(&db, "m1", "Atlas", None);
    db.set_note_folder("m1", Some("f-secret")).unwrap();
    let atlas = db.upsert_entity("Atlas", EntityKind::Project).unwrap();
    db.add_mention(&atlas, "m1").unwrap();
    db.apply_fact_ops(&[add_op(
        &atlas,
        "status",
        "shipped",
        "2026-06-01T00:00:00Z",
        "m1",
    )])
    .unwrap();
    let fact_id = db
        .facts_for_entities(std::slice::from_ref(&atlas))
        .unwrap()
        .remove(0)
        .id;
    db.set_fact_importance(&fact_id, 9.0).unwrap();

    let ck = [7u8; 32];
    crate::commands::seal_fact_ledger_for_meeting(&db, "f-secret", "m1", &ck).unwrap();
    db.purge_chunks_for_meetings(&["m1".to_string()]).unwrap();
    crate::commands::restore_fact_ledger_for_meeting(&db, "f-secret", "m1", &ck).unwrap();

    assert_eq!(
        db.fact_importance_map().unwrap().get(&fact_id).copied(),
        Some(9.0),
        "importance must survive — otherwise every restored fact is silently re-assessed"
    );
}
