# Rust / Tauri ruleset — `src-tauri` (binding rules-for-Claude)

> The canonical Rust core of Murmur is the `meetnotes_lib` crate (`src-tauri/`, package
> `murmur`, bin `Murmur`). These rules are BINDING for ANY change under `src-tauri/src/`.
> They are not style preferences — several encode crash / data-loss / leak bugs already paid
> for in production. Trust the code, not the docs: when a claim here matters, open the cited
> file and confirm the symbol still exists before relying on it.

---

## 1. Errors — `AppError` + `Result<T>` everywhere

- The ONLY error type is `AppError` (`src-tauri/src/error.rs:4`); every fallible fn returns
  `crate::error::Result<T>` (= `Result<T, AppError>`, `error.rs:33`). NEVER return bare
  `anyhow::Result`, `Box<dyn Error>`, or `unwrap()`/`expect()` in non-test code.
- Use the right variant for the failure domain — `Audio`, `Transcribe`, `Summarize`,
  `Export`, `Storage`, `Migration`, `Auth`, `Locked`, `Secrets`, `Config`, `Unavailable`,
  `InvalidArg`, or `Other(#[from] anyhow::Error)`. A locked-content refusal MUST be
  `AppError::Locked(..)`, never a generic `Storage`/`Other`.
- `AppError` is `Serialize` (`error.rs:36`) so it crosses the IPC boundary cleanly to the
  Angular FE. Do not hand-build error strings the FE has to parse.

## 2. Commands — registered in `lib.rs`, one source of truth

- Every `#[tauri::command]` lives in `src-tauri/src/commands.rs` and MUST be added to the
  `tauri::generate_handler![ … ]` list in `src-tauri/src/lib.rs:51`. A command that compiles
  but is not in that list is silently un-callable from the FE — the most common "why is my
  IPC undefined" bug. Adding a command = edit commands.rs AND lib.rs in the same change.
- Commands take `State<'_, AppState>` (defined in `src-tauri/src/state.rs`: `db`,
  `unlocked_folders`, `master_kek`) — never reach for globals. Long-running work
  (transcribe/summarize/unlock-with-biometric) is `async` (see `unlock_meeting`
  `commands.rs:2055`, `unlock_folder` `commands.rs:1793`).
- Emit progress/state to the FE via the typed helpers in `src-tauri/src/events.rs`, not by
  inventing ad-hoc event names at the call site.

## 3. Storage — SQLCipher; `PRAGMA key` FIRST

- The whole DB is SQLCipher-encrypted. Open ONLY through `Db::open` (`db.rs:84`, pulls the
  DEK from the keychain) or `Db::open_with_key(path, dek_hex)` (`db.rs:91`). The `PRAGMA key`
  MUST be the first statement on the connection, before any other SQL — `open_with_key`
  enforces this; do not open a raw `rusqlite::Connection` to the murmur DB anywhere else.
- The DEK is a 64-hex-char raw key from `secrets::keychain::get_or_create_db_dek`
  (`secrets/keychain.rs:21`), released at launch. Never log it, never embed it, never pass it
  to the FE.

## 4. Schema migrations — guarded + ADDITIVE only

- Schema evolves through `Db::migrate()` (`db.rs:111`) using `add_column_if_missing`
  (`db.rs:212`) and `CREATE TABLE IF NOT EXISTS`. NEVER write a destructive migration —
  no `DROP`, no `ALTER … DROP COLUMN`, no `DELETE`/rewrite of user rows. Real user DBs exist;
  a destructive migration is unrecoverable data loss.
- `migrate()` is idempotent (test `migrate_is_idempotent`, `db.rs:1991`) and MUST stay so:
  re-running it on an already-migrated DB is a no-op. New columns are added there, guarded;
  new tables use `IF NOT EXISTS` (see `entities` / `entity_mentions`, `db.rs:172`/`180`).
