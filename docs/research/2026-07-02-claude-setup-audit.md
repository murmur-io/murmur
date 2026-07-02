<!-- Generated 2026-07-02 via /research (4-agent fan-out: meetnotes .claude audit, urc-monorepo .claude audit, official Claude Code docs, community best-practices web sweep). Tool versions / URLs = point-in-time. -->
# Research: `.claude` setup audit — Murmur vs urc-monorepo vs 2026 best practices

## TL;DR / Verdict

Murmur's `.claude` is **top-tier at the prose layer** (war-story-encoded binding rules, independent adversarial verification, a domain security reviewer that is ahead of the published state of the art) and **absent at the deterministic layer**: zero hooks, no checked-in `settings.json`, no deny rules, no secret scan, no learnings loop, no machine-readable evidence. Worst finding: the `block-bash` trunk-push hook that CLAUDE.md, `agentic-workflow.md`, `release-engineer.md` and the `release-murmur` skill all cite **does not exist and never existed in git history** — a docs-drift bug in the very layer whose motto is "trust code, not docs". Community + official consensus is unambiguous: prose rules are advisory and degrade under context pressure; hooks are the one layer the agent cannot talk itself out of.

**Plan: (0) make the phantom hook real + 4 sibling guards for already-paid-for incidents, (1) context diet + citation policy, (2) learnings loop, (3) evidence gates (finish-guard), (4) selective extras.** Details below.

## What we already have (verified in-tree)

| Artifact | State | Quality |
| --- | --- | --- |
| `CLAUDE.md` + 4 `@`-imported rules | ~49 KB / ~12k tokens always-on | ⭐⭐⭐⭐⭐ content, ⭐⭐ weight & drift |
| Rules: `rust-tauri`, `angular-zoneless`, `lock-model`, `agentic-workflow` | binding, auto-loaded | exceptional — every rule encodes a paid-for bug |
| Skills: `release-murmur`, `tauri-dev`, `ship-feature`, `research` | runbook-style | high; no trigger-condition discipline in descriptions |
| Agents (6): implementers × 2, adversarial-verifier, lock-security-reviewer, release-engineer, murmur-researcher | `model: inherit`, tools scoped to role | high; verifier has ToolSearch→Playwright ✓ |
| `settings.local.json` | 2 permission allows only | minimal |
| `settings.json` (checked-in) / `hooks/` / `commands/` / `learnings/` / `traces/` / `.mcp.json` / `.github/` | **do not exist** | the gap |

Strengths to preserve: implementer-never-certifies discipline; honest "needs a signed build / real Mac" boundaries; tool access matched to role; lock-security-reviewer as a mandatory domain gate (no better published pattern found).

## Findings

### F1 — The phantom `block-bash` hook (CRITICAL, confirmed twice independently)

Cited at `CLAUDE.md` (release rule 6), `.claude/rules/agentic-workflow.md:43`, `.claude/agents/release-engineer.md:17,94`, `.claude/skills/release-murmur/SKILL.md:32`. No `.claude/hooks/`, no hook config in any settings file (project, local, or `~/.claude`), and **no such file was ever committed** (string introduced as prose in commit `a9f51b4`). Every "hard-won release rule" (no direct push, no `security` CLI, no `codesign --deep`, no `clippy --all-targets`) is currently guarded by prose only.

### F2 — file:line citation drift (MEDIUM)

`commands.rs` is now 8,307 lines; cited anchors are thousands of lines off: `meeting_is_unlocked` cited at `commands.rs:2249` → actually **6026**; `visibility_clause` cited `db.rs:1269` → **4535**; `lock_folder` cited `commands.rs:1731` → **4681**. Symbols all still exist (grep-by-name works), but "see `db.rs:1269`" sends a reader to the wrong place. Also: `release-murmur` says it supersedes `docs/RELEASE-CHECKLIST.md`, yet the checklist sits un-deprecated in-tree.

### F3 — What urc-monorepo has that we don't (transferable mechanisms)

Full audit of `../urc-monorepo/.claude` (10 hooks, 11 agents, 7 commands, 6 JS workflows, per-agent learnings + memory, per-task evidence traces):

