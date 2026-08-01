<!-- Generated 2026-07-31 via a 15-agent workflow (4 code auditors, 4 web researchers, 3-proposal design panel, 2 judges, 1 adversarial critic). Every number below was re-verified by hand against trunk `murmur` before publication; corrections to the agents' findings are marked. -->
# Harness simplification — diagnosis and remediation plan

> ## ⚠️ PHASE 1 IS SUPERSEDED — read `2026-08-01-harness-plan-verification.md` first
>
> An 11-agent architecture research + adversarial verification pass (2026-08-01) tested every
> Phase-1 item against external evidence and against this machine. Outcome:
>
> - **P1 (shared build root) — REJECTED.** Reproduced live: two checkouts of one repo layout
>   collide in a shared `CARGO_TARGET_DIR`; cargo reports task A `Fresh` while the artifact was
>   compiled from task B's source. It would have made the receipt assert a verdict over the wrong
>   binary. Replaced by **sccache + per-task target** (R2).
> - **P4 (path-gate the selftests) — WITHDRAWN.** Its premise is false: `verifier.py:475`
>   `_harness_surface` already path-gates them, and one of the five targets (`ci.sh:70-78`) is
>   `agent-remote-audit`, not a selftest. **Finding D3 below is wrong — the real count is 2x, not 3x.**
> - **P2, P3, P5 — confirmed, mechanisms rewritten** (R3, R4, R5).
> - **Two new items larger than anything here:** the 67.8 GB fixture leak (R0) and evidence-store
>   GC. The `risk_classification.lock` widening is promoted out of Phase 2 to R1, un-gated.
>
> The executable plan is now **R0–R5** in that document. Everything below stays valid as the
> diagnosis and as the Phase-2 / delete-keep record, with D3 corrected as noted.
>
> **Status:** decision document. Supersedes the latency half of
> `2026-07-24-harness-fast-lanes.md`, `2026-07-27-harness-simplification-root-cause.md`,
> and `2026-07-30-harness-v2-latency-optimization.md`. It does **not** supersede their
> trust-model conclusions, which still hold.
>
> **Verdict:** the harness is not too rigorous. It is expensive in the wrong place and
> blind in the right one. Phase 1 is five small edits, deletes nothing, touches no
> security path, and removes the dominant cost. The 26k-LOC deletion is Phase 2 and is
> *contingent on Phase 1 having already banked the gain* — that inversion is the only
> structural difference between this plan and the four before it.

---

## 0. The mechanism, in four verified steps

This is the whole diagnosis. Everything else is detail.

1. **`cli.py` has no `reopen` and no `amend`.** Subparsers are exactly
   `open, plan, status, commit, guard, verify, resume, clean, doctor, metrics, selftest`
   (`.agents/harness/cli.py:3957-4037`). Both `prepare_plan` (`cli.py:672`) and
   `verify_task` (`cli.py:1759`) refuse when
   `state["status"] in verifier.V2_TERMINAL_STATES | {"COMMITTED"}`.
   → **A task is strictly one commit.** Every post-CI fix needs a brand-new task id.

2. **A new task id means a new `task_dir`.** `runtime.py::_check_runtime_paths(task_dir)`
   (`runtime.py:1011-1027`) returns `task_dir/runtime/checks/cargo-target`, exported as
   `CARGO_TARGET_DIR` at `runtime.py:1263`.
   → **Every new task id starts from an empty Cargo target dir.**

3. **The ML tree is always compiled** (the feature gates were removed; mistralrs/candle
   ship by default — `CLAUDE.md`, `rust-tauri.md §9`).
   → **Empty target + always-compiled ML tree = a full cold build, measured at 6–11 GB.**

4. **Therefore one feature = N cold builds.** The `reminders-smart-inbox` feature consumed
   **11 task ids**; `codex-cloud-provider` ran v1…v8; `perf-stop-lifecycle` ran v2…v2f.

**Measured consequence** — 23 separate `cargo-target` directories, **96.4 GB**, inside
`../.murmur-agent-driver/.git/agent-harness/`:

```
 11.5 GB  codex-cloud-provider-v7          8.5 GB  reminders-smart-inbox
  9.7 GB  debug-map-production-trust       7.5 GB  reminders-...-final-scope2
  9.4 GB  perf-stop-lifecycle-v2f          6.2 GB  reminders-...-current-base
  9.0 GB  codex-cloud-provider-v8          5.8 GB  reminders-...-ci-runtime-harness-fix
  7.9 GB  codex-cloud-provider-v5          5.8 GB  reminders-...-merge
                                           2.9 GB  reminders-...-ci-followup-runtime
```

The `reminders` series alone rebuilt the same code from zero **six times, ~37 GB**.

**None of the four prior documents names this mechanism.** The 2026-07-30 doc's headline
fix ("pin the snapshot path") attacks the *smaller* half: the target dir was **already**
stable across attempts *inside* a task. The cost is per **task id**.

---

## 0b. Re-verification after the 1.1.0 release cleanup (2026-07-31, same day)

Every measurement above was re-run after the operator cut a release and deleted the task
worktrees. **Result: the code findings are unchanged, and the disk finding got sharper.**

| | Before cleanup | After cleanup |
|---|---|---|
| `.murmur-agent-tasks` | 69 G | **28 MB** |
| `.murmur-agent-driver/.git/agent-harness` | 178 G | **178 G — byte-identical** |
| ├ `cargo-target` | 96.4 G / 23 dirs | **96.4 G / 23 dirs** |
| ├ `runtime/checks/tmp` | 67.8 G / 25,366 sqlite | **67.8 G / 25,366 sqlite** |
| └ `clang-cache` | 13.1 G | **13.1 G** |
| Task dirs, `v2/tasks` | 35 | **81** (plus 52 v1 dirs under `tasks/`) |
| Free space | 53 GiB | 117 GiB |

Deleting worktrees reclaimed the 69 GB task root and **none** of the 178 GB evidence store.
**72% of the agent disk survives a full cleanup**, because it lives inside `.git/` where
`git clean`, `git worktree remove`, and the harness's own `clean` cannot see it. That is not
a corollary of the plan — it is the plan's disk claim demonstrated under field conditions.

Code anchors re-checked and **all unchanged**: `cli.py:672` / `cli.py:1759` refusal predicates
intact; 11 subparsers, still no `reopen`/`amend`; `runtime.py:1011-1012` still scopes
`cargo_target` to `task_dir`; CSP grep still returns **0**; `risk_classification.lock` still
omits `commands/mod.rs` and `commands/meetings.rs`; still 3 `PreToolUse:Bash` hooks;
`config.json:43` still `--workers=1` against `ci.sh:243` `--workers=2`.

**Two corrections to the agent findings, and one new datum:**

1. **Receipt-skip rate is 70–77%, not 91%.** Last 400 commits: 63 of 82 control-plane commits
   carry no `Harness-Verdict` (77%). Last 300: 44 of 63 (70%). Both windows say the same thing;
   the agents' 91% was wrong. The argument in §6 is unaffected.
2. **`.murmur-agent-tasks` is no longer a useful measurement surface** — it is empty. All future
   disk claims must be made against `.murmur-agent-driver/.git/agent-harness`.