- The separate one-time PLAINTEXT→SQLCipher upgrade lives in `storage/migration.rs`:
  `encrypt_in_place` (`migration.rs:50`) exports to an encrypted temp, `verify_encrypted`s it
  (integrity + per-table row counts, `migration.rs:79`/`100`), writes a `.pre-encrypt.bak`,
  and ONLY THEN atomically renames. The original plaintext file is never mutated until a
  fully-verified encrypted copy exists. Do not shortcut that verify-then-swap ordering.

## 5. The seal pattern — VERIFY-BEFORE-DESTROY (non-negotiable)

Encrypting content at rest (locking) MUST prove the ciphertext is decryptable BEFORE blanking
the plaintext. Lose this ordering and a crash/bug between encrypt and verify destroys the only
copy.

- `crypto::encrypt`/`decrypt` (AES-256-GCM, `crypto.rs:15`/`31`) and `encrypt_file`/`decrypt_file`
  (`crypto.rs:54`/`74`) — `encrypt_file` decrypts its own output back and asserts byte-identical
  to the source before returning (`crypto.rs:50`).
- `Db::seal_note` (`db.rs:1000`) / `seal_timeline` (`db.rs:1204`) write the blob; the caller
  (`seal_folder_extras` `commands.rs:2087`, `lock_folder` `commands.rs:1731`) verifies each
  blob reads back BEFORE `blank_sealed_notes_in_folders` (`db.rs:1044`) blanks the plaintext
  column or deletes the vault `.md` (`commands.rs:1751`/`1768`). Audio: encrypt WAV → `.enc`
  (verify-before-destroy inside `encrypt_file`) → only then remove the plaintext WAV
  (`commands.rs:2123`). Round-trip test: `seal_transcript_timeline_round_trips_byte_identical`
  (`db.rs:2672`).
- RULE: any NEW seal/encrypt-at-rest path you add MUST verify-decryptable before destroying the
  plaintext, and must be reversible by the matching unseal (`unlock_folder`/`remove_lock`).
  See `.claude/rules/lock-model.md` for the full lock invariants.

## 6. Every content read is GATED — `meeting_is_unlocked` / `visibility_clause`

- A sealed-and-not-session-unlocked meeting MUST leak NOTHING. In `commands.rs`, content reads
  (note, segments, timeline, audio) check `meeting_is_unlocked(state, &meeting_id)?`
  (`commands.rs:2249`; call sites e.g. `commands.rs:478`, `1386`, `1443`) and return a masked
  DTO (`locked: true`, no note/segments, `audio_path: None`) otherwise.
- The MCP surface and graph/search reads push the session `unlocked` set through
  `visibility_clause` (`db.rs:1269`) — `search_visible` (`db.rs:1257`),
  `list_meetings_visible` (`db.rs:1309`), `get_note_if_visible` (`db.rs:1348`),
  `meeting_is_visible` (`db.rs:1369`), `list_entities_visible` (`db.rs:1448`). NEVER add a read
  path that bypasses these helpers.
- RULE: any NEW read/query/export that returns meeting content MUST route through
  `meeting_is_unlocked` (commands) or `visibility_clause` (db/MCP). An ungated read is a leak
  bug and fails the lock-security review.

## 7. macOS FFI — CRASH-SAFE; prefer CoreGraphics/CoreFoundation C funcs

The war story (do not repeat it): a prior screen-share probe sent `msg_send![screen, isCaptured]`.
`NSScreen.isCaptured` does not exist on macOS — it is a UIScreen/iOS selector — so it raised an
unrecognized-selector `NSException`, which unwound across the FFI boundary and ABORTED the process
at launch ("Rust cannot catch foreign exceptions"). See the header doc in
`src-tauri/src/screenshare.rs:12-38`.

- Prefer pure CoreGraphics / CoreFoundation **C functions** for macOS introspection — they
  return null/empty on failure, they do not throw. `screenshare.rs` uses ONLY
  `CGWindowListCopyWindowInfo`, `CFArrayGetCount`, `CFArrayGetValueAtIndex`,
  `CFDictionaryGetValue`, `CFGetTypeID`, CFString reads — ZERO `msg_send` (`screenshare.rs:32`).
