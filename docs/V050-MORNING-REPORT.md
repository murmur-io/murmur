<!-- Overnight autonomous build + full verification of the brain2 v0.5.0 work. Read this first in the morning. -->
# v0.5.0 — morning report (overnight autonomous build + verification)

**TL;DR:** Everything that could be built and *verified* headless is **built, adversarially-verified, lock-security-reviewed, and merged to `murmur` as v0.5.0** — 12 PRs (#28–#39). A full 4-axis verification of the whole body of work came back **architecture SOUND · lock-security PASS · constraints UPHELD · prod-readiness no-critical** (3 minor cleanups already applied). The only things left for you: **(1) the value bake-off on your Mac, (2) cutting + signing the release** — both genuinely need your machine/auth, exactly as agreed.

All gates green on trunk: `cargo test --lib` **273/0** · `clippy --all-targets -D warnings` clean · `ng lint` + `ng build` clean.

---

## What shipped (12 PRs, all QueaT-authored, all 2× adversarial-gated)

### ✅ Prod-ready NOW — real value today, live in the app
| PR | Feature |
|---|---|
| #28 | **FTS5/BM25 search** replacing the broken word-order-sensitive `LIKE` — UI + MCP + Ask inherit ranked search. **+ fixed a real bug:** Ask/Brief were silently omitting session-unlocked folders. + cited Ask. |
| #34 | **Open-commitments rollup** — "what do I owe / what's still open across my meetings" (gated aggregation) + an MCP `get_open_commitments` tool + **fixed `add_reminder`'s dropped due-date** (now flows to Apple Reminders, injection-safe). |
| #35 | **Entity Dossier** — "state of [[X]]" (Overview · Timeline · Open commitments · Last said), cited synthesis via the cloud provider + an **egress-free** MCP `get_entity_dossier` tool. |

The local **MCP server now exposes 6 gated tools** (was 3): `search_meetings`, `get_meeting`, `list_recent_meetings`, `search_semantic`, `get_open_commitments`, `get_entity_dossier` — your encrypted memory, queryable by Claude Desktop/Code, zero egress.

### 🟡 Built + lock-safe, but DORMANT behind default-off flags (flip after the real model)
| PR | Feature | Flag |
|---|---|---|
| #29 | **Lock-safe sqlite-vec vector layer** (proven to link under our bundled-SQLCipher on macOS; #169 doesn't bite) — gated KNN, purge-on-every-seal-path | — (infra) |
| #30 | Semantic/hybrid wired into Ask + the MCP `search_semantic` tool, graceful FTS5 fallback | `semantic_search_enabled` (off) |
| #31 | **GraphRAG-lite** — entity-anchored expansion (FTS ∪ vector ∪ entity-graph, RRF) — our one uncontested differentiator | (same flag) |
| #36 | **Retrieval-augmented note generation** — new notes grounded in related prior notes ("last time you decided X") | `augment_notes_with_context` (off) |

These ship **inert** (a `StubEmbedder` produces meaningless vectors; the flags have no FE setter). They become *useful* the moment a real embedder lands — see "your morning steps."

### 🔌 Seams ready — prod-inert, the real models are a one-line swap
| PR | Seam | Real impl (later) |
|---|---|---|
| #33 | **NameRedactor** in the redaction firewall (no-op default → egress byte-identical) | GLiNER (closes the name-redaction gap) |
| #33 | **LocalReasoner** trait + StubReasoner + robust JSON extractor | mistral.rs (grammar-constrained) |
| #33 | **`correction_log`** flywheel table (LoRA dataset substrate) | populated once the planner runs |
| #29 | **Embedder** trait + `active_embedder()` swap site | BGE-M3 |
| #37 | **Voice wake-matcher + intent parser** (`detect_wake` anchored on the distinctive "Claudku" vocative; `parse_voice_intent`) — **16/16 precision negatives silent, 4/4 vocative positives fire** | wired to the live mic loop + Whisper bias |

> The wake-matcher's first pass had real precision false-fires ("ok loud and clear", Polish "kłódka") — the adversarial gate caught them; I re-anchored on the vocative and re-verified before merging. The discipline held all night: nothing green-washed.

### 🧰 Release-prep
- #38 **bump → 0.5.0** (package.json + tauri.conf.json + Cargo.toml + Cargo.lock).
- #39 **post-audit polish** — dropped an MCP-loop `unwrap()` (would kill the server thread), corrected a stale TODO + the dim-swap comment.
- Docs: `PLAN-brain2-rag-voice.md`, `RAG-BAKEOFF.md`, `2026-06-28-local-model-voice-decision.md`, this report.

---

## Full verification (the 4-axis audit you asked for)

- **Architecture — SOUND (minor documented debts).** New pieces fit the existing seams (provider seam, SQLite-canonical, egress-free MCP); the Embedder/NameRedactor/LocalReasoner traits are genuine swap points; "three callers (UI/MCP/voice) over one gated reader set" holds; dependency graph acyclic; Phases 3b/6 are pre-wired (`source_type` column, the flywheel table, the VoiceIntent schema).
- **Lock-security — PASS.** Every new reader (FTS, vector KNN, hybrid, GraphRAG, commitments, dossier, related-context, all 6 MCP tools) routes through `visibility_clause`/`meeting_is_unlocked` on the **live** unlock set; derived chunks/vectors are purged **in the same transaction** as the plaintext blanking on *every* seal/lock/relock/reconcile/move/delete path; both flags + the NoopNameRedactor leave prod egress byte-identical; all cloud egress funnels through the fail-closed consent gate + RedactingProvider on visible content only.
- **Constraints — UPHELD (all 7).** Zero new network egress (grep-confirmed across every new file); on-device/stubbed models; Obsidian-native `.md` unchanged; the vector index lives *inside* the canonical SQLCipher DB (no second store); provider seam + firewall intact; `com.meetnotes.app` immutable; dev hatches still debug-gated; only `sqlite-vec` added (approved); additive guarded migrations only.
- **Prod-readiness — no critical issues.** 3 minor findings, all fixed in #39. Remaining low-pri debts below.

### Architectural debts to address *before flipping semantic on* (not blockers for 0.5.0)
1. **Embed-on-unlock not wired** — on lock, vectors are purged; on unlock they are NOT re-indexed (FTS self-heals, vectors don't). Graceful in hybrid (the FTS leg still surfaces the meeting) and **not leaky** — but the two retrieval legs have inconsistent recovery. Close this in Phase 2c.
2. **`EMBED_DIM` swap = a vec0 schema migration + full re-index** (now documented in `embed.rs`), not a code one-liner. Fails loud (dimension error), never silent.
3. **N+1 in `list_open_commitments`** (per-meeting note fetch) — fine at current scale, worth a JOIN later.

---

## What is NOT done — and the honest why (all Mac/OAuth-gated)

I deliberately did **not** build these blindly overnight, because their *value* is unverifiable headless and building them would be unvalidated scaffolding (or a real link-risk):

- **Phase 2c — the real embedder + the Polish bake-off.** This is the **value gate**: does the vector layer actually beat the (already-live) FTS5 at your corpus size, and does BGE-M3 read your spoken/code-switched Polish? Needs a real model on a real Mac. The seam is ready.
- **Phase 3b — GLiNER name-redactor + mistral.rs reasoner.** Both pull a heavy second ML runtime (`ort`/`candle`) with a real risk of a link conflict against sherpa-onnx's bundled ONNX runtime, and the models' quality (Polish NER, planning) can't be measured headless. The seams (#33) are ready for a clean swap; do this interactively with a build-proof, like we did for sqlite-vec.
- **Phase 6 — external sources.** Calendar (EventKit) needs a Swift sidecar + real-Mac TCC; Slack/Jira need OAuth and are the research-flagged "integration treadmill / privacy-contradiction" trap. The `source_type` ingest seam is in place so they're additive when you want them. **Recommendation stands: Calendar first (local, zero-OAuth), Slack/Jira only after retrieval is proven valuable.**
- **Phase 7 — live-mic wiring** of the (verified) wake-matcher into `transcribe/live.rs`, the Whisper `set_initial_prompt` bias, and real-mic precision tuning. Real audio only.

**The honest bar:** headless I verified the *logic*, the *lock-safety*, and the *no-regression dormancy*. The *value* of the model/voice/external features needs your signed build + real data. I flagged each one rather than pretend a green unit test proves it.

---

## Your morning steps

### 1. (cheap, decisive) Run the Stage-1 bake-off — `docs/RAG-BAKEOFF.md`
Score the **live FTS5 Ask** on your real vault with the PL/EN question set. This tells us whether to invest in the real embedder at all, or whether FTS5 (already shipped) is enough. **Caveat:** `npm run dev` uses an isolated dev vault (`MeetNotes-dev`) — to test on your *real* meetings you need the signed 0.5.0 build (step 2). So: cut the release, install it, then run Stage 1 on your real data.

### 2. Cut + sign + publish the 0.5.0 release (your part — needs your keychain/Apple auth)
Everything up to the signed build is prepped (version bumped, all gates green). Run the **`release-murmur`** runbook — the parts I cannot do from the agent shell (sign/notarize/keychain) are exactly your interactive steps:
```bash
# from a clean tree on murmur @ 0.5.0:
rustup target add aarch64-apple-darwin x86_64-apple-darwin
# stop any dev server first (holds the cargo target lock)
npx tauri build --target universal-apple-darwin --bundles app
# sign INSIDE-OUT by identity HASH (the cert CN has Polish 'ń' → name matching fails):
bash scripts/macos-sign-notarize.sh        # signs nested audio helpers FIRST, then the .app (no --deep), then the DMG
# → notarize: xcrun notarytool submit <dmg> --keychain-profile murmur --wait
# → xcrun stapler staple <dmg> ; spctl -a -vvv -t open --context context:primary-signature <dmg>  (expect "Notarized Developer ID")
# → gh release create v0.5.0 -R JakubGawr/murmur <dmg>  (+ upload)
```
(I left the version bump in; if you'd rather I drive more of this, say so and I'll run `release-murmur` up to the signing handoff.)

### 3. (later, after a real embedder) flip the dormant flags + re-run the bake-off Stage 2
Once 2c lands a real BGE-M3 embedder, set `semantic_search_enabled` (and try `augment_notes_with_context`), re-run the same questions FTS-only vs hybrid, and we'll know if the vector + graph stack earns its bundle cost.

---

## Status of the night
- **12 PRs merged**, 0 reverted, every one through adversarial-verify + (where it touched the lock model) lock-security-review.
- **2 real bugs caught + fixed before merge** by the gates (the Ask unlock blind-spot in #28; the voice precision false-fires in #37) — plus 2 latent lock-leaks in the vector layer (seal_moved_note / delete_meeting orphan) caught + fixed in #29.
- I stopped exactly where agreed: **before the release build, sign, and notarization**, which are yours. Goal complete.

Sleep well — it's all on `murmur`, green, and reviewed. 🌙
