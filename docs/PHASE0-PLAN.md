<!-- Phase 0 implementation plan, code-grounded, generated 2026-06-26. -->

# Murmur Phase 0 — Unified Implementation Plan
### SQLCipher at-rest + per-folder biometric lock + rich folder UI

Crate `murmur` / lib `meetnotes_lib` at `/Users/jakubgawronski/Projects/meetnotes`. **In scope:** F2 Layer 1 (SQLCipher), F2 Layer 2 (per-folder content key behind biometric), F1 (folder UI + lock lifecycle), MCP exposure filter + token, screen-share auto-relock. **Explicitly OUT:** in-app graph, cloud/sync/sharing, enrichment, hosted MCP, CRDTs.

**The single hardest truth (from all three analyses, and the spec §5.2):** the migration is the only destructive operation and biometric must release a real key from a `.biometryCurrentSet`-ACL'd Keychain entry, not flip a boolean. Everything below is ordered so the destructive step is proven on a throwaway copy before it touches real data, and so the boolean-theater trap is closed by design.

---

## 1. BUILD ORDER

Legend: **S/M/L** effort · **[INLINE]** = orchestrator edits a shared Rust file serially (conflict-prone, see §2) · **[WORKFLOW]** = delegable to a parallel workflow (new file or isolated FE surface).

### Stage A — SQLCipher foundation (must land first; nothing else is safe until the DB is encrypted-and-migrated)

