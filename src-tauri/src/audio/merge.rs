//! Wall-clock merge of two independently-captured transcript streams into ONE attributed,
//! time-ordered segment list (Phase B "Me vs Others").
//!
//! ## Why this exists (the critical correctness detail)
//!
//! The mic (cpal) and the system audio (ScreenCaptureKit sidecar) are captured on SEPARATE
//! streams with INDEPENDENT clocks. Each is transcribed on its OWN timeline, so a segment's
//! `start_s`/`end_s` are RELATIVE to that stream's first sample (t=0 at its capture start).
//! You CANNOT merge them by sample count: the two clocks drift relative to each other
//! (seconds per hour), so "system sample N" and "mic sample N" do not refer to the same wall
//! moment. Concatenating or zipping by index would scramble who-said-what-when.
//!
//! The fix: anchor each stream to the HOST wall-clock instant at which it started capturing
//! (`Recorder::started_at`, and the system stream's spawn instant). The offset between the two
//! anchors converts every segment's stream-relative time into a shared ABSOLUTE timeline; we
//! then merge-sort by absolute start. The earlier-starting stream is the timeline origin
//! (absolute t=0), and the later stream's segments are shifted forward by its start delay.
//!
//! ## Attribution (cheap 2-way diarization)
//!
//! Each segment is labelled purely by WHICH stream produced it: mic → `"me"`, system → others.
//! HONEST DENTS:
//! - This is NOT per-remote-person diarization. Every remote participant collapses into the
//!   single `"others"` label — we attribute by capture stream, not by voice fingerprint.
//! - ECHO: if the user runs the call on SPEAKERS (not headphones), the mic re-captures the
//!   remote audio, so some "others" speech also lands in the mic stream and gets mislabelled
//!   "me". Recommend headphones (surface this in the UI later).
//! - If ScreenCaptureKit permission is denied / system capture is off, there is no system
//!   stream at all and the caller falls back to mic-only (everything "me").

use std::time::Instant;

use crate::transcribe::types::Segment;

/// The "me" stream label (local microphone).
pub const SPEAKER_ME: &str = "me";
/// The "others" stream label (captured system audio — all remote participants collapsed).
pub const SPEAKER_OTHERS: &str = "others";

/// One transcribed stream plus the host instant at which its capture started.
pub struct StreamInput {
    /// Stream-relative segments (start/end in seconds from THIS stream's first sample).
    pub segments: Vec<Segment>,
    /// Host wall-clock instant when this stream started capturing.
    pub started_at: Instant,
    /// Attribution label applied to every segment from this stream (`"me"` / `"others"`).
    pub speaker: &'static str,
}

