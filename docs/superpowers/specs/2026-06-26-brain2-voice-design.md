# brain2 — Voice Foundation Design

> Status: **DRAFT for review** · Date: 2026-06-26 · Owner: Jakub
> Scope of THIS spec: the first sub-project of brain2 — make **voice** a hardened,
> safe, queryable first source and lay the aggregation foundation. Multi-source
> (Slack/mail/calendar/Linear) is the north star but is **explicitly deferred**
> (see §9). Every claim about the current code below was adversarially verified
> against the real tree on 2026-06-26 (24-agent workflow `wf_b8d9ac5a-88d`); the
> directive throughout was **trust code, not docs** — the docs were repeatedly wrong.

---

## 1. Vision & intent

**brain2** evolves the existing MeetNotes/Murmur desktop app (Tauri 2 Rust core +
Angular 18 frontend) into a **multi-source context-aggregation hub** — a "second
brain" where context from many sources flows into one coherent, queryable store.

Decisions locked in the brainstorm:

- **Audience:** personal-first, **product-aware** later. Build for the single user
  (local-first, no multi-tenancy now), but make the cheap architectural decisions
  that keep a future product viable (§8).
- **Core value (the "magic moment"):** *aggregation of many sources into one
  queryable brain.* Not proactive nudging, not task-management — those are later.
- **Start with voice.** Voice is **source adapter #1** in a pluggable
  ingest → normalize → store → query pipeline. Later sources plug into the same
  mechanism.
- **Voice MVP modes:** (a) **meetings** (multi-party: mic + system audio) and
  (b) **solo brain-dump / daily planning** (mic only). NOT hotkey-capture, NOT
  ambient/always-on (deferred).
- **Three consumption surfaces, one source of truth:** a rich own UI, an advanced
  MCP server for Claude, and Obsidian export — all as **thin readers/exporters over
  one canonical store**. Never three diverging copies.

## 2. Chosen architecture — "Approach A′" (revised after verification)

Keep the Murmur repo. The canonical store is the existing **SQLite DB**; the UI,
MCP, and Obsidian are readers/exporters over it.

**Why A′ and not the original A:** verification refuted the load-bearing premise
that Murmur is "Obsidian-native". It is **already SQLite-canonical**:

- Write path is DB-first: `pipeline.rs:220-262` upserts the full note markdown into
  the `notes` table, *then* writes the vault `.md` as a downstream export and stamps
  `exported_path` back.
- All three current readers read **only** SQLite: MCP (`mcp.rs:111-187`),
  Ask-My-Vault (`summarize/vault_context.rs:21-69` — DB despite its name), UI
  (`commands.rs` `get_meeting_detail`/`get_last_note`/`get_action_items`).
- The Angular UI has **zero** filesystem reads (verified by grep across `src/`).

So the original plan's biggest line item — "de-Obsidian-ify / promote SQLite to
canonical" — is **largely already done**. Budget moves **away** from that and
**toward**: hardening voice, fixing two real bugs, real search, entity persistence,
and a safe migration framework.

### 2.1 The one exception that IS real work: entities