3. **The 1.1.0 release is live evidence for §2 and §6.** Commits `33c308e`
   (*fix(app): address recording and UI regressions* — touching `commands/mod.rs`,
   `commands/audio.rs`, `audio/system.rs`, `summarize/provider.rs`, `ipc.service.ts`, three e2e
   specs) and `bef2fb0` (version bump) shipped through an **ordinary branch**
   (`fix/ask-drawer-viewport` → PR #532 → merge) with **no harness trailers**. A multi-file
   Rust + Angular + e2e change and a release both went through the plain PR flow the same day
   this document proposes making the default. Note also that `33c308e` edits
   `src-tauri/src/commands/mod.rs` — precisely the file inside the **D7 taxonomy hole** — and
   `lifecycle_tests.rs`, the fixture-leak file from item 0.

---

## 1. Diagnosis

| # | Finding | Verified evidence |
|---|---|---|
| **D1** | Eleven task ids per feature is **code-enforced**, not operator sloppiness. | `cli.py:672`, `cli.py:1759`; no `reopen`/`amend` subparser. Trunk commit `e862179 fix(reminders): satisfy strict Rust checks` carries `Harness-Task: reminders-smart-inbox-v2-ci-runtime-harness-fix-20260731` — a full task lifecycle for a clippy fix. |
| **D2** | Cold build cost is per-**task-id**, not per-attempt-snapshot. **Contradicts the 2026-07-30 doc.** | `runtime.py:1011-1027` + `:1263`. 23 cargo-targets = 96.4 GB. |
| **D3** | 27,800 LOC of control plane guarding a 273-LOC gate — **ratio 102:1**. | `v2_selftest.py` 6,678 > `cli.py` 4,057 (the file it tests). `runtime.py` 3,311, `verifier.py` 2,684, `hook_guard.py` 2,201, `harness-runtime-smoke.py` 2,085, `config_audit.py` 1,170, `verify-harness-attestation` 906. Control-plane selftests run **3×** per change: harness `verify`, `scripts/ci.sh:57,78,89,92,95`, then Actions re-running `ci.sh`. |
| **D4** | Interactive shell friction — **missed entirely by all four prior docs**, which audited the pipeline and never the shell. | `.claude/settings.json` wires **three** `PreToolUse:Bash` hooks (`block-bash.sh`, `secret-scan.sh`, `finish-guard.sh`); two early-return unless the command is `git commit`. `hook_guard.py::_unsupported_execution_indirection` refuses `$( )`, backticks, `<( )`, `xargs`, `find -exec`, `eval`, `source` **unconditionally on every command**. It blocked read-only `du`/`git log`/`wc -l` audit commands for three investigators *during this analysis*, and twice for the orchestrator. |
| **D5** | The "bootstrap trap" is **not** why nothing landed. | `cli.py:502` does refuse protected paths — but **63 of 82 control-plane commits (77%) carry no `Harness-Verdict` trailer** (last-300 window: 44 of 63 = 70%). The escape hatch is the norm, and the 1.1.0 release shipped the same way (§0b). *(Correction: agents reported 91%.)* The real reason: last `.agents/harness` commit is `ac496e6` (2026-07-29); the latency doc is dated 2026-07-30. **There was never an implementation window.** |
| **D6** | Rigor is applied in the wrong dimension. | `grep -rln 'dangerousDisableAssetCspModification\|style-src' src-tauri/src e2e scripts src` → **zero hits**. Incident class 4 (packaged-WebKit CSP style loss — shipped broken in 0.5.0, cost three failed fixes) has **no automated coverage**. Meanwhile class 2 (sealed-content leak) already has a 619-LOC deterministic oracle at `src-tauri/src/commands/tests/lock_read_gate_tests.rs`. 27,800 LOC does not catch class 4; ~60 LOC of Playwright would. |
| **D7** | A live hole in the risk taxonomy, currently masked by the always-on reviewer. | `config.json risk_classification.lock` omits `src-tauri/src/commands/mod.rs` (where `meeting_is_unlocked` lives) and `commands/meetings.rs` (`masked_detail`). A diff confined to the masked-DTO branch triggers **no** lock specialist today. **Any path-triggered demotion must widen these globs first.** |
| **D8** | Disk: the reclaimable half is invisible to every normal tool. **Re-verified after a real cleanup — see §0b.** | Before cleanup: `.murmur-agent-tasks` 69 G + `.murmur-agent-driver/.git/agent-harness` 178 G = **247 G** against 53 GiB free. After the operator deleted the worktrees: task root 69 G → **28 MB**, but the evidence store is **178 G, byte-identical**. Breakdown, unchanged across the cleanup: **96.4 G** cargo-target (23 dirs), **67.8 G** `runtime/checks/tmp`, **13.1 G** clang-cache. The tmp figure is **25,366 leaked `murmur-lifecycle-*.sqlite` fixtures** — a real product-test bug in `src-tauri/src/commands/tests/lifecycle_tests.rs`, amplified by per-task private `$TMPDIR`. Real evidence is ~0.4% of the store. |
| **D9** | Maintenance load. | Of the last 300 non-merge commits on `murmur` (2026-07-13 → 07-31): **56 control-plane-only (18.7%) + 7 mixed (2.3%)** — roughly **one commit in five maintains the tool, not the product**. |

> **Steelman, stated once and honestly:** the v2 engine is three days old (`29bfccc`/`f78c381`,
> 2026-07-28) and absorbed 43 of 44 lifetime harness commits in eight days. Every number here
> is a *break-in* measurement of an unstabilized system. That argues **for** Phase 1 (cheap,
> reversible, no deletions) and **against** rushing Phase 2.

---

## 2. The target flow

One loop. One default path. **One escalation branch, and CI chooses it — never the human.**

```bash
# ── ONCE PER FEATURE (not per attempt) ───────────────────────────────
git worktree add ../wt/<slug> -b <slug> && cd ../wt/<slug>

# ── THE LOOP (repeat freely; same path, warm cache, ~1–2 min) ────────
<edit>
make check            # nextest + ng lint + ng build; e2e only if src/ or e2e/ touched
                      # silent on pass, verbose on fail

# ── WHEN GREEN ──────────────────────────────────────────────────────
/code-review          # fresh-context subagent on the diff. ADVISORY.
git commit -m "…" && git push -u origin <slug> && gh pr create -B murmur

# ── CI PICKS THE LANE FROM THE CHANGED PATHS ────────────────────────
#   default                    : rust ‖ web  (~17 min)          → merge
#   lock|egress|protocol paths : + security-review job (BLOCKING)
#                                + `security-approved` label    → merge
#
#   CI red ⇒ ANOTHER COMMIT ON THE SAME BRANCH. Never a new task id.

gh pr merge --squash && git worktree remove ../wt/<slug>
```

Everything the operator must hold: **`make check` → `/code-review` → PR → CI decides.**
No task ids, no attempts, no plans, no snapshots, no receipts, no lanes, no abandon/clean
lifecycle.

---

## 3. Where Murmur actually sits

| Primitive | Murmur **today** | Loop-engineering model (Osmani / Steinberger / Cherny) | Vendor-recommended | **Proposed target** |
|---|---|---|---|---|
| **Automations / cadence** | None. Every step human-prompted. The harness is a *gate*, not a loop. | Steinberger: 3–8 parallel agents. Cherny: overnight fan-out, ~20% keep rate. | `/goal` stop conditions, `CronCreate`, `/loop`, Codex Automations → Triage inbox. | **Phase 1: none** (out of scope). Optional later: one nightly `discover` that *proposes only*. |
| **Worktrees** | Full **clone** per attempt (`verify-<64hex>/`, 79 snapshots) + per-**task** cargo-target (96 GB). | Steinberger **deleted** worktrees; parallel panes in one folder. | Native `git worktree`, `isolation: worktree`, auto-sweep. | **One worktree per BRANCH**, shares `.git`; build root shared **across** worktrees behind the existing flock. |
| **Skills** | 3 overlapping orchestration skills (`harness`, `pr-program`, `ship-feature`) + `agentic-workflow.md`. 1,019 always-on rule lines. | Steinberger: 4 slash commands, rarely used; ~800-line AGENTS.md = "organizational scar tissue". | Skills auto-load. Osmani: *every AGENTS.md line must trace to a specific failure*. | **One** `ship-feature`; rules **path-scoped** (`rust-tauri.md`→`src-tauri/**`, `angular-zoneless.md`→`src/**`); `lock-model.md` stays unconditional. |
| **Connectors / MCP** | Product MCP on `127.0.0.1:8765` (gated) + agent-side MCPs. | Steinberger: most MCPs are "a checkbox for the marketing department"; GitHub MCP = "23k tokens gone" vs `gh`. | MCP supported; context tax acknowledged. | **Keep the product MCP** (it is a feature). Prefer CLI over agent-side MCP. |
| **Subagents** | Tool-**free** reviewers + a typed-probe request/re-review round-trip. Up to 3 parallel. | Steinberger replaced subagents with terminal windows. Cherny: fan-out, ~80% discard. | Fresh-context reviewers **with** `Read/Grep/Glob/Bash`. Debiasing = fresh context, **not** tool removal. | Reviewers keep tools. **Delete the probe round-trip.** Generalist → advisory; **lock + egress specialists stay blocking**, dispatched by CI from paths. |
| **State / memory** | `events.jsonl` + attempts + receipts, 178 GB, **never rolled up** (no metrics artifact exists in the store). Task = 1 commit ⇒ no way to say "still doing this". | Steinberger commits straight to main. | Session resume/fork, checkpoints, `/rewind`. Evidence-in-transcript, not receipts. | **git is the state.** One branch = unlimited commits. Optional: one markdown line per in-flight item. |
| **Verification** | Every deterministic check + up to 3 LLM reviews **before every local commit**, then again in CI. LLM verdict binary-blocking. | Steinberger: none. Cherny: CI-green loops. Willison: red/green TDD + runtime demo. | Independent reviewer at the **top** of a 4-tier ladder, scaled to unattended duration. Explicit warning: a gap-hunting reviewer manufactures findings. | **Deterministic first, LLM on the residual.** 4 incident classes → machine oracles. LLM advisory by default; blocking only where a false negative is an *incident*. |
| **Merge authority** | Ambiguous: local receipt + `ci.sh` + Actions + a CI receipt-verifier. Receipt is **unsigned** (`ci.yml` says so verbatim), skipped by 77% of control-plane commits. | Steinberger: none. Osmani: *"The Verdict is mine"* — human at the merge boundary. | CI + human PR approval; `claude -p` non-interactive in CI. | **GitHub CI on the PR head is the only authority.** Human approves. Risky paths additionally require a `security-approved` label. |

### Where Murmur is genuinely better than the popular model — keep these

1. **619 LOC of deterministic lock-read-gate tests** asserting masked-DTO nulling and the
   `convertFileSrc` asset-protocol regression. No blogger in the primary literature has a
   machine oracle for a security invariant. Murmur has one and forgot it was there.
2. **RED-before-GREEN as a binding rule.** Willison converged on it independently.
3. **A real telemetry corpus** — 553 check executions with `duration_ms`. The failure is that
   nothing reads it, not that it doesn't exist.
4. **The one-cargo-at-a-time lane.** Independently re-derived by Huntley ("500 parallel
   subagents but only 1 for build/tests"). Justified by a recorded corruption incident.
5. **Risk-triggered specialist reviewers** — the verification-cost ladder *implemented*, while
   the bloggers only describe it.
6. **Isolated worktrees protecting a running dev app and a real SQLCipher DB.**

---

## 4. The plan

### Item 0 — Day-0 housekeeping (20 min, not a control-plane change)

Archive the ~1 GB of real evidence (`attempts/`, `logs/`, `events.jsonl`) to one tarball, then
`rm -rf` the scratch roots. Separately, one `src-tauri` commit adding a `Drop`/`tempfile` guard
to the `lifecycle_tests.rs` fixtures.
**S — reclaims 247 GB → ~15 GB on a volume with 53 GiB free.** The 67.8 GB fixture leak is a
genuine product bug and the fix survives every downstream decision.

### PHASE 1 — this week. Zero deletions, zero security paths, all revertible.

| # | Change | Files | Effort | Saving |
|---|---|---|---|---|
| **1** | **Share the build root across tasks** — the single largest lever, and the one all four prior docs mis-diagnosed. | `runtime.py::_check_runtime_paths` — point `cargo_home`/`cargo_target`/`clang_cache`/`npm_cache`/`xdg_cache` at `../.murmur-agent-tasks/.resources/build/`. Keep `tmp` per-task. | S | Eliminates the cold ML compile paid once per task id (8–11× per feature). **~100 min/feature**; 96 GB → one ~10 GB target. Safe by construction: registry deps are path-insensitive, only workspace fingerprints churn. Concurrency already covered by the existing flock — verify with two concurrent `cargo test --lib` first. |
| **2** | **Let one task carry more than one commit.** | `cli.py:672` and `cli.py:1759` — drop `{"COMMITTED"}` from the refusal predicate; `cmd_commit` keys idempotence on the diff hash, not status. | S | Collapses 11 task ids → 1 branch. Removes 4 zero-check task dirs per feature and every subsequent cold start. |
| **3** | **One Bash hook; scope the indirection refusal to dangerous verbs.** | `.claude/settings.json` — collapse three `PreToolUse:Bash` entries into one shim. `hook_guard.py::_unsupported_execution_indirection` — fire only when the command *also* matches `git\|security\|codesign\|xcrun\|rm\|cargo`, matched against the full decoded string. | S/M | 139 ms → 46 ms per Bash call, and **every ordinary shell idiom stops being refused**. The most literal answer to "overcomplicated". Residual risk is covered server-side: GitHub branch protection refuses a direct push to `murmur` regardless. |
| **4** | **Stop re-verifying the control plane on product diffs.** | `scripts/ci.sh` — wrap lines 57, 70–78, 89, 92, 95 in a changed-path guard (`.agents/**`, `.claude/**`, `scripts/agent-*`, `scripts/ci.sh`). Move `check-vocabulary.mjs` + the live remote audit to a weekly workflow. | S | ~46 s × **3 executions** per change ⇒ **~26 min/feature**. Also fixes the gate that currently exits 1 on a byte-clean trunk, so a red local `ci.sh` becomes informative again. |
| **5** | **Parallelism parity between harness and CI.** | `config.json:43` `--workers=1` → `2` (`ci.sh:240-243` already uses 2 *and carries the written safety rationale*). `config.json:37` → `cargo nextest run --lib --no-fail-fast` (already installed, used at `ci.sh:156`); drop `RUST_TEST_THREADS=1`/`CARGO_BUILD_JOBS=2`. Path-gate `tauri-boot`. Make `perf-contracts` assert instead of re-running four `cargo test --lib` subsets `rust-lib` already ran. | S | Playwright 6.7 → ~3.4 min (**~70 min/feature**); perf-contracts duplicate execution → ~0. |

**Phase 1 total against the measured 359-min reminders baseline: ~208 min ≈ 58%, with nothing
deleted and no security control changed.**

### PHASE 2 — only after Phase 1 is merged and measured on one real feature

6. **Widen `risk_classification.lock` — MANDATORY PRE-CONDITION for everything below.** Add
   `commands/mod.rs`, `commands/meetings.rs`, `commands/documents.rs`, `commands/links.rs`,
   `commands/reminders.rs`. **[S]** Closes the D7 hole. *Demoting the always-on reviewer before
   this converts a latent taxonomy bug into a live sealed-content leak path.*
7. **WebKit-under-real-CSP Playwright spec** — `e2e/csp/packaged-csp.spec.ts` (~60 LOC): serve
   `dist/meetnotes/browser` with a CSP header carrying a **nonce in `style-src`**, load in the
   **WebKit** project, assert every runtime-injected `<style>` has `el.sheet !== null`. **[M]**
   Closes the only uncovered incident class. Must be RED against a `tauri.conf.json` with
   `dangerousDisableAssetCspModification` removed.
8. **Gate-inventory test + anti-gaming interlock** — parse `generate_handler![...]` in `lib.rs`,
   cross-reference content-namespace commands against a checked-in `gated_commands.txt`, and put
   that file inside the paths filter of the blocking security-review job, so an agent that
   satisfies the lint by editing the allowlist thereby *guarantees* human review fires. **[M]**
   Verify the interlock with a deliberate RED test before trusting it.
9. **Move blocking security reviews to a paths-filtered CI job.** **[M]** Costs **+0 min critical
   path** (parallel to the ~17 min rust lane) and moves enforcement from a receipt 77% of commits
   skip to a required check on a ruleset already confirmed not admin-bypassable. *`egress-security`
   must stay blocking: the corpus holds a MAJOR finding that `rendered_summarize_egress`
   special-cased only `PROVIDER_OLLAMA` while `all_providers` added a cloud provider — a
   CLAUDE.md constraint-#1 violation that clippy, cargo test and ng build all pass cleanly.*
10. **Replace `harness-runtime-smoke.py` — migrate first, delete second.** Move its six real
    assertions into Rust tests, **prove each fails RED against a deliberately broken build**,
    then write `scripts/boot-smoke.sh` (~120 LOC), then `git rm` the 2,085 LOC. Add
    `no_unguarded_msg_send.rs` (~25 LOC) for the 6 `msg_send!` sites. **[M]**
11. **Shadow-run, then delete the engine.** Three real changes (FE-only, Rust, lock-touching);
    confirm 7/8/10 fire RED on broken variants. **Only then** `git rm -r .agents/harness/`. **[L]**
12. **Path-scope the instruction surface.** `paths:` frontmatter on `rust-tauri.md`
    (`src-tauri/**`) and `angular-zoneless.md` (`src/**`); `lock-model.md` unconditional;
    `agentic-workflow.md` 137 → ~30 lines; CLAUDE.md → <200; fix the roster drift (names 7 of 11
    agents). **[S]** 1,019 always-on lines → ~230.

### OPTIONAL — after Phase 2 is stable

- Native `isolation: worktree` frontmatter instead of hand-rolled clones.
- **Nightly `discover`** as the home for the demoted generalist reviewer — proposes only, never
  commits. This is what makes the demotion honest rather than merely cheaper.
- **Minimal durable memory:** one markdown line per in-flight item. *Do not build a 5-state queue.*
- **Size ceiling as policy:** the control plane may not exceed `scripts/ci.sh`, and every new
  guard must cite a specific shipped incident. Nothing else prevents regrowth — the last document
  titled *"simplification"* produced a control plane ~50% larger.

---

## 5. Delete / keep

### DELETE (Phase 2, ≈26,300 LOC)

| Path | LOC | Why |
|---|---:|---|
| `v2_selftest.py` | 6,678 | Tests `cli.py` (4,057). Larger than its subject. Dies with it. |
| `cli.py` | 4,057 | Task lifecycle → `git worktree add` + `git commit` + `gh pr create` |
| `runtime.py` | 3,311 | Durable state → git is already a durable state machine |
| `verifier.py` | 2,684 | Profile derivation → CI `paths:` filter |
| `harness-runtime-smoke.py` | 2,085 | Grew 211→2,085 in a week; hardcodes `ENHANCED_RUNTIME_TASK_ID` at line 37 |
| `hook_guard.py` | 2,201 → ~200 | Keeps every load-bearing rule; drops the ~1,152-LOC `_Selftest` and the unconditional indirection guard |
| `config_audit.py` (+shim) | 1,175 | Audits that the control plane is wired to itself |
| `verify-harness-attestation` | 906 | Unsigned; `ci.yml` says verbatim "a presence-and-consistency receipt, not a signed attestation"; skipped by 77% of the commits it governed |
| `resource_policy.py` + `process_guardian.py` | 979 | Heavy classifier → ~20-line matcher |
| `v2_fault_selftest.py` | 689 | Selftest for deleted code |
| `agent-remote-audit(.py)` (+shim) | 533 | Branch protection is a GitHub ruleset; audit weekly |
| `metrics.py` + `metrics_selftest.py` | 850 | Recorded on every check; **never once rolled up** |
| `checks/npm-lock-evidence.py` | 436 | → `git diff --exit-code package-lock.json` |
| schemas/prompts, receipt trailers, `Harness-Lane`, `agent/v2/*` | — | Vocabulary with nothing left to describe |
| `.claude/skills/{harness,pr-program}/` | — | Folded into one `ship-feature` |
| **Disk** | **247 GB** | ~231 GB duplicated build scratch, ~1 GB real evidence |

**End state: 27,800 → ~1,500 LOC. Ratio to `scripts/ci.sh`: 102:1 → ~5:1.**

### KEEP — each with the incident that earns it

| Keeper | LOC | Incident |
|---|---:|---|
| `scripts/ci.sh` + the two Actions lanes | 273 | The merge authority. ~17 min after #447. |
| `block-bash` dangerous verbs: `security` CLI, `notarytool store-credentials`, `codesign --deep`, `clippy --all-targets`, `rm -rf /`\|`$HOME`, push to `murmur` | ~55 | **2026-06-27**: 11 hung `security` processes; `--deep` left helpers unsigned → notarization **Invalid** on v0.3.0/0.3.1; clippy `--all-targets` thrashes the sqlcipher profile |
| Secret detectors (PEM, `sk-ant-*`, `gh[ps]_*`, 64-hex DEK/KEK), **on `git commit` only** | ~45 | `lock-model.md` "never log the DEK" |
| `agent-resource-run` + `resource_lane.py` (flock over Cargo) | 809 | `ci-red-keychain-lock-and-shared-target-flakes`. **Becomes more load-bearing under Phase 1 item 1.** |
| `lock_read_gate_tests.rs` + `seal_transcript_timeline_round_trips_byte_identical` | 619+ | The existing oracles for classes 1 and 2. **Promote to a named subset; do not rebuild.** |
| `lock-security-reviewer` — **BLOCKING** | — | PR #416: managed-block fences leaked titles + live connector data on share |
| Egress specialist on `summarize/**`, `reason/**`, `connectors/**`, `share/**` — **BLOCKING** | — | PR #370: `claude_code` leaked via ambient MCP (`env_clear ≠ tool sandbox`). **No machine oracle exists for CLAUDE.md constraint #1.** |
| `.claude/rules/lock-model.md` unconditional | 109 | The one rule file whose absence is a security event |
| `config.json risk_classification` globs | — | Good taxonomy — **after item 6 fixes the hole** |
| `e2e/` + `scripts/e2e-*.sh` | — | The runtime proof layer |

---

## 6. Why this lands where four prior plans did not

**The bootstrap trap is not the blocker, and treating it as one is how the last round died.**
`cli.py:502` refuses protected paths, but **77% of control-plane commits already ship without a
receipt**. Two prior proposals wanted to build "a cheap receipted route for control-plane change"
*first* — a step spent on a constraint that empirically does not bind. **Every Phase 1 item ships
the way most control-plane commits already ship: an ordinary branch, an ordinary PR, `ci.sh`
green, one human read of the diff.**

**The real reason nothing landed is timing, and it is not arguable.** Last `.agents/harness`
commit `ac496e6` = 2026-07-29; the latency doc = 2026-07-30. The four prior documents did not lose
an argument — they were never attempted. Meanwhile the 2026-07-27 doc's Option B *did* land in
under 24 hours, as **+16,730 LOC replacing 9,529**. Recommendations in this repo are not ignored;
**they are executed as additions.**

**Therefore the structural fix is the ordering, not the content.** All three design proposals were
10–12 step programs whose deletion was step 10 — the predictable outcome is that steps 1–5 land,
the deletion never does, and the repo ends with **two** control planes, strictly worse than today.
This plan inverts the dependency:

- **Phase 1 is five items and delivers its full value even if Phase 2 never happens.**
- **Phase 2 is gated on Phase 1 being measured on one real feature.** If the numbers do not
  appear, the diagnosis was wrong and the 26k-LOC deletion is *not* justified.
- **The deletion is gated on the deterministic oracles existing and being RED-verified.** Items
  7, 8, 10 are the permission slip; item 11 is the deletion. In that order, never the reverse.
  The one time this repo reversed it — deleting the v1 engine two days after a doc requiring a
  ten-task shadow corpus — it silently lost `eval_runner.py` and all eleven bug-class fixtures.

**Two corrections that would otherwise sink the effort.** (a) If someone implements "pin the
snapshot path" and measures a small gain, they will wrongly conclude simplification does not
work — the target dir was **already** stable across attempts; the cost is per **task id**.
(b) If the generalist reviewer is demoted before `risk_classification.lock` is widened, the
demotion opens a real hole over `commands/mod.rs` and `commands/meetings.rs`.

**Falsification test, stated in advance:** if Phase 1 item 1 is not a merged commit within
**48 hours**, this plan has failed exactly as the previous four did, and the correct response is
to stop writing analyses. The first artifact must be a diff, not a document.

---

## 7. Honest boundary

**What this plan does not fix:**

1. **Elapsed time.** The reminders feature took ~57 h wall-clock for ~6 h of machine time. The
   rest is human availability between sessions. All quoted savings are *machine* time.
2. **Review latency.** 216 review model calls across the corpus and **none records a duration** —
   `review-checkpoint` events carry `review_kind` and `verdict` but no `duration_ms`. Moving
   reviews into a GitHub job at least makes them timed and visible for the first time.
3. **Escape rate.** Demoting the generalist reviewer trades measured latency for an **unmeasured**
   change in defect escape. The containment is that trunk is not shipped — releases are cut
   manually via `release-murmur` with the full gate green.
4. **Regrowth.** Nothing in Phase 1 or 2 structurally prevents the control plane growing back.
5. **The baseline itself.** Every number was collected during eight days of active engine churn.
   A fair steady-state comparison does not exist and will not for two weeks.
6. **`lifecycle_tests.rs` at 19,466 LOC in one file.** Item 0 stops the fixture leak; the file
   remains a latent cost.

**What cannot be proven without a real signed-Mac run** — unchanged by this plan, and must stay
stated in whatever skill replaces the harness: real microphone capture and ScreenCaptureKit/TCC;
Touch ID / user-presence Keychain reads (`MacKekStore::read_biometric` — the `MURMUR_DEV_KEK`
hatch is not the path users hit); lock-at-rest and screen-share auto-relock on a signed build;
notarization/stapling/Gatekeeper; a real end-to-end meeting workflow; and the packaged-WKWebView
render path (item 7 reproduces the CSP *mechanism* under a real nonce header — far more than the
zero coverage that exists today, but it is not the notarized `.dmg`).
