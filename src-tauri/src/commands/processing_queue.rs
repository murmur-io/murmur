//! SQLite-canonical deferred work. Commands return metadata only and recheck content visibility
//! at admission; the existing salvage pipeline owns ASR, provider consent, and lock finalization.
use std::collections::HashSet;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::error::{AppError, Result};
use crate::state::AppState;
use crate::storage::models::{ProcessingQueueJob, ProcessingQueueState};

const EVENT_QUEUE_CHANGED: &str = "murmur://processing-queue-changed";
const MAX_SELECTION: usize = 500;

pub(crate) fn emit_processing_queue_changed(app: &AppHandle) {
    let _ = app.emit(EVENT_QUEUE_CHANGED, ());
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessingQueueItem {
    meeting_id: String,
    title: String,
    state: ProcessingQueueState,
    position: i64,
    stage: Option<String>,
    attempts: i64,
    enqueued_at: String,
    updated_at: String,
    last_error_code: Option<String>,
    locked: bool,
    queue_running: bool,
}

fn item(
    job: ProcessingQueueJob,
    title: Option<String>,
    unlocked: bool,
    queue_running: bool,
) -> ProcessingQueueItem {
    ProcessingQueueItem {
        meeting_id: job.meeting_id,
        title: if unlocked {
            title
                .filter(|t| !t.trim().is_empty())
                .unwrap_or_else(|| "Untitled recording".into())
        } else {
            "Locked recording".into()
        },
        state: job.state,
        position: job.position,
        stage: if unlocked { job.stage } else { None },
        attempts: job.attempts,
        enqueued_at: job.enqueued_at,
        updated_at: job.updated_at,
        last_error_code: if unlocked { job.error_code } else { None },
        locked: !unlocked,
        queue_running,
    }
}

#[tauri::command]
pub async fn list_processing_queue(app: AppHandle) -> Result<Vec<ProcessingQueueItem>> {
    super::offload_read(app, |state| {
        let _lifecycle = super::lifecycle_guard(state);
        let queue_running = state.processing_queue_running.load(Ordering::Acquire);
        state
            .db
            .list_processing_queue()?
            .into_iter()
            .map(|job| {
                let unlocked = super::meeting_is_unlocked(state, &job.meeting_id)?;
                let title = if unlocked {
                    state.db.get_meeting(&job.meeting_id)?.and_then(|m| m.title)
                } else {
                    None
                };
                Ok(item(job, title, unlocked, queue_running))
            })
            .collect()
    })
    .await
}

fn validate_selection(state: &AppState, ids: &[String], allow_processing: bool) -> Result<()> {
    if ids.is_empty() || ids.len() > MAX_SELECTION {
        return Err(AppError::InvalidArg(
            "select between 1 and 500 queue jobs".into(),
        ));
    }
    let mut seen = HashSet::new();
    for id in ids {
        if id.len() > 128 || !seen.insert(id) {
            return Err(AppError::InvalidArg("invalid or duplicate queue id".into()));
        }
        if !super::meeting_is_unlocked(state, id)? {
            return Err(AppError::Locked(
                "unlock this recording before changing its queue job".into(),
            ));
        }
        let job = state
            .db
            .processing_job(id)?
            .ok_or_else(|| AppError::InvalidArg("unknown queue job".into()))?;
        if !allow_processing && job.state == ProcessingQueueState::Processing {
            return Err(AppError::InvalidArg(
                "this queue job is already processing".into(),
            ));
        }
    }
    Ok(())
}

struct WorkerPermit {
    running: Arc<AtomicBool>,
    app: Option<AppHandle>,
}
impl Drop for WorkerPermit {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
        // This must follow release: the UI's final read must observe the free lane,
        // including yields/errors where all remaining rows are still waiting.
        if let Some(app) = &self.app {
            emit_processing_queue_changed(app);
        }
    }
}

fn reserve_worker(state: &AppState, app: Option<AppHandle>) -> Result<WorkerPermit> {
    state
        .processing_queue_running
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .map_err(|_| AppError::Unavailable("a processing queue run is already active".into()))?;
    Ok(WorkerPermit {
        running: state.processing_queue_running.clone(),
        app,
    })
}

