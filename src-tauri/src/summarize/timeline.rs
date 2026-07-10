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

/// OOM guard (P0.2, perf-memory-audit §2): the max joined-transcript size fed to an ON-DEVICE
/// model when deriving the timeline. A 1h meeting joins to ~15–25k tokens; the local mistralrs
/// engine plans for `max_seq_len = 4096`, so a whole-hour prompt blows past it and the prefill KV
/// cache balloons several GB on top of the resident weights → the machine OOMs. The timeline is a
/// COARSE speaker/topic map — it does not need every word — so we cap the joined transcript that
/// reaches a local model. ~14k chars ≈ a few thousand tokens, comfortably inside the 4096 window
/// with headroom for the system prompt + the JSON reply. Cloud providers are NOT capped (big
/// context windows, and they are not the OOM path — see `generate`). Bytes, not tokens: cheap,
/// deterministic, pure-testable.
const LOCAL_TIMELINE_MAX_CHARS: usize = 14_000;

/// True when `provider_id` names an ON-DEVICE model (the on-device brain, Ollama, or Apple
/// Foundation Models) — the residency-bound engines the transcript cap protects. Mirrors
/// `related_context::is_weak_provider` (same three connection ids) so the two OOM-relevant
/// classifications never drift; this file owns the canonical timeline OOM classification.
///
/// `pub(crate)` so the command layer can ask "would generating this meeting's timeline load a
/// residency-bound on-device model?" — the gate that keeps a passive Audio-tab open from firing a
/// multi-GB synchronous model load (perf-memory-audit; the whole-Mac beachball on open).
pub(crate) fn is_on_device_provider(provider_id: &str) -> bool {
    provider_id == crate::summarize::roles::CONN_LOCAL
        || provider_id == crate::summarize::roles::CONN_AFM
        || provider_id == crate::summarize::PROVIDER_OLLAMA
}

/// Render ONE transcript line for a segment, preserving its real `[start-end]` timestamps + the
/// canonical diarization tag (me / others / others-N) so the LLM-derived timeline AGREES with the
/// segment speaker labels (and its time-spans stay anchored to real meeting time) instead of
/// inventing its own.
fn render_segment_line(s: &Segment) -> String {
    let who = s.speaker.as_deref().unwrap_or("?");
    format!("[{:.1}-{:.1}] ({}) {}", s.start_s, s.end_s, who, s.text.trim())
}

