//! Offline mic↔system stream alignment + speaker-leak detection (envelope NCC).
//!
//! When the user records a call on SPEAKERS, the remote voice bleeds acoustically into the
//! mic. Cross-correlating the two tracks' 100 Hz RMS envelopes over a few windows yields:
//!
//! - `offset_s` — how much later system content appears in the mic (echo lag + capture-start
//!   delta): the pad the archive mix needs, and the dedup window's center;
//! - `correlation` — the leak strength: the "speakers, not headphones" evidence that arms the
//!   relaxed transcript-dedup tier. Headphones ⇒ no peak ⇒ `None` ⇒ dedup stays strict.
//!
//! Pure, deterministic, no deps — fully unit-testable headless. Cost: <100 ms per meeting.

/// 16 kHz / 160 = 100 Hz envelope rate.
const ENV_HOP: usize = 160;
const ENV_RATE: f64 = 100.0;
/// Correlation windows: up to 3 × 30 s (start / middle / end), DISJOINT on a ≥90 s recording
/// so the multi-window lag-agreement check can actually discriminate a genuine constant echo
/// offset (all windows agree) from independent streams / headphones (peaks scatter → rejected).
const WINDOW_ENV: usize = 30 * 100;
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
        for (i, &sv) in sys_env[..n].iter().enumerate() {
            let j = i as i64 + lag;
            if j >= 0 && (j as usize) < n {
                acc += (mic_env[j as usize] - mm) * (sv - sm);
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
    let starts: [usize; 3] = [
        0,
        n.saturating_sub(WINDOW_ENV) / 2,
        n.saturating_sub(WINDOW_ENV),
    ];
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
    Some(EchoLeak {
        offset_s: med_lag as f64 / ENV_RATE,
        correlation: med_corr,
    })
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
        return if l.offset_s >= 0.0 {
            (0, to_samples(l.offset_s))
        } else {
            (to_samples(l.offset_s), 0)
        };
    }
    match sys_started {
        Some(sys) if sys >= mic_started => (
            0,
            to_samples(sys.saturating_duration_since(mic_started).as_secs_f64()),
        ),
        Some(sys) => (
            to_samples(mic_started.saturating_duration_since(sys).as_secs_f64()),
            0,
        ),
        None => (0, 0),
    }
}

/// The same front-padding as [`archive_delays`], derived from the PERSISTED signed system offset
/// instead of live `Instant`s.
///
/// A retry runs long after the capture `Instant`s are gone, but the ledger keeps
/// `recording_generations.system_start_offset_micros` — the identical information, in micros. The
/// archive path calls `archive_delays(None, ..)` (no measured leak), so its wall-clock branch is
/// what this reproduces; `archive_delays_agrees_with_the_persisted_offset` pins the two together so
/// they cannot drift apart.
pub fn archive_delays_from_offset(
    system_start_offset_micros: Option<i64>,
    rate_hz: u32,
) -> (usize, usize) {
    let to_samples =
        |micros: i64| ((micros.unsigned_abs() as f64 / 1_000_000.0) * rate_hz as f64).round() as usize;
    match system_start_offset_micros {
        Some(offset) if offset >= 0 => (0, to_samples(offset)),
        Some(offset) => (to_samples(offset), 0),
        None => (0, 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic pseudo-random speech-like signal (LCG — no rand crate, reproducible).
    /// Burst and gap lengths vary per-burst AND per-seed, so two different seeds produce
    /// genuinely uncorrelated envelopes (the "headphones / independent streams" case) while a
    /// delayed copy of the SAME signal still correlates perfectly (the "echo" case).
    fn burst_signal(len: usize, seed: u64) -> Vec<f32> {
        let mut x = seed.wrapping_add(1);
        let next = |x: &mut u64| {
            *x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            *x
        };
        let mut out = vec![0.0f32; len];
        let mut i = 0;
        while i < len {
            let burst = 3_000 + (next(&mut x) >> 40) as usize % 9_000; // 3000..12000
            let gap = 3_000 + (next(&mut x) >> 40) as usize % 9_000; //   3000..12000
            for j in 0..burst.min(len - i) {
                out[i + j] = (next(&mut x) as f32 / u64::MAX as f32 - 0.5) * 0.6;
            }
            i += burst + gap;
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
        assert!(
            (leak.offset_s - 0.24).abs() <= 0.03,
            "offset {} ≉ 0.24",
            leak.offset_s
        );
        assert!(
            leak.correlation >= MIN_CORR,
            "correlation {} too weak",
            leak.correlation
        );
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

    /// The persisted-offset derivation MUST agree with the live-`Instant` one, or a retry would
    /// align its masters differently than the archive they were cut from.
    #[test]
    fn archive_delays_agrees_with_the_persisted_offset() {
        let t0 = std::time::Instant::now();
        for micros in [0i64, 1, 250_000, 1_000_000, 3_500_000] {
            let sys = t0 + std::time::Duration::from_micros(micros as u64);
            assert_eq!(
                archive_delays(None, t0, Some(sys), 16_000),
                archive_delays_from_offset(Some(micros), 16_000),
                "system started {micros} us AFTER the mic",
            );
            let mic_late = t0 + std::time::Duration::from_micros(micros as u64);
            assert_eq!(
                archive_delays(None, mic_late, Some(t0), 16_000),
                archive_delays_from_offset(Some(-micros), 16_000),
                "system started {micros} us BEFORE the mic",
            );
        }
        // No system track at all: both paths pad nothing.
        assert_eq!(
            archive_delays(None, t0, None, 16_000),
            archive_delays_from_offset(None, 16_000),
        );
    }

    /// archive_delays: measured leak wins; positive offset delays the SYSTEM track.
    #[test]
    fn archive_delays_prefers_measured_leak() {
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        let leak = EchoLeak {
            offset_s: 0.5,
            correlation: 0.8,
        };
        assert_eq!(
            archive_delays(Some(&leak), t0, Some(t0), 16_000),
            (0, 8_000),
            "positive offset ⇒ pad the system track"
        );
        let leak_neg = EchoLeak {
            offset_s: -0.25,
            correlation: 0.8,
        };
        assert_eq!(
            archive_delays(Some(&leak_neg), t0, Some(t0), 16_000),
            (4_000, 0)
        );
        // No leak → wall-clock anchors: system started 0.8 s AFTER the mic ⇒ pad system.
        let sys_started = t0 + Duration::from_millis(800);
        assert_eq!(
            archive_delays(None, t0, Some(sys_started), 16_000),
            (0, 12_800)
        );
        // Mic started later ⇒ pad the mic.
        assert_eq!(
            archive_delays(None, sys_started, Some(t0), 16_000),
            (12_800, 0)
        );
        // No system stream at all → no padding.
        assert_eq!(archive_delays(None, t0, None, 16_000), (0, 0));
    }
}
