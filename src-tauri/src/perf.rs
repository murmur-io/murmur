//! Shared "one heavy inference at a time" gate — see `AppState::heavy_inference`'s doc comment
//! for the full rationale. `spawn_blocking` alone gets CPU-bound native work off the async
//! runtime but is NOT a concurrency limiter (Tokio's own guidance: an unbounded blocking pool
//! means nothing stops N heavy calls from running simultaneously and fighting each other for the
//! same RAM/Metal context). Every native-runtime call site that loads/runs a heavy ML model
//! (whisper ASR, the diarizer, the Candle embedder/NER, a brain-sidecar dispatch) should route
//! through [`run_heavy`] instead of calling `tokio::task::spawn_blocking` directly.

use std::sync::Arc;

use crate::error::{AppError, Result};

/// Acquire the ONE global heavy-inference permit (`AppState::heavy_inference`), then run `f` on
/// the blocking thread pool. The permit is held for the duration of `f` and released when this
/// future resolves (Ok or Err) — a second heavy call queues behind it rather than running
/// concurrently. `f`'s own `Result<T>` (whatever `AppError` variant its domain uses) passes
/// through unchanged; only the semaphore-closed / task-panicked cases surface as
/// [`AppError::Other`]. Takes the semaphore directly (not the whole `AppState`) so it's
/// independently unit-testable and so a caller that only has the `Arc` (not a full `&AppState`)
/// can still route through it.
pub async fn run_heavy<F, T>(semaphore: &Arc<tokio::sync::Semaphore>, f: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    let _permit = semaphore
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| AppError::Other(anyhow::anyhow!("heavy-inference semaphore closed")))?;
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("heavy inference task panicked: {e}")))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    /// The core contract: two `run_heavy` calls sharing ONE semaphore never overlap — the second
    /// call's closure only starts once the first has fully finished, even though both are handed
    /// to the blocking pool "concurrently" (a pool with >1 thread would otherwise happily run
    /// them at the same time). Proves the serialization is real, not just "it compiles".
    #[tokio::test]
    async fn run_heavy_serializes_two_concurrent_calls() {
        let sem = Arc::new(tokio::sync::Semaphore::new(1));
        let concurrent: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(0));
        let max_concurrent: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(0));

        let make_task = |sem: Arc<tokio::sync::Semaphore>,
                          concurrent: Arc<AtomicUsize>,
                          max_concurrent: Arc<AtomicUsize>| {
            tokio::spawn(async move {
                run_heavy(&sem, move || -> Result<()> {
                    let now = concurrent.fetch_add(1, Ordering::SeqCst) + 1;
                    max_concurrent.fetch_max(now, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(50));
                    concurrent.fetch_sub(1, Ordering::SeqCst);
                    Ok(())
                })
                .await
            })
        };

        let t1 = make_task(sem.clone(), concurrent.clone(), max_concurrent.clone());
        let t2 = make_task(sem.clone(), concurrent.clone(), max_concurrent.clone());
        t1.await.unwrap().unwrap();
        t2.await.unwrap().unwrap();

        assert_eq!(
            max_concurrent.load(Ordering::SeqCst),
            1,
            "two run_heavy calls on the same semaphore must never overlap"
        );
    }

    /// The permit must release even when `f` returns `Err` — a failed heavy op must not
    /// permanently wedge every future heavy call behind a semaphore that never frees up.
    #[tokio::test]
    async fn run_heavy_releases_the_permit_on_error() {
        let sem = Arc::new(tokio::sync::Semaphore::new(1));

        let first: Result<()> = run_heavy(&sem, || Err(AppError::Other(anyhow::anyhow!("boom")))).await;
        assert!(first.is_err());

        // A second call must still be able to acquire — proves the failed first call didn't leak
        // its permit. Bounded by a timeout so a real leak fails the test instead of hanging it.
        let second = tokio::time::timeout(Duration::from_secs(2), run_heavy(&sem, || Ok(42))).await;
        assert_eq!(second.unwrap().unwrap(), 42, "the permit must be free again after an Err");
    }
}
