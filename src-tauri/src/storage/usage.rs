//! Recording-storage accounting + retention. Stateless: usage is a live disk scan
//! (no cached size column → no drift); the prune engine deletes oldest-first, tiered
//! (masters before playback), skipping any meeting in a locked folder (never a `.enc`).

use crate::error::Result;
use crate::storage::Db;
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PruneSummary {
    pub freed_bytes: u64,
    pub pruned_count: u64,    // playback recordings whose audio was removed
    pub masters_deleted: u64, // hi-res master files removed
}

/// Delete a PLAINTEXT audio file, returning its size (0 if missing). REFUSES to touch a
/// `.enc` — a sealed blob is the at-rest copy of a locked meeting and must never be pruned.
fn delete_plaintext(path: &str) -> u64 {
    if path.ends_with(".enc") {
        return 0;
    }
    let sz = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let _ = std::fs::remove_file(path); // best-effort; a missing file frees 0.
    sz
}

/// Delete oldest recordings' audio until usage ≤ `limit_bytes`. Tiered: hi-res masters first
/// (heaviest, least-used), then the playback WAV. NEVER touches a meeting in a locked folder
/// (excluded by `prunable_audio_candidates`), a `.enc`, or notes/transcripts/timelines.
/// `exclude_meeting` protects the recording that just finished. Idempotent under the cap.
pub fn prune_to_limit(
    db: &Db,
    audio_dir: &Path,
    limit_bytes: u64,
    exclude_meeting: Option<&str>,
) -> Result<PruneSummary> {
    let mut summary = PruneSummary::default();
    let mut used = scan_audio_usage(audio_dir)?.used_bytes;
    if used <= limit_bytes {
        return Ok(summary);
    }
    let candidates = db.prunable_audio_candidates()?; // oldest-first, locked excluded.

    // Pass 1 — hi-res masters (mic then sys) oldest-first.
    for c in &candidates {
        if used <= limit_bytes {
            break;
        }
        if exclude_meeting == Some(c.meeting_id.as_str()) {
            continue;
        }
        if let Some(p) = c.mic_master_path.as_deref() {
            let f = delete_plaintext(p);
            // Conditional-clear (CAS): NULL the column ONLY if it still holds the snapshotted
            // plaintext path `p`. A concurrent seal that re-pointed it to `.enc` between the
            // snapshot and now is left intact — so prune can never orphan a freshly-sealed
            // pointer (TOCTOU data-loss guard). The lifecycle lock at the call sites makes this
            // interleaving impossible in the first place; the CAS is a second, defence-in-depth net.
            db.clear_meeting_mic_master_path_if(&c.meeting_id, p)?;
            used = used.saturating_sub(f);
            summary.freed_bytes += f;
            summary.masters_deleted += 1;
        }
        if used <= limit_bytes {
            break;
        }
        if let Some(p) = c.sys_master_path.as_deref() {
            let f = delete_plaintext(p);
            db.clear_meeting_sys_master_path_if(&c.meeting_id, p)?;
            used = used.saturating_sub(f);
            summary.freed_bytes += f;
            summary.masters_deleted += 1;
        }
    }

    // Pass 2 — the playback WAV oldest-first (meeting keeps its note/transcript/timeline).
    for c in &candidates {
        if used <= limit_bytes {
            break;
        }
        if exclude_meeting == Some(c.meeting_id.as_str()) {
            continue;
        }
        if let Some(p) = c.audio_path.as_deref() {
            let f = delete_plaintext(p);
            db.clear_meeting_audio_path_if(&c.meeting_id, p)?;
            used = used.saturating_sub(f);
            summary.freed_bytes += f;
            summary.pruned_count += 1;
        }
    }
    Ok(summary)
}

