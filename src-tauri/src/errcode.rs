//! The `[code]` contract — the ONE machine-readable channel between a Rust failure and the
//! sentence the user reads.
//!
//! # Why this exists
//!
//! [`crate::error::AppError`] is `Serialize` and crosses IPC as its bare `to_string()`
//! (`error.rs`), so historically the frontend had exactly one thing to work with: English prose
//! written for a developer. Two bad habits grew out of that:
//!
//! 1. Components *prose-matched* the message to decide behaviour — `record.component.ts` decided
//!    whether to show the cloud-consent Allow banner by regex-matching `cloud egress not
//!    consented`. Rewording the Rust string silently broke the consent flow for every cloud user.
//! 2. Components *rendered* the message. There are ~2100 `AppError::*` constructions in this
//!    crate and a large minority carry vocabulary that must never reach a user ("brain sidecar
//!    stdin missing", "account-session mutex poisoned", "E2EE decrypt/authentication failed",
//!    "HKDF expand failed").
//!
//! The fix is a small, explicit allowlist. A failure that *is* meant to reach a banner or a toast
//! carries a stable `[code]` at the front of its message body; the frontend
//! (`src/app/core/copy/error-copy.service.ts`) maps that code to copy it owns, and renders a
//! generic sentence for **anything** un-coded. Deny-by-default: adding a code is a deliberate act,
//! never a heuristic.
//!
//! # The wire shape
//!
//! ```text
//! AppError::Locked(tag(NOTE_LOCKED, "note {id} is in a locked folder"))
//!   → to_string() → "locked: [note-locked] note n1 is in a locked folder"
//! ```
//!
//! The variant tag (`locked: `) is still there — it is part of `AppError`'s `Display` and stays.
//! The frontend strips the 14 known variant prefixes, then reads the leading `[code]`.
//!
//! # Rules
//!
//! - A code is stable API. Renaming one is a breaking change that must land in the same commit as
//!   the frontend map, exactly like a wire format.
//! - The message body after the code is still developer prose. It is NOT rendered; keep it useful
//!   for logs and never put PII in it.
//! - Codes are lowercase kebab-case so the frontend's extractor can be a strict pattern rather
//!   than a sniff.
//!
//! # Prose couplings that are NOT codes (recorded so a de-jargon PR cannot break them silently)
//!
//! `brain.component.ts::friendlyImportError` used to match the document-import failures by prose,
//! in a deliberate order (`no text found` had to beat `unsupported document type`). Those are now
//! codes ([`DOC_NO_TEXT`], [`DOC_UNSUPPORTED`], …) and the ordering lives in the frontend's code
//! map. If you reword one of those message bodies, nothing breaks. If you *remove* its
//! [`tag`] call, the user gets the generic sentence — silently. Grep this module's constants
//! before touching a message that has one.
//!
//! **ONE prose coupling survives, recorded here so it cannot break silently:**
//! `commands/export.rs`'s `"this meeting has no master for that stream"` is read by
//! `src/app/features/detail/detail/detail.component.ts::masterErrorMessage` with a `/no master/`
//! test, to pick between the mic and system wordings. It has no other consumer and is not a
//! failure the user can act on beyond knowing it, so it did not earn a code — but rewording it to
//! drop the words "no master" WILL change what that surface renders.

/// Prefix a message body with its stable machine code.
///
/// Always used at the `AppError::*` construction site, never after the fact — the code must
/// travel with the error through every `?`.
pub fn tag(code: &str, msg: impl AsRef<str>) -> String {
    format!("[{code}] {}", msg.as_ref())
}

// ── Egress / consent ────────────────────────────────────────────────────────────────────────

/// The fail-closed cloud-egress consent gate refused (`summarize::make_provider_resolved`).
/// The Record screen turns this into the "Allow" banner rather than an error.
pub const CLOUD_CONSENT: &str = "cloud-consent";
/// The one-time link-share upload consent has not been granted.
pub const SHARE_CONSENT: &str = "share-consent";
/// The one-time org ("Shared Brain") upload consent has not been granted.
pub const ORG_CONSENT: &str = "org-consent";

// ── The lock gate ───────────────────────────────────────────────────────────────────────────

/// A note lives in a folder that is sealed and not unlocked this session.
pub const NOTE_LOCKED: &str = "note-locked";
/// A meeting is sealed and not unlocked this session.
pub const MEETING_LOCKED: &str = "meeting-locked";
/// A brain document is in a folder that is sealed and not unlocked this session.
pub const DOC_LOCKED: &str = "doc-locked";
/// The target folder itself is locked (accepting a share into it, moving into it, …).
pub const FOLDER_LOCKED: &str = "folder-locked";

// ── Missing rows ────────────────────────────────────────────────────────────────────────────

/// The note id does not exist — a stale tab or bookmark after a delete.
pub const NOTE_MISSING: &str = "note-missing";
/// The note-folder id does not exist.
pub const NOTE_FOLDER_MISSING: &str = "note-folder-missing";

// ── Document import / extraction ────────────────────────────────────────────────────────────

/// The file extension is not on the import allowlist.
pub const DOC_UNSUPPORTED: &str = "doc-unsupported";
/// Extraction ran (including OCR) and produced nothing usable.
pub const DOC_NO_TEXT: &str = "doc-no-text";
/// The archive expands past the safe import ceiling (decompression-bomb guard).
pub const DOC_TOO_LARGE: &str = "doc-too-large";
/// The PDF is password-protected.
pub const DOC_PASSWORD: &str = "doc-password";
/// The file is corrupt / malformed / could not be opened.
pub const DOC_UNREADABLE: &str = "doc-unreadable";

