//! Oracles for WHERE `org_share_mutation_lock` is acquired.
//!
//! The lock serializes org mutation/revoke against share dispatch, and that job needs it held
//! across a short durable commit — not across a human or a network. Two acquisitions were doing the
//! latter (2026-09-02 audit, O4):
//!
//!   * `unlock_folder` took it in its FIRST statement, then presented the Touch ID sheet ~30 lines
//!     later. Every org mutation in the process waited for as long as the user took to answer that
//!     dialog, or forever if they never did.
//!   * `org_background_sync_tick` took it once and ran FOUR network phases under it, so a user
//!     sharing or revoking could wait behind four consecutive HTTP timeouts on a 60 s tick.
//!
//! These are SOURCE-ORDER oracles, and that is a deliberate, stated limitation: the properties are
//! "the lock is not held across X", and there is no in-process way to assert that without a seam
//! for the human prompt and for each HTTP phase. Asserting the acquisition's POSITION is the honest
//! proxy, and it catches the regression that actually happens — someone hoists the acquisition back
//! to the top of the function.
//!
//! Comments are stripped before matching. An earlier oracle of mine in this repo asked whether a
//! body CONTAINED a call's name and was defeated in one move by putting that name in a comment;
//! searching for text is always cheaper to fool than to defend.

use std::path::PathBuf;

fn commands_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("commands")
}

/// Blank out `//` and `/* */`, respecting string literals so a `"http://…"` survives intact.
fn strip_comments(source: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    let mut out = String::with_capacity(source.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '"' {
            out.push(c);
            i += 1;
            while i < chars.len() {
                if chars[i] == '\\' {
                    i += 2;
                    continue;
                }
                out.push(chars[i]);
                let done = chars[i] == '"';
                i += 1;
                if done {
                    break;
                }
            }
            continue;
        }
        if c == '/' && i + 1 < chars.len() && chars[i + 1] == '/' {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && i + 1 < chars.len() && chars[i + 1] == '*' {
            i += 2;
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                if chars[i] == '\n' {
                    out.push('\n');
                }
                i += 1;
            }
            i += 2;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// The body of `fn <name>(` up to its matching closing brace, comments already stripped.
fn body_of(file: &str, name: &str) -> String {
    let source = strip_comments(
        &std::fs::read_to_string(commands_dir().join(file))
            .unwrap_or_else(|e| panic!("cannot read {file}: {e}")),
    );
    let sig = format!("fn {name}(");
    let start = source
        .find(&sig)
        .unwrap_or_else(|| panic!("{file}: `{name}` not found — update this oracle deliberately"));
    let open = start + source[start..].find('{').expect("no body");
    let mut depth = 0usize;
    for (offset, ch) in source[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return source[open..=open + offset].to_string();
                }
            }
            _ => {}
        }
    }
    panic!("{file}: `{name}` has no closing brace");
}

const ACQUIRE: &str = "org_share_mutation_lock";

/// `unlock_folder` must not hold the org mutex across the Touch ID sheet.
///
/// RED CONTROL (run 2026-09-03, observed): hoisting the acquisition back to the command's first
/// statement fails with "acquires `org_share_mutation_lock` at byte 32, BEFORE the biometric KEK
/// release at 1180".
#[test]
fn unlock_folder_takes_the_org_mutex_only_after_the_biometric_kek_release() {
    let body = body_of("lock.rs", "unlock_folder");
    let kek = body
        .find("master_kek_with_policy")
        .expect("unlock_folder no longer resolves the KEK — this oracle is stale, fix it");
    let acquire = body
        .find(ACQUIRE)
        .expect("unlock_folder no longer takes the org mutex — was that deliberate?");
    assert!(
        acquire > kek,
        "unlock_folder acquires `{ACQUIRE}` at byte {acquire}, BEFORE the biometric KEK release at \
         {kek}. That holds a process-wide org mutex across the Touch ID sheet — for as long as the \
         user takes to answer it, and indefinitely if they never do."
    );
}

/// The background tick must take the org mutex per phase, never once around all four.
///
/// Asserts on brace DEPTH, not on a count: what makes the old shape wrong is that the acquisition
/// sat at the function's own level and so outlived every phase. A count would pass just as happily
/// for one acquisition at the top plus one nested.
///
/// RED CONTROL (run 2026-09-03, observed): restoring the single top-level acquisition ALONGSIDE the
/// per-phase ones fails with "depths seen: [1, 2, 2, 2, 2]" — which is exactly why this asserts on
/// depth and not on a count. A count-based check would have passed that mutation happily (5 >= 2).
#[test]
fn the_org_background_tick_never_holds_the_mutex_across_a_network_phase() {
    let body = body_of("org.rs", "org_background_sync_tick");
    let mut depth = 0usize;
    let mut acquisitions = Vec::new();
    let chars: Vec<char> = body.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            _ => {
                if body[i..].starts_with(ACQUIRE) {
                    acquisitions.push(depth);
                    i += ACQUIRE.len();
                    continue;
                }
            }
        }
        i += 1;
    }
    assert!(
        !acquisitions.is_empty(),
        "the tick no longer takes `{ACQUIRE}` at all — was that deliberate?"
    );
    // Depth 1 is the function body itself; anything acquired there is held for the whole tick.
    assert!(
        !acquisitions.contains(&1),
        "the tick acquires `{ACQUIRE}` at the function's own scope (depths seen: {acquisitions:?}), \
         so it is held across every network phase in the tick. Take it inside each phase's block \
         instead: a user sharing or revoking then waits behind at most one HTTP timeout, not four."
    );
    assert!(
        acquisitions.len() >= 2,
        "expected one acquisition per network phase, found {} (depths {acquisitions:?})",
        acquisitions.len()
    );
}

