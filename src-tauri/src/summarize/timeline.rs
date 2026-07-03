//! AI-derived interactive timeline: speaker turns + topic spans from the transcript.
//!
//! Whisper doesn't diarize, so we ask the configured provider to infer speaker turns
//! (named if identifiable in the conversation, else "User N") and topic spans from the
//! timestamped transcript, returning strict JSON we parse into [`MeetingTimeline`].

use crate::error::Result;
use crate::storage::models::MeetingTimeline;
use crate::summarize::provider::SummarizerProvider;
use crate::transcribe::types::Segment;

const SYSTEM: &str = "You are an expert meeting analyst. You receive a meeting transcript \
as timestamped lines `[start-end] (speaker) text` (seconds). The `(speaker)` tag comes from \
on-device diarization: `me` is the person recording the meeting; `others`, `others-0`, \
`others-1`, … are the DISTINCT people on the other side of the call. Output STRICT JSON ONLY — \
no prose, no markdown, no code fences — with EXACTLY this shape:\n\
{\"speakers\":[{\"speaker\":\"Speaker 1\",\"startS\":0.0,\"endS\":12.5}],\
\"topics\":[{\"label\":\"Budget\",\"startS\":0.0,\"endS\":60.0}]}\n\
Rules:\n\
- speakers: use the `(speaker)` TAGS as the source of truth for who is talking. Map `me` to \
the recording user (their real name if clearly stated), and each distinct `others-N` to a \
consistent label (a real name if clearly stated in the conversation, else \"Speaker 1\", \
\"Speaker 2\", …). Build consecutive, non-overlapping turns in time order from the tags; do NOT \
invent speaker changes the tags don't support. Cover the whole timeline.\n\
- topics: segment the meeting into 3-8 main topics/threads, each a short 2-4 word label \
with its start/end span; sequential spans covering the discussion.\n\
- Use only timestamps from the transcript; the final endS should be near the meeting end.\n\
- Output ONLY the JSON object, nothing before or after it.";

/// Ask the provider to derive the timeline from `segments`, then parse strict JSON out of
/// the (possibly noisy) reply.
pub async fn generate(
    provider: &dyn SummarizerProvider,
    segments: &[Segment],
    _duration_s: i64,
) -> Result<MeetingTimeline> {
    let transcript: String = segments
        .iter()
        .map(|s| {
            // Feed the canonical diarization tag (me / others / others-N) so the LLM-derived
            // timeline AGREES with the segment speaker labels instead of inventing its own.
            let who = s.speaker.as_deref().unwrap_or("?");
            format!("[{:.1}-{:.1}] ({}) {}", s.start_s, s.end_s, who, s.text.trim())
        })
        .collect::<Vec<_>>()
        .join("\n");

    // Minimal JSON schema for the timeline — passed to the gateway for native constrained decoding;
    // the DEFAULT `complete_json` impl only stringifies it into the system prompt (same parse path
    // as before). The schema covers the two required arrays so the gateway can enforce the shape.
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "speakers": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "speaker": {"type": "string"},
                        "startS":  {"type": "number"},
                        "endS":    {"type": "number"}
                    },
                    "required": ["speaker", "startS", "endS"],
                    "additionalProperties": false
                }
            },
            "topics": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "label":  {"type": "string"},
                        "startS": {"type": "number"},
                        "endS":   {"type": "number"}
                    },
                    "required": ["label", "startS", "endS"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["speakers", "topics"],
        "additionalProperties": false
    });
    let v = provider.complete_json(SYSTEM, &transcript, &schema).await?;
    let mut tl: MeetingTimeline = serde_json::from_value(v).map_err(|e| {
        crate::error::AppError::Summarize(format!("timeline: invalid JSON shape from provider: {e}"))
    })?;
    // The provider prompt asks it to "cover the whole timeline", but in practice — especially on
    // SPARSE transcripts with silence gaps, and on smaller/local models — it drops the trailing
    // segments and ends the timeline far short of the recording (and occasionally overshoots with
    // a hallucinated endS). Repair deterministically so the derived timeline actually spans the
    // transcript instead of collapsing into an early cluster.
    repair_coverage(&mut tl, segments);
    Ok(tl)
}