/// Build the timestamped transcript fed to the provider.
///
/// For an ON-DEVICE model we bound the prompt to `LOCAL_TIMELINE_MAX_CHARS` (the KV/OOM lever, P0.2)
/// via UNIFORM DECIMATION — NOT head-truncation. A naive head-cut would derive the whole timeline
/// from only the first minutes of a 1h meeting (a correctness bug); instead we keep an EVENLY-SPACED
/// stride of segments across the FULL duration, so the coarse timeline still spans the entire
/// meeting, just at lower resolution. We always keep the FIRST and LAST segment (the timeline's real
/// start/end anchors). Deterministic + pure over `segments` (unit-tested). Cloud providers pass every
/// segment (`on_device == false` ⇒ no cap, no decimation — byte-identical to the pre-guard prompt).
fn build_transcript(segments: &[Segment], on_device: bool) -> String {
    let full = |segs: &[Segment]| {
        segs.iter()
            .map(render_segment_line)
            .collect::<Vec<_>>()
            .join("\n")
    };
    if !on_device {
        return full(segments);
    }
    let joined = full(segments);
    if joined.chars().count() <= LOCAL_TIMELINE_MAX_CHARS || segments.len() <= 2 {
        // Already within budget (short meeting), or too few segments to decimate meaningfully.
        return joined;
    }
    // Pick an EVENLY-SPACED subset that fits the char budget by its ACTUAL rendered cost — NOT a
    // mean-line estimate + a trailing `take()`. The estimate approach was wrong on non-uniform
    // transcripts: a few long monologue turns make the mean under-count the strided subset's real
    // size, the estimate overshoots the budget, and a tail trim then silently DROPS the last kept
    // segments — re-introducing the very head-only coverage this guards against (adversarial finding
    // 2026-07-08). Instead we START from an even-stride subset and SHRINK `keep` until the real joined
    // cost is within budget, so the FIRST + LAST segment (the timeline's end anchors) always survive
    // and the tail is never chopped.
    let n = segments.len();
    // Cost of a strided subset of `keep` segments (rendered line chars + '\n' joins), without
    // allocating the whole string each time — sum the picked lines' lengths.
    let stride_indices = |keep: usize| -> Vec<usize> {
        let mut idxs: Vec<usize> = Vec::with_capacity(keep);
        for k in 0..keep {
            // k ∈ [0, keep-1] → index in [0, n-1]; k=0 → first, k=keep-1 → last.
            let idx = (k * (n - 1)) / (keep - 1);
            if idxs.last() != Some(&idx) {
                idxs.push(idx);
            }
        }
        idxs
    };
    let cost = |idxs: &[usize]| -> usize {
        let chars: usize = idxs
            .iter()
            .map(|&i| render_segment_line(&segments[i]).chars().count())
            .sum();
        chars + idxs.len().saturating_sub(1) // '\n' joins
    };
    // Start from a generous even-stride guess, then shrink proportionally until within budget. Each
    // step strictly decreases `keep` (min(scaled, keep-1)) so it terminates; the floor is 2 (just the
    // first + last anchors), which we accept even if two pathological giant anchor segments exceed the
    // budget — keeping BOTH ends beats dropping the last.
    let mut keep = segments.len();
    loop {
        let idxs = stride_indices(keep);
        let c = cost(&idxs);
        if c <= LOCAL_TIMELINE_MAX_CHARS || keep <= 2 {
            return idxs
                .iter()
                .map(|&i| render_segment_line(&segments[i]))
                .collect::<Vec<_>>()
                .join("\n");
        }
        // Proportional shrink, but force progress (at least -1) so we can't stall on a slight overage.
        let scaled = (keep.saturating_mul(LOCAL_TIMELINE_MAX_CHARS)) / c.max(1);
        keep = scaled.min(keep - 1).max(2);
    }
}