1. **Gates-as-JSON + finish-guard** — every review lane emits a schema'd verdict (`code-review.json`, `security-review.json`, `repro.json`, `verification.json`, `e2e.json`); a `PreToolUse(Bash git commit)` hook (`ship-task-finish-guard.sh`) reads the task's lane and **mechanically blocks commit** until the required gate files exist with `verdict: PASS`. DoD is machine-checked, not prose-parsed.
2. **Two-tier learnings loop** — `.claude/learnings/<agent>.md` with `## Recurring patterns` (curated, ≤20 bullets, injected into every dispatch prompt as "binding — do NOT repeat") + `## Run journal` (append-only evidence-backed entries, auto-pruned at 50). Writers: a `learnings-extractor` agent post-gates + `/learn` (operator) ; promotion via `/curate-learnings`.
3. **Evidence traces** — `trace-span.sh` (one JSON span per phase/gate → `trace.jsonl`), post-commit archive to `.claude/traces/<TASK>/` with `MANIFEST.txt` one-liner index; enables post-hoc questions like "average design-qa iterations".
4. **Discipline hooks beyond permissions** — `wip-guard.sh` (WIP=1: one active feature), `ship-task-guard.sh` (orchestrator may not Edit/Write app code — implementation must go through Agent dispatch; `.subagent-active` marker with 30-min failsafe unblocks legit subagent edits), `init-guard.sh` (phase confinement), `e1-base-branch-guard.sh` (temporal branch policy read from a flat policy file).
5. **Advisory→enforce rollout** — new gates ship with `MODE=advisory` (warn, log) via env var in `settings.json`, flipped to enforce after observation.
6. **PostToolUse auto-format** — eslint --fix + prettier on every Edit/Write, never blocking.
7. **Untrusted-data quarantine** — external ticket text wrapped in `<linear_untrusted>` XML + marker-abort on injection phrases; a headless-mode system-prompt anchor (`automation/untrusted-anchor.txt`) re-asserts it for autonomous runs.
8. **Cross-component detection** — `lib/cross-component.sh` checks whether changed files span API+FE and escalates the required gate (joined E2E) even when the task was classified single-lane.

### F4 — Official docs (2026) alignment