/// Small epsilon (seconds) below which a coverage gap is ignored — avoids extending a span for
/// sub-second float noise while still catching the real "timeline ends at 0:14 for a 0:45
/// recording" gap this repairs.
const COVER_EPS: f64 = 0.25;

/// Deterministically repair an AI-derived timeline so it COVERS the transcript.
///
/// The provider tends to (a) drop trailing/sparse segments — leaving the timeline ending well
/// before the recording does — and occasionally (b) emit a wildly out-of-range `endS`. Both make
/// the FE's shared scale disagree with the audio player (the "timeline shows only a few seconds of
/// a 45s recording" bug). We anchor to the LAST TRANSCRIBED SEGMENT — deliberately NOT the raw
/// recording duration — so a meeting with a genuinely silent tail still lets the FE zoom to the
/// meaningful content rather than being stretched across dead air. Steps, per track:
///   1. clamp every span to `[0, content_end]` (kills the hallucinated overshoot + inverted spans),
///   2. drop spans left degenerate (`end <= start`) after clamping,
///   3. extend the last (max-`end`) turn/topic to `content_end` when it falls meaningfully short,
///      so both the speaker lanes and the topic ribbon reach the end of the transcript.
///
/// Idempotent (running it twice is a no-op) and pure over `(timeline, segments)`, so it can also
/// heal a legacy cached timeline when read back, not just freshly-generated ones.
pub fn repair_coverage(tl: &mut MeetingTimeline, segments: &[Segment]) {
    let content_end = segments.iter().map(|s| s.end_s).fold(0.0_f64, f64::max);
    if content_end <= 0.0 {
        return; // no transcript span to anchor to — leave the timeline untouched.
    }
    cover(
        &mut tl.speakers,
        content_end,
        |t| (t.start_s, t.end_s),
        |t, s, e| {
            t.start_s = s;
            t.end_s = e;
        },
    );
    cover(
        &mut tl.topics,
        content_end,
        |t| (t.start_s, t.end_s),
        |t, s, e| {
            t.start_s = s;
            t.end_s = e;
        },
    );
}

