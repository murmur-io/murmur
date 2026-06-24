//! Mix the mic track and the system-audio track into one mono buffer.
//!
//! Both inputs must already be at the same sample rate — the pipeline resamples each to
//! 16 kHz first, then mixes. Summation is clamped to [-1.0, 1.0] to avoid overflow when
//! both parties speak at once.

/// Sum two mono f32 tracks into one (clamped). Lengths may differ — the shorter track is
/// treated as silence past its end (one party talking while the other is quiet).
pub fn mix(a: &[f32], b: &[f32]) -> Vec<f32> {
    let n = a.len().max(b.len());
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let x = a.get(i).copied().unwrap_or(0.0) + b.get(i).copied().unwrap_or(0.0);
        out.push(x.clamp(-1.0, 1.0));
    }
    out
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

    #[test]
    fn clamps_to_unit_range() {
        // 0.75 + 0.5 = 1.25 → 1.0 ; -0.75 + -0.5 = -1.25 → -1.0
        assert_eq!(mix(&[0.75, -0.75], &[0.5, -0.5]), vec![1.0, -1.0]);
    }
}
