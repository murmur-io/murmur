//! Pure, bounded candidate preparation for the local Smart-reminder audit.
//!
//! This module never reads storage and never creates a reminder. Model output can only select
//! candidate ids that were produced deterministically from already-gated caller input.

use std::collections::HashSet;
use std::fmt::Write;

use chrono::{Local, NaiveDate, TimeZone};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::{AppError, Result};
use crate::summarize::action_items::parse_action_items;

const MAX_MARKDOWN_BYTES: usize = 256 * 1024;
const MAX_TRANSCRIPT_CANDIDATE_BYTES: usize = 64 * 1024;
const MAX_TRANSCRIPT_CANDIDATE_INPUTS: usize = 128;
const MAX_CANDIDATES: usize = 32;
const MAX_TITLE_CHARS: usize = 240;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReminderAuditCandidate {
    pub id: String,
    pub title: String,
    pub suggested_due_at: Option<i64>,
    pub candidate_key: String,
}

/// Hash all canonical source parts without separator ambiguity.
///
/// Each UTF-8 part is framed by its big-endian byte length, so `["ab", "c"]` and `["a", "bc"]`
/// cannot collide merely because their concatenation is the same. Hashing streams over the input
/// and does not copy or log source content.
pub(crate) fn content_hash(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        let bytes = part.as_bytes();
        let byte_len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        hasher.update(byte_len.to_be_bytes());
        hasher.update(bytes);
    }
    hex_digest(hasher.finalize())
}

/// Build deterministic Smart-reminder candidates from already-gated caller content.
///
/// Open note checkboxes are considered before caller-vetted transcript cues. This gives an
/// explicit action item precedence when both sources normalize to the same title. The caller owns
/// transcript cue extraction; accepting arbitrary transcript lines here would turn this bounded
/// audit into a second, less conservative classifier.
pub(crate) fn build_candidates(
    markdown: &str,
    transcript_candidates: &[String],
) -> Vec<ReminderAuditCandidate> {
    let mut candidates = Vec::new();
    let mut seen_keys = HashSet::new();

    for item in parse_action_items(bounded_complete_lines(markdown, MAX_MARKDOWN_BYTES))
        .into_iter()
        .filter(|item| !item.done)
    {
        push_candidate(
            &mut candidates,
            &mut seen_keys,
            bounded_prefix(&item.text, MAX_TRANSCRIPT_CANDIDATE_BYTES),
        );
        if candidates.len() == MAX_CANDIDATES {
            return candidates;
        }
    }

    let mut remaining_bytes = MAX_TRANSCRIPT_CANDIDATE_BYTES;
    for raw in transcript_candidates
        .iter()
        .take(MAX_TRANSCRIPT_CANDIDATE_INPUTS)
    {
        if candidates.len() == MAX_CANDIDATES || remaining_bytes == 0 {
            break;
        }
        let bounded = bounded_prefix(raw, remaining_bytes);
        remaining_bytes = remaining_bytes.saturating_sub(bounded.len());
        push_candidate(&mut candidates, &mut seen_keys, bounded);
    }

    candidates
}

/// Validate the one allowed local-model decision: selecting ids from the supplied candidate set.
///
/// The returned rows are clones of the deterministic candidates in their original order. Unknown
/// or duplicate ids, an oversized selection, and any additional JSON field reject the entire
/// response, so model text can never become a title or date.
pub(crate) fn validate_keep_ids(
    value: Value,
    candidates: &[ReminderAuditCandidate],
) -> Result<Vec<ReminderAuditCandidate>> {
    if candidates.len() > MAX_CANDIDATES {
        return Err(invalid_model_selection());
    }
    let object = value.as_object().ok_or_else(invalid_model_selection)?;
    if object.len() != 1 {
        return Err(invalid_model_selection());
    }
    let keep_ids = object
        .get("keepIds")
        .and_then(Value::as_array)
        .ok_or_else(invalid_model_selection)?;
    if keep_ids.len() > candidates.len() {
        return Err(invalid_model_selection());
    }

    let mut candidate_ids = HashSet::with_capacity(candidates.len());
    for candidate in candidates {
        if !candidate_ids.insert(candidate.id.as_str()) {
            return Err(invalid_model_selection());
        }
    }

    let mut kept_ids = HashSet::with_capacity(keep_ids.len());
    for id in keep_ids {
        let id = id.as_str().ok_or_else(invalid_model_selection)?;
        if !candidate_ids.contains(id) || !kept_ids.insert(id) {
            return Err(invalid_model_selection());
        }
    }

    Ok(candidates
        .iter()
        .filter(|candidate| kept_ids.contains(candidate.id.as_str()))
        .cloned()
        .collect())
}

