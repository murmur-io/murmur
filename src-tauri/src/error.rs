use serde::Serialize;

/// The ONE error type. Every fallible fn in this crate returns [`Result<T>`].
///
/// # The `[code]` convention (read before adding a user-facing failure)
///
/// `AppError` serializes across IPC as its bare `Display` string, which means the message body is
/// **developer prose** — it is not, and must never be, what the user reads. The frontend renders a
/// generic sentence for any error it does not recognise.
///
/// A failure that IS meant to reach a banner or a toast opts in by carrying a stable machine code
/// at the front of its body, via [`crate::errcode::tag`]:
///
/// ```ignore
/// return Err(AppError::Locked(errcode::tag(
///     errcode::NOTE_LOCKED,
///     format!("note {id} is in a locked folder"),
/// )));
/// // → "locked: [note-locked] note n1 is in a locked folder"
/// ```
///
/// `src/app/core/copy/error-copy.service.ts` strips the variant tag, reads the `[code]`, and owns
/// the sentence. See [`crate::errcode`] for the full allowlist and the rules.
///
/// **Do not hand-build error strings the FE has to parse.** The code is the contract; the prose
/// after it is free to change.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("audio capture error: {0}")]
    Audio(String),
    #[error("transcription error: {0}")]
    Transcribe(String),
    #[error("summarizer error: {0}")]
    Summarize(String),
    #[error("export error: {0}")]
    Export(String),
    #[error("storage error: {0}")]
    Storage(String),
    #[error("migration error: {0}")]
    Migration(String),
    #[error("authentication error: {0}")]
    Auth(String),
    #[error("locked: {0}")]
    Locked(String),
    #[error("secrets error: {0}")]
    Secrets(String),
    /// macOS Keychain access was denied or the secure store was unreachable at runtime (the user
    /// clicked "Deny" on the keychain prompt, or the keychain is locked). Distinct from
    /// [`AppError::Secrets`] so startup can branch on the "couldn't reach the keychain" case and
    /// show a specific, non-technical message instead of crashing.
    #[error("keychain access denied: {0}")]
    KeychainDenied(String),
    /// The biometric-gated master KEK read was cancelled or failed at the Touch ID / passcode sheet
    /// (the user pressed Cancel, the prompt timed out, or `errSecUserCanceled` / `errSecAuthFailed`
    /// came back from `SecItemCopyMatching`). Distinct from [`AppError::KeychainDenied`] (the legacy
    /// password-prompt deny) so the unlock flow can show "Touch ID was cancelled — try again" rather
    /// than a generic keychain error, and from [`AppError::Auth`] so callers can tell a presence
    /// failure from an internal auth bug. Carries only the OSStatus / context — never the key value.
    #[error("biometric authentication failed or was cancelled: {0}")]
    BiometricFailed(String),
    #[error("config error: {0}")]
    Config(String),
    #[error("provider unavailable: {0}")]
    Unavailable(String),
    #[error("invalid argument: {0}")]
    InvalidArg(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, AppError>;

// IPC: AppError serializes to a string message so Tauri commands can return it.
impl Serialize for AppError {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}