/// Merge the mic + system streams into one absolute-timeline, speaker-attributed segment list.
///
/// Steps:
/// 1. Pick the EARLIER `started_at` as the absolute timeline origin (t=0).
/// 2. For each stream, shift its segment times forward by `started_at - origin` (its capture
///    delay relative to the origin) → absolute start/end.
/// 3. Tag every segment with its stream's speaker label.
/// 4. Drop empty/whitespace-only segments (a muted-mic span yields silence → Whisper yields
///    nothing → nothing to merge; this also prunes any stray blank Whisper output).
/// 5. Merge-sort by absolute start (ties broken by absolute end, then speaker for determinism).
/// 6. Re-index `idx` 0..N over the merged order.
///
/// Times never go negative: the origin is the earliest start, so every shift is `>= 0`.
pub fn merge_streams(streams: Vec<StreamInput>) -> Vec<Segment> {
    let Some(origin) = streams.iter().map(|s| s.started_at).min() else {
        return Vec::new();
    };

    let mut merged: Vec<Segment> = Vec::new();
    for stream in &streams {
        // Capture delay of this stream relative to the timeline origin, in seconds (>= 0).
        let offset_s = stream
            .started_at
            .saturating_duration_since(origin)
            .as_secs_f64();
        for seg in &stream.segments {
            // DROP fully-silent/empty segments (e.g. a muted mic span).
            if seg.text.trim().is_empty() {
                continue;
            }
            merged.push(Segment {
                idx: 0, // re-assigned after the sort
                start_s: seg.start_s + offset_s,
                end_s: seg.end_s + offset_s,
                text: seg.text.clone(),
                // Preserve a per-segment speaker (diarized "others-N") if set; else the stream default.
                speaker: Some(
                    seg.speaker
                        .clone()
                        .unwrap_or_else(|| stream.speaker.to_string()),
                ),
            });
        }
    }

    // Stable merge order by absolute start; deterministic tie-breaks so overlapping mic/system
    // segments (both parties talking at once) sort reproducibly rather than corrupting order.
    merged.sort_by(|a, b| {
        a.start_s
            .partial_cmp(&b.start_s)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(
                a.end_s
                    .partial_cmp(&b.end_s)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
            .then_with(|| a.speaker.cmp(&b.speaker))
    });

    for (i, seg) in merged.iter_mut().enumerate() {
        seg.idx = i as i64;
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn seg(idx: i64, start_s: f64, end_s: f64, text: &str) -> Segment {
        Segment {
            idx,
            start_s,
            end_s,
            text: text.into(),
            speaker: None,
        }
    }

    /// The core correctness test: two streams with DIFFERENT host start offsets must interleave
    /// by ABSOLUTE wall-clock time, not by stream-relative time, and carry the right labels.
    ///
    /// Scenario: system audio starts 5.0 s AFTER the mic.
    ///   mic    (origin):  "hello" @ 0..2 (abs 0..2),  "bye"  @ 10..11 (abs 10..11)
    ///   system (+5.0 s):  "hi"    @ 0..1 (abs 5..6),   "ok"   @ 4..5  (abs 9..10)
    /// Expected absolute order: hello(0) < hi(5) < ok(9) < bye(10).
    #[test]
    fn merges_by_absolute_wall_clock_not_relative() {
        let origin = Instant::now();
        let mic = StreamInput {
            segments: vec![seg(0, 0.0, 2.0, "hello"), seg(1, 10.0, 11.0, "bye")],
            started_at: origin,
            speaker: SPEAKER_ME,
        };
        let system = StreamInput {
            segments: vec![seg(0, 0.0, 1.0, "hi"), seg(1, 4.0, 5.0, "ok")],
            started_at: origin + Duration::from_secs(5),
            speaker: SPEAKER_OTHERS,
        };

        let out = merge_streams(vec![mic, system]);

        let order: Vec<(&str, Option<&str>)> = out
            .iter()
            .map(|s| (s.text.as_str(), s.speaker.as_deref()))
            .collect();
        assert_eq!(
            order,
            vec![
                ("hello", Some("me")),
                ("hi", Some("others")),
                ("ok", Some("others")),
                ("bye", Some("me")),
            ]
        );

        // Absolute times: the +5 s system stream is shifted forward.
        assert_eq!(out[0].start_s, 0.0); // hello
        assert_eq!(out[1].start_s, 5.0); // hi  (0 + 5)
        assert_eq!(out[2].start_s, 9.0); // ok  (4 + 5)
        assert_eq!(out[3].start_s, 10.0); // bye

        // idx is re-indexed over the merged order.
        assert_eq!(out.iter().map(|s| s.idx).collect::<Vec<_>>(), vec![0, 1, 2, 3]);
    }

    /// When the SYSTEM stream starts FIRST, it becomes the origin and the mic is shifted forward.
    #[test]
    fn earlier_stream_becomes_origin_either_direction() {
        let base = Instant::now();
        let system = StreamInput {
            segments: vec![seg(0, 0.0, 1.0, "remote-first")],
            started_at: base, // earlier
            speaker: SPEAKER_OTHERS,
        };
        let mic = StreamInput {
            segments: vec![seg(0, 0.0, 1.0, "me-later")],
            started_at: base + Duration::from_secs(3), // +3 s
            speaker: SPEAKER_ME,
        };

        // Order of args must not matter — pass mic first to prove offset is by start time.
        let out = merge_streams(vec![mic, system]);
        assert_eq!(out[0].text, "remote-first");
        assert_eq!(out[0].start_s, 0.0);
        assert_eq!(out[0].speaker.as_deref(), Some("others"));
        assert_eq!(out[1].text, "me-later");
        assert_eq!(out[1].start_s, 3.0); // shifted by the +3 s mic delay
        assert_eq!(out[1].speaker.as_deref(), Some("me"));
    }

    /// Overlapping speech (both talk at once) keeps BOTH segments and a deterministic order —
    /// no segment is dropped or its label corrupted.
    #[test]
    fn overlapping_segments_are_not_corrupted() {
        let origin = Instant::now();
        let mic = StreamInput {
            segments: vec![seg(0, 2.0, 4.0, "me-overlap")],
            started_at: origin,
            speaker: SPEAKER_ME,
        };
        let system = StreamInput {
            segments: vec![seg(0, 2.0, 5.0, "others-overlap")],
            started_at: origin, // same start → same absolute t
            speaker: SPEAKER_OTHERS,
        };
        let out = merge_streams(vec![mic, system]);
        assert_eq!(out.len(), 2, "both overlapping segments survive");
        // Equal start; tie broken by end (4.0 < 5.0) → mic ("me-overlap") first.
        assert_eq!(out[0].text, "me-overlap");
        assert_eq!(out[0].speaker.as_deref(), Some("me"));
        assert_eq!(out[1].text, "others-overlap");
        assert_eq!(out[1].speaker.as_deref(), Some("others"));
    }

    /// Empty / whitespace-only segments (e.g. a muted-mic silent span) are dropped.
    #[test]
    fn drops_empty_and_blank_segments() {
        let origin = Instant::now();
        let mic = StreamInput {
            segments: vec![
                seg(0, 0.0, 1.0, "kept"),
                seg(1, 1.0, 2.0, ""),     // empty (muted span → no speech)
                seg(2, 2.0, 3.0, "   "), // whitespace-only
            ],
            started_at: origin,
            speaker: SPEAKER_ME,
        };
        let out = merge_streams(vec![mic]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "kept");
        assert_eq!(out[0].idx, 0);
    }

    /// A single stream (mic-only fallback when system capture is off/denied) passes through
    /// labelled "me", re-indexed, with no time shift.
    #[test]
    fn single_stream_passthrough_is_mic_only_me() {
        let origin = Instant::now();
        let mic = StreamInput {
            segments: vec![seg(7, 0.0, 1.0, "a"), seg(9, 1.0, 2.0, "b")],
            started_at: origin,
            speaker: SPEAKER_ME,
        };
        let out = merge_streams(vec![mic]);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|s| s.speaker.as_deref() == Some("me")));
        assert_eq!(out.iter().map(|s| s.idx).collect::<Vec<_>>(), vec![0, 1]);
        assert_eq!(out[0].start_s, 0.0);
    }

    /// No streams → empty merge (no panic on the `min()` of an empty iterator).
    #[test]
    fn no_streams_yields_empty() {
        assert!(merge_streams(Vec::new()).is_empty());
    }

    /// REGRESSION (mute × wall-clock alignment — the load-bearing Phase-B invariant).
    ///
    /// A muted-mic span writes SILENCE that keeps the mic buffer full-length (see
    /// `recorder::accumulate_frames`), so Whisper yields NOTHING for those seconds and the
    /// FIRST post-unmute "me" segment keeps its true stream-relative start (~the mute
    /// duration). This test proves that the merge then places that post-unmute "me" segment
    /// at the correct ABSOLUTE position relative to the "others" speech that happened DURING
    /// the mute — i.e. the timeline did not collapse forward (which is exactly what dropping
    /// muted frames instead of writing silence would have caused).
    ///
    /// Scenario (both streams start together, mic muted 0..30 s):
    ///   mic    (origin):  [silence 0..30 → no segment], "back now" @ 30..31 (abs 30..31)
    ///   system (origin):  "remote talks while you're muted" @ 5..8 (abs 5..8)
    /// Expected absolute order: others(5) BEFORE me(30) — the mute did NOT pull "me" forward.
    #[test]
    fn muted_mic_span_keeps_post_unmute_segment_in_absolute_order() {
        let origin = Instant::now();
        let mic = StreamInput {
            // The 0..30 s muted span produced silence → Whisper emitted nothing for it; the
            // only mic segment is the real speech AFTER unmute, still at its true t≈30 s.
            segments: vec![seg(0, 30.0, 31.0, "back now")],
            started_at: origin,
            speaker: SPEAKER_ME,
        };
        let system = StreamInput {
            segments: vec![seg(0, 5.0, 8.0, "remote talks while you're muted")],
            started_at: origin,
            speaker: SPEAKER_OTHERS,
        };

        let out = merge_streams(vec![mic, system]);

        assert_eq!(out.len(), 2, "no audio/content lost across the mute boundary");
        // "others" (spoken during the mute) MUST precede the post-unmute "me" segment.
        assert_eq!(out[0].speaker.as_deref(), Some("others"));
        assert_eq!(out[0].start_s, 5.0, "others speech stays at its true wall-clock time");
        assert_eq!(out[1].speaker.as_deref(), Some("me"));
        assert_eq!(
            out[1].start_s, 30.0,
            "post-unmute mic segment is NOT pulled forward — the muted-silence span \
             preserved the mic stream's wall-clock timeline"
        );
        // Re-indexed over the merged order.
        assert_eq!(out.iter().map(|s| s.idx).collect::<Vec<_>>(), vec![0, 1]);
    }
}
