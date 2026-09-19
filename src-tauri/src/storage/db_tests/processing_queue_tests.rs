//! File-backed tests for the DEFERRED-PROCESSING QUEUE (T5 meeting surface, 2026-09-19).
//!
//! The Track-B oracle here is `queue_survives_reopen_and_reclaims_interrupted_job`: it closes and
//! REOPENS an encrypted database, because the whole point of the table is that "process later"
//! outlives an app restart. Against the pre-change code it fails at `migrate()` — there is no
//! `processing_queue` table to enqueue into.
//!
//! These use `open_with_key` + a fixed literal DEK, so they never touch the Keychain.

use super::*;
use crate::storage::models::{Meeting, MeetingStatus, ProcessingQueueState};

/// The same fixed placeholder the sibling file-backed suites use — the documented
/// MURMUR_DEV_DEK-shaped literal, never a real Keychain DEK.
const TEST_DEK: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"; // MURMUR_DEV_DEK placeholder

fn seed_meeting(db: &Db, meeting_id: &str) {
    db.insert_meeting(&Meeting {
        id: meeting_id.to_string(),
        started_at: "2026-09-19T10:00:00Z".to_string(),
        ended_at: Some("2026-09-19T10:30:00Z".to_string()),
        title: Some("standup".to_string()),
        duration_s: 1800,
        audio_path: None,
        status: MeetingStatus::Draft,
        folder_id: None,
    })
    .unwrap();
}

fn ids(db: &Db) -> Vec<String> {
    db.list_processing_queue()
        .unwrap()
        .into_iter()
        .map(|job| job.meeting_id)
        .collect()
}

/// TRACK-B ORACLE: the queue loses neither its rows nor their ORDER across a restart, and a job
/// interrupted mid-processing comes back as waiting AT THE SAME POSITION without auto-running.
#[test]
fn queue_survives_reopen_and_reclaims_interrupted_job() {
    let path = super::unique_temp_path("meetnotes-queue-restart", "sqlite");
    {
        let db = Db::open_with_key(&path, TEST_DEK).unwrap();
        for id in ["m-a", "m-b", "m-c"] {
            seed_meeting(&db, id);
            db.enqueue_processing_job(id, "2026-09-19T10:31:00Z")
                .unwrap();
        }
        // The user reorders: c first, then a, then b.
        db.reorder_processing_queue(
            &["m-c".to_string(), "m-a".to_string(), "m-b".to_string()],
            "2026-09-19T10:32:00Z",
        )
        .unwrap();
        let claimed = db
            .claim_next_processing_job("2026-09-19T10:33:00Z")
            .unwrap()
            .expect("head of queue is claimable");
        assert_eq!(claimed.meeting_id, "m-c");
        assert_eq!(claimed.state, ProcessingQueueState::Processing);
        db.set_processing_job_stage("m-c", "transcribing", "2026-09-19T10:33:30Z")
            .unwrap();
        // …and the app dies here, holding m-c.
    }

    let db = Db::open_with_key(&path, TEST_DEK).unwrap();
    // Before reconciliation the row is still exactly as the dead process left it.
    let stranded = db.processing_job("m-c").unwrap().unwrap();
    assert_eq!(stranded.state, ProcessingQueueState::Processing);
    assert_eq!(stranded.attempts, 1);

    let reclaimed = db
        .reconcile_processing_queue("2026-09-19T11:00:00Z")
        .unwrap();
    assert_eq!(reclaimed, 1);

    assert_eq!(
        ids(&db),
        vec!["m-c", "m-a", "m-b"],
        "order must survive restart"
    );
    let head = db.processing_job("m-c").unwrap().unwrap();
    assert_eq!(
        head.state,
        ProcessingQueueState::Queued,
        "interrupted work waits again"
    );
    assert_eq!(head.position, 0, "and waits IN ITS OLD POSITION");
    assert_eq!(head.attempts, 1, "the dead attempt is still counted");
    assert_eq!(head.stage, None, "stale progress is not replayed as truth");
    // Reconciliation must NOT start anything: nothing is processing afterwards.
    assert!(db
        .list_processing_queue()
        .unwrap()
        .iter()
        .all(|job| job.state != ProcessingQueueState::Processing));
}

