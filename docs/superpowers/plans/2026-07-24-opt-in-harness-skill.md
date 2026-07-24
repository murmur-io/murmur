# Opt-in Harness (`/harness`) + Relaxed Normal Mode — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the Murmur agent harness *opt-in* — full ceremony only when a harness task is active in the worktree (entered via `/harness`); otherwise commits/heavy commands run freely — while keeping `secret-scan` and trunk-push protection always on, and fixing two block-bash false-positives (`source ~/.cargo/env`, `gh` bodies mentioning "cargo").

**Architecture:** One switch — *"does this worktree have an active harness task?"* (`_resolve_task`, already computed) — governs both the `finish-guard` review gate and the resource-lane wrapping requirement. No new persistent state. `/harness` is a thin guided wrapper over the existing `scripts/agent-harness`. The guard logic lives in two canonical files (`hook_guard.py`, `resource_policy.py`) shared by both the Claude and Codex adapters, so there is no vendor-parity work.

**Tech Stack:** Python 3 (harness guards + selftest), Markdown (skills/docs). No new dependencies.

**Design spec:** `docs/superpowers/specs/2026-07-24-opt-in-harness-skill-design.md`

## Global Constraints

- **Commit identity:** author **only** `QueaT <kgm004a@gmail.com>`; **no** AI co-author trailers. `gh` active account = `JakubGawr`.
- **Never push directly to `murmur`/`main`/`master`** — work on a feature branch (`feat/opt-in-harness`) and merge via `gh pr create` → `gh pr merge`.
- **This change edits `protected_paths` files and changes `instructions_sha256`** (it touches `hook_guard.py`, `resource_policy.py`, `.claude/`, `.agents/`, `AGENTS.md`, `scripts/`). Land it by committing from a normal terminal (agent PreToolUse hooks do not fire there) **or** via `scripts/agent-harness` with `--kind harness`. Expect the pre-Task-3 commits to require the terminal path; after Task 3 lands, no-task commits pass `finish-guard` anyway.
- **`MURMUR_FINISH_GUARD` stays `"enforce"`** in `.claude/settings.json` — its *semantics* change (enforce = "enforce if a task is present"), its wiring does not, so `config_audit --ci` stays green.
- **The gate for guard-logic changes is `bash .claude/hooks/selftest.sh`** (runs the canonical `hook_guard.py` selftest against both vendor payloads); `config_audit` does not inspect this logic. Keep hooks fast.
- **No new npm packages or crates.**
- **`com.meetnotes.app` is immutable** (not touched here).

---

## File structure

| File | Responsibility | Change |
| --- | --- | --- |
| `.agents/harness/hook_guard.py` | canonical guard | `NoTaskForWorktree` exception; `_finish_guard` allow-on-no-task; `_block_bash` resource-lane gated on task-present; `source` allowlist in the indirection guard; new selftest cases |
| `.agents/harness/resource_policy.py` | heavy/dev classifier | `gh` exemption (`command_is_heavy` skips substitution scan for gh-only commands) |
| `.claude/skills/harness/SKILL.md` | Claude opt-in entry | create |
| `.agents/skills/harness/SKILL.md` | canonical mirror | create |
| `AGENTS.md` | Codex opt-in entry | add "want rigor → `scripts/agent-harness`" note |
| `.claude/skills/release-murmur/SKILL.md` | release runbook | bake sandbox + P1/P3 human-pause recipe |
| `.agents/skills/release-murmur/SKILL.md` | canonical mirror | same recipe |
| `scripts/release.sh` | ad-hoc packaging smoke test | deprecation header (stale `MeetNotes.app` path) |

---

## Task 1: Allowlist `source ~/.cargo/env` in the indirection guard

`_unsupported_execution_indirection` (`hook_guard.py:282`) hard-blocks `source`/`.` with no exceptions, even though `resource_policy.SAFE_SOURCE_TARGETS` already lists the safe cargo env file. CLAUDE.md lists `source ~/.cargo/env` as a standard command; unblock exactly that allowlist, keep everything else blocked.

**Files:**
- Modify: `.agents/harness/hook_guard.py` (function `_unsupported_execution_indirection`, ~line 333; selftest `cases` list, ~line 1648)