- If an Objective-C `msg_send!` (objc2) is genuinely unavoidable, GUARD it first with
  `respondsToSelector:` / `class_getInstanceMethod` and have a safe fallback. Never send a
  selector you have not proven the receiver implements on the target OS version.
- Biometric (`biometric.rs`, objc2 `LAContext`) deliberately GRACEFULLY DEGRADES: FFI failure,
  no Touch ID hardware, unsigned/CI binary → `Ok(true)` with a warning, never a panic
  (`biometric.rs:7`/`29`/`46`). Touch ID + lock-at-rest + screen-share only TRULY verify on a
  signed build — typecheck/`cargo test` is not proof for FFI/permission code.

## 7b. CSP in `tauri.conf.json` — keep `style-src` nonce-FREE or Angular styles vanish in prod

- Tauri injects a per-load nonce into `style-src` (and stamps `nonce=` on every inline `<style>`
  in `index.html`): `tauri-utils/html.rs` `inject_nonce_token` + `tauri/src/manager/mod.rs`
  `replace_csp_nonce`. A nonce in `style-src` makes the browser IGNORE `'unsafe-inline'` (CSP3
  §6.7.3.2), which BLOCKS Angular's runtime-injected emulated-encapsulation component `<style>`
  nodes → the packaged WKWebView build renders every component unstyled while the global
  `styles.css` `<link>` still works. (`ng serve` never reproduces it — green `ng build` ≠ shipped.)
- **REQUIRED:** `app.security` keeps `"dangerousDisableAssetCspModification": ["style-src"]`
  (added 2026-06-29). Do NOT remove it, and do NOT add a nonce/hash to `style-src`. `script-src`
  stays strict. Full root-cause + diagnostics + the disproven theories: `angular-zoneless.md` **T4**.
- `identifier` `com.meetnotes.app` and the rest of the `csp` string are immutable for the usual
  TCC/Keychain-continuity reasons — change `security` only with a real WKWebView render-test.

## 8. No PII in logs

- Never log note text, transcript segments, titles, attendee names, file paths that embed
  personal content, keys, tokens, or DEK/KEK material. Logs may carry IDs, stage names, counts,
  durations. Compliance: audio + transcript stay on device; logs must not become the leak.
  `tracing` calls use `target:` + non-PII fields (see `migration.rs:91`, `screenshare.rs:181`).

## 9. Test loop — `cargo test --lib` ONLY; never `clippy --all-targets` in the loop

- Inner dev/verify loop: `cargo test --lib` (the ~128 unit tests live in `src-tauri/src/**`).
  Run it from `src-tauri/` (or `source ~/.cargo/env` first).
- The on-device brain (mistralrs) + embedder/NER (candle/tokenizers) are now ALWAYS compiled (the
  feature gates were removed so the real impls ship by default, selected at runtime on model
  presence). So `cargo test --lib` now DOES compile the heavy ML tree: the FIRST build after a clean
  checkout (or a deps bump) is slow (hundreds of MB), but the incremental loop stays fast once warm —
  let a slow first build finish, don't bail. `MISTRALRS_METAL_PRECOMPILE=0` is baked into
  `src-tauri/.cargo/config.toml [env]` (CLT-only Mac defers Metal-shader compile to first run).
- Do NOT run `cargo clippy --all-targets` in the iterative loop — it thrashes the
  openssl/sqlcipher build profile and times out. The full gate `scripts/ci.sh` (which DOES run
  clippy `-D warnings` + tests + `ng lint` + `ng build` + headless E2E) is the FINAL check, run
  ONCE at the end — not repeatedly while iterating.

## 10. Monetary / currency

- N/A. Murmur handles no money, prices, or currency. Do not introduce currency/amount fields,
  cents conventions, or payment code — if a task seems to need them, it is out of scope; stop
  and ask.