/// Clamp → drop-degenerate → extend-last for one track. Generic over the span type via
/// get/set accessors so `SpeakerTurn` and `TopicSpan` share the exact same repair.
fn cover<T>(
    items: &mut Vec<T>,
    content_end: f64,
    get: impl Fn(&T) -> (f64, f64),
    set: impl Fn(&mut T, f64, f64),
) {
    for it in items.iter_mut() {
        let (s, e) = get(it);
        let s = s.clamp(0.0, content_end);
        let e = e.clamp(s, content_end);
        set(it, s, e);
    }
    items.retain(|it| {
        let (s, e) = get(it);
        e > s
    });
    // Extend whichever span currently ends latest to the transcript end, so the track covers the
    // recording. (Extending the LAST span — not inserting a synthetic one — keeps the real labels.)
    let last = items
        .iter()
        .enumerate()
        .max_by(|a, b| {
            get(a.1)
                .1
                .partial_cmp(&get(b.1).1)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(i, _)| i);
    if let Some(i) = last {
        let (s, e) = get(&items[i]);
        if content_end - e > COVER_EPS {
            set(&mut items[i], s, content_end);
        }
    }
}

/// Extract the first balanced JSON object from a reply and parse it into a [`MeetingTimeline`].
///
/// Used directly in unit tests to validate the free-text extraction path (the same path the
/// DEFAULT `complete_json` impl uses). Production code now calls `complete_json` which
/// subsumes this step — so this function is compiled only in test mode.
#[cfg(test)]
fn parse(reply: &str) -> Result<MeetingTimeline> {
    // Recover the FIRST balanced top-level JSON object via the string/escape-aware extractor in
    // `reason.rs` instead of the brittle `find('{')..=rfind('}')` slice — the old slice swept up a
    // stray `}` in trailing prose (or a second object) and then failed to parse a valid reply.
    crate::reason::parse_first_json(reply)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_clean_json() {
        let r = r#"{"speakers":[{"speaker":"User 1","startS":0,"endS":5}],"topics":[{"label":"Intro","startS":0,"endS":5}]}"#;
        let t = parse(r).unwrap();
        assert_eq!(t.speakers.len(), 1);
        assert_eq!(t.speakers[0].speaker, "User 1");
        assert_eq!(t.topics[0].label, "Intro");
    }

    #[test]
    fn extracts_json_from_fenced_reply_with_aliases() {
        let r = "Here you go:\n```json\n{\"speakers\":[],\"topics\":[{\"label\":\"X\",\"start\":1,\"end\":2}]}\n```\n";
        let t = parse(r).unwrap();
        assert_eq!(t.topics.len(), 1);
        assert_eq!(t.topics[0].start_s, 1.0); // "start" alias → start_s
        assert_eq!(t.topics[0].end_s, 2.0); // "end" alias → end_s
    }

    #[test]
    fn errors_on_no_json() {
        assert!(parse("no json here").is_err());
    }

    /// RED-before-GREEN: the OLD `find('{')..=rfind('}')` slice extended to the stray `}` in the
    /// trailing prose, producing trailing garbage that `serde_json::from_str` rejected.
    /// `parse_first_json` stops at the first balanced object and parses the timeline cleanly.
    /// (Verified RED: reverting `parse` to the rfind slice makes this `unwrap()` panic.)
    #[test]
    fn parses_despite_trailing_prose_with_stray_brace() {
        let r = r#"{"speakers":[{"speaker":"User 1","startS":0,"endS":5}],"topics":[{"label":"Intro","startS":0,"endS":5}]} (note: } end of object)"#;
        let t = parse(r).unwrap();
        assert_eq!(t.speakers.len(), 1);
        assert_eq!(t.speakers[0].speaker, "User 1");
        assert_eq!(t.topics[0].label, "Intro");
    }

    // ── repair_coverage: make the AI timeline actually span the transcript ────────────────

    use crate::storage::models::{MeetingTimeline, SpeakerTurn, TopicSpan};

    fn seg(start_s: f64, end_s: f64) -> Segment {
        Segment {
            idx: 0,
            start_s,
            end_s,
            text: "x".into(),
            speaker: Some("me".into()),
        }
    }
    fn turn(start_s: f64, end_s: f64) -> SpeakerTurn {
        SpeakerTurn {
            speaker: "Jakub".into(),
            start_s,
            end_s,
        }
    }
    fn topic(label: &str, start_s: f64, end_s: f64) -> TopicSpan {
        TopicSpan {
            label: label.into(),
            start_s,
            end_s,
        }
    }

    /// RED-before-GREEN: the exact production bug (meeting fe25c4ee) — segments cover 0.2..44.5 but
    /// the provider's timeline ends at 13.7. Before `repair_coverage`, the timeline stays at 13.7
    /// (< 0.9× the 45s recording → the FE zooms the axis to ~0:14). After, both tracks reach 44.5.
    #[test]
    fn repair_extends_short_timeline_to_last_segment() {
        let segments = vec![seg(0.2, 3.9), seg(9.9, 13.7), seg(29.4, 31.6), seg(43.5, 44.5)];
        let mut tl = MeetingTimeline {
            speakers: vec![turn(0.2, 3.9), turn(9.9, 13.7)],
            topics: vec![topic("Test Brain Note", 0.2, 7.9), topic("Architecture Issue", 7.9, 13.7)],
        };
        repair_coverage(&mut tl, &segments);
        let sp_max = tl.speakers.iter().map(|t| t.end_s).fold(0.0, f64::max);
        let tp_max = tl.topics.iter().map(|t| t.end_s).fold(0.0, f64::max);
        assert!((sp_max - 44.5).abs() < 1e-6, "speakers should reach 44.5, got {sp_max}");
        assert!((tp_max - 44.5).abs() < 1e-6, "topics should reach 44.5, got {tp_max}");
        // Labels + earlier spans are preserved; only the LAST span was extended.
        assert_eq!(tl.topics.len(), 2);
        assert_eq!(tl.topics[0].end_s, 7.9);
        assert_eq!(tl.topics[1].label, "Architecture Issue");
        assert_eq!(tl.topics[1].end_s, 44.5);
    }

    /// A hallucinated far-future `endS` is clamped back to the transcript end (the eee2e31e case:
    /// segments to 82.6, provider emitted endS=1200).
    #[test]
    fn repair_clamps_hallucinated_overshoot() {
        let segments = vec![seg(0.5, 40.0), seg(80.0, 82.6)];
        let mut tl = MeetingTimeline {
            speakers: vec![turn(0.5, 1200.0)],
            topics: vec![topic("Intro", 0.0, 5.0), topic("Deep dive", 5.0, 1200.0)],
        };
        repair_coverage(&mut tl, &segments);
        assert!((tl.speakers[0].end_s - 82.6).abs() < 1e-6);
        assert!((tl.topics[1].end_s - 82.6).abs() < 1e-6);
    }

    /// Idempotent: repairing an already-repaired timeline changes nothing.
    #[test]
    fn repair_is_idempotent() {
        let segments = vec![seg(0.0, 10.0), seg(40.0, 44.5)];
        let mut tl = MeetingTimeline {
            speakers: vec![turn(0.0, 10.0)],
            topics: vec![topic("A", 0.0, 6.0), topic("B", 6.0, 10.0)],
        };
        repair_coverage(&mut tl, &segments);
        let once = serde_json::to_string(&tl).unwrap();
        repair_coverage(&mut tl, &segments);
        assert_eq!(once, serde_json::to_string(&tl).unwrap());
    }

    /// When the timeline already covers the transcript, repair does not over-extend (no spurious
    /// stretch past the real content).
    #[test]
    fn repair_noop_when_already_covers() {
        let segments = vec![seg(0.0, 30.0)];
        let mut tl = MeetingTimeline {
            speakers: vec![turn(0.0, 30.0)],
            topics: vec![topic("Whole", 0.0, 30.0)],
        };
        repair_coverage(&mut tl, &segments);
        assert_eq!(tl.topics[0].end_s, 30.0);
        assert_eq!(tl.speakers[0].end_s, 30.0);
    }

    /// A span entirely beyond the transcript end is dropped (clamps to a zero-width span, then
    /// pruned); the remaining last span is extended to cover.
    #[test]
    fn repair_drops_span_entirely_beyond_content() {
        let segments = vec![seg(0.0, 20.0)];
        let mut tl = MeetingTimeline {
            speakers: vec![turn(0.0, 5.0)],
            topics: vec![topic("Real", 0.0, 8.0), topic("Ghost", 50.0, 60.0)],
        };
        repair_coverage(&mut tl, &segments);
        assert_eq!(tl.topics.len(), 1, "the beyond-content ghost span is dropped");
        assert_eq!(tl.topics[0].label, "Real");
        assert!((tl.topics[0].end_s - 20.0).abs() < 1e-6, "the surviving span covers to 20.0");
    }

    /// No segments (or all zero-length) → nothing to anchor to → the timeline is left untouched
    /// (no panic, no bogus extension).
    #[test]
    fn repair_noop_on_empty_segments() {
        let mut tl = MeetingTimeline {
            speakers: vec![turn(0.0, 5.0)],
            topics: vec![topic("A", 0.0, 5.0)],
        };
        repair_coverage(&mut tl, &[]);
        assert_eq!(tl.speakers[0].end_s, 5.0);
        assert_eq!(tl.topics[0].end_s, 5.0);
    }
}
