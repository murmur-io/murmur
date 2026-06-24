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
    #[error("secrets error: {0}")]
    Secrets(String),
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