**Interfaces:**
- Consumes: `resource_policy.SAFE_SOURCE_TARGETS` (= `{"$HOME/.cargo/env", "${HOME}/.cargo/env", "~/.cargo/env"}`), already importable (`resource_policy` is imported in `hook_guard.py`).
- Produces: nothing new for later tasks.

- [ ] **Step 1: Add the failing selftest cases**

In `hook_guard.py`, in the `cases` list inside `_run_selftest` (currently ending at the `("quoted security search", ...)` entry ~line 1666), add three entries:

```python
                ("PR creation", "gh pr create --base murmur --title x", "ALLOW"),
                ("quoted push search", "rg 'git push origin murmur' .", "ALLOW"),
                ("quoted security search", "rg 'security find-identity' .", "ALLOW"),
                ("source cargo env", "source ~/.cargo/env", "ALLOW"),
                ("dot source cargo env", ". $HOME/.cargo/env", "ALLOW"),
                ("source arbitrary script", "source ./guard-bypass.sh", "BLOCK"),
```

(The first three lines already exist — shown for placement. The `source ./guard-bypass.sh` BLOCK also lives in the `indirections` list; duplicating it in `cases` guards against the allowlist being too broad.)

- [ ] **Step 2: Run the selftest to verify the new ALLOW cases fail**

Run: `bash .claude/hooks/selftest.sh`
Expected: FAIL lines for `source cargo env` and `dot source cargo env` — `got BLOCK, want ALLOW` (current code blocks all `source`).

- [ ] **Step 3: Implement the allowlist**

In `_unsupported_execution_indirection`, replace the combined `source`/`.` block (currently):

```python
        if executable in {"eval", "source", ".", "exec", "xargs"}:
            return f"execution indirection via {executable!r} is unsupported by the command guard"
```

with:

```python
        if executable in {"source", "."}:
            # `source`/`.` of a known-safe env file (e.g. ~/.cargo/env) is a
            # standard setup step, not executable indirection. All other targets
            # stay blocked.
            target = effective[1] if len(effective) > 1 else ""
            if target not in resource_policy.SAFE_SOURCE_TARGETS:
                return f"execution indirection via {executable!r} is unsupported by the command guard"
        elif executable in {"eval", "exec", "xargs"}:
            return f"execution indirection via {executable!r} is unsupported by the command guard"
```

- [ ] **Step 4: Run the selftest to verify all cases pass**

Run: `bash .claude/hooks/selftest.sh`
Expected: PASS — including `source cargo env` (ALLOW), `dot source cargo env` (ALLOW), `source arbitrary script` (BLOCK), and the existing `indirections` `source`/`dot source` guard-bypass cases (still BLOCK).

- [ ] **Step 5: Commit**

```bash
git add .agents/harness/hook_guard.py
git commit -m "fix(harness): allow source ~/.cargo/env through the command guard"
```

---

## Task 2: Stop classifying `gh` invocations as resource-heavy

A `gh pr create --body "…cargo…"` (free-form PR text that mentions cargo, or contains markdown backticks / `$()`, or an unbalanced quote that breaks `shlex`) is flagged heavy by `command_is_heavy` — via its backtick/`$()` substitution scan (`resource_policy.py:585-594`) and/or the `__MURMUR_UNPARSEABLE__` regex (`:474-486`). `gh` is a network/VCS CLI, never a build tool. Exempt commands whose every segment leads with a safe non-build tool.

**Files:**
- Modify: `.agents/harness/resource_policy.py` (constants ~line 16; new helper; `command_is_heavy` ~line 580)
- Modify: `.agents/harness/hook_guard.py` (resource selftest `resource_cases` list, ~line 1694)

**Interfaces:**
- Consumes: existing `READ_ONLY_SEARCHES`, `skip_assignments`, `basename`, `tokenize`, `command_segments` in `resource_policy.py`.
- Produces: `resource_policy.SAFE_NONBUILD_TOOLS` (set) and `resource_policy._segment_leading_tool(tokens) -> Optional[str]` — used only within `resource_policy.py`.

- [ ] **Step 1: Add the failing selftest cases**

In `hook_guard.py`, in the `resource_cases` list (~line 1694), add three entries after the existing `("read-only cargo search", …)` line:

