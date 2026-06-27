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

/// Read a WAV file (any sample rate) into mono f32 samples plus its sample rate.
/// Supports 16-bit int PCM and 32-bit float WAVs; multi-channel input is down-mixed to
/// mono by averaging. Used to read the system-audio sidecar's WAV before mixing.
pub fn read_wav_mono(path: &Path) -> Result<(Vec<f32>, u32)> {
    let mut reader = hound::WavReader::open(path)
        .map_err(|e| AppError::Audio(format!("open wav {}: {e}", path.display())))?;
    let spec = reader.spec();
    let channels = spec.channels.max(1) as usize;

    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .map(|s| s.map_err(|e| AppError::Audio(format!("decode wav sample: {e}"))))
            .collect::<Result<Vec<f32>>>()?,
        hound::SampleFormat::Int => {
            let max = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| {
                    s.map(|v| v as f32 / max)
                        .map_err(|e| AppError::Audio(format!("decode wav sample: {e}")))
                })
                .collect::<Result<Vec<f32>>>()?
        }
    };

    let mono = if channels <= 1 {
        interleaved
    } else {
        interleaved
            .chunks(channels)
            .map(|f| f.iter().sum::<f32>() / channels as f32)
            .collect()
    };
    Ok((mono, spec.sample_rate))
}

/// In-place loudness normalisation for the 16 kHz ASR FEED ONLY — never the archive master
/// (rec #6). A single scalar peak gain toward `TARGET_PEAK`, capped at `MAX_GAIN` so a
/// near-silent buffer isn't amplified into noise, then hard-clamped to [-1, 1]. It only ever
/// AMPLIFIES quiet audio (never attenuates loud speech) and has no attack/release, so it can't
/// introduce pumping artefacts that would confuse the decoder.
pub fn normalize_for_asr(samples: &mut [f32]) {
    const TARGET_PEAK: f32 = 0.95;
    const MAX_GAIN: f32 = 8.0;
    let peak = samples.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
    if peak <= 0.0 {
        return;
    }
    let gain = (TARGET_PEAK / peak).min(MAX_GAIN);
    if gain <= 1.0 {
        return; // already at/above target — don't touch loud speech
    }
    for s in samples.iter_mut() {
        *s = (*s * gain).clamp(-1.0, 1.0);
    }
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
    fn normalize_amplifies_quiet_audio_capped() {
        let mut s = vec![0.1f32, -0.05, 0.1];
        normalize_for_asr(&mut s);
        // peak 0.1 → ideal gain 9.5, capped at MAX_GAIN 8.0 → 0.1*8 = 0.8.
        assert!((s[0] - 0.8).abs() < 1e-5, "got {}", s[0]);
        assert!((s[1] + 0.4).abs() < 1e-5, "got {}", s[1]);
    }

    #[test]
    fn normalize_leaves_loud_audio_untouched() {
        let mut s = vec![0.98f32, -0.97, 0.5];
        let orig = s.clone();
        normalize_for_asr(&mut s);
        assert_eq!(s, orig, "peak >= target must not change the buffer");
    }

    #[test]
    fn normalize_silence_is_noop() {
        let mut s = vec![0.0f32; 8];
        normalize_for_asr(&mut s);
        assert!(s.iter().all(|&v| v == 0.0));
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