#[test]
fn enqueue_is_idempotent_and_never_reorders() {
    let db = Db::open_with_key(
        &super::unique_temp_path("meetnotes-queue-idempotent", "sqlite"),
        TEST_DEK,
    )
    .unwrap();
    for id in ["m-1", "m-2"] {
        seed_meeting(&db, id);
        db.enqueue_processing_job(id, "2026-09-19T10:00:00Z")
            .unwrap();
    }
    db.enqueue_processing_job("m-1", "2026-09-19T10:05:00Z")
        .unwrap();
    assert_eq!(ids(&db), vec!["m-1", "m-2"]);
    assert_eq!(db.list_processing_queue().unwrap().len(), 2);
    assert!(db.claim_processing_job("m-1", "running").unwrap());
    db.set_processing_job_stage("m-1", "transcribing", "running")
        .unwrap();
    let running = db.processing_job("m-1").unwrap().unwrap();
    db.enqueue_processing_job("m-1", "duplicate-enqueue")
        .unwrap();
    assert_eq!(
        db.processing_job("m-1").unwrap().unwrap(),
        running,
        "duplicate enqueue must not reset an active job or its attempt/progress"
    );
    assert_eq!(
        db.get_meeting("m-1").unwrap().unwrap().status,
        MeetingStatus::Recording,
        "duplicate enqueue must not downgrade a processing meeting to Queued"
    );
}

#[test]
fn only_one_job_is_claimable_at_a_time() {
    let db = Db::open_with_key(
        &super::unique_temp_path("meetnotes-queue-claim", "sqlite"),
        TEST_DEK,
    )
    .unwrap();
    for id in ["m-1", "m-2"] {
        seed_meeting(&db, id);
        db.enqueue_processing_job(id, "2026-09-19T10:00:00Z")
            .unwrap();
    }
    assert_eq!(
        db.claim_next_processing_job("2026-09-19T10:01:00Z")
            .unwrap()
            .unwrap()
            .meeting_id,
        "m-1"
    );
    assert!(
        db.claim_next_processing_job("2026-09-19T10:01:01Z")
            .unwrap()
            .is_none(),
        "a second claim while one job runs must not double-book the worker"
    );
}

#[test]
fn failed_job_retries_in_place_and_keeps_its_attempts() {
    let db = Db::open_with_key(
        &super::unique_temp_path("meetnotes-queue-retry", "sqlite"),
        TEST_DEK,
    )
    .unwrap();
    for id in ["m-1", "m-2"] {
        seed_meeting(&db, id);
        db.enqueue_processing_job(id, "2026-09-19T10:00:00Z")
            .unwrap();
    }
    db.claim_next_processing_job("2026-09-19T10:01:00Z")
        .unwrap();
    db.fail_processing_job("m-1", "transcription_failed", "2026-09-19T10:02:00Z")
        .unwrap();
    let failed = db.processing_job("m-1").unwrap().unwrap();
    assert_eq!(failed.state, ProcessingQueueState::Failed);
    assert_eq!(failed.error_code.as_deref(), Some("transcription_failed"));

    assert!(db
        .retry_processing_job("m-1", "2026-09-19T10:03:00Z")
        .unwrap());
    let retried = db.processing_job("m-1").unwrap().unwrap();
    assert_eq!(retried.state, ProcessingQueueState::Queued);
    assert_eq!(
        retried.position, 0,
        "retry does not send the job to the back"
    );
    assert_eq!(retried.attempts, 1);
    assert_eq!(retried.error_code, None);
    assert!(
        !db.retry_processing_job("m-2", "2026-09-19T10:04:00Z")
            .unwrap(),
        "retrying a job that never failed is a no-op, not a silent state change"
    );
}

#[test]
fn promote_moves_a_block_to_the_front_and_keeps_positions_dense() {
    let db = Db::open_with_key(
        &super::unique_temp_path("meetnotes-queue-promote", "sqlite"),
        TEST_DEK,
    )
    .unwrap();
    for id in ["m-1", "m-2", "m-3", "m-4"] {
        seed_meeting(&db, id);
        db.enqueue_processing_job(id, "2026-09-19T10:00:00Z")
            .unwrap();
    }
    db.promote_processing_jobs(
        &["m-3".to_string(), "m-4".to_string()],
        "2026-09-19T10:01:00Z",
    )
    .unwrap();
    assert_eq!(ids(&db), vec!["m-3", "m-4", "m-1", "m-2"]);
    let positions: Vec<i64> = db
        .list_processing_queue()
        .unwrap()
        .into_iter()
        .map(|job| job.position)
        .collect();
    assert_eq!(
        positions,
        vec![0, 1, 2, 3],
        "positions stay dense and non-negative"
    );
}

