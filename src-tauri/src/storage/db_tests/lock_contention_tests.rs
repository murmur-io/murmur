//! Oracles for the ONE `Mutex<Connection>` every DB access funnels through.
//!
//! Before these, the cost of that single connection was UNOBSERVABLE: nothing recorded how long
//! anyone waited on it, so "the app freezes while a delete cascade runs" could only be argued from
//! feel. That is why the 2026-09-02 audit's S1 finding could not be closed with a number until now.

use super::*;

const TEST_DEK: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn file_db(label: &str) -> Db {
    Db::open_with_key(
        &super::unique_temp_path(&format!("meetnotes-lock-contention-{label}"), "sqlite"),
        TEST_DEK,
    )
    .unwrap()
}

/// A read that waits behind a long write is COUNTED and TIMED.
///
/// Asserts a BEFORE/AFTER delta rather than resetting the counters: `cargo test --lib` runs the
/// whole suite in one process, so a reset would race every other test's DB work — and the race
/// would be invisible under `cargo nextest` (a process per test), which is what CI runs. A delta
/// is correct under both runners.
///
/// RED CONTROL (run 2026-09-03, observed): deleting the `db_lock_stats().record_wait(waited)` call
/// in `Db::lock` fails this test on "a real wait must increment the contended counter (0 -> 0)"
/// while the wall-clock assertion still PASSES — i.e. it distinguishes "the wait happened" from
/// "the wait was measured", which is the property under test. Without that discrimination the test
/// would only be proving that a mutex blocks.
#[test]
fn a_read_waiting_behind_a_long_write_is_measured_not_just_slow() {
    use std::sync::mpsc;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    const HELD: Duration = Duration::from_millis(250);
    // Generous floor: the assertion is "the wait was real and recorded", not a timing benchmark.
    const FLOOR: Duration = Duration::from_millis(120);

    let db = Arc::new(file_db("waits"));
    let (contended_before, _, _, _) = crate::storage::db::db_lock_stats().snapshot();

    let holder = Arc::clone(&db);
    let (held_tx, held_rx) = mpsc::channel();
    let writer = std::thread::spawn(move || {
        let _guard = holder.lock();
        held_tx.send(()).unwrap();
        std::thread::sleep(HELD);
    });

    held_rx.recv().expect("writer never took the connection");
    let started = Instant::now();
    let guard = db.lock();
    let waited = started.elapsed();
    drop(guard);
    writer.join().unwrap();

    let (contended_after, _, _, max_wait_us) = crate::storage::db::db_lock_stats().snapshot();
    assert!(
        waited >= FLOOR,
        "the reader should have waited for the writer, waited {waited:?}"
    );
    assert!(
        contended_after > contended_before,
        "a real wait must increment the contended counter ({contended_before} -> {contended_after})"
    );
    assert!(
        max_wait_us >= FLOOR.as_micros() as u64,
        "the recorded max wait ({max_wait_us}us) must reflect a wait of at least {FLOOR:?}"
    );
}

/// The uncontended path is counted, and counted CHEAPLY — one atomic, no clock read.
///
/// This is the half that keeps the instrumentation honest about its own cost: an accounting layer
/// that charged every access would be a perf regression sold as a perf fix.
///
/// Asserts a DELTA of at least the acquisitions this test performs. The earlier version also
/// asserted that the wait totals were "monotonic", which adversarial review showed to be
/// tautological — those counters are only ever `fetch_add`/`fetch_max`ed and there is no `reset`
/// anywhere in the crate, so that assertion was true by construction for any two ordered snapshots
/// and could never fail. It is gone; a check that cannot fail is not a check, it is decoration that
/// makes a test look stronger than it is.
///
/// RED CONTROL (run 2026-09-03, observed by the reviewer): making `record_immediate` a no-op fails
/// this test — so the surviving assertion does carry signal.
#[test]
fn an_uncontended_read_costs_no_wait_accounting() {
    const ACQUISITIONS: u64 = 50;

    let db = file_db("free");
    let (_, uncontended_before, _, _) = crate::storage::db::db_lock_stats().snapshot();
    for _ in 0..ACQUISITIONS {
        drop(db.lock());
    }
    let (_, uncontended_after, _, _) = crate::storage::db::db_lock_stats().snapshot();
    assert!(
        uncontended_after - uncontended_before >= ACQUISITIONS,
        "{ACQUISITIONS} uncontended acquisitions must be counted, delta was {}",
        uncontended_after - uncontended_before
    );
}
