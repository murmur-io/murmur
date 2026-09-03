//! The ONE debug-KEK every test in this crate runs under.
//!
//! # Why this module exists
//!
//! `MURMUR_DEV_KEK` is process-global. Under `cargo nextest` that is harmless, because nextest gives
//! every test its own process — which is exactly why CI never saw the problem. But the documented
//! inner loop in `CLAUDE.md` is `cargo test --lib`, which runs the whole suite in ONE process with
//! many threads. Five modules used to install the hatch themselves, each behind its own `Once`, and
//! four of them agreed on one key while `trash_tests` installed a different one. Whichever `Once`
//! happened to fire first won the variable for every test that followed, so a test could fail with
//! `Locked("decryption failed (wrong key…)")` purely because of thread scheduling — and pass on its
//! own. A suite that fails for reasons unrelated to the code under test is a suite people learn to
//! ignore.
//!
//! The value matches the one `prepare_reminder_runtime_probe_environment` already installs at
//! process entry for the harness runtime probe, so the test hatch and the probe hatch cannot
//! disagree either.
//!
//! # Why it is built rather than written out
//!
//! A literal 64-hex string in a diff is the shape this repo's secret scanner exists to catch. These
//! are throwaway keys for temp SQLCipher files, but they must not LOOK like a real DEK/KEK. (The
//! rationale is inherited verbatim from `trash_tests`, which got this right.)

use std::sync::Once;

/// The single debug KEK for the whole test process.
pub(crate) fn dev_kek() -> String {
    "1".repeat(64)
}

static KEK_ENV: Once = Once::new();

/// Install the debug KEK exactly once per process. Safe to call from any test's setup.
pub(crate) fn ensure_dev_kek() {
    KEK_ENV.call_once(|| {
        // SAFETY: installed before any test builds an `AppState`, and only ever to this one value —
        // the invariant `one_dev_kek_for_the_whole_test_process` below is what keeps it that way.
        std::env::set_var("MURMUR_DEV_KEK", dev_kek());
    });
}

/// Every `MURMUR_DEV_KEK` writer in the crate must agree on one value.
///
/// This is a SOURCE invariant on purpose. The bug it guards is real by construction — two different
/// values written to one process-global — but its symptom depends on which thread wins a race, so a
/// behavioural test would be a coin flip and a useless oracle. Reproducing the original failure took
/// a specific interleaving that a later run did not reproduce at all. Scanning the source is
/// deterministic: it fails the moment somebody adds a second key, whatever the scheduler does.
#[test]
fn one_dev_kek_for_the_whole_test_process() {
    use std::path::Path;

    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut writers: Vec<(String, String)> = Vec::new();

    fn walk(dir: &Path, out: &mut Vec<(String, String)>) {
        for entry in std::fs::read_dir(dir).expect("read src dir") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text = std::fs::read_to_string(&path).expect("read source file");
                for (i, line) in text.lines().enumerate() {
                    if line.contains("\"MURMUR_DEV_KEK\"") && text_sets_var(&text, i) {
                        // Record the file plus the 64-char run of a single repeated digit that this
                        // writer installs, if the key is spelled out nearby.
                        let window: String = text
                            .lines()
                            .skip(i)
                            .take(4)
                            .collect::<Vec<_>>()
                            .join(" ");
                        out.push((
                            path.strip_prefix(dir.parent().unwrap())
                                .unwrap_or(&path)
                                .display()
                                .to_string(),
                            distinct_key_shape(line, &window),
                        ));
                    }
                }
            }
        }
    }

    /// True when the `"MURMUR_DEV_KEK"` on line `i` belongs to a `set_var` call rather than a read
    /// (`var_os`) or a mention in prose.
    fn text_sets_var(text: &str, i: usize) -> bool {
        let lines: Vec<&str> = text.lines().collect();
        // A comment that merely NAMES the variable is not a writer — including this module's own
        // prose, which would otherwise make the scanner flag itself.
        if lines[i].trim_start().starts_with("//") {
            return false;
        }
        // `set_var` may sit on the line itself or on the line above (rustfmt splits long calls).
        let start = i.saturating_sub(1);
        lines[start..=i].iter().any(|l| l.contains("set_var"))
    }

    /// Collapse a writer's key to a comparable shape: a 64-long run of one repeated character, or
    /// the `repeat(64)` form, or `"fixture"` when it defers to this module.
    ///
    /// The fixture check reads the `set_var` LINE, never the window. Reading the window made this
    /// scanner vacuous: a writer sitting one line above a `ensure_dev_kek()` call was classified as
    /// the fixture and filtered out, so a deliberately planted second key passed. Mutation is how
    /// that surfaced — the first version of this test happily accepted the exact bug it exists to
    /// catch.
    fn distinct_key_shape(set_var_line: &str, window: &str) -> String {
        if set_var_line.contains("dev_kek()") {
            return "fixture".to_string();
        }
        for digit in '0'..='9' {
            let literal: String = std::iter::repeat(digit).take(64).collect();
            if window.contains(&literal) {
                return format!("literal:{digit}x64");
            }
            if window.contains(&format!("\"{digit}\".repeat(64)")) {
                return format!("repeat:{digit}x64");
            }
        }
        "unknown".to_string()
    }

    walk(&src, &mut writers);

    assert!(
        !writers.is_empty(),
        "the scanner found no MURMUR_DEV_KEK writers at all — it has gone vacuous, fix the scan \
         before trusting this test"
    );

    let shapes: std::collections::BTreeSet<&str> = writers
        .iter()
        .map(|(_, shape)| shape.as_str())
        .filter(|shape| *shape != "fixture")
        .collect();

    assert!(
        shapes.len() <= 1,
        "every MURMUR_DEV_KEK writer must install the SAME key — `cargo test --lib` runs one \
         process, so a second value silently re-keys whichever tests lose the race. Found: {writers:?}"
    );
}
