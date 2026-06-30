# Murmur — Architecture Audit: Vector-DB · Performance · Security + Business-Assumptions Verification

**Date:** 2026-06-29 · **Version audited:** 0.5.0 (`murmur` branch) · **Method:** 5 parallel code-grounded subagent analyses (vector / perf-backend / perf-frontend / lock-security / egress-security) → adversarial verification of the material claims by re-opening cited code → business-assumptions synthesis.

**Baseline gate at audit time:** `cargo test --lib` = **452 passed, 0 failed, 2 ignored** (the 2 ignored are the candle e5 / DeBERTa smoke tests that need the real model on disk). Strong lock/visibility coverage in the passing set (`search_semantic_is_visibility_gated_when_enabled`, `index_gate_skips_sealed_meeting_until_unlocked`, `hybrid_corpus_respects_visibility_gate`, `cloud_no_consent_maps_to_needs_consent_failclosed`, `list_facts_visible_excludes_sealed_meeting`).

> **Trust-code-not-docs corrections surfaced by this audit**
> 1. The vector layer is **real**, not a stub: `sqlite-vec` `vec0` KNN + candle `multilingual-e5-small` + RRF hybrid fusion. The earlier "ships StubEmbedder / Noop redaction" memory is **stale** — the real impls are now *always-compiled* and activate on **model presence**, but the models are **downloaded on first use, not bundled**, so a fresh install still runs on the stub/Noop floor.
> 2. `mcp_require_token` default is **`true`** (`config.rs:219`), i.e. the MCP server requires a token by default — *not* off as an earlier scouting note assumed.
> 3. There is **no `biometric.rs`** file (CLAUDE.md / rules cite `biometric.rs:7/29/46`). Touch-ID gating is the **keychain ACL** `kSecAccessControlUserPresence` in `secrets/keychain.rs:405-407` — a robust design, but the rule-file citations are stale.

---

## TL;DR verdict

The app **mostly meets its business assumptions**, on genuinely strong privacy/lock/crypto foundations and a real (no-longer-stub) on-device vector layer. The gaps are real but fixable, clustered in three places:

- **Privacy / redaction firewall has two coverage holes** on the cloud-egress path (assumption A): the meeting **title + the full `vault_titles` taxonomy** egress un-redacted on note generation, and **PERSON names** egress un-redacted at the shipped Noop floor (regex still covers email/card/phone). Both are gated behind explicit, default-OFF consent.
- **One new metadata/timing leak** in the bitemporal `facts` layer (assumption F) — a sealed meeting's date + "an attribute changed" can surface through a *visible* superseded fact in the dossier (which egresses to cloud + MCP). Dormant in the default stub build.
- **Performance defects that bite once the real models download and the vault grows**: the embedder and NER redactor are **rebuilt from disk on every call** (only the brain is cached in `AppState`), live captions run **always-on large-v3** re-transcribing a 14 s window every 3 s, and the auto-index can **poison the vector index with stub vectors**.

Plus one **FE runtime crash** (NG0600 on the prefilled pre-meeting prep card).

---

## 1. Vector-DB architecture

**Bottom line:** the security posture of the vector layer is solid — `note_chunks` + `vec_chunks` are purged in the *same transaction* that blanks plaintext on every seal path, the KNN read carries a defense-in-depth `visibility_clause`, width contracts fail loud, no raw vector crosses IPC. The real weaknesses are **retrieval quality + performance**, and they are all gated OFF by default (`semantic_search_enabled: false`).

