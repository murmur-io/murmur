use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MeetingStatus {
    Draft,
    Recording,
    Transcribed,
    Summarized,
    Exported,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Meeting {
    /// uuid
    pub id: String,
    /// ISO 8601
    pub started_at: String,
    pub ended_at: Option<String>,
    pub title: Option<String>,
    pub duration_s: i64,
    pub audio_path: Option<String>,
    pub status: MeetingStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteRecord {
    pub meeting_id: String,
    pub provider_id: String,
    pub markdown: String,
    pub created_at: String,
    pub exported_path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusCount {
    pub status: String,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DayCount {
    /// "YYYY-MM-DD"
    pub date: String,
    pub count: i64,
    pub duration_s: i64,
}

/// Aggregate stats for the dashboard + Analytics tab.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Analytics {
    pub total_meetings: i64,
    pub total_duration_s: i64,
    pub avg_duration_s: i64,
    pub longest_duration_s: i64,
    pub meetings_7d: i64,
    pub duration_7d_s: i64,
    pub notes_count: i64,
    pub first_meeting_at: Option<String>,
    pub by_status: Vec<StatusCount>,
    /// Per-day activity for the last ~30 days (only days with meetings).
    pub per_day: Vec<DayCount>,
}
