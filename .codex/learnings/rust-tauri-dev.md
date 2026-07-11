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

### [2026-07-05 password-links + Touch ID — #195/#196] cross-lang crypto salt + biometric MK cache
- **Pattern:** (a) Password link-shares never decrypted — the Argon2id salt had NO protocol field, so
  the client's RANDOM salt was lost the instant the share sealed (a green Rust round-trip test hid it).
  Fixed by DERIVING `salt_p` from the per-link `gate_salt` (`HKDF(gate_salt, None, info="murmur-link/v1:argon-salt")[..16]`)
  on both Rust (`e2ee/link.rs`) + the JS viewer — NO protocol/server/DB change. (b) Added a biometric-
  gated account-MK Keychain cache by REUSING the folder-KEK `MacKekStore` pattern verbatim (user-
  presence + WhenUnlockedThisDeviceOnly, non-syncable); debug = plaintext dev-file + `MURMUR_DEV_ACCOUNT_MK`,
  all `#[cfg(debug_assertions)]` → compiled OUT of release.
- **Caught by:** operator (Rust↔JS byte-exact cross-check via the `link_share_interop_vector` fixture +
  hash-wasm in Node) + lock-security-reviewer (caught a logout clear-before-drop ordering) + the user (live).
- **Lesson:** To make a client value reproducible on a JS viewer, DERIVE it from something both sides
  already hold (`gate_salt`) instead of plumbing a new field through protocol+server+DB+migration. An
  Argon2id salt need only be unique-per-hash, NOT secret → deriving from a public per-link random is
  sound (distinct HKDF `info` = domain separation). A green unit test on ONE side is NOT interop proof —
  emit a vector + cross-check the other language byte-exact. New keychain secrets reuse the proven
  biometric seam + the DEV-hatch `#[cfg]` pattern; in logout DROP the in-RAM `Zeroizing` secret BEFORE
  any fallible `clear_*()?` (a `?` must never skip the zeroize).
- **Status:** journal

### [2026-07-04 PR#181 Murmur Brain] Semantic-wiring bug classes a green cargo-test + clippy can't see
- **Pattern:** a large brain feature shipped 7 real defects behind green `cargo test --lib` + green clippy,
  all surfaced only by a deep-review Workflow: (1) **producer≠consumer config field** — a `select_model`
  command set `brain_model_id`, but the runtime handle read class-specific `brain_light_model_id` /
  `brain_heavy_model_id` (via a `class_model_id` resolver) → enabling the feature silently ran the stub;
  fix = the select command sets the class id from the model's class. (2) **invariant at the tested site
  only** — `derive_posture` gated the "Fully Local" label on 2 of 3 role axes (Notes+Ask), leaving the
  Live/`@brain` axis able to egress under a "zero egress" label. (3) **dead helper with over-claiming
  doc** — `is_recording()` doc said it "drives the recording-active gate" but had ZERO callers; removed it
  (don't ship a lying mechanism). (4) **pure fn nobody calls** — the RAM guard `residency_fits` was tested
  but unreachable; wired a command so it's live. (5) **per-call runtime leak** — `light()/heavy()` built a
  fresh `MistralReasoner` (each owning a new tokio multi-thread Runtime) on EVERY call (~every 21 s) →
  thread churn; fix = one process-global `brain_rt()` `OnceLock<Option<Runtime>>` shared by all instances.
  (6) **worker-thread panic wedge** — a spawned worker set a busy `AtomicBool` but reset it only at the
  END → a panic wedged it `true` forever; fix = an RAII `Drop` guard. (7) **download resume 416** — a
  fully-complete `.part` returned 416 Range-Not-Satisfiable → permanent error; handle 416 as "complete →
  verify + promote". Tooling: `2>&1 > file` MISORDERS the redirect (stderr → old stdout = terminal); use
  `> file 2>&1` — clippy/cargo diagnostics go to stderr.
- **Caught by:** deep-review Workflow (6-dimension fan-out, each finding adversarially verified).
- **Lesson:** green tests + clippy prove NEITHER wiring nor completeness. When you add a config field, grep
  producers vs consumers (SAME field?). When a helper's doc says it "drives/gates" X, grep its callers.
  When a handle is built per call, check it doesn't build a Runtime per call. Reset a shared busy-flag via
  RAII, not a tail statement (panic-safe). Enforce every "never X" invariant at ALL sites, not just the
  one with a test.
- **Status:** journal

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
- **Lesson:** the bullets above; full detail in `.codex/rules/rust-tauri.md` + `lock-model.md`.
- **Status:** distilled (2026-07-02)