The entity graph (people/projects) is the single thing **not** in SQLite. Today
`link_meeting_entities` (`commands.rs:716-752`) writes entities **straight to the
Obsidian vault** as markdown stubs (`export/entity_stub.rs:10-48`, `std::fs`).
There is **no `entities` table**. So "Obsidian = export-only" is *false for
entities*, and persisting them in the DB (plus a vault→DB importer so existing
stubs aren't orphaned) is genuine, required work — not a relabel.

## 3. What verification changed (the binding corrections)

| Finding | Evidence | Consequence for this spec |
|---|---|---|
| SQLite already canonical; Obsidian already export | `pipeline.rs:220-262`, `mcp.rs`, `vault_context.rs` | Cheaper than planned; drop "de-Obsidian-ify" framing |
| **Entities live only in the vault**, no `entities` table | `commands.rs:745-750`, `entity_stub.rs`, schema `db.rs:76-123` | Add `entities`/`entity_mentions` tables; DB-first write; vault→DB importer (M1) |
| **`search()` is `LIKE`, not full-text** — proven broken live | `db.rs:252-291`; live MCP: `"test nagrywania"`→5 hits, `"nagrywania test"`→0 | Replace with FTS5+BM25 (M1) before building more on it |
| **Redaction firewall bypassed by default** | `summarize/mod.rs:50-60` wraps only `anthropic`; default provider is `claude_code` (`config.rs:43`), a cloud relay falsely labelled "local" | Route `claude_code` through `RedactingProvider`; treat any non-Ollama provider as cloud (M1) |
| **mix-to-mono destroys diarization** | `pipeline.rs:103-117` sums mic+system → one buffer before Whisper | Preserve dual-track / channel split NOW, before the value is lost (M0) |
| **System-audio never captured live** | `audio/system.rs:2-6`, `sysaudio.swift:11-13` "RUNTIME-UNVERIFIED"; off by default `config.rs:52` | M0 must prove it on a real Mac, recorded |
| **Polish quality never measured** | E2E only English/`base.en`; default model `small` (`config.rs:53`) | M0 adds a Polish E2E lane + pins a model recommendation |
| **No permissions UX** | no `AVAuthorizationStatus`/`requestAccess` anywhere; silent "No speech detected" | M0 adds a pre-flight permission probe + onboarding step |
| **Migration framework absent** | `user_version=0`, additive-only `CREATE TABLE IF NOT EXISTS`, E2E only on fresh DB; 18 real meetings live | M2: versioned transactional runner + backup + old-schema-fixture test |
| **Entity extraction has no quality floor** | exact-string dedup only (`graph.rs:59`) — "Anna K."/"Anna Kowalska" = 3 people | Quality bar defined before scaling sources (deferred to multi-source, §9) |
| **Cost/throughput unmodeled** | 1 LLM call/item, no batch/incremental (`graph.rs:29-31`) | Budget + incremental design required before a 2nd source (§9) |
| **4 aggregation layers already exist** | Ask-My-Vault, Digest, Topic Threads, entity graph | Generalize their **source axis**; do NOT rebuild (M3+) |

What held up (no change needed): all 13 features are real (C1 ✅), the app is
already headless-core + thin-readers (C6 ✅), and the quality gate is real and
reproducible — 71 tests, 0 ignored, 0 `todo!()`, clippy `-D warnings` clean (C7 ✅).

## 4. Components (units, each one job)

- **Capture** (`audio/*` + `sysaudio.swift`): mic (cpal) + system audio
  (ScreenCaptureKit sidecar). Output: **separate** mic + system tracks (changed
  from mixed) plus the legacy mixed buffer for transcription.
- **Transcription** (`transcribe/*`): whisper.cpp via `whisper-rs` (Metal). Wrapped
  behind a new **`Transcriber` trait** (impl #1 = on-device) so a future
  server/remote impl is an adapter, not a refactor.
- **Canonical store** (`storage/db.rs`): SQLite, single source of truth. New tables
  this spec: `entities`, `entity_mentions`; additive columns: `source_type` on
  `meetings`, `owner_id` on new root tables, optional `speaker`/`channel` on
  `segments`. FTS5 virtual table over `notes.markdown` + `segments.text`.
- **Migration runner** (new, `storage/migrate.rs`): `PRAGMA user_version` +
  ordered, transactional steps + automatic pre-migration `.sqlite` backup.
- **Summarize/entities** (`summarize/*`): provider trait already generic
  (`provider.rs:46 complete(system,user)`). Redaction made provider-agnostic for
  all cloud providers.
- **Readers/exporters:** Angular UI (IPC), MCP server (`mcp.rs`), Obsidian export
  (`export/*`). All read the store; none is a source of truth.

## 5. Data flow

```
[mic track]  [system track]            (M0: kept separate, not pre-mixed)
      \         /
       v       v
   (mix for ASR only) --> Whisper --> segments{idx,start_s,end_s,text,channel?}
                                          |
                                          v
                              SQLite (DB-first, canonical)
                              meetings(+source_type,+owner_id)
                              notes, segments, entities, entity_mentions
                                          |
                 +------------------------+------------------------+
                 v                        v                        v
            Angular UI                MCP server              Obsidian export
          (IPC commands)         (read-only, advanced)     (.md + .canvas, write-through)
```

All cloud-bound text (summaries, entity extraction, chat) passes the **redaction
firewall** regardless of provider (M1 fix).

## 6. Milestones (re-sequenced so unverified, value-bearing risk leads)

### M0 — Voice Hardening  *(gate: real Mac, recorded evidence; unit-green is NOT acceptance)*
1. Live **mic** capture with permission granted → non-empty transcript.
2. Live **system-audio** capture via the sidecar → non-empty WAV (closes the
   `system.rs:2` / `sysaudio.swift:11` "RUNTIME-UNVERIFIED" gap).
3. A real **two-party mixed** note end-to-end.
4. **Dual-track preserved:** persist mic + system as separate tracks (and/or a
   `channel` on segment rows) so future diarization is possible; stop letting
   `pipeline.rs` mix-to-mono destroy attribution.
5. **Permissions UX:** a Tauri command probing mic (`AVAuthorizationStatus`) +
   Screen-Recording (`CGPreflightScreenCaptureAccess`), surfaced as an onboarding
   step; convert silent failure into an actionable prompt.
6. **Polish baseline:** a Polish E2E lane (deterministic PL fixture) against the
   multilingual model at the default size; measure WER; pin a model recommendation
   (likely `medium`, not `small`).

### M1 — Safe foundation  *(the two bug fixes + real search + entity persistence)*
1. **Redaction fix:** route the default `claude_code` provider through
   `RedactingProvider`; reclassify all non-Ollama providers as cloud
   (`summarize/mod.rs:47-60`). Fix the false "local" comments/docs.
2. **FTS5 search:** replace the `LIKE` scan (`db.rs:252-291`) with an FTS5 virtual
   table + BM25 ranking, behind the existing `db.search()` signature so MCP, Ask,
   and UI inherit it for free. Regression test: `"A B"` and `"B A"` both return the
   doc containing both words.
3. **Entity persistence:** add `entities(id, kind, name, canonical_name, owner_id)`
   + `entity_mentions(entity_id, meeting_id)`. Rewrite `link_meeting_entities` to
   write **DB-first**, then `entity_stub.rs` exports from the DB rows. Add a
   one-time **vault→DB importer** so existing markdown stubs aren't orphaned.
4. **Product-safe columns now:** `owner_id TEXT NOT NULL DEFAULT 'local'` on every
   new root table; `source_type TEXT NOT NULL DEFAULT 'voice'` on `meetings`.
5. **`Transcriber` trait** around `whisper.rs` (impl #1 = on-device); generalize
   `secrets::get_secret` to `(scope, account)` even if scope is always `'local'`.

### M2 — Migration framework  *(must exist before any schema change ships to a real DB)*
- `PRAGMA user_version` + ordered transactional migration runner with an automatic
  pre-migration `.sqlite` backup.
- CI lane (extend `e2e-core.sh`) that runs migrations against a **seeded old-schema
  fixture** DB and asserts existing meeting rows survive losslessly. Not a fresh DB.

> Note: M1's additive columns/tables ride on M2's runner. In practice M2's runner
> lands first or alongside M1's first migration; they are listed in value order, not
> strict calendar order. The binding rule: **no schema change touches a real user DB
> without the runner + backup + fixture test in place.**

### M3 — Generalize one reader's source axis  *(the bridge to multi-source)*
- Make `build_vault_context` (Ask-My-Vault) operate over a generic, source-tagged
  **item corpus** instead of a meeting list — proving one reader generalizes before
  touching Digest/Threads/MCP. No new source adapter yet.

## 7. Testing strategy

- Keep the existing gate green throughout (`scripts/ci.sh`: clippy `-D warnings`,
  cargo test, ng lint/build, `e2e-core.sh`, `e2e-mix.sh`).
- **New lanes:** Polish ASR E2E (M0); FTS word-order regression (M1); entity
  DB-persistence + vault-export-faithfulness test (M1); migration-against-fixture
  test (M2); an assertion that every new generic row carries `owner_id` (M1/M2).
- **Runtime gates** (M0 system-audio, permission-denied path, Polish WER) require a
  real interactive Mac and **recorded evidence** — unit-green is explicitly not
  acceptance (mirrors the existing stubbed-summary caveat in C7).

## 8. Product-later decisions (cheap now, prevent a rewrite)

Made now: `owner_id` column on new root tables; `Transcriber` + existing
`SummarizerProvider` traits with local impls as adapter #1; `secrets` keyed by
`(scope, account)`; **all** data access stays behind the single `storage/db.rs`
wrapper (MCP already re-opens via `Db::open` — keep it that way).

Deferred (reachable behind those seams): auth/login/session, multi-tenant Postgres,
the `Mutex<Connection>` → pool rework, sync/CRDT, RBAC, server-side inference.

## 9. Explicitly deferred (YAGNI) — and why

- **Full `sources→items→entities→links` normalization.** Designing a generic
  4-table graph against a sample size of one adapter is premature. Defer the generic
  `items`/`links`/`sources` tables until a **second real adapter** (Slack/calendar)
  reveals what an "item" and a "link" actually need. For now: `meetings` row = the
  de-facto item, distinguished by `source_type`.
- **Second/third source adapters** (Slack, mail, calendar, Linear).
- **Entity-extraction quality program** at scale: entity resolution / canonical IDs
  (kill exact-string dedup), local-NER fast path (also unblocks name redaction),
  batching + incremental "new-items-only" extraction, and an LLM cost/throughput
  budget. Required **before** a second source ships; not needed for voice-only.
- **Speaker-print diarization (ONNX).** M0 only *preserves* the channel signal; full
  diarization is a later epic.
- **Hotkey capture, ambient/always-on capture.**

## 10. Open questions

1. M0 acceptance: is "system audio proven on your Mac with one real call, recorded"
   enough to exit, or do we want a multi-meeting soak first?
2. Polish model size: accept a larger default (`medium`) globally, or only when
   `language=pl`? (Bigger model = slower/more memory.)
3. Entity persistence shape: minimal `entities`+`entity_mentions` now, or also a
   generic `entity_links` (entity↔entity) table while we're in there?
4. Does the rich UI need new views in this sub-project, or is the existing
   Library/Detail/Ask/Analytics surface enough until multi-source lands?

## 11. Evidence

Verification workflow `wf_b8d9ac5a-88d` (24 agents, ~1.6M tokens): 7/10 claims
returned with verifier+skeptic verdicts; C2/C3/C10 covered by the four plan
critiques. Verdicts: C1 ✅, C4 ❌(refuted, favourably), C5 ⚠️, C6 ✅, C7 ✅,
C8 ⚠️(real bug), C9 ⚠️. No fatal flaw found; "voice is mostly done" rejected as a
dangerous underestimate.
