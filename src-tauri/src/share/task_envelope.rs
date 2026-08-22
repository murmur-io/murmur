//! Canonical plaintext payload for one org-owned Task.
//!
//! The payload is serialized into `OrgEnvelope.markdown` while the outer envelope owns the
//! authenticated image bundle and the stable org-document revision. Keeping Tasks inside the
//! existing OCK envelope means the relay still sees only ciphertext and the org feed retains one
//! cursor for Notes and Tasks.

use serde::{Deserialize, Serialize};

use crate::error::{AppError, Result};

pub const TASK_ENVELOPE_VERSION: u16 = 1;
pub const MAX_TASK_TITLE_BYTES: usize = 512;
pub const MAX_TASK_DESCRIPTION_BYTES: usize = 128 * 1024;
pub const MAX_TASK_SUBTASKS: usize = 256;
pub const MAX_TASK_ORG_REFS: usize = 128;
pub const MAX_TASK_IMAGES: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TaskStatus {
    Todo,
    InProgress,
    Done,
}

impl TaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Todo => "todo",
            Self::InProgress => "inProgress",
            Self::Done => "done",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskSubtask {
    pub id: String,
    pub title: String,
    pub done: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskOrgRef {
    pub org_id: String,
    pub doc_id: String,
}

/// `reference` is Murmur's canonical attachment marker (`![alt](murmur-attachment://UUID)`). The outer
/// `OrgEnvelope.attachments` manifest carries the authenticated bytes. Keeping the token here lets
/// the existing verified attachment remapper replace wire UUIDs with collision-free local UUIDs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskImageRef {
    pub reference: String,
    pub alt: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskEnvelope {
    pub version: u16,
    pub org_id: String,
    pub title: String,
    pub description: String,
    pub status: TaskStatus,
    pub due_at: Option<String>,
    pub assignee_user_id: Option<String>,
    pub created_at: String,
    pub subtasks: Vec<TaskSubtask>,
    pub org_refs: Vec<TaskOrgRef>,
    pub images: Vec<TaskImageRef>,
}

impl TaskEnvelope {
    pub fn to_canonical_json(&self, org_id: &str) -> Result<String> {
        self.validate(org_id)?;
        serde_json::to_string(self)
            .map_err(|_| AppError::Unavailable("task payload encoding failed".into()))
    }

    pub fn from_json(value: &str, org_id: &str) -> Result<Self> {
        let parsed: Self = serde_json::from_str(value)
            .map_err(|_| AppError::InvalidArg("shared task payload is invalid".into()))?;
        parsed.validate(org_id)?;
        Ok(parsed)
    }

    pub fn validate(&self, org_id: &str) -> Result<()> {
        if self.version != TASK_ENVELOPE_VERSION {
            return Err(AppError::InvalidArg(
                "shared task payload has an unsupported version".into(),
            ));
        }
        if self.org_id != org_id || uuid::Uuid::parse_str(&self.org_id).is_err() {
            return Err(AppError::InvalidArg(
                "shared task organization does not match its encrypted document".into(),
            ));
        }
        let title = self.title.trim();
        if title.is_empty() || title.len() > MAX_TASK_TITLE_BYTES {
            return Err(AppError::InvalidArg(
                "task title is empty or exceeds the size limit".into(),
            ));
        }
        if self.description.len() > MAX_TASK_DESCRIPTION_BYTES {
            return Err(AppError::InvalidArg(
                "task description exceeds the size limit".into(),
            ));
        }
        if self.created_at.trim().is_empty()
            || chrono::DateTime::parse_from_rfc3339(&self.created_at).is_err()
        {
            return Err(AppError::InvalidArg("task createdAt is invalid".into()));
        }
        if self
            .due_at
            .as_deref()
            .is_some_and(|value| chrono::DateTime::parse_from_rfc3339(value).is_err())
        {
            return Err(AppError::InvalidArg("task dueAt is invalid".into()));
        }
        if self.subtasks.len() > MAX_TASK_SUBTASKS
            || self.org_refs.len() > MAX_TASK_ORG_REFS
            || self.images.len() > MAX_TASK_IMAGES
        {
            return Err(AppError::InvalidArg(
                "task payload exceeds its bounded collection limits".into(),
            ));
        }
        let mut subtask_ids = std::collections::HashSet::new();
        for row in &self.subtasks {
            if uuid::Uuid::parse_str(&row.id).is_err()
                || row.title.trim().is_empty()
                || row.title.len() > MAX_TASK_TITLE_BYTES
                || !subtask_ids.insert(row.id.as_str())
            {
                return Err(AppError::InvalidArg("task subtask is invalid".into()));
            }
        }
        let mut refs = std::collections::HashSet::new();
        for row in &self.org_refs {
            if row.org_id != org_id
                || uuid::Uuid::parse_str(&row.org_id).is_err()
                || uuid::Uuid::parse_str(&row.doc_id).is_err()
                || !refs.insert((row.org_id.as_str(), row.doc_id.as_str()))
            {
                return Err(AppError::InvalidArg(
                    "task references must be unique documents in the same organization".into(),
                ));
            }
        }
        let mut images = std::collections::HashSet::new();
        for row in &self.images {
            let token = row.reference.trim();
            let marker = token
                .strip_prefix("![")
                .and_then(|value| value.strip_suffix(')'))
                .and_then(|value| value.split_once("](murmur-attachment://"));
            let id = marker.map(|(_, id)| id);
            let label_is_canonical = marker.is_some_and(|(label, _)| {
                !label.is_empty()
                    && label.len() <= 160
                    && !label
                        .chars()
                        .any(|ch| matches!(ch, '\\' | '[' | ']' | '(' | ')' | '\r' | '\n' | '\t'))
            });
            let valid_id = id
                .and_then(|value| uuid::Uuid::parse_str(value).ok().map(|parsed| (value, parsed)))
                .is_some_and(|(value, parsed)| {
                    parsed.get_version_num() == 4 && parsed.to_string() == value
                });
            if !label_is_canonical
                || !valid_id
                || token.len() > 1024
                || row.alt.len() > MAX_TASK_TITLE_BYTES
                || !images.insert(id.unwrap_or_default())
            {
                return Err(AppError::InvalidArg("task image reference is invalid".into()));
            }
        }
        if self
            .assignee_user_id
            .as_deref()
            .is_some_and(|value| uuid::Uuid::parse_str(value).is_err())
        {
            return Err(AppError::InvalidArg("task assignee is invalid".into()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_envelope_round_trips_and_rejects_cross_org_refs() {
        let org_id = "00000000-0000-4000-8000-000000000001";
        let task = TaskEnvelope {
            version: TASK_ENVELOPE_VERSION,
            org_id: org_id.into(),
            title: "Ship Tasks".into(),
            description: "One encrypted document".into(),
            status: TaskStatus::InProgress,
            due_at: Some("2026-08-22T09:00:00Z".into()),
            assignee_user_id: Some("00000000-0000-4000-8000-000000000002".into()),
            created_at: "2026-08-21T09:00:00Z".into(),
            subtasks: vec![TaskSubtask {
                id: "00000000-0000-4000-8000-000000000003".into(),
                title: "Backend".into(),
                done: false,
            }],
            org_refs: vec![TaskOrgRef {
                org_id: org_id.into(),
                doc_id: "00000000-0000-4000-8000-000000000004".into(),
            }],
            images: vec![TaskImageRef {
                reference: "![diagram](murmur-attachment://00000000-0000-4000-8000-000000000005)"
                    .into(),
                alt: "diagram".into(),
            }],
        };
        let json = task.to_canonical_json(org_id).unwrap();
        assert_eq!(TaskEnvelope::from_json(&json, org_id).unwrap(), task);
        assert_eq!(
            crate::commands::referenced_attachment_ids(&json).unwrap(),
            std::collections::HashSet::from([
                "00000000-0000-4000-8000-000000000005".to_string()
            ])
        );
        let remapped = crate::share::envelope::remap_share_images(
            &json,
            &std::collections::HashMap::from([(
                "00000000-0000-4000-8000-000000000005".to_string(),
                "00000000-0000-4000-8000-000000000006".to_string(),
            )]),
        );
        assert!(TaskEnvelope::from_json(&remapped, org_id).unwrap().images[0]
            .reference
            .contains("00000000-0000-4000-8000-000000000006"));

        let mut wrong = task;
        wrong.org_refs[0].org_id = "00000000-0000-4000-8000-000000000099".into();
        assert!(wrong.to_canonical_json(org_id).is_err());

        wrong.org_refs[0].org_id = org_id.into();
        wrong.images[0].reference = "![[not-a-share-marker.png]]".into();
        assert!(wrong.to_canonical_json(org_id).is_err());

        wrong.images[0].reference = concat!(
            "![one](murmur-attachment://00000000-0000-4000-8000-000000000005)",
            "![hidden](murmur-attachment://00000000-0000-4000-8000-000000000006)"
        )
        .into();
        assert!(wrong.to_canonical_json(org_id).is_err());
    }

    #[test]
    fn task_redaction_preserves_protocol_structure_and_attachment_identity() {
        let org_id = "00000000-0000-4000-8000-000000000001";
        let attachment_id = "00000000-0000-4000-8000-000000000005";
        let task = TaskEnvelope {
            version: TASK_ENVELOPE_VERSION,
            org_id: org_id.into(),
            title: "Email alice@example.com".into(),
            description: "Call +48 600 700 800".into(),
            status: TaskStatus::Todo,
            due_at: Some("2026-08-22T09:00:00Z".into()),
            assignee_user_id: None,
            created_at: "2026-08-21T09:00:00Z".into(),
            subtasks: vec![TaskSubtask {
                id: "00000000-0000-4000-8000-000000000003".into(),
                title: "Send 4111 1111 1111 1111".into(),
                done: false,
            }],
            org_refs: vec![TaskOrgRef {
                org_id: org_id.into(),
                doc_id: "00000000-0000-4000-8000-000000000004".into(),
            }],
            images: vec![TaskImageRef {
                reference: format!(
                    "![alice@example.com](murmur-attachment://{attachment_id})"
                ),
                alt: "alice@example.com".into(),
            }],
        };
        let canonical = task.to_canonical_json(org_id).unwrap();
        let (redacted, redacted_json, counts) =
            crate::commands::scrub_task_envelope_json(&canonical, org_id).unwrap();

        assert!(!redacted_json.contains("alice@example.com"));
        assert!(!redacted_json.contains("+48 600 700 800"));
        assert!(!redacted_json.contains("4111 1111 1111 1111"));
        assert_eq!(redacted.org_id, task.org_id);
        assert_eq!(redacted.status, task.status);
        assert_eq!(redacted.due_at, task.due_at);
        assert_eq!(redacted.created_at, task.created_at);
        assert_eq!(redacted.subtasks[0].id, task.subtasks[0].id);
        assert_eq!(redacted.org_refs, task.org_refs);
        assert!(redacted.images[0].reference.contains(attachment_id));
        assert!(counts.emails >= 3);
        assert!(counts.phones >= 1);
        assert!(counts.cards >= 1);
        TaskEnvelope::from_json(&redacted_json, org_id).unwrap();
    }
}
