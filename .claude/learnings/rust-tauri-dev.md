# Learnings — rust-tauri-dev

## Recurring patterns
<!-- Curated, binding. Prepended to every dispatch. Keep ≤ ~20 bullets. -->

- **`AppError` + `Result<T>` only.** Never bare `anyhow::Result`, `Box<dyn Error>`, or
  `unwrap()`/`expect()` in non-test code. A locked-content refusal is `AppError::Locked`, never a
  generic `Storage`/`Other`. `AppError` is `Serialize` — don't hand-build error strings for the FE.
- **A new command = edit `commands.rs` AND `lib.rs`.** A `#[tauri::command]` missing from
  `generate_handler![…]` compiles but is silently un-callable — the #1 "IPC undefined" bug.
- **SQLCipher `PRAGMA key` is the FIRST statement.** Open only through `Db::open` /
  `Db::open_with_key`; never a raw `rusqlite::Connection` to the murmur DB. Never log/embed/FE-pass
  the DEK.
- **Migrations are ADDITIVE + idempotent.** `add_column_if_missing` / `CREATE TABLE IF NOT EXISTS`
  in `Db::migrate()`. NEVER `DROP`/`DELETE`/rewrite user rows — real user DBs exist; destructive =
  unrecoverable loss. The one-time plaintext→SQLCipher upgrade verifies-then-swaps; don't shortcut it.
- **Seal = VERIFY-BEFORE-DESTROY.** Any encrypt-at-rest path must prove the ciphertext decrypts
  back byte-identical BEFORE blanking the plaintext / deleting the `.md` / removing the WAV, and be
  reversible by the matching unseal. Round-trip test the seal.
- **Gate EVERY content read.** Note/segments/timeline/audio commands check
  `meeting_is_unlocked(state, &id)?` and return the masked DTO (`locked:true`, `audio_path:None`)
  otherwise; db/MCP reads route through `visibility_clause`. An ungated read is a leak → fails
  lock-security review. (Grep the symbol — line numbers in the rules have drifted thousands of rows.)
- **`audio_path: None` on the masked DTO is load-bearing** — the FE feeds `audio_path` to
  `convertFileSrc`/the `asset:` protocol, which bypasses every backend command and the gate. Never
  hand the FE an on-disk path for a locked meeting.
- **macOS FFI must be crash-safe.** Prefer CoreGraphics/CoreFoundation C funcs (return null, never
  throw). An unrecognized `msg_send!` selector raises an `NSException` that unwinds across FFI and
  ABORTS at launch (the `NSScreen.isCaptured` war story). Guard any objc2 `msg_send` with
  `respondsToSelector:`; biometric FFI degrades gracefully (`Ok(true)`), never panics.
- **CSP:** keep `style-src` nonce-free (`dangerousDisableAssetCspModification:["style-src"]`) or
  prod Angular styles vanish. `com.meetnotes.app` identifier immutable.
- **No PII in logs** — IDs/stages/counts/durations only; never note text, titles, names, keys, DEK/KEK.
- **Test loop = `cargo test --lib` from `src-tauri/`.** NEVER `cargo clippy --all-targets` in the
  loop (openssl/sqlcipher profile thrash → timeout; also blocked by `block-bash.sh`). Full gate =
  `scripts/ci.sh`, run ONCE at the end. Let a slow cold ML build finish; don't bail.

## Run journal
<!-- Append-only, newest first. -->

### [2026-07-02 seed] Distilled from rust-tauri.md + lock-model.md
- **Pattern:** errors/commands/SQLCipher/additive-migrations/seal-verify/gate-reads/crash-safe-FFI.
- **Caught by:** operator (seeding the loop).
- **Lesson:** the bullets above; full detail in `.claude/rules/rust-tauri.md` + `lock-model.md`.
- **Status:** distilled (2026-07-02)