/// Ask the provider to derive the timeline from `segments`, then parse strict JSON out of
/// the (possibly noisy) reply.
///
/// OOM guard (P0.2): for an ON-DEVICE provider the transcript is decimated to
/// `LOCAL_TIMELINE_MAX_CHARS` (uniform-stride, full-coverage) so a 1h prompt cannot balloon the
/// local engine's prefill KV cache and OOM the machine. Cloud providers pass the full transcript.
pub async fn generate(
    provider: &dyn SummarizerProvider,
    segments: &[Segment],
    _duration_s: i64,
) -> Result<MeetingTimeline> {
    let transcript = build_transcript(segments, is_on_device_provider(provider.id()));

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
        crate::error::AppError::Summarize(format!(
            "timeline: invalid JSON shape from provider: {e}"
        ))
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
            confidence: None,
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
        let segments = vec![
            seg(0.2, 3.9),
            seg(9.9, 13.7),
            seg(29.4, 31.6),
            seg(43.5, 44.5),
        ];
        let mut tl = MeetingTimeline {
            speakers: vec![turn(0.2, 3.9), turn(9.9, 13.7)],
            topics: vec![
                topic("Test Brain Note", 0.2, 7.9),
                topic("Architecture Issue", 7.9, 13.7),
            ],
        };
        repair_coverage(&mut tl, &segments);
        let sp_max = tl.speakers.iter().map(|t| t.end_s).fold(0.0, f64::max);
        let tp_max = tl.topics.iter().map(|t| t.end_s).fold(0.0, f64::max);
        assert!(
            (sp_max - 44.5).abs() < 1e-6,
            "speakers should reach 44.5, got {sp_max}"
        );
        assert!(
            (tp_max - 44.5).abs() < 1e-6,
            "topics should reach 44.5, got {tp_max}"
        );
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
        assert_eq!(
            tl.topics.len(),
            1,
            "the beyond-content ghost span is dropped"
        );
        assert_eq!(tl.topics[0].label, "Real");
        assert!(
            (tl.topics[0].end_s - 20.0).abs() < 1e-6,
            "the surviving span covers to 20.0"
        );
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

    // ── P0.2: bound the transcript fed to a LOCAL model (the KV/OOM lever) ─────────────────────────

    /// Build a synthetic ~1h transcript: many segments, each with realistic text, spread evenly
    /// across `dur_s` seconds. Returns segments whose joined form vastly exceeds
    /// `LOCAL_TIMELINE_MAX_CHARS` so the cap is exercised.
    fn hour_of_segments(n: usize, dur_s: f64) -> Vec<Segment> {
        (0..n)
            .map(|i| {
                let start = (i as f64) * dur_s / (n as f64);
                let end = start + (dur_s / (n as f64));
                Segment {
                    idx: i as i64,
                    start_s: start,
                    end_s: end,
                    // ~60-char line of real-ish content so the join is big.
                    text: format!(
                        "segment {i} discussing the quarterly roadmap and budget details here"
                    ),
                    speaker: Some(if i % 2 == 0 { "me" } else { "others" }.into()),
                    confidence: None,
                }
            })
            .collect()
    }

    /// RED-before-GREEN (OOM guard): an ON-DEVICE provider ("local") gets a transcript BOUNDED to
    /// `LOCAL_TIMELINE_MAX_CHARS`, while a CLOUD provider ("anthropic") gets the FULL transcript
    /// (no regression — cloud has a big context window and is not the OOM path).
    #[test]
    fn local_transcript_is_capped_cloud_is_not() {
        // ~1h of dense conversation: 1200 segments × ~75 chars ≈ 90k chars — well past the 14k cap.
        let segments = hour_of_segments(1200, 3600.0);

        let full = build_transcript(&segments, false); // cloud provider
        let capped = build_transcript(&segments, true); // on-device provider

        // Cloud: every segment present, uncapped.
        assert_eq!(
            full.lines().count(),
            segments.len(),
            "cloud provider must receive the FULL transcript (no cap)"
        );
        assert!(
            full.chars().count() > LOCAL_TIMELINE_MAX_CHARS,
            "the synthetic 1h transcript must exceed the cap so the test is meaningful"
        );

        // On-device: bounded to the cap.
        assert!(
            capped.chars().count() <= LOCAL_TIMELINE_MAX_CHARS,
            "on-device transcript must be capped at {LOCAL_TIMELINE_MAX_CHARS}, got {}",
            capped.chars().count()
        );
        assert!(
            capped.chars().count() < full.chars().count(),
            "the on-device transcript must be strictly smaller than the full one"
        );
    }

    /// COVERAGE (not head-truncation): the decimated on-device transcript must still SPAN the whole
    /// meeting — the LAST kept segment's start time must be in the LATTER portion of the recording,
    /// and the FIRST kept line must be near the start. A naive head-cut would fail this (its last
    /// line would be in the first minutes). Parses the `[start-end]` prefix of the kept lines.
    #[test]
    fn local_transcript_preserves_full_coverage() {
        let dur_s = 3600.0;
        let segments = hour_of_segments(1200, dur_s);
        let capped = build_transcript(&segments, true);

        // Parse the leading `[start-` float off each kept line.
        let starts: Vec<f64> = capped
            .lines()
            .filter_map(|l| {
                let inner = l.strip_prefix('[')?;
                let dash = inner.find('-')?;
                inner[..dash].parse::<f64>().ok()
            })
            .collect();
        assert!(starts.len() >= 2, "expected multiple kept lines, got {starts:?}");

        let first = *starts.first().unwrap();
        let last = *starts.last().unwrap();
        assert!(
            first < dur_s * 0.05,
            "first kept segment must be near the start (< 3 min of a 1h meeting), got {first}s"
        );
        assert!(
            last > dur_s * 0.9,
            "COVERAGE: last kept segment must be in the final tenth of the meeting (not head-only), got {last}s"
        );
        // Kept lines are in time order (evenly-spaced stride), so the span is monotonic.
        assert!(
            starts.windows(2).all(|w| w[0] <= w[1]),
            "kept segments must stay in time order"
        );
    }

    /// COVERAGE under NON-UNIFORM line lengths — the adversarial regression (2026-07-08). A minority
    /// of long monologue turns (~2000 chars) among short turns made the OLD `mean_line` estimate
    /// under-count the strided subset, overshoot the budget, and then a trailing `take()` silently
    /// DROPPED the last kept lines — re-introducing head-only coverage (repro: last kept ≈ 3160s of
    /// 3600s; extreme: 0s). This asserts the fix: the LAST segment always survives and the prompt is
    /// still bounded, regardless of length skew. RED on the old mean-estimate + tail-`take()` code.
    #[test]
    fn local_transcript_coverage_survives_length_skew() {
        let dur_s = 3600.0;
        let n = 600usize;
        // 10% long monologues (~2000 chars), 90% short (~200 chars), spread across the hour.
        let segments: Vec<Segment> = (0..n)
            .map(|i| {
                let start = (i as f64) * dur_s / (n as f64);
                let end = start + dur_s / (n as f64);
                let text = if i % 10 == 0 {
                    format!("monologue {i} ").repeat(140) // ~2000 chars
                } else {
                    format!("turn {i} ").repeat(25) // ~200 chars
                };
                Segment {
                    idx: i as i64,
                    start_s: start,
                    end_s: end,
                    text,
                    speaker: Some(if i % 2 == 0 { "me" } else { "others" }.into()),
                    confidence: None,
                }
            })
            .collect();

        let capped = build_transcript(&segments, true);
        // Bounded (the OOM guarantee): the on-device prompt stays within budget despite the skew.
        assert!(
            capped.chars().count() <= LOCAL_TIMELINE_MAX_CHARS,
            "skewed on-device transcript must stay within the cap, got {}",
            capped.chars().count()
        );
        // COVERAGE: the LAST kept line's start must be in the final tenth — the last segment (≈3600s)
        // is never dropped. This is the exact assertion the old tail-`take()` failed (last ≈ 3160s).
        let starts: Vec<f64> = capped
            .lines()
            .filter_map(|l| {
                let inner = l.strip_prefix('[')?;
                let dash = inner.find('-')?;
                inner[..dash].parse::<f64>().ok()
            })
            .collect();
        let last = *starts.last().expect("kept lines");
        assert!(
            last > dur_s * 0.9,
            "COVERAGE under skew: last kept segment must reach the final tenth (not head-only), got {last}s"
        );
    }

    /// A SHORT meeting (already within budget) is fed verbatim to an on-device model — no decimation,
    /// no lost segments — so small meetings are byte-identical for local + cloud.
    #[test]
    fn local_transcript_short_meeting_is_unchanged() {
        let segments = hour_of_segments(6, 30.0); // ~6 short lines, well under 14k chars
        let capped = build_transcript(&segments, true);
        let full = build_transcript(&segments, false);
        assert_eq!(
            capped, full,
            "a short meeting under the cap must be identical for local + cloud"
        );
        assert_eq!(capped.lines().count(), segments.len());
    }

    /// `is_on_device_provider` classifies exactly the three residency-bound connection ids (mirrors
    /// `related_context::is_weak_provider`); cloud providers are NOT on-device.
    #[test]
    fn on_device_provider_classification() {
        assert!(is_on_device_provider(crate::summarize::roles::CONN_LOCAL));
        assert!(is_on_device_provider(crate::summarize::roles::CONN_AFM));
        assert!(is_on_device_provider(crate::summarize::PROVIDER_OLLAMA));
        assert!(!is_on_device_provider(crate::summarize::PROVIDER_ANTHROPIC));
        assert!(!is_on_device_provider(
            crate::summarize::PROVIDER_CLAUDE_CODE
        ));
        assert!(!is_on_device_provider(crate::summarize::PROVIDER_GATEWAY));
    }
}