```python
            ("read-only cargo search", "rg 'cargo test --lib' .", "ALLOW"),
            ("gh PR body mentions cargo", "gh pr create --base murmur --title x --body 'Fixes the cargo build path'", "ALLOW"),
            ("gh PR body with backticks", "gh pr create --title x --body 'run `cargo test --lib` first'", "ALLOW"),
            ("gh then cargo still heavy", "gh pr view 1 && cargo build", "BLOCK"),
```

(First line exists — shown for placement.)

- [ ] **Step 2: Run the selftest to verify the two gh ALLOW cases fail**

Run: `bash .claude/hooks/selftest.sh`
Expected: FAIL for `gh PR body with backticks` — `got BLOCK, want ALLOW` (the backtick scan finds `cargo test --lib`). `gh then cargo still heavy` must already be BLOCK (cargo segment).

- [ ] **Step 3: Add the constant and helper**

In `resource_policy.py`, after `READ_ONLY_SEARCHES = {"grep", "rg"}` (line 16) add:

```python
ALWAYS_ALLOWED_TOOLS = {"gh"}
SAFE_NONBUILD_TOOLS = READ_ONLY_SEARCHES | ALWAYS_ALLOWED_TOOLS
```

Then add this helper above `command_is_heavy` (near the other segment helpers):

```python
def _segment_leading_tool(tokens):
    """Best-effort basename of the executable a segment launches, or None."""
    if not tokens:
        return None
    if tokens[0] == "__MURMUR_UNPARSEABLE__":
        raw = tokens[1].lstrip()
        match = re.match(r"(?:[A-Za-z_][A-Za-z0-9_]*=\S*\s+)*([^\s;|&]+)", raw)
        return basename(match.group(1)) if match else None
    index = skip_assignments(tokens, 0)
    if index >= len(tokens):
        return None
    return basename(tokens[index])
```

- [ ] **Step 4: Exempt safe-tool-only commands in `command_is_heavy`**

In `command_is_heavy` (line 580), immediately after the `TEST_GUARDIAN_ENV` check and BEFORE the backtick loop, insert:

```python
def command_is_heavy(command, depth=0):
    if depth > MAX_DEPTH:
        return True
    if TEST_GUARDIAN_ENV in command:
        return True
    # A command whose every segment leads with a safe non-build tool (gh, grep,
    # rg) cannot launch heavy work. Skip the substitution scan whose only job is
    # to catch build commands hidden in backticks/$() — those are exactly what a
    # gh PR/issue body legitimately contains as free-form text.
    segments = list(command_segments(tokenize(command)))
    if segments and all(
        _segment_leading_tool(segment) in SAFE_NONBUILD_TOOLS for segment in segments
    ):
        return False
    for backtick_body in re.findall(r"`([^`]*)`", command, flags=re.DOTALL):
        ...
```

(Leave the rest of the function unchanged; the final line still returns `any(segment_is_heavy(...))`.)

- [ ] **Step 5: Run the selftest to verify all cases pass**

Run: `bash .claude/hooks/selftest.sh`
Expected: PASS — `gh PR body mentions cargo` (ALLOW), `gh PR body with backticks` (ALLOW), `gh then cargo still heavy` (BLOCK, mixed segments), `read-only cargo search` (ALLOW, still), and all existing cargo/ng/npm cases still BLOCK (no task-gating yet — that is Task 4).

- [ ] **Step 6: Commit**

```bash
git add .agents/harness/resource_policy.py .agents/harness/hook_guard.py
git commit -m "fix(harness): exempt gh invocations from resource-heavy classification"
```

---

## Task 3: `finish-guard` allows commits when no harness task is active

Invert the fail-closed-on-no-task behavior. When `_resolve_task` finds no task for the worktree, `finish-guard` allows the commit (normal mode). A task that is *present but invalid* (malformed, wrong worktree, failed attestation) still blocks. `secret-scan` and trunk protection are separate hooks and are unaffected.

**Files:**
- Modify: `.agents/harness/hook_guard.py` (new exception; `_resolve_task` ~line 625; `_finish_guard` ~line 937; selftest assertion ~line 1807)

**Interfaces:**
- Consumes: existing `GuardFailure`, `_resolve_task`, `_validate_attestation`.
- Produces: `NoTaskForWorktree(GuardFailure)` exception — reused by Task 4.

- [ ] **Step 1: Flip the failing selftest assertion**

In `hook_guard.py` (~line 1807), change the "missing manifest" expectation from `BLOCK` to `ALLOW`:

```python
        test.result(f"{vendor}: default-enforce missing manifest", got, "ALLOW")
```

Leave the malformed-manifest (`BLOCK`, ~1821), fake-receipt (`BLOCK`, ~1825), and minimal-receipt (`BLOCK`, ~1839) assertions unchanged.

- [ ] **Step 2: Run the selftest to verify it now fails**

Run: `bash .claude/hooks/selftest.sh`
Expected: FAIL for `default-enforce missing manifest` — `got BLOCK, want ALLOW` (current code fails closed on no task).

- [ ] **Step 3: Define the distinct exception and raise it only for the true no-task case**

In `hook_guard.py`, near the `GuardFailure` definition, add:

```python
class NoTaskForWorktree(GuardFailure):
    """Raised when no harness task claims the current worktree (normal mode)."""
```

In `_resolve_task`, change ONLY the final raise (line 625) from `GuardFailure` to `NoTaskForWorktree`:

```python
    raise NoTaskForWorktree(
        "no task manifest matches this worktree; use scripts/agent-harness init/run"
    )
```

Leave the "multiple task manifests" (line 604) and every other raise as plain `GuardFailure` — those mean tasks exist and must still fail closed.

- [ ] **Step 4: Allow-on-no-task in `_finish_guard`**

In `_finish_guard` (the `try` block ~line 937-950), add a dedicated `except` before the generic one:

```python
        try:
            repo = _repo_for_invocation(commits[0])
            _, common, _, _ = _repo_context(repo)
            task, task_dir = _resolve_task(repo, common)
            _validate_attestation(
                repo,
                common,
                task,
                task_dir,
                allow_test_adapter=allow_test_adapter,
            )
            return None
        except NoTaskForWorktree:
            return None  # normal mode: no active harness task → allow the commit
        except GuardFailure as exc:
            reason = str(exc)
```

- [ ] **Step 5: Run the selftest to verify all cases pass**

Run: `bash .claude/hooks/selftest.sh`
Expected: PASS — `default-enforce missing manifest` (ALLOW), `malformed task manifest` (BLOCK), `production rejects fake receipt` (BLOCK), `minimal receipt` (BLOCK). The `secret-scan` cases (including the clean `git commit` ALLOW at ~1795) still pass.

- [ ] **Step 6: Commit**

```bash
git add .agents/harness/hook_guard.py
git commit -m "feat(harness): finish-guard allows commits when no task is active (opt-in)"
```

---

## Task 4: Resource-lane wrapping required only when a harness task is active

`_block_bash` currently forces every heavy/dev command through `agent-resource-run`/`agent-dev-run` unconditionally. Gate that on the same switch: only enforce it when a task claims the worktree (parallel harness writers must serialize on the shared cargo lane). In normal mode, heavy commands run directly.

**Files:**
- Modify: `.agents/harness/hook_guard.py` (new helper; `_block_bash` ~line 494-504; resource selftest `resource_cases` ~line 1694, and add a task-present block case)

**Interfaces:**
- Consumes: `NoTaskForWorktree` (Task 3), `_resolve_task`, `_repo_context`, `_git_text`.
- Produces: `_worktree_has_active_task(process_cwd) -> bool` — internal to `hook_guard.py`.

- [ ] **Step 1: Update the resource selftest expectations (no-task → ALLOW)**

The `resource_cases` list runs against a repo with **no** task, so under the new rule they must ALLOW. Change each unwrapped-heavy expectation from `BLOCK` to `ALLOW`:

```python
        resource_cases = [
            ("direct cargo metadata", "cargo metadata --no-deps", "ALLOW"),
            ("direct Rust test", "cd src-tauri && cargo test --lib", "ALLOW"),
            ("direct Angular build", "npx ng build", "ALLOW"),
            ("direct npm dev", "npm run dev", "ALLOW"),
            ("direct full CI", "bash scripts/ci.sh", "ALLOW"),
            ("read-only cargo search", "rg 'cargo test --lib' .", "ALLOW"),
            ("gh PR body mentions cargo", "gh pr create --base murmur --title x --body 'Fixes the cargo build path'", "ALLOW"),
            ("gh PR body with backticks", "gh pr create --title x --body 'run `cargo test --lib` first'", "ALLOW"),
            ("gh then cargo still heavy", "gh pr view 1 && cargo build", "ALLOW"),
```

Note `gh then cargo still heavy` also becomes ALLOW here (no task). Keep the `lane-wrapped Rust test` entry (~line 1701) as `ALLOW` (unchanged). Leave any assertions that check the wrapper-forging / dev-runner allowlist as-is — those test classification, not the block gate; verify them individually if any turn red and adjust only genuine no-task cases.

- [ ] **Step 2: Add a task-present BLOCK case for the resource lane**

The finish-guard selftest already builds a task-bearing repo via `_finish_repo()` + `_write_receipt(repo, "fresh")` (~line 1823). Add, right after the `minimal receipt` block (~line 1839), a resource-lane assertion proving wrapping is still required in harness mode:

```python
        # With a task claiming the worktree, the resource lane is enforced again.
        task_dir, task, attestation = _write_receipt(repo, "lane")
        got, _ = test.invoke(vendor, "block-bash", "cargo test --lib", repo)
        test.result(f"{vendor}: task-present unwrapped heavy blocked", got, "BLOCK")
        got, _ = test.invoke(
            vendor,
            "block-bash",
            "scripts/agent-resource-run --chdir src-tauri -- cargo test --lib",
            repo,
        )
        test.result(f"{vendor}: task-present lane-wrapped allowed", got, "ALLOW")
```

- [ ] **Step 3: Run the selftest to verify the new expectations fail**

Run: `bash .claude/hooks/selftest.sh`
Expected: FAIL for the `direct …` no-task cases (`got BLOCK, want ALLOW`) — current code blocks regardless of task. The `task-present unwrapped heavy blocked` case may pass or fail depending on ordering; the decisive failures are the no-task ones.

- [ ] **Step 4: Add the task-detection helper**

In `hook_guard.py`, add near `_resolve_task`:

```python
def _worktree_has_active_task(process_cwd: Path) -> bool:
    """True when a harness task claims this worktree (harness mode)."""
    try:
        top = _git_text(process_cwd, "rev-parse", "--show-toplevel", check=False)
        if not top:
            return False
        repo = Path(top).resolve()
        _, common, _, _ = _repo_context(repo)
        _resolve_task(repo, common)
        return True
    except NoTaskForWorktree:
        return False
    except GuardFailure:
        # Tasks exist but are ambiguous/malformed → fail safe: treat as harness
        # mode so the lane stays enforced.
        return True
    except Exception:
        # Not a resolvable git worktree → normal mode.
        return False
```

- [ ] **Step 5: Gate the resource-lane block on task presence**

In `_block_bash`, wrap the two resource-lane checks (lines 494-504) so they only fire in harness mode. Replace:

```python
    if resource_policy.command_is_dev_in(command, process_cwd):
        return (
            "long-lived dev commands must run through scripts/agent-dev-run; "
            "the dev supervisor stays outside the global lane while its cargo/rustc "
            "descendants acquire it per process"
        )
    if resource_policy.command_is_heavy_in(command, process_cwd):
        return (
            "unwrapped resource-heavy build/test/dev command; run it through "
            "scripts/agent-resource-run so Murmur worktrees share one supervised lane"
        )
    return None
```

with:

```python
    dev = resource_policy.command_is_dev_in(command, process_cwd)
    heavy = resource_policy.command_is_heavy_in(command, process_cwd)
    if (dev or heavy) and _worktree_has_active_task(process_cwd):
        if dev:
            return (
                "long-lived dev commands must run through scripts/agent-dev-run; "
                "the dev supervisor stays outside the global lane while its cargo/rustc "
                "descendants acquire it per process"
            )
        return (
            "unwrapped resource-heavy build/test/dev command; run it through "
            "scripts/agent-resource-run so Murmur worktrees share one supervised lane"
        )
    return None
```

(Note: `_worktree_has_active_task` is only called when the command is already classified heavy/dev, so the common fast path pays no git cost.)

- [ ] **Step 6: Run the selftest to verify all cases pass**

Run: `bash .claude/hooks/selftest.sh`
Expected: PASS — all no-task `direct …` cases ALLOW; `task-present unwrapped heavy blocked` BLOCK; `task-present lane-wrapped allowed` ALLOW; trunk-push/security/codesign block-bash cases still BLOCK.

- [ ] **Step 7: Commit**

```bash
git add .agents/harness/hook_guard.py
git commit -m "feat(harness): require the resource lane only when a task is active"
```

---

## Task 5: The `/harness` skill (both vendors) + Codex entry

Create the opt-in entry point. Claude gets a skill; the canonical runbook is mirrored under `.agents/skills/`; Codex (no skills mechanism) is pointed at the CLI from `AGENTS.md`. Skills are not part of the `config_audit` parity contract, so no Codex `.toml` mirror is required.

**Files:**
- Create: `.agents/skills/harness/SKILL.md`
- Create: `.claude/skills/harness/SKILL.md`
- Modify: `AGENTS.md` (add a Codex-facing opt-in note)

**Interfaces:** none (documentation/runbook).

- [ ] **Step 1: Write the canonical runbook**

Create `.agents/skills/harness/SKILL.md`:

```markdown
---
name: harness
description: Run a change through the full Murmur harness — isolated worktree, writer, deterministic checks, independent adversarial + risk reviews, hash-bound PASS attestation, guarded commit. Use PROACTIVELY when a change is risky, multi-step, or you want the earned safety net (lock/crypto/egress/protocol, or anything you want independently verified). Skip it for docs, chores, and low-risk edits — those commit normally.
---

# `/harness` — opt-in rigor

Murmur's harness is **opt-in**. Normal commits run freely (only secret-scan and
trunk-push protection are always on). Invoke the harness deliberately, via this
skill, when a change deserves independent verification.

## When to reach for it

- Anything touching the lock model / crypto / secrets / storage / MCP / egress /
  the sharing protocol (the `risk_classification` paths in
  `.agents/harness/config.json`).
- A multi-step feature or refactor where you want a fresh adversarial reviewer to
  try to break the change before it lands.
- Any time you want the hash-bound Definition-of-Done receipt on the commit.

For a docs fix, a chore, a version bump, or a small low-risk edit: **do not use
this** — just commit normally.

## How it works

The switch is physical: the harness runs in an **isolated sibling worktree** that
carries a task manifest. While that task is active, `finish-guard` enforces the
full attestation and the resource lane is required for heavy commands. Your main
`murmur` checkout never has a task, so work there is unconstrained.

## Run it

```bash
# 1. Create the contract + isolated worktree
scripts/agent-harness init --kind <feature|refactor|docs|harness> --title "<what>"

# 2. Drive writer → checks → independent reviews → PASS attestation
scripts/agent-harness run

# 3. Commit from the attested index (QueaT identity, no AI trailers) and open a PR
scripts/agent-harness commit
gh pr create -R murmur-io/murmur --base murmur
gh pr merge --merge

# 4. Close the task
scripts/agent-harness close
```

The default vendor pair is Claude writer → Codex reviewer (the only supported
reversal is Codex writer → Claude reviewer). The implementer never owns the
verdict.

## Verify the harness itself

```bash
bash .claude/hooks/selftest.sh
scripts/agent-harness selftest --ci
scripts/agent-config-audit --ci
```
```

- [ ] **Step 2: Mirror it for Claude**

Create `.claude/skills/harness/SKILL.md` with **identical** content to Step 1 (the repo mirrors `.agents/skills/` runbooks into `.claude/skills/`).

```bash
cp .agents/skills/harness/SKILL.md .claude/skills/harness/SKILL.md
```

- [ ] **Step 3: Add the Codex-facing note to `AGENTS.md`**

In `AGENTS.md`, under the agents/skills/harness section (mirroring where `CLAUDE.md` documents the harness), add:

```markdown
## Opt-in harness (`/harness`)

The harness is **opt-in**. Normal commits run freely; only `secret-scan` and
direct-push-to-`murmur` protection are always on. Reach for rigor deliberately:

- **Codex has no skills mechanism** — invoke the harness directly:
  `scripts/agent-harness init … && scripts/agent-harness run && scripts/agent-harness commit`.
- Use it for lock/crypto/egress/protocol changes or anything you want a fresh
  adversarial reviewer to verify. Skip it for docs/chores/low-risk edits.
- Guard behavior is identical across vendors (same `hook_guard.py`): a commit in
  a worktree with **no** active task is allowed; a worktree **with** a task
  enforces the full hash-bound attestation.
```

- [ ] **Step 4: Verify config-audit stays green**

Run: `scripts/agent-config-audit --ci`
Expected: exits 0 (skills are not parity-checked; the `AGENTS.md` edit passes the semantic lint — no "angular 18" / `allowSignalWrites` / `provideExperimentalZonelessChangeDetection` strings introduced).

- [ ] **Step 5: Commit**

```bash
git add .agents/skills/harness/SKILL.md .claude/skills/harness/SKILL.md AGENTS.md
git commit -m "docs(harness): add opt-in /harness skill (Claude) + Codex AGENTS entry"
```

---

## Task 6: Bake the release recipe into `release-murmur`; deprecate `scripts/release.sh`

Make a smooth release independent of memory recall: document the unsandboxed steps, the human pause points, and the correct order in the `release-murmur` skill. Mark the stale `scripts/release.sh` (targets `MeetNotes.app`) as a smoke test only.

**Files:**
- Modify: `.claude/skills/release-murmur/SKILL.md`
- Modify: `.agents/skills/release-murmur/SKILL.md`
- Modify: `scripts/release.sh` (deprecation header)

**Interfaces:** none (documentation).

- [ ] **Step 1: Add the sandbox + human-pause recipe to the release skill**

Append a section to `.claude/skills/release-murmur/SKILL.md` (near the "Mac-only boundary"):

```markdown
## Sandbox & human-pause recipe (do not rely on memory recall)

Release steps that touch the keychain, codesign, notarytool, or `gh`/`git push`
**must run unsandboxed** and **must not** be wrapped in `agent-resource-run`
(the harness is opt-in; on the release machine you are in normal mode).

- **Sandbox:** the release machine keeps a per-machine, git-ignored
  `.claude/settings.local.json` override. Never commit sandbox-disabling to the
  repo — it would weaken the sandbox for the whole team.
- **Heavy commands run directly** in normal mode (no `agent-resource-run`
  wrapping required). `finish-guard` is asleep, so the version-bump commit goes
  through directly; merge to `murmur` still requires a **PR** (never a direct
  push), and `gh` PR bodies are no longer misclassified as heavy.

Irreducible human pause points (macOS auth dialogs the agent shell cannot answer):

1. **P1 — unlock the login keychain** (once, up front): needed for `git`/`gh`
   push and for Developer-ID key + notary-profile access. Run
   `security unlock-keychain` **yourself** — the agent must never run `security`.
2. **P2 — supply the Developer-ID hash** as `DEVELOPER_ID` (40 hex chars) to
   `scripts/macos-sign-notarize.sh`. Pre-supply it so it does not stall the run.
3. **P3 — approve the Developer-ID codesign key dialog** once ("Always Allow");
   this collapses the per-helper + app + DMG prompts into one interaction.

One-time-only (not per release): `xcrun notarytool store-credentials murmur`,
run interactively by you.

Everything else — CI gate, version bump, universal build, DMG, `notarytool
submit --wait`, staple, `spctl`, `gh release create/upload` — is headless.

## Enable repo auto-merge

`gh pr merge --merge --auto` requires auto-merge enabled on the repo (Settings →
General → "Allow auto-merge"). Enable it once to stop hand-waiting on CI.
```

- [ ] **Step 2: Mirror to the canonical skill**

Apply the **same** appended section to `.agents/skills/release-murmur/SKILL.md`.

- [ ] **Step 3: Deprecate `scripts/release.sh`**

Replace the top comment block of `scripts/release.sh` (lines 2-6) with:

```bash
# DEPRECATED — smoke test only, NOT the release path.
# This targets the stale bundle name (MeetNotes.app); the real universal release
# is Murmur.app at the workspace-root target/, driven by the `release-murmur`
# skill + scripts/macos-sign-notarize.sh. Use this file only to prove the bundle
# builds, ad-hoc-signs, and packages into a functional .dmg on a headless box.
```

- [ ] **Step 4: Verify config-audit stays green**

Run: `scripts/agent-config-audit --ci`
Expected: exits 0 (the release skill is not parity-fingerprinted beyond `risk_classification`; `bash -n scripts/*.sh` passes for `release.sh`).

- [ ] **Step 5: Commit**

```bash
git add .claude/skills/release-murmur/SKILL.md .agents/skills/release-murmur/SKILL.md scripts/release.sh
git commit -m "docs(release): bake sandbox + human-pause recipe; deprecate stale release.sh"
```

---

## Task 7: Full-gate verification + rollout

Prove the whole change is coherent, then merge and clean up.

**Files:** none (verification + ops).

- [ ] **Step 1: Run the canonical selftest against both vendors**

Run: `bash .claude/hooks/selftest.sh`
Expected: `0 failures` across all sections (command guard, indirection, resource lane, secret scan, finish gate).

- [ ] **Step 2: Run the harness selftest and config audit**

Run:
```bash
scripts/agent-harness selftest --ci
scripts/agent-config-audit --ci
```
Expected: both exit 0. `config_audit` confirms `MURMUR_FINISH_GUARD=enforce` still wired, hook parity intact, no rule/agent drift.

- [ ] **Step 3: Smoke-test the switch end to end (manual)**

From the main `murmur` worktree (no task), from a normal terminal:
```bash
git commit --allow-empty -m "chore: switch smoke test"   # expected: succeeds (no task → finish-guard allows)
git push origin murmur                                     # expected: BLOCKED by the agent hook if run via the agent; from your terminal it is your call — do NOT push the smoke commit; drop it:
git reset --hard HEAD~1
```
Then confirm harness mode still enforces: `scripts/agent-harness init …` in a sibling worktree and verify a bare `git commit` there is rejected without a receipt.

- [ ] **Step 4: Open the PR and merge**

```bash
git push -u origin feat/opt-in-harness
gh pr create -R murmur-io/murmur --base murmur --title "Opt-in harness (/harness) + relaxed normal mode"
# after CI green:
gh pr merge --merge
```

- [ ] **Step 5: Prune abandoned task worktrees**

The `instructions_sha256` change invalidates in-flight receipts; remove the stale experiments:
```bash
git worktree list
# for each ../.murmur-agent-tasks/* worktree (incl. opt-in-harness-quick-lane-v2..v4,
# harness-light-lane-v2..v7, release-1-0-2-*):
git worktree remove --force <path>
git branch -D <agent/branch>
```

- [ ] **Step 6: Final commit / confirmation**

No code to commit here; confirm `git worktree list` shows only the main checkout (+ any intentional worktree) and that the feature is merged to `murmur`.

---

## Self-review notes

- **Spec §3.1.1** (finish-guard allow-on-no-task) → Task 3. **§3.1.2** (resource-lane task-gated) → Task 4. **§3.1.3** (secret-scan/branch unconditional) → preserved (not modified; verified in Task 3 Step 5 / Task 7 Step 1).
- **Spec §3.2.4** (`source ~/.cargo/env`) → Task 1. **§3.2.5** (`gh` exempt) → Task 2 (residual: shell-substitution in a *double-quoted* gh body is still expanded by the shell → use `--body-file`; documented in Task 6 recipe).
- **Spec §3.3** (`/harness` both vendors) → Task 5. **§3.4** (sandbox + release recipe, release.sh) → Task 6.
- **Spec §6** (testing) → each task's selftest steps + Task 7. **§8** (rollout/cleanup) → Task 7 Steps 4-5. **§7** (accepted risk) → inherent to Tasks 3-4 (no risk-gating added, by design).
- **Type consistency:** `NoTaskForWorktree` defined in Task 3, consumed in Task 4's `_worktree_has_active_task`. `SAFE_NONBUILD_TOOLS` / `_segment_leading_tool` defined and used within Task 2.
