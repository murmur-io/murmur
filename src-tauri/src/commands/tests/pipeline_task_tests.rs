use super::*;

/// The panic-mapping contract of the detached pipeline execution (2026-07-16 wedge fix): a
/// PANIC inside the spawned pipeline task must surface from `await_pipeline_task` as a real
/// `Err(AppError)` — which is what makes the JS invoke Promise REJECT (FE catch → "error")
/// instead of never settling. Pre-fix there was no spawn at all: the panic unwound through
/// the command future, tokio swallowed it at the task boundary, and the Promise hung forever.
#[tokio::test]
async fn pipeline_task_panic_maps_to_a_rejecting_err() {
    let task: tauri::async_runtime::JoinHandle<Result<u32, AppError>> =
        tauri::async_runtime::spawn(async { panic!("simulated pipeline panic") });
    let res = await_pipeline_task(task).await;
    match res {
        Err(AppError::Other(e)) => {
            let msg = e.to_string();
            assert!(
                msg.contains("pipeline crashed"),
                "the mapped error must say the pipeline crashed (got: {msg})"
            );
        }
        other => panic!("a panicked pipeline task must map to Err(AppError::Other), got {other:?}"),
    }
}

/// Ok and Err results of the spawned task pass through `await_pipeline_task` unchanged —
/// the join mapping only covers the panic case.
#[tokio::test]
async fn pipeline_task_results_pass_through_unchanged() {
    let ok = tauri::async_runtime::spawn(async { Ok::<u32, AppError>(42) });
    assert_eq!(await_pipeline_task(ok).await.unwrap(), 42);

    let err = tauri::async_runtime::spawn(async {
        Err::<u32, AppError>(AppError::Transcribe("real stage failure".into()))
    });
    match await_pipeline_task(err).await {
        Err(AppError::Transcribe(msg)) => assert_eq!(msg, "real stage failure"),
        other => panic!("an inner Err must pass through unchanged, got {other:?}"),
    }
}
