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
    /// Sentence cue already evaluated with committed context; retained only for Stop flush.
    #[serde(skip)]
    question_if_final: bool,
}

/// Content-free state of the resident captions worker. Decode failures retry on its next tick;
/// model/worker failures require fixing the local model and starting another recording.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CaptionsState {
    #[default]
    Starting,
    Ready,
    Retrying,
    ModelError,
    Unavailable,
    Paused,
    Stopped,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptionsHealth {
    pub captions_state: CaptionsState,
    pub model_label: Option<String>,
    pub tick_interval_ms: Option<u64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveTranscriptPage {
    pub lines: Vec<LiveCaptionPayload>,
    pub truncated: bool,
    pub next_before_seq: Option<u64>,
    pub others_state: String,
    #[serde(flatten)]
    pub health: CaptionsHealth,
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
    health: CaptionsHealth,
}
impl LiveTranscriptHistory {
    pub fn clear(&mut self) {
        *self = Self::default();
    }
    pub(crate) fn restart_allowed(&self, meeting_id: &str) -> bool {
        self.meeting_id == meeting_id
            && matches!(
                self.health.captions_state,
                CaptionsState::ModelError | CaptionsState::Stopped | CaptionsState::Unavailable
            )
    }

    fn update_health(&mut self, others: Option<&str>, health: Option<CaptionsHealth>) -> bool {
        let changed = others.is_some_and(|s| s != self.others_state)
            || health.as_ref().is_some_and(|h| h != &self.health);
        if let Some(others) = others {
            self.others_state = others.into();
        }
        if let Some(health) = health {
            self.health = health;
        }
        changed
    }
    fn confirmed_tail(&self) -> Vec<LiveCaptionPayload> {
        self.partials
            .iter()
            .filter(|p| p.confirmed)
            .cloned()
            .map(|mut line| {
                line.finalized = true;
                line.is_question = line.text.contains('?');
                line.possible_question =
                    line.is_question && line.speaker == "others" && line.question_if_final;
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
                health: CaptionsHealth::default(),
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
            health: self.health.clone(),
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
            // Erase all content, retaining only content-free worker health and the session
            // sequence counter. A failed worker cannot republish its status after unlock.
            history.lines.clear();
            history.partials.clear();
            history.bytes = 0;
            history.truncated = false;
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
    overlaps_committed: bool,
}
#[derive(Default)]
pub(crate) struct StreamCaptionAssembler {
    pending: Vec<Hypothesis>,
    committed_end: u64,
    committed_tail: String,
    generation: u64,
    last_partial: String,
    last_window_end: u64,
    restart_namespace: String,
    resume_floor_ms: u64,
    sentence_cue: SentenceQuestionCue,
}
impl StreamCaptionAssembler {
    /// A recovered worker starts with new speech. Preserve historical identities, and never
    /// recycle a line id when the old worker's rolling window is decoded again.
    pub(crate) fn resumed(resume_floor_ms: u64) -> Self {
        Self {
            restart_namespace: uuid::Uuid::new_v4().to_string(),
            resume_floor_ms,
            committed_end: resume_floor_ms,
            last_window_end: resume_floor_ms,
            ..Self::default()
        }
    }

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
        let previous_window_end = self.last_window_end;
        self.last_window_end = now_ms;
        let previous = std::mem::take(&mut self.pending);
        let mut out = Vec::new();
        let mut candidates: Vec<_> = transcript
            .segments
            .iter()
            .map(|segment| Hypothesis {
                text: speech_text(&segment.text),
                // Whisper may timestamp padded audio beyond the supplied window. Those
                // synthetic future times must never delay retirement of a real utterance.
                start: window_start_ms
                    .saturating_add((segment.start_s.max(0.0) * 1000.0) as u64)
                    .min(now_ms),
                end: window_start_ms
                    .saturating_add((segment.end_s.max(segment.start_s).max(0.0) * 1000.0) as u64)
                    .min(now_ms),
                confirmed: false,
                overlaps_committed: false,
            })
            .collect();
        candidates.sort_by_key(|h| h.start);
        // The rolling decoder may revise/resegment a whole 14-second window. A partial
        // that cannot be aligned to the next decode must not disappear from scrollback.
        // Retire the aged previous observation before replacing it. "Final" here means
        // immutable session caption, NOT authoritative ASR: batch transcription still owns
        // the stored transcript. Exact overlap continues through the two-observation path.
        let retiring = previous.iter().any(|old| {
            !old.overlaps_committed
                && old.end <= previous_window_end
                && previous_window_end < now_ms
                && !candidates.iter().any(|next| {
                    old.start.abs_diff(next.start) < 2_000
                        && old
                            .text
                            .split_whitespace()
                            .zip(next.text.split_whitespace())
                            .take_while(|(a, b)| normalize(a) == normalize(b))
                            .count()
                            >= 3.min(old.text.split_whitespace().count())
                                .min(next.text.split_whitespace().count())
                                .max(1)
                })
        });
        if retiring {
            for old in &previous {
                if old.overlaps_committed {
                    continue;
                }
                if old.end > previous_window_end || previous_window_end >= now_ms {
                    break;
                }
                if old.end > self.committed_end {
                    // An unrelated later revision must not make retirement discard a
                    // strictly extending, already-aged hypothesis of this earlier chunk.
                    // Preserve the old prefix verbatim; only append new words. Never
                    // adopt rewritten prefixes, fresh audio, or pre-resume content here.
                    let old_words: Vec<_> = old.text.split_whitespace().map(normalize).collect();
                    let extension = candidates
                        .iter()
                        .filter(|next| {
                            next.start >= self.resume_floor_ms
                                && old.start.abs_diff(next.start) < 2_000
                                && next.end >= old.end
                                && next.end <= previous_window_end
                                && next.text.len() <= 16_384
                        })
                        .filter_map(|next| {
                            let words: Vec<_> = next.text.split_whitespace().collect();
                            if words.len() <= old_words.len()
                                || !words
                                    .iter()
                                    .zip(&old_words)
                                    .all(|(a, b)| normalize(a) == *b)
                            {
                                return None;
                            }
                            let text =
                                format!("{} {}", old.text, words[old_words.len()..].join(" "));
                            (text.len() <= 16_384).then_some((text, next.end))
                        })
                        .max_by_key(|(text, end)| (*end, text.len()));
                    let (text, end) = extension.unwrap_or_else(|| (old.text.clone(), old.end));
                    out.push(self.commit(text, old.start, end, meeting_id, speaker));
                }
            }
        }
        let mut blocked = false;
        for mut candidate in candidates {
            if candidate.text.is_empty()
                || candidate.text.len() > 16_384
                || candidate.start < self.resume_floor_ms
                || candidate.end <= self.committed_end
            {
                continue;
            }
            // A growing segment can contain several already-committed chunks. Preserve a
            // bounded suffix across commits and use it as an anchor inside long hypotheses.
            let mut overlap_unresolved = false;
            if candidate.start < self.committed_end && !self.committed_tail.is_empty() {
                let words: Vec<_> = candidate.text.split_whitespace().collect();
                let committed: Vec<_> = self.committed_tail.split_whitespace().collect();
                let normalized_words: Vec<_> = words.iter().map(|word| normalize(word)).collect();
                let overlap = (1..=words.len().min(committed.len()))
                    .rev()
                    .find_map(|n| {
                        let suffix = &committed[committed.len() - n..];
                        if n == 64
                            && committed.len() == 64
                            && normalized_words
                                .windows(n)
                                .filter(|window| *window == suffix)
                                .take(2)
                                .count()
                                > 1
                        {
                            // A repeated full anchor cannot locate the old/new boundary safely.
                            // Stop matching (Some(0)), rather than falling back to a shorter repeat.
                            return Some(0);
                        }
                        if normalized_words[..n] == *suffix {
                            Some(n)
                        } else if n == committed.len() {
                            // Beyond the 64-word bound, the retained FULL tail may start inside
                            // the growing segment. Never anchor an interior match on a short suffix.
                            normalized_words
                                .windows(n)
                                .position(|window| window == suffix)
                                .map(|start| start + n)
                        } else {
                            None
                        }
                    })
                    .unwrap_or(0);
                if overlap > 0 {
                    candidate.text = words[overlap..].join(" ");
                    candidate.start = self.committed_end;
                } else if self.committed_end.saturating_sub(candidate.start) <= 2_000
                    && normalized_words.len() >= 3
                    && !committed
                        .windows(normalized_words.len())
                        .any(|window| window == normalized_words)
                {
                    // Whisper shifts boundaries by up to the same two-second tolerance used
                    // for matching hypotheses below. A novel sentence immediately after a
                    // tiny finalized fragment must not be discarded solely for that jitter.
                    candidate.start = self.committed_end;
                } else {
                    // Keep uncertain novel content visible as a partial, but never recommit
                    // an unanchored hypothesis which overlaps already-finalized history.
                    overlap_unresolved = true;
                }
            }
            if candidate.text.is_empty() {
                continue;
            }
            candidate.overlaps_committed = overlap_unresolved;
            let old = previous
                .iter()
                .find(|old| old.start.abs_diff(candidate.start) < 2_000);
            let words: Vec<_> = candidate.text.split_whitespace().collect();
            let shared = old
                .filter(|_| !overlap_unresolved)
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
            let current_aged = now_ms.saturating_sub(candidate.end) >= 3_000;
            // Whisper can return one growing segment ending exactly at the window edge.
            // Its repeated prefix was already present at the PREVIOUS segment end; requiring
            // the latest growing end to age would retain no scrollback until speech stops.
            let previous_end = old.map(|old| old.end.min(candidate.end));
            let prefix_aged = previous_end.is_some_and(|end| now_ms.saturating_sub(end) >= 3_000);
            let commit_words =
                if (current_aged || prefix_aged) && (shared == words.len() || shared >= 3) {
                    shared
                } else {
                    0
                };
            if commit_words > 0 && !blocked {
                let text = words[..commit_words].join(" ");
                let end = if !current_aged && prefix_aged {
                    // Only advance through the earlier observation, never through fresh words
                    // which merely share the growing segment's new end timestamp.
                    previous_end.unwrap_or(candidate.start).max(candidate.start)
                } else if commit_words == words.len() {
                    candidate.end
                } else {
                    candidate.start
                        + (candidate.end - candidate.start) * commit_words as u64
                            / words.len() as u64
                };
                out.push(self.commit(text, candidate.start, end, meeting_id, speaker));
                if commit_words < words.len() {
                    blocked = true;
                    self.pending.push(Hypothesis {
                        text: words[commit_words..].join(" "),
                        start: end,
                        end: candidate.end,
                        confirmed: false,
                        overlaps_committed: false,
                    });
                }
            } else {
                // An unanchored revision of already-finalized audio cannot prevent later,
                // disjoint speech from becoming history. It remains provisional only.
                blocked |= !overlap_unresolved;
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
    fn commit(
        &mut self,
        text: String,
        start: u64,
        end: u64,
        meeting_id: &str,
        speaker: &str,
    ) -> LiveCaptionPayload {
        let line = self.payload(text.clone(), start, true, meeting_id, speaker);
        self.sentence_cue.observe(&text);
        self.generation += 1;
        self.committed_end = self.committed_end.max(end);
        let normalized = normalize(&text);
        let words: Vec<_> = self
            .committed_tail
            .split_whitespace()
            .chain(normalized.split_whitespace())
            .collect();
        self.committed_tail = words[words.len().saturating_sub(64)..].join(" ");
        self.last_partial.clear();
        line
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
        let question_if_final = self.sentence_cue.clone().observe(&text);
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
            line_id: if self.restart_namespace.is_empty() {
                format!("{meeting_id}:{speaker}:{}", self.generation)
            } else {
                format!(
                    "{meeting_id}:{speaker}:{}:{}",
                    self.restart_namespace, self.generation
                )
            },
            seq: None,
            finalized,
            is_question,
            possible_question: is_question && speaker == "others" && question_if_final,
            confirmed: finalized,
            question_if_final,
        }
    }
}

/// A question may span committed chunks. Carry only the unfinished sentence's direct
/// address cue, never an arbitrary historical "you" from a different sentence.
#[derive(Clone, Default)]
struct SentenceQuestionCue {
    words_since_address: Option<usize>,
}
impl SentenceQuestionCue {
    fn observe(&mut self, text: &str) -> bool {
        let mut addressed_question = false;
        for word in text.split_whitespace() {
            self.words_since_address = self
                .words_since_address
                .and_then(|words| (words + 1 < 64).then_some(words + 1));
            if matches!(
                normalize(word).as_str(),
                "you" | "your" | "ty" | "ciebie" | "cię" | "tobie"
            ) {
                self.words_since_address = Some(0);
            }
            if word.contains('?') && self.words_since_address.is_some() {
                addressed_question = true;
            }
            let end = word.trim_end_matches(['"', '\'', ')', ']']);
            if end.ends_with(['.', '?', '!']) {
                self.words_since_address = None;
            }
        }
        addressed_question
    }
}

/// Decoder control/non-speech labels are not utterances (and must not become questions).
fn speech_text(text: &str) -> String {
    let trimmed = text.trim();
    let label = trimmed
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .or_else(|| trimmed.strip_prefix('(').and_then(|s| s.strip_suffix(')')));
    if let Some(label) = label {
        let normalized = label
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_ascii_lowercase();
        if matches!(
            normalized.as_str(),
            "blank_audio"
                | "silence"
                | "no_speech"
                | "music"
                | "applause"
                | "speaking in foreign language"
                | "music playing"
                | "upbeat music"
                | "inaudible"
                | "_beg_"
        ) {
            return String::new();
        }
    }
    text.split_whitespace()
        .filter(|word| {
            !matches!(
                word.to_ascii_lowercase().as_str(),
                "[blank_audio]"
                    | "[silence]"
                    | "[no_speech]"
                    | "[music]"
                    | "[applause]"
                    | "(silence)"
                    | "(music)"
                    | "(applause)"
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
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
            question_if_final: false,
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
    fn growing_window_segment_commits_twice_observed_prefix_before_stop() {
        let mut assembler = StreamCaptionAssembler::default();
        let first = assembler.update(
            &transcript(&[("one two three", 0.0, 3.0)]),
            0,
            3000,
            "m",
            "others",
        );
        assert!(first.iter().all(|line| !line.finalized));
        let second = assembler.update(
            &transcript(&[("one two three four five six", 0.0, 6.0)]),
            0,
            6000,
            "m",
            "others",
        );
        assert_eq!(
            second.iter().filter(|line| line.finalized).count(),
            1,
            "a growing segment end cannot indefinitely postpone stable words"
        );
        assert_eq!(second[0].text, "one two three");
        let third = assembler.update(
            &transcript(&[("one two three four five six seven eight nine", 0.0, 9.0)]),
            0,
            9000,
            "m",
            "others",
        );
        assert_eq!(third.iter().filter(|line| line.finalized).count(), 1);
        assert_eq!(third[0].text, "four five six");
    }

    #[test]
    fn sliding_windows_keep_earlier_utterances_and_question_before_stop() {
        let mut assembler = StreamCaptionAssembler::default();
        let mut history = LiveTranscriptHistory::default();
        let words: Vec<String> = (0..96)
            .map(|index| match index {
                30 => "Can".into(),
                31 => "you".into(),
                32 => "answer?".into(),
                _ => format!("word{index}"),
            })
            .collect();
        // The actual loop decodes the last 14 seconds, often six seconds apart.
        // Once full, both the segment start and its textual prefix MOVE each tick.
        for end in (6usize..=96).step_by(6) {
            let start = end.saturating_sub(14);
            for line in assembler.update(
                &transcript(&[(&words[start..end].join(" "), 0.0, (end - start) as f64)]),
                start as u64 * 1000,
                end as u64 * 1000,
                "m",
                "others",
            ) {
                history.insert(line);
            }
        }
        let page = history.page("m", None, 200);
        let finals: Vec<_> = page.lines.iter().filter(|line| line.finalized).collect();
        let actual: Vec<_> = finals
            .iter()
            .flat_map(|line| line.text.split_whitespace())
            .collect();
        assert_eq!(
            actual,
            words[..90],
            "all prior rolling windows survive exactly once"
        );
        assert!(finals.iter().any(|line| line.possible_question));
        assert_eq!(finals.first().unwrap().seq, Some(1));
    }

    #[test]
    fn native_window_revisions_retain_previously_visible_question() {
        // Real Whisper-small outputs replayed from QA's operator-owned synthetic system
        // audio. The decoder changes segment boundaries and even the whole hypothesis.
        let windows: serde_json::Value =
            serde_json::from_str(include_str!("tests/live_sliding_windows.json")).unwrap();
        let mut assembler = StreamCaptionAssembler::default();
        let mut history = LiveTranscriptHistory::default();
        for window in windows.as_array().unwrap() {
            let end = window["end"].as_u64().unwrap();
            let transcript = super::super::types::Transcript {
                full_text: String::new(),
                language: None,
                segments: serde_json::from_value(window["segments"].clone()).unwrap(),
            };
            for line in assembler.update(
                &transcript,
                end.saturating_sub(14) * 1000,
                end * 1000,
                "m",
                "others",
            ) {
                history.insert(line);
            }
        }
        let page = history.page("m", None, 200);
        let finals: Vec<_> = page.lines.iter().filter(|line| line.finalized).collect();
        assert!(
            finals.len() >= 4,
            "native rolling speech must keep scrollback"
        );
        assert_eq!(
            finals
                .iter()
                .filter(|line| line
                    .text
                    .contains("Jakub, what do you think about shipping the code?"))
                .count(),
            1,
            "a previously visible question survives ASR revisions exactly once"
        );
        assert!(finals.iter().any(|line| line.possible_question));
        assert!(page
            .lines
            .iter()
            .all(|line| !line.text.contains("BLANK_AUDIO")));
        assert!(finals.windows(2).all(|pair| pair[0].seq < pair[1].seq));
    }

    #[test]
    fn ambiguous_overlap_cannot_starve_later_question_or_padded_timestamp_retirement() {
        let mut assembler = StreamCaptionAssembler {
            committed_end: 3000,
            committed_tail: "already committed words".into(),
            ..Default::default()
        };
        let first = transcript(&[
            ("revised old words", 0.0, 4.0),
            // Real Whisper can report a segment beyond the end of its input audio.
            ("Can you answer the question?", 5.0, 70.0),
        ]);
        let partial = assembler.update(&first, 0, 9000, "m", "others");
        assert!(partial
            .iter()
            .any(|line| line.text.contains("Can you answer")));
        let next = assembler.update(
            &transcript(&[("[BLANK_AUDIO]", 0.0, 10.0)]),
            3000,
            12000,
            "m",
            "others",
        );
        let finals: Vec<_> = next.iter().filter(|line| line.finalized).collect();
        assert_eq!(finals.len(), 1);
        assert_eq!(finals[0].text, "Can you answer the question?");
        assert!(finals[0].possible_question);
        assert!(assembler
            .update(
                &transcript(&[("[BLANK_AUDIO]", 0.0, 10.0)]),
                6000,
                15000,
                "m",
                "others"
            )
            .is_empty());
    }

    fn finalize_growing_question(first: &str, second: &str) -> Vec<LiveCaptionPayload> {
        let mut assembler = StreamCaptionAssembler::default();
        let mut finals = Vec::new();
        let third = format!("{second} The next topic");
        for (text, end) in [(first, 3), (second, 6), (third.as_str(), 9)] {
            finals.extend(
                assembler
                    .update(
                        &transcript(&[(text, 0.0, end as f64)]),
                        0,
                        end * 1000,
                        "m",
                        "others",
                    )
                    .into_iter()
                    .filter(|line| line.finalized),
            );
        }
        finals
    }

    #[test]
    fn split_direct_question_keeps_affordance_across_finalized_chunks() {
        let finals = finalize_growing_question(
            "What do you think",
            "What do you think about shipping this week?",
        );
        assert_eq!(
            finals
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>()
                .join(" "),
            "What do you think about shipping this week?"
        );
        assert!(finals.iter().any(|line| line.possible_question));
    }

    #[test]
    fn direct_question_cue_expires_at_sentence_boundary_and_sixty_four_words() {
        for punctuation in [".", "!", "?"] {
            let first = format!("You already approved{punctuation}");
            let second = format!("{first} Is Monday available?");
            let finals = finalize_growing_question(&first, &second);
            assert!(
                !finals
                    .iter()
                    .find(|line| line.text.contains("Monday"))
                    .unwrap()
                    .possible_question
            );
        }
        let first = format!("You {}", vec!["word"; 64].join(" "));
        let finals = finalize_growing_question(&first, &format!("{first} Is Monday available?"));
        assert!(finals.iter().all(|line| !line.possible_question));
    }

    #[test]
    fn stop_flush_preserves_split_question_context_without_marking_partial_event() {
        let mut assembler = StreamCaptionAssembler::default();
        let mut history = LiveTranscriptHistory::default();
        for (text, end) in [
            ("What do you think", 3),
            ("What do you think about shipping this week?", 6),
        ] {
            for line in assembler.update(
                &transcript(&[(text, 0.0, end as f64)]),
                0,
                end * 1000,
                "m",
                "others",
            ) {
                history.insert(line);
            }
        }
        for line in assembler.update(
            &transcript(&[("about shipping this week?", 0.0, 3.0)]),
            3000,
            7000,
            "m",
            "others",
        ) {
            assert!(!line.finalized);
            assert!(!line.possible_question);
            let json = serde_json::to_value(&line).unwrap();
            assert!(json.get("questionIfFinal").is_none());
            history.insert(line);
        }
        let flushed = history.confirmed_tail();
        assert_eq!(flushed.len(), 1);
        assert_eq!(flushed[0].text, "about shipping this week?");
        assert!(flushed[0].possible_question);
    }

    fn retirement_with_later_revision(first_current: &str, end: f64) -> Vec<LiveCaptionPayload> {
        let mut assembler = StreamCaptionAssembler::default();
        assembler.update(
            &transcript(&[
                ("alpha beta gamma delta", 0.0, 3.0),
                ("zulu yankee xray", 3.0, 5.0),
            ]),
            0,
            6000,
            "m",
            "others",
        );
        assembler.update(
            &transcript(&[
                (first_current, 0.0, end),
                ("completely different words here", 3.0, 5.0),
            ]),
            0,
            12000,
            "m",
            "others",
        )
    }

    #[test]
    fn unrelated_retirement_preserves_aged_entire_prefix_extension() {
        let lines = retirement_with_later_revision("alpha beta gamma delta epsilon", 3.5);
        let finals: Vec<_> = lines
            .iter()
            .filter(|line| line.finalized)
            .map(|line| line.text.as_str())
            .collect();
        assert_eq!(
            finals,
            ["alpha beta gamma delta epsilon", "zulu yankee xray"]
        );
    }

    #[test]
    fn retirement_never_adopts_rewritten_prefix_or_fresh_extension() {
        for (text, end) in [
            ("alpha beta revised delta epsilon", 3.5),
            ("alpha beta gamma delta epsilon", 9.0),
        ] {
            let lines = retirement_with_later_revision(text, end);
            assert_eq!(lines[0].text, "alpha beta gamma delta");
            assert!(lines[0].finalized);
            assert!(lines
                .iter()
                .filter(|line| line.finalized)
                .all(|line| !line.text.contains("epsilon")));
        }
    }

    #[test]
    fn nearly_nominal_tick_preserves_replaced_question() {
        let mut assembler = StreamCaptionAssembler::default();
        assembler.update(
            &transcript(&[("Can you answer?", 0.0, 3.0)]),
            0,
            3000,
            "m",
            "others",
        );
        let next = assembler.update(
            &transcript(&[("The next statement.", 0.0, 3.0)]),
            3000,
            5990,
            "m",
            "others",
        );
        assert!(next
            .iter()
            .any(|line| line.finalized && line.text == "Can you answer?"));
    }

    #[test]
    fn boundary_timestamp_jitter_does_not_discard_novel_question() {
        let mut assembler = StreamCaptionAssembler {
            committed_end: 20000,
            committed_tail: "yeah".into(),
            ..Default::default()
        };
        let question = "Jakub, what do you think about shipping the panel this week?";
        let first = assembler.update(
            &transcript(&[(question, 0.0, 10.0)]),
            19000,
            29000,
            "m",
            "others",
        );
        assert!(first.iter().any(|line| line.text == question));
        let next = assembler.update(
            &transcript(&[("The next statement.", 0.0, 3.0)]),
            29000,
            32000,
            "m",
            "others",
        );
        assert_eq!(
            next.iter()
                .filter(|line| line.finalized && line.text == question)
                .count(),
            1
        );
        assert!(next.iter().any(|line| line.possible_question));
    }

    #[test]
    fn non_speech_tokens_never_become_live_caption_content() {
        let mut assembler = StreamCaptionAssembler::default();
        for marker in [
            "[BLANK_AUDIO]",
            "[SILENCE]",
            "[Music]",
            "(silence)",
            "[NO_SPEECH]",
            "(speaking in foreign language)",
            "[ Silence ]",
            "[Music playing]",
            "(upbeat music)",
            "[ INAUDIBLE ]",
            "[_BEG_]",
        ] {
            let input = transcript(&[(marker, 0.0, 1.0)]);
            assert!(assembler.update(&input, 0, 4000, "m", "others").is_empty());
            assert!(assembler.update(&input, 0, 7000, "m", "others").is_empty());
        }
        // Parenthesized speech is not a decoder label merely because it has brackets.
        assert_eq!(speech_text("(Can you answer?)"), "(Can you answer?)");
        let speech = transcript(&[("[BLANK_AUDIO] Can you answer? [Music]", 0.0, 1.0)]);
        let lines = assembler.update(&speech, 0, 10000, "m", "others");
        assert_eq!(lines[0].text, "Can you answer?");
    }

    #[test]
    fn growing_segment_keeps_all_committed_prefixes_without_duplicates_past_tail_bound() {
        let mut assembler = StreamCaptionAssembler::default();
        let words: Vec<_> = (0..150).map(|n| format!("word{n}")).collect();
        let mut final_words = Vec::new();
        for tick in 1..=50 {
            let text = words[..tick * 3].join(" ");
            for line in assembler.update(
                &transcript(&[(&text, 0.0, (tick * 3) as f64)]),
                0,
                (tick * 3000) as u64,
                "m",
                "others",
            ) {
                if line.finalized {
                    final_words.extend(line.text.split_whitespace().map(str::to_owned));
                }
            }
            assert_eq!(
                final_words,
                words[..tick.saturating_sub(1) * 3],
                "every stable word appears once, tick {tick}"
            );
            assert!(assembler.committed_tail.split_whitespace().count() <= 64);
        }
    }

    #[test]
    fn ambiguous_old_overlap_stays_visible_but_cannot_recommit() {
        let mut assembler = StreamCaptionAssembler {
            committed_end: 3000,
            committed_tail: "already committed words".into(),
            ..Default::default()
        };
        let input = transcript(&[("revised old words and novel speech", 0.0, 6.0)]);
        for now in [9000, 12000, 15000] {
            let output = assembler.update(&input, 0, now, "m", "others");
            assert!(output.iter().all(|line| !line.finalized));
            assert!(assembler
                .pending
                .iter()
                .any(|line| line.text.contains("novel speech")));
        }
    }

    #[test]
    fn repeated_long_anchor_remains_partial_instead_of_duplicating_history() {
        let mut assembler = StreamCaptionAssembler {
            committed_end: 90000,
            committed_tail: vec!["again"; 64].join(" "),
            ..Default::default()
        };
        let repeated = vec!["again"; 100].join(" ");
        let input = transcript(&[(&repeated, 0.0, 100.0)]);
        for now in [103000, 106000, 109000] {
            let output = assembler.update(&input, 0, now, "m", "others");
            assert!(output.iter().all(|line| !line.finalized));
            assert!(!assembler.pending.is_empty());
        }
    }

    /// Opt-in real local Whisper replay; input must be operator-owned synthetic speech.
    /// Prints only counts/timestamps, never transcript content.
    #[test]
    #[ignore = "requires MURMUR_LIVE_PROBE_WAV and MURMUR_LIVE_PROBE_MODEL synthetic fixtures"]
    fn native_live_window_probe_finalizes_synthetic_speech() {
        use crate::transcribe::live_asr::LiveAsr;
        let wav = std::env::var("MURMUR_LIVE_PROBE_WAV").unwrap();
        let model = std::env::var("MURMUR_LIVE_PROBE_MODEL").unwrap();
        let (samples, rate) = crate::audio::wav::read_wav_mono(std::path::Path::new(&wav)).unwrap();
        let mut samples = crate::audio::resample_to_16k(&samples, rate).unwrap();
        samples.extend(vec![0.0; 18 * 16000]);
        let asr =
            crate::transcribe::whisper::Transcriber::load(std::path::Path::new(&model)).unwrap();
        let mut assembler = StreamCaptionAssembler::default();
        let mut finals = 0;
        for end_s in (3..samples.len() / 16000).step_by(3) {
            let begin_s = end_s.saturating_sub(14);
            let transcript = asr
                .transcribe_live(&samples[begin_s * 16000..end_s * 16000], Some("en"))
                .unwrap();
            let output = assembler.update(
                &transcript,
                begin_s as u64 * 1000,
                end_s as u64 * 1000,
                "probe",
                "others",
            );
            finals += output.iter().filter(|line| line.finalized).count();
            eprintln!(
                "live_probe end_s={end_s} segments={} emitted={} finals={finals} spans={:?}",
                transcript.segments.len(),
                output.len(),
                transcript
                    .segments
                    .iter()
                    .map(|s| (s.start_s, s.end_s, s.text.split_whitespace().count()))
                    .collect::<Vec<_>>()
            );
        }
        assert!(
            finals >= 2,
            "real rolling speech must produce scrollback before Stop"
        );
    }

    #[test]
    fn restarted_assembler_preserves_history_and_never_reuses_line_identity() {
        let mut history = LiveTranscriptHistory::default();
        let old = history.insert(line("Earlier committed speech"));
        let mut first = StreamCaptionAssembler::resumed(5000);
        let mut second = StreamCaptionAssembler::resumed(5000);
        let next = transcript(&[("Old speech", 0.0, 1.0), ("New speech", 6.0, 7.0)]);
        let initial = first.update(&next, 0, 10000, "m", "me");
        assert_eq!(initial.len(), 1);
        assert_eq!(initial[0].text, "New speech");
        let distinct = second.update(&next, 0, 10000, "m", "me");
        assert_ne!(initial[0].line_id, distinct[0].line_id);
        let finalized = first.update(&next, 0, 13000, "m", "me");
        assert_eq!(finalized.len(), 1);
        assert!(finalized[0].finalized);
        assert_eq!(initial[0].line_id, finalized[0].line_id);
        history.insert(finalized[0].clone());
        let page = history.page("m", None, 200);
        assert_eq!(page.lines.len(), 2);
        assert_eq!(page.lines[0].text, "Earlier committed speech");
        assert_eq!(page.lines[0].seq, old.seq);
        assert_eq!(page.lines[1].seq, Some(2));
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
        state
            .live_transcript_lines
            .lock()
            .unwrap()
            .health
            .captions_state = CaptionsState::Stopped;
        assert!(crate::commands::ensure_caption_restart_allowed(&state, &id.to_string()).is_ok());
        state
            .live_transcript_lines
            .lock()
            .unwrap()
            .health
            .captions_state = CaptionsState::Starting;
        assert!(crate::commands::ensure_caption_restart_allowed(&state, &id.to_string()).is_err());
        state
            .live_transcript_lines
            .lock()
            .unwrap()
            .health
            .captions_state = CaptionsState::ModelError;
        *state.current_meeting.lock().unwrap() = None;
        assert!(crate::commands::ensure_caption_restart_allowed(&state, &id.to_string()).is_err());
        *state.current_meeting.lock().unwrap() = Some(id);
        assert!(crate::commands::ensure_caption_restart_allowed(&state, &id.to_string()).is_ok());
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
            crate::commands::ensure_caption_restart_allowed(&state, &id.to_string()),
            Err(AppError::Locked(_))
        ));
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
        assert_eq!(
            state
                .live_transcript_lines
                .lock()
                .unwrap()
                .health
                .captions_state,
            CaptionsState::ModelError
        );
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
    others: String,
    #[serde(flatten)]
    health: CaptionsHealth,
}

pub(crate) fn health(app: &AppHandle, meeting_id: &str, others: &str) {
    let state = app.state::<AppState>();
    let _lifecycle = crate::commands::lifecycle_guard(&state);
    update_health_under_lifecycle(app, &state, meeting_id, Some(others), None);
}

pub(crate) fn captions_health(app: &AppHandle, meeting_id: &str, health: CaptionsHealth) {
    let state = app.state::<AppState>();
    let _lifecycle = crate::commands::lifecycle_guard(&state);
    captions_health_under_lifecycle(app, &state, meeting_id, health);
}

/// The recording start owner already holds lifecycle; never acquire it recursively.
pub(crate) fn captions_health_under_lifecycle(
    app: &AppHandle,
    state: &AppState,
    meeting_id: &str,
    health: CaptionsHealth,
) {
    update_health_under_lifecycle(app, state, meeting_id, None, Some(health));
}

fn update_health_under_lifecycle(
    app: &AppHandle,
    state: &AppState,
    meeting_id: &str,
    others: Option<&str>,
    health: Option<CaptionsHealth>,
) {
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
    let payload = match state.live_transcript_lines.lock() {
        Ok(mut history) => {
            if history.meeting_id != meeting_id {
                history.clear();
                history.meeting_id = meeting_id.into();
                history.others_state = "starting".into();
            }
            if !history.update_health(others, health) {
                return;
            }
            LiveTranscriptHealth {
                meeting_id,
                others: history.others_state.clone(),
                health: history.health.clone(),
            }
        }
        Err(_) => return,
    };
    // No content or paths are included. Replay of transcript content retains its lock gate.
    let _ = app.emit(crate::events::EVENT_LIVE_TRANSCRIPT_HEALTH, payload);
}

#[cfg(test)]
mod health_tests {
    use super::*;

    #[test]
    fn others_health_cannot_clear_worker_error_or_stopped_state() {
        let mut history = LiveTranscriptHistory::default();
        for status in [
            CaptionsState::ModelError,
            CaptionsState::Retrying,
            CaptionsState::Stopped,
        ] {
            history.update_health(
                None,
                Some(CaptionsHealth {
                    captions_state: status,
                    ..Default::default()
                }),
            );
            history.update_health(Some("ready"), None);
            assert_eq!(history.health.captions_state, status);
            assert_eq!(history.others_state, "ready");
            assert!(
                !history.update_health(Some("ready"), None),
                "unchanged health emits no event"
            );
        }
    }

    #[test]
    fn captions_model_failure_is_replayed_without_exposing_paths_or_losing_scrollback() {
        let mut history = LiveTranscriptHistory::default();
        history.insert(LiveCaptionPayload {
            text: "The earlier utterance".into(),
            meeting_id: "m".into(),
            speaker: "me".into(),
            captured_at: "2026-09-19T12:00:00Z".into(),
            offset_ms: 1000,
            line_id: "me-1".into(),
            seq: None,
            finalized: true,
            is_question: false,
            possible_question: false,
            confirmed: true,
            question_if_final: false,
        });
        history.health = CaptionsHealth {
            captions_state: CaptionsState::ModelError,
            model_label: Some("whisper:small".into()),
            tick_interval_ms: Some(3000),
        };
        let page = serde_json::to_value(history.page("m", None, 200)).unwrap();
        assert_eq!(page["captionsState"], "model-error");
        assert_eq!(page["modelLabel"], "whisper:small");
        assert_eq!(page["tickIntervalMs"], 3000);
        assert_eq!(page["lines"][0]["text"], "The earlier utterance");
        assert_eq!(page["lines"][0]["seq"], 1);
        let event = serde_json::to_value(LiveTranscriptHealth {
            meeting_id: "m",
            others: "degraded".into(),
            health: history.health.clone(),
        })
        .unwrap();
        assert_eq!(event["meetingId"], "m");
        assert_eq!(event["captionsState"], "model-error");
        assert_eq!(event.as_object().unwrap().len(), 5);
        assert!(event.get("text").is_none());
        assert!(event.get("lines").is_none());
        history.health.captions_state = CaptionsState::Retrying;
        assert_eq!(history.page("m", None, 200).lines[0].seq, Some(1));
        assert_eq!(
            history.page("m", None, 200).health.captions_state,
            CaptionsState::Retrying
        );
        assert!(history
            .page("another", None, 200)
            .health
            .model_label
            .is_none());
    }
}
