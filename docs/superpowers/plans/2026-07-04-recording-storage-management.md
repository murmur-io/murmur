# Recording Storage Management — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the user see where recordings live + how much disk they use, cap the audio footprint in GB, and auto-remove the oldest recordings when over the cap — **never** notes/transcripts/timelines, and **never** locked (Touch-ID) audio.

**Architecture:** A stateless disk-scan computes usage (no cached size column, no drift). A prune engine deletes oldest-first, tiered (hi-res masters before the playback WAV), excluding any meeting in a locked folder. Two new `AppConfig` fields (`audio_storage_limit_gb`, `audio_auto_prune`) drive it. Auto-prune runs after each recording and on settings-save; a manual "Free up space" command is always available. A new Settings "Storage" section + a Library header usage bar surface it. No schema migration is needed.

**Tech Stack:** Rust (Tauri 2.11, `meetnotes_lib`, rusqlite/SQLCipher), Angular 18 zoneless (standalone, signals).

## Global Constraints

- Errors: every fallible Rust fn returns `crate::error::Result<T>` (`= Result<T, AppError>`); use `AppError::Storage`/`Config`/`InvalidArg`. NO `unwrap`/`expect` in non-test code.
- Rust test loop: `(cd src-tauri && cargo test --lib)` ONLY. NEVER `cargo clippy --all-targets` in the loop (openssl/sqlcipher profile thrash). Full gate `bash scripts/ci.sh` runs ONCE at the end.
- Additive-only DB: no `DROP`/`DELETE`/rewrite of user rows. (This plan adds NO migration.)
- Lock model is load-bearing: auto-prune MUST skip meetings in a locked folder (`folders.locked = 1`) → it never touches a `.enc`. No new content-read/export path; the report exposes byte counts only, never note/transcript text (no PII in logs).
- Verify-before-destroy discipline stays: we are DELETING audio deliberately (the feature), but only PLAINTEXT `.wav`/master files of NON-locked meetings, never a sealed `.enc`.
- FE is zoneless/standalone/signals: `@if`/`@for` only, `input()`/`output()`/`inject()`, state in `signal()`, one typed `IpcService` method per command, NO `.subscribe()` into a field, NO NgRx, inline template+styles, `var(--token)` styling, 16 kB/component style budget.
- Commits: QueaT `<kgm004a@gmail.com>`, NO Claude trailers, on branch `feat/recording-storage-mgmt`. NEVER push to `murmur` trunk directly — PR at the end.
- No new npm packages or crates.
- Adversarial-verifier owns PASS/FAIL; lock-security-reviewer is a REQUIRED second gate (touches audio deletion + visibility).

## File Structure

**Backend (`src-tauri/src`):**
- `storage/usage.rs` — **NEW.** Pure disk-scan (`scan_audio_usage`) + prune engine (`prune_to_limit`, `maybe_prune`) + `AudioUsage`/`PruneSummary`/`BYTES_PER_GB`. Depends on `crate::storage::Db` + `std::fs`. Testable with temp dir + temp DB.
- `storage/mod.rs` — register `pub mod usage;`, re-export `PrunableAudio`.
- `storage/db.rs` — add `PrunableAudio` struct + `Db::prunable_audio_candidates()` (oldest-first, locked excluded).
- `settings/config.rs` — 2 new `AppConfig` fields + `K_*` consts + load/save.
- `commands.rs` — `AppConfigDto` 2 new fields + `config_to_dto`/`dto_to_config` wiring; new commands `get_storage_report`, `free_up_space`, `reveal_audio_dir` + their DTOs; auto-prune hook in `save_config`.
- `pipeline.rs` — `audio_dir()` → `pub(crate)`; auto-prune call after the masters block in `run_inner`.
- `events.rs` — `EVENT_STORAGE_PRUNED` + `StoragePrunedPayload`.
- `lib.rs` — register the 3 new commands in `generate_handler!`.

**Frontend (`src/app`):**
- `core/models.ts` — `AppConfigDto` 2 new fields; `StorageReport` + `PruneSummary` interfaces.
- `core/ipc.service.ts` — `getStorageReport()`, `freeUpSpace()`, `revealAudioDir()`.
- `features/settings/settings.store.ts` — 2 new form controls + load/save wiring; storage-report signal + `loadStorageReport()` + `freeUpSpace()` + `revealAudioDir()`.
- `features/settings/sections/settings-storage-section.component.ts` — **NEW** section.
- `features/settings/settings.component.ts` — register the "storage" section (sidebar entry + icon + import + `@case`).
- `features/library/library.component.ts` — header usage bar (load report in `ngOnInit`); "audio freed" row chip.

---

### Task 1: `AppConfig` + DTO fields (limit + auto-prune)

