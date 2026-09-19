//! Session-only live captions. Never writes authoritative transcript segments or calls a model.
use crate::{
    error::{AppError, Result},
    state::AppState,
};
use serde::Serialize;
use std::collections::VecDeque;
use tauri::{AppHandle, Emitter, Manager};

const MAX_LINES: usize = 10_000;
const MAX_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveCaptionPayload {
    pub text: String,
    pub meeting_id: String,
    pub speaker: String,
    pub captured_at: String,
    pub offset_ms: u64,
    pub line_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    #[serde(rename = "final")]
    pub finalized: bool,
    pub is_question: bool,
    pub possible_question: bool,
    /// Internal stabilization witness; never part of the IPC contract.
    #[serde(skip)]
    pub(crate) confirmed: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveTranscriptPage {
    pub lines: Vec<LiveCaptionPayload>,
    pub truncated: bool,
    pub next_before_seq: Option<u64>,
    pub others_state: String,
}

#[derive(Default)]
pub struct LiveTranscriptHistory {
    meeting_id: String,
    lines: VecDeque<LiveCaptionPayload>,
    partials: Vec<LiveCaptionPayload>,
    next_seq: u64,
    bytes: usize,
    truncated: bool,
    others_state: String,
}
impl LiveTranscriptHistory {
    pub fn clear(&mut self) {
        *self = Self::default();
    }
    fn confirmed_tail(&self) -> Vec<LiveCaptionPayload> {
        self.partials
            .iter()
            .filter(|p| p.confirmed)
            .cloned()
            .map(|mut line| {
                line.finalized = true;
                line.is_question = line.text.contains('?');
                let direct = normalize(&line.text)
                    .split_whitespace()
                    .any(|w| matches!(w, "you" | "your" | "ty" | "ciebie" | "cię" | "tobie"));
                line.possible_question = line.is_question && line.speaker == "others" && direct;
                line
            })
            .collect()
    }
    fn insert(&mut self, mut line: LiveCaptionPayload) -> LiveCaptionPayload {
        if self.meeting_id != line.meeting_id {
            self.clear();
            self.meeting_id.clone_from(&line.meeting_id);
        }
        // Bound each hypothesis independently; UTF-8 boundaries are preserved.
        if line.text.len() > 16_384 {
            let mut end = 16_384;
            while !line.text.is_char_boundary(end) {
                end -= 1;
            }
            line.text.truncate(end);
            self.truncated = true;
        }
        self.partials.retain(|p| p.speaker != line.speaker);
        if line.finalized {
            self.next_seq += 1;
            line.seq = Some(self.next_seq);
            self.bytes += line.text.len();
            self.lines.push_back(line.clone());
        } else {
            self.partials.push(line.clone());
        }
        let partial_bytes: usize = self.partials.iter().map(|p| p.text.len()).sum();
        while self.lines.len() > MAX_LINES || self.bytes + partial_bytes > MAX_BYTES {
            if let Some(old) = self.lines.pop_front() {
                self.bytes -= old.text.len();
                self.truncated = true;
            } else {
                break;
            }
        }
        line
    }
    pub fn page(&self, meeting_id: &str, before: Option<u64>, limit: usize) -> LiveTranscriptPage {
        if self.meeting_id != meeting_id {
            return LiveTranscriptPage {
                lines: vec![],
                truncated: false,
                next_before_seq: None,
                others_state: "starting".into(),
            };
        }
        let limit = limit.clamp(1, 200);
        let eligible: Vec<_> = self
            .lines
            .iter()
            .filter(|l| before.map_or(true, |s| l.seq.unwrap_or(0) < s))
            .collect();
        let partial_count = if before.is_none() {
            self.partials.len().min(limit)
        } else {
            0
        };
        let start = eligible.len().saturating_sub(limit - partial_count);
        let mut lines: Vec<_> = eligible[start..].iter().map(|l| (*l).clone()).collect();
        let next_before_seq = if start > 0 {
            lines.first().and_then(|l| l.seq)
        } else {
            None
        };
        if before.is_none() {
            lines.extend(self.partials.iter().take(partial_count).cloned());
        }
        LiveTranscriptPage {
            lines,
            truncated: self.truncated,
            next_before_seq,
            others_state: self.others_state.clone(),
        }
    }
}

pub fn clear_history(state: &AppState) {
    if let Ok(mut history) = state.live_transcript_lines.lock() {
        history.clear();
    }
}

/// Called at the synchronous renderer invalidation barrier, AFTER visibility changes.
/// A global epoch also advances for unrelated documents/facts: it revokes in-flight work,
/// but is not evidence that this recording's already-authorized scrollback became hidden.
/// Do not acquire lifecycle here: lock/relock callers already hold that non-reentrant guard.
pub(crate) fn clear_hidden_history(state: &AppState) {
    let meeting_id = match state.live_transcript_lines.lock() {
        Ok(history) if !history.meeting_id.is_empty() => history.meeting_id.clone(),
        Ok(_) => return,
        Err(poisoned) => {
            poisoned.into_inner().clear();
            return;
        }
    };
    let visible = (|| -> Result<bool> {
        let current = state
            .current_meeting
            .lock()
            .map_err(|_| AppError::Storage("recording state unavailable".into()))?
            .map(|id| id.to_string());
        Ok(current.as_deref() == Some(meeting_id.as_str())
            && state.db.get_meeting(&meeting_id)?.is_some()
            && crate::commands::meeting_is_unlocked(state, &meeting_id)?)
    })()
    .unwrap_or(false);
    if !visible {
        let mut history = state
            .live_transcript_lines
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Never clear a newer recording installed between the visibility probe and this lock.
        if history.meeting_id == meeting_id {
            history.clear();
        }
    }
}

/// Lifecycle barrier covers authorization, cache insertion and event emission as one interval.
/// The visibility epoch captured before ASR prevents a late decode repopulating a cleared buffer.
pub(crate) fn publish(
    app: &AppHandle,
    line: LiveCaptionPayload,
    visibility: crate::commands::ContentVisibilitySnapshot,
) -> Result<()> {
    let state = app.state::<AppState>();
    let _lifecycle = crate::commands::lifecycle_guard(&state);
    require_visible_under_lifecycle(&state, &line, visibility)?;
    let (line, changed) = {
        let mut history = state
            .live_transcript_lines
            .lock()
            .map_err(|_| AppError::Storage("live history unavailable".into()))?;
        let changed = line.finalized
            || !history
                .partials
                .iter()
                .any(|previous| previous.line_id == line.line_id && previous.text == line.text);
        (history.insert(line), changed)
    };
    // Confirmation may change while the displayed partial does not. Persist the witness
    // for Stop without sending duplicate content events to every renderer.
    if !changed {
        return Ok(());
    }
    app.emit(crate::events::EVENT_LIVE_CAPTION, line)
        .map_err(|e| AppError::Storage(format!("live caption event failed: {e}")))?;
    tracing::info!(target: "live_perf", caption_events = 1u64, "live caption published");
    Ok(())
}

/// Stop owns `state.lifecycle` here, before draining/removing the recorder or clearing its id.
/// Flush only twice-observed partials. At most two rows; no ASR, waits, or fresh content reads.
pub(crate) fn flush_confirmed_under_lifecycle(app: &AppHandle, state: &AppState) -> Result<usize> {
    let visibility = crate::commands::capture_content_visibility_snapshot_under_lifecycle(state);
    let lines = state
        .live_transcript_lines
        .lock()
        .map_err(|_| AppError::Storage("live history unavailable".into()))?
        .confirmed_tail();
    let mut emitted = 0;
    for line in lines {
        require_visible_under_lifecycle(state, &line, visibility)?;
        let line = state
            .live_transcript_lines
            .lock()
            .map_err(|_| AppError::Storage("live history unavailable".into()))?
            .insert(line);
        app.emit(crate::events::EVENT_LIVE_CAPTION, line)
            .map_err(|e| AppError::Storage(format!("live caption event failed: {e}")))?;
        emitted += 1;
    }
    Ok(emitted)
}

fn require_visible_under_lifecycle(
    state: &AppState,
    line: &LiveCaptionPayload,
    visibility: crate::commands::ContentVisibilitySnapshot,
) -> Result<()> {
    crate::commands::require_current_content_visibility_snapshot_under_lifecycle(
        state, visibility,
    )?;
    if state
        .current_meeting
        .lock()
        .map_err(|_| AppError::Storage("recording state unavailable".into()))?
        .map(|id| id.to_string())
        .as_deref()
        != Some(line.meeting_id.as_str())
        || state.db.get_meeting(&line.meeting_id)?.is_none()
        || !crate::commands::meeting_is_unlocked(state, &line.meeting_id)?
    {
        return Err(AppError::Locked(
            "live transcript is no longer visible".into(),
        ));
    }
    Ok(())
}

/// Per-stream absolute-time assembler. Matching hypotheses commit only after one nominal tick.
/// A stable line id lets the webview replace a partial without replacing prior utterances.
#[derive(Clone)]
struct Hypothesis {
    text: String,
    start: u64,
    end: u64,
    confirmed: bool,
}
#[derive(Default)]
pub(crate) struct StreamCaptionAssembler {
    pending: Vec<Hypothesis>,
    committed_end: u64,
    committed_tail: String,
    generation: u64,
    last_partial: String,
    last_window_end: u64,
}
impl StreamCaptionAssembler {
    pub fn update(
        &mut self,
        transcript: &super::types::Transcript,
        window_start_ms: u64,
        now_ms: u64,
        meeting_id: &str,
        speaker: &str,
    ) -> Vec<LiveCaptionPayload> {
        if now_ms < self.last_window_end {
            return vec![];
        }
        self.last_window_end = now_ms;
        let previous = std::mem::take(&mut self.pending);
        let mut out = Vec::new();
        let mut candidates: Vec<_> = transcript
            .segments
            .iter()
            .map(|segment| Hypothesis {
                text: segment.text.trim().into(),
                start: window_start_ms.saturating_add((segment.start_s.max(0.0) * 1000.0) as u64),
                end: window_start_ms
                    .saturating_add((segment.end_s.max(segment.start_s).max(0.0) * 1000.0) as u64),
                confirmed: false,
            })
            .collect();
        candidates.sort_by_key(|h| h.start);
        let mut blocked = false;
        for mut candidate in candidates {
            if candidate.text.is_empty()
                || candidate.text.len() > 16_384
                || candidate.end <= self.committed_end
            {
                continue;
            }
            // A growing segment can include a prefix already committed on the preceding tick.
            if candidate.start < self.committed_end && !self.committed_tail.is_empty() {
                let words: Vec<_> = candidate.text.split_whitespace().collect();
                let committed: Vec<_> = self.committed_tail.split_whitespace().collect();
                let overlap = (1..=words.len().min(committed.len()))
                    .rev()
                    .find(|n| {
                        normalize(&words[..*n].join(" "))
                            == committed[committed.len() - *n..].join(" ")
                    })
                    .unwrap_or(0);
                if overlap > 0 {
                    candidate.text = words[overlap..].join(" ");
                    candidate.start = self.committed_end;
                }
            }
            if candidate.text.is_empty() {
                continue;
            }
            let old = previous
                .iter()
                .find(|old| old.start.abs_diff(candidate.start) < 2_000);
            let words: Vec<_> = candidate.text.split_whitespace().collect();
            let shared = old
                .map(|old| {
                    old.text
                        .split_whitespace()
                        .zip(words.iter())
                        .take_while(|(a, b)| normalize(a) == normalize(b))
                        .count()
                })
                .unwrap_or(0);
            candidate.confirmed = shared == words.len();
            // All complete matching segments are considered on EVERY tick. An unstable first
            // segment cannot starve later turns until the 14-second window has rolled past them.
            let aged = now_ms.saturating_sub(candidate.end) >= 3_000;
            let commit_words = if aged && (shared == words.len() || shared >= 3) {
                shared
            } else {
                0
            };
            if commit_words > 0 && !blocked {
                let text = words[..commit_words].join(" ");
                let end = if commit_words == words.len() {
                    candidate.end
                } else {
                    candidate.start
                        + (candidate.end - candidate.start) * commit_words as u64
                            / words.len() as u64
                };
                out.push(self.payload(text.clone(), candidate.start, true, meeting_id, speaker));
                self.generation += 1;
                self.committed_end = self.committed_end.max(end);
                self.committed_tail = normalize(&text);
                self.last_partial.clear();
                if commit_words < words.len() {
                    blocked = true;
                    self.pending.push(Hypothesis {
                        text: words[commit_words..].join(" "),
                        start: end,
                        end: candidate.end,
                        confirmed: false,
                    });
                }
            } else {
                blocked = true;
                self.pending.push(candidate);
            }
        }
        let partial = self
            .pending
            .iter()
            .map(|p| p.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        if !partial.is_empty()
            && (partial != self.last_partial || self.pending.iter().all(|p| p.confirmed))
        {
            let start = self.pending.first().map_or(now_ms, |p| p.start);
            let mut line = self.payload(partial.clone(), start, false, meeting_id, speaker);
            line.confirmed = self.pending.iter().all(|p| p.confirmed);
            out.push(line);
            self.last_partial = partial;
        }
        out
    }
    fn payload(
        &self,
        text: String,
        offset_ms: u64,
        finalized: bool,
        meeting_id: &str,
        speaker: &str,
    ) -> LiveCaptionPayload {
        let is_question = finalized && text.contains('?');
        let direct = normalize(&text)
            .split_whitespace()
            .any(|w| matches!(w, "you" | "your" | "ty" | "ciebie" | "cię" | "tobie"));
        LiveCaptionPayload {
            text,
            meeting_id: meeting_id.into(),
            speaker: speaker.into(),
            captured_at: (chrono::Utc::now()
                - chrono::Duration::milliseconds(
                    self.last_window_end
                        .saturating_sub(offset_ms)
                        .min(i64::MAX as u64) as i64,
                ))
            .to_rfc3339(),
            offset_ms,
            line_id: format!("{meeting_id}:{speaker}:{}", self.generation),
            seq: None,
            finalized,
            is_question,
            possible_question: is_question && speaker == "others" && direct,
            confirmed: finalized,
        }
    }
}

fn normalize(s: &str) -> String {
    s.split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn line(text: &str) -> LiveCaptionPayload {
        LiveCaptionPayload {
            text: text.into(),
            meeting_id: "m".into(),
            speaker: "me".into(),
            captured_at: "now".into(),
            offset_ms: 0,
            line_id: text.into(),
            seq: None,
            finalized: true,
            is_question: false,
            possible_question: false,
            confirmed: true,
        }
    }
    #[test]
    fn transcript_retains_earlier_utterances_and_pages() {
        let mut h = LiveTranscriptHistory::default();
        h.insert(line("first utterance"));
        h.insert(line("second utterance"));
        let p = h.page("m", None, 1);
        assert_eq!(p.lines[0].text, "second utterance");
        let older = h.page("m", p.next_before_seq, 1);
        assert_eq!(older.lines[0].text, "first utterance");
        h.clear();
        assert!(h.page("m", None, 200).lines.is_empty());
    }
    fn transcript(parts: &[(&str, f64, f64)]) -> super::super::types::Transcript {
        super::super::types::Transcript {
            full_text: String::new(),
            language: None,
            segments: parts
                .iter()
                .enumerate()
                .map(|(idx, (text, start, end))| super::super::types::Segment {
                    idx: idx as i64,
                    text: (*text).into(),
                    start_s: *start,
                    end_s: *end,
                    speaker: None,
                    confidence: None,
                })
                .collect(),
        }
    }
    #[test]
    fn assembler_observes_all_segments_without_losing_earlier_turns() {
        let mut a = StreamCaptionAssembler::default();
        let t = transcript(&[
            ("First sentence.", 0.0, 2.0),
            ("Second sentence.", 3.0, 5.0),
        ]);
        assert_eq!(a.update(&t, 0, 8000, "m", "me").len(), 1);
        let finals = a.update(&t, 0, 11000, "m", "me");
        assert_eq!(finals.len(), 2);
        assert!(finals.iter().all(|l| l.finalized));
        assert_eq!(finals[0].text, "First sentence.");
        assert_eq!(finals[1].text, "Second sentence.");
        assert_ne!(finals[0].line_id, finals[1].line_id);
    }
    #[test]
    fn assembler_commits_stable_prefix_of_a_revised_hypothesis() {
        let mut a = StreamCaptionAssembler::default();
        a.update(
            &transcript(&[("We can ship this tomorrow.", 0.0, 3.0)]),
            0,
            6000,
            "m",
            "me",
        );
        let changed = a.update(
            &transcript(&[("We can ship this next week.", 0.0, 3.0)]),
            0,
            9000,
            "m",
            "me",
        );
        assert!(changed[0].finalized);
        assert_eq!(changed[0].text, "We can ship this");
        assert!(!changed[1].finalized);
        assert_eq!(changed[1].text, "next week.");
        assert!(a
            .update(&transcript(&[("old", 0.0, 1.0)]), 0, 1000, "m", "me")
            .is_empty());
    }
    #[test]
    fn sealed_live_page_and_emission_are_denied_and_privacy_clears_cache() {
        use crate::storage::{
            models::{Folder, Meeting, MeetingStatus},
            Db,
        };
        let db = Db::open_with_key(
            std::path::Path::new(":memory:"),
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )
        .unwrap();
        db.insert_folder(&Folder {
            id: "private".into(),
            name: "test".into(),
            path: "test".into(),
            parent_id: None,
            locked: false,
            created_at: "now".into(),
        })
        .unwrap();
        let id = uuid::Uuid::new_v4();
        db.insert_meeting(&Meeting {
            id: id.to_string(),
            started_at: "now".into(),
            ended_at: None,
            title: None,
            duration_s: 0,
            audio_path: None,
            status: MeetingStatus::Recording,
            folder_id: Some("private".into()),
        })
        .unwrap();
        let state = AppState::for_tests(db);
        *state.current_meeting.lock().unwrap() = Some(id);
        let mut caption = line("private words");
        caption.meeting_id = id.to_string();
        assert!(matches!(
            crate::commands::get_live_transcript_page_inner(&state, "missing", None, None),
            Err(AppError::Locked(_))
        ));
        let visibility = crate::commands::capture_content_visibility_snapshot(&state);
        assert!(require_visible_under_lifecycle(&state, &caption, visibility).is_ok());
        state
            .live_transcript_lines
            .lock()
            .unwrap()
            .insert(caption.clone());
        assert_eq!(
            crate::commands::get_live_transcript_page_inner(&state, &id.to_string(), None, None)
                .unwrap()
                .lines
                .len(),
            1
        );
        // Unrelated deletion follows the same epoch + invalidation seams as document/note
        // deletion. It must revoke late work WITHOUT destroying this meeting's scrollback.
        let other_id = uuid::Uuid::new_v4().to_string();
        state
            .db
            .insert_meeting(&Meeting {
                id: other_id.clone(),
                started_at: "now".into(),
                ended_at: None,
                title: None,
                duration_s: 0,
                audio_path: None,
                status: MeetingStatus::Draft,
                folder_id: None,
            })
            .unwrap();
        crate::commands::bump_seal_epoch(&state);
        state.db.delete_meeting(&other_id).unwrap();
        clear_hidden_history(&state);
        let restored =
            crate::commands::get_live_transcript_page_inner(&state, &id.to_string(), None, None)
                .unwrap();
        assert_eq!(
            restored.lines.len(),
            1,
            "unrelated deletion must retain live scrollback"
        );
        assert_eq!(restored.lines[0].text, "private words");
        assert!(
            matches!(
                require_visible_under_lifecycle(&state, &caption, visibility),
                Err(AppError::Locked(_))
            ),
            "unrelated epoch still revokes in-flight decode"
        );
        let visibility = crate::commands::capture_content_visibility_snapshot(&state);
        state.db.set_folder_locked("private", true, None).unwrap();
        assert!(matches!(
            require_visible_under_lifecycle(&state, &caption, visibility),
            Err(AppError::Locked(_))
        ));
        assert!(matches!(
            crate::commands::get_live_transcript_page_inner(&state, &id.to_string(), None, None),
            Err(AppError::Locked(_))
        ));
        crate::commands::bump_seal_epoch(&state);
        clear_hidden_history(&state);
        assert!(state
            .live_transcript_lines
            .lock()
            .unwrap()
            .page(&id.to_string(), None, 200)
            .lines
            .is_empty());
        state
            .unlocked_folders
            .lock()
            .unwrap()
            .insert("private".into());
        assert!(
            matches!(
                require_visible_under_lifecycle(&state, &caption, visibility),
                Err(AppError::Locked(_))
            ),
            "old decode must stay revoked after unlock"
        );
    }
    #[test]
    fn revised_suffix_cannot_be_overtaken_by_a_later_stable_segment() {
        let mut a = StreamCaptionAssembler::default();
        let first = transcript(&[
            ("We can ship this tomorrow.", 0.0, 3.0),
            ("Second sentence.", 4.0, 5.0),
        ]);
        a.update(&first, 0, 8000, "m", "me");
        let revised = transcript(&[
            ("We can ship this next week.", 0.0, 3.0),
            ("Second sentence.", 4.0, 5.0),
        ]);
        let tick = a.update(&revised, 0, 11000, "m", "me");
        assert_eq!(tick.iter().filter(|l| l.finalized).count(), 1);
        let next = a.update(&revised, 0, 14000, "m", "me");
        assert_eq!(
            next.iter()
                .filter(|l| l.finalized)
                .map(|l| l.text.as_str())
                .collect::<Vec<_>>(),
            vec!["next week.", "Second sentence."]
        );
    }
    #[test]
    fn stop_flush_only_finalizes_twice_observed_safe_tail() {
        let mut a = StreamCaptionAssembler::default();
        let mut history = LiveTranscriptHistory::default();
        let t = transcript(&[("Can you answer?", 0.0, 3.0)]);
        for line in a.update(&t, 0, 3000, "m", "others") {
            history.insert(line);
        }
        assert!(
            history.confirmed_tail().is_empty(),
            "single observation is never safe to flush"
        );
        for line in a.update(&t, 0, 4000, "m", "others") {
            history.insert(line);
        }
        let safe = history.confirmed_tail();
        assert_eq!(safe.len(), 1);
        assert!(safe[0].finalized);
        assert!(safe[0].possible_question);
        history.insert(safe[0].clone());
        assert!(history.confirmed_tail().is_empty());
        assert_eq!(history.page("m", None, 200).lines[0].seq, Some(1));
        let json = serde_json::to_value(&safe[0]).unwrap();
        assert!(json.get("confirmed").is_none());
    }
    #[test]
    fn caption_wire_contract_is_camel_case() {
        let json = serde_json::to_value(line("hi")).unwrap();
        for key in [
            "meetingId",
            "capturedAt",
            "offsetMs",
            "lineId",
            "final",
            "isQuestion",
            "possibleQuestion",
        ] {
            assert!(json.get(key).is_some(), "{key}");
        }
        assert!(json.as_object().unwrap().keys().all(|k| !k.contains('_')));
    }
    #[test]
    fn history_is_bounded_and_partials_do_not_consume_sequences() {
        let mut h = LiveTranscriptHistory::default();
        let mut p = line("partial");
        p.finalized = false;
        assert_eq!(h.insert(p).seq, None);
        for _ in 0..MAX_LINES + 1 {
            h.insert(line("final"));
        }
        assert_eq!(h.lines.len(), MAX_LINES);
        assert!(h.truncated);
        assert!(h.partials.is_empty());
    }
    #[test]
    fn assembler_requires_matching_old_hypotheses_and_preserves_prior_commit() {
        let mut a = StreamCaptionAssembler::default();
        let t = super::super::types::Transcript {
            full_text: "Hello?".into(),
            language: None,
            segments: vec![super::super::types::Segment {
                idx: 0,
                start_s: 0.0,
                end_s: 1.0,
                text: "Can you answer?".into(),
                speaker: None,
                confidence: None,
            }],
        };
        let first = a.update(&t, 0, 4000, "m", "others");
        assert!(!first[0].finalized);
        let second = a.update(&t, 0, 7000, "m", "others");
        assert!(second[0].finalized);
        assert!(second[0].possible_question);
        assert_eq!(first[0].line_id, second[0].line_id);
        assert!(a.update(&t, 0, 10000, "m", "others").is_empty());
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct LiveTranscriptHealth<'a> {
    meeting_id: &'a str,
    others: &'a str,
}
pub(crate) fn health(app: &AppHandle, meeting_id: &str, others: &str) {
    let state = app.state::<AppState>();
    let _lifecycle = crate::commands::lifecycle_guard(&state);
    if state
        .current_meeting
        .lock()
        .ok()
        .and_then(|id| id.map(|id| id.to_string()))
        .as_deref()
        != Some(meeting_id)
    {
        return;
    }
    if let Ok(mut history) = state.live_transcript_lines.lock() {
        if history.meeting_id != meeting_id {
            history.clear();
            history.meeting_id = meeting_id.into();
        }
        if history.others_state == others {
            return;
        }
        history.others_state = others.into();
    }
    let _ = app.emit(
        crate::events::EVENT_LIVE_TRANSCRIPT_HEALTH,
        LiveTranscriptHealth { meeting_id, others },
    );
}
