<!-- Generated 2026-06-28 via murmur-researcher fan-out (2 angles: prior-art + Angular feasibility). Pricing/UI = point-in-time. -->
# Research: the in-meeting AI-assistant state "orb" — how leading apps do it + how we build it

## TL;DR / Verdict
Build a **single morphing gradient orb** whose STATE is expressed by changing **motion + color**, not by swapping widgets — the convergent pattern across ClickUp Brain, ChatGPT Advanced Voice, Apple Intelligence/Siri, and Gemini Live. The interaction the user wants for #28 — **click-to-start → click-to-stop toggle with an explicit Stop affordance** — is exactly ClickUp's "Talk to Text" floating-bar model, so it's the right call. **Fully buildable in our zoneless Angular stack with ZERO new deps**, ~2–4 kB CSS against the 16 kB budget; the repo already ships every primitive (the `[style.--level]` audio→CSS-var wave, a conic spinner, a pulse ring, thinking-dots, a shimmer). Biggest current gap = the **PROCESSING/"thinking" state** ("you don't know what it's doing") → solve with a **breathing orb + a shimmer label naming the substep** ("Szukam w notatkach… / Szukam w sieci…").

## The canonical 4-state model (industry-convergent; Gemini Live spells it out)
```
idle ──click──▶ listening ──click(stop)──▶ processing ──▶ answer ──▶ idle
 slow            mic-amplitude              NO amplitude:    settle to a
 breathe         reactive scale/bars        breathe + spin   steady glow
                                            + shimmer label
                                  error → static red
```
- **listening & speaking share the SAME amplitude widget** — only the source differs (mic-in vs playback-out). **processing is the one state that drops amplitude** for a pulse/spin (there's nothing to react to).
- Keep **ONE orb element and cross-fade its animation/color per state** (200–280 ms, spring ease) so it reads as one living entity morphing — not mounting/unmounting widgets (the ChatGPT/Siri lesson).

## What leading apps do (cited)
- **ClickUp Brain** = pink→purple gradient **star/✨ orb** + "animated loading indicators help users understand when the AI is processing"; **voice = a floating bar with hotkey/click toggle to start AND a Stop button to end** ([ClickUp Help — Talk to Text](https://help.clickup.com/hc/en-us/articles/37457270037271-Use-Talk-to-Text-in-Brain-MAX)). Validates our click-to-stop + floating-bar model. (Exact bar motion unverified — pages 403'd.)
- **ChatGPT Advanced Voice** = a glowing **breathing blue orb**; state = motion (pulse/breathe), not named colors; inline-in-chat since 2025-11-25 ([Croma](https://www.croma.com/unboxed/chatgpt-in-chat-voice-live-visuals-update)).
- **Apple Intelligence/Siri** = the orb became a **reactive screen-edge glow** — same idea (continuous audio-reactive gradient light), orb form fits a small bar ([iDownloadBlog](https://www.idownloadblog.com/2024/07/30/how-to-get-apple-intelligence/)).
- **Gemini Live** = explicit idle/listening/processing/speaking/error → visual spec ([gemini-cli #21109](https://github.com/google-gemini/gemini-cli/issues/21109)).
- **Processing treatments worth stealing:** breathing orb (ChatGPT) + **shimmer-text** (`background-clip:text` moving gradient, ~2 s loop — [Vercel AI SDK Shimmer](https://elements.ai-sdk.dev/components/shimmer), Raycast) + Notion's principle that a **custom on-brand motion beats a stock spinner** ([Fast Company](https://www.fastcompany.com/91192119/notions-new-animated-ai-assistant-looks-more-new-yorker-than-clippy)).
- **Reproducible orb recipe:** SmoothUI "Siri Orb" = layered `conic-gradient`s + animated `@property --angle` + `filter: blur()` (blur turns hard gradients into a liquid lava-lamp orb) ([source](https://github.com/educlopez/smoothui/blob/main/packages/smoothui/components/siri-orb/index.tsx)).

## Fit + feasibility (our stack — all grounded file:line)
- **Zero new deps** (no Lottie/GSAP/three.js) — pure CSS + inline SVG. ✅ rule §7.
- **Already shipped primitives:** `[style.--level]="store.level()"` signal→CSS-var audio binding (`record.component.ts:129-134`, CSS `:608-636`) = the listening-reactive pattern; `.orb.proc` conic spin (`:842-852`); `ask-pulse` ring (`:714-722`); thinking-dots (`assistant-actions.component.ts:192-218`); shimmer (`:778-792`). `RecorderStore.level()` is a **signal polled inside `toSignal`** — no component timer (`recorder.store.ts:52-66`).
- **One `computed()` state machine** collapses existing signals (`assistant.listening()`, `manualAskInFlight()`, top interaction `status`) → `idle|listening|processing|answer`, bound as `[class]="'is-'+orbState()"`. Pure computed → no NG0600.
- **Budget/a11y:** ~2–4 kB inline CSS (16 kB budget); orb `aria-hidden`, state announced via paired `role="status" aria-live="polite"`; `aria-busy` during processing.
- **prefers-reduced-motion:** global rule zeroes `animation` duration; ADD an explicit guard for the value-driven `transform: scale(calc(1 + var(--level)…))` and conic rotation (mirror `record.component.ts:751-757`).
- **WebView:** this Mac = macOS 26.5 → WKWebView well past Safari 16.4 (`@property`) + 12.1 (conic). Namespace `@property --orb-spin` (at-rules aren't view-scoped) OR use the simpler `transform: rotate()` element-spin (matches existing `.orb.proc`) to sidestep it.

## The concrete design (Option B — recommended)
A standalone `app-ai-orb` (`state = input<OrbState>()`, optional `level = input<number>()`), 56–72 px, in the recording bar + the assistant card head:
- **IDLE** — gradient orb (`--accent-gradient` core + breathing halo), slow `transform: scale` breathe.
- **LISTENING** — `transform: scale(calc(1 + var(--level)*0.18))` driven by `level()` + a reactive ring (opacity/scale ∝ level). Voice-reactive, the ClickUp/Siri look.
- **PROCESSING** — rotating conic-gradient ring (carved with `mask: radial-gradient`) + a **shimmer status label** naming the substep ("Szukam w notatkach…" → "Szukam w sieci…" → "Piszę odpowiedź…"). This is the loud disclosure point for **web egress**.
- **ANSWER** — settle to a steady gently-breathing accent orb (CSS entry-animation keyed on the class change — no component timer per rule §5).

## Recommendation & first step
Build **Option B**, scaffolded from the existing primitives, as part of **#28** (click-to-stop + processing state). First verifiable slice: the `app-ai-orb` component driven by a dev toggle → Playwright on `:1420` cycles all 4 states + synthetic `level` 0→1; assert distinct motion, `ng build` under budget, reduced-motion collapses to static. The animation *quality* wants a 30-s real-Mac glance (not a headless gate).

## Open questions
- ClickUp's exact recording-bar motion (waveform vs orb) — unverified (pages 403'd); the interaction model (toggle + Stop) IS verified.
- `@property` through Angular's CSS pipeline in THIS build — low-risk but unverified; the `transform: rotate()` variant avoids it.
- ANSWER motion = steady glow vs a one-shot flourish — confirm with the user (a CSS entry-animation gives a flourish with no timer).
- Audio-reactive *feel* of the 100 ms `level()` poll smoothed by a 90 ms CSS transition — wants a real mic.

## Sources
Prior-art: ClickUp Help/marketing, Croma/justainews (ChatGPT orb), iDownloadBlog (Siri glow), gemini-cli #21109/#22436 (state spec), SmoothUI Siri Orb source, Vercel AI SDK Shimmer, Raycast changelog, Fast Company (Notion). Feasibility: web.dev @property-baseline, animationpatterns.art conic spinner, Pyxofy @property+conic, CSS-Tricks conic almanac. Repo: `record.component.ts:129/608/714/778/842`, `recorder.store.ts:52`, `assistant.store.ts:97/106/37`, `assistant-actions.component.ts:192`, `styles.css:55/478`, `angular.json:51`.