**Files:**
- Modify: `src-tauri/src/settings/config.rs` (struct ~70, Default ~362, `K_*` ~423, `load` ~481, `save` ~663)
- Modify: `src-tauri/src/commands.rs` (`AppConfigDto` ~96, `config_to_dto` ~3395, `dto_to_config` ~3458)
- Test: `src-tauri/src/settings/config.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Produces: `AppConfig.audio_storage_limit_gb: Option<u32>`, `AppConfig.audio_auto_prune: bool`; DTO camelCase `audioStorageLimitGb: Option<u32>`, `audioAutoPrune: bool`.

- [ ] **Step 1: Write the failing test** — append to `config.rs` tests:

```rust
#[test]
fn audio_storage_settings_round_trip() {
    let p = crate::storage::db::unique_temp_path("murmur-cfg-storage", "sqlite");
    let _ = std::fs::remove_file(&p);
    let db = Db::open_with_key(&p, "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef").unwrap();

    // Defaults: no cap, auto-prune OFF (fail-safe).
    let def = AppConfig::default();
    assert_eq!(def.audio_storage_limit_gb, None);
    assert!(!def.audio_auto_prune);

    let mut cfg = AppConfig::default();
    cfg.audio_storage_limit_gb = Some(2);
    cfg.audio_auto_prune = true;
    cfg.save(&db).unwrap();

    let loaded = AppConfig::load(&db).unwrap();
    assert_eq!(loaded.audio_storage_limit_gb, Some(2));
    assert!(loaded.audio_auto_prune);
    let _ = std::fs::remove_file(&p);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `(cd src-tauri && cargo test --lib audio_storage_settings_round_trip)`
Expected: FAIL to compile — `no field audio_storage_limit_gb on AppConfig`.

- [ ] **Step 3: Write minimal implementation**

In `config.rs` struct (near `keep_hires_masters`):
```rust
    /// Recording-storage cap in GB (`None` = no cap). Drives auto-prune of the OLDEST
    /// recordings' audio when exceeded; notes/transcripts are never touched.
    pub audio_storage_limit_gb: Option<u32>,
    /// When true AND a cap is set, delete the oldest recordings' audio after each
    /// recording / on save to stay under the cap. OPT-IN, default OFF.
    pub audio_auto_prune: bool,
```
In `impl Default for AppConfig` (fill both): `audio_storage_limit_gb: None,` and `audio_auto_prune: false,`.
Add `K_*` consts near the others:
```rust
const K_AUDIO_STORAGE_LIMIT_GB: &str = "audio_storage_limit_gb";
const K_AUDIO_AUTO_PRUNE: &str = "audio_auto_prune";
```
In `load` (before `Ok(cfg)`):
```rust
        cfg.audio_storage_limit_gb = db
            .get_setting(K_AUDIO_STORAGE_LIMIT_GB)?
            .and_then(|v| v.trim().parse::<u32>().ok())
            .filter(|n| *n > 0);
        if let Some(v) = db.get_setting(K_AUDIO_AUTO_PRUNE)? {
            cfg.audio_auto_prune = v == "true";
        }
```
In `save` (before `Ok(())`):
```rust
        db.set_setting(
            K_AUDIO_STORAGE_LIMIT_GB,
            &self.audio_storage_limit_gb.map(|n| n.to_string()).unwrap_or_default(),
        )?;
        db.set_setting(
            K_AUDIO_AUTO_PRUNE,
            if self.audio_auto_prune { "true" } else { "false" },
        )?;
```
In `commands.rs` `AppConfigDto` (after `post_aec_enabled`):
```rust
    #[serde(default)]
    pub audio_storage_limit_gb: Option<u32>,
    #[serde(default)]
    pub audio_auto_prune: bool,
```
In `config_to_dto`: `audio_storage_limit_gb: c.audio_storage_limit_gb, audio_auto_prune: c.audio_auto_prune,`.
In `dto_to_config` (plain settable — the Storage UI owns them): `audio_storage_limit_gb: d.audio_storage_limit_gb, audio_auto_prune: d.audio_auto_prune,`.

- [ ] **Step 4: Run test + fix every other construction site**

Run: `(cd src-tauri && cargo test --lib)`
Expected: the compiler lists EVERY remaining `AppConfig { .. }` / `AppConfigDto { .. }` literal (tests included) missing the two fields. Add `audio_storage_limit_gb: None, audio_auto_prune: false,` to each. Re-run until green + `audio_storage_settings_round_trip` PASSES.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/settings/config.rs src-tauri/src/commands.rs
git commit -m "feat(storage): add audio_storage_limit_gb + audio_auto_prune config fields"
```

---

### Task 2: Usage disk-scan (`storage/usage.rs`)

**Files:**
- Create: `src-tauri/src/storage/usage.rs`
- Modify: `src-tauri/src/storage/mod.rs` (add `pub mod usage;`)
- Modify: `src-tauri/src/pipeline.rs` (`fn audio_dir` → `pub(crate) fn audio_dir`)
- Test: in `usage.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Produces: `pub const BYTES_PER_GB: u64`; `pub struct AudioUsage { used_bytes, playback_bytes, masters_bytes, sealed_bytes, recording_count }`; `pub fn scan_audio_usage(audio_dir: &std::path::Path) -> Result<AudioUsage>`.

- [ ] **Step 1: Write the failing test** — create `usage.rs` with ONLY the test module first:

```rust
//! Recording-storage accounting + retention. Stateless: usage is a live disk scan
//! (no cached size column → no drift); the prune engine deletes oldest-first, tiered
//! (masters before playback), skipping any meeting in a locked folder (never a `.enc`).

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
        std::fs::write(d.join("a.wav"), vec![0u8; 1000]).unwrap();       // playback
        std::fs::write(d.join("b.wav"), vec![0u8; 2000]).unwrap();       // playback
        std::fs::write(d.join("a.mic.wav"), vec![0u8; 500]).unwrap();    // master
        std::fs::write(d.join("a.sys.wav"), vec![0u8; 500]).unwrap();    // master
        std::fs::write(d.join("c.wav.enc"), vec![0u8; 4000]).unwrap();   // sealed playback
        std::fs::write(d.join("notes.txt"), vec![0u8; 9]).unwrap();      // ignored

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
```

- [ ] **Step 2: Run test to verify it fails**

Add `pub mod usage;` to `src-tauri/src/storage/mod.rs` first, then run:
`(cd src-tauri && cargo test --lib scan_buckets_by_suffix)`
Expected: FAIL to compile — `cannot find function scan_audio_usage`.

- [ ] **Step 3: Write minimal implementation** — prepend above the test module in `usage.rs`:

```rust
use std::path::Path;
use crate::error::Result;

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
enum AudioKind { Playback, Master, Sealed, Other }

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
```
Also change `pipeline.rs` line ~90: `fn audio_dir()` → `pub(crate) fn audio_dir()`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `(cd src-tauri && cargo test --lib usage::tests)`
Expected: PASS (both scan tests).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/storage/usage.rs src-tauri/src/storage/mod.rs src-tauri/src/pipeline.rs
git commit -m "feat(storage): audio-usage disk scan (usage.rs) + expose audio_dir"
```

---

### Task 3: `Db::prunable_audio_candidates()` (oldest-first, locked excluded)

**Files:**
- Modify: `src-tauri/src/storage/db.rs` (add `PrunableAudio` near `LockedMeetingAudio` ~25; add the method near `folder_for_meeting` ~3403)
- Modify: `src-tauri/src/storage/mod.rs` (re-export `PrunableAudio` alongside `LockedMeetingAudio`)
- Test: in `db.rs` tests

**Interfaces:**
- Consumes: `notes.folder_id`, `folders.locked` (a meeting's folder = its note's folder).
- Produces: `pub struct PrunableAudio { meeting_id, started_at, audio_path, mic_master_path, sys_master_path }`; `Db::prunable_audio_candidates(&self) -> Result<Vec<PrunableAudio>>` (ORDER BY started_at ASC).

- [ ] **Step 1: Write the failing test** — append to `db.rs` tests:

```rust
#[test]
fn prunable_candidates_are_oldest_first_and_exclude_locked_folders() {
    let p = unique_temp_path("murmur-prunable", "sqlite");
    let _ = std::fs::remove_file(&p);
    let db = Db::open_with_key(&p, GOOD_KEY).unwrap();

    // Two meetings in an OPEN vault-root (folder_id NULL), one in a LOCKED folder.
    for (id, at) in [("old", "2026-01-01T00:00:00Z"), ("new", "2026-06-01T00:00:00Z"), ("secret", "2026-03-01T00:00:00Z")] {
        db.insert_meeting(&crate::storage::Meeting {
            id: id.into(), started_at: at.into(), ended_at: None, title: Some("t".into()),
            duration_s: 1, audio_path: Some(format!("/a/{id}.wav")), status: crate::storage::MeetingStatus::Summarized, folder_id: None,
        }).unwrap();
        db.upsert_note(&crate::storage::NoteRecord { meeting_id: id.into(), provider_id: "claude_code".into(), markdown: "m".into(), created_at: at.into(), exported_path: None, model_requested: None, model_served: None, gateway_host: None }).unwrap();
    }
    db.insert_folder(&crate::storage::Folder { id: "f".into(), name: "Secret".into(), path: "Secret".into(), parent_id: None, locked: true, created_at: "2026-01-01T00:00:00Z".into() }).unwrap();
    db.set_note_folder("secret", Some("f")).unwrap();

    let cands = db.prunable_audio_candidates().unwrap();
    let ids: Vec<&str> = cands.iter().map(|c| c.meeting_id.as_str()).collect();
    assert_eq!(ids, vec!["old", "new"], "locked 'secret' excluded; oldest-first order");
    let _ = std::fs::remove_file(&p);
}
```
(Use the exact `GOOD_KEY` const already present in `db.rs` tests.)

- [ ] **Step 2: Run test to verify it fails**

Run: `(cd src-tauri && cargo test --lib prunable_candidates_are_oldest_first)`
Expected: FAIL to compile — `no method prunable_audio_candidates`.

- [ ] **Step 3: Write minimal implementation** — add struct near `LockedMeetingAudio`:

```rust
/// One meeting eligible for storage prune: NOT in a locked folder, with its three audio paths.
/// Ordered oldest-first by [`Db::prunable_audio_candidates`]. Any column may be `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrunableAudio {
    pub meeting_id: String,
    pub started_at: String,
    pub audio_path: Option<String>,
    pub mic_master_path: Option<String>,
    pub sys_master_path: Option<String>,
}
```
Add the method (near `folder_for_meeting`):
```rust
    /// Meetings whose audio may be auto-pruned: every meeting NOT in a locked folder
    /// (a meeting's folder = its `notes.folder_id`), OLDEST FIRST. A locked folder's audio
    /// is exempt — it is the sealed `.enc` at rest and must never be deleted by prune.
    pub fn prunable_audio_candidates(&self) -> Result<Vec<PrunableAudio>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, started_at, audio_path, mic_master_path, sys_master_path \
                   FROM meetings \
                  WHERE id NOT IN ( \
                      SELECT DISTINCT meeting_id FROM notes \
                       WHERE folder_id IN (SELECT id FROM folders WHERE locked = 1) \
                  ) \
                  ORDER BY started_at ASC, id ASC",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok(PrunableAudio {
                    meeting_id: r.get(0)?,
                    started_at: r.get(1)?,
                    audio_path: r.get(2)?,
                    mic_master_path: r.get(3)?,
                    sys_master_path: r.get(4)?,
                })
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }
```
In `storage/mod.rs`, add `PrunableAudio` to the same `pub use db::{ ... }` line that exports `LockedMeetingAudio`.

- [ ] **Step 4: Run test to verify it passes**

Run: `(cd src-tauri && cargo test --lib prunable_candidates_are_oldest_first)`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/storage/db.rs src-tauri/src/storage/mod.rs
git commit -m "feat(storage): Db::prunable_audio_candidates (oldest-first, locked excluded)"
```

