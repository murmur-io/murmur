use super::*;
use crate::storage::models::{Meeting, MeetingStatus, NoteRecord};
use crate::storage::Db;
use std::collections::HashSet;

const TEST_DEK: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn tmp_db() -> Db {
    let p = crate::storage::db::unique_temp_path("murmur-dossier", "sqlite");
    Db::open_with_key(&p, TEST_DEK).unwrap()
}

fn seed_note(db: &Db, id: &str, title: &str, markdown: &str) {
    db.insert_meeting(&Meeting {
        id: id.into(),
        started_at: "2026-06-26T09:00:00Z".into(),
        ended_at: None,
        title: Some(title.into()),
        duration_s: 60,
        audio_path: None,
        status: MeetingStatus::Summarized,
        folder_id: None,
    })
    .unwrap();
    db.upsert_note(&NoteRecord {
        meeting_id: id.into(),
        provider_id: "claude_code".into(),
        markdown: markdown.into(),
        created_at: "2026-06-26T09:05:00Z".into(),
        exported_path: None,
        model_requested: None,
        model_served: None,
        gateway_host: None,
    })
    .unwrap();
}

/// Join org-1 for this session (mirrors `tools::tests::seed_org`).
fn seed_org(db: &Db) {
    db.upsert_org_state(&crate::storage::OrgState {
        org_id: "org-1".to_string(),
        name: "Acme".to_string(),
        role: "member".to_string(),
        joined_at: "2026-07-10T00:00:00Z".to_string(),
        consented: true,
        last_seq: 0,
        generation: 1,
        context_enabled: true,
    })
    .unwrap();
}

/// Ingest one org-brain item into the local replica (mirrors `tools::tests::ingest_org`).
fn ingest_org(db: &Db, item_id: &str, author: &str, title: &str, body: &str, sha: &[u8]) {
    db.upsert_org_item(
        item_id,
        "org-1",
        1,
        author,
        title,
        body,
        "2026-07-10T09:00:00Z",
        1,
        1,
        sha,
        None,
        None,
        Some(&crate::embed::StubEmbedder),
    )
    .unwrap();
}

fn empty_unlocked() -> HashSet<String> {
    HashSet::new()
}

/// RED-before-GREEN (B2, the load-bearing leak/write-safety test): an entity that exists ONLY in
/// the org brain — ZERO local `entities`/`entity_mentions`/`facts` rows for it at all — must now
/// resolve via the dossier prompt builder with `[org · author]`-cited content, AND the local
/// `entities`/`entity_mentions` tables (proxied via `list_entities_visible` — row count + summed
/// visible mention count) must be BYTE-IDENTICAL (same COUNT) before and after the call, proving
/// the org leg is READ-ONLY and never calls `build_and_persist_entities` / writes derived rows.
/// Pre-fix, an org-only entity had NO local row at all and `resolve_entity_id` returned `None`
/// unconditionally ⇒ `InvalidArg("no visible entity matching …")` — RED.
#[test]
fn org_only_entity_resolves_via_dossier_and_writes_nothing_to_local_tables() {
    let db = tmp_db();
    seed_org(&db);
    ingest_org(
        &db,
        "it-1",
        "anna",
        "Anna's roadmap",
        "Project Apollo: the apollo migration ships friday, owned by anna",
        &[3u8; 32],
    );
    let unlocked = empty_unlocked();
    let cfg = AppConfig {
        semantic_search_enabled: false,
        ..AppConfig::default()
    };

    // BASELINE row counts — a fresh DB with an org item but NO local entities/mentions at all.
    let entities_before = db.list_entities_visible(&unlocked).unwrap();
    let count_before = entities_before.len();
    let mentions_before: i64 = entities_before.iter().map(|e| e.mention_count).sum();

    // "Project Apollo" is ORG-ONLY — no local meeting/entity ever mentions it.
    let (system, user, has_org_context) =
        build_entity_dossier_prompt(&db, "Project Apollo", &unlocked, &cfg, "claude_code")
            .expect("an org-only entity must now resolve (RED on the pre-fix hard miss)");

    assert!(
        has_org_context,
        "has_org_context must be true when org content contributed"
    );
    assert!(
        user.contains("[org · anna]") && user.contains("apollo migration"),
        "the org-cited content must reach the synthesis USER prompt: {user}"
    );
    assert!(
        system.contains("[org ·"),
        "the synthesis SYSTEM prompt must instruct org-provenance attribution: {system}"
    );

    // THE LOAD-BEARING ASSERTION: local entities/entity_mentions rows are UNCHANGED — the org
    // leg never persisted anything derived from org content.
    let entities_after = db.list_entities_visible(&unlocked).unwrap();
    let count_after = entities_after.len();
    let mentions_after: i64 = entities_after.iter().map(|e| e.mention_count).sum();
    assert_eq!(
        count_before, count_after,
        "entities row count must be UNCHANGED by a read-only org-context dossier call"
    );
    assert_eq!(
        mentions_before, mentions_after,
        "entity_mentions row count (proxied via summed visible mention_count) must be UNCHANGED"
    );
    assert!(
            count_after == 0 && mentions_after == 0,
            "an org-only entity must create NO local entity/mention row at all: entities={count_after} mentions={mentions_after}"
        );
}

