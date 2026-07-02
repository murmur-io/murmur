# Audio Echo Full Remediation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate the double-hearing of remote speakers (speaker→mic echo) from Murmur's recordings and transcripts — transcript-level echo dedup, time-aligned archive mixing, offline AEC with the system track as reference, VPIO containment, and honest UX hints.

**Architecture:** The remote voice enters twice (digitally via the system-audio capture, acoustically via the mic on speakers). We fix it in four independent layers: (1) a leak-gated cross-stream transcript dedup at merge time, (2) an offset-measured, peak-normalized archive mix, (3) post-hoc WebRTC-AEC3 (`sonora`) on the recorded mic using the digitally captured system track as the far-end reference, (4) containment of the existing VPIO helper (which research shows is likely 0 dB-effective and system-audio-ducking-harmful). Research basis: `docs/research/2026-07-02-audio-echo-full-remediation.md`.

**Tech Stack:** Rust (`src-tauri`, `meetnotes_lib`), Swift sidecars (built by `build.rs`), Angular 18 zoneless FE. New pure-Rust modules only in PR-1; `sonora` crate + `coreaudio-sys` direct dep in PR-2 (both **approval-gated**).

## Global Constraints

- Rules files are binding: `rust-tauri.md`, `angular-zoneless.md`, `lock-model.md`, `agentic-workflow.md`.
- Errors: `crate::error::Result<T>` + `AppError` only; no `unwrap()/expect()` outside tests.
- **No new crates/npm packages without explicit user approval** — Tasks 9 and 11 are approval-gated; Tasks 1–8 and 10 add ZERO dependencies.
- Test loop: `( cd src-tauri && cargo test --lib )` ONLY (never `cargo clippy --all-targets`); FE: `npx ng lint` + `npx ng build`. `bash scripts/ci.sh` ONCE at the end.
- No PII in logs: counts/IDs/durations only — never transcript text.
- Lock model: the AEC/system WAVs remain `$TMPDIR` scratch (ScratchWav delete-on-drop); the archive stays the same `<meeting_id>.wav` path. **No new at-rest audio file may be created** — that would require new seal paths and a lock-security review.
- Do NOT change the signature of `audio::mix` (used by `examples/e2e_core.rs:42` + `scripts/e2e-mix.sh`).
- Commits authored by `QueaT <kgm004a@gmail.com>`, no Claude trailers; merge to `murmur` via PR only.
- FE: signals-first, `@if/@for`, inline template/styles, `inject()`, tokens from `src/styles.css`.
- PR split: **PR-1 = Tasks 1–8** (no new deps, fully headless-verifiable), **PR-2 = Tasks 9–11** (approval-gated deps), Task 12 = verification (both PRs).

---

### Task 1: `audio/align.rs` — envelope-NCC stream-offset + leak estimator

**Files:**
- Create: `src-tauri/src/audio/align.rs`
- Modify: `src-tauri/src/audio/mod.rs` (add `pub mod align;` next to the existing `pub mod` lines)

**Interfaces:**
- Produces: `pub struct EchoLeak { pub offset_s: f64, pub correlation: f32 }` (derives `Debug, Clone, Copy`);
  `pub fn estimate_stream_offset(mic_16k: &[f32], sys_16k: &[f32]) -> Option<EchoLeak>`;
  `pub fn archive_delays(leak: Option<&EchoLeak>, mic_started: std::time::Instant, sys_started: Option<std::time::Instant>, rate_hz: u32) -> (usize, usize)` — `(mic_delay_samples, sys_delay_samples)`.
- Semantics: `offset_s > 0` means system-track content appears in the mic track `offset_s` seconds LATER (the echo lags its source) → to align, delay the SYSTEM track. `correlation` is the median normalized cross-correlation peak — the "user is on speakers" evidence (headphones → no peak → `None`).