fn prepare_run(
    app: &AppHandle,
    state: &AppState,
    ids: &[String],
    retry: bool,
) -> Result<(WorkerPermit, Vec<String>)> {
    let _lifecycle = super::lifecycle_guard(state);
    if crate::perf::recording_has_priority() {
        return Err(AppError::Unavailable(
            "finish recording before running queued work".into(),
        ));
    }
    validate_selection(state, ids, false)?;
    let permit = reserve_worker(state, Some(app.clone()))?;
    let now = chrono::Utc::now().to_rfc3339();
    let selected: HashSet<&str> = ids.iter().map(String::as_str).collect();
    let jobs = state.db.list_processing_queue()?;
    let ordered: Vec<String> = jobs
        .iter()
        .filter(|job| selected.contains(job.meeting_id.as_str()))
        .map(|job| job.meeting_id.clone())
        .collect();
    if !retry
        && jobs.iter().any(|job| {
            selected.contains(job.meeting_id.as_str()) && job.state == ProcessingQueueState::Failed
        })
    {
        return Err(AppError::InvalidArg(
            "retry failed jobs before processing".into(),
        ));
    }
    if retry {
        state.db.retry_processing_jobs(&ordered, &now)?;
    }
    // Run promotes the selected canonical-order block; unrelated waiting jobs keep
    // their relative order, and a later yield retains this newly chosen position.
    state.db.promote_processing_jobs(&ordered, &now)?;
    Ok((permit, ordered))
}

#[tauri::command]
pub fn process_queue_now(
    app: AppHandle,
    state: State<'_, AppState>,
    meeting_ids: Vec<String>,
) -> Result<()> {
    let (permit, ordered) = prepare_run(&app, state.inner(), &meeting_ids, false)?;
    spawn_worker(app, ordered, permit);
    Ok(())
}

#[tauri::command]
pub fn retry_processing_queue(
    app: AppHandle,
    state: State<'_, AppState>,
    meeting_ids: Vec<String>,
) -> Result<()> {
    let (permit, ordered) = prepare_run(&app, state.inner(), &meeting_ids, true)?;
    spawn_worker(app, ordered, permit);
    Ok(())
}

fn claim_selected(state: &AppState, id: &str) -> Result<std::path::PathBuf> {
    let _lifecycle = super::lifecycle_guard(state);
    if crate::perf::recording_has_priority() {
        return Err(AppError::Unavailable(
            "recording has priority; queued work remains waiting".into(),
        ));
    }
    validate_selection(state, &[id.to_owned()], false)?;
    super::reconcile_released_generation_cleanup(state, id)?;
    if state.db.meeting_has_recording_recovery_ownership(id)? {
        return Err(AppError::Unavailable(
            "recording recovery still owns this audio".into(),
        ));
    }
    let meeting = state
        .db
        .get_meeting(id)?
        .ok_or_else(|| AppError::InvalidArg("recording no longer exists".into()))?;
    let path = meeting
        .audio_path
        .ok_or_else(|| AppError::Storage("recording archive is missing".into()))?;
    if path.ends_with(".enc") {
        return Err(AppError::Locked(
            "unlock the recording archive first".into(),
        ));
    }
    if !std::path::Path::new(&path).is_file() {
        return Err(AppError::Storage("recording archive is missing".into()));
    }
    if !state
        .db
        .claim_processing_job(id, &chrono::Utc::now().to_rfc3339())?
    {
        return Err(AppError::Unavailable(
            "queue job is no longer waiting".into(),
        ));
    }
    Ok(path.into())
}

fn spawn_worker(app: AppHandle, ids: Vec<String>, permit: WorkerPermit) {
    emit_processing_queue_changed(&app);
    tauri::async_runtime::spawn(async move {
        let _permit = permit;
        for id in ids {
            let state = app.state::<AppState>();
            let path = match claim_selected(state.inner(), &id) {
                Ok(path) => path,
                Err(AppError::Unavailable(_)) | Err(AppError::Locked(_)) => break,
                Err(_) => {
                    let _ = state.db.fail_processing_job(
                        &id,
                        "audio_unavailable",
                        &chrono::Utc::now().to_rfc3339(),
                    );
                    emit_processing_queue_changed(&app);
                    continue;
                }
            };
            emit_processing_queue_changed(&app);
            let task_app = app.clone();
            let task_id = id.clone();
            // Detached inner task turns an unwind into a stable failed job, without cancelling the
            // selected snapshot or leaving the process-wide worker latch permanently true.
            let outcome = tauri::async_runtime::spawn(async move {
                let state = task_app.state::<AppState>();
                let guard = crate::pipeline::TerminalStatusGuard::arm(
                    Some(task_app.clone()),
                    state.db.clone(),
                    &task_id,
                );
                let result =
                    crate::pipeline::run_salvage_from_disk(&task_app, &state, &task_id, &path)
                        .await;
                if result.as_ref().is_err_and(crate::pipeline::is_queue_yield) {
                    // Persist the pause before disarming terminal cleanup: if this write fails,
                    // the ordinary failure path remains responsible for an honest terminal state.
                    state
                        .db
                        .park_processing_job(&task_id, &chrono::Utc::now().to_rfc3339())?;
                    guard.disarm();
                } else if result.is_ok() {
                    guard.disarm();
                }
                result.map(|_| ())
            })
            .await;
            match outcome {
                Ok(Err(error)) if crate::pipeline::is_queue_yield(&error) => {
                    emit_processing_queue_changed(&app);
                    break;
                }
                Ok(Ok(())) => {
                    let _ = state.db.remove_processing_job(&id);
                }
                _ => {
                    let _ = state.db.fail_processing_job(
                        &id,
                        "processing_failed",
                        &chrono::Utc::now().to_rfc3339(),
                    );
                }
            }
            emit_processing_queue_changed(&app);
        }
    });
}

