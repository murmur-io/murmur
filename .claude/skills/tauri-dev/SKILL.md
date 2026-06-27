---
name: tauri-dev
description: Run and iterate Murmur locally (Tauri 2.11 + Angular 18 zoneless). The exact dev-run recipe — MURMUR_DEV_DEK to avoid keychain re-prompts, source ~/.cargo/env, ng on :1420 + MCP on :8765, the dev biometric degradation, the `cargo test --lib`-not-`clippy --all-targets` inner loop, clean-relaunch (pkill + free ports), and reading the boot/abort log. Use whenever the user wants to start/run/serve Murmur in dev, debug a launch/abort, relaunch cleanly, or run the Rust test loop.
---

# /tauri-dev — run & iterate Murmur in dev

Murmur is a Tauri 2.11 desktop app: a Rust core (crate `murmur`, bin `Murmur`, lib
`meetnotes_lib`) hosting an Angular 18 **zoneless** webview. `npm run dev` (= `tauri dev`)
builds the Rust binary, serves Angular on **http://localhost:1420**, and the app boots the
read-only MCP server on **127.0.0.1:8765**.

## The dev-run recipe (copy-paste)

```bash
source "$HOME/.cargo/env"
cd /Users/jakubgawronski/Projects/meetnotes
MURMUR_DEV_DEK=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef \
  npm run dev 2>&1 | tee /tmp/murmur-dev.log
```

- `source ~/.cargo/env` — puts `cargo` on PATH (the shell here isn't a login cargo shell).
- **`MURMUR_DEV_DEK`** (64 hex chars) — the **whole-DB SQLCipher DEK** as a fixed dev value,
  read by `get_or_create_db_dek` in `src-tauri/src/secrets/keychain.rs` (the
  `MURMUR_DEV_DEK` env hatch, ~line 26). **Why it matters:** without it, the DEK lives in
  the macOS keychain with an ACL bound to the binary's code signature. Each `tauri dev`
  rebuild produces a *new* ad-hoc signature, so macOS re-prompts to authorize the keychain
  item on every relaunch. The fixed env DEK bypasses the keychain entirely for dev. There is
  a sibling `MURMUR_DEV_KEK` hatch (~line 51) for the per-folder master KEK if you're
  iterating on lock/unlock without Touch ID.
- `tee /tmp/murmur-dev.log` — capture boot output so you can grep for an abort.

> The env DEK/KEK hatches are **dev-only** (`#[cfg(debug_assertions)]`-gated in
> `keychain.rs`). Never bake them into a release — release builds get the real keychain
> DEK released at launch.

## Ports

- **1420** — Angular dev server (`ng serve --port 1420`, the `start` npm script).
- **8765** — Murmur's MCP server (`src-tauri/src/mcp.rs`, `127.0.0.1:8765`, read-only,
  visibility-gated). If a stale process holds it, the app's MCP bind fails — free it (below).

## Dev-mode degradations (so you don't chase ghosts)

- **Touch ID / biometric degrades to `Ok(true)`** in dev/unsigned builds
  (`src-tauri/src/biometric.rs`, objc2 `LAContext`). So `unlock_folder` / `unlock_meeting`
  "succeed" without a real fingerprint. **Lock-at-rest, Touch ID, and screen-share
  auto-relock only TRULY verify on a Developer-ID-signed build** (stable signature) — see
  `/release-murmur`. Don't conclude "lock works" from a dev run alone.
- **Screen-share auto-relock** (`src-tauri/src/screenshare.rs`) is best-effort and uses pure
  CoreGraphics C funcs — it runs in dev but the heuristic is only meaningful with a real
  screen-share session.

## The inner test loop — `cargo test --lib` ONLY

```bash
source "$HOME/.cargo/env"
( cd src-tauri && cargo test --lib )      # ~127 lib tests, fast
```

> **CALLOUT — do NOT run `cargo clippy --all-targets` in the iterate loop.** It rebuilds the
> test/bench targets against the openssl + sqlcipher profile and **thrashes/times out**.
> Use `cargo test --lib` while iterating. The full `clippy --all-targets -- -D warnings`
> belongs only in the release gate (`scripts/ci.sh`), run once before shipping.

Other gates when you need them: `npx ng lint`, `npx ng build`, and the full
`bash scripts/ci.sh` (clippy + tests + lint + build + headless E2E) before a release.

## Clean relaunch (kill stragglers + free ports)

A crashed or backgrounded run can leave the binary holding `target/` or the MCP port:

```bash
pkill -x Murmur ; pkill -f 'tauri dev' ; pkill -f 'target/debug/Murmur' || true
lsof -ti :1420 -ti :8765 | xargs kill -9 2>/dev/null || true   # free the ports
```

Then re-run the dev recipe above. (Also do this before a release build — a live dev server
holds the cargo `target` lock; see `/release-murmur` stage 6.)

## Reading the boot / abort log

```bash
tail -n 80 /tmp/murmur-dev.log
grep -nE 'panic|abort|Error|unrecognized selector|SQLCipher|PRAGMA key' /tmp/murmur-dev.log
```

What to look for:
- **`unrecognized selector … NSException` → instant abort at launch.** Hard-won lesson:
  Rust cannot catch foreign (ObjC) exceptions, so a bad `msg_send` aborts the process.
  Cause seen before: calling an iOS-only selector (`NSScreen.isCaptured`) on macOS. The fix
  pattern lives in `screenshare.rs`/`biometric.rs` — prefer CoreGraphics/CoreFoundation C
  funcs; if `msg_send` is unavoidable, guard with `respondsToSelector:` /
  `class_getInstanceMethod` before sending.
- **SQLCipher / `PRAGMA key` errors** → the DEK is wrong. Confirm `MURMUR_DEV_DEK` is set
  (64 hex) and matches the DEK the DB was first created with; a mismatched key fails to
  decrypt (`storage/db.rs` `open_with_key` sets `PRAGMA key` FIRST).
- A clean boot prints the Angular dev-server URL (`:1420`) and the MCP listen line (`:8765`).

## Rules

- **Always `source ~/.cargo/env` first** and use **absolute** paths — the shell resets cwd
  between calls.
- **Always set `MURMUR_DEV_DEK`** for dev runs (skip the keychain re-prompt loop).
- **`cargo test --lib` for iterating; never `clippy --all-targets`** in the loop.
- **Don't trust dev for security claims** — Touch ID / lock-at-rest / screen-share need a
  signed build. State that honestly rather than implying a dev run verified them.
- **No PII in logs.** Don't paste meeting/transcript content into shared logs.
