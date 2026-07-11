# `.codex/` — Murmur's Codex agent setup

The map of this directory, and the decisions behind it. Rationale + the audit that drove this:
`docs/research/2026-07-02-claude-setup-audit.md`.

Design principle (from the audit): **prose rules are advisory; hooks are deterministic.** Keep the
war-story rules for *why*, and encode the *already-paid-for incidents* as hooks the agent cannot
talk itself out of. Start lean — the failure mode of this exercise is a 95-hook cathedral for a
one-person repo, so anything with false-positive risk ships advisory/opt-in first.

## Layout

| Path | What | Loaded |
| --- | --- | --- |
| `../AGENTS.md` | Project charter + binding rules index + release rules | Codex autoload |
| `rules/*.md` | The 4 binding rulesets (`rust-tauri`, `angular-zoneless`, `lock-model`, `agentic-workflow`) | read on matching task |
| `claude-settings*.migrated.json` | Original Claude Code hook/permission settings kept as migration reference | not Codex-active |
| `hooks.json` | Codex hook wiring for the Bash guardrails | Codex hook layer, after trust |
| `hooks/` | Deterministic guardrail scripts — see [`hooks/README.md`](hooks/README.md) | via `hooks.json` + CI selftest |
| `lib/trace-span.sh` | Best-effort JSONL observability span helper | on demand |
| `../.agents/skills/*/SKILL.md` | Runbooks: `release-murmur`, `tauri-dev`, `ship-feature`, `research`, `dreaming`, CI helpers, learnings helpers | Codex repo skills |
| `agents/*.toml` | Codex custom subagent roles (2 builders, 2 verifiers, releaser, researcher) | on explicit spawn |
| `learnings/*.md` | The compounding-lessons loop — see [`learnings/README.md`](learnings/README.md) | injected at dispatch |
| `traces/` | Optional per-task evidence archive | manual |
| `tmp/` | Gitignored scratch (gate JSONs, in-flight `trace.jsonl`) | runtime |

## What's active vs dormant vs deferred (be honest about this)

**Active guardrails**: `block-bash.sh` (trunk push / `security` CLI / `clippy --all-targets` /
`codesign --deep` / `rm -rf /`) and `secret-scan.sh` (staged-diff secret gate on commit).
`hooks.json` wires the Bash guards for Codex; new or changed hooks still need to be reviewed/trusted
by Codex. `bash .codex/hooks/selftest.sh` proves they still block, and `scripts/ci.sh` runs that
selftest. The old credential-path deny list remains only in `claude-settings.migrated.json` as
migration reference; it is not Codex-active until reimplemented as a Codex hook or rule.

**Dormant scaffolding** (wired, waiting on the loop that feeds them): `finish-guard.sh` (advisory
until the ship-feature gates emit `.codex/tmp/<task>/*.json`; flip with `MURMUR_FINISH_GUARD=enforce`),
`autoformat.sh` (off until `MURMUR_AUTOFMT=1`), the learnings loop + trace archive.

**Deliberately deferred** (documented, not built — enable when a real need appears):

- **`wip-guard` (WIP=1)** — urc blocks a 2nd active feature branch. Lower value for a solo dev; adds
  latency to every Bash call. Enable by adding a `PreToolUse(Bash)` hook counting active
  `.codex/tmp/*/` task dirs if branch-thrashing becomes a real problem.
- **Stop-hook DoD gate** — block turn-end until `cargo test --lib`/`ng lint` pass, for fully-unattended
  runs. Deferred to avoid a noisy Stop hook layered on the existing user-global one; revisit for long
  autonomy sessions.
- **PostToolUse TS/Angular auto-format** — intentionally NOT added; a blind prettier pass fights the
  zoneless rules (dir-per-component ts/html/scss, per-component style budget). `npx ng lint` stays the FE formatter.
- **`.mcp.json` (checked-in MCP servers)** — none needed yet; MCP tools reach the session already.
- **GitHub Action SAST** (`anthropics/claude-code-security-review`) — cloud-bound + needs an API-key
  secret; out of step with local-first + solo. One YAML file away if desired.

## Identity interlocks (unchanged, still binding)

Commits/PRs authored ONLY by `QueaT <kgm004a@gmail.com>`, no AI co-author trailers; `gh` account
`JakubGawr`; trunk is `murmur` reached via PR only (now enforced by `block-bash.sh`);
`com.meetnotes.app` immutable.
