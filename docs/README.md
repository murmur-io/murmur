# `docs/` — what's in here, and which of it to believe

Twenty-odd markdown files accumulated over Murmur's life, and until this index existed there was no
way to tell a current reference from a planning note written before the product was renamed. Several
of them describe a product that no longer exists; all of them read as though they are current.

**The rule: nothing in `docs/` outranks the code.** Where a claim matters, open the cited file and
confirm the symbol still exists. That is not a caveat, it is the standing instruction in `CLAUDE.md`,
and it is why some agent definitions name `docs/STATUS.md` as a file to distrust.

For the current product, read [`README.md`](../README.md) in the repo root, and
[`landing/docs.html`](../landing/docs.html) for the user-facing guide.

---

## Current — maintained, safe to cite

| File | What it is |
| --- | --- |
| [`STATUS.md`](STATUS.md) | How to *verify* the current state, what a headless machine can't prove, and the oracle owning each shipped bug class. Deliberately not a second copy of the feature list. |
| [`RAG-BAKEOFF.md`](RAG-BAKEOFF.md) | The turnkey protocol for measuring retrieval quality on a real Mac with a real vault. Referenced by the retrieval skills as the re-run command. |
| [`DIARIZATION-EVAL.md`](DIARIZATION-EVAL.md) | The same, for speaker diarization and voiceprint recall. Cited from `src-tauri/src/eval/diarization.rs`. |
| [`USE-WITH-YOUR-AGENT.md`](USE-WITH-YOUR-AGENT.md) | Pointing your own AI agent at your vault, and the `vault-skills/` pack. |
| [`screenshots/`](screenshots) | The product screenshots the README uses. Generated — never hand-edited. See [`scripts/screenshots/README.md`](../scripts/screenshots/README.md) for the capture runbook. |
| [`research/`](research) | Dated, cited research briefs. Each one is a snapshot of its own date and says so; that is the format working as intended, not staleness. |
| [`research/2026-09-02-full-app-analysis.md`](research/2026-09-02-full-app-analysis.md) | The whole-app audit of 2.3.1 — brain and entity connections, MCP, memory, recording, stability, org sharing. Every `file:line` in it was opened during the audit; anchors drift with the tree, so grep the symbol, not the line number. |
| [`research/2026-09-02-prod-ready-tasks.md`](research/2026-09-02-prod-ready-tasks.md) | The task list that audit produced, with acceptance criteria per task. Unlike its neighbours this is a **live plan, not a snapshot**: execution state lives outside the repo, in `../.murmur-agent-tasks/prod-ready/STATUS.md`, and lands here only when the work closes. |
| [`superpowers/specs/`](superpowers) | Design specs, dated. The most recent ones are the live plans. |
| [`agent-harness-guide/`](agent-harness-guide) | How the `scripts/h` harness works. |
| [`dreams/`](dreams) | Dated ideation notes from `/dreaming` sweeps, each stamped with its own date. Not roadmap and not spec — but several did ship (receipts, dashboards, off-the-record), so read them as "where an idea came from", never as "what exists". |
| [`branding/`](branding) | Brand assets. |

## Superseded — kept for the record, do not follow

| File | Superseded by |
| --- | --- |
| [`RELEASE-CHECKLIST.md`](RELEASE-CHECKLIST.md) | The `release-murmur` skill. Carries its own SUPERSEDED banner and predates the rename; the skills reference it by name as *the stale doc*. Kept only for its first-run / mic / system-audio manual checks. |
| [`KILLER-FEATURES.md`](KILLER-FEATURES.md) | `README.md` → Status. A 2026-06 batch write-up: everything in it did ship, but it is a snapshot of that batch, not the shipped set. |
| [`DESIGN.md`](DESIGN.md) | The whole product. The original v1 design, partly in Polish, still titled "MeetNotes". |
| [`ARCHITECTURE-LOCAL-CLOUD.md`](ARCHITECTURE-LOCAL-CLOUD.md) | `docs/superpowers/specs/2026-07-04-murmur-server-spec.md` for the parts that shipped. Its Feature 5 (`TeamBrainProvider`, hosted MCP) remains deferred, and later research briefs cite it by section — which is why it stays put. |
| [`DESIGN-local-brain-orchestration.md`](DESIGN-local-brain-orchestration.md), [`PLAN-brain2-rag-voice.md`](PLAN-brain2-rag-voice.md), [`PLAN-finished-product.md`](PLAN-finished-product.md) | The brain that shipped. Historical blueprints, cited by name from `docs/research/`. |
| [`PHASE0-PLAN.md`](PHASE0-PLAN.md), [`PHASE1-PLAN.md`](PHASE1-PLAN.md), [`PHASE2-SYSTEM-AUDIO.md`](PHASE2-SYSTEM-AUDIO.md) | Shipped long ago. Phase-numbered plans from the first weeks. |
| [`PLAN-obsidian-notion-parity.md`](PLAN-obsidian-notion-parity.md) | Largely delivered by 2.0 (Spaces, boards, imports, the editor). Useful only as a record of what was aimed at. |
| [`V050-MORNING-REPORT.md`](V050-MORNING-REPORT.md), [`V050-RELEASE-HANDOFF.md`](V050-RELEASE-HANDOFF.md), [`V060-RELEASE-HANDOFF.md`](V060-RELEASE-HANDOFF.md) | Release handoffs for 0.5.0 / 0.6.0. Two versions of the same era that each claim to supersede the other. |
| [`COMPETITIVE-LANDSCAPE.md`](COMPETITIVE-LANDSCAPE.md), [`COMPETITIVE-COMPARISON.md`](COMPETITIVE-COMPARISON.md) | Nothing — but they are 2026-06/07 market sweeps, and a competitor's feature list is the fastest thing here to rot. Treat as dated. |

## UI prototypes — frozen

`appletv-shell-preview/`, `landing-preview/`, `macos-shell-redesign-preview/`, `quiet-glass-preview/`,
`settings-preview/`, `theme-preview/`, `download-progress-preview/`, `notes-feature/`

Standalone HTML mockups from various design passes, none wired into the app build. They record what a
design looked like when it was being argued about. The shipped design language lives in
`src/design-tokens/*.css` and `src/app/design-system/`, and `.claude/rules/angular-zoneless.md` §6b is
its binding description — a prototype that disagrees with those is a prototype, not a spec.

---

## Why nothing was deleted or moved

The obvious tidy — move the superseded files into `docs/archive/` — would have broken something real.
Half of them are cited by exact path from `docs/research/` briefs and from agent definitions
(`docs/STATUS.md`, `docs/KILLER-FEATURES.md` and `docs/COMPETITIVE-LANDSCAPE.md` are all named in the
`research` skill and the `murmur-researcher` prompt as places to read). Moving a file would turn every
one of those citations into a dangling path, trading a documentation problem for a broken one.

An index costs nothing and answers the actual question, which was never "where are these files" but
"which of them is true".
