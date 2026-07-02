//! Mix the mic track and the system-audio track into one mono buffer.
//!
//! Both inputs must already be at the same sample rate — the pipeline resamples each to
//! 16 kHz first, then mixes. `mix_aligned` front-pads each track by its capture-start delay
//! (so the two streams line up on the wall clock, killing most of the perceptual double-hearing
//! on speakers) and peak-normalizes the sum instead of hard-clamping — clamping distorts
//! exactly when both parties speak.

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
/// Lengths may differ — the shorter track is treated as silence past its end.
pub fn mix(a: &[f32], b: &[f32]) -> Vec<f32> {
    mix_aligned(a, 0, b, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Values chosen to be exactly representable in f32 (sums of powers of two).
    #[test]
    fn sums_equal_length() {
        assert_eq!(mix(&[0.5, 0.25], &[0.25, -0.5]), vec![0.75, -0.25]);
    }

    #[test]
    fn pads_shorter_with_silence() {
        assert_eq!(mix(&[0.5], &[0.25, 0.5, 0.25]), vec![0.75, 0.5, 0.25]);
        assert_eq!(mix(&[], &[0.25]), vec![0.25]);
        assert_eq!(mix(&[0.5], &[]), vec![0.5]);
    }

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
}
