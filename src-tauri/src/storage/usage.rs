//! Recording-storage accounting + retention. Stateless: usage is a live disk scan
//! (no cached size column → no drift); the prune engine deletes oldest-first, tiered
//! (masters before playback), skipping any meeting in a locked folder (never a `.enc`).

use crate::error::Result;
use std::path::Path;

/// Bytes in one GB (binary GiB, matching disk-tool display).
pub const BYTES_PER_GB: u64 = 1024 * 1024 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AudioUsage {
    pub used_bytes: u64,
    pub playback_bytes: u64,
    pub masters_bytes: u64,
    pub sealed_bytes: u64,
    pub recording_count: u64,
}

/// A meeting's audio file categorized by filename suffix. All audio is `*.wav`; a sealed
/// file is `*.wav.enc` (playback) or `*.mic.wav.enc`/`*.sys.wav.enc` (masters).
enum AudioKind {
    Playback,
    Master,
    Sealed,
    Other,
}

fn classify(name: &str) -> AudioKind {
    if name.ends_with(".enc") {
        AudioKind::Sealed
    } else if name.ends_with(".mic.wav") || name.ends_with(".sys.wav") {
        AudioKind::Master
    } else if name.ends_with(".wav") {
        AudioKind::Playback
    } else {
        AudioKind::Other
    }
}

/// Live disk scan of the audio dir. Sums bytes bucketed by category and counts distinct
/// playback recordings (plaintext `{id}.wav` + sealed `{id}.wav.enc`). A missing dir is
/// zero usage (not an error). Sizes/counts ONLY — never reads file contents.
pub fn scan_audio_usage(audio_dir: &Path) -> Result<AudioUsage> {
    let mut u = AudioUsage::default();
    let entries = match std::fs::read_dir(audio_dir) {
        Ok(e) => e,
        Err(_) => return Ok(u), // dir absent → nothing recorded yet.
    };
    for entry in entries.flatten() {
        let meta = match entry.metadata() {
            Ok(m) if m.is_file() => m,
            _ => continue,
        };
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let len = meta.len();
        match classify(&name) {
            AudioKind::Playback => {
                u.playback_bytes += len;
                u.recording_count += 1;
            }
            AudioKind::Master => u.masters_bytes += len,
            AudioKind::Sealed => {
                u.sealed_bytes += len;
                // A sealed PLAYBACK file counts as a recording (exclude sealed masters).
                if !name.ends_with(".mic.wav.enc") && !name.ends_with(".sys.wav.enc") {
                    u.recording_count += 1;
                }
            }
            AudioKind::Other => continue,
        }
        u.used_bytes += len;
    }
    Ok(u)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("murmur-usage-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn scan_buckets_by_suffix_and_counts_recordings() {
        let d = tmp_dir("scan");
        std::fs::write(d.join("a.wav"), vec![0u8; 1000]).unwrap(); // playback
        std::fs::write(d.join("b.wav"), vec![0u8; 2000]).unwrap(); // playback
        std::fs::write(d.join("a.mic.wav"), vec![0u8; 500]).unwrap(); // master
        std::fs::write(d.join("a.sys.wav"), vec![0u8; 500]).unwrap(); // master
        std::fs::write(d.join("c.wav.enc"), vec![0u8; 4000]).unwrap(); // sealed playback
        std::fs::write(d.join("notes.txt"), vec![0u8; 9]).unwrap(); // ignored

        let u = scan_audio_usage(&d).unwrap();
        assert_eq!(u.playback_bytes, 3000);
        assert_eq!(u.masters_bytes, 1000);
        assert_eq!(u.sealed_bytes, 4000);
        assert_eq!(u.used_bytes, 8000);
        assert_eq!(u.recording_count, 3); // a, b (plaintext) + c (sealed playback)
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn scan_missing_dir_is_zero_not_error() {
        let u = scan_audio_usage(std::path::Path::new("/no/such/murmur/dir")).unwrap();
        assert_eq!(u.used_bytes, 0);
        assert_eq!(u.recording_count, 0);
    }
}
