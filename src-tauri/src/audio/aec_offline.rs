//! Post-hoc echo cancellation over the RECORDED tracks (WebRTC AEC3 via `sonora`).
//!
//! We hold the perfect far-end reference — the digitally captured system-audio track — so
//! this is the textbook AEC setup, run OFFLINE after Stop: zero effect on the live call
//! (no VPIO ducking, no device contention), deterministic, and headless-testable.
//! Best-effort: any error leaves the recording on the raw mic.

use crate::error::{AppError, Result};
use sonora::config::EchoCanceller;
use sonora::{AudioProcessing, Config, StreamConfig};

/// 10 ms @ 16 kHz — the frame size the WebRTC APM processes.
const FRAME: usize = 160;

/// Echo-cancel `mic_16k` using `sys_16k` as the far-end reference. `sys_lead_samples` is the
/// measured alignment pad (from `align::EchoLeak.offset_s`, ≥ 0 — how far the system track
/// leads the mic content); pre-shifting the reference leaves AEC3's own delay estimator only a
/// small residual to track. Returns exactly `mic_16k.len()` samples.
pub fn cancel_echo_offline(
    mic_16k: &[f32],
    sys_16k: &[f32],
    sys_lead_samples: usize,
) -> Result<Vec<f32>> {
    let config = Config {
        echo_canceller: Some(EchoCanceller::default()),
        ..Default::default()
    };
    let sc = StreamConfig::new(16_000, 1);
    let mut apm = AudioProcessing::builder()
        .config(config)
        .capture_config(sc)
        .render_config(sc)
        .build();

    let mut out = Vec::with_capacity(mic_16k.len());
    let mut render = [0.0f32; FRAME];
    let mut render_out = [0.0f32; FRAME];
    let mut capture = [0.0f32; FRAME];
    let mut capture_out = [0.0f32; FRAME];
    let mut i = 0usize;
    while i < mic_16k.len() {
        let n = FRAME.min(mic_16k.len() - i);
        // Render (far-end) frame: the system content the speakers were playing that echoes into
        // the mic around time i — sys index (i + k − lead), zero-padded outside the track.
        for (k, r) in render.iter_mut().enumerate() {
            *r = (i + k)
                .checked_sub(sys_lead_samples)
                .and_then(|j| sys_16k.get(j))
                .copied()
                .unwrap_or(0.0);
        }
        capture[..n].copy_from_slice(&mic_16k[i..i + n]);
        capture[n..].fill(0.0);
        // Process render FIRST (tell the AEC what was played), THEN capture (cancel it out).
        apm.process_render_f32(&[render.as_slice()], &mut [render_out.as_mut_slice()])
            .map_err(|e| AppError::Audio(format!("AEC render: {e:?}")))?;
        apm.process_capture_f32(&[capture.as_slice()], &mut [capture_out.as_mut_slice()])
            .map_err(|e| AppError::Audio(format!("AEC capture: {e:?}")))?;
        out.extend_from_slice(&capture_out[..n]);
        i += FRAME;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn burst_signal(len: usize, seed: u64) -> Vec<f32> {
        let mut x = seed.wrapping_add(1);
        let next = |x: &mut u64| {
            *x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            *x
        };
        let mut out = vec![0.0f32; len];
        let mut i = 0;
        while i < len {
            let burst = 3_000 + (next(&mut x) >> 40) as usize % 9_000;
            let gap = 3_000 + (next(&mut x) >> 40) as usize % 9_000;
            for j in 0..burst.min(len - i) {
                out[i + j] = (next(&mut x) as f32 / u64::MAX as f32 - 0.5) * 0.6;
            }
            i += burst + gap;
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
