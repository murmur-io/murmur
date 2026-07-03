<!-- Generated 2026-07-02 via /research (murmur-researcher fan-out, 2 agents: Granola deep-dive + local-first positioning/demand). Pricing/funding/version = point-in-time. -->
# Research: Murmur vs Granola — how do we differ, where do they beat us, how do we stand as the local-first alternative

## TL;DR / Verdict

**Granola and Murmur solve the same job (bot-free meeting notes) with opposite architectures, and the gap has *widened* since our June doc — in both directions.** Granola (July 2026) is a four-platform (macOS/Windows/iOS/Android since Jul 1), $1.5B-valued, enterprise-pivoting **cloud** product: transcription runs on Deepgram/AssemblyAI, summaries on OpenAI/Anthropic, notes live indefinitely in a US AWS VPC, **customer data trains their models by default** (opt-out; org-enforced only on Enterprise $35), audio is deleted (no replay), there is still **no Obsidian export and no on-device mode**, and the free tier now paywalls notes older than 30 days. Murmur is the structural inverse: on-device Whisper, SQLCipher storage, per-folder Touch-ID lock, replayable encrypted audio, redaction firewall on any cloud egress, fully-local Ollama path, owned `.md` files in the vault, local MCP.

**Honest deltas:** Granola beats us on the *craft of the core note loop* (their signature "enhance my sloppy bullets" merge — we only append `## My notes` verbatim), on frictionless calendar-driven capture, and on distribution (teams/mobile/Windows/velocity). We beat them on everything privacy/ownership — and on **live in-meeting AI** (`@brain` on the rolling transcript; Granola has nothing live). Two fresh threats: Granola's SEO now **co-opts "local-first"/"device-level" language** for a cloud-ASR product, and their new public REST API makes third-party Obsidian sync plugins viable, softening the lock-in complaint.

**One-line answer:** *Murmur jest tym, czym ludzie myślą, że jest Granola* — no bot, notes that stay yours, and nothing leaves your Mac.

## Co już mamy (z repo, z file:line)

- June-2026 baseline: `docs/COMPETITIVE-LANDSCAPE.md:13` (Granola entry), `:71` (highest-risk assessment) — directionally correct, stale on platforms/API/pricing/privacy-flap.
- **"Your notes + AI" surface exists but is adjacent, not merged:** typed in-meeting notes + standalone-`@brain` threads live on the conversation-first record screen (`src/app/features/record/meeting-conversation.component.ts:52-64`); agent-proposes-notes with explicit accept. But `SummarizeRequest` carries transcript/meta/template/vault_titles/related_context — **no user notes** (`src-tauri/src/pipeline.rs:559-570`); typed notes are folded verbatim as `## My notes` after generation (`pipeline.rs:576-583`, `fold_manual_notes` `pipeline.rs:740`).
- **Live in-meeting agentic brain** (Granola has no live surface): `@brain` threads answer against live transcript + brain; voice+text; model decides answer-vs-draft.
- Calendar ingredients unwired into one loop: local zero-OAuth EventKit sidecar (`src-tauri/src/calendar.rs:1-19`) + running-meeting-app poll/nudge (`src/app/features/record/record.component.ts:1013-1016`).
- Lock model (SQLCipher + per-folder seal, verify-before-destroy) = the answer to Granola's "we delete audio, that's privacy" framing — we *keep* audio, encrypted and replayable.
- Local MCP `127.0.0.1:8765`; providers claude_code/anthropic(BYO, redacted)/ollama(zero egress); AGPL-3.0, free.

## Findings

### Granola, state of July 2026 (verified live 2026-07-02)

