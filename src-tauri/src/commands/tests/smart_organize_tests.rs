use super::*;
use std::sync::Arc;
const TEST_DEK: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
fn test_state(label: &str) -> (AppState, std::path::PathBuf) {
    let path = crate::storage::db::unique_temp_path(
        &format!("murmur-note-organize-state-{label}"),
        "sqlite",
    );
    let _ = std::fs::remove_file(&path);
    let db = Arc::new(crate::storage::db::Db::open_with_key(&path, TEST_DEK).unwrap());
    db.lock().execute("DELETE FROM folders", []).unwrap();
    let state = AppState {
        recorder: Mutex::new(None),
        recording_stop: Mutex::new(None),
        voice_listener: Mutex::new(None),
        voice_listener_lifecycle: Mutex::new(()),
        recording_starting: std::sync::atomic::AtomicBool::new(false),
        voice_command_capture: Mutex::new(None),
        pending_manual_command: Mutex::new(None),
        live_running: std::sync::atomic::AtomicBool::new(false),
        db,
        config: Arc::new(Mutex::new(AppConfig::default())),
        reasoner: crate::reason::ReasonerCell::fixed(Arc::new(crate::reason::StubReasoner)),
        current_meeting: Mutex::new(None),
        focus_meeting: Mutex::new(None),
        live_transcript: Mutex::new(String::new()),
        live_transcript_lines: std::sync::Mutex::new(Default::default()),
        processing_queue_running: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        live_bullets: Mutex::new(String::new()),
        live_bullets_tracker: Mutex::new(crate::transcribe::bullets::BulletsTracker::default()),
        capped_notified: std::sync::atomic::AtomicBool::new(false),
        capture_fault_notified: std::sync::atomic::AtomicBool::new(false),
        reactions_shadow_count: std::sync::atomic::AtomicU64::new(0),
        reactions_emitted: Mutex::new(HashSet::new()),
        in_flight_turns: Mutex::new(std::collections::HashMap::new()),
        user_turn_in_progress: std::sync::atomic::AtomicBool::new(false),
        verify_cache: Mutex::new(std::collections::HashMap::new()),
        unlocked_folders: Arc::new(Mutex::new(HashSet::new())),
        master_kek: Mutex::new(None),
        org_ock_cache: Mutex::new(std::collections::HashMap::new()),
        account_session: Mutex::new(None),
        lifecycle: Mutex::new(()),
        active_salvages: Mutex::new(HashSet::new()),
        share_refresh_lock: tokio::sync::Mutex::new(()),
        org_share_mutation_lock: tokio::sync::Mutex::new(()),
        seal_epoch: std::sync::atomic::AtomicU64::new(0),
        heavy_inference: Arc::new(tokio::sync::Semaphore::new(1)),
    };
    (state, path)
}

fn folder(
    db: &crate::storage::db::Db,
    id: &str,
    name: &str,
    parent_id: Option<&str>,
    kind: &str,
    level: &str,
    locked: bool,
) {
    db.insert_folder(&Folder {
        id: id.into(),
        name: name.into(),
        path: id.into(),
        parent_id: parent_id.map(str::to_string),
        locked,
        created_at: "2026-08-27T08:00:00Z".into(),
    })
    .unwrap();
    db.lock()
        .execute(
            "UPDATE folders SET kind=?2, level=?3, locked=?4 WHERE id=?1",
            rusqlite::params![id, kind, level, locked],
        )
        .unwrap();
}