fn push_candidate(
    candidates: &mut Vec<ReminderAuditCandidate>,
    seen_keys: &mut HashSet<String>,
    raw_title: &str,
) {
    if candidates.len() == MAX_CANDIDATES {
        return;
    }
    let title = bounded_title(raw_title);
    if title.is_empty() {
        return;
    }
    let normalized = normalize_title(&title);
    let candidate_key = hex_digest(Sha256::digest(normalized.as_bytes()));
    if !seen_keys.insert(candidate_key.clone()) {
        return;
    }
    candidates.push(ReminderAuditCandidate {
        id: format!("c{}", candidates.len() + 1),
        suggested_due_at: first_valid_due_at(&title),
        title,
        candidate_key,
    });
}

fn bounded_title(raw: &str) -> String {
    let mut out = String::new();
    for word in raw.split_whitespace() {
        let separator_chars = usize::from(!out.is_empty());
        let remaining = MAX_TITLE_CHARS.saturating_sub(out.chars().count() + separator_chars);
        if remaining == 0 {
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.extend(word.chars().take(remaining));
        if word.chars().count() > remaining {
            break;
        }
    }
    out
}

fn normalize_title(title: &str) -> String {
    title.to_lowercase()
}

fn first_valid_due_at(title: &str) -> Option<i64> {
    let len = title.len();
    if len < 10 {
        return None;
    }
    for start in 0..=len - 10 {
        let Some(raw_date) = title.get(start..start + 10) else {
            continue;
        };
        if !has_iso_shape(raw_date) || !has_date_boundaries(title, start) {
            continue;
        }
        let Ok(date) = NaiveDate::parse_from_str(raw_date, "%Y-%m-%d") else {
            continue;
        };
        if date.format("%Y-%m-%d").to_string() != raw_date {
            continue;
        }
        let local_nine = date.and_hms_opt(9, 0, 0)?;
        if let Some(timestamp) = Local.from_local_datetime(&local_nine).single() {
            let timestamp = timestamp.timestamp_millis();
            if (crate::storage::reminder_store::MIN_REMINDER_DUE_AT
                ..crate::storage::reminder_store::MAX_REMINDER_DUE_AT)
                .contains(&timestamp)
            {
                return Some(timestamp);
            }
        }
    }
    None
}

fn has_date_boundaries(value: &str, start: usize) -> bool {
    let before_is_digit = value
        .get(..start)
        .and_then(|prefix| prefix.chars().next_back())
        .is_some_and(|character| character.is_ascii_digit());
    let after_is_digit = value
        .get(start + 10..)
        .and_then(|suffix| suffix.chars().next())
        .is_some_and(|character| character.is_ascii_digit());
    !before_is_digit && !after_is_digit
}

fn has_iso_shape(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && bytes[8..].iter().all(u8::is_ascii_digit)
}

fn bounded_complete_lines(value: &str, max_bytes: usize) -> &str {
    let prefix = bounded_prefix(value, max_bytes);
    if prefix.len() == value.len() {
        return prefix;
    }
    prefix
        .rfind('\n')
        .map_or("", |last_newline| &prefix[..=last_newline])
}

fn bounded_prefix(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    &value[..end]
}

fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    let bytes = bytes.as_ref();
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

fn invalid_model_selection() -> AppError {
    AppError::InvalidArg("invalid local reminder-audit selection".into())
}

#[cfg(test)]
mod tests {
    use chrono::{Local, TimeZone};
    use serde_json::json;

    use super::*;

    #[test]
    fn candidates_include_open_but_not_completed_checkboxes() {
        let markdown =
            "## Action items\n- [ ] Ship the plan\n- [x] Already shipped\n- [X] Also done\n";
        let transcript = vec!["Call the customer".to_string()];

        let candidates = build_candidates(markdown, &transcript);

        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.title.as_str())
                .collect::<Vec<_>>(),
            vec!["Ship the plan", "Call the customer"]
        );
    }

    #[test]
    fn only_calendar_valid_exact_iso_dates_become_local_nine_am() {
        let transcript = vec![
            "Impossible deadline 2026-02-30".to_string(),
            "Leap-day deadline 2028-02-29".to_string(),
            "Not exact 2028-2-29".to_string(),
            "Embedded in a longer number 12028-02-29".to_string(),
        ];

        let candidates = build_candidates("", &transcript);

        assert_eq!(candidates[0].suggested_due_at, None);
        assert_eq!(
            candidates[1].suggested_due_at,
            Local
                .with_ymd_and_hms(2028, 2, 29, 9, 0, 0)
                .single()
                .map(|value| value.timestamp_millis())
        );
        assert_eq!(candidates[2].suggested_due_at, None);
        assert_eq!(candidates[3].suggested_due_at, None);
    }

    #[test]
    fn suggested_dates_obey_the_canonical_reminder_horizon() {
        assert_eq!(first_valid_due_at("Legacy deadline 1999-12-31"), None);
        assert_eq!(first_valid_due_at("Far deadline 2200-01-01"), None);
        assert!(first_valid_due_at("Supported deadline 2199-12-30").is_some());
    }

    #[test]
    fn content_hash_is_stable_and_length_framed() {
        assert_eq!(
            content_hash(&["ab", "c"]),
            "601d5476e2ccfe2c87a2bba7a322659734a05749d5b5aa781f513e4912db0d5f"
        );
        assert_ne!(content_hash(&["ab", "c"]), content_hash(&["a", "bc"]));
        assert_ne!(content_hash(&[""]), content_hash(&[]));
    }

    #[test]
    fn candidate_count_title_size_and_normalized_dedupe_are_bounded() {
        let mut transcript = vec![
            "  Review   the PLAN  ".to_string(),
            "review the plan".to_string(),
            "Ż".repeat(MAX_TITLE_CHARS + 20),
        ];
        transcript.extend((0..MAX_CANDIDATES + 10).map(|idx| format!("Unique task {idx}")));

        let candidates = build_candidates("", &transcript);

        assert_eq!(candidates.len(), MAX_CANDIDATES);
        assert_eq!(
            candidates
                .iter()
                .filter(|candidate| candidate.title.eq_ignore_ascii_case("review the plan"))
                .count(),
            1
        );
        assert!(candidates
            .iter()
            .all(|candidate| candidate.title.chars().count() <= MAX_TITLE_CHARS));
        assert_eq!(
            candidates
                .iter()
                .find(|candidate| candidate.title.starts_with('Ż'))
                .map(|candidate| candidate.title.chars().count()),
            Some(MAX_TITLE_CHARS)
        );

        let mut empty_prefix = vec![String::new(); MAX_TRANSCRIPT_CANDIDATE_INPUTS];
        empty_prefix.push("Must stay beyond the bounded scan".into());
        assert!(
            build_candidates("", &empty_prefix).is_empty(),
            "the transcript candidate-list scan itself must be bounded"
        );
    }

    #[test]
    fn model_selection_rejects_unknown_ids_and_returns_only_known_rows() {
        let candidates = build_candidates(
            "- [ ] First\n- [ ] Second\n- [ ] Third\n",
            &Vec::<String>::new(),
        );

        let kept = validate_keep_ids(json!({"keepIds": ["c3", "c1"]}), &candidates).unwrap();
        assert_eq!(
            kept.iter()
                .map(|candidate| candidate.id.as_str())
                .collect::<Vec<_>>(),
            vec!["c1", "c3"],
            "candidate source order stays authoritative"
        );
        assert!(validate_keep_ids(json!({"keepIds": ["invented"]}), &candidates).is_err());
        assert!(validate_keep_ids(json!({"keepIds": "c1"}), &candidates).is_err());
        assert!(validate_keep_ids(
            json!({"keepIds": ["c1"], "inventedTitle": "poison"}),
            &candidates
        )
        .is_err());
    }
}
