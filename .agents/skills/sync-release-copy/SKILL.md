---
name: sync-release-copy
description: Use before cutting a Murmur release (or whenever the user wants the public-facing copy checked/refreshed) to verify the landing page (`landing/index.html`) and the GitHub repo description still match what the app actually does now, and update them. Diffs the change surface since the last SHIPPED release, cross-references every public claim against the real code, flags stale/wrong/missing copy with evidence, then proposes edits and — on approval — lands them via a PR to `murmur` (the page auto-deploys) plus `gh repo edit`. Text only; screenshots are out of scope. Use whenever the user says the landing/description is out of date, wants marketing copy synced to reality, or runs this as a release preflight.
---

# /sync-release-copy — keep Murmur's public copy true to the app

Before a release, the two places Murmur describes itself **to the outside world**
drift out of sync with what the app actually does. This skill finds that drift and
fixes it. **Text only** — screenshots are explicitly out of scope (a human eye
regenerates those).

## The two surfaces this skill owns

1. **Landing page** — `landing/index.html` (single ~56 KB file) + `landing/assets/`.
   Served at **https://murmur-io.github.io/murmur/**. `.github/workflows/pages.yml`
   **auto-deploys it on any push to `murmur` that touches `landing/**`** — so a merged
   landing PR goes live with no extra step. Copy blocks it carries: the hero, the
   feature cards, the **privacy** section, the **Free/Pro/Team** pricing, footer.
2. **GitHub repo metadata** — the `description` string + `homepageUrl`, editable with
   `gh repo edit -R murmur-io/murmur`. (Current homepage is the landing URL above.)

## Core principle

**Every proposed claim must be grounded in real code or a merged PR** — cite the
`file:symbol` or `#PR`. This skill never invents a feature, never softens a privacy
claim below what the lock model actually enforces, and never adds a competitor
comparison to user-facing copy. It **proposes a diff and waits for approval**; it does
not silently rewrite marketing.

## Hard constraints (each has bitten before — verify every one)

1. **`gh` active account MUST be `JakubGawr`** (`gh auth status`). A second account
   `jakub-united` may also be logged in — do not use it.
2. **Commit/PR author MUST be `JakubGawr <63911380+JakubGawr@users.noreply.github.com>`. NO Codex trailers**, no
   `Co-Authored-By: Codex`, anywhere. (Repo git config is already JakubGawr.)
3. **NEVER `git push origin murmur` directly** — the trunk is `murmur`; land landing
   edits via a PR (`gh pr create --base murmur` → `gh pr merge`). `block-bash.sh`
   rejects a direct trunk push anyway.
4. **Always `-R murmur-io/murmur`** on every `gh` call (the repo lives in the
   `murmur-io` org; a bare `gh` may resolve the wrong remote).
5. **No competitor comparisons in user-facing copy** — strip any competitor name-drop
   from landing/description (internal research may name them; the public copy may not).
6. **Truthfulness bar** — a claim the current code does NOT do is a bug, not a nicety.
   Privacy claims in particular must match the real lock model (see
   `.codex/rules/lock-model.md`): "nothing leaves the device", "two encryption layers",
   "every read is gated", "seals verify-before-destroy" are load-bearing — only keep
   them if `meeting_is_unlocked`/`visibility_clause` + the seal/verify path still back
   them (grep the symbols; trust code, not docs).
7. **Screenshots are OUT OF SCOPE.** If a `landing/assets/*` image plainly shows a
   screen the app has since redesigned, **flag it in the report** for the user to
   regenerate — never swap or edit an image.

---

## Stage 1 — Baseline: what shipped last, and what changed since

**Do NOT use `git describe --tags` for the baseline** — local tags lag (they can read
`v0.6.4` while `v0.7.4` is already published). The **authoritative last-shipped
version is the latest GitHub release**:

```bash
cd /Users/jakubgawronski/Projects/meetnotes
LAST=$(gh release view -R murmur-io/murmur --json tagName -q .tagName)   # e.g. v0.7.4
git fetch --tags origin >/dev/null 2>&1                                  # tag may be missing locally
grep -m1 '"version"' package.json                                        # the version being released now
```

Compute the **change surface** since that release — this is "the newest changes" the
copy must be checked against:

```bash
git log --oneline "$LAST"..HEAD           # commit subjects since last release
gh pr list -R murmur-io/murmur --state merged --base murmur -L 60 \
  --json number,title,mergedAt -q '.[] | "\(.number)  \(.title)"'   # merged PR titles
```

If `$LAST` isn't a local ref even after fetch (releases can outrun pushed tags), fall
back to the merged-PR list alone plus the release notes of the last few
`gh release view` entries — they describe what already shipped, so anything NOT in them
is a candidate "new, maybe-unmentioned" feature.

## Stage 2 — Ground truth: read both surfaces + the app's real current surface