| Axis | Granola | Confidence |
|---|---|---|
| Capture | Bot-free, computer audio; auto-detects ad-hoc calls on mic activity | High (homepage) |
| Platforms | macOS, Windows (Jun 2025), iOS (phone calls), **Android Jul 1 2026** (Play listing `ai.granola` live) | High; one aggregator still claimed no Android — Play listing wins |
| ASR | **Cloud**: "Deepgram and Assembly" (their security page) | High (primary) |
| LLM | OpenAI + Anthropic; model picker on Business | High |
| Audio | **Deleted after transcription — no replay** (their own blog admits it) | High |
| Storage | US AWS VPC, retained indefinitely (Enterprise auto-delete only) | High |
| Training | **On customer data BY DEFAULT**, settings opt-out; org-wide off only on Enterprise | High (primary) |
| Compliance | SOC 2 Type II, GDPR/DPA | High |
| Pricing | $0 (notes >30 days paywalled) / $14 Business / $35 Enterprise per user/mo | High (official docs); "25-note lifetime cap" = aggregator, unverified |
| MCP | Cloud `mcp.granola.ai` (OAuth, ~100 rpm, 6 tools; transcripts paid-only; free = 30-day window) | High (docs) |
| API | **NEW**: public REST `public-api.granola.ai/v1`, Business+ (personal beta + enterprise keys) | High (docs) |
| AI features | Agentic cross-meeting Chat + inline citations (Apr 21 2026), Recipes, Briefs (May 20 2026) — all **post-meeting**; no live in-meeting assistant found | High/medium |
| Collaboration | Spaces, shared folders, @mentions, transcript editing, MS SSO | High |
| Obsidian | **Still no export** — community reverse-engineers the local cache (Thacker write-up; ≥3 GitHub sync plugins; forum thread) | High |
| Security record | Tenable TRA-2025-07 (leaked AssemblyAI key, ~300 alpha users' transcripts); PromptArmor Mar 2026 (share links public-by-default + prompt-injection exfil; remediated); Apr 2026 press cycle ""Private" AI notes are public by default" | High (advisories primary) |
| Positioning drift | May 2026 SEO blog co-opts "local-first"/"device-level" wording while ASR stays cloud | High |

### Where Granola genuinely beats us (be honest)

1. **Enhance-my-notes merge** — your sloppy bullets are the *skeleton*, AI expands each with transcript detail; reviewers consistently name this the moat ("better results than either pure AI summarization or manual note-taking alone"). **We concatenate; they synthesize.** Real gap.
2. **Zero-friction capture loop** — calendar sync + pre-meeting notification + mic-activity auto-detect. Our EventKit sidecar + meeting-app nudge exist but aren't wired into one "your 10:00 with Anna is starting — Record?" loop. Partial gap.
3. **Auto-templates per meeting type** (1:1/standup/discovery). Partial gap (we have note_style/Recipes, manual).
4. **Distribution:** 4 platforms, teams/Spaces, SSO, $192M raised, biweekly cadence. Structural — not ours to contest now.
5. Polish/onboarding (we require provider/model setup). Grind, not feature.

### Where we structurally win (they can't follow without a rebuild)

1. **Nothing leaves the Mac** (ollama path = zero egress; cloud path = redaction firewall). They stream every meeting to Deepgram/AssemblyAI + OpenAI/Anthropic.
2. **No training on you.** Their default is train-on-your-data (opt-out buried in Settings; enforced org-wide only at $35/seat).
3. **Audio survives** — encrypted, replayable, pinnable timeline. Theirs is gone forever ("you cannot replay the recording" — their own blog). Our lock model is the counter to their "deletion = privacy" framing.
4. **Owned files** — plain `.md`, front-matter, `[[wikilinks]]`, block-refs, `.canvas` in *your* vault vs notes held in their DB behind a 30-day free-tier paywall and a paid API.
5. **Local MCP over on-device data** vs cloud OAuth MCP with rate limits and paid-only transcripts.
6. **Live in-meeting agentic brain** — @brain threads on the rolling transcript; Granola's AI is strictly post-meeting. We are *ahead* here, not behind.
7. **No "private by default" link leaks** — there is no link. (Apr 2026: their share links were viewable by anyone with the URL.)

### Demand signals for the local-first segment

- Hyprnote/Char Launch HN: 270 pts/180 comments; Meetily 13.2k GitHub stars ("your sensitive discussions shouldn't live on servers you don't control"); recurring "I built a local Granola alternative" HN posts (Apr & Jun 2026).
- Obsidian users hack around Granola: cache reverse-engineering + ≥3 community sync plugins + forum threads — people *build tooling* to get what we ship natively.
- Real user language worth reusing: "hosted alternatives don't make sense if you can record locally"; companies IT-ban cloud notetakers; counter-signals to respect: "work conversations are not private anyways", "local quantized models are still much worse than frontier cloud" (our answer: provider seam — frontier cloud *through the redaction firewall*, or fully local, user's choice).
- Even pro-Granola Reddit aggregations credit it with "on-device processing" **which its own security page contradicts** — the market *wants* the local story so badly it projects it onto a cloud product. That's our positioning gold.
- Segment size: niche but durable and vocal (regulated professions, IT-restricted, EU/GDPR, PKM/Obsidian crowd). No hard numbers — proxies only.

## Fit z ograniczeniami Murmur

- The one copy-worthy feature (enhance-flow) rides existing seams: `manual_notes` already in SQLite (canonical), merge = prompt/template change on `SummarizerProvider`, cloud-bound text already passes redaction, output stays vault-`.md`. No new deps, no new egress.
- Calendar-nudge loop touches notifications/TCC → **needs a signed build on a real Mac to verify** (honesty bar).
- Do NOT copy: Spaces/link-sharing (cloud egress + multi-user), mobile (macOS-first), cloud Chat. Windows = L-effort, separate decision.
- Watch: Granola's "local-first" term co-optation → our copy must define the term concretely (on-device ASR, on-device storage, replayable local audio, zero training), not just claim the label.

## Opcje i tradeoffy

1. **"Enhance my notes" mode (S–M, low risk, highest impact).** Pass `user_notes` into `SummarizeRequest`; template variant: user bullets = skeleton, expand each from transcript, keep wording/order, add "Also discussed". Keep verbatim `## My notes` as fallback/toggle. RED test: bullets-present changes structure; bullets-absent stays byte-identical (extend `pipeline.rs:1150-1156`). Converts our biggest craft gap into a superiority claim (their enhance + our live @brain + local).
2. **Close the calendar capture loop (M, medium risk — TCC/real Mac).** Wire EventKit sidecar + meeting-app poll into one record nudge with title/attendees pre-filled.
3. **Auto-template by meeting type (S, low).** Calendar heuristics (recurring 1:1, external attendees) pick a recipe/note_style. Multiplier on #2.
4. **Granola→Murmur importer (M, medium risk).** Churn-capture wedge at the moment of the free-tier squeeze (30-day paywall); reads their local cache (unofficial, shifting) or paid API. Log as candidate, don't build yet.
5. **Refresh `docs/COMPETITIVE-LANDSCAPE.md` Granola entry (S, zero risk).** 4 platforms, API, pricing restructure, training-default, Apr-2026 flap, term co-optation, audio-deletion tradeoff.

## Rekomendacja i pierwszy krok

**Do #1 (enhance-flow) next; do #5 (doc refresh) opportunistically.** Smallest verifiable slice for #1: add `user_notes: Option<String>` to `SummarizeRequest` (`pipeline.rs:559`), one template branch, RED-before-GREEN test pair, then manual A/B on 2–3 real Polish meetings on both `claude_code` and `ollama`.

**Positioning (grounded in found user language):** lead with three concrete claims Granola architecturally cannot copy — *replayable encrypted local audio* / *zero egress (or redacted egress) + never training data* / *your notes are `.md` files you own, not a $14/mo hostage behind a 30-day paywall*. Headline: **"Murmur is what people think Granola is."**

## Otwarte pytania / czego nie udało się zweryfikować

- Original Verge article on the Apr-2026 link-sharing flap (3 concurring secondary sources; couldn't fetch primary).
- Free-tier "25-note lifetime cap" vs official 30-day window — official docs say 30-day; the cap may be stale/A-B'd.
- Whether Granola's enhance uses bullets as hard skeleton vs soft conditioning — inferred from reviews; define our target UX by our own taste.
- Exact Briefs launch date (May 20 2026 entry may be a relaunch).
- Live in-meeting assistant at Granola: no evidence found (medium confidence it doesn't exist).
- Segment size: proxies only (HN points, stars, plugin activity).

## Sources

**Granola primary (fetched 2026-07-02):** granola.ai (homepage) · granola.ai/pricing · granola.ai/updates · granola.ai/security · granola.ai/blog/local-first-ai-notetaker-vs-cloud · docs.granola.ai/introduction (API) · docs.granola.ai/help-center/sharing/integrations/mcp · docs.granola.ai/help-center/managing-your-account/subscriptions-and-billing · play.google.com/store/apps/details?id=ai.granola

**Security/privacy:** tenable.com/security/research/tra-2025-07 · promptarmor.com/resources/granola-ai-security-risks-and-remediations · techbuzz.ai/articles/granola-s-private-ai-notes-are-public-by-default · themeridiem.com/enterprise/2026/4/2/ai-meeting-tools-hit-privacy-inflection-as-granola-default-exposes-notes

**Funding/reviews/sentiment:** techcrunch.com/2026/03/25/granola-raises-125m… · zackproser.com/blog/granola-ai-review · tldv.io/blog/granola-review/ · aitooldiscovery.com/guides/granola-ai-reddit · get-alfred.ai/blog/granola-pricing · hedy.ai/post/granola-redesign-alternative-hedy/

**Demand signals:** news.ycombinator.com/item?id=44725306 (Hyprnote Launch HN) · id=47612681 · id=48518759 · id=47527778 · id=47628018 · github.com/Zackriya-Solutions/meeting-minutes · github.com/fastrepl/anarlog · josephthacker.com/hacking/2025/05/08/reverse-engineering-granola-notes.html · github.com/dannymcc/Granola-to-Obsidian · github.com/tomelliot/obsidian-granola-sync · github.com/amittell/obsidian-granola · forum.obsidian.md/t/plugin-granola-meetings-simple-sync/111950

**Key code refs:** `src-tauri/src/pipeline.rs:559-583,740` (summary transcript-only; notes folded verbatim) · `src/app/features/record/meeting-conversation.component.ts:52-64` (notes + @brain surface) · `src/app/features/record/record.component.ts:1013-1016` (meeting-app nudge) · `src-tauri/src/calendar.rs:1-19` (EventKit sidecar) · `docs/COMPETITIVE-LANDSCAPE.md:13,71` (June baseline)
