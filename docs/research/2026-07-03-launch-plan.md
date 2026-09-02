<!-- ws9 — distribution prep for the Tier 0-4 brain program (2026-07-03). Distribution is the standing
     verdict's unmoved gap ("un-findable under its own name"). This doc is the ready-to-fire launch plan;
     the ONE blocker to going live is the NAME decision (maintainer-only). No branding is deployed here. -->

# Launch plan — the brain program is shipped; distribution is the last gate

## Status: code complete, distribution ready-pending-a-name

The full Tier 0–4 brain program + capture-default merged today (PRs #158–#168, 11 PRs, each triple-verified: `cargo test --lib`/`ng build` green + adversarial-verifier + lock-security-reviewer). The moat is now **visible and mostly on by default** — semantic retrieval on, far-side capture on, the people/commitments brain revealed on day one, a native relationship Person page, a per-note privacy receipt. What remains is **not code — it is getting a stranger to find and try it.**

## The one blocker: the name

Per the deep-analysis (`2026-07-03-murmur-deep-analysis-v3-breakthrough.md`), **"Murmur" is un-findable under its own name**: a priced commercial twin **trymurmur.app / murmur.you** ($29 one-time, Nextaim GmbH) owns the exact name + search + price point, plus ~8 macOS namesakes. Every marketing dollar / HN upvote leaks to competitors via search until this is resolved. **This is a maintainer-only decision** — do not deploy branding under the colliding name first.

**Two paths (pick one before launch):**
1. **Rename** to a distinct, searchable mark (the repo's own `brain2` codename is un-collided but too generic — pick a real one). Highest-leverage; unblocks all paid/organic acquisition cleanly.
2. **Keep "Murmur" + hard-differentiate** in every title/description ("Murmur — the local-first meeting brain for Obsidian", always with the `murmur-io` qualifier + the owned-files/on-device hook). Cheaper, but fights the SERP.

## Ready-to-fire once the name is settled

### 1. Deploy the finished landing page (`landing/index.html`, ~1 hour, currently 404)
The 51 KB pricing site is complete (hero, honest egress table, 3-tier pricing, all 11 assets, relative paths). Enable GitHub Pages (Settings → Pages → Source: GitHub Actions) and add this **manual-trigger** workflow so it never auto-deploys under an undecided name:

```yaml
# .github/workflows/pages.yml  (workflow_dispatch only — deploy after the name is decided)
name: Deploy landing to Pages
on: { workflow_dispatch: {} }
permissions: { pages: write, id-token: write, contents: read }
concurrency: { group: pages, cancel-in-progress: true }
jobs:
  deploy:
    runs-on: ubuntu-latest
    environment: { name: github-pages, url: "${{ steps.deploy.outputs.page_url }}" }
    steps:
      - uses: actions/checkout@v4
      - uses: actions/configure-pages@v5
      - uses: actions/upload-pages-artifact@v3
        with: { path: landing }
      - id: deploy
        uses: actions/deploy-pages@v4
```
Then point the repo `homepageUrl` at the Pages URL (currently it just links to `releases/latest`).

### 2. Show HN + subreddit launch — lead with the un-clonable bundle
NOT "another local notetaker." Lead with what a cloud tool physically cannot match:

> **Show HN: <Name> — a local-first meeting brain that captures both sides of a call, on-device, into your own Obsidian files**
>
> Records mic + system audio (ScreenCaptureKit, no bot in the participant list), transcribes on-device (whisper.cpp), and builds a private relationship brain — a per-person dossier (who owes what, what changed, when you last spoke), cross-meeting memory, and speaker voiceprints — behind per-folder Touch ID encryption, exported as plain Obsidian `.md`. Nothing leaves the device unless you choose a cloud model, and each note carries a privacy receipt (`privacy-cloud-calls: 0`). Fully AGPL. macOS, Apple Silicon + Intel.

Targets: **Show HN** (Hyprnote's stack-twin hit ~270 pts — the demonstrated ceiling for this category), **r/ObsidianMD**, **r/macapps**, **r/LocalLLaMA**, **r/privacy** (time it to the Otter/Brewer wiretap-ruling news for the local-first angle). Ship a 60-second screen recording of the one demo no competitor can reproduce: *[headphones on] far-side captured with no bot → the [[Anna]] Person page (2 open commitments, shares [[Project Atlas]]) → lock the folder → Touch ID masks it → unlock → it returns.*

### 3. Obsidian community-plugin companion (durable second channel; defer until there are users)
A thin `Open in Murmur` / index-Murmur-notes / insert-`obsidian://`-block-ref plugin lists the app in the ~1.5M-MAU directory. Real TS + a PR to `obsidianmd/obsidian-releases` — do it after the launch has traction.

### Not now
- **The $59 Pro license** — premature at ~20 lifetime downloads; awareness (not monetization) is the constraint, and the twin already owns the one-time-price slot. Ship it once there's a funnel.
- **Finalize the CLA** — a defensibility/dual-license move (separate from acquisition); Lucas's ~13 commits need retroactive sign-off.

## Also deferred from the code program (honest bar)
- **The onboarding "capture the whole call" step** (primes the Screen-Recording TCC prompt at setup + the recording-consent/BIPA disclosure) — a fast-follow FE PR; capture-default already degrades gracefully to mic-only without it.
- **Signed-Mac verifications**: real TCC prompt + dual-stream + headphone survival; the AFM native `@Generable` sidecar + a zero-egress network capture; live voiceprint behavior.
- **The measured-proof RUN** (the standing verdict's actual breakthrough gate): the shipped RAG bake-off + the new DER harness on the maintainer's real bilingual vault + gold labels → the first publishable recall@k / DER numbers. This is a *run*, not a build — and the harnesses now all exist.
