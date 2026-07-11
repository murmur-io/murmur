<!-- Dreamed 2026-07-06 via /dreaming. Prototypes are vibe-prototypes (fake data, not production). -->
# Dream: CKEditor AI — beyond "rewrite this"

## The spark
- **Adversarial Review** — AI plays devil's advocate and finds weaknesses in YOUR arguments before someone else does
- **Argument DNA Map** — visual node graph of claims → evidence → conclusions (click node = jump to paragraph)
- **Temporal freshness layer** — AI flags sentences that may be stale/outdated and why

## The wide spread (graveyard)
1. Adversarial review / "challenge me" ✅ SURVIVOR
2. Argument map / claim-evidence visualization ✅ SURVIVOR  
3. Temporal freshness flags ✅ SURVIVOR
4. Ghost co-author (AI writes next paragraph, you accept/reject) — too generic, Copilot does it
5. Voice annotations that float as comments — interesting but audio in browser is complex
6. Contradiction detector (intra-document) — merged into adversarial
7. Live dynamic summary in sidebar — nice but really just another rewrite variant
8. Semantic anchor links between related ideas — elegant but hard to show in prototype
9. Claim confidence markers (certain vs. speculative) — good but niche
10. "Explain to 5yo" hover tooltips — useful, not spectacular
11. Reverse outline (generate outline FROM existing prose) — quick win, already in some tools
12. Document stress test (simulate critical reader dropping off) — interesting for marketing
13. Reading difficulty heatmap — more analytics than AI
14. Auto-glossary generation — useful utility, not a spark
15. Cascade rewrite (change section → AI suggests what else to update) — complex, useful
16. Version archaeology (explain WHAT changed and WHY in revision history) — genuinely novel
17. Hallucination guard (flags implausible factual claims) — too passive/scary for prototype
18. "Write like me" style learning — too generic
19. Cross-language real-time translation — infrastructure problem
20. AI "track changes" explainer — clever but niche

## Why these survived
- **Adversarial Review**: stands on the gap nobody fills — AI that FIGHTS your document, not helps it. "Strengthen it" vs "rewrite it". The emotion: productive fear.
- **Argument Map**: visual, immediately clear, impossible to fake without real NLP. The emotion: seeing your thinking from outside.
- **Freshness flags**: temporal awareness is uniquely valuable for living documents (proposals, strategy docs). The emotion: trust.

## Prototype
- Path: `docs/dreams/prototypes/ckeditor-adversarial/index.html`
- Shows: Adversarial Review mode — click "Challenge Me", AI finds logical weaknesses, you can "Strengthen with AI" per challenge
- Fakes: AI analysis (hardcoded challenges for a business proposal document)

## What it'd really take
- CKEditor plugin (medium, 1-2 weeks): custom sidebar + annotation integration
- Real AI: Claude/GPT call on document text → structured JSON of challenges
- The "strengthen" suggestion: second AI call with challenge context
- **M size**, no platform limits — runs in any browser

## Verdict
(pending user)