#[test]
fn removing_a_job_closes_the_gap_and_leaves_the_meeting_alone() {
    let db = Db::open_with_key(
        &super::unique_temp_path("meetnotes-queue-remove", "sqlite"),
        TEST_DEK,
    )
    .unwrap();
    for id in ["m-1", "m-2", "m-3"] {
        seed_meeting(&db, id);
        db.enqueue_processing_job(id, "2026-09-19T10:00:00Z")
            .unwrap();
    }
    assert!(db.remove_processing_job("m-2").unwrap());
    assert_eq!(ids(&db), vec!["m-1", "m-3"]);
    assert_eq!(
        db.list_processing_queue().unwrap()[1].position,
        1,
        "a removed job must not leave a hole in the order"
    );
    assert!(
        db.get_meeting("m-2").unwrap().is_some(),
        "removing a QUEUE row never deletes the recording"
    );
    assert!(!db.remove_processing_job("m-2").unwrap());
}

#[test]
fn reorder_refuses_a_partial_or_duplicated_list() {
    let db = Db::open_with_key(
        &super::unique_temp_path("meetnotes-queue-reorder-guard", "sqlite"),
        TEST_DEK,
    )
    .unwrap();
    for id in ["m-1", "m-2"] {
        seed_meeting(&db, id);
        db.enqueue_processing_job(id, "2026-09-19T10:00:00Z")
            .unwrap();
    }
    assert!(db
        .reorder_processing_queue(&["m-1".to_string()], "2026-09-19T10:01:00Z")
        .is_err());
    assert!(db
        .reorder_processing_queue(
            &["m-1".to_string(), "m-1".to_string()],
            "2026-09-19T10:01:00Z"
        )
        .is_err());
    assert!(db
        .reorder_processing_queue(
            &["m-1".to_string(), "m-nope".to_string()],
            "2026-09-19T10:01:00Z"
        )
        .is_err());
    assert_eq!(
        ids(&db),
        vec!["m-1", "m-2"],
        "a refused reorder changes nothing"
    );
}

#[test]
fn migrate_stays_idempotent_with_the_queue_table() {
    let path = super::unique_temp_path("meetnotes-queue-idempotent-migrate", "sqlite");
    let db = Db::open_with_key(&path, TEST_DEK).unwrap();
    seed_meeting(&db, "m-1");
    db.enqueue_processing_job("m-1", "2026-09-19T10:00:00Z")
        .unwrap();
    drop(db);
    let db = Db::open_with_key(&path, TEST_DEK).unwrap();
    db.migrate().unwrap();
    assert_eq!(ids(&db), vec!["m-1"]);
}

#[test]
fn selected_claim_never_runs_an_unselected_head() {
    let db = Db::open_with_key(
        &super::unique_temp_path("queue-selected", "sqlite"),
        TEST_DEK,
    )
    .unwrap();
    for id in ["leave-later", "run-selected"] {
        seed_meeting(&db, id);
        db.enqueue_processing_job(id, "now").unwrap();
    }
    assert!(db.claim_processing_job("run-selected", "later").unwrap());
    assert_eq!(
        db.processing_job("leave-later").unwrap().unwrap().state,
        ProcessingQueueState::Queued
    );
    assert_eq!(
        db.processing_job("run-selected").unwrap().unwrap().state,
        ProcessingQueueState::Processing
    );
    assert!(!db.claim_processing_job("leave-later", "later").unwrap());
    assert_eq!(
        db.get_meeting("run-selected").unwrap().unwrap().status,
        MeetingStatus::Recording
    );
}

#[test]
fn queue_commit_sets_honest_status_and_protects_archive_from_pruning() {
    let db =
        Db::open_with_key(&super::unique_temp_path("queue-prune", "sqlite"), TEST_DEK).unwrap();
    seed_meeting(&db, "waiting");
    db.enqueue_processing_job("waiting", "now").unwrap();
    assert_eq!(
        db.get_meeting("waiting").unwrap().unwrap().status,
        MeetingStatus::Queued
    );
    assert!(!db
        .prunable_audio_candidates()
        .unwrap()
        .iter()
        .any(|m| m.meeting_id == "waiting"));
}

