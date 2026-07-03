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
                // Carry the ASR confidence through the wall-clock merge unchanged (metadata).
                confidence: seg.confidence,
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

/// ── Cross-stream echo suppression (speakers → mic bleed) ────────────────────────────────
///
/// On speakers, the remote voice is captured twice: clean on the system stream and, tens of
/// ms to ~1 s later, as acoustic bleed on the mic — so the same sentence lands as `others`
/// AND `me`. This pass drops the `me` echo copies. Thresholds follow prod-proven values
/// (screenpipe PR #4440 strict tier; OpenWhispr relaxed tier) — see
/// docs/research/2026-07-02-audio-echo-full-remediation.md.
///
/// CONTENT-LOSS GUARDS (load-bearing):
///   - NO dedup fires without measured acoustic leak evidence — echo only exists when the user
///     is on speakers, so headphones / no-leak recordings are never touched (BOTH tiers gated);
///   - only `me` segments are ever dropped; the clean `others*` copy ALWAYS survives;
///   - both sides need ≥ 4 tokens (short real acks — "okay", "yes ship it" — are immune);
///   - echo is CAUSAL (it lags its source), so a `me` segment that starts before the matching
///     `others` (beyond a small jitter tolerance) is the user speaking first, NOT echo — kept;
///   - identical text outside the echo window is genuine repetition and survives.
const ECHO_MIN_TOKENS: usize = 4;
/// Echo lags its source, so the strict window is asymmetric: a small causal back-tolerance for
/// merge/timestamp jitter, then forward to cover the echo lag + residual offset.
const ECHO_CAUSAL_BACK_S: f64 = 0.5;
const ECHO_STRICT_FWD_S: f64 = 2.0;
const ECHO_STRICT_JACCARD: f32 = 0.85;
const ECHO_RELAXED_AFTER_S: f64 = 4.0;
const ECHO_RELAXED_SIMILARITY: f32 = 0.7;
const ECHO_CONCAT_MAX: usize = 3;
const ECHO_CONCAT_MAX_GAP_S: f64 = 1.0;

