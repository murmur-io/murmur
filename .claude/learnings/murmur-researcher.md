# Learnings — murmur-researcher

## Recurring patterns
<!-- Curated, binding. Prepended to every dispatch. Keep ≤ ~20 bullets. -->

- **Cite or it doesn't exist.** Every external claim → a URL someone actually fetched; every claim
  about our code → `file:line` (grep the symbol to confirm it still exists — the big files drift).
- **Trust code, not docs.** `docs/STATUS.md` and friends have been repeatedly wrong. When a claim is
  load-bearing, open the file and confirm against the current tree; distrust your own first read.
- **Deliver a decision, not a survey.** End with one recommendation + the smallest verifiable next
  step (a spike), not a list of links. Weigh confidence; say what you could NOT verify.
- **Name commodity vs differentiated.** If a feature is table-stakes (local Whisper, Ollama, "some
  Ask"), say so — the moat is usually in the integration (local-first + owned Obsidian files + no
  per-seat AI fee), not a single feature.
- **Ground every angle against Murmur's real constraints:** local-first / privacy (cloud egress must
  be loud + redacted + consent-gated), Obsidian-native owned `.md`, SQLite-canonical, provider seam
  + redaction firewall, macOS-first, CI honesty bar.
- **Read-only for app code.** The research skill writes ONLY the report in `docs/research/`.
- **Harness gotcha:** Playwright defaults colorScheme LIGHT and a mock-field typo poisons judge
  rounds — eyeball PNGs yourself before trusting a scored verdict.

## Run journal
<!-- Append-only, newest first. -->

### [2026-07-02 .claude audit] Setup-audit fan-out
- **Pattern:** the `.claude` audit surfaced the phantom `block-bash` hook (docs-drift in the
  trust-code-not-docs layer) and thousands-of-lines file:line drift.
- **Caught by:** operator (this run) + cross-check across two independent code audits.
- **Lesson:** for meta/setup research, verify claimed artifacts EXIST (grep the file), not just that
  the prose asserts them. Report: `docs/research/2026-07-02-claude-setup-audit.md`.
- **Status:** distilled (2026-07-02)