- Anthropic's pruning test for CLAUDE.md: *"Would removing this cause mistakes? If not, cut it"* and *"If Claude already does something correctly… delete it **or convert it to a hook**"*. Hooks are explicitly framed as the deterministic layer vs advisory CLAUDE.md. (https://code.claude.com/docs/en/best-practices)
- Hook decision contract: exit 2 = block w/ stderr shown to the model; or JSON `permissionDecision: allow|deny|ask` (https://code.claude.com/docs/en/hooks.md).
- Skills support `disable-model-invocation` for side-effect runbooks (manual-only trigger) and `references/` + `scripts/` progressive disclosure.
- Path-scoped rules (`paths:` frontmatter) can cut always-on context: e.g. `angular-zoneless.md` could load only when FE files are touched.
- Checked-in project `.claude/settings.json` is the sanctioned, diff-reviewed home for hooks/permissions; `settings.local.json` for personal deltas.

### F5 — Community sweep (highest-signal, cited)

- **Hooks-as-guardrails thesis** (independent practitioners, converging): "Prompts are interpreted at runtime by an LLM that can be convinced otherwise; hooks execute regardless." Exit-code contract, warn→block tuning, ~200 ms latency budget, and **"start with three hooks, not 25"** (https://paddo.dev/blog/claude-code-hooks-guardrails/, https://blakecrosley.com/blog/claude-code-hooks — 95 hooks in production, git-safety hook intercepted 8 dangerous attempts in 9 months).
- **Secret-scan on staged diff** — dwarvesf/claude-guardrails intercepts `git commit`, regexes the staged diff for keys/PEM/hex material; plus deny rules for credential paths, honest about bash-bypass limits (https://github.com/dwarvesf/claude-guardrails).
- **Superpowers 4** (Jesse Vincent): split review into **spec-review → code-review** sequential agents; skill descriptions rewritten as **trigger conditions only** (fixes "claims the skill then wings it"); consolidate overlapping skills; **e2e-test the workflow itself** (https://blog.fsck.com/2025/12/18/superpowers-4/).
- **Compound engineering** (Every): Plan→Work→Review→**Compound** — every bug becomes a `docs/solutions/` entry with YAML metadata + a rule-or-hook update + a "would the system catch it next time?" check (https://every.to/chain-of-thought/compound-engineering-how-every-codes-with-agents, plugin: https://github.com/EveryInc/compound-engineering-plugin).
- **Context discipline** (HumanLayer ACE-FCA): keep always-loaded context lean; persist research/plan artifacts and compact status back into the plan (https://github.com/humanlayer/advanced-context-engineering-for-coding-agents).
- **Anthropic security-review GitHub Action** exists (AI SAST incl. crypto misuse) but is cloud-bound and optional for this repo (https://github.com/anthropics/claude-code-security-review).

## Fit with Murmur constraints

- All recommended mechanisms are **local shell scripts + in-repo markdown — zero cloud egress**; consistent with local-first.
- Hooks *strengthen* the lock model (they enforce that the process around `lock-security-reviewer` actually runs); they never replace domain review.
- The keychain-CLI blocker directly encodes release rule 3 (the 2026-06-27 11-hung-`security`-procs incident). `pkill security` must stay allowed.
- Solo dev + autonomous multi-phase preference (user's stated mode) is exactly the profile where deterministic gates matter most — no human between the agent and the trunk/keychain.
- Anti-goal: a 95-hook cathedral. Start minimal; advisory-first for anything with false-positive risk.

## Options & tradeoffs

- **A. Hooks-first minimal (S, low risk)** — checked-in `settings.json` + 5 guards + secret scan; fix the phantom claim. Makes past incidents physically unrepeatable.
- **B. Truth & diet (S, low risk)** — citation policy (symbol names > line numbers), prune hook-covered prose, deprecate RELEASE-CHECKLIST.md, trigger-condition skill descriptions, `disable-model-invocation` on release-murmur.
- **C. Learnings loop (M, low risk)** — per-agent two-tier learnings + extractor step in ship-feature + `/learn` + `/curate-learnings`; inject Recurring patterns into dispatch prompts.
- **D. Evidence gates (M–L, medium risk)** — verifier/lock-reviewer verdicts as schema'd JSON in `.claude/tmp/<task>/`, archived to `.claude/traces/`; a finish-guard hook blocking commit until lane gates PASS; advisory mode first; harness self-test in `scripts/ci.sh`.
- **E. Selective extras (defer)** — Stop-hook DoD for unattended runs, spec-review/code-review split, PostToolUse auto-format (prettier/eslint; NOT cargo-heavy tools in the loop), wip-guard, orchestrator firewall, `.mcp.json`, GitHub Action SAST.

## Recommendation & first step

**A → B → C → D, E selectively.** The smallest verifiable slice: commit `.claude/settings.json` with exactly two PreToolUse hooks — (1) block `git push` to the `murmur` trunk, (2) block the `security`/keychain CLI — plus the staged-diff secret scanner; verify RED-before-GREEN (synthetic push + `security find-identity` both blocked with exit 2 in a fresh session); then replace every phantom-hook mention with a pointer to the real file.

### Target layout (end-state)

```
.claude/
├── settings.json            # checked-in: hooks wiring + deny rules + env (reviewed like code)
├── settings.local.json      # personal (gitignored)
├── hooks/
│   ├── block-bash.sh        # trunk push / force push / security CLI / clippy --all-targets / codesign --deep
│   ├── secret-scan.sh       # staged-diff regex gate on git commit (64-hex DEK/KEK, PEM, tokens)
│   └── finish-guard.sh      # (phase D) lane gates must be PASS before commit — advisory first
├── rules/                   # existing 4, pruned of hook-covered prose; consider paths: scoping
├── skills/                  # existing 4 + /compound (or /learn + /curate-learnings)
├── agents/                  # existing 6 (+ optional spec-reviewer later)
├── learnings/               # per-agent: Recurring patterns (injected) + Run journal (append-only)
└── traces/                  # (phase D) per-task archived gate JSONs + MANIFEST
```

## Open questions / not verified

- Whether the user-level `superpowers` plugin injects runtime hooks (plugin manifests not inspected; contains no Murmur-specific guardrails either way).
- Deny-rule bypass status in the newest Claude Code builds (absolute-path bash bypass reported 2026) — treat deny rules as defense-in-depth, hooks as the enforcement layer, sandbox as the boundary.
- Hook latency on this machine (needs a measurement once hooks exist; budget ~200 ms).
- awesome-claude-code exemplar repos (tevm, metabase) reported at catalog confidence, not individually audited.

## Sources

Official: https://code.claude.com/docs/en/best-practices · https://code.claude.com/docs/en/hooks.md · https://code.claude.com/docs/en/skills.md · https://code.claude.com/docs/en/sub-agents.md · https://code.claude.com/docs/en/memory.md · https://code.claude.com/docs/en/configuration.md
Community: https://blog.fsck.com/2025/12/18/superpowers-4/ · https://github.com/obra/superpowers · https://every.to/chain-of-thought/compound-engineering-how-every-codes-with-agents · https://github.com/EveryInc/compound-engineering-plugin · https://github.com/humanlayer/advanced-context-engineering-for-coding-agents · https://github.com/humanlayer/12-factor-agents · https://paddo.dev/blog/claude-code-hooks-guardrails/ · https://blakecrosley.com/blog/claude-code-hooks · https://github.com/dwarvesf/claude-guardrails · https://github.com/anthropics/claude-code-security-review · https://github.com/hesreallyhim/awesome-claude-code
Local: `.claude/rules/agentic-workflow.md:43` + `CLAUDE.md` release rule 6 + `.claude/agents/release-engineer.md:17,94` + `.claude/skills/release-murmur/SKILL.md:32` (phantom hook claims) · commit `a9f51b4` (prose introduced, no hook ever committed) · `../urc-monorepo/.claude/{settings.json,hooks/,learnings/,traces/,workflows/,commands/}` (reference implementation).
