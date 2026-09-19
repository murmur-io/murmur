//! Read-only growing RIFF source. AVAudioFile leaves chunk sizes stale until close.
//! Parse chunks (including Apple's JUNK/FLLR), then trust only complete physical PCM frames.
use crate::error::{AppError, Result};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

pub(crate) struct LiveWavTail {
    file: File,
    data: u64,
    rate: u32,
    channels: usize,
    format: u16,
    bits: u16,
    align: usize,
    end_frame: u64,
}
pub(crate) struct TailWindow {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub start_frame: u64,
    pub end_frame: u64,
}
impl LiveWavTail {
    pub fn open(path: &Path) -> Result<Self> {
        let mut file = File::open(path).map_err(io_error)?;
        let mut riff = [0; 12];
        file.read_exact(&mut riff).map_err(io_error)?;
        if &riff[..4] != b"RIFF" || &riff[8..] != b"WAVE" {
            return Err(AppError::Audio("live source is not RIFF/WAVE".into()));
        }
        let mut format = None;
        let length = file.metadata().map_err(io_error)?.len();
        let mut cursor = 12u64;
        while cursor + 8 <= length && cursor < 1024 * 1024 {
            file.seek(SeekFrom::Start(cursor)).map_err(io_error)?;
            let mut chunk = [0; 8];
            file.read_exact(&mut chunk).map_err(io_error)?;
            let size = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]) as u64;
            if &chunk[..4] == b"fmt " {
                if !(16..=4096).contains(&size) {
                    return Err(AppError::Audio("unsupported live WAV format".into()));
                }
                let mut fmt = vec![0; size as usize];
                file.read_exact(&mut fmt).map_err(io_error)?;
                let u16at = |i| u16::from_le_bytes([fmt[i], fmt[i + 1]]);
                let mut code = u16at(0);
                if code == 0xfffe && fmt.len() >= 40 {
                    code = u16at(24);
                }
                let channels = u16at(2) as usize;
                let rate = u32::from_le_bytes([fmt[4], fmt[5], fmt[6], fmt[7]]);
                let align = u16at(12) as usize;
                let bits = u16at(14);
                if !(1..=8).contains(&channels)
                    || !(8000..=192000).contains(&rate)
                    || align != channels * (bits as usize / 8)
                    || !matches!((code, bits), (3, 32) | (1, 16) | (1, 24) | (1, 32))
                {
                    return Err(AppError::Audio("unsupported live PCM layout".into()));
                }
                format = Some((code, channels, rate, align, bits));
            } else if &chunk[..4] == b"data" {
                let (format, channels, rate, align, bits) =
                    format.ok_or_else(|| AppError::Audio("live WAV has no format".into()))?;
                return Ok(Self {
                    file,
                    data: cursor + 8,
                    rate,
                    channels,
                    format,
                    bits,
                    align,
                    end_frame: 0,
                });
            }
            cursor = cursor.saturating_add(8 + size + size % 2);
        }
        Err(AppError::Audio("live WAV header is not ready".into()))
    }
    pub fn window(&mut self, seconds: usize) -> Result<Option<TailWindow>> {
        let end = self
            .file
            .metadata()
            .map_err(io_error)?
            .len()
            .saturating_sub(self.data)
            / self.align as u64;
        if end <= self.end_frame || end < self.rate as u64 {
            return Ok(None);
        }
        let start = end.saturating_sub(seconds.min(14) as u64 * self.rate as u64);
        let mut raw = vec![0; (end - start) as usize * self.align];
        self.file
            .seek(SeekFrom::Start(self.data + start * self.align as u64))
            .map_err(io_error)?;
        self.file.read_exact(&mut raw).map_err(io_error)?;
        let bytes = self.bits as usize / 8;
        let samples = raw
            .chunks_exact(self.align)
            .map(|frame| {
                frame
                    .chunks_exact(bytes)
                    .map(|v| {
                        let sample = match (self.format, self.bits) {
                            (3, 32) => f32::from_le_bytes([v[0], v[1], v[2], v[3]]),
                            (1, 16) => i16::from_le_bytes([v[0], v[1]]) as f32 / 32768.0,
                            (1, 24) => {
                                ((i32::from_le_bytes([0, v[0], v[1], v[2]])) >> 8) as f32
                                    / 8_388_608.0
                            }
                            (1, 32) => {
                                i32::from_le_bytes([v[0], v[1], v[2], v[3]]) as f32
                                    / 2_147_483_648.0
                            }
                            _ => 0.0,
                        };
                        if sample.is_finite() {
                            sample
                        } else {
                            0.0
                        }
                    })
                    .sum::<f32>()
                    / self.channels as f32
            })
            .collect();
        self.end_frame = end;
        Ok(Some(TailWindow {
            samples,
            sample_rate: self.rate,
            start_frame: start,
            end_frame: end,
        }))
    }
}
fn io_error(error: std::io::Error) -> AppError {
    AppError::Audio(format!("live WAV read failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn growing_riff_ignores_stale_sizes_and_partial_frames() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("murmur-live-tail-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("live.wav");
        let mut b = b"RIFF\0\0\0\0WAVEJUNK\x03\0\0\0abc\0fmt \x10\0\0\0".to_vec();
        b.extend_from_slice(&3u16.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&8000u32.to_le_bytes());
        b.extend_from_slice(&32000u32.to_le_bytes());
        b.extend_from_slice(&4u16.to_le_bytes());
        b.extend_from_slice(&32u16.to_le_bytes());
        b.extend_from_slice(b"data\0\0\0\0");
        for _ in 0..8000 {
            b.extend_from_slice(&0.25f32.to_le_bytes());
        }
        b.push(0);
        std::fs::write(&path, b).unwrap();
        let mut source = LiveWavTail::open(&path).unwrap();
        let first = source.window(14).unwrap().unwrap();
        assert_eq!(first.samples.len(), 8000);
        assert!(first.samples.iter().all(|s| *s == 0.25));
        assert!(source.window(14).unwrap().is_none());
        let mut writer = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writer.write_all(&[0, 128, 62]).unwrap();
        let second = source.window(14).unwrap().unwrap();
        assert_eq!(second.end_frame, 8001);
        assert_eq!(second.samples[8000], 0.25);
        drop(source);
        drop(writer);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
