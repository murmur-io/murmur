# Agentic workflow — Murmur (binding)

How to get maximum leverage out of the agent fleet on this project. The throughline: **the implementer never owns the verdict.** Every real bug here was caught by an *independent, adversarial* check — not by the agent that wrote the code.

## When to reach for the Workflow tool

Use the **Workflow tool** (not a single inline pass) whenever work is multi-step or benefits from independent verification:

- **Ship a feature** → `plan → build (backend and/or FE) → adversarial verify`. See `.claude/skills/ship-feature`.
- **Refactor / migrate** → `map the seams → change → re-verify the same behavior`.
- **Research / design** → fan out independent angles (`murmur-researcher`) → synthesize a decision-ready brief.
- **Release** → the `release-murmur` runbook stages can be driven as a `build → sign → notarize → publish` pipeline, or with fanned-out pre-release gates. See `.claude/skills/release-murmur`.

Default shape: a **build** phase, then a **verify** phase done by a *different* agent. Backend (Rust) and FE (Angular) are usually disjoint files → run them in parallel, then serialize anything that shares a file.

## The adversarial-verify discipline (the core)

A change is not done because it compiles. It is done when an independent agent **tried to break it and failed**:

- Run the **real gates**: `cargo test --lib` (never `clippy --all-targets` — it thrashes the openssl/sqlcipher profile and times out), `npx ng lint`, `npx ng build`.
- **Live-reproduce**, don't trust unit tests at the FE↔BE seam: drive the running app at `http://localhost:1420` via Playwright MCP with a mocked `window.__TAURI_INTERNALS__.invoke`; or launch the dev app and watch `/tmp/murmur-dev.log` for a clean boot (no abort).
- **RED before GREEN.** A bug fix needs a regression that fails on the old code and passes on the new. A test that passes against unpatched code didn't capture the bug.
- **Hunt the failure modes this project actually ships** (every one slipped past a green build+lint):
  1. **Seal content-loss** — keyed dedup destroying non-first rows on encrypt.
  2. **Sealed-content leak** — a read/asset path returning sealed data un-gated (incl. `audio_path` reaching `convertFileSrc`/the `asset:` protocol, which bypasses every backend command).
  3. **macOS FFI abort** — an unrecognized-selector `NSException` crossing FFI ("Rust cannot catch foreign exceptions") and aborting at launch.
  4. **NG0600** — a signal written in an `effect()` without `{ allowSignalWrites: true }`.
  5. **Import-cycle `ɵcmp`** — mutually-recursive standalone components each in the other's `imports` (needs `forwardRef`).
  6. **Opacity bleed** — a popover/modal using the frosted `.card` instead of an opaque `--surface-overlay`.
- The **adversarial-verifier** agent owns PASS/FAIL. For anything touching the lock model or crypto, the **lock-security-reviewer** is a required second gate. The implementing agent self-checks but **must not self-certify**.

## Trust code, not docs

The hard-won lesson on this repo: **the docs were repeatedly wrong.** `docs/STATUS.md` and friends drift. When a claim is load-bearing, open the file (`file:line`) and confirm it against the current tree. Distrust your own first read, too.

## Honesty bar

Some things genuinely cannot be verified headless: real mic capture, live ScreenCaptureKit, the **Touch ID** prompt, lock-at-rest behavior, and whether screen-share auto-relock fires on a real Zoom/Meet share — these need a **signed build on a real Mac**. Say so plainly; don't claim a green unit test proves them. "Needs a signed build / a real Mac / recorded evidence" is the honest bar.

## Constraints the fleet must respect

- Commits/PRs authored **only** by `QueaT <kgm004a@gmail.com>`; **no Claude trailers**. `gh` active account = `JakubGawr`.
- **Never push to the `murmur` trunk directly** (a `block-bash` hook forbids it) — merge via a PR (`gh pr create` → `gh pr merge`).
- `com.meetnotes.app` is immutable (TCC/Keychain continuity).
- No new npm packages or crates without explicit user approval.
