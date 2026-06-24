use std::path::Path;

use hound::{SampleFormat, WavSpec, WavWriter};
use rubato::{FftFixedIn, Resampler};

use crate::error::{AppError, Result};

pub const TARGET_RATE_HZ: u32 = 16_000;

/// Chunk size (in input frames) fed to the FFT resampler per `process` call.
const RESAMPLE_CHUNK: usize = 1024;

/// Resample mono f32 @ src_rate to 16 kHz mono and write 16-bit PCM WAV to `path`.
pub fn write_wav_16k_mono(path: &Path, samples: &[f32], src_rate: u32) -> Result<()> {
    let resampled = resample_to_16k(samples, src_rate)?;

    let spec = WavSpec {
        channels: 1,
        sample_rate: TARGET_RATE_HZ,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };

    let mut writer = WavWriter::create(path, spec)
        .map_err(|e| AppError::Audio(format!("failed to create WAV file: {e}")))?;

    for &s in &resampled {
        // Clamp to [-1.0, 1.0] then scale to i16. 32767 avoids overflow at +1.0.
        let clamped = s.clamp(-1.0, 1.0);
        let v = (clamped * i16::MAX as f32).round() as i16;
        writer
            .write_sample(v)
            .map_err(|e| AppError::Audio(format!("failed to write WAV sample: {e}")))?;
    }

    writer
        .finalize()
        .map_err(|e| AppError::Audio(format!("failed to finalize WAV file: {e}")))?;

    Ok(())
}

/// Resample mono f32 @ src_rate to 16 kHz mono f32 (in-memory, for Whisper input).
///
/// Whisper expects 16 kHz mono `f32` in [-1.0, 1.0]. If the source is already 16 kHz
/// the input is returned unchanged. Empty input yields empty output.
pub fn resample_to_16k(samples: &[f32], src_rate: u32) -> Result<Vec<f32>> {
    if src_rate == 0 {
        return Err(AppError::Audio("source sample rate is zero".into()));
    }
    if samples.is_empty() || src_rate == TARGET_RATE_HZ {
        return Ok(samples.to_vec());
    }

    // FftFixedIn consumes a fixed number of input frames per chunk and emits the
    // corresponding output frames. We process in CHUNK-sized blocks and zero-pad the
    // final partial block so no tail samples are dropped.
    let mut resampler =
        FftFixedIn::<f32>::new(src_rate as usize, TARGET_RATE_HZ as usize, RESAMPLE_CHUNK, 1, 1)
            .map_err(|e| AppError::Audio(format!("failed to build resampler: {e}")))?;

    // Pre-size output using the exact ratio (a small over-estimate is fine).
    let est = (samples.len() as f64 * TARGET_RATE_HZ as f64 / src_rate as f64).ceil() as usize;
    let mut out: Vec<f32> = Vec::with_capacity(est + RESAMPLE_CHUNK);

    let mut pos = 0usize;
    // Reusable single-channel input/output frame buffers.
    let mut in_buf = vec![vec![0.0f32; RESAMPLE_CHUNK]];

    while pos < samples.len() {
        let take = RESAMPLE_CHUNK.min(samples.len() - pos);
        let chan = &mut in_buf[0];
        chan[..take].copy_from_slice(&samples[pos..pos + take]);
        // Zero-pad the final short chunk so the fixed-size resampler accepts it.
        for v in chan.iter_mut().take(RESAMPLE_CHUNK).skip(take) {
            *v = 0.0;
        }
        pos += take;

        let chunk_out = resampler
            .process(&in_buf, None)
            .map_err(|e| AppError::Audio(format!("resample failed: {e}")))?;
        if let Some(ch) = chunk_out.into_iter().next() {
            out.extend_from_slice(&ch);
        }
    }

    // Trim any over-estimated tail produced by zero-padding the last partial chunk.
    if out.len() > est {
        out.truncate(est);
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_when_already_16k() {
        let samples = vec![0.1f32, -0.2, 0.3];
        let out = resample_to_16k(&samples, TARGET_RATE_HZ).unwrap();
        assert_eq!(out, samples);
    }

    #[test]
    fn empty_input_yields_empty() {
        let out = resample_to_16k(&[], 48_000).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn zero_rate_is_error() {
        assert!(resample_to_16k(&[0.0], 0).is_err());
    }

    #[test]
    fn downsamples_sine_to_expected_length() {
        // 1 second of 440 Hz sine at 48 kHz → ~16000 samples at 16 kHz.
        let src_rate = 48_000u32;
        let n = src_rate as usize;
        let mut input = Vec::with_capacity(n);
        for i in 0..n {
            let t = i as f32 / src_rate as f32;
            input.push((2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.5);
        }
        let out = resample_to_16k(&input, src_rate).unwrap();
        let expected = TARGET_RATE_HZ as usize;
        // Allow a small delta for resampler framing.
        let diff = (out.len() as i64 - expected as i64).unsigned_abs() as usize;
        assert!(
            diff <= RESAMPLE_CHUNK,
            "expected ~{expected} samples, got {}",
            out.len()
        );
    }
}