- [ ] **Step 1: Write the failing tests** (bottom of the new `align.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic pseudo-random speech-like burst signal (LCG — no rand crate, reproducible).
    fn burst_signal(len: usize, seed: u64) -> Vec<f32> {
        let mut x = seed;
        let mut out = vec![0.0f32; len];
        let mut i = 0;
        while i < len {
            // ~0.8 s burst, ~0.7 s silence — envelope has structure to correlate on.
            for j in 0..12_800.min(len - i) {
                x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                out[i + j] = ((x >> 33) as f32 / u32::MAX as f32 - 0.5) * 0.6;
            }
            i += 12_800 + 11_200;
        }
        out
    }

    /// mic = attenuated copy of sys delayed by 240 ms + small independent noise
    /// → estimator must recover ≈ +0.24 s with strong correlation.
    #[test]
    fn recovers_synthetic_echo_offset() {
        let sys = burst_signal(16_000 * 90, 7);
        let delay = (0.24 * 16_000.0) as usize;
        let mut mic = vec![0.0f32; sys.len()];
        for i in delay..mic.len() {
            mic[i] = 0.3 * sys[i - delay];
        }
        let noise = burst_signal(mic.len(), 99);
        for (m, n) in mic.iter_mut().zip(noise.iter()) {
            *m += 0.02 * n;
        }
        let leak = estimate_stream_offset(&mic, &sys).expect("leak must be detected");
        assert!((leak.offset_s - 0.24).abs() <= 0.03, "offset {} ≉ 0.24", leak.offset_s);
        assert!(leak.correlation >= MIN_CORR, "correlation {} too weak", leak.correlation);
    }

    /// Headphones case: mic is INDEPENDENT of sys → no reliable peak → None.
    #[test]
    fn independent_streams_yield_none() {
        let sys = burst_signal(16_000 * 90, 7);
        let mic = burst_signal(16_000 * 90, 12345);
        assert!(estimate_stream_offset(&mic, &sys).is_none());
    }

    /// A silent system track (no remote speech) → None, never a panic.
    #[test]
    fn silent_system_yields_none() {
        let mic = burst_signal(16_000 * 30, 7);
        let sys = vec![0.0f32; 16_000 * 30];
        assert!(estimate_stream_offset(&mic, &sys).is_none());
        assert!(estimate_stream_offset(&mic, &[]).is_none());
        assert!(estimate_stream_offset(&[], &sys).is_none());
    }

    /// archive_delays: measured leak wins; positive offset delays the SYSTEM track.
    #[test]
    fn archive_delays_prefers_measured_leak() {
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        let leak = EchoLeak { offset_s: 0.5, correlation: 0.8 };
        assert_eq!(
            archive_delays(Some(&leak), t0, Some(t0), 16_000),
            (0, 8_000),
            "positive offset ⇒ pad the system track"
        );
        let leak_neg = EchoLeak { offset_s: -0.25, correlation: 0.8 };
        assert_eq!(archive_delays(Some(&leak_neg), t0, Some(t0), 16_000), (4_000, 0));
        // No leak → wall-clock anchors: system started 0.8 s AFTER the mic ⇒ pad system.
        let sys_started = t0 + Duration::from_millis(800);
        assert_eq!(archive_delays(None, t0, Some(sys_started), 16_000), (0, 12_800));
        // Mic started later ⇒ pad the mic.
        assert_eq!(archive_delays(None, sys_started, Some(t0), 16_000), (12_800, 0));
        // No system stream at all → no padding.
        assert_eq!(archive_delays(None, t0, None, 16_000), (0, 0));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd src-tauri && cargo test --lib audio::align`
Expected: compile FAIL — `estimate_stream_offset` / `EchoLeak` not defined.

- [ ] **Step 3: Write the implementation** (top of `align.rs`)

```rust
//! Offline mic↔system stream alignment + speaker-leak detection (envelope NCC).
//!
//! When the user records a call on SPEAKERS, the remote voice bleeds acoustically into the
//! mic. Cross-correlating the two tracks' 100 Hz RMS envelopes over a few windows yields:
//!   - `offset_s`  — how much later system content appears in the mic (echo lag + capture-
//!     start delta): the pad the archive mix needs, and the dedup window's center;
//!   - `correlation` — the leak strength: the "speakers, not headphones" evidence that arms
//!     the relaxed transcript-dedup tier. Headphones ⇒ no peak ⇒ `None` ⇒ dedup stays strict.
//! Pure, deterministic, no deps — fully unit-testable headless. Cost: <100 ms per meeting.

/// 16 kHz / 160 = 100 Hz envelope rate.
const ENV_HOP: usize = 160;
const ENV_RATE: f64 = 100.0;
/// Correlation windows: up to 3 × 60 s (start / middle / end).
const WINDOW_ENV: usize = 60 * 100;
/// Lag search range: ±2 s (covers capture-start delta + playout + acoustic path).
const MAX_LAG_ENV: i64 = 200;
/// Minimum system-envelope RMS for a window to count (must contain remote speech).
const MIN_SYS_RMS: f32 = 0.004;
/// Below this correlation there is no reliable leak (headphones / quiet speakers).
pub const MIN_CORR: f32 = 0.35;
/// Windows must agree on the lag within this spread, or the estimate is unreliable.
const MAX_SPREAD_S: f64 = 0.2;

#[derive(Debug, Clone, Copy)]
pub struct EchoLeak {
    /// Seconds by which system content appears LATER in the mic track (>0 ⇒ delay system).
    pub offset_s: f64,
    /// Median NCC peak strength across windows (leak evidence, 0..1).
    pub correlation: f32,
}

/// 100 Hz RMS envelope of a 16 kHz mono buffer.
fn envelope(samples: &[f32]) -> Vec<f32> {
    samples
        .chunks(ENV_HOP)
        .map(|c| (c.iter().map(|s| s * s).sum::<f32>() / c.len().max(1) as f32).sqrt())
        .collect()
}

/// Peak normalized cross-correlation between two mean-removed envelope windows.
/// Returns (lag_env_steps, ncc): lag > 0 ⇒ `mic` trails `sys`.
fn ncc_peak(mic_env: &[f32], sys_env: &[f32]) -> Option<(i64, f32)> {
    let n = mic_env.len().min(sys_env.len());
    if n < (MAX_LAG_ENV as usize) * 2 + 100 {
        return None;
    }
    let mean = |v: &[f32]| v[..n].iter().sum::<f32>() / n as f32;
    let (mm, sm) = (mean(mic_env), mean(sys_env));
    let dev = |v: &[f32], m: f32| v[..n].iter().map(|x| (x - m) * (x - m)).sum::<f32>().sqrt();
    let (md, sd) = (dev(mic_env, mm), dev(sys_env, sm));
    if md <= f32::EPSILON || sd <= f32::EPSILON {
        return None;
    }
    let mut best: Option<(i64, f32)> = None;
    for lag in -MAX_LAG_ENV..=MAX_LAG_ENV {
        let mut acc = 0.0f32;
        for i in 0..n {
            let j = i as i64 + lag;
            if j >= 0 && (j as usize) < n {
                acc += (mic_env[j as usize] - mm) * (sys_env[i] - sm);
            }
        }
        let ncc = acc / (md * sd);
        if best.map(|(_, b)| ncc > b).unwrap_or(true) {
            best = Some((lag, ncc));
        }
    }
    best
}

/// Estimate the mic↔system offset + leak strength on up to 3 windows. `None` when there is
/// no system speech, no reliable peak, or the windows disagree (drift / unreliable signal).
pub fn estimate_stream_offset(mic_16k: &[f32], sys_16k: &[f32]) -> Option<EchoLeak> {
    let mic_env = envelope(mic_16k);
    let sys_env = envelope(sys_16k);
    let n = mic_env.len().min(sys_env.len());
    if n < WINDOW_ENV / 2 {
        return None;
    }
    let starts: [usize; 3] = [0, n.saturating_sub(WINDOW_ENV) / 2, n.saturating_sub(WINDOW_ENV)];
    let mut peaks: Vec<(i64, f32)> = Vec::new();
    for &s in &starts {
        let e = (s + WINDOW_ENV).min(n);
        if e <= s {
            continue;
        }
        let sys_w = &sys_env[s..e];
        let rms = (sys_w.iter().map(|x| x * x).sum::<f32>() / sys_w.len() as f32).sqrt();
        if rms < MIN_SYS_RMS {
            continue; // no remote speech in this window
        }
        if let Some(p) = ncc_peak(&mic_env[s..e], sys_w) {
            peaks.push(p);
        }
    }
    // Dedup identical windows (short recordings collapse all 3 starts onto 0).
    peaks.dedup();
    if peaks.is_empty() {
        return None;
    }
    peaks.sort_by_key(|(lag, _)| *lag);
    let (med_lag, _) = peaks[peaks.len() / 2];
    let mut corrs: Vec<f32> = peaks.iter().map(|(_, c)| *c).collect();
    corrs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let med_corr = corrs[corrs.len() / 2];
    if med_corr < MIN_CORR {
        return None;
    }
    let spread = (peaks.last().map(|(l, _)| *l).unwrap_or(med_lag)
        - peaks.first().map(|(l, _)| *l).unwrap_or(med_lag)) as f64
        / ENV_RATE;
    if spread > MAX_SPREAD_S {
        return None; // windows disagree — do not feed a wrong pad into the mix
    }
    Some(EchoLeak { offset_s: med_lag as f64 / ENV_RATE, correlation: med_corr })
}

/// Front-padding (in samples) for the archive mix: `(mic_delay, sys_delay)`.
/// Priority: measured leak (also corrects the spawn-anchor error) → wall-clock anchors → 0.
pub fn archive_delays(
    leak: Option<&EchoLeak>,
    mic_started: std::time::Instant,
    sys_started: Option<std::time::Instant>,
    rate_hz: u32,
) -> (usize, usize) {
    let to_samples = |s: f64| (s.abs() * rate_hz as f64).round() as usize;
    if let Some(l) = leak {
        return if l.offset_s >= 0.0 { (0, to_samples(l.offset_s)) } else { (to_samples(l.offset_s), 0) };
    }
    match sys_started {
        Some(sys) if sys >= mic_started => {
            (0, to_samples(sys.saturating_duration_since(mic_started).as_secs_f64()))
        }
        Some(sys) => (to_samples(mic_started.saturating_duration_since(sys).as_secs_f64()), 0),
        None => (0, 0),
    }
}
```

Add to `src-tauri/src/audio/mod.rs`: `pub mod align;`

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd src-tauri && cargo test --lib audio::align`
Expected: 4 tests PASS. If `recovers_synthetic_echo_offset` is off by >0.03 s, check the lag sign convention in `ncc_peak` (mic index = sys index + lag).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/audio/align.rs src-tauri/src/audio/mod.rs
git commit -m "feat(audio): envelope-NCC mic/system offset + speaker-leak estimator"
```

---

### Task 2: Cross-stream transcript echo dedup in `audio/merge.rs`

**Files:**
- Modify: `src-tauri/src/audio/merge.rs` (append new fns + tests; `merge_streams` itself is UNCHANGED)

**Interfaces:**
- Consumes: `crate::audio::align::EchoLeak` (Task 1), `crate::transcribe::types::Segment { idx: i64, start_s: f64, end_s: f64, text: String, speaker: Option<String> }`, `SPEAKER_ME` (`merge.rs:36`).
- Produces: `pub fn suppress_cross_stream_echo(segments: Vec<Segment>, leak: Option<&crate::audio::align::EchoLeak>) -> (Vec<Segment>, usize)` — returns the cleaned, re-indexed list + the number of suppressed `me` segments.
- Semantics: only `speaker == Some("me")` segments are ever dropped; anything whose speaker is NOT `me` (`others`, diarized `others-N`) is the clean reference and always survives. Two tiers: **strict** (ungated — screenpipe-proven: both ≥4 tokens, |Δstart| ≤ 2.0 s, word-set Jaccard ≥ 0.85) and **relaxed** (armed ONLY by acoustic leak evidence `leak.correlation ≥ align::MIN_CORR`: ≥4 tokens, Δ = me.start − other.start ∈ [−1.5, +4.0] s, equality/containment or max(token coverage, token LCS)/shorter ≥ 0.7). Candidates include concatenations of up to 3 consecutive `others` segments (the two Whisper passes segment the same speech differently).

- [ ] **Step 1: Write the failing tests** (append to `merge.rs` `mod tests`)

```rust
    use crate::audio::align::EchoLeak;

    fn seg_sp(start_s: f64, end_s: f64, text: &str, speaker: &str) -> Segment {
        Segment { idx: 0, start_s, end_s, text: text.into(), speaker: Some(speaker.into()) }
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
        let (out, n) = suppress_cross_stream_echo(segs, None);
        assert_eq!(n, 1);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].speaker.as_deref(), Some("others"), "the clean copy survives");
        assert_eq!(out[0].idx, 0, "re-indexed");
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
        // Jaccard here is 7/8 ≥ 0.85 → strict already catches it; force a relaxed-only
        // pair (Jaccard < 0.85, coverage ≥ 0.7) to prove the gate:
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
        let (out, n) = suppress_cross_stream_echo(segs, None);
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
        let (out, n) = suppress_cross_stream_echo(segs, None);
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd src-tauri && cargo test --lib audio::merge`
Expected: compile FAIL — `suppress_cross_stream_echo` not defined. (If `Segment` lacks `Clone` for the test, derive check: `transcribe/types.rs` — `Segment` already derives `Clone`; if not, add it.)

- [ ] **Step 3: Write the implementation** (in `merge.rs`, after `merge_streams`)

```rust
/// ── Cross-stream echo suppression (speakers → mic bleed) ────────────────────────────────
///
/// On speakers, the remote voice is captured twice: clean on the system stream and, tens of
/// ms to ~1 s later, as acoustic bleed on the mic — so the same sentence lands as `others`
/// AND `me`. This pass drops the `me` echo copies. Thresholds follow prod-proven values
/// (screenpipe PR #4440 strict tier; OpenWhispr relaxed tier) — see
/// docs/research/2026-07-02-audio-echo-full-remediation.md.
///
/// CONTENT-LOSS GUARDS (load-bearing):
///   - only `me` segments are ever dropped; the clean `others*` copy ALWAYS survives;
///   - both sides need ≥ 4 tokens (short real acks — "okay", "yes ship it" — are immune);
///   - the relaxed tier fires ONLY under measured acoustic leak evidence (headphones ⇒
///     no evidence ⇒ strict tier only);
///   - identical text outside the echo window is genuine repetition and survives.
const ECHO_MIN_TOKENS: usize = 4;
const ECHO_STRICT_WINDOW_S: f64 = 2.0;
const ECHO_STRICT_JACCARD: f32 = 0.85;
const ECHO_RELAXED_BEFORE_S: f64 = 1.5;
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
    if union == 0 { 0.0 } else { inter as f32 / union as f32 }
}

/// Multiset token coverage normalized by the SHORTER side (garbled echo fragments still hit).
fn coverage(a: &[String], b: &[String]) -> f32 {
    use std::collections::HashMap;
    let shorter = a.len().min(b.len());
    if shorter == 0 {
        return 0.0;
    }
    let mut counts: HashMap<&String, usize> = HashMap::new();
    for t in b {
        *counts.entry(t).or_default() += 1;
    }
    let mut hits = 0usize;
    for t in a {
        if let Some(c) = counts.get_mut(t) {
            if *c > 0 {
                *c -= 1;
                hits += 1;
            }
        }
    }
    hits as f32 / shorter as f32
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
    let relaxed_armed = leak
        .map(|l| l.correlation >= crate::audio::align::MIN_CORR)
        .unwrap_or(false);
    let others_refs: Vec<&Segment> = segments.iter().filter(|s| is_others(s)).collect();
    if others_refs.is_empty() {
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
            let delta = seg.start_s - cand.start_s;
            let strict_hit = delta.abs() <= ECHO_STRICT_WINDOW_S
                && jaccard(&me_tokens, &cand.tokens) >= ECHO_STRICT_JACCARD;
            let relaxed_hit = relaxed_armed
                && delta >= -ECHO_RELAXED_BEFORE_S
                && delta <= ECHO_RELAXED_AFTER_S
                && (me_norm == cand.text_norm
                    || cand.text_norm.contains(&me_norm)
                    || me_norm.contains(&cand.text_norm)
                    || coverage(&me_tokens, &cand.tokens).max(token_lcs(&me_tokens, &cand.tokens))
                        >= ECHO_RELAXED_SIMILARITY);
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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd src-tauri && cargo test --lib audio::merge`
Expected: all existing merge tests + the 8 new tests PASS. If `relaxed_tier_is_gated_on_leak_evidence` fails on the relaxed-only pair, hand-compute coverage: me = 6 tokens {finalize,the,annual,budget,proposal,monday}, cand = 10 tokens — intersection 6/6 = 1.0 ≥ 0.7 → hit; Jaccard 6/10 = 0.6 < 0.85 → strict correctly misses.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/audio/merge.rs
git commit -m "feat(transcribe): cross-stream echo dedup — leak-gated two-tier suppression"
```

---

### Task 3: `mix_aligned` + peak-normalize in `audio/mixer.rs`

**Files:**
- Modify: `src-tauri/src/audio/mixer.rs`
- Modify: `src-tauri/src/audio/mod.rs` (re-export `mix_aligned` exactly like the existing `mix` re-export)

**Interfaces:**
- Produces: `pub fn mix_aligned(a: &[f32], a_delay: usize, b: &[f32], b_delay: usize) -> Vec<f32>` — front-pads each track with silence by its delay, sums, then peak-normalizes the WHOLE buffer only if |peak| > 1.0 (no clipping, no hard clamp).
- `pub fn mix(a, b)` keeps its exact signature (used by `examples/e2e_core.rs:42`) and becomes `mix_aligned(a, 0, b, 0)`.

- [ ] **Step 1: Write the failing tests** (replace `clamps_to_unit_range` in `mixer.rs` — this is the intended RED: the hard-clamp contract is being replaced)

```rust
    /// Peaks above 1.0 are handled by scaling the WHOLE buffer (no clipping distortion):
    /// 0.75 + 0.5 = 1.25 → scaled by 1/1.25 → [1.0, ...], ratios preserved.
    #[test]
    fn normalizes_peaks_above_unit_instead_of_clamping() {
        let out = mix(&[0.75, 0.4], &[0.5, 0.1]);
        assert!((out[0] - 1.0).abs() < 1e-6);
        assert!((out[1] - 0.5 / 1.25).abs() < 1e-6, "ratios preserved, not clamped");
    }

    /// mix_aligned pads the delayed track with leading silence.
    #[test]
    fn mix_aligned_pads_the_delayed_track() {
        // b delayed by 2 samples: out = a + [0,0,b...]
        let out = mix_aligned(&[0.1, 0.2, 0.3, 0.4], 0, &[0.5, 0.5], 2);
        assert_eq!(out, vec![0.1, 0.2, 0.8, 0.9]);
        // a delayed instead:
        let out2 = mix_aligned(&[0.5, 0.5], 2, &[0.1, 0.2, 0.3, 0.4], 0);
        assert_eq!(out2, vec![0.1, 0.2, 0.8, 0.9]);
        // zero delays == plain mix
        assert_eq!(mix_aligned(&[0.5], 0, &[0.25], 0), vec![0.75]);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd src-tauri && cargo test --lib audio::mixer`
Expected: compile FAIL (`mix_aligned` undefined) and `normalizes_peaks_above_unit_instead_of_clamping` would fail against the clamp.

- [ ] **Step 3: Write the implementation** (replace the body of `mixer.rs`, keeping the module doc)

```rust
/// Sum two mono f32 tracks, each front-padded with `*_delay` samples of silence (wall-clock /
/// measured-offset alignment), then peak-normalize the whole buffer if the sum exceeds ±1.0.
/// Normalizing (instead of the old hard clamp) avoids clipping distortion exactly at the
/// moments both parties speak.
pub fn mix_aligned(a: &[f32], a_delay: usize, b: &[f32], b_delay: usize) -> Vec<f32> {
    let n = (a.len() + a_delay).max(b.len() + b_delay);
    let mut out = vec![0.0f32; n];
    for (i, s) in a.iter().enumerate() {
        out[i + a_delay] += s;
    }
    for (i, s) in b.iter().enumerate() {
        out[i + b_delay] += s;
    }
    let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    if peak > 1.0 {
        let scale = 1.0 / peak;
        for s in &mut out {
            *s *= scale;
        }
    }
    out
}

/// Sum two mono tracks with no alignment (legacy shape — `examples/e2e_core.rs` calls this).
pub fn mix(a: &[f32], b: &[f32]) -> Vec<f32> {
    mix_aligned(a, 0, b, 0)
}
```

In `audio/mod.rs`, next to the existing `mix` re-export add: `pub use mixer::mix_aligned;`

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd src-tauri && cargo test --lib audio::mixer`
Expected: `sums_equal_length`, `pads_shorter_with_silence`, the 2 new tests PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/audio/mixer.rs src-tauri/src/audio/mod.rs
git commit -m "feat(audio): offset-aligned archive mix with peak normalization (no clamp)"
```

---

### Task 4: Wire leak estimation, aligned mix, and dedup into `pipeline.rs` + typed echo event

**Files:**
- Modify: `src-tauri/src/pipeline.rs` (three places: leak probe before the archive mix at `pipeline.rs:278-287`; the spawn_blocking closure return at `pipeline.rs:379-443`; an event emit after `insert_segments` at `pipeline.rs:449`)
- Modify: `src-tauri/src/events.rs` (new typed event)

**Interfaces:**
- Consumes: `audio::align::{estimate_stream_offset, archive_delays, EchoLeak}` (Task 1), `audio::merge::suppress_cross_stream_echo` (Task 2), `audio::mix_aligned` (Task 3).
- Produces: `pub const EVENT_ECHO_SUPPRESSED: &str = "murmur://echo-suppressed";` + `pub struct EchoSuppressedPayload { pub suppressed: usize, pub meeting_id: String }` in `events.rs` (Serialize, camelCase — counts only, NO PII). Task 8 (FE) listens to it.

- [ ] **Step 1: Add the typed event** (append to `events.rs`, following the `ReindexPayload` pattern)

```rust
/// Emitted once after transcription when the cross-stream echo dedup removed ≥1 mic-echo
/// segment (the user recorded on speakers). Counts only — NO PII. The FE shows a toast
/// recommending headphones.
pub const EVENT_ECHO_SUPPRESSED: &str = "murmur://echo-suppressed";

/// Payload for [`EVENT_ECHO_SUPPRESSED`]. Counts only — NO PII.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EchoSuppressedPayload {
    /// Number of mic-echo segments removed from the transcript.
    pub suppressed: usize,
    pub meeting_id: String,
}
```

- [ ] **Step 2: Probe the leak + align the mix.** In `run_inner`, REPLACE the current archive block (`pipeline.rs:278-287`):

```rust
    // Archive WAV = the MIX (for playback only). Mic-only when there's no system stream.
    // Archive = the RAW (cpal) mic mixed with system audio — never the AEC'd ASR feed.
    let archive_src = mic_16k_archive.as_ref().unwrap_or(&mic_16k);
    let archive_16k = match &sys_16k {
        Some(sys) => {
            tracing::info!(target: "audio", "archiving mixed mic + system-audio track");
            audio::mix(archive_src, sys)
        }
        None => archive_src.clone(),
    };
```

with:

```rust
    // Archive WAV = the MIX (for playback only). Mic-only when there's no system stream.
    // Archive = the RAW (cpal) mic mixed with system audio — never the AEC'd ASR feed.
    let archive_src = mic_16k_archive.as_ref().unwrap_or(&mic_16k);
    // Measure the mic↔system offset + speaker-leak strength ONCE, on the RAW mic (never an
    // AEC'd feed — the leak only exists in the raw capture). Drives BOTH the aligned archive
    // mix below and the echo dedup after transcription. Best-effort: None ⇒ wall-clock pads
    // and strict-tier-only dedup (today's behaviour, minus the echo).
    let leak: Option<audio::align::EchoLeak> = sys_16k
        .as_ref()
        .and_then(|sys| audio::align::estimate_stream_offset(archive_src, sys));
    if let Some(l) = &leak {
        tracing::info!(
            target: "audio",
            offset_s = l.offset_s,
            correlation = l.correlation,
            "mic/system offset measured (speaker-leak evidence)"
        );
    }
    let archive_16k = match &sys_16k {
        Some(sys) => {
            let (mic_delay, sys_delay) = audio::align::archive_delays(
                leak.as_ref(),
                mic_started_at,
                system_started_at,
                audio::TARGET_RATE_HZ,
            );
            tracing::info!(
                target: "audio",
                mic_delay,
                sys_delay,
                "archiving mixed mic + system-audio track (offset-aligned)"
            );
            audio::mix_aligned(archive_src, mic_delay, sys, sys_delay)
        }
        None => archive_src.clone(),
    };
```

- [ ] **Step 3: Dedup after the merge.** In the `spawn_blocking` closure (`pipeline.rs:379-443`): change the closure's return type from `Result<Vec<crate::transcribe::types::Segment>>` to `Result<(Vec<crate::transcribe::types::Segment>, usize)>`, and replace the final line of the closure (`pipeline.rs:438-440`):

```rust
        // Anchor each stream's segments to its capture-start host instant → absolute timeline,
        // merge sorted by absolute start, drop empty (e.g. muted-mic) segments, label "me"/"others".
        Ok(merge_streams(streams))
```

with:

```rust
        // Anchor each stream's segments to its capture-start host instant → absolute timeline,
        // merge sorted by absolute start, drop empty (e.g. muted-mic) segments, label "me"/"others" —
        // then drop mic-echo copies of others' speech (speakers → mic bleed; leak-gated).
        let (merged, echo_suppressed) =
            crate::audio::merge::suppress_cross_stream_echo(merge_streams(streams), leak.as_ref());
        Ok((merged, echo_suppressed))
```

The `leak` value is `Copy` — bind it before the closure so it moves in cleanly:
`let leak_for_dedup = leak;` and use `leak_for_dedup.as_ref()` inside. Update the receiving binding (`pipeline.rs:379`):

```rust
    let (merged_segments, echo_suppressed) = tokio::task::spawn_blocking(move || -> Result<(Vec<crate::transcribe::types::Segment>, usize)> {
```

- [ ] **Step 4: Emit the event.** Right after `state.db.insert_segments(meeting_id, &merged_segments)?;` (`pipeline.rs:449`) add:

```rust
    if echo_suppressed > 0 {
        tracing::info!(target: "transcribe", suppressed = echo_suppressed, "cross-stream echo segments removed");
        let _ = app.emit(
            crate::events::EVENT_ECHO_SUPPRESSED,
            crate::events::EchoSuppressedPayload {
                suppressed: echo_suppressed,
                meeting_id: meeting_id.to_string(),
            },
        );
    }
```

- [ ] **Step 5: Run the full unit suite**

Run: `cd src-tauri && cargo test --lib`
Expected: all ~520+ tests PASS (merge/mixer/align/pipeline guards included). Fix borrow-order issues if `archive_src` conflicts with the later `mem::replace` — the probe uses only `&` reads before the mix, which happens before `normalize_for_asr` mutates `mic_16k`.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/pipeline.rs src-tauri/src/events.rs
git commit -m "feat(pipeline): leak-probed aligned archive mix + echo dedup + echo-suppressed event"
```

---

### Task 5: First-frame capture anchors from the system helpers

**Files:**
- Modify: `src-tauri/sysaudio/sysaudio.swift:100-107` (first audio buffer site)
- Modify: `src-tauri/audiocap/audiocap.swift:206-213` (first audio buffer site)
- Modify: `src-tauri/src/audio/system.rs` (stderr reader thread + anchor preference)

**Interfaces:**
- Protocol addition (backward/forward safe — helpers are bundled with the app, versions always match): each helper writes ONE stderr line containing `first-frame` when its first audio buffer arrives. Rust notes `Instant::now()` on receipt (never ship an `Instant` across processes — it is per-process monotonic; pipe latency is µs–ms vs the 100–500 ms spawn-lag being fixed).
- Produces: `SystemAudioRecorder::started_at()` now prefers the first-frame instant, falling back to the spawn instant (today's behaviour). `pub(crate) fn is_first_frame_line(line: &str) -> bool` (pure, tested).

- [ ] **Step 1: Write the failing Rust test** (new `mod tests` at the bottom of `system.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::is_first_frame_line;

    #[test]
    fn first_frame_line_is_recognized() {
        assert!(is_first_frame_line("sysaudio: first-frame"));
        assert!(is_first_frame_line("audiocap: first-frame\n".trim()));
        assert!(!is_first_frame_line("sysaudio: capturing"));
        assert!(!is_first_frame_line("audiocap: tap stuck silent — rebuilding (1)"));
        assert!(!is_first_frame_line(""));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd src-tauri && cargo test --lib audio::system`
Expected: compile FAIL — `is_first_frame_line` not defined.

- [ ] **Step 3: Swift — emit the line once.** In `sysaudio.swift`, inside the sample handler where the file is first created (`sysaudio.swift:100-107`), change:

```swift
            lock.lock()
            defer { lock.unlock() }
            if file == nil {
                file = try? AVAudioFile(
                    forWriting: outURL, settings: format.settings,
                    commonFormat: .pcmFormatFloat32, interleaved: false)
            }
            try? file?.write(from: pcm)
```

to:

```swift
            lock.lock()
            defer { lock.unlock() }
            if file == nil {
                // Anchor line for the Rust wall-clock merge: the true capture start (vs the
                // process-spawn instant, which precedes SCK setup by hundreds of ms).
                FileHandle.standardError.write(Data("sysaudio: first-frame\n".utf8))
                file = try? AVAudioFile(
                    forWriting: outURL, settings: format.settings,
                    commonFormat: .pcmFormatFloat32, interleaved: false)
            }
            try? file?.write(from: pcm)
```

In `audiocap.swift` `handle()` (`audiocap.swift:206-213`), the same pattern:

```swift
        lock.lock()
        defer { lock.unlock() }
        if file == nil {
            // Anchor line for the Rust wall-clock merge (true capture start).
            FileHandle.standardError.write(Data("audiocap: first-frame\n".utf8))
            file = try? AVAudioFile(
                forWriting: outURL, settings: format.settings,
                commonFormat: .pcmFormatFloat32, interleaved: format.isInterleaved)
        }
        try? file?.write(from: pcm)
```

NOTE: `audiocap`'s watchdog `rebuild()` keeps the same open file, so the line fires once per file — but a rebuild after a stuck-silent teardown re-enters with `file != nil` → no duplicate line. Good.

- [ ] **Step 4: Rust — read stderr, prefer the anchor.** In `system.rs`:

```rust
use std::io::BufRead;
use std::sync::{Arc, OnceLock};

/// One stderr line from a system-capture helper announcing its FIRST audio buffer.
pub(crate) fn is_first_frame_line(line: &str) -> bool {
    line.trim_end().ends_with("first-frame")
}
```

Extend the struct:

```rust
pub struct SystemAudioRecorder {
    child: Child,
    wav_path: PathBuf,
    started_at: std::time::Instant,
    /// True capture-start anchor: `Instant::now()` taken when the helper's `first-frame`
    /// stderr line is received (pipe latency ≪ the spawn→capture setup lag it replaces).
    first_frame_at: Arc<OnceLock<std::time::Instant>>,
    stderr_reader: Option<std::thread::JoinHandle<()>>,
}
```

In `start()`, after `let child = cmd.spawn()...` and before `Ok(Self { ... })`:

```rust
        let mut child = child;
        let first_frame_at: Arc<OnceLock<std::time::Instant>> = Arc::new(OnceLock::new());
        // Drain stderr on a dedicated thread: (a) capture the first-frame anchor, (b) prevent
        // the 64 KB pipe buffer from ever blocking the helper (stderr was piped-but-unread).
        let stderr_reader = child.stderr.take().map(|stderr| {
            let anchor = first_frame_at.clone();
            std::thread::spawn(move || {
                let reader = std::io::BufReader::new(stderr);
                for line in reader.lines() {
                    let Ok(line) = line else { break };
                    if is_first_frame_line(&line) {
                        let _ = anchor.set(std::time::Instant::now());
                    }
                    // Never log helper lines verbatim beyond known markers (no-PII rule).
                }
            })
        });
        Ok(Self {
            child,
            wav_path,
            started_at: std::time::Instant::now(),
            first_frame_at,
            stderr_reader,
        })
```

Replace `started_at()`:

```rust
    /// Host wall-clock instant when this system-audio stream started CAPTURING (for the merge).
    /// Prefers the helper's first-frame anchor; falls back to the spawn instant (a helper that
    /// died before capturing, or an old helper without the line).
    pub fn started_at(&self) -> std::time::Instant {
        self.first_frame_at.get().copied().unwrap_or(self.started_at)
    }
```

In `stop()`, after `self.child.wait()` add:

```rust
        if let Some(handle) = self.stderr_reader.take() {
            let _ = handle.join();
        }
```

(make `stop(mut self)` keep working — `stderr_reader.take()` needs `mut self`, which it already is).

- [ ] **Step 5: Run tests + compile the helpers**

Run: `cd src-tauri && cargo test --lib`
Expected: PASS (build.rs recompiles both Swift helpers — a `swiftc` error here means a typo in the Swift edits).

- [ ] **Step 6: Commit**

```bash
git add src-tauri/sysaudio/sysaudio.swift src-tauri/audiocap/audiocap.swift src-tauri/src/audio/system.rs
git commit -m "feat(audio): first-frame capture anchors from system helpers (spawn-lag fix)"
```

---

### Task 6: VPIO helper containment (`aeccap`): kill the system-audio ducking + AGC

**Files:**
- Modify: `src-tauri/aeccap/aeccap.swift:41-73` (`start()`)
- Modify: `src/app/features/settings/sections/settings-audio-section.component.ts:94-107` (toggle copy)

**Interfaces:** none new. Behavior: with `aec_enabled` ON, VPIO no longer ducks other apps' audio (the −51 dB system-track kill + quieting the user's live call — research F1) and no longer applies AGC to the ASR feed. Default stays OFF; the helper remains opt-in until the Task-9 offline AEC replaces it (removal is a follow-up decision after the real-Mac ERLE spike in Task 12).

- [ ] **Step 1: Patch `aeccap.swift`.** In `start()`, directly after `try input.setVoiceProcessingEnabled(true)` (`aeccap.swift:46`) add:

```swift
        // CONTAINMENT (see docs/research/2026-07-02-audio-echo-full-remediation.md):
        // 1) By default VPIO DUCKS all other apps' audio system-wide — it can quiet the very
        //    call being recorded and was observed killing a system-audio capture to ~-51 dB.
        //    macOS 14+ exposes the knob; pin it to minimum.
        // 2) Uplink AGC pumps the ASR feed's levels; disable for a level-faithful feed.
        if #available(macOS 14.0, *) {
            input.voiceProcessingOtherAudioDuckingConfiguration =
                AVAudioVoiceProcessingOtherAudioDuckingConfiguration(
                    enableAdvancedDucking: false, duckingLevel: .min)
            input.isVoiceProcessingAGCEnabled = false
        }
```

(If `swiftc` rejects the initializer shape, the struct also supports member-wise assignment: `var duck = AVAudioVoiceProcessingOtherAudioDuckingConfiguration(); duck.enableAdvancedDucking = false; duck.duckingLevel = .min; input.voiceProcessingOtherAudioDuckingConfiguration = duck`.)

- [ ] **Step 2: Verify the helper compiles**

Run: `cd src-tauri && cargo test --lib audio::aec`
Expected: PASS (build.rs runs swiftc over aeccap.swift).

- [ ] **Step 3: Honest Settings copy.** In `settings-audio-section.component.ts:94-107` replace the toggle sub-copy:

```html
                    <span class="text-secondary toggle-sub">
                      When recording without headphones, apply system echo cancellation to
                      the microphone used for transcription. Experimental — headphones are
                      still the most reliable fix.
                    </span>
```

with:

```html
                    <span class="text-secondary toggle-sub">
                      Experimental Apple voice processing on the transcription mic. May not
                      remove echo on all setups (macOS gives it no reference signal) — echoed
                      lines are also removed automatically after each recording. Headphones
                      remain the most reliable fix.
                    </span>
```

- [ ] **Step 4: FE gates**

Run: `npx ng lint && npx ng build`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/aeccap/aeccap.swift src/app/features/settings/sections/settings-audio-section.component.ts
git commit -m "fix(audio): stop VPIO ducking other apps' audio + disable AGC; honest AEC copy"
```

---

### Task 7: FE — echo-suppressed toast + headphones hint during recording

**Files:**
- Modify: `src/app/core/models.ts` (payload type)
- Modify: `src/app/core/ipc.service.ts` (event const + listener, next to `onStatus` at `ipc.service.ts:858`)
- Modify: `src/app/core/recorder.store.ts` (subscribe in `init()` at `recorder.store.ts:96-112`)
- Modify: `src/app/features/record/record.component.ts:75` (banner visibility)

**Interfaces:**
- Consumes: `EVENT_ECHO_SUPPRESSED` + `EchoSuppressedPayload` (Task 4), `ToastService.info(message, ttlMs?)` (`toast.service.ts:88`).
- Produces: `IpcService.onEchoSuppressed(cb): Promise<UnlistenFn>`; toast on suppression; the headphones banner now also shows WHILE recording.

- [ ] **Step 1: models.ts** — add next to `StatusPayload`:

```ts
/** Payload of murmur://echo-suppressed — counts only, no content. */
export interface EchoSuppressedPayload {
  suppressed: number;
  meetingId: string;
}
```

- [ ] **Step 2: ipc.service.ts** — next to the other event consts add `const EVENT_ECHO_SUPPRESSED = "murmur://echo-suppressed";` and next to `onStatus` (`ipc.service.ts:858`) add:

```ts
  /** Cross-stream echo dedup removed segments after a recording (user was on speakers). */
  onEchoSuppressed(
    cb: (payload: EchoSuppressedPayload) => void,
  ): Promise<UnlistenFn> {
    return listen<EchoSuppressedPayload>(EVENT_ECHO_SUPPRESSED, (event) =>
      cb(event.payload),
    );
  }
```

- [ ] **Step 3: recorder.store.ts** — inject the toast service (`private readonly toast = inject(ToastService);` — match the store's existing `inject()` style) and in `init()` (after the `onLiveCaption` subscription, `recorder.store.ts:108-110`):

```ts
    // Echo cleanup notice: recording was made on speakers; echoed lines were removed.
    this.unlistenEcho = await this.ipc.onEchoSuppressed((p) => {
      this.toast.info(
        `Removed ${p.suppressed} echoed line${p.suppressed === 1 ? "" : "s"} from the transcript — wear headphones for best results 🎧`,
      );
    });
```

with the matching field `private unlistenEcho: UnlistenFn | null = null;` (next to `unlistenLive`, `recorder.store.ts:94`) and release it wherever the other `UnlistenFn`s are released.

- [ ] **Step 4: record.component.ts** — the hint is MOST needed while recording; change line 75 from:

```html
      @if (headphonesHint() && !store.isRecording()) {
```

to:

```html
      @if (headphonesHint()) {
```

- [ ] **Step 5: FE gates**

Run: `npx ng lint && npx ng build`
Expected: clean (watch the 16 kB per-component style budget — no style changes here).

- [ ] **Step 6: Commit**

```bash
git add src/app/core/models.ts src/app/core/ipc.service.ts src/app/core/recorder.store.ts src/app/features/record/record.component.ts
git commit -m "feat(record): echo-suppressed toast + headphones hint visible during recording"
```

---

### Task 8: PR-1 — gates, adversarial verify, pull request

**Files:** none (verification + git).

- [ ] **Step 1: Full gate, once**

Run: `bash scripts/ci.sh`
Expected: clippy `-D warnings` + `cargo test --lib` + `ng lint` + `ng build` + headless E2E (incl. `scripts/e2e-mix.sh`) all green. `e2e-mix` exercises `audio::mix` — signature unchanged, transcript assertions unaffected by peak-normalize.

- [ ] **Step 2: Dispatch the adversarial-verifier subagent** with this charter: verify (a) RED-before-GREEN of the dedup tests against pre-change `merge.rs` (revert-check), (b) content-loss hunt: craft transcripts where genuine speech could be eaten (short acks, repeated phrases, Polish diacritics, `others-N` labels, mic-only), (c) the mix-alignment sign convention (positive offset pads the SYSTEM track), (d) no PII in any new tracing call, (e) dev app boots clean (`tauri-dev` skill, watch the boot log — the stderr reader thread must not break helper spawn), (f) live Chromium smoke of the toast + banner with a mocked `window.__TAURI_INTERNALS__.invoke`. The implementer does NOT self-certify.

- [ ] **Step 3: PR**

```bash
git push -u origin feat/audio-echo-remediation
gh pr create -R murmur-io/murmur --title "feat(audio): echo remediation — leak-gated transcript dedup + aligned archive mix + first-frame anchors + VPIO containment" --body "<summary + honest real-Mac caveats from Task 12>"
```

Honesty note in the PR body: end-to-end "double-hearing gone on speakers" and the true first-frame latency win are **real-Mac-only** verifications (Task 12); the headless suite proves the algorithms, not the acoustics.

---

### Task 9: Offline AEC3 (`sonora`) — echo ENERGY removal (⚠️ APPROVAL GATE)

**⚠️ STOP: adding the `sonora` crate requires explicit user approval (repo rule: no new crates). Ask before executing this task. If declined, skip Tasks 9 and re-scope Task 12's bake-off.**

**Files:**
- Modify: `src-tauri/Cargo.toml` (add `sonora = "0.1"`)
- Create: `src-tauri/src/audio/aec_offline.rs`
- Modify: `src-tauri/src/audio/mod.rs` (`pub mod aec_offline;`)
- Modify: `src-tauri/src/settings/config.rs` (new flag `post_aec_enabled`, default `true` — mirror `aec_enabled` at `config.rs:96-98/287/339/428-429/571-572`)
- Modify: `src-tauri/src/commands.rs:3083-3104` + `3145-3166` (DTO both directions)
- Modify: `src-tauri/src/pipeline.rs` (apply after the leak probe, before `normalize_for_asr` and the mix)
- Modify: `src/app/core/models.ts`, `src/app/features/settings/settings.store.ts:86/519/752`, `src/app/features/settings/sections/settings-audio-section.component.ts` (FE toggle "Remove speaker echo from recordings (on-device)")

**Interfaces:**
- Produces: `pub fn cancel_echo_offline(mic_16k: &[f32], sys_16k: &[f32], sys_lead_samples: usize) -> crate::error::Result<Vec<f32>>` — WebRTC-AEC3 over 10 ms/160-sample frames: for each frame, feed the (already offset-aligned) system frame as the RENDER (far-end) stream, then process the mic CAPTURE frame; returns the echo-cancelled mic, exactly `mic_16k.len()` samples. On ANY error → the caller keeps the raw mic (best-effort, like every audio enhancement in this pipeline).
- Pipeline placement: processed mic replaces the ASR feed AND the archive-mix mic input; the raw mic still goes to the `keep_hires_masters` `.mic.wav`. The system track is untouched (it is the clean signal). The `$TMPDIR`-scratch/lock-model condition is unaffected — no new files.

- [ ] **Step 1 (SPIKE): confirm the `sonora` API.** Fetch https://github.com/dignifiedquire/sonora README + docs.rs; the canonical WebRTC-APM shape is `Processor::new(config)` + `process_render_frame(&mut [f32])` + `process_capture_frame(&mut [f32])` at 10 ms frames. **Adapt the exact type/method names in Steps 2–3 to the real crate API** — the wrapper signature and tests below are OURS and fixed. If the crate cannot do 16 kHz mono 10 ms frames or fails the Step-4 test, STOP and report — fallback candidate is DTLN-aec over the already-bundled onnxruntime (separate plan).

- [ ] **Step 2: Write the failing test** (in `aec_offline.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn burst_signal(len: usize, seed: u64) -> Vec<f32> {
        let mut x = seed;
        let mut out = vec![0.0f32; len];
        let mut i = 0;
        while i < len {
            for j in 0..12_800.min(len - i) {
                x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                out[i + j] = ((x >> 33) as f32 / u32::MAX as f32 - 0.5) * 0.6;
            }
            i += 12_800 + 11_200;
        }
        out
    }

    fn energy_db(sig: &[f32]) -> f32 {
        let e = sig.iter().map(|s| s * s).sum::<f32>() / sig.len().max(1) as f32;
        10.0 * (e + 1e-12).log10()
    }

    /// mic = pure echo (0.3 × sys delayed 40 ms). After AEC the residual energy in the
    /// echo-only span must drop ≥ 15 dB vs the raw mic. RED on a passthrough implementation.
    #[test]
    fn cancels_synthetic_echo_by_at_least_15_db() {
        let sys = burst_signal(16_000 * 30, 7);
        let delay = (0.04 * 16_000.0) as usize;
        let mut mic = vec![0.0f32; sys.len()];
        for i in delay..mic.len() {
            mic[i] = 0.3 * sys[i - delay];
        }
        let out = cancel_echo_offline(&mic, &sys, 0).expect("aec runs");
        assert_eq!(out.len(), mic.len(), "length-faithful (timeline invariant)");
        // Skip the first 5 s (filter convergence), measure the tail.
        let tail = 16_000 * 5;
        let reduction = energy_db(&mic[tail..]) - energy_db(&out[tail..]);
        assert!(reduction >= 15.0, "echo reduced only {reduction:.1} dB");
    }

    /// Near-end (local) speech must survive: mic = own speech + echo; own-speech-only spans
    /// keep ≥ 50% of their energy after AEC.
    #[test]
    fn near_end_speech_survives() {
        let sys = burst_signal(16_000 * 30, 7);
        let own = burst_signal(16_000 * 30, 4242);
        let delay = (0.04 * 16_000.0) as usize;
        let mut mic = own.clone();
        for i in delay..mic.len() {
            mic[i] += 0.3 * sys[i - delay];
        }
        let out = cancel_echo_offline(&mic, &sys, 0).expect("aec runs");
        let tail = 16_000 * 5;
        let kept = energy_db(&out[tail..]) - energy_db(&own[tail..]);
        assert!(kept > -3.0, "near-end speech attenuated {kept:.1} dB — too aggressive");
    }
}
```

- [ ] **Step 3: Implement the wrapper** (`aec_offline.rs`; adapt names per the Step-1 spike)

```rust
//! Post-hoc echo cancellation over the RECORDED tracks (WebRTC AEC3 via `sonora`).
//!
//! We hold the perfect far-end reference — the digitally captured system-audio track — so
//! this is the textbook AEC setup, run OFFLINE after Stop: zero effect on the live call
//! (no VPIO ducking, no device contention), deterministic, and headless-testable.
//! Best-effort: any error leaves the recording on the raw mic.

use crate::error::{AppError, Result};

/// 10 ms @ 16 kHz.
const FRAME: usize = 160;

/// Echo-cancel `mic_16k` using `sys_16k` as the far-end reference. `sys_lead_samples` is the
/// measured alignment pad (from `align::EchoLeak.offset_s`, ≥ 0 — how far the system track
/// leads the mic content). Returns exactly `mic_16k.len()` samples.
pub fn cancel_echo_offline(
    mic_16k: &[f32],
    sys_16k: &[f32],
    sys_lead_samples: usize,
) -> Result<Vec<f32>> {
    let mut apm = sonora::Processor::builder()
        .sample_rate_hz(16_000)
        .channels(1)
        .echo_cancellation(true)
        .build()
        .map_err(|e| AppError::Audio(format!("AEC init: {e}")))?;

    let mut out = Vec::with_capacity(mic_16k.len());
    let mut render = [0.0f32; FRAME];
    let mut capture = [0.0f32; FRAME];
    let mut i = 0usize;
    while i < mic_16k.len() {
        let n = FRAME.min(mic_16k.len() - i);
        // Render (far-end) frame: the system content the speakers were playing at mic-time i —
        // i.e. sys index (i - sys_lead) … zero-padded outside the track.
        for k in 0..FRAME {
            let mic_idx = i + k;
            render[k] = mic_idx
                .checked_sub(sys_lead_samples)
                .and_then(|j| sys_16k.get(j))
                .copied()
                .unwrap_or(0.0);
        }
        capture[..n].copy_from_slice(&mic_16k[i..i + n]);
        capture[n..].fill(0.0);
        apm.process_render_frame(&mut render)
            .map_err(|e| AppError::Audio(format!("AEC render: {e}")))?;
        apm.process_capture_frame(&mut capture)
            .map_err(|e| AppError::Audio(format!("AEC capture: {e}")))?;
        out.extend_from_slice(&capture[..n]);
    }
    Ok(out)
}
```

- [ ] **Step 4: Run tests**

Run: `cd src-tauri && cargo test --lib audio::aec_offline`
Expected: both PASS. If reduction < 15 dB, check the render/capture call ORDER (render must precede its matched capture frame) and the sign of `sys_lead_samples`.

- [ ] **Step 5: Config flag + DTO + FE toggle.** Mirror `aec_enabled` exactly: `config.rs` — doc line, `post_aec_enabled: true` in `Default` (`config.rs:283-287` block), `const K_POST_AEC_ENABLED: &str = "post_aec_enabled";` (`:339` block), load arm (`:417-429` block), save arm (`:556-572` block); update the JSON-fixture tests (`config.rs:742/805/1033/1075/1121`) — they go RED first (expected) then GREEN with `"postAecEnabled":true`. `commands.rs:3083-3104` + `3145-3166`: add `post_aec_enabled` both ways. FE: `models.ts` `postAecEnabled: boolean;`, `settings.store.ts:86/519/752` mirror `aecEnabled`, and a new toggle card in `settings-audio-section.component.ts` (above the VPIO one):

```html
              <!-- On-device echo removal (post-processing) — toggle row -->
              <div class="card">
                <label class="toggle-row">
                  <span class="toggle-copy">
                    <span class="toggle-title">Remove speaker echo from recordings</span>
                    <span class="text-secondary toggle-sub">
                      After each recording, cancel the other participants' voices out of your
                      microphone track using the captured system audio — fixes the doubled
                      voice when recording on speakers. Runs fully on-device.
                    </span>
                  </span>
                  <input type="checkbox" formControlName="postAecEnabled" />
                </label>
              </div>
```

- [ ] **Step 6: Pipeline integration.** In `run_inner`, restructure the Task-4 block so the borrows sequence cleanly — the leak probe gets its own temporary binding, the AEC pass mutates, and only THEN is `archive_src` bound for the mix:

```rust
    // Measure the mic↔system offset + speaker-leak strength ONCE, on the RAW mic (never an
    // AEC'd feed — the leak only exists in the raw capture). Immutable borrow ends here.
    let leak: Option<audio::align::EchoLeak> = {
        let raw_probe: &[f32] = mic_16k_archive.as_ref().unwrap_or(&mic_16k);
        sys_16k
            .as_ref()
            .and_then(|sys| audio::align::estimate_stream_offset(raw_probe, sys))
    };

    // Post-hoc AEC (on-device, offline): cancel the system-audio reference out of the mic.
    // Only when there IS a system stream, the flag is on, and a leak was measured (headphones
    // ⇒ no echo energy ⇒ skip the pass entirely). Best-effort: any error keeps the raw mic.
    if config.post_aec_enabled {
        if let (Some(sys), Some(l)) = (sys_16k.as_ref(), leak.as_ref()) {
            let sys_lead = (l.offset_s.max(0.0) * audio::TARGET_RATE_HZ as f64).round() as usize;
            match audio::aec_offline::cancel_echo_offline(&mic_16k, sys, sys_lead) {
                Ok(clean) if clean.len() == mic_16k.len() => {
                    // The AEC'd mic becomes BOTH the ASR feed and the archive-mix input: feed
                    // and archive stay the SAME buffer, so the timeline-desync class (51 s
                    // recording / 8 s feed) is impossible by construction. The raw mic is
                    // still archived faithfully via keep_hires_masters (`.mic.wav` is written
                    // from the pre-resample `samples`, pipeline.rs:304 — untouched here).
                    mic_16k = clean;
                    mic_16k_archive = None; // archive follows the AEC'd feed
                    tracing::info!(target: "audio", "offline AEC applied to the mic track");
                }
                Ok(_) => tracing::warn!(target: "audio", "offline AEC length mismatch; raw mic kept"),
                Err(e) => tracing::warn!(target: "audio", error = %e, "offline AEC failed; raw mic kept"),
            }
        }
    }

    // Only NOW bind the archive source (after any AEC swap): VPIO path unchanged when the
    // offline pass didn't run; AEC'd feed otherwise.
    let archive_src = mic_16k_archive.as_ref().unwrap_or(&mic_16k);
```

The Task-4 `archive_16k` mix block stays as written (it consumes `archive_src` + `leak`).

- [ ] **Step 7: Run everything**

Run: `cd src-tauri && cargo test --lib && npx ng lint && npx ng build`
Expected: green (config fixture tests updated in Step 5).

- [ ] **Step 8: Commit**

```bash
git add src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/src/audio/aec_offline.rs src-tauri/src/audio/mod.rs src-tauri/src/settings/config.rs src-tauri/src/commands.rs src-tauri/src/pipeline.rs src/app/core/models.ts src/app/features/settings/settings.store.ts src/app/features/settings/sections/settings-audio-section.component.ts
git commit -m "feat(audio): offline AEC3 echo removal — system track as far-end reference"
```

---

### Task 10: Built-in-speakers detection command (⚠️ APPROVAL GATE for `coreaudio-sys` direct dep)

**⚠️ STOP: declaring `coreaudio-sys` as a direct dependency requires user approval (it is ALREADY compiled transitively via cpal — zero build cost, but the no-new-deps rule applies to Cargo.toml lines). Ask before executing. If declined, fall back to the cpal name-heuristic variant noted in Step 3.**

**Files:**
- Create: `src-tauri/src/audio/output.rs`
- Modify: `src-tauri/src/audio/mod.rs` (`pub mod output;`)
- Modify: `src-tauri/Cargo.toml` (`coreaudio-sys = "0.2"` — version already pinned in Cargo.lock via cpal)
- Modify: `src-tauri/src/commands.rs` (new command) + `src-tauri/src/lib.rs:51` (`generate_handler!` — a command NOT added here is silently un-callable)
- Modify: `src/app/core/ipc.service.ts`, `src/app/features/record/record.component.ts:1036-1039`

**Interfaces:**
- Produces: Rust `pub fn default_output_is_builtin_speakers() -> Option<bool>` (crash-safe: pure Core Audio **C functions** only, per rules §7 — `AudioObjectGetPropertyData`, zero `msg_send`); Tauri command `output_is_builtin_speakers() -> Result<Option<bool>>`; FE `IpcService.outputIsBuiltinSpeakers(): Promise<boolean | null>`.
- `Some(true)` = default output is the built-in speakers (echo risk); `Some(false)` = headphones/external; `None` = could not determine (FE treats as "assume speakers" — hint stays visible, today's behaviour).

- [ ] **Step 1: Write the failing test** (in `output.rs` — the FFI itself is real-Mac-only; test the pure classifier)

```rust
#[cfg(test)]
mod tests {
    use super::classify_output;

    #[test]
    fn classifies_transport_and_data_source() {
        // Built-in transport + internal-speaker data source ⇒ speakers.
        assert_eq!(classify_output(Some(TRANSPORT_BUILTIN), Some(SOURCE_INTERNAL_SPEAKER)), Some(true));
        // Built-in transport + headphone data source (3.5 mm jack) ⇒ not speakers.
        assert_eq!(classify_output(Some(TRANSPORT_BUILTIN), Some(SOURCE_HEADPHONES)), Some(false));
        // Built-in transport, unreadable data source ⇒ conservatively speakers.
        assert_eq!(classify_output(Some(TRANSPORT_BUILTIN), None), Some(true));
        // Bluetooth / USB / anything else ⇒ not the built-in speakers.
        assert_eq!(classify_output(Some(0x626C7565 /* 'blue' */), None), Some(false));
        // Unknown transport ⇒ None.
        assert_eq!(classify_output(None, None), None);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd src-tauri && cargo test --lib audio::output`
Expected: compile FAIL.

- [ ] **Step 3: Implement** (`output.rs`)

```rust
//! Default-output introspection: is the user playing the call through the BUILT-IN SPEAKERS
//! (echo risk) or headphones/external gear? Crash-safe by construction: pure CoreAudio C
//! functions only (rules §7) — they return error codes, they cannot raise an ObjC exception
//! across FFI. Real verification (speakers vs jack vs AirPods) needs a real Mac — headless
//! covers the classifier only.

#[cfg(target_os = "macos")]
use coreaudio_sys::{
    kAudioDevicePropertyDataSource, kAudioDevicePropertyTransportType,
    kAudioHardwarePropertyDefaultOutputDevice, kAudioObjectPropertyElementMain,
    kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyScopeOutput, kAudioObjectSystemObject,
    AudioDeviceID, AudioObjectGetPropertyData, AudioObjectPropertyAddress,
};

/// 'bltn' — kAudioDeviceTransportTypeBuiltIn.
pub(crate) const TRANSPORT_BUILTIN: u32 = 0x626C_746E;
/// 'ispk' — internal speaker data source.
pub(crate) const SOURCE_INTERNAL_SPEAKER: u32 = 0x6973_706B;
/// 'hdpn' — headphones data source (built-in 3.5 mm jack).
pub(crate) const SOURCE_HEADPHONES: u32 = 0x6864_706E;

/// Pure classifier (unit-tested): transport + optional data source → speakers?.
pub(crate) fn classify_output(transport: Option<u32>, data_source: Option<u32>) -> Option<bool> {
    match transport {
        None => None,
        Some(TRANSPORT_BUILTIN) => match data_source {
            Some(SOURCE_HEADPHONES) => Some(false),
            // 'ispk' or unreadable: the built-in output without headphone routing is speakers.
            _ => Some(true),
        },
        Some(_) => Some(false),
    }
}

#[cfg(target_os = "macos")]
fn read_u32(object: u32, selector: u32, scope: u32) -> Option<u32> {
    let address = AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: scope,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut value: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: out-params sized to a u32; the C call returns a status code on failure.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            &address,
            0,
            std::ptr::null(),
            &mut size,
            &mut value as *mut u32 as *mut _,
        )
    };
    (status == 0).then_some(value)
}

/// `Some(true)` = default output is the built-in speakers; `None` = undeterminable.
#[cfg(target_os = "macos")]
pub fn default_output_is_builtin_speakers() -> Option<bool> {
    let device: AudioDeviceID = read_u32(
        kAudioObjectSystemObject,
        kAudioHardwarePropertyDefaultOutputDevice,
        kAudioObjectPropertyScopeGlobal,
    )?;
    let transport = read_u32(device, kAudioDevicePropertyTransportType, kAudioObjectPropertyScopeGlobal);
    let source = read_u32(device, kAudioDevicePropertyDataSource, kAudioObjectPropertyScopeOutput);
    classify_output(transport, source)
}

#[cfg(not(target_os = "macos"))]
pub fn default_output_is_builtin_speakers() -> Option<bool> {
    None
}
```

(If the bundled `coreaudio-sys` predates `kAudioObjectPropertyElementMain`, use `kAudioObjectPropertyElementMaster` — same value. If the user DECLINED the direct dep: fall back to `cpal::default_host().default_output_device().and_then(|d| d.name().ok())` and classify by name containing "MacBook"/"Built-in" — mark it best-effort, names may be localized.)

- [ ] **Step 4: Command + registration.** In `commands.rs` (near the other audio commands):

```rust
/// Whether the CURRENT default audio output is the built-in speakers (echo risk while
/// capturing system audio). Best-effort introspection — `None` when undeterminable.
#[tauri::command]
pub fn output_is_builtin_speakers() -> Result<Option<bool>, AppError> {
    Ok(crate::audio::output::default_output_is_builtin_speakers())
}
```

Add `output_is_builtin_speakers` to `generate_handler![...]` in `lib.rs:51` — **both edits in the same commit** (an unregistered command is the classic silent-undefined IPC bug).

- [ ] **Step 5: FE.** `ipc.service.ts`:

```ts
  /** Is the default audio output the built-in speakers? null = unknown. */
  outputIsBuiltinSpeakers(): Promise<boolean | null> {
    return this.invoke<boolean | null>("output_is_builtin_speakers");
  }
```

(match the file's existing `invoke` wrapper style). `record.component.ts`: add a signal + load next to the existing `config` load (the component already fetches `AppConfigDto` on entry — mirror that effect):

```ts
  /** Best-effort: is the default output the built-in speakers? null/undetermined ⇒ true. */
  private readonly onSpeakers = signal<boolean>(true);
```

in the same place the config is loaded: `void this.ipc.outputIsBuiltinSpeakers().then((v) => this.onSpeakers.set(v ?? true));` — and tighten the hint (`record.component.ts:1036-1039`):

```ts
  /** Headphones hint: system-audio capture + built-in speakers = echo into the mic. */
  readonly headphonesHint = computed(
    () => (this.config()?.captureSystemAudio ?? false) && this.onSpeakers(),
  );
```

- [ ] **Step 6: Gates**

Run: `cd src-tauri && cargo test --lib && npx ng lint && npx ng build`
Expected: green.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/audio/output.rs src-tauri/src/audio/mod.rs src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/src/commands.rs src-tauri/src/lib.rs src/app/core/ipc.service.ts src/app/features/record/record.component.ts
git commit -m "feat(audio): built-in-speakers detection gates the headphones hint"
```

---

### Task 11: PR-2 — gates, adversarial verify, pull request

- [ ] **Step 1:** `bash scripts/ci.sh` — green, once.
- [ ] **Step 2:** Dispatch the **adversarial-verifier**: (a) AEC test rig honesty — does `cancels_synthetic_echo_by_at_least_15_db` fail on a passthrough stub? (revert-check), (b) pipeline fallback paths (sonora error / length mismatch / no leak / flag off ⇒ byte-identical raw-mic behaviour), (c) config fixture round-trips, (d) the `lib.rs` registration of `output_is_builtin_speakers`, (e) FE toggle round-trip in live Chromium with mocked invoke. Since Task 9 changes WHAT the archive/note contain (not any read/seal path), lock-security review is NOT triggered — but re-confirm: no new at-rest file, ScratchWav guards untouched.
- [ ] **Step 3:** PR to `murmur-io/murmur` with the honest caveat block from Task 12.

---

### Task 12: Real-Mac verification checklist (cannot be verified headless — the honesty bar)

Run on a signed build on the real Mac, with a live call (e.g. a YouTube video or a second-device Meet call) through the **built-in speakers**:

- [ ] **E2E echo verdict:** record 2 min of remote speech on speakers. BEFORE (v0.6.3) vs AFTER: (a) play the archive — the doubled/flanged voice must be gone (offline AEC) or reduced to mild coloration (alignment only, if Task 9 was declined); (b) the transcript must show the remote lines ONCE, as `others*` only; (c) the echo toast fires with a plausible count.
- [ ] **ERLE spike for the VPIO helper (decides its removal):** with `aec_enabled=ON`, play a fixed clip through speakers, compare the raw cpal WAV vs the `aeccap` WAV energy in echo-only spans. Expected per research: ~0 dB → file a follow-up to delete `aeccap` + its settings surface once Task 9 has shipped and baked.
- [ ] **Ducking containment check:** with `aec_enabled=ON` on macOS 14+, system playback volume must NOT audibly duck when recording starts (the Task-6 fix) and the system-audio track must not be near-silent.
- [ ] **First-frame anchor:** boot log shows the helper's `first-frame` line; log the spawn→first-frame delta once (expect 100–500 ms — the merge/mix accuracy win).
- [ ] **Speakers detection:** toggle default output between built-in speakers / wired or BT headphones → the record-screen hint appears/disappears accordingly (including mid-recording).
- [ ] **Genuine-repetition safety on a real Polish meeting:** record a natural conversation where you verbally agree with the remote side ("tak, zróbmy tak") — confirm no genuine `me` line was suppressed (check the suppressed count vs reality).
- [ ] **AEC bake-off (Task 9):** listen to the archive with `post_aec_enabled` on vs off on one real speakers meeting; verify no audible near-end damage (your own voice intact). If quality disappoints → flip the default to `false` and file the DTLN-aec follow-up.

Record the evidence (before/after WAV snippets + transcript diffs, PII-safe) in the PR bodies.

---

## Self-review notes

- **Spec coverage:** dedup (Tasks 2/4), mix alignment + anchor fix (Tasks 1/3/4/5), mixer clamp (Task 3), echo energy (Task 9), VPIO ducking/AGC + honest copy (Task 6), UI hint + detection + toast (Tasks 7/10), live-captions echo = documented known gap (no cheap fix — the system WAV is unreadable mid-write; noted in PR body), separate-tracks storage = rejected (research), lock model untouched (global constraint).
- **Known threshold risk:** `ECHO_RELAXED_SIMILARITY = 0.7` can still eat a ≥4-token verbatim agreement uttered within 4 s on speakers with strong leak — accepted tradeoff (gated by measured acoustic evidence), verified on a real recording in Task 12.
- **Type consistency check:** `EchoLeak{offset_s: f64, correlation: f32}` used in Tasks 1/2/4/9; `suppress_cross_stream_echo(Vec<Segment>, Option<&EchoLeak>) -> (Vec<Segment>, usize)` consistent in Tasks 2/4; `mix_aligned(&[f32], usize, &[f32], usize)` consistent in Tasks 3/4; `EchoSuppressedPayload{suppressed, meeting_id}` consistent in Tasks 4/7 (FE camelCase `meetingId` via serde).
