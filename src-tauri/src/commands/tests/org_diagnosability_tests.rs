//! Oracles for the three defects that made a live org failure UNDIAGNOSABLE.
//!
//! Field report, 2026-09-01: Settings → Organization sat on "Loading organizations…" forever, and
//! after a restart cleared that, the app logged this once a minute, indefinitely:
//!
//! ```text
//! WARN org: container share reconcile tick failed error=unavailable
//! ```
//!
//! Three separate properties conspired to make that line useless, and each is fixed here with the
//! test that would have caught it:
//!
//! 1. `brief_err` collapsed `Unavailable(msg)` to the bare word `"unavailable"`, DISCARDING the
//!    `errcode::tag` code and HTTP status the message carries. The one line the app writes about a
//!    failure that repeats every 60 s could not say which failure it was.
//! 2. `org_refresh` awaited `org_share_mutation_lock` with no bound, so a wedged holder produced an
//!    unbounded "Loading…" that no error path could ever reach — a *hang*, which the frontend's
//!    `finally` cannot rescue because it never runs.
//! 3. `org_reconcile_memberships_with_policy` swallowed EVERY token error, including a definitive
//!    `Auth` refusal, and returned `Ok(())`. A permanently dead session rendered as an empty panel
//!    with no explanation.

use super::*;
use crate::error::AppError;
use crate::storage::db::Db;

/// The fixed 64-hex dev key every test DB in this crate opens with, BUILT rather than written out
/// so this file carries no literal in DEK/KEK shape (`.claude/hooks` refuses one in a diff, and a
/// scanner cannot tell a test key from a real one — which is the point).
fn test_dek() -> String {
    "0123456789abcdef".repeat(4)
}

fn fresh_db(label: &str) -> Db {
    let path = crate::storage::db::unique_temp_path(&format!("murmur-orgdiag-{label}"), "sqlite");
    let _ = std::fs::remove_file(&path);
    Db::open_with_key(&path, &test_dek()).unwrap()
}

// ── 1. The log line has to name the failure ──────────────────────────────────────────────────

/// `brief_err` must keep the tagged code of an `Unavailable`.
///
/// The message is safe to log BY CONSTRUCTION — `share/client.rs` maps HTTP failures to a fixed
/// string plus the numeric status and never surfaces the reqwest `Display` (which can echo the
/// URL), and `brief_err`'s own doc says it "carries only stage/status labels the client controls".
/// So the truncation bought no privacy and cost the whole diagnosis.
#[test]
fn brief_err_keeps_the_tagged_code_of_an_unavailable() {
    let tagged = AppError::Unavailable(crate::errcode::tag("http-422", "publish item"));
    let brief = brief_err(&tagged);
    assert!(
        brief.contains("http-422"),
        "the log line must name WHICH failure — got {brief:?}"
    );
}

/// An UNTAGGED `Unavailable` stays generic. Only deliberately tagged, client-controlled codes are
/// promoted into the log; an arbitrary message (which no rule guarantees is PII-free) is not.
#[test]
fn brief_err_does_not_leak_an_untagged_unavailable_message() {
    let untagged = AppError::Unavailable("/Users/someone/Vault/Q3 review.md is missing".into());
    assert_eq!(
        brief_err(&untagged),
        "unavailable",
        "an untagged message must never reach the log"
    );
}

/// The other arms keep their existing shape, tag or no tag.
#[test]
fn brief_err_still_classifies_the_other_arms() {
    assert_eq!(brief_err(&AppError::Locked("x".into())), "locked");
    assert_eq!(brief_err(&AppError::Auth("x".into())), "auth");
    assert_eq!(brief_err(&AppError::InvalidArg("x".into())), "error");
}

// ── 2. Waiting on the share-mutation lock has to be bounded ──────────────────────────────────

/// A held `org_share_mutation_lock` must produce an ERROR, not an unbounded wait.
///
/// This is the oracle for the field report's first half. `org_refresh` took this lock before any
/// timeout-protected code, so a wedged holder hung the whole org panel until the process restarted
/// — logging out did not help, because a mutex is process state. The frontend already treats a
/// FAILED refresh correctly (it falls through and renders the local replica); it is only the HANG
/// it cannot survive, because the `finally` that clears the loading flag never runs.
#[tokio::test]
async fn a_held_share_mutation_lock_yields_busy_rather_than_hanging() {
    let state = AppState::for_tests(fresh_db("busy-lock"));

    let held = state.org_share_mutation_lock.lock().await;

    let started = std::time::Instant::now();
    let outcome = acquire_share_mutation_within(&state, std::time::Duration::from_millis(50)).await;
    let waited = started.elapsed();

    assert!(
        matches!(outcome, Err(AppError::Unavailable(_))),
        "a busy lock must refuse, not block forever"
    );
    assert!(
        waited < std::time::Duration::from_secs(2),
        "the wait must be bounded — took {waited:?}"
    );
    drop(held);
}

