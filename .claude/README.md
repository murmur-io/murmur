# `.claude/` — Murmur's agent setup

The map of this directory, and the decisions behind it. Rationale + the audit that drove this:
`docs/research/2026-07-02-claude-setup-audit.md`.

Design principle (from the audit): **prose rules are advisory; hooks are deterministic.** Keep the
war-story rules for *why*, and encode the *already-paid-for incidents* as hooks the agent cannot
talk itself out of. Start lean — the failure mode of this exercise is a 95-hook cathedral for a
one-person repo, so anything with false-positive risk ships advisory/opt-in first.

## Layout

| Path | What | Loaded |
| --- | --- | --- |
| `../CLAUDE.md` | Project charter + binding rules index + release rules | always (session start) |
| `rules/*.md` | The 4 always-on rulesets (`rust-tauri`, `angular-zoneless`, `lock-model`, `agentic-workflow`) | always (`@`-imported by CLAUDE.md) |
| `settings.json` | **Checked-in**: hook wiring + `deny` rules for credential paths + env defaults | always |
| `settings.local.json` | Personal overrides (gitignored) | always |
| `hooks/` | Deterministic guardrails — see [`hooks/README.md`](hooks/README.md) | via `settings.json` |
| `lib/trace-span.sh` | Best-effort JSONL observability span helper | on demand |
| `skills/*/SKILL.md` | Runbooks: `release-murmur`, `tauri-dev`, `ship-feature`, `research` | on match / on `/name` |
| `agents/*.md` | Subagent roles (2 builders, 2 verifiers, releaser, researcher) | on dispatch |
| `commands/*.md` | Operator slash-commands: `/learn`, `/curate-learnings` | on `/name` |
| `learnings/*.md` | The compounding-lessons loop — see [`learnings/README.md`](learnings/README.md) | injected at dispatch |
| `traces/` | Optional per-task evidence archive | manual |
| `tmp/` | Gitignored scratch (gate JSONs, in-flight `trace.jsonl`) | runtime |

## What's active vs dormant vs deferred (be honest about this)

**Active guardrails** (enforce today): `block-bash.sh` (trunk push / `security` CLI /
`clippy --all-targets` / `codesign --deep` / `rm -rf /`), `secret-scan.sh` (staged-diff secret
gate on commit), the `permissions.deny` credential-path list. These close the incidents that have
already cost the project. `bash .claude/hooks/selftest.sh` proves they still block (also runs in
`scripts/ci.sh`).

**Dormant scaffolding** (wired, waiting on the loop that feeds them): `finish-guard.sh` (advisory
until the ship-feature gates emit `.claude/tmp/<task>/*.json`; flip with `MURMUR_FINISH_GUARD=enforce`),
`autoformat.sh` (off until `MURMUR_AUTOFMT=1`), the learnings loop + trace archive.

**Deliberately deferred** (documented, not built — enable when a real need appears):

- **`wip-guard` (WIP=1)** — urc blocks a 2nd active feature branch. Lower value for a solo dev; adds
  latency to every Bash call. Enable by adding a `PreToolUse(Bash)` hook counting active
  `.claude/tmp/*/` task dirs if branch-thrashing becomes a real problem.
- **Stop-hook DoD gate** — block turn-end until `cargo test --lib`/`ng lint` pass, for fully-unattended
  runs. Deferred to avoid a noisy Stop hook layered on the existing user-global one; revisit for long
  autonomy sessions.
- **PostToolUse TS/Angular auto-format** — intentionally NOT added; a blind prettier pass fights the
  zoneless rules (dir-per-component ts/html/scss, per-component style budget). `npx ng lint` stays the FE formatter.
- **`.mcp.json` (checked-in MCP servers)** — none needed yet; MCP tools reach the session already.
- **GitHub Action SAST** (`anthropics/claude-code-security-review`) — cloud-bound + needs an API-key
  secret; out of step with local-first + solo. One YAML file away if desired.

## Identity interlocks (unchanged, still binding)

Commits/PRs authored ONLY by `QueaT <kgm004a@gmail.com>`, no Claude trailers; `gh` account
`JakubGawr`; trunk is `murmur` reached via PR only (now enforced by `block-bash.sh`);
`com.meetnotes.app` immutable.
