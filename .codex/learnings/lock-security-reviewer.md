# Learnings — lock-security-reviewer

## Recurring patterns
<!-- Curated, binding. Prepended to every dispatch. Keep ≤ ~20 bullets. -->

- **The two hard rules.** (1) A new content read OR export path MUST be gated —
  `meeting_is_unlocked` (commands) or `visibility_clause` (db/MCP). (2) Any new seal MUST
  verify-before-destroy (prove decryptable byte-identical before blanking plaintext) and be
  reversible by the matching unseal.
- **The `convertFileSrc` trap is a real, previously-closed leak.** The masked DTO sets
  `audio_path: None` ON PURPOSE — the FE feeds `audio_path` to Tauri's `asset:` protocol, which
  serves the file WITHOUT `export_audio` or the gate. Never hand the FE an on-disk path for a locked
  meeting; never add an asset/`convertFileSrc` serve path that skips the gate.
- **Sealed-not-unlocked leaks NOTHING** across every read: detail, segments, timeline, audio, MCP,
  graph, search. Confirm the masked DTO (`locked:true`, title "🔒 Locked", no note/segments/timeline,
  `audio_path:None`) on every path the change touches.
- **Seal covers note + transcript segments + timeline + audio WAV.** Each blob verified decryptable
  BEFORE the plaintext column is blanked / the `.md` deleted / the WAV removed. A plaintext WAV that
  briefly survives in the scoped asset dir must still be unreachable (that's what nulling the path
  buys).
- **Two encryption layers — don't conflate.** Whole-DB SQLCipher (DEK, keychain, whole session) vs
  per-folder content-key (CK wrapped by the master KEK, released only by Touch ID on a signed build).
  A locked folder must be unreadable even with the DB open.
- **No PII in lock/seal logs** — IDs, stages, counts only; never note/transcript text, titles, keys,
  or DEK/KEK/CK material. A debug log must not become the leak the seal prevents.
- **Identity is immutable.** Keychain service `com.meetnotes.app`; the `MURMUR_DEV_DEK`/`MURMUR_DEV_KEK`
  hatches are debug-only, must not be reachable in release, must not be logged.
- **This review is READ-ONLY and is the REQUIRED gate** before merging any lock/crypto/visibility
  change. Grep the symbol, not the line number (the rules' `db.rs:1269`-style anchors have drifted).

## Run journal
<!-- Append-only, newest first. -->

### [2026-07-05 Touch ID MK cache — #196] a master secret gains an at-rest cache
- **Pattern:** The sharing-account MK moved from RAM-only to a biometric-gated Keychain item (a
  deliberate posture change for UX). Review confirmed it was sound — user-presence + `WhenUnlocked-
  ThisDeviceOnly` (non-syncable, no iCloud), the DEBUG plaintext dev-file path `#[cfg]`-compiled OUT
  of release, cleared-on-logout, FFI-safe (SecItem* C funcs only, no `msg_send`), fail-closed, no
  keychain-item-name collision with the folder-lock KEK — but caught a LOW: `account_logout` ran the
  fallible `clear_*()?` keychain deletes BEFORE `*session = None`, so a rare keychain error early-
  returns and leaves the live MK in RAM.
- **Caught by:** lock-security-reviewer.
- **Lesson:** When a secret gains an at-rest cache, audit the full checklist: ACL (user-presence +
  device-only + non-syncable), the DEBUG/plaintext hatch is `#[cfg]`-compiled-out-of-release, cleared
  on logout (+ consider rotation/password-change), never logged, held `Zeroizing`, fails-closed, and
  no keychain-item-name collision with existing secrets. ORDERING: drop the in-RAM secret FIRST, then
  run any fallible clear — a `?` must never skip the zeroize. Adjacent: a CSP gaining `'wasm-unsafe-eval'`
  is wasm-compile-only (not `eval`) → not a broadened XSS surface; a self-hosted crypto lib should be
  sha256-pinned (a CI test).
- **Status:** journal

### [2026-07-02 seed] Distilled from lock-model.md
- **Pattern:** gate-every-read + verify-before-destroy + the convertFileSrc asset-path leak.
- **Caught by:** operator (seeding the loop). (Historically this gate caught a Resummarize
  content-loss in PR #95 and the @brain-substring egress in #98.)
- **Lesson:** the bullets above; full detail in `.codex/rules/lock-model.md`.
- **Status:** distilled (2026-07-02)