#[tauri::command]
pub fn remove_processing_queue(
    app: AppHandle,
    state: State<'_, AppState>,
    meeting_ids: Vec<String>,
) -> Result<()> {
    let _lifecycle = super::lifecycle_guard(state.inner());
    validate_selection(state.inner(), &meeting_ids, false)?;
    if state.processing_queue_running.load(Ordering::Acquire) {
        return Err(AppError::Unavailable(
            "wait for the current queue run to finish".into(),
        ));
    }
    state.db.remove_processing_jobs(&meeting_ids)?;
    emit_processing_queue_changed(&app);
    Ok(())
}

#[tauri::command]
pub fn reorder_processing_queue(
    app: AppHandle,
    state: State<'_, AppState>,
    ordered_meeting_ids: Vec<String>,
) -> Result<()> {
    reorder_processing_queue_inner(state.inner(), &ordered_meeting_ids)?;
    emit_processing_queue_changed(&app);
    Ok(())
}

fn reorder_processing_queue_inner(state: &AppState, ordered_meeting_ids: &[String]) -> Result<()> {
    let _lifecycle = super::lifecycle_guard(state);
    // The worker owns a canonical-order snapshot for its whole run, including the
    // gaps between claims when no row is marked Processing. Match removal's latch.
    if state.processing_queue_running.load(Ordering::Acquire) {
        return Err(AppError::Unavailable(
            "wait for the current queue run to finish".into(),
        ));
    }
    if ordered_meeting_ids.is_empty() || ordered_meeting_ids.len() > MAX_SELECTION {
        return Err(AppError::InvalidArg("invalid queue order length".into()));
    }
    let current = state.db.list_processing_queue()?;
    let moved: Vec<String> = ordered_meeting_ids
        .iter()
        .enumerate()
        .filter(|(position, id)| current.get(*position).map(|job| &job.meeting_id) != Some(*id))
        .map(|(_, id)| id.clone())
        .collect();
    if !moved.is_empty() {
        validate_selection(state, &moved, false)?;
    }
    state
        .db
        .reorder_processing_queue(ordered_meeting_ids, &chrono::Utc::now().to_rfc3339())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::AppConfig;
    use crate::storage::{Db, Meeting, MeetingStatus};
    use std::collections::HashMap;
    use std::sync::Mutex;

    fn build_state(tag: &str) -> AppState {
        crate::commands::dev_kek_fixture::ensure_dev_kek();
        let db_path =
            crate::storage::db::unique_temp_path(&format!("murmur-trash-{tag}"), "sqlite");
        let _ = std::fs::remove_file(&db_path);
        let db =
            Db::open_with_key(&db_path, &"0123456789abcdef".repeat(4)).expect("open trash test db");
        db.migrate().expect("migrate trash test db");
        AppState {
            recorder: Mutex::new(None),
            recording_stop: Mutex::new(None),
            voice_listener: Mutex::new(None),
            voice_listener_lifecycle: Mutex::new(()),
            recording_starting: std::sync::atomic::AtomicBool::new(false),
            voice_command_capture: Mutex::new(None),
            pending_manual_command: Mutex::new(None),
            live_running: std::sync::atomic::AtomicBool::new(false),
            db: Arc::new(db),
            config: Arc::new(Mutex::new(AppConfig::default())),
            reasoner: crate::reason::ReasonerCell::fixed(Arc::new(crate::reason::StubReasoner)),
            current_meeting: Mutex::new(None),
            focus_meeting: Mutex::new(None),
            live_transcript: Mutex::new(String::new()),
            live_transcript_lines: std::sync::Mutex::new(Default::default()),
            processing_queue_running: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
                false,
            )),
            live_bullets: Mutex::new(String::new()),
            live_bullets_tracker: Mutex::new(crate::transcribe::bullets::BulletsTracker::default()),
            capped_notified: std::sync::atomic::AtomicBool::new(false),
            capture_fault_notified: std::sync::atomic::AtomicBool::new(false),
            reactions_shadow_count: std::sync::atomic::AtomicU64::new(0),
            reactions_emitted: Mutex::new(HashSet::new()),
            in_flight_turns: Mutex::new(HashMap::new()),
            user_turn_in_progress: std::sync::atomic::AtomicBool::new(false),
            verify_cache: Mutex::new(HashMap::new()),
            unlocked_folders: Arc::new(Mutex::new(HashSet::new())),
            master_kek: Mutex::new(None),
            org_ock_cache: Mutex::new(HashMap::new()),
            account_session: Mutex::new(None),
            share_refresh_lock: tokio::sync::Mutex::new(()),
            org_share_mutation_lock: tokio::sync::Mutex::new(()),
            lifecycle: Mutex::new(()),
            active_salvages: Mutex::new(HashSet::new()),
            seal_epoch: std::sync::atomic::AtomicU64::new(0),
            heavy_inference: Arc::new(tokio::sync::Semaphore::new(1)),
        }
    }

    #[test]
    fn active_worker_keeps_its_selected_order_until_permit_release() {
        let state = build_state("queue-order-permit");
        for id in ["a", "b", "c"] {
            state
                .db
                .insert_meeting(&Meeting {
                    id: id.into(),
                    started_at: "now".into(),
                    ended_at: Some("now".into()),
                    title: None,
                    duration_s: 0,
                    audio_path: None,
                    status: MeetingStatus::Draft,
                    folder_id: None,
                })
                .unwrap();
            state.db.enqueue_processing_job(id, "now").unwrap();
        }
        let permit = reserve_worker(&state, None).unwrap();
        let job = state.db.processing_job("a").unwrap().unwrap();
        let dto = item(
            job,
            None,
            true,
            state.processing_queue_running.load(Ordering::Acquire),
        );
        assert_eq!(serde_json::to_value(dto).unwrap()["queueRunning"], true);
        // All rows are waiting between claims; the permit, not a Processing row,
        // must protect the worker's selected order for its complete lifetime.
        let changed_order = vec!["a".into(), "c".into(), "b".into()];
        assert!(
            matches!(
                reorder_processing_queue_inner(&state, &changed_order),
                Err(AppError::Unavailable(_))
            ),
            "a worker must never execute a stale snapshot after a successful reorder"
        );
        let ids: Vec<_> = state
            .db
            .list_processing_queue()
            .unwrap()
            .into_iter()
            .map(|job| job.meeting_id)
            .collect();
        assert_eq!(ids, ["a", "b", "c"]);
        drop(permit);
        assert!(!state.processing_queue_running.load(Ordering::Acquire));
        reorder_processing_queue_inner(&state, &changed_order).unwrap();
        let ids: Vec<_> = state
            .db
            .list_processing_queue()
            .unwrap()
            .into_iter()
            .map(|job| job.meeting_id)
            .collect();
        assert_eq!(ids, changed_order);
    }
    #[test]
    fn deferred_stop_result_has_frontend_disposition() {
        let value = serde_json::to_value(super::super::StopResult {
            meeting_id: "opaque".into(),
            processing_disposition: Some("queued".into()),
        })
        .unwrap();
        assert_eq!(
            value,
            serde_json::json!({"meetingId":"opaque", "processingDisposition":"queued"})
        );
    }

    #[test]
    fn queue_dto_masks_content_and_serializes_exact_frontend_keys() {
        let dto = item(
            ProcessingQueueJob {
                meeting_id: "opaque".into(),
                state: ProcessingQueueState::Queued,
                position: 0,
                stage: Some("transcribing".into()),
                attempts: 1,
                error_code: Some("processing_failed".into()),
                enqueued_at: "now".into(),
                updated_at: "now".into(),
            },
            Some("private meeting title".into()),
            false,
            false,
        );
        let value = serde_json::to_value(dto).unwrap();
        assert_eq!(value["title"], "Locked recording");
        assert_eq!(value["locked"], true);
        assert_eq!(value["queueRunning"], false);
        assert!(value["lastErrorCode"].is_null());
        assert!(value["stage"].is_null());
        assert!(value.get("meetingId").is_some());
        assert!(value.get("enqueuedAt").is_some());
        assert!(value.get("updatedAt").is_some());
        assert!(value.as_object().unwrap().keys().all(|k| !k.contains('_')));
        assert!(!value.to_string().contains("private"));
    }
}
