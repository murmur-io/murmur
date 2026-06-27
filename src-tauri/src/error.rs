use serde::Serialize;

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
