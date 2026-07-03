# Recording Storage Management — Design Spec

**Date:** 2026-07-04
**Status:** Approved (brainstorming) → ready for implementation plan
**Branch:** `feat/recording-storage-mgmt`

## 1. Problem & goal

Recordings (audio WAVs, optional hi-res masters, sealed `.enc`) are the heavy part of
Murmur's on-disk footprint and grow without bound — there is today **no** size accounting
and **no** retention. Notes/transcripts/timelines are light and are the durable value.

The user wants to:

1. **See** where recordings are stored on disk and how much space they use.
2. **Cap** the audio-on-disk footprint (e.g. 2 GB).
3. **Auto-remove the oldest recordings** when over the cap — **never** notes, transcripts,
   or timelines, so cross-meeting context is never lost.
4. See a **usage progress bar in the meetings view** and configure the cap + strategy in a
   **new Settings section**.

The load-bearing guarantee: **"NOTES ARE LIGHT, RECORDINGS ARE NOT — never lose context."**

## 2. Decisions (locked in brainstorming)

| Question | Decision |
| --- | --- |
| Change the storage folder? | **No (v1).** Only *show* the path + usage + "Reveal in Finder". Custom-location is a heavier, separate effort (file migration + `asset://` scope + lock reconcile) — explicitly out of scope. |
| Enforcement | **Auto-prune, opt-in, default OFF.** When enabled and over cap, delete oldest audio (never notes), with a toast + a UI badge. A manual "Free up space" action is always available. |
| Limit dimension | **Size cap in GB** (e.g. 2 GB). No age / keep-last-N in v1. |
| Locked (Touch-ID-sealed) recordings | **Exempt.** Auto-prune never touches a locked folder's audio (`.enc`); it counts toward usage but is never auto-deleted. Only manual per-meeting delete can remove it. |
| Tiered deletion | **Yes.** Delete heavy hi-res masters (`.mic.wav`/`.sys.wav`) first; only if still over cap, delete the playback `{id}.wav`. Max space freed, least loss of playback. |
| Prune trigger | After each recording finalizes + once at startup (best-effort). **No background timer.** |
| Prune annotation | **UI badge + `audio_pruned_at` DB flag only.** Do **not** inject text into the exported Obsidian `.md` (keep owned files clean). |

## 3. Architecture choice

**Stateless disk scan for accounting, not a cached running total.**

At Murmur's scale (hundreds of audio files) a directory walk with `fs::metadata` is instant
and **always accurate** — no drift, no running-total maintenance on every write/delete. The
only schema change is a single **additive** column `meetings.audio_pruned_at` (nullable TEXT)
used purely for the UI "audio freed" badge. This honors the additive-migration-only rule and
adds no reconciliation surface.

## 4. Components

### 4.1 Backend (Rust, `src-tauri/src`)

**New module `storage/usage.rs`**

- `fn audio_storage_report(state) -> Result<StorageReport>`:
  - Resolves the audio dir the same way the pipeline does (`pipeline::audio_dir` →
    `dirs::data_dir()/app_dir_name()/audio`).
  - Walks the dir, sums bytes, bucketed by category:
    - `playback_bytes` — `{id}.wav`
    - `masters_bytes` — `{id}.mic.wav` + `{id}.sys.wav`
    - `sealed_bytes` — `*.enc`
  - Counts distinct recordings; derives `used_bytes = sum`, `limit_bytes` from config.
  - Maps files → meetings via `meetings.audio_path` / `mic_master_path` / `sys_master_path`
    to compute `recording_count` and (for the report) oldest recording date.
  - **No content.** Sizes, counts, categories, `started_at` only — never note/transcript
    text. Titles for locked meetings are masked the same way the detail DTO masks them
    (or omitted); the report is size/telemetry, not a content read.

```rust
pub struct StorageReport {
    pub audio_dir: String,          // absolute path, for "Reveal in Finder"
    pub used_bytes: u64,
    pub limit_bytes: Option<u64>,   // None = no cap
    pub playback_bytes: u64,
    pub masters_bytes: u64,
    pub sealed_bytes: u64,          // locked/exempt; shown but never auto-pruned
    pub recording_count: u64,
    pub auto_prune: bool,
}
```

**Prune engine** `fn prune_audio_to_limit(state, reason: PruneReason) -> Result<PruneSummary>`

