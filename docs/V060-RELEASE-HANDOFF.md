<!-- The 0.6.0 finished-product release handoff. Read this to ship. Pairs with docs/PLAN-finished-product.md + the skills/release-murmur runbook. -->
# v0.6.0 — release handoff (the finished product)

**Status:** every stub is now a real component; trunk `murmur` @ **0.6.0**, all headless gates green
(`cargo test --lib` **341/0** · `clippy --all-targets -D warnings` clean · `ng lint` + `ng build` clean).
Both local models were **smoke-validated on this Mac**. The only things left are **your build-feature
choice + the signed release** (sign/notarize/publish need your Mac + auth).

---

## What shipped (PRs #44–#57, every stub → real)

| Area | Before (0.5.0) | Now (0.6.0) |
|---|---|---|
| **Brain** | stub | **Claude (default, `CloudReasoner`)** + **Bielik-11B** local option (`MistralReasoner`); `brain_backend` Cloud/Local/Off; orchestrate.rs (brain decides context via gated tools, deterministic floor); model registry (Bielik/Qwen3-14B/Qwen2.5-3B/custom) + RAM-gating |
| **Embedder** | `StubEmbedder` (hash) | **real `CandleBertEmbedder`** (multilingual-e5-small, 384-dim, candle — no 2nd ORT) |
| **Name redaction** | `NoopNameRedactor` (names egress) | **real `DebertaNameRedactor`** (candle DeBERTa NER, PERSON masking before egress, mask-only) |
| **Voice** | matcher unwired | in-meeting **assistant dispatch** (research/recall over the gated vault → live result), `realtime_reactions` opt-in |
| **UX** | — | settings **Brain/AI** section (picker + backend selector + toggle) + **live assistant-actions card** |
| **Calendar** | — | **EventKit Swift sidecar** + `list_calendar_events` + `CalendarContext` (4th bundled helper) |
| **Flywheel** | lock-unsafe, test-only | **lock-safe** (meeting_id + gated reader + purge-on-seal) + first capture |

Every phase went through build → adversarial-verify → lock-security-review → PR-merge.

### Final holistic verification (whole 0.6.0 surface, 2026-06-28)
A 3-axis audit of the cumulative change came back:
- **Lock-security: PASS** — every new content read gated on the LIVE unlock set (brain orchestrate, voice Research/Recall, embedder index+query, dossier/commitments); every new cloud egress on the ONE `make_provider` firewall + fail-closed consent gate; vec_chunks + correction_log purged in the same atomic tx on every seal/lock/relock/delete + startup reconcile; the NER strictly *reduces* egress (mask-only, a miss ≤ Noop); no PII in logs; no startup hard-crash. *"No leak, no loss found."*
- **Constraints: UPHELD across all 7** — zero new cloud-egress destination (the only new `reqwest` calls are inbound-only HF model downloads); **single onnxruntime** (sherpa — the embedder + NER use candle, no 2nd ORT); SQLite-canonical (derived state only); firewall **stronger** (real NER option); `com.meetnotes.app` immutable; deps as authorized (candle stack only, behind opt-in features; gline-rs/fastembed/ort rejected); additive guarded migrations.
- **Prod-readiness: READY-WITH-CAVEATS** — the brain stack is coherent (one `LocalReasoner` seam, 3 impls by `brain_backend`; `orchestrate.rs`/`voice_action.rs` reasoner-agnostic; one gated `tools.rs` registry shared by MCP+brain+voice; candle shared across the 3 features). The caveats are **not bugs**: the Mac-gated runtime evals (below) + two dormant user-reachability surfaces (the **calendar FE picker**, and **a brain_backend/model change needs an app restart** to re-resolve `active_reasoner`). Nothing aborts, leaks, or loses content. *"Ship-able as a release."*

### On-Mac smoke validation (not just "compiles")
- **Bielik-11B brain:** `reason()` loads on Metal **without full Xcode** (via `MISTRALRS_METAL_PRECOMPILE=0`) and produced correct Polish. `structured()` was fixed (dropped the context-overflowing `Constraint::JsonSchema` for the prompt+`parse_first_json` path). **Debug inference is ~min/token — the local brain needs a RELEASE build to be usable.**
- **e5 embedder:** loads + embeds; **dim=384, L2-norm=1.000, and a Polish query ranked the Polish passage (cos 0.886) above an unrelated English one (cos 0.722) — multilingual semantics confirmed.**

---

## ⚠️ THE BUILD DECISION (determines whether the local models work in the shipped app)

The three local-model runtimes are **opt-in cargo features** (`local-brain`, `local-embed`, `local-ner`).
**This is your call at build time** — it's the difference between a small Claude-only app and the full
local-capable product:

| Option | Build command | What the user gets | Cost |
|---|---|---|---|
| **A — Default (smallest)** | `npx tauri build --target universal-apple-darwin --bundles app` | Claude brain · FTS search · voice · calendar · UX. The "Local model" options in the UI are **inert** (fall back to Stub). | Smallest binary; no candle/mistralrs; no Xcode/metal step. |
| **B — Embedder + NER** | add `--features local-embed,local-ner` (no metal hatch needed) | + real semantic search (after bake-off) + on-device name-redaction. Local **brain** still inert. | Moderate size (+candle). Builds clean without full Xcode. |
| **C — Full (RECOMMENDED for the product you built)** | `MISTRALRS_METAL_PRECOMPILE=0 npx tauri build … --features local-brain,local-embed,local-ner` | The **full product**: Bielik local brain + e5 semantic + DeBERTa NER all work as in-app options. | Largest binary (+mistralrs/candle ~hundreds of MB); needs the `MISTRALRS_METAL_PRECOMPILE=0` env (you only have CLT, not full Xcode — the hatch defers metal-shader compile to first run, which works). |

> How to pass cargo features to `tauri build`: either `npx tauri build … -- --features <list>` (cargo args after `--`) or set them in `tauri.conf.json` → `build.features`. Confirm the flag reaches cargo (`--features` in the build log).

**My recommendation: Option C** — you built this for the local options (privacy/offline/"nasz finetuning"); they're validated and one flag away. Ship the full product. If the binary size is a concern, **Option B** is the pragmatic middle (local semantic + NER, Claude as the brain — which is the default anyway). **Option A** if you want a minimal Claude-only first 0.6.0 and defer local models to 0.7.0.

---

## The release steps (your Mac — sign/notarize/publish need your auth)

Everything up to the signed build is done (version bumped, gates green). Run the **`release-murmur`** runbook with your chosen feature set:

```bash
# clean tree on murmur @ 0.6.0:
rustup target add aarch64-apple-darwin x86_64-apple-darwin
pkill -x Murmur ; pkill -f 'tauri dev' || true        # free the cargo target lock
# build (pick A/B/C above — example = C, the full product):
MISTRALRS_METAL_PRECOMPILE=0 npx tauri build --target universal-apple-darwin --bundles app -- --features local-brain,local-embed,local-ner
# sign INSIDE-OUT by identity HASH — NOW FOUR bundled helpers (the 3 audio + meetnotes-calendar):
bash scripts/macos-sign-notarize.sh        # MUST sign meetnotes-calendar too (entitlements.plist carries the calendars key) or notarization = Invalid
# notarize + staple + verify + publish:
xcrun notarytool submit <dmg> --keychain-profile murmur --wait
xcrun stapler staple <dmg> ; spctl -a -vvv -t open --context context:primary-signature <dmg>   # expect "Notarized Developer ID"
gh release create v0.6.0 -R JakubGawr/murmur <dmg>
```
**Critical (release rules):** the new **`meetnotes-calendar`** is a **4th bundled helper** — `scripts/macos-sign-notarize.sh` must sign it inside-out like the three audio helpers, or notarization comes back `Invalid`. Verify the script picks it up (it signs everything in `Contents/Resources/`).

---

## What still needs YOU on a real Mac (the honest residual)
1. **Run a feature build + launch it** (Option B/C) so the local models actually run signed (Metal at runtime, the model downloads in-app via the picker). The local-brain first-run compiles metal shaders (the `…PRECOMPILE=0` defer) — a one-time pause.
2. **The RAG bake-off** (`docs/RAG-BAKEOFF.md`): with the real embedder, does semantic beat FTS on your vault? → decide whether to flip `semantic_search_enabled` on by default.
3. **Polish NER recall** of `Davlan/mdeberta-v3-base-ner-hrl` (the chosen redaction model) on your real names — swap the model URL if recall is weak (the decode is id2label-driven, zero code change).
4. **Real-mic voice precision** (the "Claudku" wake) + the Touch-ID/lock-at-rest behaviours — signed build only.
5. **Bielik `structured()` runtime re-check** in a release build (debug was ~min/token).

## Deferred (not in 0.6.0 — clean follow-ups)
- The **calendar FE picker** (the IPC methods + types are in; the UI is a small follow-up).
- **Slack/Jira** external sources (the `source_type` seam is ready; deliberately deferred — research-flagged connector treadmill).
- The **LoRA fine-tune** of the local brain on the now-lock-safe flywheel (premature until hundreds of clean correction pairs accrue).

The finished product is on `murmur` @ 0.6.0, green and reviewed. Pick a build option and ship. 🚀