---

### Task 4: Prune engine (`prune_to_limit` + `maybe_prune`) — tiered, lock-safe, context-preserving

**Files:**
- Modify: `src-tauri/src/storage/usage.rs`
- Test: in `usage.rs` tests

**Interfaces:**
- Consumes: `crate::storage::Db` (`prunable_audio_candidates`, `set_meeting_audio_path`, `set_meeting_mic_master_path`, `set_meeting_sys_master_path`), `scan_audio_usage`.
- Produces: `pub struct PruneSummary { freed_bytes, pruned_count, masters_deleted }`; `pub fn prune_to_limit(db: &Db, audio_dir: &Path, limit_bytes: u64, exclude_meeting: Option<&str>) -> Result<PruneSummary>`; `pub fn maybe_prune(db: &Db, audio_dir: &Path, limit_gb: Option<u32>, auto_prune: bool, exclude_meeting: Option<&str>) -> Result<PruneSummary>`.

- [ ] **Step 1: Write the failing tests** — append to `usage.rs` tests:

```rust
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
            id: id.into(), started_at: at.into(), ended_at: None, title: Some("t".into()),
            duration_s: 1, audio_path: Some(wav.to_string_lossy().into()),
            status: crate::storage::MeetingStatus::Summarized, folder_id: None,
        }).unwrap();
        db.set_meeting_mic_master_path(id, Some(mic.to_string_lossy().as_ref())).unwrap();
        db.set_meeting_sys_master_path(id, Some(sys.to_string_lossy().as_ref())).unwrap();
        db.upsert_note(&crate::storage::NoteRecord { meeting_id: id.into(), provider_id: "claude_code".into(), markdown: "kept".into(), created_at: at.into(), exported_path: None, model_requested: None, model_served: None, gateway_host: None }).unwrap();
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
        assert_eq!(db.get_latest_note_for_meeting("old").unwrap().unwrap().markdown, "kept");
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn prune_never_touches_locked_folder_audio() {
        let d = tmp_dir("prune-lock");
        let p = crate::storage::db::unique_temp_path("murmur-prune-lock", "sqlite");
        let _ = std::fs::remove_file(&p);
        let db = Db::open_with_key(&p, GOOD_KEY).unwrap();
        db.insert_folder(&crate::storage::Folder { id: "f".into(), name: "S".into(), path: "S".into(), parent_id: None, locked: true, created_at: "2026-01-01T00:00:00Z".into() }).unwrap();
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
        assert_eq!(maybe_prune(&db, &d, Some(2), false, None).unwrap().freed_bytes, 0, "auto off → no-op");
        assert_eq!(maybe_prune(&db, &d, None, true, None).unwrap().freed_bytes, 0, "no cap → no-op");
        assert!(d.join("m.wav").exists());
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_file(&p);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `(cd src-tauri && cargo test --lib prune_deletes_masters_before_playback)`
Expected: FAIL to compile — `cannot find function prune_to_limit`. (The lock test is the RED-before-GREEN guard: a naive engine that ignores `prunable_audio_candidates`' locked filter would delete `locked.wav` and fail.)

- [ ] **Step 3: Write minimal implementation** — add to `usage.rs` (after `scan_audio_usage`):

```rust
use crate::storage::Db;

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
            db.set_meeting_mic_master_path(&c.meeting_id, None)?;
            used = used.saturating_sub(f);
            summary.freed_bytes += f;
            summary.masters_deleted += 1;
        }
        if used <= limit_bytes {
            break;
        }
        if let Some(p) = c.sys_master_path.as_deref() {
            let f = delete_plaintext(p);
            db.set_meeting_sys_master_path(&c.meeting_id, None)?;
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
            db.set_meeting_audio_path(&c.meeting_id, None)?;
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
    let Some(gb) = limit_gb else {
        return Ok(PruneSummary::default());
    };
    prune_to_limit(db, audio_dir, gb as u64 * BYTES_PER_GB, exclude_meeting)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `(cd src-tauri && cargo test --lib usage::tests)`
Expected: PASS (scan + all 3 prune tests, incl. the locked-exemption guard).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/storage/usage.rs
git commit -m "feat(storage): tiered lock-safe prune engine (masters→playback, oldest-first)"
```

---

### Task 5: Commands `get_storage_report` / `free_up_space` / `reveal_audio_dir` + register

**Files:**
- Modify: `src-tauri/src/commands.rs` (new DTOs + 3 commands)
- Modify: `src-tauri/src/lib.rs` (`generate_handler!` — add the 3)
- Test: in `commands.rs` tests

**Interfaces:**
- Consumes: `crate::pipeline::audio_dir`, `crate::storage::usage::{scan_audio_usage, prune_to_limit, BYTES_PER_GB}`, `AppState.config`, `AppState.db`.
- Produces: `get_storage_report -> StorageReportDto`, `free_up_space -> PruneSummaryDto`, `reveal_audio_dir -> ()`.

- [ ] **Step 1: Write the failing test** — append to `commands.rs` tests (headless — builds a real `AppState` via `AppState::init_at`; no Tauri needed):

```rust
#[test]
fn free_up_space_is_noop_without_a_cap() {
    let p = crate::storage::db::unique_temp_path("murmur-cmd-storage", "sqlite");
    let _ = std::fs::remove_file(&p);
    let state = AppState::init_at(&p, "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef").unwrap();
    // No limit set (default None) → free_up_space must be an inert zero summary.
    let s = crate::storage::usage::prune_to_limit(&state.db, &crate::pipeline::audio_dir().unwrap(), u64::MAX, None).unwrap();
    assert_eq!(s.freed_bytes, 0);
    let _ = std::fs::remove_file(&p);
}
```
(The command wrappers are thin; this asserts the no-cap semantics the wrapper encodes. The live command behavior is exercised by the FE smoke in the verify task.)

- [ ] **Step 2: Run test to verify it fails**

Run: `(cd src-tauri && cargo test --lib free_up_space_is_noop_without_a_cap)`
Expected: FAIL — `pipeline::audio_dir` is private OR (after Task 2) PASS-compiles; if it references not-yet-added command DTOs it fails. Ensure it compiles only after Step 3.

- [ ] **Step 3: Write minimal implementation** — add to `commands.rs` (near other DTOs + commands):

```rust
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageReportDto {
    pub audio_dir: String,
    pub used_bytes: u64,
    pub limit_bytes: Option<u64>,
    pub playback_bytes: u64,
    pub masters_bytes: u64,
    pub sealed_bytes: u64,
    pub recording_count: u64,
    pub auto_prune: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PruneSummaryDto {
    pub freed_bytes: u64,
    pub pruned_count: u64,
    pub masters_deleted: u64,
}

/// Recording-storage usage report: on-disk audio path, byte totals bucketed by category,
/// recording count, and the current cap + auto-prune flag. Sizes only — no content.
#[tauri::command]
pub fn get_storage_report(state: State<'_, AppState>) -> Result<StorageReportDto, AppError> {
    let (limit_bytes, auto_prune) = {
        let c = state.config.lock().map_err(|_| AppError::Config("config mutex poisoned".into()))?;
        (
            c.audio_storage_limit_gb.map(|g| g as u64 * crate::storage::usage::BYTES_PER_GB),
            c.audio_auto_prune,
        )
    };
    let dir = crate::pipeline::audio_dir()?;
    let u = crate::storage::usage::scan_audio_usage(&dir)?;
    Ok(StorageReportDto {
        audio_dir: dir.to_string_lossy().into_owned(),
        used_bytes: u.used_bytes,
        limit_bytes,
        playback_bytes: u.playback_bytes,
        masters_bytes: u.masters_bytes,
        sealed_bytes: u.sealed_bytes,
        recording_count: u.recording_count,
        auto_prune,
    })
}

/// Manual "Free up space": prune oldest recordings to the cap NOW (works even when auto-prune
/// is off). Requires a cap — with none set it is an inert zero summary (the FE disables the
/// button). Never touches notes or locked audio.
#[tauri::command]
pub fn free_up_space(state: State<'_, AppState>) -> Result<PruneSummaryDto, AppError> {
    let limit_bytes = {
        let c = state.config.lock().map_err(|_| AppError::Config("config mutex poisoned".into()))?;
        c.audio_storage_limit_gb.map(|g| g as u64 * crate::storage::usage::BYTES_PER_GB)
    };
    let Some(limit) = limit_bytes else {
        return Ok(PruneSummaryDto { freed_bytes: 0, pruned_count: 0, masters_deleted: 0 });
    };
    let dir = crate::pipeline::audio_dir()?;
    let s = crate::storage::usage::prune_to_limit(&state.db, &dir, limit, None)?;
    Ok(PruneSummaryDto { freed_bytes: s.freed_bytes, pruned_count: s.pruned_count, masters_deleted: s.masters_deleted })
}

/// Reveal the recordings folder in Finder (macOS `open`). No content read.
#[tauri::command]
pub fn reveal_audio_dir() -> Result<(), AppError> {
    let dir = crate::pipeline::audio_dir()?;
    std::process::Command::new("open")
        .arg(&dir)
        .spawn()
        .map_err(|e| AppError::Storage(format!("reveal audio dir: {e}")))?;
    Ok(())
}
```
In `lib.rs` `generate_handler!` (near `get_config`): add `commands::get_storage_report,`, `commands::free_up_space,`, `commands::reveal_audio_dir,`.

- [ ] **Step 4: Run test + build to verify green**

Run: `(cd src-tauri && cargo test --lib free_up_space_is_noop_without_a_cap && cargo build)`
Expected: PASS + build OK.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/commands.rs src-tauri/src/lib.rs
git commit -m "feat(storage): get_storage_report + free_up_space + reveal_audio_dir commands"
```

---

### Task 6: Auto-prune triggers (pipeline after-recording + settings-save) + event

**Files:**
- Modify: `src-tauri/src/events.rs` (new event + payload)
- Modify: `src-tauri/src/pipeline.rs` (`run_inner` — after the masters block)
- Modify: `src-tauri/src/commands.rs` (`save_config` — after `save_config_inner`)
- Test: reuse `maybe_prune` tests (Task 4) — no new unit test (the triggers are thin wiring; the wiring is verified live).

**Interfaces:**
- Consumes: `crate::storage::usage::maybe_prune`, `AppState.config`, `AppState.db`, `crate::pipeline::audio_dir`.
- Produces: `EVENT_STORAGE_PRUNED` + `StoragePrunedPayload { freed_bytes, pruned_count }`.

- [ ] **Step 1: Add the event** — append to `events.rs`:

```rust
/// Emitted after an AUTO-prune removed ≥1 old recording's audio to stay under the storage cap.
/// Counts/bytes ONLY — NO PII. The FE refreshes the usage bar + shows a "freed space" toast.
pub const EVENT_STORAGE_PRUNED: &str = "murmur://storage-pruned";

/// Payload for [`EVENT_STORAGE_PRUNED`]. Bytes + count only — NO PII.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoragePrunedPayload {
    pub freed_bytes: u64,
    pub pruned_count: u64,
}
```

- [ ] **Step 2: Hook the pipeline** — in `pipeline.rs` `run_inner`, immediately AFTER the `if config.keep_hires_masters { … }` masters block (~line 385), add:

```rust
    // Storage retention (opt-in): if the user set a cap + enabled auto-prune, delete the
    // OLDEST recordings' audio to stay under it — never THIS recording (excluded), never a
    // locked folder's, never notes/transcripts. Best-effort: a prune error never fails the
    // recording.
    match crate::storage::usage::maybe_prune(
        &state.db,
        &wav_dir,
        config.audio_storage_limit_gb,
        config.audio_auto_prune,
        Some(meeting_id),
    ) {
        Ok(s) if s.freed_bytes > 0 => {
            tracing::info!(target: "storage", freed = s.freed_bytes, count = s.pruned_count, "auto-pruned old recordings to stay under the storage cap");
            let _ = app.emit(
                crate::events::EVENT_STORAGE_PRUNED,
                crate::events::StoragePrunedPayload { freed_bytes: s.freed_bytes, pruned_count: s.pruned_count },
            );
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(target: "storage", error = %e, "auto-prune failed (non-fatal)"),
    }
```
(`app` is `&AppHandle` and `Emitter` is already in scope via `emit_status`; `wav_dir` is the `let wav_dir = audio_dir()?;` from earlier in `run_inner`.)

- [ ] **Step 3: Hook settings-save** — in `commands.rs` `save_config`, after `restart_voice_listener(app);` and before `Ok(())`:

```rust
    // Enforce the cap immediately when a save leaves auto-prune ON with a cap set (e.g. the
    // user just lowered the limit). Best-effort; the config lock is already released.
    let (limit_gb, auto) = {
        match state.config.lock() {
            Ok(c) => (c.audio_storage_limit_gb, c.audio_auto_prune),
            Err(_) => (None, false),
        }
    };
    if let Ok(dir) = crate::pipeline::audio_dir() {
        if let Ok(s) = crate::storage::usage::maybe_prune(&state.db, &dir, limit_gb, auto, None) {
            if s.freed_bytes > 0 {
                let _ = app.emit(
                    crate::events::EVENT_STORAGE_PRUNED,
                    crate::events::StoragePrunedPayload { freed_bytes: s.freed_bytes, pruned_count: s.pruned_count },
                );
            }
        }
    }
```
(`save_config` already takes `app: AppHandle`; `Emitter` is imported in `commands.rs` — confirm `use tauri::Emitter;` is present, it is used by other emit sites.)

- [ ] **Step 4: Build + full lib tests to verify green**

Run: `(cd src-tauri && cargo test --lib)`
Expected: PASS (no regressions; the prune wiring compiles).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/events.rs src-tauri/src/pipeline.rs src-tauri/src/commands.rs
git commit -m "feat(storage): auto-prune after recording + on settings-save; storage-pruned event"
```

---

### Task 7: FE models + IPC methods

**Files:**
- Modify: `src/app/core/models.ts` (`AppConfigDto` fields + `StorageReport` + `PruneSummary`)
- Modify: `src/app/core/ipc.service.ts` (3 methods)

**Interfaces:**
- Produces: `StorageReport`, `PruneSummary` types; `IpcService.getStorageReport()`, `.freeUpSpace()`, `.revealAudioDir()`; `AppConfigDto.audioStorageLimitGb`, `.audioAutoPrune`.

- [ ] **Step 1: Add the model types** — in `models.ts`, inside `AppConfigDto` (after `postAecEnabled`):

```ts
  /** Recording-storage cap in GB (`null` = no cap). Mirrors Rust `audio_storage_limit_gb`. */
  audioStorageLimitGb: number | null;
  /** Auto-delete oldest recordings' audio over the cap. Opt-in, default false. Mirrors Rust `audio_auto_prune`. */
  audioAutoPrune: boolean;
```
Add near the end of `models.ts`:
```ts
/** Recording-storage usage report (mirrors Rust `StorageReportDto`). Bytes + counts only. */
export interface StorageReport {
  audioDir: string;
  usedBytes: number;
  limitBytes: number | null;
  playbackBytes: number;
  mastersBytes: number;
  sealedBytes: number;
  recordingCount: number;
  autoPrune: boolean;
}

/** Result of a prune / free-up-space run (mirrors Rust `PruneSummaryDto`). */
export interface PruneSummary {
  freedBytes: number;
  prunedCount: number;
  mastersDeleted: number;
}
```

- [ ] **Step 2: Add the IPC methods** — in `ipc.service.ts` (after `saveConfig`), importing `StorageReport`/`PruneSummary` in the existing model-type import:

```ts
  /** Recording-storage usage report (on-disk path, byte totals, cap, auto-prune flag). */
  getStorageReport(): Promise<StorageReport> {
    return invoke<StorageReport>("get_storage_report");
  }

  /** Prune oldest recordings to the cap NOW (no-op with no cap set). Never touches notes/locked audio. */
  freeUpSpace(): Promise<PruneSummary> {
    return invoke<PruneSummary>("free_up_space");
  }

  /** Reveal the recordings folder in Finder. */
  revealAudioDir(): Promise<void> {
    return invoke<void>("reveal_audio_dir");
  }
```

- [ ] **Step 3: Verify build + lint**

Run: `npx ng build && npx ng lint`
Expected: green (types resolve; no unused-import error — ensure `StorageReport`/`PruneSummary` are imported where used).

- [ ] **Step 4: Commit**

```bash
git add src/app/core/models.ts src/app/core/ipc.service.ts
git commit -m "feat(storage): FE models + IPC (getStorageReport / freeUpSpace / revealAudioDir)"
```

---

### Task 8: SettingsStore — storage form fields + report signal + actions

**Files:**
- Modify: `src/app/features/settings/settings.store.ts`

**Interfaces:**
- Consumes: `IpcService.getStorageReport/freeUpSpace/revealAudioDir`, `AppConfigDto`.
- Produces (readonly on the store): `storageReport`, `storageBusy`, `lastFreed`; methods `loadStorageReport()`, `freeUpSpace()`, `revealAudioDir()`; form controls `audioStorageLimitGb` (string), `audioAutoPrune` (boolean).

- [ ] **Step 1: Add form controls** — in the `this.fb.nonNullable.group({ … })` (near `keepHiresMasters`):

```ts
    // Recording-storage cap (GB, string for an empty = "no cap") + opt-in auto-prune.
    audioStorageLimitGb: "",
    audioAutoPrune: false,
```

- [ ] **Step 2: Add report signal + actions** — add fields + methods to the class:

```ts
  import type { StorageReport } from "../../core/models"; // (add to the existing type import)

  private readonly _storageReport = signal<StorageReport | null>(null);
  readonly storageReport = this._storageReport.asReadonly();
  private readonly _storageBusy = signal(false);
  readonly storageBusy = this._storageBusy.asReadonly();
  /** Bytes freed by the last manual "Free up space" (for a confirmation line). */
  private readonly _lastFreed = signal<number | null>(null);
  readonly lastFreed = this._lastFreed.asReadonly();

  async loadStorageReport(): Promise<void> {
    this._storageReport.set(await this.ipc.getStorageReport().catch(() => null));
  }

  async freeUpSpace(): Promise<void> {
    this._storageBusy.set(true);
    this._lastFreed.set(null);
    try {
      const s = await this.ipc.freeUpSpace();
      this._lastFreed.set(s.freedBytes);
      await this.loadStorageReport();
    } finally {
      this._storageBusy.set(false);
    }
  }

  revealAudioDir(): void {
    void this.ipc.revealAudioDir();
  }
```

- [ ] **Step 3: Wire load + save** — in `load()`, add to the `this.form.patchValue({ … })`:
```ts
        audioStorageLimitGb: cfg.audioStorageLimitGb != null ? String(cfg.audioStorageLimitGb) : "",
        audioAutoPrune: cfg.audioAutoPrune ?? false,
```
Still in `load()`, after the other best-effort loads (e.g. after `this._appInfo.set(...)`):
```ts
      await this.loadStorageReport();
```
In `save()`, add to the `cfg: AppConfigDto = { … }` literal:
```ts
      audioStorageLimitGb: v.audioStorageLimitGb.trim() ? Number(v.audioStorageLimitGb) : null,
      audioAutoPrune: v.audioAutoPrune,
```
And after `await this.ipc.saveConfig(cfg);` (inside the try), refresh the report (a save may have pruned):
```ts
      await this.loadStorageReport();
```

- [ ] **Step 4: Verify build + lint**

Run: `npx ng build && npx ng lint`
Expected: green.

- [ ] **Step 5: Commit**

```bash
git add src/app/features/settings/settings.store.ts
git commit -m "feat(storage): SettingsStore storage fields + report signal + free-up/reveal actions"
```

---

### Task 9: Settings "Storage" section component + register in the shell

**Files:**
- Create: `src/app/features/settings/sections/settings-storage-section.component.ts`
- Modify: `src/app/features/settings/settings.component.ts` (`SETTINGS_SECTIONS` + import + `@case` + nav icon)

**Interfaces:**
- Consumes: `SettingsStore` (`form`, `storageReport`, `storageBusy`, `lastFreed`, `freeUpSpace`, `revealAudioDir`, `loadStorageReport`).

- [ ] **Step 1: Create the section component** — mirror `settings-audio-section.component.ts`:

```ts
import {
  ChangeDetectionStrategy,
  Component,
  OnInit,
  computed,
  inject,
} from "@angular/core";
import { ReactiveFormsModule } from "@angular/forms";
import { SettingsStore } from "../settings.store";

/** Settings → Storage: recordings location + usage, the GB cap, opt-in auto-prune, and a
 *  manual "Free up space". Notes/transcripts are never deleted — only audio. */
@Component({
  selector: "app-settings-storage-section",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [ReactiveFormsModule],
  template: `
    <div class="section-stack" [formGroup]="form">
      @if (report(); as r) {
        <!-- Usage summary -->
        <div class="card">
          <div class="usage-head">
            <span class="usage-total">{{ mb(r.usedBytes) }}</span>
            @if (r.limitBytes != null) {
              <span class="text-muted"> / {{ mb(r.limitBytes) }}</span>
            }
          </div>
          @if (r.limitBytes != null) {
            <div class="usage-bar" [attr.data-state]="barState()">
              <span class="usage-fill" [style.width.%]="pct()"></span>
            </div>
          }
          <div class="usage-legend text-secondary">
            {{ r.recordingCount }} recordings ·
            playback {{ mb(r.playbackBytes) }} ·
            masters {{ mb(r.mastersBytes) }} ·
            locked {{ mb(r.sealedBytes) }}
          </div>
          <div class="usage-path">
            <code class="path">{{ r.audioDir }}</code>
            <button type="button" class="btn btn-ghost" (click)="reveal()">Reveal in Finder</button>
          </div>
        </div>
      } @else {
        <div class="card"><p class="text-muted">Loading storage usage…</p></div>
      }

      <!-- Cap (GB) -->
      <div class="card">
        <label class="field">
          <span class="field-label">Storage limit (GB)</span>
          <input
            type="number"
            min="1"
            step="1"
            inputmode="numeric"
            placeholder="No limit"
            formControlName="audioStorageLimitGb"
          />
          <span class="field-help text-muted">
            The most disk your recordings may use. Leave blank for no limit. Notes and
            transcripts are always kept — only audio counts here.
          </span>
        </label>
      </div>

      <!-- Auto-prune toggle -->
      <div class="card">
        <label class="toggle-row">
          <span class="toggle-copy">
            <span class="toggle-title">Automatically delete old recordings</span>
            <span class="text-secondary toggle-sub">
              When over the limit, delete the OLDEST recordings' audio to make room
              (heavy masters first). Your notes and transcripts are never deleted, and
              recordings in locked folders are never touched.
            </span>
          </span>
          <input type="checkbox" formControlName="audioAutoPrune" />
        </label>
      </div>

      <!-- Manual free-up -->
      <div class="card">
        <div class="freeup-row">
          <div class="toggle-copy">
            <span class="toggle-title">Free up space now</span>
            <span class="text-secondary toggle-sub">
              @if (report()?.limitBytes == null) {
                Set a limit above to enable this.
              } @else {
                Delete oldest recordings' audio down to the limit right now. This can't be undone.
              }
            </span>
            @if (lastFreed(); as f) {
              <span class="pill is-success">Freed {{ mb(f) }}</span>
            }
          </div>
          <button
            type="button"
            class="btn"
            [disabled]="storageBusy() || report()?.limitBytes == null"
            (click)="onFreeUp()"
          >
            {{ storageBusy() ? "Freeing…" : "Free up space" }}
          </button>
        </div>
      </div>
    </div>
  `,
  styles: [
    `
      :host { display: contents; }
      .section-stack { display: flex; flex-direction: column; gap: var(--space-5); }
      .field { display: flex; flex-direction: column; gap: var(--space-1); }
      .field-label { color: var(--text-secondary); font-size: 0.9rem; font-weight: 550; }
      .field-help { font-size: 0.8125rem; line-height: 1.5; }
      input[type="number"] {
        width: 8rem; height: 34px; padding: 0 var(--space-3);
        border: 1px solid var(--border); border-radius: var(--radius-md);
        background: var(--surface-input); color: var(--text-primary); font: inherit;
      }
      .usage-head { display: flex; align-items: baseline; gap: var(--space-1); }
      .usage-total { font-size: 1.35rem; font-weight: 650; letter-spacing: -0.01em; }
      .usage-bar {
        margin: var(--space-2) 0; height: 8px; border-radius: var(--radius-pill);
        background: var(--surface-input); overflow: hidden;
      }
      .usage-fill { display: block; height: 100%; background: var(--accent); border-radius: inherit; }
      .usage-bar[data-state="amber"] .usage-fill { background: var(--warning, #d9a441); }
      .usage-bar[data-state="red"] .usage-fill { background: var(--live, #e5484d); }
      .usage-legend { font-size: 0.8125rem; margin-top: var(--space-1); }
      .usage-path { display: flex; align-items: center; gap: var(--space-3); margin-top: var(--space-3); flex-wrap: wrap; }
      .path {
        flex: 1 1 12rem; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
        font-size: 0.8rem; color: var(--text-secondary);
        background: var(--surface-input); padding: var(--space-1) var(--space-2); border-radius: var(--radius-sm);
      }
      .toggle-row { display: flex; align-items: center; justify-content: space-between; gap: var(--space-4); cursor: pointer; }
      .toggle-copy { display: flex; flex-direction: column; gap: var(--space-1); }
      .toggle-title { color: var(--text-primary); font-size: 0.95rem; font-weight: 550; }
      .toggle-sub { font-size: 0.85rem; }
      .freeup-row { display: flex; align-items: center; justify-content: space-between; gap: var(--space-4); }
    `,
  ],
})
export class SettingsStorageSectionComponent implements OnInit {
  private readonly store = inject(SettingsStore);
  readonly form = this.store.form;
  readonly report = this.store.storageReport;
  readonly storageBusy = this.store.storageBusy;
  readonly lastFreed = this.store.lastFreed;

  /** % fill of the cap bar (0..100, clamped). */
  readonly pct = computed(() => {
    const r = this.report();
    if (!r || r.limitBytes == null || r.limitBytes === 0) return 0;
    return Math.min(100, Math.round((r.usedBytes / r.limitBytes) * 100));
  });
  /** Bar color state by fill. */
  readonly barState = computed(() => {
    const p = this.pct();
    return p >= 95 ? "red" : p >= 75 ? "amber" : "ok";
  });

  ngOnInit(): void {
    // Ensure the report is fresh when the section mounts (load() already fetched it once).
    void this.store.loadStorageReport();
  }

  /** Human MB/GB label (binary). */
  mb(bytes: number): string {
    if (bytes >= 1024 * 1024 * 1024) return (bytes / (1024 * 1024 * 1024)).toFixed(2) + " GB";
    return Math.round(bytes / (1024 * 1024)) + " MB";
  }

  reveal(): void {
    this.store.revealAudioDir();
  }

  onFreeUp(): void {
    if (!confirm("Delete oldest recordings' audio to free up space? Notes are kept. This can't be undone.")) return;
    void this.store.freeUpSpace();
  }
}
```
(Note: `confirm()` is a native dialog. Per the browser-automation guidance dialogs block automation, but this is a real app affordance, acceptable here; the Playwright smoke will stub `window.confirm` to return true.)

- [ ] **Step 2: Register in the shell** — in `settings.component.ts`:
  - Add to `SETTINGS_SECTIONS` after the `audio` entry:
    ```ts
    { id: "storage", label: "Storage", keywords: "disk space usage recordings audio size limit cap gb delete old cleanup prune free up finder location" },
    ```
  - Add the import:
    ```ts
    import { SettingsStorageSectionComponent } from "./sections/settings-storage-section.component";
    ```
  - Add `SettingsStorageSectionComponent` to the `imports:` array.
  - Add the nav icon `@case ("storage")` inside the `@switch (s.id)` block (a simple disk glyph):
    ```html
    @case ("storage") {
      <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><rect x="2.5" y="3" width="11" height="10" rx="1.6" /><path d="M2.5 6.5h11M5 9.5h.01M5 11.3h3" /></svg>
    }
    ```
  - Add the content `@case`:
    ```html
    @case ("storage") {
      <app-settings-storage-section />
    }
    ```

- [ ] **Step 3: Verify build + lint**

Run: `npx ng build && npx ng lint`
Expected: green, including the 16 kB per-component style budget.

- [ ] **Step 4: Commit**

```bash
git add src/app/features/settings/sections/settings-storage-section.component.ts src/app/features/settings/settings.component.ts
git commit -m "feat(storage): Settings → Storage section (usage, cap, auto-prune, free up space)"
```

---

### Task 10: Library header usage bar

**Files:**
- Modify: `src/app/features/library/library.component.ts` (template header + class signal + `ngOnInit`)

**Interfaces:**
- Consumes: `IpcService.getStorageReport`.

- [ ] **Step 1: Add the report signal + load** — in `LibraryComponent`, near `readonly meetings = signal<Meeting[]>([]);`:

```ts
  import type { StorageReport } from "../../core/models"; // add to existing type import
  readonly storageReport = signal<StorageReport | null>(null);
  readonly storagePct = computed(() => {
    const r = this.storageReport();
    if (!r || r.limitBytes == null || r.limitBytes === 0) return 0;
    return Math.min(100, Math.round((r.usedBytes / r.limitBytes) * 100));
  });
```
In `ngOnInit()`, alongside the existing `this.ipc.listMeetings()` load, add a best-effort:
```ts
    this.ipc.getStorageReport().then((r) => this.storageReport.set(r)).catch(() => {});
```

- [ ] **Step 2: Add the bar to the header** — in the no-query `<header class="library-head">` block (~line 289), after the `<span class="count">` line, add a compact bar (only when a cap is set):

```html
          @if (storageReport(); as sr) {
            @if (sr.limitBytes != null) {
              <a class="storage-chip" routerLink="/settings" [attr.data-state]="storagePct() >= 95 ? 'red' : storagePct() >= 75 ? 'amber' : 'ok'" title="Manage recording storage">
                <span class="storage-fill" [style.width.%]="storagePct()"></span>
                <span class="storage-label">{{ mb(sr.usedBytes) }} / {{ mb(sr.limitBytes) }}</span>
              </a>
            }
          }
```
Add the `mb()` helper method to the class (same as Task 9):
```ts
  mb(bytes: number): string {
    if (bytes >= 1024 * 1024 * 1024) return (bytes / (1024 * 1024 * 1024)).toFixed(2) + " GB";
    return Math.round(bytes / (1024 * 1024)) + " MB";
  }
```
Add styles (in the component's `styles`, near `.library-head`):
```css
      .storage-chip {
        position: relative; display: inline-flex; align-items: center; overflow: hidden;
        margin-left: auto; padding: 3px var(--space-2); border-radius: var(--radius-pill);
        background: var(--surface-input); color: var(--text-secondary);
        font-size: 0.75rem; text-decoration: none; min-width: 8rem;
      }
      .storage-fill { position: absolute; inset: 0 auto 0 0; background: var(--accent-soft); border-radius: inherit; }
      .storage-chip[data-state="amber"] .storage-fill { background: color-mix(in srgb, var(--warning, #d9a441) 30%, transparent); }
      .storage-chip[data-state="red"] .storage-fill { background: color-mix(in srgb, var(--live, #e5484d) 30%, transparent); }
      .storage-label { position: relative; }
```
Ensure `RouterLink` is already imported (it is — `imports: [RouterLink, …]`).

- [ ] **Step 3: Verify build + lint**

Run: `npx ng build && npx ng lint`
Expected: green.

- [ ] **Step 4: Commit**

```bash
git add src/app/features/library/library.component.ts
git commit -m "feat(storage): Library header recording-storage usage bar"
```

---

### Task 11: Library "audio freed" row chip

**Files:**
- Modify: `src/app/features/library/library.component.ts` (meeting-row template + a small method)

**Interfaces:**
- Consumes: `Meeting.audioPath`, `Meeting.status`.

- [ ] **Step 1: Add the derivation method** — in `LibraryComponent`:

```ts
  /** A finalized recording whose audio was freed to save space (audio gone, note kept).
   *  A locked meeting's path points at its `.enc` (non-null), so this is prune-specific. */
  isAudioFreed(m: Meeting): boolean {
    return m.audioPath === null && m.status !== "ERROR";
  }
```

- [ ] **Step 2: Add the chip to the meeting row** — locate the `@for` that renders the meetings list rows (grep the template for `displayedMeetings()` used in a `@for (m of …)` — the meeting `<li>`/row). Inside a row, next to the meeting's date/meta, add:

```html
              @if (isAudioFreed(m)) {
                <span class="pill audio-freed" title="Recording audio was freed to save space — the note is kept">audio freed</span>
              }
```
Add a minimal style (reuse the global `.pill`; add only a muted tint):
```css
      .audio-freed { font-size: 0.7rem; color: var(--text-muted); }
```

- [ ] **Step 3: Verify build + lint**

Run: `npx ng build && npx ng lint`
Expected: green.

- [ ] **Step 4: Commit**

```bash
git add src/app/features/library/library.component.ts
git commit -m "feat(storage): 'audio freed' chip on pruned meeting rows"
```

---

### Task 12: Full gate + adversarial verify + lock-security review

**Files:** none (verification only).

- [ ] **Step 1: Run the full local gate**

Run: `bash scripts/ci.sh`
Expected: green (clippy -D warnings + `cargo test --lib` + `ng lint` + `ng build` + headless E2E). Fix anything red, re-run.

- [ ] **Step 2: Live smoke (mocked Tauri invoke)**

Boot the dev app OR drive `http://localhost:1420` via Playwright with a mocked `window.__TAURI_INTERNALS__.invoke` that returns a `StorageReport` (used 1.4 GB / cap 2 GB) for `get_storage_report` and a `PruneSummary` for `free_up_space`, and stubs `window.confirm` → true. Verify:
  - Settings → **Storage** section renders: usage bar, GB input, auto-prune toggle, "Free up space" (disabled with no cap), "Reveal in Finder".
  - Library header shows the usage chip when a cap is set; color shifts amber ≥75%, red ≥95%.
  - Setting a limit + Save round-trips (getConfig echoes `audioStorageLimitGb`/`audioAutoPrune`).

- [ ] **Step 3: Adversarial-verifier** — dispatch the `adversarial-verifier` agent (pin it to the absolute main worktree path) to TRY TO BREAK the change: run the real gates; hunt content-loss (does a prune ever delete a note/segment/timeline? — it must not), sealed-content leak (`get_storage_report` must expose no titles/content; `free_up_space` must skip locked), FFI/NG0600/import-cycle/opacity regressions. It owns PASS/FAIL.

- [ ] **Step 4: lock-security-reviewer (REQUIRED)** — dispatch the `lock-security-reviewer` agent. Audit: auto-prune excludes every `locked=1` folder (never deletes a `.enc`); `delete_plaintext` refuses `.enc`; no new ungated content read (report = sizes/counts only); no PII in the new logs; the seal/reconcile invariants are untouched. It must PASS before merge.

- [ ] **Step 5: Address findings** — fix any RED-before-GREEN gaps or leaks the verifiers surface, re-run Step 1, re-request the two verdicts. Only proceed to the PR when both PASS.

---

### Task 13: PR to trunk

**Files:** none.

- [ ] **Step 1: Push the branch**

```bash
git push -u origin feat/recording-storage-mgmt
```

- [ ] **Step 2: Open the PR** (QueaT author, no Claude trailers, `-R murmur-io/murmur`):

```bash
gh pr create -R murmur-io/murmur --base murmur --head feat/recording-storage-mgmt \
  --title "feat(storage): recording storage management (usage + cap + opt-in auto-prune)" \
  --body "Adds a Storage settings section + Library usage bar. Size cap (GB) + opt-in auto-prune of the OLDEST recordings' audio (masters→playback, oldest-first), never notes/transcripts, never locked audio. Backend: usage.rs disk scan + prune engine + get_storage_report/free_up_space/reveal_audio_dir. Spec: docs/superpowers/specs/2026-07-04-recording-storage-management-design.md. adversarial + lock-security PASS."
```

- [ ] **Step 3: Merge via PR** (never direct-push to `murmur`): `gh pr merge` once CI + reviews are green.

---

## Self-Review

**1. Spec coverage:**
- "See where recordings live + usage" → Task 5 (`get_storage_report`.audioDir + buckets) + Task 9 (path + Reveal) + Task 10 (bar). ✓
- "Cap in GB" → Task 1 (`audio_storage_limit_gb`) + Task 9 (GB input). ✓
- "Auto-remove oldest, never notes" → Task 3 (candidates) + Task 4 (tiered engine, context-preservation test) + Task 6 (triggers). ✓
- "Never locked audio" → Task 3 (SQL exclusion) + Task 4 (`prune_never_touches_locked_folder_audio` RED-before-GREEN). ✓
- "Progress bar in meetings" → Task 10. ✓
- "New Settings section + strategy" → Task 9. ✓
- "Manual free-up / reveal" → Task 5 + Task 8 + Task 9. ✓
- Spec deviations (documented): NO `audio_pruned_at` column — the "audio freed" chip derives from `audioPath === null` (Task 11), so NO migration. Startup-prune trigger replaced by save-config prune (Task 6) — strictly better UX (enforces on limit change). Both are scope reductions, not additions.

**2. Placeholder scan:** No "TBD"/"add error handling"/"similar to Task N" — every code step carries the real code; every test step carries real assertions. ✓

**3. Type consistency:** `AudioUsage`/`PruneSummary`/`PrunableAudio`/`StorageReportDto`/`PruneSummaryDto` names match across producer/consumer tasks; FE `StorageReport`/`PruneSummary` camelCase mirror the Rust `#[serde(rename_all="camelCase")]` DTOs; `audioStorageLimitGb`/`audioAutoPrune` consistent FE↔BE; `scan_audio_usage`/`prune_to_limit`/`maybe_prune`/`prunable_audio_candidates` used with identical signatures everywhere. ✓
