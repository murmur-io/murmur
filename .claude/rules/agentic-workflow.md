# Agentic workflow — Murmur (binding)

How to get maximum leverage out of the agent fleet on this project. The throughline: **the implementer never owns the verdict.** Every real bug here was caught by an *independent, adversarial* check — not by the agent that wrote the code.

## The executable workflow

Use `scripts/agent-harness` for every write task that is multi-step or benefits from independent verification. The runner, not chat state, owns the contract, isolated worktree, checks, reviews, bounded repairs, trace and final attestation:

- **Ship a feature** → `plan → build (backend and/or FE) → adversarial verify`. See `.agents/skills/ship-feature`.
- **Refactor / migrate** → `map the seams → change → re-verify the same behavior`.
- **Research / design** → fan out independent angles (`murmur-researcher`) → synthesize a decision-ready brief.
- **Release** → the `release-murmur` runbook stages can be driven as a `build → sign → notarize → publish` pipeline, or with fanned-out pre-release gates. See `.agents/skills/release-murmur`.

Default shape: `init → writer → checks → spec review → adversarial/risk review → max two repairs → final checks → hash-bound PASS → exact commit → PR → close`. The default vendor pair is **Claude writer → Codex reviewer**; the only supported reversal is Codex writer → Claude reviewer. Same-vendor production review and the selftest-only `fake` adapter are forbidden. Use `scripts/agent-harness commit` so the committed tree and QueaT identity are derived from the receipt. One writer owns one worktree. Parallelize read-only research; serialize writers, Cargo and anything sharing a file.

## One resource lane for every Rust/full-CI process

Parallel reasoning is useful; overlapping the always-compiled ML tree is not. Every agent-issued
Cargo/rustc/full-CI command must run through the repo-global supervisor:

```bash
scripts/agent-resource-run --chdir src-tauri -- cargo test --lib
scripts/agent-resource-run -- bash scripts/ci.sh
scripts/agent-dev-run -- npm run dev
```

The supervisor locks Git's common directory, so linked worktrees serialize; it caps build/test
parallelism, owns a fresh process group, and reaps descendants on timeout, signal, or normal exit.
The executable harness uses that exact same kernel lock for its deterministic checks. A long-lived
dev server is the one special case: `agent-dev-run` supervises it outside the flock and injects
private `cargo`/`rustc` PATH proxies whose individual processes acquire `agent-resource-run`.
Wrapping `npm run dev` in `agent-resource-run` is forbidden because it starves every other
worktree. Direct Cargo (including metadata), bare Tauri dev/build, and Angular production builds
are blocked by the shared hook guard; text searches and lightweight lint remain available.

## The adversarial-verify discipline (the core)

A change is not done because it compiles. It is done when an independent agent **tried to break it and failed**:

- Run the **real gates**: `scripts/agent-resource-run --chdir src-tauri -- cargo test --lib` (never bare `clippy --all-targets` — it thrashes the openssl/sqlcipher profile and times out), `npx ng lint`, `scripts/agent-resource-run -- npx ng build`.
- **Live-reproduce**, don't trust unit tests at the FE↔BE seam: drive the running app at `http://localhost:1420` via Playwright MCP with a mocked `window.__TAURI_INTERNALS__.invoke`; or launch the dev app and watch `/tmp/murmur-dev.log` for a clean boot (no abort).
- **RED before GREEN.** A bug fix needs a regression that fails on the old code and passes on the new. A test that passes against unpatched code didn't capture the bug.
- **Hunt the failure modes this project actually ships** (every one slipped past a green build+lint):
  1. **Seal content-loss** — keyed dedup destroying non-first rows on encrypt.
  2. **Sealed-content leak** — a read/asset path returning sealed data un-gated (incl. `audio_path` reaching `convertFileSrc`/the `asset:` protocol, which bypasses every backend command).
  3. **macOS FFI abort** — an unrecognized-selector `NSException` crossing FFI ("Rust cannot catch foreign exceptions") and aborting at launch.
  4. **Unguarded IPC effect** — an effect-orchestrated fetch without a stale-result guard
     (late response overwrites newer state; NG0600/`allowSignalWrites` itself is gone since v19).
  5. **Import-cycle `ɵcmp`** — mutually-recursive standalone components each in the other's `imports` (needs `forwardRef`).
  6. **Opacity bleed** — a popover/modal using the frosted `.card` instead of an opaque `--surface-overlay`.
  7. **Prod-only CSP style break** — Angular component styles disappear in packaged WebKit when
     a nonce is injected into `style-src`; a styled shell screenshot is not route-content proof.
- The **adversarial-verifier** agent owns PASS/FAIL. For anything touching the lock model or crypto, the **lock-security-reviewer** is a required second gate. The implementing agent self-checks but **must not self-certify**.

## Trust code, not docs

The hard-won lesson on this repo: **the docs were repeatedly wrong.** `docs/STATUS.md` and friends drift. When a claim is load-bearing, open the file (`file:line`) and confirm it against the current tree. Distrust your own first read, too.

**Cite by SYMBOL, not line number.** Commands and storage are split across growing domain modules
under `commands/` and `storage/`; `grep` the symbol name (`fn meeting_is_unlocked`,
`visibility_clause`) before trusting any prose anchor. A line citation is a hint, never a promise
about the current row. (The audit that surfaced this:
`docs/research/2026-07-02-claude-setup-audit.md`.)

## Honesty bar

Some things genuinely cannot be verified headless: real mic capture, live ScreenCaptureKit, the **Touch ID** prompt, lock-at-rest behavior, and whether screen-share auto-relock fires on a real Zoom/Meet share — these need a **signed build on a real Mac**. Say so plainly; don't claim a green unit test proves them. "Needs a signed build / a real Mac / recorded evidence" is the honest bar.

## Constraints the fleet must respect

- Commits/PRs authored **only** by `QueaT <kgm004a@gmail.com>`; **no AI co-author trailers**. `gh` active account = `JakubGawr`.
- **Never push to the `murmur` trunk directly** (the canonical `block-bash` guard, exposed through
  both vendor hook adapters, refuses it with exit 2) — merge via a PR (`gh pr create` → `gh pr merge`).
- `com.meetnotes.app` is immutable (TCC/Keychain continuity).
- No new npm packages or crates without explicit user approval.