Read what we currently claim, and what the app currently is:

- **Landing copy** — `landing/index.html` (read the hero, every `<h3>`/feature card,
  the privacy section, the pricing tiers, the footer CTA).
- **Repo description** — `gh repo view murmur-io/murmur --json description,homepageUrl`.
- **The app's real current surface** — enumerate, don't assume:
  - features present: `ls src/app/features/` (analytics, ask, bar, detail, folders,
    graph, library, onboarding, record, settings, …).
  - providers / AI posture: `src-tauri/src/summarize/` + `settings/config.rs`.
  - the brain / MCP claims: `src-tauri/src/agent.rs`, `mcp.rs`.
  - the privacy/lock claims: grep `meeting_is_unlocked`, `visibility_clause`,
    `encrypt_file`, `seal_note` in `src-tauri/src/` — the code IS the privacy promise.
  - pricing: whatever `landing/index.html` asserts (Free/Pro/Team) — confirm it still
    matches current intent with the user if the release changes the model.

## Stage 3 — Drift analysis (dispatch to a subagent)

Hand Stages 1–2's material to a subagent (`murmur-researcher` or `general-purpose`)
and have it return a **structured findings list** — one row per public claim:

| Claim (verbatim, + where: landing §/description) | Status | Evidence | Proposed edit |
|---|---|---|---|
| … | `accurate` \| `stale` \| `wrong` \| `missing` | `file:symbol` or `#PR` | new wording, or "add card: …" |

The subagent MUST also check, explicitly:
- **Version/number literals** in the copy match the release being cut.
- **Pricing tiers** (Free/Pro/Team) still describe the current model.
- **Privacy claims** each map to a still-present code path (constraint #6) — a privacy
  claim with no backing symbol is `wrong`, downgrade or cut it.
- **Newly shipped, unmentioned** features from the Stage-1 change surface that deserve a
  landing card or a description refresh (`missing`).
- **Competitor mentions** to strip (constraint #5).
- **Stale screenshots** to FLAG only (constraint #7) — a separate note, no edit.

Instruct it: cite evidence for every non-`accurate` row; propose no claim it cannot
ground in a symbol or a merged PR.

## Stage 4 — Present the consolidated diff, wait for approval

Show the user, in one message:
1. the **landing edits** as a concrete diff of `landing/index.html`,
2. the **new repo `description`** (and `homepageUrl` if it changed),
3. the **flag list** (stale screenshots, anything you chose NOT to auto-edit).

**Stop and wait for an explicit OK.** This is marketing copy that deploys live to real
users the moment the landing PR merges — do not proceed on assumption.

## Stage 5 — Apply (only after approval)

**Landing → a PR to `murmur`** (never a direct push; the merge auto-deploys via
`pages.yml`):

```bash
git checkout -b chore/sync-release-copy-<ver>
# apply the approved edits to landing/index.html
git add landing/index.html
git commit -m "chore(landing): sync copy to <ver> — <one-line what changed>"
git push -u origin chore/sync-release-copy-<ver>
gh pr create -R murmur-io/murmur --base murmur \
  --title "chore(landing): sync copy to <ver>" \
  --body "Refresh landing copy for <ver>. <evidence-cited summary>."
# after checks: gh pr merge --squash --delete-branch
```

**Repo description → `gh repo edit`** (takes effect immediately, no PR):

```bash
gh repo edit murmur-io/murmur --description "<approved description>"
# only if it moved: --homepage "https://murmur-io.github.io/murmur/"
```

Confirm both landed: `gh repo view murmur-io/murmur --json description` and, after the
PR merges, the `Deploy landing to Pages` run in Actions is green.

---

## Quick reference

| Need | Command |
|---|---|
| Authoritative last shipped version | `gh release view -R murmur-io/murmur --json tagName -q .tagName` |
| Change surface since last release | `git log --oneline <tag>..HEAD` + merged-PR list |
| Read current description | `gh repo view murmur-io/murmur --json description,homepageUrl` |
| Set description | `gh repo edit murmur-io/murmur --description "…"` |
| Landing source | `landing/index.html` (+ `landing/assets/`) |
| Landing deploy trigger | push to `murmur` touching `landing/**` → `pages.yml` |
| Landing URL | https://murmur-io.github.io/murmur/ |

## Common mistakes

- **Using `git describe` as the baseline** → you diff against a stale tag and miss a
  whole shipped version. Use `gh release view` (Stage 1).
- **Editing a landing image** because it "looks old" → out of scope; flag it (constraint #7).
- **Pushing landing straight to `murmur`** → blocked and wrong; PR only (constraint #3).
- **Keeping a privacy line the code no longer backs** → truthfulness bug; grep the
  symbol before you keep the claim (constraint #6).
- **Auto-applying without showing the diff** → this deploys live; always Stage 4 first.