#[test]
fn restart_removes_success_before_row_delete_but_retains_failed_attempts() {
    let db = Db::open_with_key(
        &super::unique_temp_path("queue-success-crash", "sqlite"),
        TEST_DEK,
    )
    .unwrap();
    for id in ["saved", "unfinished"] {
        seed_meeting(&db, id);
        db.enqueue_processing_job(id, "now").unwrap();
    }
    db.claim_processing_job("saved", "later").unwrap();
    db.update_meeting_status("saved", MeetingStatus::Summarized)
        .unwrap();
    // Status is written BEFORE the note by the pipeline: status alone is not a success proof.
    db.upsert_note(&crate::storage::models::NoteRecord {
        meeting_id: "saved".into(),
        provider_id: "test".into(),
        markdown: "durable".into(),
        created_at: "now".into(),
        exported_path: None,
        model_requested: None,
        model_served: None,
        gateway_host: None,
    })
    .unwrap();
    db.set_processing_job_stage("saved", "saved", "after-note-write")
        .unwrap();
    assert_eq!(db.reconcile_processing_queue("restart").unwrap(), 0);
    assert_eq!(ids(&db), ["unfinished"]);
    assert!(db.claim_processing_job("unfinished", "restart").unwrap());
    db.reconcile_processing_queue("restart-2").unwrap();
    assert_eq!(
        db.get_meeting("unfinished").unwrap().unwrap().status,
        MeetingStatus::Queued
    );
    assert_eq!(
        db.processing_job("unfinished").unwrap().unwrap().attempts,
        1
    );
}

#[test]
fn yielding_queue_job_preserves_position_and_attempts_across_restart() {
    let path = super::unique_temp_path("queue-yield", "sqlite");
    {
        let db = Db::open_with_key(&path, TEST_DEK).unwrap();
        for id in ["head", "yielding", "tail"] {
            seed_meeting(&db, id);
            db.enqueue_processing_job(id, "now").unwrap();
        }
        assert!(db.claim_processing_job("yielding", "claim").unwrap());
        db.set_processing_job_stage("yielding", "transcribing", "asr")
            .unwrap();
        db.park_processing_job("yielding", "new-recording").unwrap();
    }
    let db = Db::open_with_key(&path, TEST_DEK).unwrap();
    assert_eq!(db.reconcile_processing_queue("restart").unwrap(), 0);
    assert_eq!(ids(&db), ["head", "yielding", "tail"]);
    let job = db.processing_job("yielding").unwrap().unwrap();
    assert_eq!(job.state, ProcessingQueueState::Queued);
    assert_eq!(job.position, 1);
    assert_eq!(job.attempts, 1);
    assert!(job.stage.is_none());
    assert!(job.error_code.is_none());
    assert_eq!(
        db.get_meeting("yielding").unwrap().unwrap().status,
        MeetingStatus::Queued
    );
}

#[test]
fn retry_crash_with_previous_note_and_summarized_status_does_not_lose_queue_job() {
    let path = super::unique_temp_path("queue-stale-note", "sqlite");
    {
        let db = Db::open_with_key(&path, TEST_DEK).unwrap();
        for id in ["head", "retrying", "tail"] {
            seed_meeting(&db, id);
            db.enqueue_processing_job(id, "now").unwrap();
        }
        assert!(db
            .claim_processing_job("retrying", "first-attempt")
            .unwrap());
        db.upsert_note(&crate::storage::models::NoteRecord {
            meeting_id: "retrying".into(),
            provider_id: "test".into(),
            markdown: "old attempt".into(),
            created_at: "first-attempt".into(),
            exported_path: None,
            model_requested: None,
            model_served: None,
            gateway_host: None,
        })
        .unwrap();
        db.set_processing_job_stage("retrying", "saved", "first-note-written")
            .unwrap();
        db.fail_processing_job("retrying", "processing_failed", "first-failed")
            .unwrap();
        db.retry_processing_job("retrying", "retry").unwrap();
        assert!(
            db.processing_job("retrying")
                .unwrap()
                .unwrap()
                .stage
                .is_none(),
            "retry must clear the previous completion witness"
        );
        assert!(db
            .claim_processing_job("retrying", "second-attempt")
            .unwrap());
        assert!(
            db.processing_job("retrying")
                .unwrap()
                .unwrap()
                .stage
                .is_none(),
            "claim must start without an inherited completion witness"
        );
        db.set_processing_job_stage("retrying", "summarizing", "second-summarizing")
            .unwrap();
        // Exact crash window: status changed but the second attempt has not upserted its note.
        db.update_meeting_status("retrying", MeetingStatus::Summarized)
            .unwrap();
    }
    let db = Db::open_with_key(&path, TEST_DEK).unwrap();
    assert_eq!(db.reconcile_processing_queue("restart").unwrap(), 1);
    assert_eq!(ids(&db), ["head", "retrying", "tail"]);
    let job = db.processing_job("retrying").unwrap().unwrap();
    assert_eq!(job.state, ProcessingQueueState::Queued);
    assert_eq!(job.position, 1);
    assert_eq!(job.attempts, 2);
    assert!(job.stage.is_none());
    assert_eq!(
        db.get_meeting("retrying").unwrap().unwrap().status,
        MeetingStatus::Queued
    );
}