| Sev | Finding | Evidence | Verdict |
|---|---|---|---|
| HIGH | **e5 model reloaded on every semantic op — no process cache.** `active_embedder()` builds a fresh `CandleBertEmbedder` per call; its lazy weight cache is per-instance. `AppState` caches `reasoner` but has **no embedder field**. | `embed.rs:146-160`, `embed/candle_bert.rs:46-48,91-140`, `state.rs:116` (reasoner only), call sites `commands.rs:1447,2588`, `tools.rs:97`, `pipeline.rs:568` | **CONFIRMED** (cross-confirmed by perf agent + direct read of `state.rs`) |
| MED | **Auto-index persists STUB vectors with no model-presence guard → mixed, distance-incomparable index.** The summarize path indexes whenever the flag is on; the manual `reindex` *refuses* the stub. No column records which embedder produced a row. | `pipeline.rs:562-574` (no `embed_model_present()` check) vs `commands.rs:2502-2503,2613-2620` (reindex returns `model_missing`); `vec_chunks` schema has no model-id col `db.rs:369-388` | **CONFIRMED** |
| MED | **"Semantic" search never embeds transcripts** — only the AI **note markdown** is chunked, while the FTS leg indexes titles + segments + notes. A transcript-only meeting has zero semantic coverage. | `db.rs:847` (chunks `note.markdown` only), `db.rs:2358-2366` (FTS indexes segments too), `source_type` hardcoded `'voice'` `db.rs:869` | **CONFIRMED (static)** |
| LOW | **`content_hash` is write-only** — computed + stored but never read back; the advertised incremental-dedup re-index is unimplemented (full purge-then-reembed every run). | `db.rs:876,883` (write), `db.rs:863` (unconditional purge); no read site | **CONFIRMED** |
| LOW | **Brute-force vec0 KNN under the global connection `Mutex`, on the async runtime; top-k capped before per-meeting dedup.** Fine at personal scale (10⁴–10⁵ vectors); linear scan + page-decrypt is the cost driver as it grows. | `db.rs:992,996-1015,1019-1039`, `commands.rs:1449-1460` (no `spawn_blocking`) | **CONFIRMED** |
| INFO | Stub fallback ranking (flag ON, model absent) is non-semantic and the FE toggle is not blocked on model presence (download/reindex *are* gated). | `embed.rs:146-161`, `settings.component.ts:507` (toggle), `:512-542,591-596` (download/reindex gated) | **CONFIRMED** |

**Strengths (cited):** atomic purge-on-seal across all three paths (`db.rs:942-958,900-919,685-689,2024-2040`); defense-in-depth `EXISTS` visibility gate on the read (`db.rs:1009-1013`); width contracts hard-fail (`candle_bert.rs:125-130,205-212`); vec0 module registered before `Connection::open` so `PRAGMA key` stays first (`db.rs:146-150`); reindex refuses the stub rather than poisoning the index.

---

## 2. Performance — backend / pipeline / ML

**Bottom line:** the big architectural calls are right — all heavy ML is lazy (never on the crash-safe startup path) and the batch whisper run is correctly `spawn_blocking`-offloaded. The defects are **model-cache thrash** and **always-on live captions**, plus a worst-case memory ceiling.