/// Companion: seed a REAL local entity (with its own mention + zero facts) so the baseline
/// tables are non-empty, then run a dossier call for a DIFFERENT org-only entity — the
/// pre-existing local entity's own facts (`list_facts_visible`) must stay untouched (still
/// empty), proving the org leg doesn't bleed into or mutate UNRELATED existing rows either.
#[test]
fn org_context_dossier_call_never_touches_an_unrelated_local_entitys_facts() {
    let db = tmp_db();
    seed_org(&db);
    ingest_org(
        &db,
        "it-2",
        "bob",
        "Bob's plan",
        "Project Zephyr: the zephyr rollout ships monday",
        &[7u8; 32],
    );
    seed_note(
        &db,
        "m1",
        "Atlas Kickoff",
        "We discussed Atlas migration and pricing.",
    );
    let entity_id = db
        .upsert_entity("Atlas", crate::storage::models::EntityKind::Project)
        .unwrap();
    db.add_mention(&entity_id, "m1").unwrap();

    let unlocked = empty_unlocked();
    let cfg = AppConfig {
        semantic_search_enabled: false,
        ..AppConfig::default()
    };
    let facts_before = db.list_facts_visible(&entity_id, &unlocked).unwrap().len();
    assert_eq!(
        facts_before, 0,
        "seed self-check: no facts yet for the local entity"
    );

    // Dossier call for a DIFFERENT, org-only entity.
    let (_, _, has_org_context) =
        build_entity_dossier_prompt(&db, "Project Zephyr", &unlocked, &cfg, "claude_code")
            .expect("org-only entity must resolve");
    assert!(has_org_context);

    // The pre-existing LOCAL entity's facts are untouched by the unrelated org-context call.
    let facts_after = db.list_facts_visible(&entity_id, &unlocked).unwrap().len();
    assert_eq!(
        facts_after, 0,
        "an unrelated local entity's facts must stay UNCHANGED by an org-context dossier call"
    );
    // And the local entity itself still resolves exactly as before (not clobbered/duplicated).
    let entities = db.list_entities_visible(&unlocked).unwrap();
    assert_eq!(
        entities.iter().filter(|e| e.name == "Atlas").count(),
        1,
        "the pre-existing local entity must not be duplicated or removed"
    );
}

/// A local-only entity (no org membership at all) keeps the EXACT pre-org behavior:
/// `has_org_context` is false and no org text reaches the prompt.
#[test]
fn local_only_entity_has_org_context_false_and_no_org_text_when_not_a_member() {
    let db = tmp_db();
    seed_note(
        &db,
        "m1",
        "Atlas Kickoff",
        "We discussed Atlas migration and pricing.",
    );
    let entity_id = db
        .upsert_entity("Atlas", crate::storage::models::EntityKind::Project)
        .unwrap();
    db.add_mention(&entity_id, "m1").unwrap();

    let unlocked = empty_unlocked();
    let cfg = AppConfig {
        semantic_search_enabled: false,
        ..AppConfig::default()
    };
    let (system, user, has_org_context) =
        build_entity_dossier_prompt(&db, "Atlas", &unlocked, &cfg, "claude_code").unwrap();
    assert!(
        !has_org_context,
        "no org membership ⇒ has_org_context must be false"
    );
    assert!(!user.contains("[org ·") && !system.contains("[org ·"));
}

/// Neither a local entity NOR any org context ⇒ the pre-org `InvalidArg` error semantics are
/// preserved exactly (genuinely unknown entity).
#[test]
fn unknown_entity_with_no_org_context_still_errors() {
    let db = tmp_db();
    let unlocked = empty_unlocked();
    let cfg = AppConfig::default();
    let err = build_entity_dossier_prompt(&db, "Nonexistent Thing", &unlocked, &cfg, "claude_code")
        .unwrap_err();
    assert!(matches!(err, AppError::InvalidArg(_)));
}
