# Learnings — rust-tauri-dev

## Recurring patterns
<!-- Curated, binding. Prepended to every dispatch. Keep ≤ ~20 bullets. -->

- **An enum crossing IPC needs BOTH `rename_all` AND `rename_all_fields`.** On an enum the first
  renames the VARIANTS, the second the FIELDS INSIDE them. `TileData` had only the first: variants
  tagged correctly so it looked right, while `started_at`/`duration_s`/`has_audio` reached a FE
  reading camelCase — every field `undefined`, the tile threw, the board died, and six fixes went
  to the wrong layer. Assert the SERIALIZED key names in a test; a round-trip through the same Rust
  type passes regardless of naming. See `rust-tauri.md` §2b.
- **`AppError` + `Result<T>` only.** Never bare `anyhow::Result`, `Box<dyn Error>`, or
  `unwrap()`/`expect()` in non-test code. A locked-content refusal is `AppError::Locked`, never a
  generic `Storage`/`Other`. `AppError` is `Serialize` — don't hand-build error strings for the FE.
- **A new command = edit its `commands/<domain>.rs` module AND `lib.rs`.** A `#[tauri::command]` missing from
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
- **ONE `cargo` process machine-wide, ever — concurrency FREEZES the Mac.** The app test binary
  statically links the ML tree (candle/whisper/onnx); several concurrent `cargo test --lib` links
  (multiple agents + your own runs) pin the macOS memory compressor → the whole UI freezes (swap
  stays 0 — it's compression). Iterate with TARGETED filters (`cargo test --lib links` = full
  compile + subset run, catches build errors, low RAM), NEVER the full 1900-test suite locally; let
  CI run the full gate. `CARGO_BUILD_JOBS=2 … -j2`. Root cause + the `[profile.dev.package."*"]
  debug=false` fix: `docs/research/2026-07-18-build-ram-freeze.md`.
- **`cargo clippy --lib -- -D warnings` before EVERY push — the cheap CI-parity gate.** Link-free
  (typecheck only → low RAM, ~10-15s warm). Catches the `dead_code` class that BOTH `cargo test
  --lib` and `cargo clippy --lib --tests` mask: a `const`/`fn` used ONLY in a `#[cfg(test)]` module
  is "never used" in the lib-only build CI runs, but the test build sees the test using it and stays
  green (this ate a CI cycle — PR-2's `MCP_DEFAULT_WINDOW_CHARS`, whose feature was ALSO inert:
  the dead-code error was the real "you wired it wrong" signal).
- **Grep the diff for post-1.77 std items BEFORE pushing** (RAM-free): `git diff origin/murmur --
  src-tauri | grep '^+' | grep -oE 'is_none_or|LazyLock|LazyCell|split_at_checked|take_if|next_multiple_of'`.
  MSRV 1.77; `ci.sh` clippy `-D warnings` implies `-D clippy::incompatible_msrv` (cargo test does
  not catch it). `is_none_or`(1.82)→`map_or(true, …)`; lazy statics→the `OnceLock` accessor idiom.
- **A guard held across a callee that re-takes it = self-deadlock (invisible to green tests).**
  `accept_link_inner` held the non-reentrant `lifecycle_guard` across `materialize_accepted_link →
  update_note_inner` (which re-acquires it) → a valid Accept hung forever; every accept TEST hit
  only refusal paths, none a SUCCESSFUL accept, so cargo test stayed green. When you take
  `lifecycle_guard`, SCOPE it in a `{ }` block around only the gate+db-write and DROP it before any
  materialize/note-write callee (the `link_items_inner`/`unlink_items_inner` idiom); and add a test
  that drives the SUCCESS path, not just the error paths.

## Run journal
<!-- Append-only, newest first. -->

### [2026-08-31 build/test perf] a `rerun-if-changed` on a path that DOES NOT EXIST is permanent staleness
- **Pattern:** `build.rs::build_swift_helper` printed `cargo:rerun-if-changed={src_rel}` BEFORE its
  own `if !src.exists() { return; }` guard. One of the five helpers is `afm/afm.swift`, deliberately
  absent (needs the macOS 26 SDK) and documented in-file as a "HARMLESS NO-OP". Cargo reports a
  missing watched path as `StaleItem::MissingFile`, which never clears — so the build script re-ran
  on EVERY cargo invocation (recompiling 4 Swift helpers × 2 arches, ~12 s) and dragged a full
  recompile + relink of the 330k-line app crate with it. Measured: a no-op `cargo test --lib
  --no-run` cost **18.6 s**; with the declaration moved below the guard, **0.33 s**. It was paid by
  every `cargo test`/`clippy`/`build`, every dev-watcher rebuild, every harness check, every agent
  loop iteration and three steps of `scripts/ci.sh`.
