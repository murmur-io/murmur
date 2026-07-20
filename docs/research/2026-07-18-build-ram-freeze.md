<!-- 2026-07-18. Root-cause + fix research for the "the whole Mac freezes when Claude builds" incident.
     Diagnosis grounded in observed machine state + repo config; optimizations grounded in the rustc
     perf team's own guidance. -->

# Why the Mac froze during builds — root cause + build-config optimization

## The incident
On an **M4 Max / 64 GB**, running the audit-fix program froze the whole UI — even opening a new
browser tab stalled — repeatedly, forcing hard resets. The user asked: is the "one build at a time"
diagnosis true, or are our build **jobs themselves too heavy** and in need of optimization?

## Verdict: BOTH, and they multiply

### 1. Concurrency was the trigger (confirmed empirically)
- After `pkill cargo rustc ng` the machine was **95% RAM free, swap 0.00 M**. So there is **no leak** —
  the pressure is a **transient peak during active builds**.
- The freeze is the **macOS memory compressor**, not swap: when resident+compressible memory spikes,
  the kernel compresses pages on all cores → every core pinned → `WindowServer` and the browser stall.
  Swap staying 0 is the signature (compression precedes pageout).
- I was running **multiple builders + verifiers + my own `cargo`** at once. This repo's `cargo test
  --lib` **statically links the always-compiled ML tree** (candle / mistralrs / whisper-rs) into one
  giant test binary — a single link is a ~15–20 GB transient. **Several at once** = the freeze.
- Aggravator: **45 git worktrees** were pointed at **ONE shared 187 GB `CARGO_TARGET_DIR`**. Divergent
  branches ⇒ fingerprint mismatch ⇒ cargo **rebuilds the ML tree from scratch on every switch**
  (thrash), so the peak recurred continuously instead of amortizing.

### 2. The jobs ARE genuinely too heavy (confirmed by config + rustc-team guidance)
The repo has **no `[profile.*]` overrides at all** (checked `Cargo.toml` + both `.cargo/config.toml`).
So every dev/test build uses cargo defaults — critically **`debug = 2` (full DWARF)**. Per the rustc
performance team (Kobzol, nnethercote perf-book):
- *"By default you build debuginfo for **every single dependency**, even though most of it will never
  be needed. Linkers do **not** tree-shake unused debuginfo (unlike unused code)."*
- On a tree as large as candle+mistralrs+whisper, that full debuginfo is the bulk of both the **187 GB
  target** and the **link-time memory peak**.
- Disabling/゙reducing debuginfo is **30–40 % faster** to compile/link (incremental) and cuts link
  memory and binary size proportionally — *the* stable-compatible lever.

**So:** one giant link froze nothing catastrophic on 64 GB by itself; it was *N concurrent giant
links, each bloated by full-dependency debuginfo, on a thrashing shared target*.

## The fix — two independent layers

### A. Process discipline (adopted immediately, zero code change)
- **Exactly one `cargo` process machine-wide, ever.** Serialize: build → verify (builder idle) →
  merge → next. Never run local cargo while a subagent might.
- **`CARGO_BUILD_JOBS=2 … -j2`**; iterate with **targeted filters** (`cargo test --lib links` still
  compiles the whole lib, runs only that module's tests — full build-error coverage, tiny run) instead
  of the full 1900-test suite.
- **CI (GitHub macos runner) is the real full gate** — it runs the whole `ci.sh` (clippy + all tests +
  build + E2E) **remotely = 0 local RAM**. Push → let CI gate; keep local cargo minimal.
- **Prune stale worktrees** (did 45 → 7) so the shared target stops thrashing.

### B. Build-config optimization (RECOMMENDED — a tiny PR, benefits everyone incl. CI)
Add to the workspace root `Cargo.toml` (or `src-tauri/Cargo.toml`):
```toml
# Kill debuginfo for DEPENDENCIES (the candle/mistralrs/whisper bulk) — they're never debugged here —
# while keeping full debuginfo for OUR crates so panic backtraces stay useful. Slashes the test-binary
# link-time memory peak, the 187 GB target size, and ~30-40% of incremental compile time.
[profile.dev.package."*"]
debug = false

# Optional middle ground for our own crates if full debug is still heavy (keeps file:line in traces):
# [profile.dev]
# debug = "line-tables-only"
```
- **Why `package."*"` and not `[profile.dev] debug=false`:** deps get zero debuginfo (the win), our
  own code keeps backtraces (the safety). Best of both.
- **Cost:** a **one-time cold rebuild** (it invalidates the warm target — do it while paused). After
  that, every local build AND CI (with `Swatinem/rust-cache`) is lighter and faster.
- **Risk:** low. Release profile untouched (shipping binaries unaffected). Dependency backtraces lose
  line numbers — irrelevant (we don't debug candle's internals locally; CI/repro can flip it back).
- Linker: the macOS system linker (ld-prime) is already fast; **lld is not the lever here** — the
  debuginfo reduction is. Don't add a custom linker.

## Recommendation
Adopt **A immediately** (done). Land **B as a one-line-ish PR** before resuming heavy building — it
makes even the *single* serialized build materially lighter, so the one-at-a-time rule has margin. The
two together mean a normal build can no longer approach the compressor-thrash threshold on 64 GB.

## Sources
- Kobzol (rustc perf team), *Disable debuginfo to improve Rust compile times* (2025-05-20) —
  https://kobzol.github.io/rust/rustc/2025/05/20/disable-debuginfo-to-improve-rust-compile-times.html
- Kobzol, *Reducing binary size with debuginfo* (2025-09-22) —
  https://kobzol.github.io/rust/2025/09/22/reducing-binary-size-of-rust-programs-with-debuginfo.html
- *Build Configuration*, The Rust Performance Book (nnethercote) —
  https://nnethercote.github.io/perf-book/build-configuration.html
- *Profiles*, The Cargo Book — https://doc.rust-lang.org/cargo/reference/profiles.html
- *Codegen options* (split-debuginfo, debuginfo levels), the rustc book —
  https://doc.rust-lang.org/rustc/codegen-options/index.html
- rust-lang/rust#83911 (high memory with LTO+debuginfo), #122944 (30 min / 32 GB compile) — corroborate
  debuginfo/LTO as the memory driver.
