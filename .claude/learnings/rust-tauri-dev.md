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

### [2026-07-03] Phase 1 COMPLETE user memory (D5 thread-turns + Ask/detail inject + flag) — lock-touching
- **Pattern:** three seams. (1) D5: `extract_user_fact_candidates` grew a `thread_turns` arg + a PURE
  testable `build_extraction_user_prompt` seam (thread turns section, bounded, omitted-when-empty);
  fed from `list_assistant_interactions_visible` (GATED — the just-finished meeting is its own unlocked
  meeting, USER command text only). (2) Inject the gated brief into the 2 non-agentic surfaces:
  `vault_chat::build`/`agentic_system` + `chat::build` each took a `memory_brief` arg with an
  EMPTY⇒byte-identical contract (a `memory_block` helper). Brief computed by a new commands-level
  `gated_memory_brief_for_injection(&AppState, &unlocked)` → `list_user_facts_visible` (same gate as the
  @brain loop). (3) `user_memory_enabled` flag (default TRUE, `default_true`) threaded EVERYWHERE:
  extraction skip, all 3 inject paths (incl. live.rs `gated_user_memory_brief(.., enabled)`),
  `get_user_memory` → `UserMemory::disabled()` marker. Full DTO wiring BE (AppConfigDto/dto_to_config/
  config_to_dto) + FE (models.ts UserMemory.disabled?/AppConfigDto.userMemoryEnabled + settings.store 3
  sites + onboarding literal).
- **Trap hit:** an `fn(a,b,c)`→`fn(a,b,c,d)` arg change hit ~8 test call sites (ask_vault_loop ×4,
  build_ask_vault_floor_prompt ×2, ask_vault_floor ×1, vault_chat::build ×1) — grep the symbol, fix them
  all in the same pass. FE: a full-DTO `AppConfigDto` literal in onboarding.component.ts (NOT just the
  store) needed the new field or `ng build` TS2741'd — grep every literal, not just the store.
- **Caught by:** `cargo test --lib` (781 pass, +13 new); `cargo clippy --lib` clean; `ng lint`+`ng build`
  green. RED proven by swapping `list_user_facts_visible`→`user_facts_all()` in the inject helper: the
  ask-gate test FAILED ("sealed-source user fact must NOT be in the injected brief"); reverted → GREEN.
- **Lesson:** the byte-identical-when-empty contract is the safe way to add a prompt segment behind a
  flag — test it explicitly (`assert_eq!(with_empty, pre_change)`) on EVERY surface, so a disabled/empty
  brief can never drift the prompt. FLAG for lock-security-reviewer (new gated read + new inject path).
- **Status:** GREEN (781 lib tests; FE lint+build clean).

### [2026-07-03] Phase 3 cross-meeting user memory (lock-touching)
- **Pattern:** reused the bitemporal `facts` substrate (pure `reconcile_facts`) for a NEW user-scoped
  `user_facts` table (separate table, NO entity FK, `USER_SCOPE` sentinel in the `entity_id` reconcile
  slot only — never persisted). Purge-on-seal wired into BOTH the seal tx (`purge_user_facts_tx`) AND
  the at-rest reconcile; gated read `list_user_facts_visible` mirrors `list_facts_visible`'s
  meetings-JOIN `visibility_clause`. Injected the brief into `assistant_system_prompt` exactly like
  `live_transcript` (rides the existing RedactingProvider + consent gate — no new egress class).
- **Trap hit:** a SQL string comment containing `"double quotes"` TERMINATES the Rust string literal →
  21 spurious compile errors. Use single-quotes / plain words in SQL-string comments.
- **Caught by:** `cargo test --lib` (12 new tests). RED-before-GREEN proven by deleting the
  visibility predicate → both the DB gate test AND the brief-injection test failed (sealed fact leaks).
- **Lesson:** when adding an `fn(a, b)` arg, grep ALL call sites incl. test modules — 13 test sites
  needed the 3rd arg; a `perl -0pi` batch is faster than 13 Edits. Flag for lock-security-reviewer.
- **Status:** GREEN (771 lib tests pass; migration still idempotent).

### [2026-07-02 seed] Distilled from rust-tauri.md + lock-model.md
- **Pattern:** errors/commands/SQLCipher/additive-migrations/seal-verify/gate-reads/crash-safe-FFI.
- **Caught by:** operator (seeding the loop).
- **Lesson:** the bullets above; full detail in `.claude/rules/rust-tauri.md` + `lock-model.md`.
- **Status:** distilled (2026-07-02)
