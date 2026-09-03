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
/// # What this scan is, and what it is NOT
///
/// It is a best-effort EARLY WARNING, not a proof. Three rounds of review established that the
/// question "where could a writer live?" has no bounded answer in Rust: `#[path]` compiles a file
/// from anywhere on disk into this binary, so any walker rooted at any directory is trusting a
/// boundary the language does not honour. Each round closed one escape (`.rs`-only filtering, then
/// `src/`-only rooting) and the next appeared. Rooting at the crate covers every escape that stays
/// inside the crate, which is where an accident actually lands; it does not cover a `#[path]` that
/// leaves it, and no amount of walking will.
///
/// The ACTUAL guard is the runtime assertion in [`ensure_dev_kek`]: it compares the live value and
/// therefore catches any writer, however it spells the call and wherever it lives, as soon as one
/// test calls the fixture after it. This scan exists to fail EARLIER and more legibly than that —
/// naming the offending file instead of surfacing as a wrong-key error somewhere unrelated.
///
/// # What this deliberately does NOT catch
///
/// Two gaps are known and accepted, because in both the runtime assertion in `ensure_dev_kek` fires
/// deterministically on the next call and names the problem:
///
/// - A name assembled at compile time (`concat!("MURMUR", "_DEV_KEK")`). Defending against this
///   means parsing, which is the approach that already failed.
/// - A second `set_var` inside a file that is ALREADY on the list. Trust here is file-grained, not
///   call-site-grained, so an allowlisted file is immune for its whole length — and `keychain.rs`,
///   the one place a careless KEK-adjacent `set_var` is most likely to be added, is exactly that
///   file. Review demonstrated both, and in both the runtime assertion caught it.
///
/// This module itself is not on the list, and does not need to be: it reaches the hatch through
/// `DEV_KEK_ENV` like every other caller should, and builds its search needle in pieces so it
/// cannot match its own source.
const FILES_THAT_MAY_NAME_THE_HATCH: &[&str] = &[
    // Owns the hatch: declares its name and value, and reads it.
    "src/secrets/keychain.rs",
    // Strips it from every spawned provider's environment (`NEVER_INHERIT_ENV`).
    "src/summarize/claude_code.rs",
];

#[test]
fn only_the_fixture_and_the_hatch_owner_may_name_the_dev_kek() {
    use std::path::{Path, PathBuf};

    // The CRATE ROOT, not `src/`. `#[path = "../../elsewhere.rs"]` is ordinary Rust and compiles a
    // file from anywhere into this very binary, so a walker rooted at `src/` trusts a directory
    // boundary the language does not honour. Review escaped to `src-tauri/` and watched the guard
    // report `ok` while the rogue test ran.
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf();
    let needle = concat!("\"MURMUR", "_DEV_KEK\"");

    // EVERY regular file, not just `*.rs`. Filtering by extension was a real hole: `#[path =
    // "tests/whatever.inc"] mod x;` is ordinary Rust, compiles into this very test binary, and its
    // `set_var` runs in this very process — while a `.rs`-only walker never even opens it. Review
    // built exactly that and watched the guard report `ok` while 620 of 3618 tests failed on the
    // corrupted key. A scanner whose blind spot is reachable by a normal language feature is worse
    // than no scanner, because it is trusted. Bytes rather than `read_to_string`, so a non-UTF-8
    // file is skipped over rather than panicking the guard.
    fn walk(dir: &Path, needle: &str, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("read src dir") {
            let path = entry.expect("dir entry").path();
            let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
            // Build output and VCS/tooling directories are not compiled into this binary and are
            // enormous; everything else under the crate root is fair game.
            if name == "target" || name == "gen" || name.starts_with('.') {
                continue;
            }
            if path.is_dir() {
                walk(&path, needle, out);
            } else {
                // A read failure is NOT "no match". Collapsing those two is the exact shape that
                // cost a reviewer 19 of 20 hits in an unrelated sweep this same week, and it is
                // reachable here without any adversarial code at all: review built a rogue file
                // while it was readable, ran `chmod 000`, and the guard went green while the
                // already-compiled test kept corrupting the key. Permission bits are not part of
                // Cargo's fingerprint, so nothing even rebuilds.
                let bytes = std::fs::read(&path).unwrap_or_else(|e| {
                    panic!(
                        "could not read {} while scanning for the debug-KEK hatch ({e}). Fix its \
                         permissions or exclude it deliberately — a file this guard cannot read is \
                         a file it cannot clear.",
                        path.display()
                    )
                });
                if String::from_utf8_lossy(&bytes).contains(needle) {
                    out.push(path);
                }
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