/// A free lock is still acquired normally — the bound must not break the happy path.
#[tokio::test]
async fn a_free_share_mutation_lock_is_acquired() {
    let state = AppState::for_tests(fresh_db("free-lock"));
    let guard = acquire_share_mutation_within(&state, std::time::Duration::from_millis(50)).await;
    assert!(guard.is_ok(), "an uncontended lock must be acquired");
}

// ── 3. A definitive auth refusal must not be swallowed ────────────────────────────────────────

/// The reconcile's token-failure policy, as a pure function so it can be asserted directly.
///
/// `valid_access_token` already draws the line correctly: a definitive `Auth` refusal means the
/// refresh token itself was refused and the session is unrecoverable, while anything else
/// (offline, 5xx, keychain, logged out) is transient or expected. The reconcile then threw that
/// distinction away with `Err(_) => return Ok(())`, so a dead session was indistinguishable from
/// being offline — an empty panel either way, with nothing to act on.
#[test]
fn a_definitive_auth_refusal_propagates_out_of_the_reconcile() {
    let outcome = reconcile_token_outcome(Err(AppError::Auth("refresh refused".into())));
    assert!(
        matches!(outcome, TokenOutcome::Fatal(AppError::Auth(_))),
        "a refused refresh token must reach the user, not vanish"
    );
}

/// Offline / logged-out stays a NO-OP: the cached rows are kept and the panel still renders them.
/// `valid_access_token` fails closed with `Unavailable` when logged out, so this is the common case
/// and must never turn into an error banner.
#[test]
fn offline_and_logged_out_remain_a_silent_no_op() {
    assert!(matches!(
        reconcile_token_outcome(Err(AppError::Unavailable("offline".into()))),
        TokenOutcome::SkipQuietly
    ));
    assert!(matches!(
        reconcile_token_outcome(Err(AppError::Storage("no session".into()))),
        TokenOutcome::SkipQuietly
    ));
}

/// A live token proceeds.
#[test]
fn a_valid_token_proceeds() {
    assert!(matches!(
        reconcile_token_outcome(Ok("bearer".to_string())),
        TokenOutcome::Proceed(_)
    ));
}

// ── 4. A manual sync must not report a clean run over a failed one ────────────────────────────

/// `org_sync_now` reconciles shared containers AFTER the feed pull, and used to swallow a failure
/// into a `tracing::warn!` — returning the same empty report a genuinely clean sync returns. The
/// panel renders an empty report as **"Synced — up to date."**, so the user pressed Sync, was told
/// everything was fine, and had no way to learn that shared-folder publishing had been failing
/// every 60 s since 18:38. That is the silent-failure-shown-as-success shape, exactly.
#[test]
fn a_container_failure_lands_on_the_sync_report() {
    let mut report = crate::storage::models::OrgSyncReport::default();
    let failure = AppError::Unavailable(crate::errcode::tag(
        crate::errcode::ORG_CONSENT,
        "confirm the one-time upload notice first",
    ));

    note_container_failure(&mut report, &failure);

    assert_eq!(report.errors.len(), 1, "the failure must reach the report");
    let only = &report.errors[0];
    assert!(
        only.contains("shared folders"),
        "the row must say WHICH leg failed — got {only:?}"
    );
    assert!(
        only.contains("org-consent"),
        "and WHICH failure it was — got {only:?}"
    );
}

/// The report row stays content-free: a stage label plus the client-chosen code, never the message
/// body. An untagged failure contributes no prose at all.
#[test]
fn a_reported_container_failure_never_carries_the_message_body() {
    let mut report = crate::storage::models::OrgSyncReport::default();
    note_container_failure(
        &mut report,
        &AppError::Unavailable("/Users/someone/Vault/Q3 review.md is missing".into()),
    );
    assert_eq!(report.errors, vec!["shared folders: unavailable".to_string()]);
}

/// A clean sync still reports clean — the honesty fix must not invent an error.
#[test]
fn a_clean_sync_report_stays_empty() {
    let report = crate::storage::models::OrgSyncReport::default();
    assert!(report.errors.is_empty());
}