- If `auto_prune == false` **and** `reason != Manual` → no-op (returns zero summary).
- Compute usage. If `used_bytes <= limit` → no-op.
- Candidate meetings: **ordered `started_at ASC` (oldest first)**, **excluding any meeting
  whose folder is locked and not session-unlocked** (reuse the lock check the commands use —
  `meeting_is_unlocked` / folder lock state). Locked → skipped entirely; `.enc` never touched.
- Do **not** prune the audio of a recording currently in progress (a prune triggered by
  finalization runs after the file is closed; guard against the active meeting id anyway).
- **Tiered passes** until `used_bytes <= limit` or candidates exhausted:
  1. Pass 1 — delete hi-res masters (`.mic.wav`, `.sys.wav`) oldest-first; null
     `mic_master_path` / `sys_master_path`.
  2. Pass 2 — delete playback `{id}.wav` oldest-first; null `audio_path`; stamp
     `meetings.audio_pruned_at = now`.
- Each deletion is best-effort on the filesystem (`let _ = fs::remove_file`) but the DB
  column update is checked. Never delete a plaintext WAV whose `.enc` should exist — moot
  here because locked folders are skipped, but assert it (a plaintext WAV in a locked folder
  is a reconcile race → skip, don't delete).
- Returns `PruneSummary { freed_bytes, pruned_meeting_ids, masters_deleted, playback_deleted }`.
- Emits a typed event (`events.rs`) so the FE usage bar refreshes.

**New Tauri commands** (in `commands.rs`, registered in `lib.rs` `generate_handler!`):

- `get_storage_report() -> StorageReportDto`
- `free_up_space() -> PruneSummaryDto` — manual "Free up space" button; runs
  `prune_audio_to_limit(Manual)` once, respecting the same cap + locked-exemption + tiering.
  `Manual` bypasses the `auto_prune` OFF check (the button works even when auto is disabled),
  but **still requires a cap**: if `audio_storage_limit_gb` is `None` there is no target to
  prune to → the command is a no-op returning a zero summary, and the FE disables the button
  with a "set a limit first" hint.
- (auto path is internal, not a command.)

**Config** (`settings/config.rs`) — extend `AppConfig` via the existing key/load/save pattern
and mirror into `AppConfigDto`:

- `audio_storage_limit_gb: Option<u32>` — `None` = no cap. Key `audio_storage_limit_gb`.
- `audio_auto_prune: bool` — default `false`. Key `audio_auto_prune`.

**Trigger points:**

- End of `pipeline::run_inner` (after `finalize_meeting`, once the WAV is closed): call
  `prune_audio_to_limit(AfterRecording)`.
- App setup/startup (once): call `prune_audio_to_limit(Startup)` **best-effort, non-fatal** —
  wrapped so a prune error can never abort launch (startup-must-never-crash rule).
- Both no-op when `auto_prune` is off.

**Migration** (`storage/db.rs` `Db::migrate`): `add_column_if_missing("meetings",
"audio_pruned_at", "TEXT")`. Idempotent, additive, no destructive ops.

### 4.2 Frontend (Angular 18 zoneless, `src/app`)

**New Settings section "Storage" (Pamięć)** — `features/settings/sections/settings-storage-section.component.ts`:

- Add `{ id: "storage", label: "Pamięć / Storage", keywords: "…" }` to `SETTINGS_SECTIONS`,
  after "Audio & Capture"; import + `@case ("storage")` in the settings shell.
- Shows: storage path (read-only) + "Reveal in Finder"; usage breakdown
  (playback / masters / sealed) as labeled bars; the **limit field (GB)**; the **auto-prune
  toggle**; a **"Free up space now"** button (confirm dialog — deletion is irreversible).
- Reads `getStorageReport()` into a signal; writes limit/toggle via `saveConfig`.

**Library header usage bar** — in `features/library/library.component.ts` header/toolbar:

- Compact progress bar: `"1.4 GB / 2 GB"` + recording count. Color by fill: green `< 75%`,
  amber `75–95%`, red `≥ 95%`/over. Small "Manage" link → Storage settings section.
- Same `getStorageReport()` signal; refreshes on the prune event and after a recording.
- When no cap is set: show usage only, no fill/warning.

**Meeting row badge** — when `audio_pruned_at` is set (surfaced on the meeting DTO): a subtle
"audio freed" chip and a disabled play control. Note/transcript render normally.

**IPC** (`core/ipc.service.ts` + `core/models.ts`): add `getStorageReport(): Promise<StorageReport>`
and `freeUpSpace(): Promise<PruneSummary>`; declare `StorageReport` / `PruneSummary` types.
One typed method per command; result lands in a signal (no `.subscribe`, no `async` pipe).

## 5. Data flow

1. User sets `2 GB` + toggles auto ON in Settings → `saveConfig` → `settings` table.
2. A recording finishes (or app starts): backend `prune_audio_to_limit` computes usage; if
   over cap, deletes oldest masters then oldest playback WAVs (skipping locked), nulls the
   path columns, stamps `audio_pruned_at`, emits the prune event.
3. FE library bar + settings usage + meeting-row badges reflect the new state via the signal.
4. `notes` / `segments` / `timelines` are never read or written by any of this.

## 6. Safety & invariants (binding rules honored)

- **Never lose context:** the prune engine touches **only** audio files and the audio path
  columns. `notes`/`segments`/`timelines` are never queried or mutated. A test asserts note +
  transcript + timeline still read back byte-identical after a prune.
- **Lock model:** auto-prune **skips locked folders entirely** → `.enc` is never deleted,
  never races `seal`/`reconcile_locked_at_rest`. Manual per-meeting delete stays as the
  existing `delete_meeting` path. No new **content-read** path is added — the report exposes
  sizes/counts, not content; locked titles are masked. (`lock-security-reviewer` is a required
  gate.)
- **Additive migration only** (`add_column_if_missing`); no `DROP`/`DELETE`/rewrite.
- **Irreversible-action honesty:** auto-prune is OFF by default; manual delete is confirmed;
  every deletion surfaces a toast + a persistent badge — never silent.
- **Startup never crashes:** the startup prune is best-effort and wrapped; a prune failure can
  never abort launch or touch the DB open path.
- **No PII in logs:** prune/usage logs carry meeting IDs, byte counts, stage names — never
  titles, note text, or paths embedding personal content.
- **Errors:** every fallible fn returns `crate::error::Result<T>` with the right `AppError`
  variant (`Storage` for FS/DB, `Locked` for a refusal); no `unwrap`/`expect` in non-test code.

## 7. Testing (Definition of Done)

**Rust (`cargo test --lib`):**

- Usage summation over a temp audio dir (playback/masters/sealed buckets correct).
- Prune ordering: oldest `started_at` deleted first.
- **Locked-folder exemption (RED-before-GREEN):** a locked meeting's audio is NOT deleted even
  when far over cap; the guard test fails on a naive implementation and passes with the skip.
- Tiered: masters deleted before playback; stops as soon as `used <= limit`.
- **Context preserved:** after a prune, the pruned meetings' note/segments/timeline still read
  back intact; `audio_pruned_at` is stamped and `audio_path` nulled.
- Idempotent no-op when under cap or when `auto_prune` is off.

**Frontend:** `npx ng lint` + `npx ng build` green (incl. 16 kB style budget). Playwright
smoke against a mocked `window.__TAURI_INTERNALS__.invoke` for the library bar states
(under/near/over cap) + the settings section (set limit, toggle, "Free up space" confirm).

**Ownership of the verdict:** implementer self-checks but does **not** self-certify. An
independent **adversarial-verifier** owns PASS/FAIL; the **lock-security-reviewer** is a
required second gate (touches audio deletion + visibility). Full gate `scripts/ci.sh` at the
end.

## 8. Out of scope (YAGNI, v1)

- Changing/relocating the storage folder (custom directory, external drive).
- Age-based retention ("older than N days") and "keep last N recordings".
- Audio compression / cloud offload / re-download of pruned audio.
- Any change to notes/transcripts/timelines or their retention.

## 9. Open items for the implementation plan

- Exact reuse of the folder-lock check inside the prune candidate query (mirror
  `meeting_is_unlocked` / the folder lock join used by `visibility_clause`).
- Whether the meeting-row `audio_pruned_at` badge needs a new field on the existing meeting
  list DTO or can piggyback on `audio_path == null`.
- Event name/shape for the prune-completed FE refresh (extend `events.rs`).