fn fixture(label: &str) -> (AppState, std::path::PathBuf) {
    let (state, path) = test_state(label);
    folder(
        &state.db,
        "space",
        "Workspace",
        None,
        "meeting",
        "project",
        false,
    );
    folder(
        &state.db,
        "source",
        "Source",
        Some("space"),
        "meeting",
        "folder",
        false,
    );
    (state, path)
}
fn request() -> SmartOrganizeRequest {
    SmartOrganizeRequest {
        source_container_id: Some("source".into()),
        include_descendants: false,
        kinds: vec![SmartOrganizeKind::Note],
        rule: SmartOrganizeRule::ByDay,
        destination_parent_id: "source".into(),
        page_offset: 0,
    }
}
fn note(state: &AppState, id: &str) {
    state
        .db
        .insert_note(id, "source", id, id, "body", 1_700_000_000_000)
        .unwrap();
}
fn plan(state: &AppState) -> (StoredPlan, SmartOrganizePreview) {
    let guard = lifecycle_guard(state);
    let result = build_plan(
        state,
        &request(),
        &|_| Ok(0),
        capture_content_visibility_snapshot_under_lifecycle(state),
    )
    .unwrap();
    drop(guard);
    result
}
fn apply(state: &AppState, plan: StoredPlan) -> SmartOrganizeReceipt {
    let selected = plan.moves.clone();
    apply_plan_inner_with_hook(state, plan, selected, |_| {}).unwrap()
}
#[test]
fn apply_rejects_renderer_target_substitution_inside_valid_parent() {
    let (state, _) = fixture("target-substitution");
    note(&state, "n");
    folder(
        &state.db,
        "target1",
        "2023-11-14",
        Some("source"),
        "meeting",
        "folder",
        false,
    );
    folder(
        &state.db,
        "target2",
        "Other",
        Some("source"),
        "meeting",
        "folder",
        false,
    );
    let (plan, _) = plan(&state);
    let forged = serde_json::json!({"planId":plan.plan_id,"selectedItemIds":["n"],"destinationParentId":"target2"});
    assert!(serde_json::from_value::<SmartOrganizeApplyRequest>(forged).is_err());
    let mut selected = plan.moves.clone();
    selected[0].existing_target_id = Some("target2".into());
    assert!(apply_plan_inner_with_hook(&state, plan, selected, |_| {}).is_err());
    assert_eq!(state.db.note_gate_anchor("n").unwrap().unwrap().0, "source");
    assert!(state
        .db
        .list_containers()
        .unwrap()
        .iter()
        .all(|row| row.parent_id.as_deref() != Some("target2")));
}
#[test]
fn registry_validates_before_consuming_and_is_one_shot() {
    let (state, _) = fixture("registry");
    note(&state, "n");
    let (plan, _) = plan(&state);
    let registry = SmartOrganizePlanRegistry::new();
    registry.insert(plan.clone());
    for ids in [
        vec!["outsider".into()],
        vec!["n".into(), "n".into()],
        vec!["n".into(); 51],
    ] {
        assert!(registry
            .take_selected_at(
                &SmartOrganizeApplyRequest {
                    plan_id: plan.plan_id.clone(),
                    selected_item_ids: ids
                },
                Instant::now()
            )
            .is_err());
    }
    registry.discard("old-plan");
    let req = SmartOrganizeApplyRequest {
        plan_id: plan.plan_id.clone(),
        selected_item_ids: vec!["n".into()],
    };
    let (_, selected) = registry.take_selected_at(&req, Instant::now()).unwrap();
    assert_eq!(selected.len(), 1);
    assert!(registry.take_selected_at(&req, Instant::now()).is_err());
    registry.insert(plan.clone());
    assert!(registry
        .take_selected_at(&req, plan.created_at + SMART_ORGANIZE_PLAN_TTL)
        .is_err());
    registry.insert(plan.clone());
    registry.discard(&plan.plan_id);
    assert!(registry.take_selected_at(&req, Instant::now()).is_err());
}
#[test]
fn empty_selection_creates_nothing_and_success_receipt_uses_actual_facts() {
    let (state, _) = fixture("empty");
    note(&state, "n");
    let (plan, preview) = plan(&state);
    let count = state.db.list_containers().unwrap().len();
    assert_eq!(preview.new_folders, 1);
    assert_eq!(preview.buckets[0].items[0].title, "n");
    let receipt = apply_plan_inner_with_hook(&state, plan.clone(), vec![], |_| {}).unwrap();
    assert!(receipt.applied.is_empty());
    assert_eq!(state.db.list_containers().unwrap().len(), count);
    let receipt = apply(&state, plan);
    assert_eq!(receipt.applied.len(), 1);
    assert_eq!(
        receipt.applied[0].from_container_id.as_deref(),
        Some("source")
    );
    assert_eq!(
        receipt.applied[0].to_container_id,
        state.db.note_gate_anchor("n").unwrap().unwrap().0
    );
    assert_eq!(
        receipt.applied[0].to_breadcrumb,
        "Workspace / Source / 2023-11-14"
    );
}
#[test]
fn source_race_is_refused_before_any_folder_is_created() {
    let (state, _) = fixture("source-race");
    note(&state, "n");
    let (plan, _) = plan(&state);
    let selected = plan.moves.clone();
    let count = state.db.list_containers().unwrap().len();
    let receipt = apply_plan_inner_with_hook(&state, plan, selected, |_| {
        state
            .db
            .lock()
            .execute("UPDATE documents SET folder_id='space' WHERE id='n'", [])
            .unwrap();
    })
    .unwrap();
    assert!(receipt.applied.is_empty());
    assert_eq!(receipt.failures.len(), 1);
    assert_eq!(state.db.note_gate_anchor("n").unwrap().unwrap().0, "space");
    assert_eq!(state.db.list_containers().unwrap().len(), count);
}
#[test]
fn existing_target_replacement_and_new_target_appearance_are_refused() {
    for existing in [false, true] {
        let (state, _) = fixture(if existing { "replace" } else { "appear" });
        note(&state, "n");
        if existing {
            folder(
                &state.db,
                "original",
                "2023-11-14",
                Some("source"),
                "meeting",
                "folder",
                false,
            );
        }
        let (plan, _) = plan(&state);
        let selected = plan.moves.clone();
        let receipt = apply_plan_inner_with_hook(&state, plan, selected, |_| {
            if existing {
                state
                    .db
                    .lock()
                    .execute("UPDATE folders SET name='Renamed' WHERE id='original'", [])
                    .unwrap();
            }
            folder(
                &state.db,
                "imposter",
                "2023-11-14",
                Some("source"),
                "meeting",
                "folder",
                false,
            );
        })
        .unwrap();
        assert!(receipt.applied.is_empty());
        assert_eq!(state.db.note_gate_anchor("n").unwrap().unwrap().0, "source");
    }
}
#[test]
fn immutable_dates_batch_limit_and_wire_contract() {
    let (state, _) = fixture("wire");
    for index in 0..60 {
        note(&state, &format!("n{index:02}"));
    }
    state
        .db
        .lock()
        .execute("UPDATE documents SET updated_at=1900000000000", [])
        .unwrap();
    let (_, preview) = plan(&state);
    assert_eq!(preview.total_scanned, 60);
    assert_eq!(preview.deferred, 10);
    assert_eq!(preview.buckets[0].items.len(), 50);
    assert_eq!(preview.buckets[0].folder_name, "2023-11-14");
    let value = serde_json::to_value(&preview).unwrap();
    for key in [
        "planId",
        "totalScanned",
        "alreadyThere",
        "deferred",
        "newFolders",
        "reusedFolders",
        "timezoneLabel",
        "nextPageOffset",
        "buckets",
        "skipped",
    ] {
        assert!(value.get(key).is_some(), "{key}");
    }
    let item = &value["buckets"][0]["items"][0];
    assert_eq!(item["kind"], "note");
    for key in [
        "itemId",
        "kind",
        "title",
        "fromContainerId",
        "fromBreadcrumb",
        "reason",
    ] {
        assert!(item.get(key).is_some(), "{key}");
    }
    assert!(!value.to_string().contains("body"));
    assert_eq!(
        serde_json::to_value(SmartOrganizeKind::Meeting).unwrap(),
        "meeting"
    );
    let request:SmartOrganizeRequest=serde_json::from_value(serde_json::json!({"sourceContainerId":null,"includeDescendants":false,"kinds":["meeting"],"rule":"byDay","destinationParentId":"space"})).unwrap();
    assert!(request.source_container_id.is_none());
    assert_eq!(request.page_offset, 0);
}
#[test]
fn local_midnight_and_per_event_dst_offsets_are_frozen() {
    let millis = chrono::DateTime::parse_from_rfc3339("2026-01-01T23:30:00Z")
        .unwrap()
        .timestamp_millis();
    assert_eq!(day_bucket_key(millis, 3600).unwrap(), "2026-01-02");
    assert_eq!(day_bucket_key(millis, -3600).unwrap(), "2026-01-01");
    let (state, _) = fixture("dst");
    note(&state, "n");
    let guard = lifecycle_guard(&state);
    let (plan, _) = build_plan(
        &state,
        &request(),
        &|_| Ok(-3600),
        capture_content_visibility_snapshot_under_lifecycle(&state),
    )
    .unwrap();
    drop(guard);
    assert!(matches!(
        plan.moves[0].witness,
        MoveWitness::Day {
            offset_seconds: -3600,
            ..
        }
    ));
    assert_eq!(apply(&state, plan).applied.len(), 1);
}
fn endpoint(meeting: &str, id: &str) -> SmartOrganizeEndpointRow {
    SmartOrganizeEndpointRow {
        meeting_id: meeting.into(),
        endpoint_kind: "note".into(),
        endpoint_id: id.into(),
    }
}
#[test]
fn relation_qualification_ignores_singletons_and_recomputes_after_ambiguity() {
    let ids = vec!["a".into(), "b".into(), "c".into()];
    let edges = vec![
        endpoint("a", "common"),
        endpoint("b", "common"),
        endpoint("a", "singleton"),
    ];
    let grouped = group_by_relation(&ids, &edges, &|_, id| Some(id.into()));
    assert!(matches!(grouped["a"], Grouping::Bucket { .. }));
    assert!(matches!(grouped["b"], Grouping::Bucket { .. }));
    let edges = vec![
        endpoint("a", "first"),
        endpoint("b", "first"),
        endpoint("b", "second"),
        endpoint("c", "second"),
    ];
    let grouped = group_by_relation(&ids, &edges, &|_, id| Some(id.into()));
    assert_eq!(grouped["b"], Grouping::Skip("ambiguousRelation"));
    assert!(matches!(grouped["a"], Grouping::Skip(_)));
    assert!(matches!(grouped["c"], Grouping::Skip(_)));
}
fn recording(state: &AppState, id: &str, owner: Option<&str>) {
    state
        .db
        .insert_meeting(&Meeting {
            id: id.into(),
            started_at: "2023-11-14T22:13:20Z".into(),
            ended_at: None,
            title: Some(format!("Title {id}")),
            duration_s: 10,
            audio_path: None,
            status: MeetingStatus::Transcribed,
            folder_id: owner.map(str::to_string),
        })
        .unwrap();
}
fn link(state: &AppState, meeting: &str, endpoint: &str, edge: &str, creator: &str, status: &str) {
    state.db.lock().execute("INSERT INTO links(src_kind,src_id,dst_kind,dst_id,edge_type,created_by,status,created_at) VALUES('meeting',?1,'note',?2,?3,?4,?5,0)",rusqlite::params![meeting,endpoint,edge,creator,status]).unwrap();
}
fn recording_plan(
    state: &AppState,
    relation: bool,
    inbox: bool,
) -> (StoredPlan, SmartOrganizePreview) {
    let guard = lifecycle_guard(state);
    let result = build_plan(
        state,
        &SmartOrganizeRequest {
            source_container_id: if inbox { None } else { Some("source".into()) },
            include_descendants: false,
            kinds: vec![SmartOrganizeKind::Meeting],
            rule: if relation {
                SmartOrganizeRule::ByRelation
            } else {
                SmartOrganizeRule::ByDay
            },
            destination_parent_id: "source".into(),
            page_offset: 0,
        },
        &|_| Ok(0),
        capture_content_visibility_snapshot_under_lifecycle(state),
    )
    .unwrap();
    drop(guard);
    result
}
#[test]
fn recording_success_and_companion_source_race_are_guarded() {
    let (state, _) = fixture("recording");
    recording(&state, "m", None);
    let (plan, preview) = recording_plan(&state, false, true);
    assert_eq!(preview.buckets[0].items[0].kind, SmartOrganizeKind::Meeting);
    assert_eq!(apply(&state, plan).applied.len(), 1);
    let (state, _) = fixture("companion-race");
    recording(&state, "m", Some("source"));
    note(&state, "companion");
    state.db.set_document_meeting_id("companion", "m").unwrap();
    let (plan, _) = recording_plan(&state, false, false);
    let selected = plan.moves.clone();
    let count = state.db.list_containers().unwrap().len();
    let receipt = apply_plan_inner_with_hook(&state, plan, selected, |_| {
        state
            .db
            .lock()
            .execute(
                "UPDATE documents SET folder_id='space' WHERE id='companion'",
                [],
            )
            .unwrap();
    })
    .unwrap();
    assert!(receipt.applied.is_empty());
    assert_eq!(
        state
            .db
            .smart_organize_meeting_owner("m")
            .unwrap()
            .as_deref(),
        Some("source")
    );
    assert_eq!(
        state.db.note_gate_anchor("companion").unwrap().unwrap().0,
        "space"
    );
    assert_eq!(state.db.list_containers().unwrap().len(), count);
}
#[test]
fn relation_policy_success_and_peer_race() {
    for (edge, creator, status, allowed) in [
        ("manual", "user", "active", true),
        ("wikilink", "user", "active", true),
        ("semantic", "accepted", "active", true),
        ("semantic", "model", "suggested", false),
        ("semantic", "accepted", "dismissed", false),
        ("companion", "user", "active", false),
    ] {
        let (state, _) = fixture("relation-policy");
        note(&state, "anchor");
        for id in ["a", "b"] {
            recording(&state, id, Some("source"));
            link(&state, id, "anchor", edge, creator, status);
        }
        let (plan, preview) = recording_plan(&state, true, false);
        assert_eq!(!preview.buckets.is_empty(), allowed);
        if allowed {
            let receipt = apply(&state, plan);
            assert_eq!(receipt.applied.len(), 2, "{edge}: {:?}", receipt.failures);
        }
    }
    let (state, _) = fixture("peer-race");
    note(&state, "anchor");
    for id in ["a", "b"] {
        recording(&state, id, Some("source"));
        link(&state, id, "anchor", "manual", "user", "active");
    }
    let (plan, _) = recording_plan(&state, true, false);
    let selected = vec![plan.moves[0].clone()];
    let count = state.db.list_containers().unwrap().len();
    let receipt = apply_plan_inner_with_hook(&state, plan, selected, |_| {
        state
            .db
            .lock()
            .execute("UPDATE meetings SET folder_id='space' WHERE id='b'", [])
            .unwrap();
    })
    .unwrap();
    assert!(receipt.applied.is_empty());
    assert_eq!(state.db.list_containers().unwrap().len(), count);
}
#[test]
fn protected_provider_recording_is_absent_from_preview_and_group_count() {
    let (state, _) = fixture("protected-provider");
    note(&state, "anchor");
    folder(
        &state.db,
        "sealed",
        "Secret",
        Some("space"),
        "meeting",
        "folder",
        true,
    );
    for id in ["a", "b"] {
        recording(&state, id, Some("source"));
        link(&state, id, "anchor", "manual", "user", "active");
    }
    state.db.lock().execute("INSERT INTO notes(meeting_id,provider_id,markdown,created_at,folder_id) VALUES('b','test','', '2026-01-01T00:00:00Z','sealed')",[]).unwrap();
    let (_, preview) = recording_plan(&state, true, false);
    assert_eq!(preview.total_scanned, 1);
    assert!(preview.buckets.is_empty());
    let wire = serde_json::to_string(&preview).unwrap();
    assert!(!wire.contains("Title b"));
    assert!(!wire.contains("\"b\""));
}
#[test]
fn next_day_batch_reaches_deferred_descendant_items() {
    let (state, _) = fixture("pagination");
    for index in 0..60 {
        note(&state, &format!("n{index:02}"));
    }
    let mut req = request();
    req.include_descendants = true;
    let make = || {
        let _guard = lifecycle_guard(&state);
        build_plan(
            &state,
            &req,
            &|_| Ok(0),
            capture_content_visibility_snapshot_under_lifecycle(&state),
        )
        .unwrap()
    };
    let (first, preview) = make();
    assert_eq!(preview.deferred, 10);
    assert_eq!(apply(&state, first).applied.len(), 50);
    let (second, preview) = make();
    assert_eq!(preview.already_there, 50);
    assert_eq!(preview.deferred, 0);
    assert_eq!(second.moves.len(), 10);
    assert_eq!(apply(&state, second).applied.len(), 10);
}
#[test]
fn receipt_drops_prior_success_titles_when_visibility_changes_mid_batch() {
    let (state, _) = fixture("receipt-privacy");
    note(&state, "a");
    note(&state, "b");
    let (plan, _) = plan(&state);
    let selected = plan.moves.clone();
    let result = apply_plan_inner_with_hook(&state, plan, selected, |id| {
        if id == "b" {
            state
                .seal_epoch
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    });
    assert!(matches!(result, Err(AppError::Locked(_))));
    assert_ne!(state.db.note_gate_anchor("a").unwrap().unwrap().0, "source");
    assert_eq!(state.db.note_gate_anchor("b").unwrap().unwrap().0, "source");
}
#[test]
fn locked_source_or_existing_target_cannot_move_reviewed_notes() {
    for lock_target in [false, true] {
        let (state, _) = fixture("relock");
        note(&state, "n");
        folder(
            &state.db,
            "target",
            "2023-11-14",
            Some("source"),
            "meeting",
            "folder",
            false,
        );
        let (plan, _) = plan(&state);
        let selected = plan.moves.clone();
        let receipt = apply_plan_inner_with_hook(&state, plan, selected, |_| {
            state
                .db
                .lock()
                .execute(
                    "UPDATE folders SET locked=1 WHERE id=?1",
                    [if lock_target { "target" } else { "source" }],
                )
                .unwrap();
        })
        .unwrap();
        assert!(receipt.applied.is_empty());
        assert_eq!(receipt.failures.len(), 1);
        assert_eq!(state.db.note_gate_anchor("n").unwrap().unwrap().0, "source");
    }
}
#[test]
fn relation_removal_or_anchor_rename_refuses_before_creation() {
    for rename in [false, true] {
        let (state, _) = fixture("relation-change");
        note(&state, "anchor");
        for id in ["a", "b"] {
            recording(&state, id, Some("source"));
            link(&state, id, "anchor", "manual", "user", "active");
        }
        let (plan, _) = recording_plan(&state, true, false);
        let selected = vec![plan.moves[0].clone()];
        let count = state.db.list_containers().unwrap().len();
        let receipt = apply_plan_inner_with_hook(&state, plan, selected, |_| {
            if rename {
                state
                    .db
                    .lock()
                    .execute(
                        "UPDATE documents SET title='Other name' WHERE id='anchor'",
                        [],
                    )
                    .unwrap();
            } else {
                state
                    .db
                    .lock()
                    .execute("UPDATE links SET status='dismissed' WHERE src_id='a'", [])
                    .unwrap();
            }
        })
        .unwrap();
        assert!(receipt.applied.is_empty());
        assert_eq!(state.db.list_containers().unwrap().len(), count);
        assert_eq!(
            state
                .db
                .smart_organize_meeting_owner("a")
                .unwrap()
                .as_deref(),
            Some("source")
        );
    }
}
#[test]
fn relation_next_page_reaches_later_matches_without_writing() {
    let (state, _) = fixture("relation-pages");
    note(&state, "anchor");
    for index in 0..52 {
        recording(&state, &format!("m{index:02}"), Some("source"));
    }
    for id in ["m50", "m51"] {
        link(&state, id, "anchor", "manual", "user", "active");
    }
    let count = state.db.list_containers().unwrap().len();
    let (_, first) = recording_plan(&state, true, false);
    assert!(first.buckets.is_empty());
    assert_eq!(first.next_page_offset, Some(50));
    assert_eq!(first.deferred, 2);
    let mut request = SmartOrganizeRequest {
        source_container_id: Some("source".into()),
        include_descendants: false,
        kinds: vec![SmartOrganizeKind::Meeting],
        rule: SmartOrganizeRule::ByRelation,
        destination_parent_id: "source".into(),
        page_offset: 50,
    };
    let make = |request: &SmartOrganizeRequest| {
        let _guard = lifecycle_guard(&state);
        build_plan(
            &state,
            request,
            &|_| Ok(0),
            capture_content_visibility_snapshot_under_lifecycle(&state),
        )
    };
    let (plan, second) = make(&request).unwrap();
    assert_eq!(second.buckets[0].items.len(), 2);
    assert_eq!(second.next_page_offset, None);
    assert_eq!(second.deferred, 0);
    assert_eq!(state.db.list_containers().unwrap().len(), count);
    assert!(plan
        .moves
        .iter()
        .all(|item| ["m50", "m51"].contains(&item.item_id.as_str())));
    request.page_offset = u32::MAX;
    let (_, empty) = make(&request).unwrap();
    assert!(empty.buckets.is_empty());
    assert_eq!(empty.next_page_offset, None);
    assert_eq!(empty.deferred, 0);
    request.rule = SmartOrganizeRule::ByDay;
    assert!(make(&request).is_err());
}
#[cfg(target_os = "macos")]
#[test]
fn canonical_unicode_and_case_folder_collisions_are_refused() {
    assert!(smart_organize_names_equal("CAFÉ", "cafe\u{301}"));
    assert!(!smart_organize_names_equal("café", "cafe"));
    let (state, _) = fixture("unicode-collision");
    folder(
        &state.db,
        "one",
        "CAFÉ",
        Some("source"),
        "meeting",
        "folder",
        false,
    );
    folder(
        &state.db,
        "two",
        "cafe\u{301}",
        Some("source"),
        "meeting",
        "folder",
        false,
    );
    assert_eq!(
        state
            .db
            .smart_organize_child_folders_case_folded("source", "Café")
            .unwrap()
            .len(),
        2
    );
    let ids = ["a", "b", "c", "d"].map(str::to_string);
    let edges = vec![
        endpoint("a", "one"),
        endpoint("b", "one"),
        endpoint("c", "two"),
        endpoint("d", "two"),
    ];
    let groups = group_by_relation(&ids, &edges, &|_, id| {
        Some(if id == "one" { "CAFÉ" } else { "cafe\u{301}" }.into())
    });
    assert!(groups
        .values()
        .all(|group| *group == Grouping::Skip("nameCollision")));
}
#[test]
fn pending_container_move_recovery_refuses_filing_before_folder_creation() {
    for after_admission in [false, true] {
        let (state, _) = fixture("pending-container-recovery");
        note(&state, "n");
        // Reuse is crucial: creation has its own admission, but an existing target reaches
        // the under-lifecycle note writer directly and previously bypassed recovery entirely.
        folder(
            &state.db,
            "target",
            "2023-11-14",
            Some("source"),
            "meeting",
            "folder",
            false,
        );
        let (plan, _) = plan(&state);
        let selected = plan.moves.clone();
        let count = state.db.list_containers().unwrap().len();
        let pending = || {
            state.db.lock().execute(
                "INSERT INTO container_move_journal(container_id,old_parent,new_parent,old_path,new_path,intended_level,phase,payload) VALUES('source','space','space','source','moved','folder','prepared','{}')",
                [],
            ).unwrap();
        };
        if !after_admission {
            pending();
        }
        let receipt = apply_plan_inner_with_hook(&state, plan, selected, |_| {
            if after_admission {
                pending();
            }
        })
        .unwrap();
        assert!(receipt.applied.is_empty());
        assert_eq!(receipt.failures.len(), 1);
        assert_eq!(state.db.note_gate_anchor("n").unwrap().unwrap().0, "source");
        assert_eq!(state.db.list_containers().unwrap().len(), count);
        assert!(state.db.ensure_container_move_ready().is_err());
    }
}