- **Caught by:** `CARGO_LOG=cargo::core::compiler::fingerprint=trace cargo test --lib --no-run`,
  which names the offending path outright (`stale: missing ".../afm/afm.swift"` →
  `dirty: FsStatusOutdated(StaleItem(MissingFile { … }))`).
- **Lesson:** when a build "feels slow", FIRST measure a no-op build. A no-op that is not
  sub-second is a fingerprint bug, not crate size, and the fingerprint log names the cause in one
  command — do not start tuning profiles, linkers, codegen-units or `--jobs` before that check.
  Never declare `rerun-if-changed` on a conditional/optional path; put the declaration after the
  existence guard. Regression oracle: the incremental-no-op check in `scripts/ci.sh` fails when an
  unchanged tree recompiles anything.

### [2026-08-31 build/test perf] `cipher_memory_security` cost 5x, and the test suite runs FASTER single-threaded
- **Pattern:** two independent findings from profiling the 3548-test suite (699 s serial). (a) Every
  `Db::open_with_key` set `PRAGMA cipher_memory_security = ON`, which swaps SQLite's allocator for
  SQLCipher's: `mlock()` on every malloc, `memset(0)` + `munlock()` on every free. On the 399-test
  `storage::db` module that was 124.2 s vs 24.0 s without it. (b) `--test-threads=1` is FASTER than
  the default: 199 s serial vs 296 s parallel on 16 cores, because SQLite/SQLCipher serialize on
  global mutexes (memstatus allocator lock, SQLCipher's provider/rand mutex). Full suite after both
  the pragma removal and `[profile.dev.package.libsqlite3-sys] opt-level = 3`: **699 s → 199 s**.
- **Caught by:** per-module timing against the built test binary directly
  (`target/debug/deps/meetnotes_lib-<hash> --test-threads=1 <module>`), plus
  `RUSTC_BOOTSTRAP=1 <binary> -Z unstable-options --report-time` for per-test times on stable.
- **Lesson:** a uniform ~300 ms per test is a fixed SETUP cost, not test logic — bisect it by
  timing one trivial test in the module, then diff that module's helper against a cheap one
  (`mem_db()` in-memory was 15 ms; the file-backed `open_with_key` helper was 300 ms). Two
  plausible-sounding causes were measured and REJECTED: `PRAGMA synchronous = OFF` changed nothing
  (123.9 s vs 124.2 s — it was never fsync), and clippy/test do NOT thrash a shared `target/`
  (both no-op at 0.33 s once warm). Measure before you optimise, and record what you disproved.

### [2026-07-20 documents as link targets — PR #415] `documents.updated_at` is NULL → recency ORDER BY silently degrades
- **Pattern:** added a documents leg to `list_link_candidates_visible` mirroring the notes leg,
  incl. `ORDER BY d.updated_at DESC, d.id ASC`. But `insert_document` only writes `created_at`
  (columns: `id,folder_id,name,text,kind,text_blob,created_at`) — it NEVER sets `updated_at`, so for
  every real ingested document `updated_at IS NULL` and the empty-prefix "recency browse" collapsed to
  the `d.id ASC` (UUID) tiebreaker. The notes leg is fine because `insert_note` sets
  `updated_at = created_at`. Green tests hid it — each test query returned ≤1 doc, so intra-leg order
  never mattered.
- **Caught by:** adversarial-verifier (LOW finding, `.claude/tmp/link-documents-related/adversarial-verify.json`).
- **Lesson:** documents and notes share the `documents` table but do NOT share column-population
  invariants — a `document` row can have `NULL` `updated_at`/`title` where a `note` row never does.
  Any ORDER BY / COALESCE / filter you copy from the notes leg to a documents leg must assume those
  columns are NULL: order documents by `COALESCE(d.updated_at, d.created_at) DESC`, title by
  `COALESCE(NULLIF(TRIM(d.title),''), d.name)`. A ≤1-row-per-query test set can't catch an intra-leg
  ordering bug — seed 2+ same-leg rows with distinct timestamps when the ordering is load-bearing.

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
- **Lesson:** the bullets above; full detail in `.claude/rules/rust-tauri.md` + `lock-model.md`.
- **Status:** distilled (2026-07-02)

### [2026-07-09 brain-p0-hotfixes] Scope resource bounds to the surface that needs them
- **Pattern:** a wall-clock timeout added to `MistralReasoner` for the LIVE path was wired at the lowest seam (`reason_with_opts`) and silently applied to EVERY local generation — FullyLocal note-gen (legit 30–90s) and the Ask floor (200k-char prefill) would have started failing `Unavailable`, and first-gen Metal shader compile (CLT-only Macs, `MISTRALRS_METAL_PRECOMPILE=0`) could always eat the first call.
- **Caught by:** adversarial-verifier (`.claude/tmp/brain-p0-hotfixes/adversarial-verify.json`, MAJOR) — builder's tests were green; the regression was on paths the new tests didn't cover.
- **Lesson:** carry per-call bounds IN the options struct (`GenOptions.timeout: Option<Duration>`, default None = old behavior; presets opt in), never as a blanket const at the lowest layer. Before bounding a shared seam, enumerate every caller class (live / notes / floor / background extraction) and ask which of them legitimately exceeds the bound.
- **Status:** journal

### [2026-07-10 brain-l1-retrieval] MSRV-gated std items: no `LazyLock` — use the repo's `OnceLock` accessor pattern
- **Pattern:** new module used `std::sync::LazyLock` for static regexes; `cargo test --lib` was green but the ci.sh clippy `-D warnings` stage failed with 36 MSRV errors (`LazyLock` stable since 1.80, crate MSRV 1.77).
- **Caught by:** scripts/ci.sh clippy stage (and initially MASKED by piping ci.sh through `| tail` — the pipe's exit code hid the failure; always capture ci.sh exit directly).
- **Lesson:** for lazy statics use the established repo pattern — `fn re_x() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| ...) }` (see redact.rs email_re). Check clippy.toml/Cargo.toml MSRV before reaching for newer std items.
- **Status:** journal

### [2026-07-10 brain-l2-memory] Derived artifacts EXPORTED OUTSIDE the DB need their own seal hooks
- **Pattern:** memory rollups (LLM synthesis over facts) were written to `memory_rollups` AND exported as plaintext `.md` into the vault. Purge-on-seal covered the source facts but not the synthesis: "regenerates next pass" was false (weekly scopes never re-touched; entity scopes only on NEW facts — a seal removes facts and is invisible to that detector). Locking a folder left a plaintext paraphrase of its content in Obsidian forever.
- **Caught by:** lock-security-reviewer + adversarial-verifier (both FAIL, independently reproduced end-to-end; builder's 1234 green tests never exercised seal-then-pass).
- **Lesson:** any derived artifact that leaves the DB (vault export, cache file) must be (1) purged at EVERY seal path (rows in-tx, files at the command layer via recorded exported_path — same layering as sealed-note .md deletion, incl. screen-share relock + startup reconcile + delete_meeting), and (2) covered by a change-detector for regeneration (fact-set hash), never "new data arrived" heuristics — seals REMOVE data. Test shape: synthesize → export → seal → assert row AND file gone → later pass recreates nothing.
- **Status:** journal

### [2026-07-10 fix-lock-reseal] A doc comment promising future lifecycle behavior hid a CRITICAL loss bug — relock never re-seals
- **Pattern:** `update_note_row`'s comment claimed "the seal blob is rebuilt on the next relock", but `seal_document` had exactly ONE call site reachable only from `lock_folder_inner`, which early-returns on an already-locked folder. Every write into a session-unlocked locked folder (notes editor autosave, re-summarize upsert, manual notes) left a STALE blob; relock then blanked the fresh plaintext against it — edits silently destroyed on screen-share auto-relock / "Lock all" / app exit. Three independent audit angles converged on it; the fix is seal-on-write (verify-before-destroy, fail-closed `AppError::Locked` when the session KEK is gone) at every write path into a locked folder, not re-seal-at-relock (the KEK is being wiped there).
- **Caught by:** post-merge re-verification audit (lock-security × seal-purge-matrix × integration), NOT by any per-PR review — the hole sat at the seam BETWEEN #228's write paths and the pre-existing relock lifecycle, which no single-PR diff review could see.
- **Lesson:** (1) never trust a comment that promises another code path will fix state up later — grep the call sites and prove reachability; (2) any WRITE path into a folder that can be `locked=1` needs the same seal discipline as the lock command itself (write = seal-on-write or refuse); (3) after a burst of concurrent PRs, run a whole-tree seam audit — per-PR gates compose, their blind spots don't.
- **Status:** journal

### [2026-07-12 brain-note-command-menu] A new action id silently absorbed by a `_ =>` catch-all — exhaustive-match a growing registry
- **Pattern:** expanding the note-assistant from 3 to ~19 actions, `build_note_assist_prompt` used explicit arms per action + a `_ =>` catch-all that WAS the `spinoff_note` prompt. The newly-added `custom` (free-text "Ask Brain to edit…") had NO arm → fell through to `_ =>` → the user's instruction was silently dropped, the model drafted an unrelated new note, and because `note_assist_shape("custom") == "replace"`, Accept DESTRUCTIVELY overwrote the selection. `cargo test --lib` (1532) + `ng lint` + `ng build` were all green.
- **Caught by:** adversarial-verifier (FAIL, RED-reproduced by calling `build_note_assist_prompt("custom", …)` and asserting the instruction was absent + the prompt was the spinoff one). The builder's own `..._weaves_instruction_and_variant` test was a FALSE-GREEN — its docstring claimed it covered `custom` but it only exercised `ask`, so it passed against the broken code.
- **Lesson:** (1) when a `match` over a string/enum registry GROWS, never let a semantic action hide behind a catch-all — give the catch-all arm a NAMED guard (`"spinoff_note" =>`) and make the true default an explicit error or a safe no-op, so a forgotten new id fails loudly instead of masquerading as another action; (2) a test that names an action in its docstring MUST assert on THAT action's output (here: the instruction is present AND the prompt is NOT the spinoff one) — a happy-path assert on a sibling action binds nothing; (3) for a destructive-on-Accept `replace` action, the "wrong prompt" failure mode is silent data-loss, not just a bad suggestion — treat routing bugs on replace paths as loss bugs.
- **Status:** journal

### [2026-07-17 claude-code-hermetic-mcp] A "hermetic" subprocess LLM leaked via MCP — --disallowedTools is a DENYLIST that fails open
- **Pattern:** the `claude_code` provider (default) spawned `claude -p ... --disallowedTools <10 built-in tools>` and claimed "hermetic, nothing leaves the Mac" (CLAUDE.md constraint #1). But `--disallowedTools` blocks only the named BUILT-IN tools — it does NOT cover MCP tools (`mcp__*`), and no `--strict-mcp-config` was passed. The `claude` CLI discovers MCP servers from FILES under `$HOME` (`~/.claude.json`, project `.mcp.json`), which `env_clear`/`harden_env` does not touch (they only strip env vars). So a user with ANY ambient MCP server registered in Claude Code (a self-referential `murmur` server at 127.0.0.1:8765, or Gmail/Drive/ClickUp/Slack) had every note-gen AND every `complete()` (Ask agentic loop, dossiers, briefs, auto-title, audit explanations, voice actions, memory/proactive) able to invoke those MCP tools, driven by meeting content on stdin — un-redacted, un-consented, un-ledgered egress that bypassed the WHOLE firewall (RedactingProvider + consent gate + egress ledger). Also a prompt-injection amplifier (anyone in a meeting can say words that steer the model to call an MCP tool) and the visible symptom: Ask hung on a `mcp__murmur__search_meetings` self-loop.
- **Caught by:** a user hitting a broken Ask on a live test → a 3-agent parallel security audit → dual-verify. The adversarial verifier LIVE-probed the real CLI (v2.1.197): the OLD `--disallowedTools` shape returned ambient Murmur context ("kontekst Murmura załadowany"), the NEW shape did not — direct runtime proof.
- **Lesson:** (1) when spawning an external LLM CLI for a "local/hermetic/nothing-leaves" provider, tool isolation must be an ALLOWLIST, not a denylist — pass `--allowedTools ""` (empty ⇒ no tool; fail-closed against FUTURE built-in tools too) AND `--strict-mcp-config` with no `--mcp-config` (⇒ zero ambient MCP servers). A denylist silently widens the moment the CLI ships a new built-in or the user adds an MCP server. (2) `env_clear` does NOT sandbox a subprocess's tool/MCP surface — MCP config is FILE-based under `$HOME`; the env and the tool surface are separate hardening axes. (3) route all spawns of the same binary through ONE builder seam (`build_claude_command`) and unit-test the exact args via `std::process::Command::get_args()` — drift-proof, so a future third spawn site can't re-open the hole. (4) LIVE-probe the real CLI when adding isolation flags: prove they don't ERROR the run (empty allowlist could have broken every note) AND that the old shape actually leaked. (5) env passthrough denylists (`NEVER_INHERIT_ENV`) should have a prefix catch-all (`MURMUR_*`) so a future secret var can't silently reach a cloud-bound child.
- **Status:** journal