// ── Touch ID / Keychain ─────────────────────────────────────────────────────────────────────

/// The Touch ID sheet was cancelled by the user (or timed out).
pub const TOUCH_ID_CANCELLED: &str = "touch-id-cancelled";
/// The user-presence check ran and failed (not a cancel).
pub const TOUCH_ID_FAILED: &str = "touch-id-failed";
/// The Keychain refused or was unreachable (locked keychain, denied prompt).
pub const KEYCHAIN_DENIED: &str = "keychain-denied";

// ── The sharing server (murmur-server) ──────────────────────────────────────────────────────

/// The sharing server could not be reached, or answered 5xx. A CONNECTIVITY problem.
pub const SHARING_UNREACHABLE: &str = "sharing-unreachable";
/// The sharing server rate-limited the request (429).
pub const SHARING_RATE_LIMITED: &str = "sharing-rate-limited";
/// The sharing server rejected the request (4xx) — a wrong or expired code, a used token.
pub const SHARING_REJECTED: &str = "sharing-rejected";
/// A Shared Brain edit lost its optimistic-concurrency race. The draft remains local.
pub const ORG_EDIT_CONFLICT: &str = "org-edit-conflict";
/// The sharing session is gone (401) — the user must sign in again.
pub const SHARING_SIGNIN_REQUIRED: &str = "sharing-signin-required";
/// The configured relay has not advertised the owner-bound share reservation contract required to
/// prevent a delayed create from resurrecting ciphertext after local deletion/lock.
pub const SHARING_UPGRADE_REQUIRED: &str = "sharing-upgrade-required";

// ── macOS permissions ───────────────────────────────────────────────────────────────────────

/// The Reminders app refused the write — the user has not granted Reminders access.
pub const REMINDERS_DENIED: &str = "reminders-denied";

/// Every code this crate emits, in declaration order. The frontend's allowlist mirrors it;
/// `error_codes_are_unique_and_kebab_case` keeps the shape a machine can rely on.
pub const ALL: &[&str] = &[
    CLOUD_CONSENT,
    SHARE_CONSENT,
    ORG_CONSENT,
    NOTE_LOCKED,
    MEETING_LOCKED,
    DOC_LOCKED,
    FOLDER_LOCKED,
    NOTE_MISSING,
    NOTE_FOLDER_MISSING,
    DOC_UNSUPPORTED,
    DOC_NO_TEXT,
    DOC_TOO_LARGE,
    DOC_PASSWORD,
    DOC_UNREADABLE,
    TOUCH_ID_CANCELLED,
    TOUCH_ID_FAILED,
    KEYCHAIN_DENIED,
    SHARING_UNREACHABLE,
    SHARING_RATE_LIMITED,
    SHARING_REJECTED,
    ORG_EDIT_CONFLICT,
    SHARING_SIGNIN_REQUIRED,
    SHARING_UPGRADE_REQUIRED,
    REMINDERS_DENIED,
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::AppError;

    #[test]
    fn tag_puts_the_code_first_in_the_body() {
        assert_eq!(
            tag(NOTE_LOCKED, "note n1 is sealed"),
            "[note-locked] note n1 is sealed"
        );
    }

    /// The frontend extractor reads the code AFTER stripping the `AppError` variant tag, so the
    /// full wire string must be exactly `<variant tag>: [code] <body>`.
    #[test]
    fn the_wire_string_carries_the_code_after_the_variant_tag() {
        let e = AppError::Locked(tag(NOTE_LOCKED, "note n1 is sealed"));
        assert_eq!(e.to_string(), "locked: [note-locked] note n1 is sealed");

        let e = AppError::Unavailable(tag(CLOUD_CONSENT, "not consented"));
        assert_eq!(
            e.to_string(),
            "provider unavailable: [cloud-consent] not consented"
        );
    }

    #[test]
    fn error_codes_are_unique_and_kebab_case() {
        let mut seen: Vec<&str> = Vec::new();
        for &code in ALL {
            assert!(!seen.contains(&code), "duplicate error code {code}");
            seen.push(code);
            assert!(!code.is_empty(), "empty error code");
            assert!(
                code.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "error code {code} must be lowercase kebab-case"
            );
            assert!(
                !code.starts_with('-') && !code.ends_with('-'),
                "bad hyphen in {code}"
            );
        }
    }

    /// Guards the ONE thing a rename would break: the frontend map keys. This list is duplicated
    /// verbatim in `src/app/core/copy/error-copy.service.ts::ERROR_CODES`; changing it here
    /// without changing it there ships a generic sentence where copy used to be.
    #[test]
    fn the_code_set_is_pinned() {
        // Compared as SLICES on both sides: `&[&str]` vs a fixed-size array reference is not a
        // `PartialEq` pair that always resolves, and a test that fails to compile guards nothing.
        let expected: &[&str] = &[
            "cloud-consent",
            "share-consent",
            "org-consent",
            "note-locked",
            "meeting-locked",
            "doc-locked",
            "folder-locked",
            "note-missing",
            "note-folder-missing",
            "doc-unsupported",
            "doc-no-text",
            "doc-too-large",
            "doc-password",
            "doc-unreadable",
            "touch-id-cancelled",
            "touch-id-failed",
            "keychain-denied",
            "sharing-unreachable",
            "sharing-rate-limited",
            "sharing-rejected",
            "org-edit-conflict",
            "sharing-signin-required",
            "sharing-upgrade-required",
            "reminders-denied",
        ];
        assert_eq!(ALL, expected);
    }
}