**A1. Flip the rusqlite feature + verify the universal build. — M — [INLINE]**
- `Cargo.toml:41`: `features = ["bundled"]` → primary `["bundled-sqlcipher-vendored-openssl"]`, documented fallback `["bundled-sqlcipher"]` (SecurityFramework/CommonCrypto, no OpenSSL in the graph → sidesteps the cross-compile trigger).
- **HARD GATE before this is accepted (DB+CRYPTO analysis §1):** green `tauri build --target universal-apple-darwin` on the actual release runner AND a launch test on both arm64 and x86_64 (or `arch -x86_64`). Compile success is NOT enough — the `libssl.3.dylib` code-sign/load failure only shows at runtime ([Tauri #9684]). If it fails, fall back to `bundled-sqlcipher` and confirm SecurityFramework is selected. `tauri.conf.json:31` is `"targets":"all"` — the release path is the universal lipo, so this gate is mandatory, not optional.
- Mitigation preference order: (1) build each arch natively + manual `lipo`; (2) fall back to `bundled-sqlcipher`; (3) cross-compile with `PKG_CONFIG_SYSROOT_DIR` (least recommended).

**A2. DEK lifecycle in the Keychain. — S — [INLINE]** (`secrets/keychain.rs`)
- Add `pub const ACCOUNT_DB_DEK: &str = "murmur_db_dek";` and `get_or_create_db_dek() -> Result<String>` returning a **64-char hex string** (32 random bytes via `getrandom`), reusing the existing `get_secret`/`set_secret` plumbing verbatim. Hex form → SQLCipher consumes it as a raw key blob with **no KDF**.
- DEK uses plain `keyring` (released at launch, file-theft protection) — **no biometric prompt at launch**. This is deliberately distinct from the KEK (Stage E).
- New dep: `getrandom` (needs approval — tiny, standard, cryptographic RNG).

**A3. Key the connection at open. — S — [INLINE]** (`db.rs:56`)
- Split `Db::open(path)` → fetches DEK, calls new `Db::open_with_key(path, dek_hex)`. The `PRAGMA key = x'<hex>'` must be the **FIRST** statement on the connection, before the existing `PRAGMA foreign_keys/journal_mode` batch at `db.rs:60`. Use `conn.pragma_update(None, "key", format!("x'{dek_hex}'"))` — never string-interpolated `execute_batch`, never log the key or formatted PRAGMA.
- The first read in `migrate()` (`db.rs:73`) validates the key (wrong key → `SQLITE_NOTADB`). WAL stays (encrypted page-by-page).
- `open_with_key` is the seam the MCP server reuses (§MCP gotcha) and the migration verify-step reuses.

**A4. Safe migration module. — M — [WORKFLOW]** (new `src-tauri/src/storage/migration.rs`)
- `is_plaintext_sqlite(path)`: read first 16 bytes; plaintext starts with magic `"SQLite format 3\0"`, SQLCipher's first 16 bytes are random salt → magic absent. Non-existent/<16 bytes ⇒ new install ⇒ no migration. This makes detection **idempotent**.
- `encrypt_in_place(db_path, dek_hex)`: the full sequence — checkpoint+collapse WAL → `VACUUM INTO` plaintext backup (`.pre-encrypt.bak`) → `ATTACH ... KEY x'<hex>'` + `SELECT sqlcipher_export('encrypted')` + `DETACH` → set `user_version` on target (export does NOT copy it) → **fresh-reopen the encrypted target with the DEK** and verify (`cipher_integrity_check`, exact per-table row counts vs source, sample-decrypt of newest meeting `(id,title)`) → atomic `rename` into place → retain backup.
- **Never `PRAGMA rekey`** (cannot encrypt a plaintext DB; export is mandatory). The original is never mutated until the atomic `rename`; every `?` before it leaves the plaintext DB bit-for-bit intact.
- This is a new file → delegable, but its **call-site wiring in `state.rs` is [INLINE]** (A5).

**A5. Wire migration into AppState::init. — S — [INLINE]** (`state.rs:32`)
- Before `Db::open`: fetch DEK once; `if migration::needs_encryption(&db_path)? { migration::encrypt_in_place(&db_path, &dek)?; }`; then `Db::open_with_key(&db_path, &dek)`. The rest of the app only ever sees an encrypted DB.
- Add `error.rs` variant `Migration` (and `Auth`/`Locked` now, used later).

**A6. MCP keying fix (the gotcha — must ride with A3/A5). — S — [INLINE]** (`lib.rs:104-108`, `mcp.rs:120`)
- `mcp.rs` re-derives the path and calls `Db::open(db_path)` on its own thread with **no key** → silently breaks the existing local Claude connection the moment encryption lands. Because `Db::open` now fetches the DEK internally, `mcp.rs:120 Db::open` keeps working **once `Db::open` is the keyed path** — verify the MCP thread can fetch from Keychain off the main thread. This is one of the three "must not break."

> **Stage A gate (MIGRATION SAFETY GATE §3 must fully pass here).** No later stage starts until the migration round-trips on a throwaway copy of real data.

### Stage B — Lock data model + crypto primitives (additive; unblocks C/D/E)

**B1. AES-256-GCM wrap/unwrap primitives. — S — [WORKFLOW]** (new `src-tauri/src/crypto.rs`)
- `aes_gcm_encrypt/decrypt`, KEK-wrap/unwrap of per-folder content keys. Cell format `nonce(12) || ciphertext || tag(16)` as BLOB. New dep `aes-gcm` (RustCrypto, needs approval) + optional `zeroize`. Deliberately NOT OpenSSL-via-rusqlite — keep app-layer crypto in an auditable Rust crate.

**B2. Folder + lock schema (guarded migration). — M — [INLINE]** (`db.rs::migrate`, `db.rs:73`)
- Append to the `CREATE TABLE IF NOT EXISTS` batch: `folders` table (`id, path UNIQUE, parent_id, locked INTEGER DEFAULT 0, wrapped_key BLOB, key_nonce BLOB, created_at`) + `idx_folders_parent`.
- Guarded `ALTER`s (check `pragma_table_info` first, since `migrate()` is idempotent and `ALTER ADD COLUMN` errors if the column exists): `notes.folder_id`, `notes.content_blob` (AES-GCM markdown when sealed; NULL when open). The two analyses differ on whether to also tag `meetings` — reconciled: **govern exposure at the note level** (`notes.folder_id`), because the note is the only artifact exported to the vault and exposed via MCP. Segments/audio are already local-only and never exported.
- This is the single most conflict-prone edit — one orchestrator pass.

**B3. Folder models + session state. — S — split.**
- `storage/models.rs`: add `Folder`, `FolderNode { folder, note_count, unlocked, children }`. **[WORKFLOW]**
- `state.rs:16`: add `unlocked_folders: Arc<Mutex<HashSet<String>>>` (Arc so MCP thread shares it) and `master_kek: Mutex<Option<[u8;32]>>`; init at `state.rs:39`. **[INLINE]**

### Stage C — Lock lifecycle commands (the exposure state machine)

**C1. Db helpers. — M — [INLINE]** (`db.rs`)
- `insert_folder`, `list_folders`, `count_notes_per_folder`, `set_note_folder`, `folder_for_path`, `set_folder_locked`, content-blob encrypt/decrypt, plus the exposure-aware reads `search_visible`, `list_meetings_visible`, `get_note_if_visible` (for §D). Reuses `crypto.rs`.

**C2. 8 commands + `relock_all_inner`. — M — [INLINE]** (`commands.rs` + register in `lib.rs:48-95`)
- `list_folders`, `create_folder`, `move_note`, `lock_folder` (no biometric — locking is always safe: generate CK, wrap under KEK, encrypt governed notes' markdown→`content_blob`, blank plaintext, delete `.md` from vault, clear `exported_path`), `unlock_folder` (async — biometric → KEK → unwrap CK → add to `unlocked_folders` for the session; **does NOT re-export**), `relock_folder`, `relock_all` (+ `pub(crate) relock_all_inner` for the screen-share watcher to call without a command boundary), `remove_lock` (permanent: decrypt back to plaintext, re-export `.md`, drop `locked_folders`/clear flags → back to default OPEN).
- **Lock model the commands enforce** (the AGREED LOCK MODEL): default OPEN; lock is explicit per-folder; session-unlock decrypts for in-app + MCP this session without re-export; permanent remove-lock re-exports; auto-relock on screen-share start clears the session set + zeroizes KEK.
- **Atomicity rule (F1 risk + DB+CRYPTO §4.4):** lock = DB transaction for column writes, delete the vault `.md` only AFTER commit; treat a leftover `.md` as reconcile-on-next-launch. `move_note` must not commit `exported_path`/`folder_id` before bytes are durable (cross-FS copy-fallback).
- **Pipeline (`pipeline.rs::resolve_subfolder`, ~`:276`):** Phase 0 simplest — new meetings always land OPEN; sealing is an explicit user action afterward (matches "lock is explicit"). Do not auto-seal at export time in P0.

### Stage D — MCP exposure filter + bearer token

**D1. Thread the unlock set into MCP + 3-state filter. — M — [INLINE]** (`mcp.rs:17/23/61/111`, `lib.rs:104`)
- `spawn/run/handle_rpc/handle_tool_call` gain `unlocked: Arc<Mutex<HashSet<String>>>` (clone of `AppState.unlocked_folders`); update call site at `lib.rs:104-108`.
- Swap `mcp.rs:127/147/167` to `_visible` query variants with a `LEFT JOIN folders f ON f.id = n.folder_id WHERE (f.locked IS NULL OR f.locked=0 OR f.id IN (<unlocked>))`.
- **Decryption-free MCP design (LOCK+MCP analysis §3a):** on `unlock_folder`, decrypt the governed notes' `content_blob` into the plaintext `markdown` column **for the session duration**; re-blank on relock. Then MCP reads plaintext `markdown` exactly as today and only consults the `f.locked / IN (unlocked)` predicate — the KEK never reaches the MCP thread (smaller attack surface).

**D2. Optional bearer token (backward-compatible). — S — [INLINE]** (`mcp.rs:41`)
- Mint a random token on first launch → Keychain (`murmur_mcp_token`) → write into the managed MCP config so it "just works." Enforce `Authorization: Bearer <token>` only on `tools/call` (`mcp.rs:77`); leave `initialize`/`tools/list`/`ping` open for discovery. **(Shipped differently: discovery is no longer open — `mcp.rs` requires the token on those methods too. This is a plan document, kept as the record of what was intended; trust `mcp.rs` for what the server does.)** Bind stays `127.0.0.1` (`mcp.rs:24`, unchanged). Gate behind `K_MCP_REQUIRE_TOKEN`. This closes the "a local process reads sealed notes during a session" hole without breaking the existing un-authenticated client mid-upgrade.

### Stage E — Biometric KEK release + screen-share auto-relock (the cryptographically-real lock)

**E1. Biometric KEK release. — M — [WORKFLOW]** (new `src-tauri/src/biometric.rs`)
- Use **`tauri-plugin-biometry` (Choochmeque fork)**, NOT the official `tauri-plugin-biometric` (Android/iOS only — no macOS Touch ID). The fork's `macos.rs` is real `objc2-local-authentication` + `objc2-security`, sets `kSecUseDataProtectionKeychain = true` (the flag that makes `kSecAttrAccessControl` honored), carries the prompt via `kSecUseAuthenticationContext`.
- Store the **master KEK** (32 random bytes) via the plugin's `set_data` behind the biometric ACL once at first lock setup. `unlock_folder` → `get_data(reason)` triggers Touch ID → returns KEK → unwrap the folder's `wrapped_key`.
- **Correction to the spec wording (decision needed):** the fork uses `SecAccessControlCreateFlags::UserPresence`; for locked notes prefer **`BiometryCurrentSet`** (auto-invalidates the entry when the enrolled fingerprint set changes) — a one-line change in the vendored plugin. Recommend `BiometryCurrentSet`; optionally `| DevicePasscode` for a passcode escape hatch. **This is the boolean-theater closer** (spec §5.2, §F2 risk #1): the KEK bytes are physically inaccessible without a passing Touch ID. Requires real code-signing (`entitlements.plist` present); ad-hoc signing may work in dev, production needs Developer ID.
- Register `.plugin(tauri_plugin_biometry::init())` at `lib.rs:35` — **[INLINE]**. New dep `tauri-plugin-biometry` (needs approval).

**E2. Screen-share watcher → auto-relock. — M — [WORKFLOW]** (new `src-tauri/src/screenshare.rs`, spawned from `lib.rs::setup` ~`:96` — **[INLINE]** spawn line)
- **Verdict: feasible and event-driven on macOS 12+.** Primary: observe `NSScreen.isCaptured` via an `NSNotificationCenter` observer (the public, App-Store-safe signal that posts on capture-state change; objc2 stack the biometry plugin already pulls in). On `false→true`, call `relock_all_inner` (clear `unlocked_folders` + zeroize `master_kek`) and emit `EVENT_STATUS`/a new `murmur://screen-share-started` so the UI toasts.
- Fallback: 1–2s `tokio::interval` polling `NSScreen.isCaptured` on the rising edge (the model says "on start, not a timer" — a 1s poll achieves the same user-visible behavior and is the pragmatic P0 fallback). **Do NOT use `CGDisplayIsCaptured()`** (deprecated + semantically wrong — exclusive-render, not screen-share).
- **Honest dent (state in UI):** full-screen/display share is caught; single-window WebRTC share (Meet-in-a-Chrome-tab) may not flip whole-screen `isCaptured` — same blind spot as the existing `detect_meeting_app`. Relock is best-effort hiding; it does not recall what's already on screen.

**E3. Config flags. — S — [INLINE]** (`settings/config.rs:63+`)
- `K_DB_ENCRYPTED`, `K_LOCK_ENABLED`, `K_MCP_REQUIRE_TOKEN`, `K_RELOCK_ON_SCREENSHARE` — add to the key-constant block + `load`/`save`.

### Stage F — Rich folder UI (Angular; depends only on the IPC contract, can start in parallel against stubs)

All Standalone + OnPush + `inject()` + signals/`computed`/`output()`/`input()`; no `@Input/@Output/EventEmitter`, no `*ngIf/*ngFor`, no `markForCheck`, no `subscribe()` for streams, no `setTimeout` in components (`afterNextRender` for focus), timeouts only in services with `DestroyRef.onDestroy`. Zero new npm packages. Inline SVG icons (no `lucide-angular`).

**F1. IPC + models contract. — S — [WORKFLOW]** (`core/models.ts`, `core/ipc.service.ts`)
- Types: `FolderNode { id, name, parentId, noteCount, noteCountRecursive, locked, children }`, `UnlockResult`, extend `Meeting` with `folderId`. Methods mirror the command surface (snake_case invoke, camelCase args): `listFolders`, `createFolder`, `moveNote`, `listMeetingsByFolder`, `setFolderLocked`, `unlockFolderSession`, `relockSessionFolders`, `onScreenShareStarted`; const `EVENT_SCREEN_SHARE_STARTED = "murmur://screen-share-started"`. **Compiles against backend stubs → unblocks all FE work.**

**F2. ToastService + slot in AppComponent. — S — [WORKFLOW]** (new `services/toast.service.ts`) — tracked-timeout queue with `DestroyRef.onDestroy`.

**F3. FoldersService signal store. — S — [WORKFLOW]** — `tree`, `loading`, `sessionUnlocked: signal<ReadonlySet<string>>`, `unlockedCount = computed`, `exposureOf(f) → 'open'|'locked'|'session'`; ops `load/create/moveNote/setLocked/sessionUnlock/relockAllSession`. Plain signal service (this app uses signal services, not NgRx).

**F4. LockBadgeComponent. — S — [WORKFLOW]** — pure input-driven 3-state (`open`/`locked`/`session`) badge, tokens only, `aria-label` per state, `rise`/`--ease-spring` motion respecting `prefers-reduced-motion`.

**F5. FolderRow + FolderTree. — M — [WORKFLOW]** — recursive tree via child-component recursion (not `*ngFor`); inline create (`afterNextRender` focus); `output<string|null>()` selection; per-folder `.count` chip.

**F6. MoveToMenuComponent. — S — [WORKFLOW]** — popover folder picker (DnD-lite; **decision: ship Move menu for v1, not drag-and-drop** — same IPC, keyboard-accessible, upgradeable later). **Lock-state guard confirm is load-bearing:** moving INTO a locked folder warns "encrypts + removes Markdown from vault"; moving OUT warns "re-exports plaintext."

**F7. LibraryComponent two-pane refactor. — M — [WORKFLOW]** — folder tree left + existing list right; `activeFolderId` signal; `folderMeetings`/`folderLoading` mirroring the existing `tagMeetings` machinery (latest-wins stale guard); `displayedMeetings` 3-way computed (search > folder > tag > all) — **no existing branch removed**. Locked rows render `app-lock-badge` + masked title until session-unlock.

**F8. DetailComponent Move action + read-only badge. — S — [WORKFLOW]**

**F9. ScreenShareService + wire into AppComponent. — S — [WORKFLOW]** — listens `onScreenShareStarted` → `relockAllSession()` → toast; init in `AppComponent` main-window only (guard `if (isBar()) return;`).

**F10. "N unlocked this session" pill + Settings honest-boundary copy. — S — [WORKFLOW]** — pill is `computed` over the service (zero polling); Settings privacy-card gets the copy-only "locked notes are pulled from the vault; open notes remain plaintext `.md`" paragraph (the spec's honest vault boundary).

---

## 2. CONFLICT MAP

**Shared Rust files → serial [INLINE] by orchestrator (never two workflows editing the same file):**

| File | Edited in stages | Why serial |
|---|---|---|
| `Cargo.toml` | A1, B1, E1 | feature flip + 3 new deps; one consolidated dep-approval pass |
| `storage/db.rs` | A3 (`open`/`open_with_key`), B2 (schema+guarded ALTER), C1 (helpers) | one file, three stages — **most conflict-prone**, fully serial |
| `state.rs` | A5 (migration wire), B3 (session fields) | `AppState` struct + `init` |
| `lib.rs` | A6 (MCP keying), C2 (+8 handlers at `:48`), D1 (pass unlocked set at `:104`), E1 (`.plugin` at `:35`), E2 (spawn watcher at `:96`) | five distinct line-regions; do as one pass per stage |
| `mcp.rs` | A6/D1 (thread unlocked set, `_visible` swaps), D2 (token at `:41`) | server file |
| `settings/config.rs` | E3 (flags) | key block + load/save |
| `error.rs` | A5 (`Migration`, `Auth`, `Locked`) | one small pass |
| `commands.rs` | C2 (8 commands) | large file (47KB) — one stage owns it |

**New modules / isolated surfaces → parallel [WORKFLOW] (no shared-file contention):**
- `storage/migration.rs` (A4), `crypto.rs` (B1), `biometric.rs` (E1), `screenshare.rs` (E2) — new files; only their **call-site wiring** in `state.rs`/`lib.rs` is INLINE.
- All of Stage F (Angular) — entirely separate from Rust; can start as soon as the F1 IPC contract is fixed, against backend stubs. New FE files (`folder-tree`, `folder-row`, `lock-badge`, `move-to-menu`, `folders.service`, `toast.service`, `screen-share.service`) are independent; only `library.component.ts`, `detail.component.ts`, `app.component.ts`, `ipc.service.ts`, `models.ts` are shared-FE and should be serialized among FE tasks.

**Parallelization summary:** Stage A is a hard serial prefix (foundation). Once A lands, B1/crypto + F1-F10 frontend can run in parallel. C/D/E share `db.rs`/`lib.rs`/`mcp.rs` and must serialize among themselves but can overlap the frontend.

---

## 3. MIGRATION SAFETY GATE (ordered checklist — run before the flag ships to any real DB)

Run top-to-bottom; do not advance on a failure. This gate IS the Stage-A exit criterion.

1. **Rust round-trip unit tests green** (file-backed `tempfile`, not in-memory, since `sqlcipher_export`+`rename` are file ops; extend `db.rs` tests ~`:862`):
   - `encrypt_migration_round_trips` — plaintext DB with known fixtures (3 meetings, segments, 2 notes, tags, timeline) → `encrypt_in_place` → assert no longer plaintext, `.pre-encrypt.bak` exists and IS plaintext, reopen with DEK and every row count + known `(id,title)` + known segment text read back **byte-identical**.
   - `wrong_key_fails_closed` — different key errors (no silent empty DB).
   - `migration_is_idempotent` — second run is a no-op, data intact.
   - `migration_rollback_on_verify_failure` — forced verify failure leaves the original plaintext DB **untouched**, no partial replacement.
   - `fresh_install_no_migration` — non-existent path ⇒ `Db::open` creates a fresh encrypted DB.
   - `brand_data_survives_lock_unlock` (Layer 2) — lock folder → column opaque to raw read → session-key decrypt → plaintext returns; wrong CK fails.
2. **Throwaway real-shaped harness** (cargo example / ignored test): COPY the developer's actual `~/Library/Application Support/MeetNotes/meetnotes.sqlite` to a throwaway temp path (NEVER the original) → run the full `AppState::init` migration path against the copy with a throwaway DEK → diff row counts + a sample of every table copy-vs-encrypted. **Must pass on real-world data shape/volume before the flag ships.**
3. **Manual E2E on a disposable profile:** point the built app at a throwaway app-data dir seeded with a plaintext DB → confirm first-launch migrates → app reads all meetings → `.pre-encrypt.bak` present → **MCP server still works** (the keying gotcha: `mcp.rs:120 Db::open` succeeds because `Db::open` now fetches the DEK) → second cold launch skips migration (idempotent).
4. **Universal-build runtime gate (A1):** green `tauri build --target universal-apple-darwin` + launch on both arm64 and x86_64.
5. **Backup retention:** `.pre-encrypt.bak` retained for one launch cycle; documented disaster-recovery path (a user reporting lost data has the plaintext snapshot on disk).

Invariant proven by this gate: **the original `db_path` is never mutated until a fully-verified encrypted copy exists; the only mutating moment is the atomic APFS `rename`.**

---

## 4. COMMIT SEQUENCE (with gates)

Branch off `main` (never target main). One logical commit per item; CI gate = `NX_DAEMON=false cargo build` + `cargo test` for Rust, lint/test/build for FE; runtime gate where noted.

1. `feat(crypto): SQLCipher feature flip + keyed Db::open + DEK in Keychain` (A1–A3) — **gate: universal-build runtime test (§3.4) + `db.rs` tests.**
2. `feat(crypto): safe plaintext→encrypted migration + verify + atomic swap` (A4–A5, error variants) — **gate: full MIGRATION SAFETY GATE §3.1–§3.3. This is the destructive-step commit — most scrutiny.**
3. `fix(mcp): key the MCP DB handle so local Claude survives encryption` (A6) — **gate: manual MCP E2E §3.3.**
4. `feat(crypto): AES-GCM content primitives + folder/lock schema` (B1–B3) — **gate: `crypto.rs` + schema-idempotency tests.**
5. `feat(folders): lock lifecycle commands + Db helpers` (C1–C2) — **gate: `brand_data_survives_lock_unlock` + lock/unlock atomicity tests.**
6. `feat(mcp): exposure-aware visibility filter + optional bearer token` (D1–D2) — **gate: 3-state filter test (sealed hidden / open visible / session-unlocked visible) + token-backward-compat check.**
7. `feat(security): biometric KEK release (.biometryCurrentSet)` (E1, config flags) — **gate: code-signed build + real Touch ID prompt releases key (NOT a boolean); biometric-change invalidation.**
8. `feat(security): screen-share auto-relock watcher` (E2) — **gate: live screen-share start → session keys zeroized → MCP + in-app revert to opaque.**
9. `feat(ui): folder IPC contract + ToastService + FoldersService` (F1–F3).
10. `feat(ui): LockBadge + FolderTree/FolderRow + MoveToMenu` (F4–F6).
11. `feat(ui): Library two-pane + Detail move + ScreenShare auto-relock + Settings copy` (F7–F10) — **gate: lint/test/build green; manual walkthrough lock→Touch-ID-unlock→screen-share-relock.**

---

## 5. OPEN RISKS + THE 3 THINGS THAT MUST NOT BREAK

**MUST NOT BREAK (spec §5.2 + all three analyses):**
1. **Do not corrupt the real DB.** The migration is the only destructive op. Safety rests entirely on: never mutate the original, `VACUUM INTO` backup, fresh-reopen verify (counts + sample decrypt), atomic `rename`. **Prove on a throwaway copy of real data (§3.2) before the flag ships.** A botched mid-swap loses all meeting history.
2. **Biometric must gate real key release, not a boolean.** The KEK must come from a Keychain entry created with `.biometryCurrentSet` (Security.framework ACL via the biometry plugin's data-protection store). A boolean `if authenticated {}` is theater — a locked folder gives zero extra protection once the SQLCipher DB is open. Requires real code-signing.
3. **MCP token must not break the existing local Claude connection.** Two sub-risks: (a) the keying gotcha — `mcp.rs Db::open` must fetch the DEK or the existing client breaks silently the instant encryption lands (fixed by routing through keyed `Db::open`, A6); (b) the new bearer token is enforced only on `tools/call`, auto-provisioned into the managed config, leaving discovery open — so the upgrade "just works" without manual reconfiguration.

**Other open risks (ranked):**
- **Universal-build OpenSSL cross-compile** (A1) — the single most common Tauri universal-build breaker; runtime `libssl` load failure is invisible at compile time. Mitigation: native-per-arch + manual lipo, or fall back to `bundled-sqlcipher`.
- **Locked content drops out of full-text search** (`db.rs:252` `LIKE` over now-opaque blobs). **Decision: accept it for P0** (locked = not searchable until unlocked) — simplest and most honest; state it in the lock UI. Do NOT build a plaintext search index (re-leaks).
- **Session key cache is plaintext in process memory** while unlocked — unavoidable for an in-app-viewable lock; mitigate with `zeroize` + screen-share relock; document.
- **Lock/vault atomicity** — a crash between encrypt-column and delete-`.md` leaves a plaintext `.md` the DB thinks is gone; DB transaction + delete-after-commit + reconcile-on-launch.
- **Single-window screen-share blind spot** — Meet-in-a-Chrome-tab may not flip whole-screen `isCaptured`; document the honest dent.

**Decisions needing user sign-off:** (1) primary `bundled-sqlcipher-vendored-openssl` vs fallback `bundled-sqlcipher`; (2) `BiometryCurrentSet` (hard-fail on re-enrollment) vs `UserPresence`+passcode; (3) Move menu vs native DnD in P0 (plan ships Move menu); (4) 4 new Rust deps need approval per the no-new-deps stance — `getrandom`, `aes-gcm`, `zeroize`(optional), `tauri-plugin-biometry`.

Files grounding this plan: `/Users/jakubgawronski/Projects/meetnotes/docs/ARCHITECTURE-LOCAL-CLOUD.md`, `src-tauri/Cargo.toml:41`, `src-tauri/tauri.conf.json:31`, `src-tauri/src/storage/db.rs:56,73`, `src-tauri/src/state.rs:32`, `src-tauri/src/lib.rs:35,48,96,104`, `src-tauri/src/mcp.rs:17,24,41,77,120`, `src-tauri/src/secrets/keychain.rs:5`.
