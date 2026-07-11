# Remediation plan — unreleased audit v0.8.0..HEAD (2026-07-11)

Source audit: `docs/research/2026-07-11-unreleased-audit-v0.8.0-HEAD.md`. Every item below has a
root cause traced to a symbol, a concrete fix, a RED→GREEN verification, and the owning
implementer + required verifier. Lock/crypto items are gated by `lock-security-reviewer`; every
item's PASS/FAIL is owned by `adversarial-verifier`, never the implementer.

Execution is autonomous, in dependency-ordered waves, on a feature branch → PR → merge to `murmur`.

## Live repro added to scope — org creation 404 (`org-put-key-grants: rejected (404)`)

**Root cause (traced end-to-end, = SB-1):** `org_create_inner` (commands.rs) sends the owner
self-grant with `user_id: account_id.clone()`, and `account_id` is the **email**
(`account_login`: `acct_id = email.trim()`). The server `put_key_grants` does
`let target = parse_org_id(&g.user_id)?` where `parse_org_id = Uuid::parse_str(..).map_err(NotFound)`
→ parsing an email as a UUID fails → uniform **404**. The server half is already fixed
(`murmur-io/murmur-server` commit `b9b08a4`: `LoginFinishResponse.user_id` now returned) but (a)
the client never consumes it and (b) the deployed Railway server may predate it. This is the same
bug as SB-1 and is fixed together.

---

## Wave 1 — Lock / seal blockers (backend, `rust-tauri-dev`; gate: lock-security-reviewer + adversarial-verifier)

All in `src-tauri/src/commands.rs` / `storage/db.rs`; serialized (same files).

### NOTES-1 (CRITICAL) — `delete_note_folder` deletes authored notes; UI promises reparent
- Fix: `delete_folder_inner` must reparent `documents(kind='note')` rows to the default note-folder
  before folder deletion (mirror the `notes`-table reparent), OR the FE must stop promising a move.
  Chosen: **reparent authored notes** (matches the UI copy + the notes.service doc) — move
  `documents.folder_id` to `ensure_default_note_folder()` and rewrite `exported_path`, then delete
  the empty folder. Never `DELETE FROM documents` on folder delete.
- RED: unit test — create a note in folder F, `delete_note_folder(F)`, assert the note row still
  exists under the default folder and its `.md` moved (not deleted).

### NOTES-2 (CRITICAL) — note-folder move/rename strands `documents.exported_path` → plaintext `.md` survives lock
- Fix: `reparent_note_folder_paths` / `Db::reparent_note_folder` must rewrite each contained
  `documents.exported_path` to the new vault path (the physical `fs::rename` already moves the files;
  the DB path must follow) so `lock_folder_inner`'s plaintext-`.md` cleanup deletes the REAL file.
- RED: unit test — note in a locked-then-unlocked folder, move the folder, lock it, assert no
  plaintext `.md` remains at either the old or new path and the DB `exported_path` matches disk.

### NOTES-3 (MAJOR) — `Db::reparent_note_folder` byte-vs-char `substr` corrupts non-ASCII paths
- Fix: stop passing Rust `old_path.len()+1` (bytes) into SQLite `substr` (chars). Either compute the
  char count (`old_path.chars().count()+1`) or rewrite descendants with a bound-parameter
  string-concat that doesn't rely on `substr` offsets. Prefer the char-count fix, guarded by a test.
- RED: unit test — folder "Notes/Sprzedaż" with a child, rename parent, assert the child path is
  `new || '/' || 'Child'` (leading slash intact), for a multi-byte name.