| Sev | Finding | Evidence | Verdict |
|---|---|---|---|
| HIGH | **Embedder reloaded from disk every query** (see §1). | `embed.rs:146-160`, `state.rs` (no embedder field) | **CONFIRMED** |
| MED | **NER redactor (~1 GB mDeBERTa) reloaded on every cloud-summarize.** `make_provider` wraps with `active_name_redactor()`, built fresh each call. | `summarize/mod.rs:124-126`, `redact.rs:76-92`, `ner_deberta.rs:62,68-73`, call sites `commands.rs:742,985,1573` | **CONFIRMED** (same anti-pattern; `state.rs` has no redactor field) |
| MED | **Live captions always-on, re-transcribe overlapping audio with large-v3.** No config gate; every 3 s decode a 14 s tail (~4.6× redundant compute, continuous GPU pin). | `commands.rs:301-311` (no flag gate), `transcribe/live.rs:23,25,107,134,158` | **CONFIRMED (static)** |
| MED | **Models never evicted → ~12–13 GB worst-case RSS** (user-selected 14B brain cached forever + large-v3 + transient e5/NER/sherpa) risks swap on a 16 GB Mac. Default (stub brain) stays ~3 GB. | `reason/mistral.rs:54,87-106`, `state.rs:116`, `pipeline.rs:349`, `config.rs:213` | **CONFIRMED (estimate; RSS not measured)** |
| MED | **Voice trigger loads full large-v3 as a wake-word detector** (~3 GB resident + continuous decode while idle). Mutually exclusive with live recording. | `commands.rs:4403-4410`, `audio/listener.rs:60` | **CONFIRMED (static)** |
| LOW-MED | **Single `Mutex<Connection>` serializes all DB I/O** incl. brute-force KNN + the N+1 loops below. (Good: guard is `!Send`, structurally can't be held across `await`.) | `db.rs:130-132,524-528,983,997-1000` | **CONFIRMED** |
| LOW | **N+1 loops** in digest (`get_note_if_visible` ×≤300) and topic-threads (`get_timeline_data` ×≤500). | `commands.rs:1546,1553,1609,1610` | **CONFIRMED** |
| LOW | **Long-meeting RAM spike** — dual raw-audio `Vec`s buffer in RAM at native rate, capped at 4 h (~5.6 GB mic+system) before the 16 kHz resample + 3 GB batch model load at Stop. | `audio/recorder.rs:18,32-37,78,82` | **CONFIRMED** |

**Strengths (cited):** batch transcription `spawn_blocking`-offloaded (`pipeline.rs:346-410`); all ML lazy, none on startup (`state.rs:176-179`, `reason/mistral.rs:24,62`, `candle_bert.rs:58-75`); sync↔async brain bridge on a scoped OS thread (`reason/mistral.rs:149-159`); embedding computed *before* the DB lock (`db.rs:857,861`); hot relational paths indexed (`db.rs:256-315`); recorder buffer hard-capped (`recorder.rs:32-37`).

---

## 3. Performance + correctness — Angular zoneless frontend

**Bottom line:** the zoneless discipline is exceptionally well kept — every component standalone + `OnPush`, signals throughout, the banned-pattern greps come back empty (no `*ngIf/*ngFor/*ngSwitch`, no `track $index`, no `async` pipe, no `.subscribe()`-into-field, no getters-in-template for derived values). One real correctness bug + a cluster of recompute-waste hotspots.

| Sev | Finding | Evidence | Verdict |
|---|---|---|---|
| HIGH | **NG0600: signal written in an un-flagged `effect()`** (the exact T1 trap). The prep card crashes on its prefilled path (calendar event / detected app feeds a non-empty `initialSubject`). The 3 sibling effects all carry `allowSignalWrites`. | `pre-meeting-brief.component.ts:432-437` (read `subject()` l.434, write `subject.set()` l.435), parent `record.component.ts:120` | **CONFIRMED** — recommend a RED/GREEN Playwright repro before the 1-line fix |
| MED | **Timeline recomputes static colors/labels every playback tick** (~4 Hz). `dotColor/blockColor/topicColor/edgeColor/fmt` are pure helpers called as methods in the template; only `isActive/playheadPct` truly depend on `currentTime`. | `meeting-timeline.component.ts:1459-1478`, `detail.component.ts:557,610,2545` | **CONFIRMED** |
| MED | **Library renders per-row methods (incl. O(n) `snippetParts` string scan) every CD pass / keystroke.** | `library.component.ts:413,475,477,1488,1749` | **CONFIRMED** |
| LOW | **Library list unvirtualized** — every row materializes + re-runs per-row methods on each CD. (Fix needs CDK dep approval or a manual windowing `computed`.) | `library.component.ts:365` | **CONFIRMED** |
| LOW | **Raw component `setInterval` poll** (12 s meeting-app detect) vs the sanctioned rxjs `interval`→`toSignal`. `DestroyRef`-torn-down (no leak). | `record.component.ts:1412,1416-1421` | **CONFIRMED** |
| LOW | **`fetchGraph()` lacks a stale-result guard** (unlike the praised `entity-detail` pattern). Low probability. | `graph.component.ts:512-539` | **CONFIRMED** |

**Strengths (cited):** markdown render memoized behind `computed()` (`shared/markdown.component.ts:206,208-221`); T2 `forwardRef` bidirectional (`folder-tree:46`/`folder-row:60`); T3 opaque overlay on the floating popover (`move-to-menu:142,146-147`); root-store `UnlistenFn` lifecycle + `DestroyRef` teardown for component listeners; `afterNextRender({ injector })` everywhere DOM-after-render is needed.

---

## 4. Security — lock model / crypto / leak surface

**Bottom line:** the core lock wiring is sound and the newest surface (the bitemporal `facts` layer) faithfully copies the established gated-read + purge-on-seal pattern. **No merge-blocking sealed-content leak or only-copy LOSS found.** One genuine *new* metadata/timing leak + a reversibility weakness.

| Sev | Finding | Evidence | Verdict |
|---|---|---|---|
| MED (metadata leak) | **Superseded-fact timing leak.** `reconcile_facts` closes an open fact with `valid_to = superseding meeting's started_at`, but `set_meeting_id` stamps only Add ops — so the closed fact keeps its *visible* `meeting_id` while pointing `valid_to` at a later **sealed** meeting. The dossier renders `was "<old>" (<old date> → <SEALED date>)`, leaking the sealed meeting's timestamp + "this entity.attribute changed". Egresses to cloud (`entity_dossier`) and MCP (`format_dossier_client`). Dormant in the stub build. | `facts.rs:169-194`, `db.rs:928-934`, `dossier.rs:226-238`, `commands.rs:1508-1515`, `tools.rs:174-175` | **ACCEPTED** (lock-security-reviewer verdict; mechanism internally consistent, appropriately hedged) |
| MED (reversibility) | **Facts are lossy across lock→unlock** — seal/relock purge facts, but unlock does *not* re-derive them (by design: "a later re-summarize re-derives"). Unlike chunks, facts carry cross-meeting history a single re-summarize can't fully rebuild. | `db.rs:916,921-934,2062` | **ACCEPTED** |
| NIT | **Fail-open `NOT EXISTS (any note)` branch** in `list_facts_visible`, inherited from `entity_mentions_visible`. Practically unreachable (no note-only delete path exists) but latent if one is ever added. | `db.rs:3266-3273` | **ACCEPTED** |

**Strengths (cited):** gate parity with the reviewed graph reads (`db.rs:3259-3274` ≡ `2807-2814`), INNER join fails closed on NULL `meeting_id`; atomic purge-on-seal in the same tx + FK cascades (tests `seal_purges_facts`, `delete_meeting_cascades_to_facts`); no new egress class (extraction routes through `make_provider` consent gate + redaction, fails closed default-OFF); facts exposed only via the gated dossier (no facts Tauri command, no facts read in `mcp.rs`); additive/idempotent migration; no PII in logs.

---

## 5. Security — egress / redaction / MCP / secrets / FFI

**Bottom line:** architecturally strong — a single `make_provider` chokepoint wraps every cloud provider in `RedactingProvider` + a fail-closed consent gate, MCP defaults token-ON and visibility-gates every read, crypto is verify-before-destroy with AAD context-binding, secrets never cross IPC, FFI is pure CoreGraphics C. The weaknesses are at the *edges* of the redaction chokepoint.

| Sev | Finding | Evidence | Verdict |
|---|---|---|---|
| MED | **Title + the entire `vault_titles` list bypass redaction on `summarize()`.** `RedactingProvider::summarize` redacts only `transcript` + `related_context`; the inner provider also emits `meta.title_hint` + every `vault_titles` entry to the cloud. (The `complete()` path *does* redact whole prompts.) | `redact.rs:321-337` vs `template.rs:180-200`; senders `anthropic.rs:135`, `claude_code.rs:261` | **CONFIRMED** |
| MED | **PERSON names egress un-redacted at the Noop floor** (no NER model on a fresh install). Default provider `claude_code` + default brain `Cloud`, so once consent is granted, names leave. Regex still scrubs email/card/phone. By-design but should be surfaced loudly in consent copy. | `redact.rs:76-91,224-228,5-6`, `config.rs:196,226` | **CONFIRMED** |
| LOW-MED | **MCP token decision fails OPEN on a poisoned config mutex** — `.unwrap_or(false)` despite the `true` default + fail-closed-to-true test. | `lib.rs:190-194` vs `config.rs:219`, `commands.rs:5020` | **CONFIRMED** (fix: `.unwrap_or(true)`) |
| LOW | **Screen-share heuristic logs raw window owner + title at info level** (titles can embed doc/meeting content). | `screenshare.rs:299-303` | **CONFIRMED** (violates the no-titles-in-logs rule) |
| LOW | **Off-device web egress + HF downloads use `reqwest::Client::new()` with no TLS floor / timeout** (vs the Anthropic client which pins TLS 1.2). No SSRF (fixed Brave host). | `connectors/web.rs:107`, `redact.rs:119` vs `anthropic.rs:82-84` | **CONFIRMED** |
| INFO | Docs cite a non-existent `biometric.rs`; gating is the keychain ACL `kSecAccessControlUserPresence`. | `secrets/keychain.rs:405-407` | **CONFIRMED** (docs drift) |

**Strengths (cited):** single consent-gated chokepoint (`summarize/mod.rs:69-75,123-128`), consent flags preserve-only / can't be flipped by a settings save (`commands.rs:1968,1999`, only mutated by `consent_to_*`); brain/voice reuse the same envelope (`reason.rs:464-468`); related-context double-gated (`related_context.rs:11-14,157,171`); MCP hardened (Host + Origin allow-lists, constant-time bearer, token default ON, every tool via gated `execute_tool`); AES-256-GCM + AAD + `encrypt_file` verify-before-destroy, DEK/KEK `Zeroizing`, constant-time KEK compare, `claude_code` `env_clear()`s the child; zero `msg_send` in FFI.

---

## 6. Business-assumptions verdict

| # | Assumption | Status | Why |
|---|---|---|---|
| A | **Local-first / privacy** — audio+transcript on device; only the *redacted* transcript egresses, with consent | **PARTIAL** | Strong defaults (egress consent default-OFF + fail-closed; single chokepoint; claude_code/ollama local). **Two holes:** title+`vault_titles` egress un-redacted (always), PERSON names egress at the Noop floor (fresh install). Both behind explicit consent. |
| B | **Obsidian-native owned files** (.md, wikilinks, obsidian:// refs, .canvas, atomic) | **MET** *(not deep-audited)* | `export/obsidian.rs` + `export/canvas.rs` present; SQLite-canonical-then-export architecture. Flagged as a coverage gap — no dedicated export-fidelity pass this round. |
| C | **SQLite is canonical** — UI/MCP/Obsidian are thin readers | **MET** | Vectors, FTS, facts, graph all derive from SQLite; `note_chunks`/`vec_chunks`/`facts` are derived caches purged on seal; everything routes through `Db`. |
| D | **macOS-first** — Touch ID, ScreenCaptureKit, Keychain, notarization; `com.meetnotes.app` immutable | **MET** *(signed-build-only confirmation)* | Keychain ACL biometric gate, ScreenCaptureKit sidecar, notarization runbook, pure-CoreGraphics FFI. Touch ID / live ScreenCaptureKit only truly verifiable on a signed build. |
| E | **Provider seam + redaction firewall intact** | **MET (seam) / PARTIAL (firewall)** | Clean swappable seams (provider, brain `active_reasoner`, embedder `active_embedder`, redactor). Firewall is a real chokepoint but has the A-holes. |
| F | **Lock model load-bearing** — gate every read, verify-before-destroy | **MET** *(2 bounded issues)* | Gate-every-read + verify-before-destroy hold across all audited surfaces incl. the new vector + facts layers. Issues: the facts metadata-timing leak (MED, dormant in stub) + MCP token fail-open-on-poison (LOW-MED). |
| G | **Product promise** — record → on-device whisper large-v3 → dual-stream Me/Others → pluggable-LLM note → Obsidian .md; 3 surfaces over 1 store | **MET** | All present. (Always-on live captions is a perf concern, not a promise violation.) |
| H | **brain2 roadmap** — bundled on-device semantic RAG over owned notes; connectors as live tools not vectorized; only owned notes get vectors; source-agnostic | **MOSTLY MET / on-track** | Real e5 + sqlite-vec + RRF hybrid (capability present), but **downloaded not "bundled"**, flag default-OFF, and indexes **notes only** (not transcripts). Connectors = live tools (web Brave BYO consent-gated, calendar local) via `execute_tool`; only notes vectorized; seam is source-agnostic. |

**Overall:** the app **meets its business assumptions with gaps** — the privacy/lock/crypto core is genuinely strong and the brain2 vector capability is really built, but the redaction firewall has two egress-coverage holes, the lock model has one new metadata-timing leak, and there is an FE crash + a set of performance defects that activate once the real models download and the vault grows.

---

## 7. Top risks, ranked (with fixes)

1. **[Privacy · MED]** Title + `vault_titles` egress un-redacted → run them through `redact_into` inside `RedactingProvider::summarize` (`redact.rs:335-337`).
2. **[Privacy · MED]** PERSON names egress at the Noop floor → surface loudly in consent copy and/or require the NER model before cloud egress of names.
3. **[FE correctness · HIGH, localized]** NG0600 prep-card crash → add `{ allowSignalWrites: true }` (or seed `subject` without the read/write loop) at `pre-meeting-brief.component.ts:432`. RED/GREEN repro first.
4. **[Perf · HIGH]** Embedder + NER rebuilt per call → cache one `Arc<dyn Embedder>` + one redactor in `AppState`, exactly like `reasoner`.
5. **[RAG correctness · MED]** Stub-vector index poisoning → gate the auto-index on `embed_model_present()` (mirror reindex) and/or stamp each `vec_chunks` row with a model id, excluding foreign-generation rows from KNN.
6. **[Perf · MED]** Always-on large-v3 live captions → gate behind a setting and/or use tiny/small for the live path.
7. **[Security · MED]** Facts superseded metadata leak → suppress a closed fact's `valid_to` (the "→ sealed date") in `list_facts_visible`/`render_structured` unless a *visible* successor exists.
8. **[Security · LOW-MED]** MCP token fail-open → `lib.rs:194` `.unwrap_or(true)`.
9. **[Perf · MED]** 16 GB-Mac memory ceiling → an idle-eviction / "unload brain" path + document the RAM floor per brain size.
10. **[RAG scope · MED]** Semantic = notes-only → also chunk/embed transcript segments (with a real `source_type`), or document the note-only scope.

Lower: N+1 in digest/topic-threads; unvirtualized library list + per-row method recompute; timeline tick recompute; web/NER `reqwest` TLS floor; screenshare window-title in logs; `content_hash` dead code; voice-trigger KWS on large-v3.

---

## 8. Coverage gaps (need a signed build / real Mac / runtime)

- Touch ID KEK release, lock-at-rest behavior, screen-share auto-relock firing on a real Zoom/Meet share.
- Real e5 / DeBERTa quality + Polish recall, Metal load times, measured RSS per model, the live-caption keep-up at a 3 s tick.
- vec0 KNN latency at real vault scale (tens of thousands of chunks).
- Obsidian export fidelity (front-matter / wikilinks / canvas / atomic write) — no dedicated pass this round.
- The facts metadata-leak field-frequency (needs a real reasoner emitting superseding facts across an open→seal boundary).

---

*Generated by a 5-branch code-grounded audit with adversarial verification of material claims. The initial heavy Workflow run stalled (agents contending on a cold ML build); this audit was re-run with build-free, read-only subagents and the highest-stakes findings personally re-verified against cited code.*
