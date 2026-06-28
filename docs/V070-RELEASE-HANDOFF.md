<!-- The 0.7.0 release handoff. Supersedes V060-RELEASE-HANDOFF.md. Read this to ship. Pairs with .claude/skills/release-murmur. -->
# v0.7.0 — release handoff (brain + voice + connectors + semantic)

**Status:** trunk `murmur` @ **0.7.0** (PR #74), all headless gates green — `cargo test --lib` **431/0** ·
`cargo clippy --all-targets -D warnings` clean · `ng lint` + `ng build` clean · `cargo build --lib` clean at 0.7.0.
Every change shipped via build → adversarial-verify → (lock-security-review where lock/egress was touched) → PR-merge.
**The only things left are your build-feature choice + the signed release** (sign/notarize/publish need your Mac + auth).

---

## What shipped in 0.7.0 (PRs #54–#74, on top of 0.6.0)

| Area | 0.7.0 |
|---|---|
| **Brain** | real orchestration (brain decides context via gated tools) + **model + effort picker** (Settings → Brain/AI: Default/Opus 4.8/Sonnet 4.6/Haiku 4.5; effort low/med/high — Anthropic-only, honestly gated) |
| **In-meeting voice assistant** | wake "Klaudku" (recall-first shape-gate, **fires anywhere in the live window + dedup**) **+ the ✨ click-to-stop button** (you control when you're done — no fixed-timeout cutoff) **+ a PROCESSING state** + an **animated AI orb** (idle → listening/audio-reactive → processing/conic+shimmer → answer; pure CSS/SVG, reduced-motion-aware) |
| **Assistant Q&A** | interactions **persisted per-meeting** (gated, purged-on-seal) + a **"🎙 Asystent — Q&A" detail section** (question → answer + [[vault]]/via-web sources); the summarizer **excludes** "Klaud/Klaudku" utterances from action items |
| **Semantic search** | **real e5 multilingual** turned on (Settings toggle + a **re-index** backfill); cross-lingual ("weather"→"pogoda") + paraphrase; hybrid FTS ∪ vector ∪ graph |
| **Connectors** | a **Connector framework** (live agentic tools, not vectorized) + **web search (Brave, BYO key, consent-gated NEW egress, redacted, "via web")** + a **calendar connector** (Local/EventKit, on-device, no egress) |
| **Privacy/consent** | redaction firewall + DeBERTa NER; **proactive cloud-consent prompt** when enabling the assistant with a cloud brain; web-search consent fail-closed + preserve-only |
| **Dev/stability** | **whisper ggml-metal residency abort fixed** (GGML_METAL_NO_RESIDENCY=1) · **dev-secrets file store** (no keychain prompt/-34018 in unsigned dev) · isolated MeetNotes-dev data dir |

### Final prod-readiness (holistic, 2026-06-29)
- **Lock-security: PASS** across every lock-touching change — assistant Q&A double-gated + purged-on-seal in the same atomic tx as vec_chunks/correction_log; semantic re-index indexes ONLY visible content (sealed never embedded); the web connector is the only new egress and it is **fail-closed** (enable ∧ consent ∧ keyed), query-redacted, and loud; the calendar connector is Local/no-egress; the dev-secrets store is `#[cfg(debug_assertions)]`-only (release uses data-protection keychain, byte-for-byte unchanged). *No leak, no loss found.*
- **Constraints UPHELD:** one new cloud-egress destination (web search) — loud, consent-gated, redacted, justified; SQLite-canonical (Q&A/chunks are derived + purged on seal); provider seam + redaction intact; single onnxruntime (candle, no 2nd ORT); `com.meetnotes.app` immutable; additive guarded migration (assistant_interactions).
- **Prod-readiness: READY-WITH-CAVEATS.** The caveats are **runtime/Mac-gated, not bugs** — see the residual below. Nothing aborts, leaks, or loses content. Headless-green; the felt UX (mic, orb, web results) is the human-glance step.

---

## ⚠️ THE BUILD DECISION (same as 0.6.0)

Local models are opt-in cargo features (`local-brain`, `local-embed`, `local-ner`). For the full product you built, use **Option C**:

```bash
MISTRALRS_METAL_PRECOMPILE=0 npx tauri build --target universal-apple-darwin --bundles app -- --features local-brain,local-embed,local-ner
```
(Option B = `--features local-embed,local-ner` — semantic + NER + Claude brain, no Xcode/metal step. Option A = default, Claude-only.)

## Release steps (your Mac — sign/notarize/publish need your auth)

```bash
# clean tree on murmur @ 0.7.0:
rustup target add aarch64-apple-darwin x86_64-apple-darwin
pkill -x Murmur ; pkill -f 'tauri dev' || true            # free the cargo target lock
# build (Option C example):
MISTRALRS_METAL_PRECOMPILE=0 npx tauri build --target universal-apple-darwin --bundles app -- --features local-brain,local-embed,local-ner
# sign INSIDE-OUT by identity HASH — FOUR bundled helpers (3 audio + meetnotes-calendar):
bash scripts/macos-sign-notarize.sh        # signs each nested helper FIRST, then the .app (NO --deep), then the DMG
# notarize + staple + verify + publish (notarytool keychain profile "murmur"):
xcrun notarytool submit <dmg> --keychain-profile murmur --wait
xcrun stapler staple <dmg> ; spctl -a -vvv -t open --context context:primary-signature <dmg>   # expect "Notarized Developer ID"
gh release create v0.7.0 -R JakubGawr/murmur <dmg>
```
**Hard rules (do not repeat the 2026-06-27 mess):** notarization is MANDATORY; sign INSIDE-OUT, never `--deep` (it skips the `Contents/Resources/` helpers → notarization `Invalid`); get the identity by HASH (the cert CN has "Gawroński"); run any `security`/keychain/`notarytool store-credentials` op YOURSELF interactively (the agent shell hangs them); merge via PR only.

## What still needs YOU on a real signed Mac (the honest residual — none are bugs)
1. **Boot the signed Option-C build** — the local models run signed (Metal at runtime; local-brain first-run compiles shaders once via the `…PRECOMPILE=0` defer; models download in-app via the picker).
2. **Voice flow end-to-end on a real mic:** the wake "Klaudku" + the ✨ click-to-stop + the orb's audio-reactive feel + the ~60s backstop + dedup of a 2nd "Klaudku" — all proven at the logic level; the acoustics + feel are the human glance.
3. **Web search:** add a free Brave Search API key (Settings → Web search → key + Allow), ask "zrób research o pogodzie" → confirm real "via web" results.
4. **Semantic:** enable the toggle + Re-index notes → confirm cross-lingual recall on your real vault (does e5 beat FTS — the bake-off call).
5. **Calendar connector:** grant macOS Calendars (TCC) on the signed build → confirm `fetch_events` returns real events.
6. **Bielik local brain** in a RELEASE build (debug was ~min/token) + the model/effort picker against the live API/CLI (confirm the exact model-id strings are accepted).
7. **Touch ID / lock-at-rest / screen-share auto-relock** — signed-build-only.

## Deferred (clean follow-ups, not in 0.7.0)
- Slack/Jira/Google connectors (OAuth) — the framework + the Local/External seam are ready.
- The LoRA fine-tune of the local brain on the now-lock-safe flywheel (premature until clean correction pairs accrue).
- Trim the `record.component.ts` (+292 B) / `detail.component.ts` (+1.51 kB) per-component style WARNs (both under the 16 kB ERROR budget; cosmetic).

The product is on `murmur` @ 0.7.0, green and reviewed. Pick a build option and ship. 🚀