/// Positions in `body` where an `org_share_mutation_lock` guard is still IN SCOPE.
///
/// Models the guard's lifetime rather than its mere presence: an acquisition at brace depth `d`
/// lives until the block at depth `d` closes. Returns every byte offset at which at least one guard
/// is held.
fn guard_is_held_at(body: &str) -> Vec<(usize, bool)> {
    let chars: Vec<char> = body.chars().collect();
    let mut depth = 0usize;
    let mut open_guards: Vec<usize> = Vec::new();
    let mut marks = Vec::with_capacity(chars.len());
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '{' => depth += 1,
            '}' => {
                depth = depth.saturating_sub(1);
                open_guards.retain(|d| *d <= depth);
            }
            _ => {
                if body[i..].starts_with(ACQUIRE) {
                    open_guards.push(depth);
                }
            }
        }
        marks.push((i, !open_guards.is_empty()));
        i += 1;
    }
    marks
}

/// The container-reconcile phase must NOT run under `org_share_mutation_lock`.
///
/// `reconcile_container_shares` -> `reconcile_one_container_root` -> `share_to_org_placed_notifying`,
/// whose FIRST statement acquires this same mutex. `tokio::sync::Mutex` is not reentrant, so holding
/// it across this phase makes the task await a second acquisition of a mutex it already holds: it
/// never returns, the guard is never dropped, and every later org operation blocks for the rest of
/// the process. There is no timeout around the tick, so it does not recover.
///
/// This is why the FE's own "sync now" (`sync_container_shares`) calls the same function with no
/// lock — the serialization lives inside `share_to_org_placed_notifying`.
///
/// RED CONTROL (run 2026-09-03): wrapping the call in `{ let _m = …lock().await; … }` — the shape
/// this PR shipped before review — fails this test.
#[test]
fn the_container_reconcile_phase_does_not_run_under_the_org_mutex() {
    let body = body_of("org.rs", "org_background_sync_tick");
    let call = body
        .find("reconcile_container_shares(")
        .expect("the tick no longer reconciles container shares — update this oracle deliberately");
    let held = guard_is_held_at(&body);
    let (_, holding) = held[call];
    assert!(
        !holding,
        "`reconcile_container_shares` is called while `{ACQUIRE}` is held. Its callee \
         `share_to_org_placed_notifying` acquires the same non-reentrant mutex as its first \
         statement, so this deadlocks permanently and takes every later org operation with it."
    );
}

/// `unlock_folder` must RE-VALIDATE after taking the mutex, not merely serialize the write.
///
/// Its `folder`, `folder.locked` refusal, `preflight_unlock_subtree` and `folder_wrapped_key` are
/// all read before any lock is held, with an unbounded Touch ID wait in between. A `lock_folder`
/// landing in that window takes its already-locked idempotent branch, bumps the seal epoch and
/// returns Ok — the user sees a successful "Lock". Without the re-check the unlock then resumes on
/// stale reads and restores plaintext, silently undoing it.
///
/// RED CONTROL (run 2026-09-03): deleting the
/// `require_current_content_visibility_snapshot` call fails this test.
#[test]
fn unlock_folder_revalidates_the_visibility_snapshot_after_taking_the_org_mutex() {
    let body = body_of("lock.rs", "unlock_folder");
    let capture = body
        .find("capture_content_visibility_snapshot")
        .expect("unlock_folder no longer captures a visibility snapshot");
    let first_read = body
        .find("folder_by_id")
        .expect("unlock_folder no longer reads the folder — this oracle is stale");
    let acquire = body.find(ACQUIRE).expect("no org mutex acquisition");
    let require = body
        .find("require_current_content_visibility_snapshot")
        .expect(
            "unlock_folder takes the org mutex late but never re-validates: a `lock_folder` that \
             ran during the Touch ID sheet would be silently undone",
        );
    assert!(
        capture < first_read,
        "the snapshot must be captured BEFORE the reads it protects (capture {capture}, read \
         {first_read})"
    );
    assert!(
        acquire < require,
        "the re-validation must happen AFTER the mutex is held (acquire {acquire}, require \
         {require}), or it can be invalidated again before the restore"
    );
}