/// Lowercased Unicode-alphanumeric tokens ("Zamknijmy budżet!" → ["zamknijmy","budżet"]).
fn norm_tokens(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

/// Word-set Jaccard: |A∩B| / |A∪B| over unique tokens.
fn jaccard(a: &[String], b: &[String]) -> f32 {
    use std::collections::HashSet;
    let sa: HashSet<&String> = a.iter().collect();
    let sb: HashSet<&String> = b.iter().collect();
    let inter = sa.intersection(&sb).count();
    let union = sa.union(&sb).count();
    if union == 0 {
        0.0
    } else {
        inter as f32 / union as f32
    }
}

/// Token-level longest-common-subsequence ratio normalized by the shorter side (word-order
/// sensitive — protects against shared-vocabulary false positives).
fn token_lcs(a: &[String], b: &[String]) -> f32 {
    let shorter = a.len().min(b.len());
    if shorter == 0 {
        return 0.0;
    }
    let mut dp = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for i in 0..a.len() {
        for j in 0..b.len() {
            dp[i + 1][j + 1] = if a[i] == b[j] {
                dp[i][j] + 1
            } else {
                dp[i][j + 1].max(dp[i + 1][j])
            };
        }
    }
    dp[a.len()][b.len()] as f32 / shorter as f32
}

/// Is this segment on the clean (system) side? Diarization may have relabelled to others-N.
fn is_others(seg: &Segment) -> bool {
    seg.speaker.as_deref().map(|s| s != SPEAKER_ME).unwrap_or(false)
}

/// One candidate = a run of 1..=ECHO_CONCAT_MAX consecutive `others` segments (close in time).
struct Candidate {
    start_s: f64,
    tokens: Vec<String>,
    text_norm: String,
}

fn build_candidates(others: &[&Segment]) -> Vec<Candidate> {
    let mut out = Vec::new();
    for i in 0..others.len() {
        let mut tokens = Vec::new();
        let mut text = String::new();
        for k in 0..ECHO_CONCAT_MAX.min(others.len() - i) {
            if k > 0 {
                let gap = others[i + k].start_s - others[i + k - 1].end_s;
                if gap > ECHO_CONCAT_MAX_GAP_S {
                    break;
                }
                text.push(' ');
            }
            tokens.extend(norm_tokens(&others[i + k].text));
            text.push_str(&others[i + k].text.to_lowercase());
            out.push(Candidate {
                start_s: others[i].start_s,
                tokens: tokens.clone(),
                text_norm: norm_tokens(&text).join(" "),
            });
        }
    }
    out
}

/// Drop `me` segments that are echo copies of `others` speech. Returns the cleaned,
/// re-indexed segments + the suppressed count. See the tier rules in the header comment.
pub fn suppress_cross_stream_echo(
    segments: Vec<Segment>,
    leak: Option<&crate::audio::align::EchoLeak>,
) -> (Vec<Segment>, usize) {
    // Echo only exists when the user is on speakers, which is exactly what a measured leak means.
    // No leak ⇒ no echo ⇒ NOTHING is deduplicated (headphones / unreliable-offset recordings are
    // returned untouched). This gates BOTH tiers — the strict tier is no longer ungated.
    let leak_armed = leak
        .map(|l| l.correlation >= crate::audio::align::MIN_CORR)
        .unwrap_or(false);
    let others_refs: Vec<&Segment> = segments.iter().filter(|s| is_others(s)).collect();
    if !leak_armed || others_refs.is_empty() {
        return (segments, 0);
    }
    let candidates = build_candidates(&others_refs);

    let mut drop = vec![false; segments.len()];
    for (i, seg) in segments.iter().enumerate() {
        if seg.speaker.as_deref() != Some(SPEAKER_ME) {
            continue;
        }
        let me_tokens = norm_tokens(&seg.text);
        if me_tokens.len() < ECHO_MIN_TOKENS {
            continue;
        }
        let me_norm = me_tokens.join(" ");
        for cand in &candidates {
            if cand.tokens.len() < ECHO_MIN_TOKENS {
                continue;
            }
            // `delta` = how much LATER the `me` (echo) copy starts than the `others` source.
            // Echo lags, so both windows run from a small causal back-tolerance forward.
            // (leak evidence is already guaranteed — the fn early-returned otherwise.)
            let delta = seg.start_s - cand.start_s;
            let strict_hit = (-ECHO_CAUSAL_BACK_S..=ECHO_STRICT_FWD_S).contains(&delta)
                && jaccard(&me_tokens, &cand.tokens) >= ECHO_STRICT_JACCARD;
            // Relaxed tier uses ORDER-PRESERVING signals only (equality, contiguous substring,
            // or token-LCS): acoustic echo is the SAME speech, so Whisper decodes it in the same
            // word order. An order-INSENSITIVE metric (multiset coverage) was dropped because it
            // eats a genuine `me` line whose words merely reappear — reordered — in a nearby
            // `others` segment (lock-security finding). Content-loss beats garbled-echo recall.
            let relaxed_hit = (-ECHO_CAUSAL_BACK_S..=ECHO_RELAXED_AFTER_S).contains(&delta)
                && (me_norm == cand.text_norm
                    || cand.text_norm.contains(&me_norm)
                    || me_norm.contains(&cand.text_norm)
                    || token_lcs(&me_tokens, &cand.tokens) >= ECHO_RELAXED_SIMILARITY);
            if strict_hit || relaxed_hit {
                drop[i] = true;
                break;
            }
        }
    }

    let suppressed = drop.iter().filter(|d| **d).count();
    let mut out: Vec<Segment> = segments
        .into_iter()
        .zip(drop)
        .filter_map(|(s, d)| if d { None } else { Some(s) })
        .collect();
    for (i, seg) in out.iter_mut().enumerate() {
        seg.idx = i as i64;
    }
    (out, suppressed)
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
            confidence: None,
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

    /// WS8 DEGRADATION (crash-safe default-on guard). With `capture_system_audio` ON, if system
    /// capture yields NOTHING — Screen-Recording (TCC) permission denied (the fresh-install state),
    /// or the sidecar spawned but captured no audio → an EMPTY "others" stream — the merge must still
    /// produce the COMPLETE mic transcript, every segment attributed "me", with no panic and no lost
    /// content. This is the "degrade to mic-only, never fail the recording" contract the default-on
    /// flip depends on, proven at the merge layer: the `audio::system` spawn/stop layer returns
    /// `Ok(None)` on capture failure and the pipeline then feeds only the mic StreamInput (see
    /// `pipeline::run_inner`). Complements `single_stream_passthrough_is_mic_only_me` (stream ABSENT)
    /// with the stream-PRESENT-but-EMPTY case.
    #[test]
    fn empty_system_stream_degrades_to_full_mic_only() {
        let origin = Instant::now();
        let mic = StreamInput {
            segments: vec![
                seg(0, 0.0, 2.0, "the whole call recorded on my mic"),
                seg(1, 2.0, 4.0, "second line survives too"),
            ],
            started_at: origin,
            speaker: SPEAKER_ME,
        };
        // System capture failed/denied → an EMPTY stream (zero segments), same start instant.
        let system = StreamInput {
            segments: Vec::new(),
            started_at: origin,
            speaker: SPEAKER_OTHERS,
        };
        let out = merge_streams(vec![mic, system]);
        // No content lost: both mic segments survive, in order, re-indexed.
        assert_eq!(out.len(), 2, "mic transcript is complete despite the absent far side");
        assert!(
            out.iter().all(|s| s.speaker.as_deref() == Some("me")),
            "with no system stream every segment is attributed to me"
        );
        assert_eq!(
            out.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(),
            vec!["the whole call recorded on my mic", "second line survives too"]
        );
        assert_eq!(out.iter().map(|s| s.idx).collect::<Vec<_>>(), vec![0, 1]);
        assert_eq!(out[0].start_s, 0.0, "no spurious time shift from the empty system stream");
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

    // ── Cross-stream echo dedup ───────────────────────────────────────────────────────────

    use crate::audio::align::EchoLeak;

    fn seg_sp(start_s: f64, end_s: f64, text: &str, speaker: &str) -> Segment {
        Segment { idx: 0, start_s, end_s, text: text.into(), speaker: Some(speaker.into()), confidence: None }
    }
    fn leak(corr: f32) -> EchoLeak {
        EchoLeak { offset_s: 0.3, correlation: corr }
    }

    /// STRICT tier (no leak evidence needed): a ≥4-token near-identical "me" copy shortly
    /// after the clean "others" original is echo — the "me" copy is dropped.
    #[test]
    fn strict_echo_pair_drops_the_me_copy() {
        let segs = vec![
            seg_sp(5.0, 7.0, "the contract is now signed by both parties", "others"),
            seg_sp(5.4, 7.3, "the contract is now signed by both parties", "me"),
        ];
        let (out, n) = suppress_cross_stream_echo(segs, Some(&leak(0.9)));
        assert_eq!(n, 1);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].speaker.as_deref(), Some("others"), "the clean copy survives");
        assert_eq!(out[0].idx, 0, "re-indexed");
    }

    /// HEADPHONES IMMUNITY (RED-before-GREEN): with NO measured leak, echo is physically
    /// impossible, so even a verbatim cross-stream match is genuine repetition — NOTHING is
    /// dropped. Dedup requires leak evidence in BOTH tiers.
    #[test]
    fn no_dedup_without_leak_evidence() {
        let segs = vec![
            seg_sp(5.0, 7.0, "the contract is now signed by both parties", "others"),
            seg_sp(5.4, 7.3, "the contract is now signed by both parties", "me"),
        ];
        let (out, n) = suppress_cross_stream_echo(segs, None);
        assert_eq!(n, 0, "no leak ⇒ no dedup (headphones are immune)");
        assert_eq!(out.len(), 2);
        // A weak leak below the arming threshold is also treated as no evidence.
        let segs2 = vec![
            seg_sp(5.0, 7.0, "the contract is now signed by both parties", "others"),
            seg_sp(5.4, 7.3, "the contract is now signed by both parties", "me"),
        ];
        let (_, n2) = suppress_cross_stream_echo(segs2, Some(&leak(0.2)));
        assert_eq!(n2, 0, "sub-threshold correlation is not leak evidence");
    }

    /// CAUSALITY (RED-before-GREEN): echo LAGS its source. A `me` segment that starts well
    /// before a verbatim `others` segment is the user speaking FIRST — never echo — even under
    /// strong leak evidence. The old symmetric ±2 s window would have eaten it.
    #[test]
    fn me_before_others_is_not_echo() {
        let segs = vec![
            // me starts 1.5 s BEFORE the matching others → user spoke first.
            seg_sp(5.0, 6.5, "the contract is now signed by both parties", "me"),
            seg_sp(6.5, 8.0, "the contract is now signed by both parties", "others"),
        ];
        let (out, n) = suppress_cross_stream_echo(segs, Some(&leak(0.9)));
        assert_eq!(n, 0, "a me line preceding the others match is not echo");
        assert_eq!(out.len(), 2);
    }

    /// Genuine short agreement ("yes ship it", 3 tokens) must SURVIVE even with strong
    /// leak evidence — the ≥4-token floor protects real speech.
    #[test]
    fn short_genuine_agreement_survives() {
        let segs = vec![
            seg_sp(5.0, 6.5, "let's ship it on friday", "others"),
            seg_sp(6.6, 7.2, "yes ship it", "me"),
        ];
        let (out, n) = suppress_cross_stream_echo(segs, Some(&leak(0.9)));
        assert_eq!(n, 0);
        assert_eq!(out.len(), 2);
    }

    /// Identical text 10 s apart is a real repetition, not echo — both survive.
    #[test]
    fn identical_text_far_apart_survives() {
        let segs = vec![
            seg_sp(5.0, 6.0, "we need to finalize the budget today", "others"),
            seg_sp(15.0, 16.0, "we need to finalize the budget today", "me"),
        ];
        let (out, n) = suppress_cross_stream_echo(segs, Some(&leak(0.9)));
        assert_eq!(n, 0);
        assert_eq!(out.len(), 2);
    }

    /// Two simultaneous short acks ("okay") are two people agreeing — both survive.
    #[test]
    fn simultaneous_short_acks_survive() {
        let segs = vec![
            seg_sp(5.0, 5.4, "okay", "others"),
            seg_sp(5.1, 5.5, "okay", "me"),
        ];
        let (out, n) = suppress_cross_stream_echo(segs, Some(&leak(0.9)));
        assert_eq!(n, 0);
        assert_eq!(out.len(), 2);
    }

    /// RELAXED tier: with leak evidence, a garbled echo (Whisper decodes the distorted mic
    /// copy differently) still matches by token coverage/LCS ≥ 0.7 and is dropped.
    /// Without leak evidence the relaxed tier is DISARMED and the segment survives.
    #[test]
    fn relaxed_tier_is_gated_on_leak_evidence() {
        let make = || vec![
            seg_sp(5.0, 7.0, "we should finalize the budget proposal by monday", "others"),
            seg_sp(5.6, 7.4, "we should finalize the budget proposal monday", "me"),
        ];
        let (out, n) = suppress_cross_stream_echo(make(), Some(&leak(0.9)));
        assert_eq!(n, 1, "garbled echo dropped under leak evidence");
        assert_eq!(out.len(), 1);
        // Force a relaxed-only pair (Jaccard < 0.85, coverage ≥ 0.7) to prove the gate:
        let relaxed_only = || vec![
            seg_sp(5.0, 7.0, "we should finalize the annual budget proposal by monday morning", "others"),
            seg_sp(5.6, 7.4, "finalize the annual budget proposal monday", "me"),
        ];
        let (_, n_gated) = suppress_cross_stream_echo(relaxed_only(), Some(&leak(0.9)));
        assert_eq!(n_gated, 1, "relaxed tier fires with evidence");
        let (out2, n2) = suppress_cross_stream_echo(relaxed_only(), None);
        assert_eq!(n2, 0, "no leak evidence ⇒ relaxed tier disarmed");
        assert_eq!(out2.len(), 2);
    }

    /// Diarized labels ("others-2") count as the clean side.
    #[test]
    fn diarized_others_labels_count_as_others() {
        let segs = vec![
            seg_sp(5.0, 7.0, "the migration plan looks good to me", "others-2"),
            seg_sp(5.3, 7.2, "the migration plan looks good to me", "me"),
        ];
        let (out, n) = suppress_cross_stream_echo(segs, Some(&leak(0.9)));
        assert_eq!(n, 1);
        assert_eq!(out[0].speaker.as_deref(), Some("others-2"));
    }

    /// Segmentation mismatch: the echo lands as ONE "me" segment while the system pass split
    /// it into two adjacent segments — the concatenated candidate still matches.
    #[test]
    fn concatenated_adjacent_others_match_the_echo() {
        let segs = vec![
            seg_sp(5.0, 6.0, "we need to finalize", "others"),
            seg_sp(6.1, 7.2, "the budget by monday", "others"),
            seg_sp(5.4, 7.5, "we need to finalize the budget by monday", "me"),
        ];
        let (out, n) = suppress_cross_stream_echo(segs, Some(&leak(0.9)));
        assert_eq!(n, 1);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|s| s.speaker.as_deref() != Some("me")));
    }

    /// Mic-only meetings (no others segments) are untouched.
    #[test]
    fn mic_only_is_untouched() {
        let segs = vec![
            seg_sp(1.0, 2.0, "just me talking to myself here", "me"),
            seg_sp(3.0, 4.0, "still me talking", "me"),
        ];
        let (out, n) = suppress_cross_stream_echo(segs.clone(), Some(&leak(0.9)));
        assert_eq!(n, 0);
        assert_eq!(out.len(), 2);
    }

    /// CONTENT-LOSS REGRESSION (lock-security finding): a GENUINE `me` line that only shares
    /// vocabulary — in a DIFFERENT order — with a nearby `others` segment must SURVIVE, even
    /// under strong leak evidence within the relaxed window. The order-insensitive `coverage`
    /// metric scored this 1.0 and ate it; order-preserving token-LCS scores 4/6 < 0.7 → kept.
    /// (Real acoustic echo preserves word order, so this does not weaken echo removal.)
    #[test]
    fn genuine_paraphrase_with_shared_vocab_survives() {
        let segs = vec![
            seg_sp(5.0, 8.0, "we need to finalize the budget by monday i think", "others"),
            // Same words, reordered — the user genuinely agreeing, NOT an echo copy.
            seg_sp(6.0, 7.0, "i think we need the budget", "me"),
        ];
        let (out, n) = suppress_cross_stream_echo(segs, Some(&leak(0.9)));
        assert_eq!(n, 0, "a reordered genuine paraphrase must not be dropped");
        assert_eq!(out.len(), 2);
        assert!(out.iter().any(|s| s.speaker.as_deref() == Some("me")));
    }
}
