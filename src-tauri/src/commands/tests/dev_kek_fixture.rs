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
//! The name and the value both come from [`crate::secrets::keychain`], next to the code that READS
//! the hatch, so this fixture and the harness runtime probe in `commands::reminders` cannot drift
//! apart. An earlier version of this module merely *claimed* they could not disagree while both
//! hand-typed their own literal; a reviewer changed one of them and nothing failed.

use crate::secrets::keychain::{dev_kek_hatch_value, DEV_KEK_ENV};
use std::sync::Once;

/// The single debug KEK for the whole test process.
pub(crate) fn dev_kek() -> String {
    dev_kek_hatch_value()
}

static KEK_ENV: Once = Once::new();

/// Install the debug KEK exactly once per process. Safe to call from any test's setup.
///
/// The assertion afterwards is deliberate defence in depth. The source guard below cannot see a
/// writer that spells the variable in some way it does not recognise, but ANY such writer shows up
/// here as a live value that is not ours — so the first test to call this after a rogue write fails
/// with a message that names the problem, instead of some unrelated test failing on a wrong key.
pub(crate) fn ensure_dev_kek() {
    KEK_ENV.call_once(|| {
        // SAFETY: installed before any test builds an `AppState`, and only ever to this one value.
        std::env::set_var(DEV_KEK_ENV, dev_kek());
    });
    let live = std::env::var(DEV_KEK_ENV).unwrap_or_default();
    assert_eq!(
        live,
        dev_kek(),
        "something else installed a different {DEV_KEK_ENV} in this process — `cargo test --lib` \
         runs one process, so that silently re-keys whichever tests lose the race"
    );
}

/// The hatch may be NAMED in exactly these files, and nowhere else.
///
/// # Why a file allowlist rather than parsing the writers
///
/// The first version of this guard tried to recognise writers and compare the keys they installed.
/// A reviewer broke it three separate ways in minutes: a module that re-declared its own local
/// `dev_kek()` (a plain `git revert` of one file) was classified as this fixture and skipped; a
/// writer that named the variable through a `const` was invisible, because the literal and the
/// `set_var` sat on different lines; and `use std::env::set_var as sv` defeated the substring test
/// for the call itself. Each bypass was cheaper to write than the check that was supposed to catch
/// it — which is the general shape of scanning source for a *pattern*.
///
/// Naming the small set of files that may mention the hatch at all inverts that. It does not try to
/// understand the code; it fails on any new mention anywhere else, whatever the mention looks like.
/// Every one of those three bypasses re-introduces the literal into a file that is not on this list.
///
/// A determined author can still assemble the string (`concat!("MURMUR", "_DEV_KEK")`). That is not
/// worth defending against: this guard exists to catch the accidental second writer — a revert, a
/// cherry-pick, a copied test-setup block — not somebody working around it on purpose. This module
/// itself is not on the list, and does not need to be: it reaches the hatch through `DEV_KEK_ENV`
/// like every other caller should, and builds its search needle in pieces so it cannot match its
/// own source.
const FILES_THAT_MAY_NAME_THE_HATCH: &[&str] = &[
    // Owns the hatch: declares its name and value, and reads it.
    "secrets/keychain.rs",
    // Strips it from every spawned provider's environment (`NEVER_INHERIT_ENV`).
    "summarize/claude_code.rs",
];

#[test]
fn only_the_fixture_and_the_hatch_owner_may_name_the_dev_kek() {
    use std::path::{Path, PathBuf};

    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let needle = concat!("\"MURMUR", "_DEV_KEK\"");

    fn walk(dir: &Path, needle: &str, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("read src dir") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                walk(&path, needle, out);
            } else if path.extension().is_some_and(|e| e == "rs")
                && std::fs::read_to_string(&path)
                    .expect("read source file")
                    .contains(needle)
            {
                out.push(path);
            }
        }
    }

    let mut found = Vec::new();
    walk(&src, needle, &mut found);
    let found: std::collections::BTreeSet<String> = found
        .iter()
        .map(|p| {
            p.strip_prefix(&src)
                .unwrap_or(p)
                .display()
                .to_string()
                .replace('\\', "/")
        })
        .collect();
    let allowed: std::collections::BTreeSet<String> = FILES_THAT_MAY_NAME_THE_HATCH
        .iter()
        .map(|s| (*s).to_string())
        .collect();

    // Anti-vacuity, in BOTH directions. A scanner that finds nothing would otherwise pass forever,
    // and an allowlist entry whose file stopped naming the hatch is a stale rule pretending to
    // guard something.
    assert!(
        !found.is_empty(),
        "the scanner found no mention of the hatch at all — it has gone vacuous, fix the scan \
         before trusting this test"
    );
    assert_eq!(
        found, allowed,
        "the debug KEK hatch may be named only by the files that own it and by this fixture. A new \
         file naming it is a second writer, and in a one-process `cargo test --lib` run a second \
         writer silently re-keys whichever tests lose the race. Install it with \
         `dev_kek_fixture::ensure_dev_kek()` instead — or, if this really is a legitimate new home \
         for the hatch, add it to FILES_THAT_MAY_NAME_THE_HATCH with a reason."
    );
}