/// Auto-prune helper: no-op unless auto-prune is ON and a cap is set. DRYs the guard for the
/// pipeline (after-recording) + settings-save call sites.
pub fn maybe_prune(
    db: &Db,
    audio_dir: &Path,
    limit_gb: Option<u32>,
    auto_prune: bool,
    exclude_meeting: Option<&str>,
) -> Result<PruneSummary> {
    if !auto_prune {
        return Ok(PruneSummary::default());
    }
    // `Some(0)` is NOT a "delete everything" cap — treat it as no cap (mirrors `AppConfig::load`'s
    // `filter(|n| *n > 0)`), so a stray 0 can never prune every recording's audio.
    let Some(gb) = limit_gb.filter(|g| *g > 0) else {
        return Ok(PruneSummary::default());
    };
    prune_to_limit(db, audio_dir, gb as u64 * BYTES_PER_GB, exclude_meeting)
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

    use crate::storage::Db;
    const GOOD_KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn seed_meeting(db: &Db, dir: &std::path::Path, id: &str, at: &str, locked_folder: Option<&str>) {
        // playback 1000B, masters 500B each → 2000B of audio per meeting.
        let wav = dir.join(format!("{id}.wav"));
        let mic = dir.join(format!("{id}.mic.wav"));
        let sys = dir.join(format!("{id}.sys.wav"));
        std::fs::write(&wav, vec![0u8; 1000]).unwrap();
        std::fs::write(&mic, vec![0u8; 500]).unwrap();
        std::fs::write(&sys, vec![0u8; 500]).unwrap();
        db.insert_meeting(&crate::storage::Meeting {
            id: id.into(),
            started_at: at.into(),
            ended_at: None,
            title: Some("t".into()),
            duration_s: 1,
            audio_path: Some(wav.to_string_lossy().into()),
            status: crate::storage::MeetingStatus::Summarized,
            folder_id: None,
        })
        .unwrap();
        db.set_meeting_mic_master_path(id, Some(mic.to_string_lossy().as_ref()))
            .unwrap();
        db.set_meeting_sys_master_path(id, Some(sys.to_string_lossy().as_ref()))
            .unwrap();
        db.upsert_note(&crate::storage::NoteRecord {
            meeting_id: id.into(),
            provider_id: "claude_code".into(),
            markdown: "kept".into(),
            created_at: at.into(),
            exported_path: None,
            model_requested: None,
            model_served: None,
            gateway_host: None,
        })
        .unwrap();
        if let Some(fid) = locked_folder {
            db.set_note_folder(id, Some(fid)).unwrap();
        }
    }

    #[test]
    fn prune_deletes_masters_before_playback_oldest_first() {
        let d = tmp_dir("prune-tier");
        let p = crate::storage::db::unique_temp_path("murmur-prune-tier", "sqlite");
        let _ = std::fs::remove_file(&p);
        let db = Db::open_with_key(&p, GOOD_KEY).unwrap();
        seed_meeting(&db, &d, "old", "2026-01-01T00:00:00Z", None);
        seed_meeting(&db, &d, "new", "2026-06-01T00:00:00Z", None); // 4000B total, cap 3000B.

        let s = prune_to_limit(&db, &d, 3000, None).unwrap();

        // Freed exactly the two OLD masters (1000B); playback + note untouched.
        assert_eq!(s.masters_deleted, 2);
        assert_eq!(s.pruned_count, 0);
        assert_eq!(s.freed_bytes, 1000);
        assert!(!d.join("old.mic.wav").exists() && !d.join("old.sys.wav").exists());
        assert!(d.join("old.wav").exists(), "playback kept — masters were enough");
        assert!(d.join("new.mic.wav").exists(), "newest untouched");
        // Context preserved: the note still reads back.
        assert_eq!(
            db.get_latest_note_for_meeting("old").unwrap().unwrap().markdown,
            "kept"
        );
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn prune_never_touches_locked_folder_audio() {
        let d = tmp_dir("prune-lock");
        let p = crate::storage::db::unique_temp_path("murmur-prune-lock", "sqlite");
        let _ = std::fs::remove_file(&p);
        let db = Db::open_with_key(&p, GOOD_KEY).unwrap();
        db.insert_folder(&crate::storage::Folder {
            id: "f".into(),
            name: "S".into(),
            path: "S".into(),
            parent_id: None,
            locked: true,
            created_at: "2026-01-01T00:00:00Z".into(),
        })
        .unwrap();
        seed_meeting(&db, &d, "locked", "2026-01-01T00:00:00Z", Some("f")); // oldest but LOCKED
        seed_meeting(&db, &d, "open", "2026-06-01T00:00:00Z", None);

        // Cap 0 → prune must free everything it CAN — but the locked meeting is exempt.
        let s = prune_to_limit(&db, &d, 0, None).unwrap();
        assert!(d.join("locked.wav").exists(), "locked audio must NEVER be deleted");
        assert!(d.join("locked.mic.wav").exists());
        assert!(!d.join("open.wav").exists(), "the open meeting's audio was pruned");
        assert!(s.pruned_count >= 1);
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn maybe_prune_is_noop_when_disabled_or_no_cap() {
        let d = tmp_dir("prune-noop");
        let p = crate::storage::db::unique_temp_path("murmur-prune-noop", "sqlite");
        let _ = std::fs::remove_file(&p);
        let db = Db::open_with_key(&p, GOOD_KEY).unwrap();
        seed_meeting(&db, &d, "m", "2026-01-01T00:00:00Z", None);
        assert_eq!(
            maybe_prune(&db, &d, Some(2), false, None).unwrap().freed_bytes,
            0,
            "auto off → no-op"
        );
        assert_eq!(
            maybe_prune(&db, &d, None, true, None).unwrap().freed_bytes,
            0,
            "no cap → no-op"
        );
        assert!(d.join("m.wav").exists());
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_file(&p);
    }
}