### SEAM-F1 (MAJOR) — `generate_timeline` leaves plaintext timeline at rest after relock
- Fix: bring `generate_timeline` under the #229/#234 seal discipline: acquire `lifecycle_guard`,
  re-check the seal epoch across the provider `.await`, and if the folder is locked (session-unlocked)
  seal the generated timeline under the folder CK via the existing `seal_timeline` path instead of a
  bare `set_timeline_data`. If a seal landed mid-generation, discard the plaintext (don't persist).
- RED: unit/integration test — session-unlock a locked folder, generate a timeline, relock, assert
  the `timeline` row has an encrypted `data_blob` and no plaintext `data`.

### SEAM-F2 (MAJOR) — `rename_speaker` edit destroyed on relock
- Fix: route the renamed-timeline write through the seal-on-write helper (reseal under the cached CK
  when the folder is session-unlocked-locked), mirroring `upsert_note_reseal_if_locked`.
- RED: unit test — rename a speaker in a session-unlocked locked folder, relock, unlock, assert the
  rename persisted (round-trips through the sealed blob).

### LOCK-SHARE-INGEST-1 (MAJOR) — share accept into a locked folder writes plaintext that survives relock
- Fix: `ingest_shared_note` must route its note write through `upsert_note_reseal_if_locked` (not the
  raw `Db::upsert_note`) and under `lifecycle_guard`, and NOT write a plaintext `.md` into a
  session-unlocked locked target — seal it like every other write path.
- RED: unit test — accept a share into a session-unlocked locked folder, relock, assert no plaintext
  note row/`.md` at rest (only the sealed blob).

---

## Wave 2 — Shared Brain crypto + server (backend + server; gate: lock-security-reviewer + adversarial-verifier)

### SB-1 (MAJOR, = live 404) — key grants on the server `user_id`, not the email
- Client fix (does NOT touch `account_id`, which stays the email for MK/identity AAD):
  1. `LoginFinishResponse` (client DTO in `share/`) gains `user_id: Option<String>`.
  2. `AccountSession` + `PersistedTokens` gain `server_user_id: String`; persisted to Keychain and
     restored on biometric/restart (fallback: empty → forces a re-login before org ops, never the
     email).
  3. `account_login` stores `finish.user_id`; `require_session_mk` (or a new accessor) exposes it.
  4. `org_create_inner`: grant `user_id` = `server_user_id`; wrap recipient + `grant_sig`
     recipient_acct_id = `server_user_id`.
  5. `acquire_org_ock`: match `g.user_id == server_user_id` and reconstruct the signed view with
     recipient = `server_user_id` (+ correct generation).
  6. `org_invite_member_inner`: confirm the grant target + signed recipient use the added member's
     server `user_id` (AddMemberResponse.user_id, already a UUID) consistently with open.
- Server: `b9b08a4` already returns `user_id`; **deploy it to Railway** (deploy-murmur-server skill)
  and confirm `/v1/auth/login/finish` returns `userId`.
- RED: (a) a client unit test asserting the KeyGrantInput.user_id is a UUID (server_user_id), not an
  email; (b) a crypto round-trip test — owner wraps→PUT grant keyed on user_id, a second session
  (same user_id, fresh MK-from-login) GETs + opens the OCK successfully.
- MANUAL E2E (real server): create org "Test" succeeds (no 404); re-login recovers the OCK.

### SB-3 (MAJOR) — `org_sweep_pending` retry row amplification
- Fix: retry must reuse the existing queued/failed `org_shares` row (update in place / dedupe on the
  logical share key) instead of `insert_org_share` minting a fresh row each attempt; only mark the
  prior row revoked on success of a REPLACEMENT, not on every retry.
- RED: unit test — a share whose upload fails N times yields exactly ONE row (state failed), and a
  later success flips that row to uploaded without a duplicate publish.

### SB-2 (MAJOR) — org provenance unwired (`SourceOrigin::org()` zero call sites)
- Decision: SB-2 is a missing-feature, not a leak. Two honest options: (a) wire org origin into the
  Ask `VaultSource` build so the chips reflect reality, or (b) remove the dead `SourceOrigin::org`/
  `OrgItemSummary`/`Db::org_item_count` + the misleading docs. Choose (b) for this remediation
  (smallest correct change; wiring real org provenance is a feature, deferred) — delete dead code +
  fix the doc claims so nothing lies. Re-evaluate during Wave 2 with the code in front of us.

### org_leave replica purge + MCP `org_search` gate (MAJOR-ish, leak/consent)
- Fix: `org_leave` must purge the decrypted `org_items`/`org_chunks` replica (and vectors) for the
  left org. MCP `org_search` + the `org_brain_search` agent tool must gate on `org_brain_available`
  (membership + consent) so a departed/un-consented user can't search org content.
- RED: unit test — after `org_leave`, `org_search`/`org_brain_search` return nothing and the replica
  rows are gone.

---

## Wave 3 — Non-lock backend + red E2E + FE (parallel where disjoint)

### TP-F1 (MAJOR, fresh-install regression) — turbo default arms voice-command with no live loop
- Fix (backend): `begin_voice_command_inner` must refuse/duck-out when there is no live loop
  (`live_model` is None and no live consumer), OR `start_recording`'s heavy-model arm must guarantee
  a live consumer (spawn the pinned `small` live loop, not just `spawn_live_pin_download`). Prefer:
  arm the voice command only when a live transcription consumer exists; otherwise surface a clean
  "voice needs the live model" state (no wedge). Verify against the shipped fresh-install default
  (`default_model_size_now` → turbo on ≥12 GB).
- RED: unit test — fresh-install config (turbo heavy, no pinned small), start recording, assert the
  voice-command path does not arm a consumer-less generation (no wedge / clean unavailable).

### RED-E2E-1 — `timeline-defer.spec.ts` infinite loop (`deriveTimeline` unvalidated set)
- Fix (FE): `deriveTimeline` / `loadTimeline` must treat a falsy/empty `generate_timeline` result as
  a terminal state (set `timelineNeedsGeneration` or `timelineError`), never leave
  `timeline==null && !error && !needsGeneration` which re-fires `_timelineOnAudioTab` forever. Also
  add a one-shot latch so the Audio-tab effect cannot re-enter for the same meeting after a resolved
  empty read.
- RED: the existing `e2e/detail/timeline-defer.spec.ts` must go green (fires exactly once on Audio).

### MEM-1 (MAJOR) — memory-import "delete to undo" leaves superseded facts closed
- Fix (backend): either (a) `import_memories_inner` records the invalidated fact ids so
  `delete_meeting` on the synthetic import can reopen them, or (b) scope the reconcile so a superseding
  import does not permanently close facts anchored to other meetings (soft-supersede that delete
  reverses). Prefer (a): stamp a reversible link (import meeting id ↔ invalidated fact ids) and reopen
  on delete.
- RED: unit test — import that supersedes an existing fact, delete the import meeting, assert the
  pre-existing fact is reopened (open row restored).

### PK-F1 (MAJOR) — Notes-section lock bypasses the lock×shares dialog
- Fix (FE): `NotesHomeComponent.lockFolder` must run the same `folderActiveShares` probe +
  warn/revoke flow as `FolderRowComponent.onLock` before calling `lockFolder` (extract the shared
  flow so both call sites use it; don't duplicate). Fail-CLOSED on a probe error (warn, don't lock
  silently) — also fixes minor F5.
- RED: E2E — mock `folder_active_shares` to return a share; lock from the Notes section; assert the
  warn/revoke dialog appears before `lock_folder` is invoked.

### WT-F1 (MAJOR, working tree) — selection bubble re-floats after Accept; own e2e assertion fails
- Fix (FE): suppress the programmatic `select` event that `replaceRange`'s `setSelectionRange`
  queues — e.g. set a `suppressNextSelect` flag consumed by `onBodySelect`, or collapse the
  selection after applying (place the caret at the end instead of selecting the inserted text). Keep
  the new `brain-popover.spec.ts` assertion (`bubble toHaveCount(0)` after Accept) green.
- Also fold in: restore reachability of `codeblock`/`divider` FormatOps (or delete the dead cases +
  their enum arms so nothing is dead), and add Esc/outside-click dismissal for the bubble.
- RED: `e2e/notes/brain-popover.spec.ts` green.

### folder_active_shares gate (minor, downgraded) — mask titles for a sealed-not-unlocked folder
- Fix (backend): gate `folder_active_shares` behind the folder-unlocked check (return an empty/masked
  list for a sealed-not-unlocked folder) so org-share titles don't leak. Small, include in Wave 3 BE.

---

## Wave 4 — hygiene, gates, manual E2E, merge

### Hygiene (must precede commit)
- Remove the bare 64-hex `railway-verify=` token from `.claude/skills/deploy-murmur-server/SKILL.md`
  (secret-scan will block the commit otherwise) — redact to a placeholder.
- Add `e2e/org/__screens__/` (+ any generated PNGs) to `.gitignore` so verification artifacts don't
  get committed; do not `git add` the stray `whisper-01.png`, `.agents/`, `.codex/`.
- Decide `crates/murmur-brain` version alignment (cosmetic; note in the release runbook, not a code
  blocker).

### Minors folded in opportunistically (non-blocking, if low-risk)
- `refreshOrgShared` stale-result guard (FE failure mode #4).
- `note-brain-popover` component `setTimeout` → service-owned timer or `afterNextRender` (rule §5).
- Hardcoded rgba scrims in new overlay scss → tokens.
- Invalid two-easing animation shorthand (notes overlays) → valid single easing.
- `AppConfig::default()` IO (`sysctl` + dir list) → memoize / lazy so a default ctor is pure.
- Sidecar idle-exit zombie reap + `murmur-brain` crate into ci.sh (BS-4).
- BS-1: document the serialize+reload trade explicitly and start the per-request deadline before the
  lock wait so a queued request isn't unfairly timed out (no re-architecture this pass).

### Gates
- `cd src-tauri && cargo test --lib` green; `npx ng lint` + `npx ng build` green;
  `npm run test:e2e` green (both currently-red specs fixed); finally `bash scripts/ci.sh`.

### Manual E2E (dev app + local/real server + Playwright/browser) — verify, don't assume
1. Boot the dev app (`MURMUR_DEV_DEK=… npm run dev`), clean boot no abort.
2. **Org flow (the live bug):** with the deployed/local server, create org "Test" → succeeds (no
   404); invite flow; consent/preview/share a note; a second session recovers the OCK.
3. **Notes flow:** create note-folders, move/rename (incl. Polish name), delete a folder → notes
   reparent (not deleted); lock a folder with notes → no plaintext `.md` at rest; lock from the Notes
   section → shares dialog appears.
4. **Timeline:** open a meeting, Audio tab → timeline generates once, no loop; locked meeting →
   masked, and a session-unlock→relock leaves no plaintext timeline.
5. **Selection Brain bubble:** select text → Refine → Accept → bubble dismisses (no re-float).
6. **Voice/record:** fresh-install-like config → start recording → no voice-command wedge.
   (Touch ID / screen-share auto-relock / real model decode = honestly flagged signed-build-only.)

### Merge
- Feature branch(es) off `murmur`; commits authored **only** by `QueaT <kgm004a@gmail.com>`, no
  Claude trailers; `gh` account `JakubGawr`. PR → `gh pr merge`. Never direct-push to `murmur`.
- Server fix: PR + merge in `murmur-io/murmur-server`, then Railway deploy via the skill.

---

## Ownership matrix

| Item | Type | Implementer | Required verifier(s) |
| --- | --- | --- | --- |
| NOTES-1/2/3, SEAM-F1/F2, LOCK-SHARE-INGEST-1 | lock/crypto BE | rust-tauri-dev | lock-security-reviewer + adversarial-verifier |
| SB-1, SB-2, SB-3, org_leave/MCP gate, folder_active_shares gate | lock/crypto BE + server | rust-tauri-dev | lock-security-reviewer + adversarial-verifier |
| TP-F1, MEM-1 | BE | rust-tauri-dev | adversarial-verifier |
| RED-E2E-1, PK-F1, WT-F1, FE minors | FE | angular-zoneless-dev | adversarial-verifier |
| Hygiene, gates, manual E2E | main loop | — | main loop + adversarial-verifier final |
