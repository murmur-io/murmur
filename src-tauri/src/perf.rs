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
    let permit = semaphore
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| AppError::Other(anyhow::anyhow!("heavy-inference semaphore closed")))?;
    tokio::task::spawn_blocking(move || {
        // The permit is moved INTO the blocking closure (2026-07-16) — not held by this async
        // future. If the CALLER's future is dropped mid-await (e.g. the pipeline's ASR watchdog
        // `tokio::time::timeout` firing), the orphaned blocking closure keeps running to
        // completion (blocking closures are not cancellable) and MUST keep the one
        // heavy-inference permit until it actually finishes — otherwise a newly-started heavy
        // call would co-run with the orphan (the whisper/diarizer co-residency class this
        // semaphore exists to prevent). Held-in-future vs held-in-closure is identical on the
        // normal Ok/Err paths (the JoinHandle resolves only after the closure ends).
        let _permit = permit;
        f()
    })
    .await
    .map_err(|e| AppError::Other(anyhow::anyhow!("heavy inference task panicked: {e}")))?
}

/// macOS kernel memory-pressure level via `sysctl -n kern.memorystatus_vm_pressure_level` — the
/// SAME no-new-crate/no-new-FFI shell-out convention as `total_ram_bytes`/`available_ram_bytes`
/// (`transcribe/model.rs`, `reason/sidecar.rs`). `1` = normal, `2` = warn, `4` = critical.
///
/// This is a DIFFERENT signal than the existing `vm_stat`-derived free/inactive/speculative
/// arithmetic: that answers "does THIS job's footprint fit the numbers we can see", this answers
/// "does the KERNEL ITSELF already think the whole system is under pressure" (e.g. another app
/// about to be jetsam-killed, or pressure that hasn't yet shown up as reduced free/inactive
/// pages). Meant to be consulted ALONGSIDE an existing RAM-floor check, not instead of it — see
/// [`heavy_op_permitted`].
///
/// Returns `None` on ANY parse/exec failure — callers must fail OPEN on `None`, matching every
/// other RAM-probe convention in this codebase (a broken measurement must never silently refuse
/// a legitimate task).
fn kernel_memory_pressure_level() -> Option<u8> {
    let out = std::process::Command::new("sysctl")
        .arg("-n")
        .arg("kern.memorystatus_vm_pressure_level")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()?.trim().parse().ok()
}

/// Combine an existing RAM-floor verdict (`ram_floor_ok`, e.g. `topic_backfill_ram_permits_now()`
/// or `parakeet_ram_permits_now()`) with the kernel's own pressure signal — refuses ONLY when the
/// floor check already said no, OR the kernel reports CRITICAL (4) pressure. `WARN` (2) is common
/// and deliberately NOT itself a refusal (per the research brief this codifies: blocking on WARN
/// would be over-eager for a user-initiated action like starting a recording). Fails OPEN on a
/// broken pressure probe — a `None` never adds a NEW refusal on top of an already-permitting floor
/// check.
pub fn heavy_op_permitted(ram_floor_ok: bool) -> bool {
    if !ram_floor_ok {
        return false;
    }
    kernel_memory_pressure_level().map_or(true, |level| level < 4)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    /// The deterministic half of `heavy_op_permitted`'s logic: an already-failing RAM floor
    /// short-circuits to `false` regardless of the kernel pressure signal (which we can't
    /// deterministically mock — it shells out to the real `sysctl` on whatever machine runs this
    /// test). This is the ONE branch fully testable without depending on real system state.
    #[test]
    fn heavy_op_permitted_short_circuits_on_a_failing_ram_floor() {
        assert!(
            !heavy_op_permitted(false),
            "an already-failing RAM floor must refuse regardless of kernel pressure"
        );
    }

    /// `kernel_memory_pressure_level` must never panic and must return a value in the documented
    /// range (or None on a broken probe) — the one thing we CAN assert about the real machine
    /// this test runs on, without asserting a specific pressure level (which is real system state,
    /// not something this test controls).
    #[test]
    fn kernel_memory_pressure_level_never_panics() {
        if let Some(level) = kernel_memory_pressure_level() {
            assert!(
                level == 1 || level == 2 || level == 4,
                "unexpected kern.memorystatus_vm_pressure_level value: {level}"
            );
        }
    }

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

    /// 2026-07-16 (pipeline hang watchdog, RED-before-GREEN): when the CALLER's future is dropped
    /// mid-flight — exactly what `tokio::time::timeout` around the pipeline's ASR stage does — the
    /// orphaned `spawn_blocking` closure keeps running (blocking closures are not cancellable) and
    /// MUST keep the one heavy-inference permit until it actually finishes. Before the fix the
    /// permit lived in `run_heavy`'s async future, so the timeout-drop released it while the
    /// blocking thread still ran — letting a second heavy inference co-run with the orphan (the
    /// whisper/diarizer co-residency class this semaphore exists to prevent).
    #[tokio::test]
    async fn run_heavy_keeps_the_permit_while_an_orphaned_closure_still_runs() {
        let sem = Arc::new(tokio::sync::Semaphore::new(1));
        let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let finished_in = finished.clone();
        let fut = run_heavy(&sem, move || -> Result<()> {
            std::thread::sleep(Duration::from_millis(400));
            finished_in.store(true, Ordering::SeqCst);
            Ok(())
        });
        // Time the caller out long before the closure finishes — the future is DROPPED here,
        // but the blocking closure is already running (or queued) and will run to completion.
        let timed_out = tokio::time::timeout(Duration::from_millis(50), fut).await;
        assert!(timed_out.is_err(), "the caller must observe the timeout");

        // The orphaned closure is still running → the permit MUST still be held.
        assert!(
            !finished.load(Ordering::SeqCst),
            "closure must still be running at this point (timing precondition)"
        );
        assert_eq!(
            sem.available_permits(),
            0,
            "the permit must stay with the still-running orphaned closure, not the dropped future"
        );

        // Once the closure finishes, the permit is released (poll with a deadline — no flaky sleep).
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while sem.available_permits() == 0 && std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            finished.load(Ordering::SeqCst),
            "the orphaned closure must have run to completion"
        );
        assert_eq!(
            sem.available_permits(),
            1,
            "the permit must be released once the orphaned closure completes"
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
