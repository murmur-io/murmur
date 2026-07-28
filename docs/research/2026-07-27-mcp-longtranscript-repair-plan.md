# Murmur — MCP + Note-Pipeline Repair Plan

**Scope:** 21 reported defects, originally verified against the 2026-07-27
trunk snapshot (post-v1.0.3, post-#453/#454/#455/#457). The status table below
is refreshed against `murmur` at `cc09eed` on 2026-07-28.
That base and the remote-state column are an operator-captured Git/GitHub
snapshot, not evidence produced by this docs task's control-plane checks.
**Verdict distribution at the original snapshot:** 18 CONFIRMED, 3 PARTIAL.
**Zero were fully ALREADY_FIXED or NOT_A_DEFECT.** Section 0 now contains six
claim-level corrections/clarifications; some are stale premises inside otherwise
valid items, not six additional verdict changes.
**Current execution model:** rebuild remaining batches sequentially from fresh
`origin/murmur`. Harness v2 is verifier-only: implement in its isolated
worktree, derive the exact-diff plan, then `verify`/`resume`; there is no
harness-owned writer or repair loop. Actual lock/content-read changes require a
fresh `lock-security` specialist. Old branch attestations and green CI do not
transfer to a rebuilt composition.

---

## Status update — 2026-07-28

| PR | Plan intent | Remote state | Current interpretation |
|---|---|---|---|
| #480 | R2 attribution contract | **MERGED** | Landed on trunk. |
| #481 | R1 transcript renderer | **MERGED** | Landed on trunk. |
| #482 | R4 fact-registry integrity | **MERGED** | Landed on trunk. |
| #483 | R3 provenance demarcation | **MERGED** | Landed on trunk. |
| #484 | R5 commitments owner recall | **MERGED** | Landed on trunk. |
| #485 | R7/R8 payload bounds | **MERGED** | Landed on trunk; preserve its bounds in every rebuild. |
| #486 | R10 located transcript search | **OPEN**, Rust/full gate failing | Rebuild with a read address accepted by `get_meeting`, global totals, and strict caps. |
| #487 | R12/R13 discovery and triage | **OPEN**, green CI | Do not merge as-is: independent audit found a sealed-folder oracle and discovery-after-cap gaps. |
| #488 | R16 org relevance floor | **OPEN**, green CI | Do not merge as-is: thresholds and substring/AND-path behavior are not correctly pinned. |
| #489 | R15 render-time channel dedup | **OPEN**, green CI | Defer/rebuild; the default heuristic can delete real short or repeated speech. |
| #490 | speaker/voiceprint mapping (out-of-plan companion to R17) | **OPEN**, green CI | Rebuild with a meeting-scoped gated reader and plain-`others` cluster handling. |
| #491 | R6 decisions registry | **OPEN**, green CI | Do not merge as-is: seal/relock and deterministic persistence findings remain. |
| #492 | R9 chapters | **OPEN**, green CI | Combine with navigation-address work so chapter/search/read share one segment space. |
| #493 | R14 domain glossary | **OPEN**, Rust/full gate failing | Rebuild after closing the glossary egress/accounting and settings-loss gaps. |
| #494 | R17 diarization/grounding settings | **OPEN**, green CI | Rebuild after #493; grounding is not wired through the Rust DTO. |
| — | R11 triage columns | **No standalone PR** | Re-scope with discovery/navigation work; do not infer it from #487's raw-size field. |

The open heads are parallel snapshots, not a safe stack. In particular,
#486/#487/#489/#490/#492 overlap in `mcp.rs`/`tools.rs`, use incompatible tool
counts or offset spaces, and have no combined runtime or lock proof. Their
current CI status is recorded for diagnosis only; it is not an acceptance
verdict. The do-not-merge details come from an independent, read-only operator
session audit on 2026-07-28; that audit was not itself a persisted Harness
receipt, so this document treats it as an explicit recommendation that must be
re-proved by each rebuilt exact diff.

---

## 0. Already handled / report is stale — do NOT re-fix

At this 2026-07-28 refresh, ten reported defects are fully closed: **#1**
(#453 + #485), **#3** (#480), **#4** (#483), **#5** (#480 + #483),
**#10** and **#13** (#485), **#12** (#481), **#15** (#485), **#16**
(#484), and **#17** (#482). Two retain open remainders: **#2** has its
renderer half closed by #481 but its default-on product decision remains R17,
and **#19** has the vector floor from #457 but no sound keyword floor. The
remaining nine — **#6, #7, #8, #9, #11, #14, #18, #20, and #21** — remain
wholly open. The original-snapshot verdict distribution (18 CONFIRMED,
3 PARTIAL, zero fully fixed) is historical evidence, not the current queue.
The claim-level corrections below were measured against an older build or a
wrong premise. **Do not scope new work against a closed claim.**

| Item | Stale claim | Evidence on trunk |
|---|---|---|
| **#1** | "20 pages × 19.5k = 390k of note tax" | The original repeated-note tax was fixed in `da63840` (PR #453). The two surviving sub-claims (note ignored `maxChars`; no `includeNote`) were subsequently resolved by R7 in merged PR #485. Later navigation work must preserve those bounds. |
| **#2** | "No diarization; 6 participants collapse to Me/Others" | **PREMISE WRONG.** Murmur ships full N-way offline diarization: `transcribe/diarize.rs::Diarizer` (pyannote-segmentation-3.0 → CAM++ → FastClustering `num_clusters=-1`) + `relabel_others` → `others-{N}`, wired in `pipeline.rs` (`asr_diarize` stage), with cross-meeting voiceprint re-ID (`compute_cluster_voiceprints`/`suggest_voiceprint_labels`, gated read `db.rs::list_voiceprints_visible`), a DER/EER harness (`eval/diarization.rs`) and a Settings toggle. The reporter saw Me/Others because **`settings/config.rs` defaults `diarize_others: false`** and because turning it ON currently makes MCP *worse* (see #2b). |
| **#19** | "Semantic junk: 'Kongo' matched 'Kong'" | **PARTLY MITIGATED** in `83b40c2` (PR #457): KNN rows below `ORG_KNN_SEARCH_COSINE_FLOOR = 0.78` are dropped. Exact FTS5 token matching cannot produce `Kongo` from `Kong`, so the historical observation most likely came from the vector/body path rather than exact FTS. Remaining: the keyword fallback still lacks a sound relevance contract, substring-based filtering can create false hits, no score is exposed, and boilerplate is unfiltered. Open PR #488 does not close this as written; its threshold and AND-path tests are insufficient. |
| **#11** | "Wrong id is indistinguishable from empty" | **Partly wrong** — the two outcomes *are* textually distinct today (`No outline for document {id} (…)` vs `No data for document {id}.`). The real defect is the **dead end** (all three stated causes are false for a meeting id, and there is no redirect) → batch **R12**. |
| **#21** | "Wrong folder name is indistinguishable from an empty folder" | **Partly wrong** — `No note folder named "{folder}".` vs `No rows matched in "{name}".` already differ. The real defect is **no discovery surface** → batch **R12**. |
| **#18** | (implicit) "the repo has a vocabulary gate" | `scripts/check-vocabulary.mjs` is **UI-copy jargon policing** (`BANNED = [['egress',…],['GGUF',…]]`, reads `src/app/core/copy/glossary.ts`). Zero relation to transcription vocabulary. Do not mistake it for an existing glossary. |

---

## 1. The root-cause map (read before sequencing)

Three **upstream** defects author damage that many downstream items only mitigate:

```
U1  diarize_others = false by default        →  one collapsed `others` lane
      └─ #3 name-guessing (prompt fills the gap with vocatives)
      └─ #16 "me" is ambiguous; owner rollups can't merge
      └─ #2b when you DO enable it, others-N renders as "Unknown" (regression)

U2  no per-region echo arming (align.rs MAX_SPREAD_S=0.2 on drifting clocks)
      └─ #14 duplicate Me/Others pairs, AEC also silently inert
      └─ #13 ~40% scaffolding is partly duplicated speech
      └─ note quality: the SUMMARIZER sees the duplicated feed too

U3  no domain glossary / no ASR bias / entity keys are lowercase-only
      └─ #18 mangled proper nouns
      └─ #7 "Konnect" and "Connect" become two permanent, unmergeable entity rows
      └─ #6/#17 durable junk facts inherit the mangling
```

**Honesty bar:** #3, #16, #13, #7 are *mitigations*. They stop the system from stating false precision — they do **not** restore who-said-what, de-duplicate the audio, or repair a mangled proper noun. Only U1 (diarization on + voiceprints), U2 (piecewise alignment), U3 (glossary + canonicalization) address the source, and **all three need a real Mac with real recordings to prove**.

---

## 2. Batches

Ordered trust-first, then value/cost within each tier.

### PHASE 0 — cheap correctness (land first, unblocks the cost work)

---

#### R1 · Transcript renderer truth
**Status:** **MERGED in PR #481.** The body below is the pre-merge design and
RED specification; its "today"/"RED on trunk" wording describes the old base.
**Defects:** #2(b) `others-N` → "Unknown"; #12 13-decimal timestamps
**Size:** XS · **Deps:** none · **Lock review:** no

**Touch list**
- `src-tauri/src/tools.rs::format_structured_transcript` (the sole call site is `tools.rs` `ToolCall::GetMeeting`, verified by grep)
- `src-tauri/src/transcribe/diarize.rs::cluster_index_of_tag` (reuse; may need `pub(crate)`)
- new private `tools.rs::secs(f64) -> String`

**Fix**
1. Replace `Some("others") => "Others", _ => "Unknown"` with a parse of the `others-{N}` tag → `Speaker {N+1}`; plain `others` → `Others`; genuinely-absent speaker → `Unknown`. Today, enabling diarization *degrades* the MCP transcript (Others → Unknown) while `summarize/template.rs` and `summarize/timeline.rs` consume the identical tag correctly — two consumers of one tag disagree.
2. `fn secs(v) { let r = (v*10.0).round()/10.0; if r.fract()==0.0 { format!("{}", r as i64) } else { format!("{r:.1}") } }`, used for both `start_s`/`end_s`. Root cause of the long decimals is confirmed: `pipeline.rs` `segment.start_s += offset_s` and `audio/merge.rs::82` `start_s: seg.start_s + offset_s` — f64 addition of a non-representable wall-clock offset. Affects essentially every `others` segment.

**RED-before-GREEN**
- `others-0`/`others-1` fixture must NOT render "Unknown" (RED on trunk).
- Fixture `start_s: 3659.3188999159997` must render `[3659.3–` (RED on trunk).
- The integer-collapse branch keeps `[12–15]` / `[5–8]` byte-identical, so `tools.rs::get_meeting_structured_transcript_default` and `mcp.rs::mcp_get_meeting_transcript_format_switches` stay green **with zero edits** — verify this, it is the whole reason for the `fract()` branch.

**Risk** — None to lock model or wire format (one-call-site free-text renderer). Behavior: `TOTAL_CHARS` shrinks ~24%, so an agent holding a cached char offset across the upgrade lands elsewhere; self-correcting because `TOTAL_CHARS` is re-disclosed every call. Leave the `"plain"` branch untouched so `get_meeting_transcript_format_plain_is_byte_identical` holds.

**Value:** reclaims ~28k chars (~7k tokens) on a 1h meeting *and* fixes a trust regression. Best value/cost in the whole program.

---

### PHASE 1 — trust: what the note claims

> These outrank everything below. A note that reads as a meeting record but is half pasted document, guessed names, and hardened guesses is more dangerous than an expensive one.

---

#### R2 · Note fidelity: attribution contract + epistemic strength
**Status:** **MERGED in PR #480.** The body below is the pre-merge design and
RED specification.
**Defects:** #3 (name guessing), #5 (hedge → spec) — prompt half
**Size:** S · **Deps:** none (but same file as R3 → sequence) · **Lock review:** no

**Touch list**
- `src-tauri/src/pipeline.rs::build_transcript_feed` — `TranscriptFeed.labeled: bool` → lane shape
- `src-tauri/src/pipeline.rs::summarize_and_export` — call site of `template::build_template`
- `src-tauri/src/summarize/template.rs::{build_template, speaker_attribution_directive, default_template, style_variant_with_keys}`
- tests: `template.rs::build_template_adds_attribution_only_when_labeled`, `pipeline.rs::feed_multi_speaker_is_labeled_timeline_format`, `redact.rs:1269`

**Fix**
1. **#3 root cause is the prompt itself.** `labeled = distinct.len() >= 2` is `true` for a normal dual-stream meeting (`me` + one collapsed `others` lane holding 4 people), so `speaker_attribution_directive()` fires and says verbatim *"use a participant's real NAME when it is clearly stated in the conversation"* + *"List the distinct speakers under the `participants` front-matter"*. With one lane covering N people the only way to comply is to guess from vocatives. The trailing "never invent a speaker the tags do not support" is far weaker and contradicts the naming clause.
   Replace the bool with the **lane shape** (`diarized_others: bool` = any `others-N` present). Two directives:
   - **collapsed** (`me` + `others` only) → *no personal-name attribution at all*; attribute to `me`/`others`; never put a guessed name in `participants`; a vocative may be written as `(inferred from "…")`, never as a record.
   - **diarized** (`others-0/1/…`) → keep today's directive (rename it as the diarized variant so the byte-pinned test gains a sibling rather than changing).
2. **#5** — add to the shared body rules in `style_variant_with_keys` **and** `default_template` (they must stay in lockstep): *"Preserve epistemic strength: when the transcript's only support for a claim is hedged ('I don't know', 'I think', 'probably', 'I forgot'), keep the hedge or mark the item `(to confirm)`; never state an unconfirmed guess as a fact or a decision."* Reuse the exact `(to confirm)` token `summarize/recipes.rs` already ships so the vocabulary is single-sourced.
3. Cheap deterministic backstop for #3 (optional, same PR): drop any `participants:` front-matter entry whose name never appears in the transcript text. Pure, local, zero egress.

**RED-before-GREEN**
- Collapsed-lane prompt contains the no-name clause and does **not** contain `"use a participant's real NAME"` (RED on trunk).
- Diarized-lane prompt is byte-identical to today's.
- Body rules contain the hedge clause for every built-in style.

**Risk** — No lock/wire/egress risk (prompt text + one internal struct field). **Real behavior change users will notice:** notes on dual-stream meetings stop carrying personal names. Frame it in the release note as *removing false precision*, and pair it with the U1 decision (R17). Prompt byte-identity tests must be updated deliberately — they exist to make prompt drift a conscious act.

**What it does NOT solve:** who actually said what. That is U1.

---

#### R3 · Provenance demarcation + deterministic grounding backstops
**Status:** **MERGED in PR #483.** The body below is the pre-merge design and
RED specification.
**Defects:** #4 (undemarcated user-notes/document material), #5 (deterministic half)
**Size:** M · **Deps:** R2 (same file, sequential) · **Lock review:** no

**Touch list**
- `src-tauri/src/summarize/template.rs::render_user_content` (the user-notes block)
- `src-tauri/src/summarize/grounding.rs::{is_skipped_heading, annotate_unverified, content_tokens, GROUND_MIN_COVERAGE, NEGATOR_TOKENS, has_negator}`
- `src-tauri/src/summarize/recall_net::append_possible_missed_items` (precedent to copy)
- `src-tauri/src/facts.rs::extract_fact_candidates` (skip hedge/foreign-marked lines)

**Fix**
1. **Prompt.** The current block is headed *"The user's own in-meeting notes (SKELETON — build the note around these)"* and says *"Never invent content that is not grounded in the transcript **OR THESE NOTES**"* — it explicitly widens the grounding set to the pasted document, with no instruction that Decisions/Key points come from the recording. Admitted whenever `config.notes_mode == "enhance"`, which is the **default**. Rewrite it as a provenance contract: Summary / Key points / Decisions / Action items come **exclusively from the TRANSCRIPT**; anything from the user's notes the transcript does not cover goes verbatim under one section headed exactly `## From my notes` (and later `## From attached materials`); a document's open question never lands under Decisions.
2. **Deterministic backstop (the trustworthy half).** Add the new heading(s) to `grounding.rs::is_skipped_heading` (which already protects `my notes` / `related prior notes` / `also discussed` + PL variants). Add an enhance-mode pass that, for each line under a recording-only section, computes transcript token coverage with the existing `content_tokens` / `GROUND_MIN_COVERAGE` math and **annotates** (v1: do not move) below-floor lines with `> from your notes, not the recording`. Reuse `annotate_unverified`'s non-destructive, idempotent line walk.
3. **#5 deterministic half.** Add `HEDGE_TOKENS` + `has_hedge()` beside `NEGATOR_TOKENS`/`has_negator` (proof the pattern is implementable — it already exists for negation). When a note line's best-matching segment (already computed by `align_claims_to_segments`) is hedged and the line is not, append `> unconfirmed in the recording`. Feed the same signal into `facts.rs` so hedge-marked lines produce no fact candidates. Gate behind the existing `ground_summary` opt-in until calibrated.

**RED-before-GREEN**
- A note whose `## Decisions` contains a user-notes-only claim must not survive unmarked (RED on trunk).
- Note line "The flag is a Boolean" vs segment "a Boolean, true, false, probably" must earn the `unconfirmed` marker (RED on trunk).
- Idempotence: running the pass twice yields byte-identical markdown.

**Risk** — All local string ops on plaintext the pipeline just produced; markers are sealed with the note exactly like `> unverified`. Keep `## Action items` line positions byte-stable so `action_items::parse_action_items` does not shift. Prefer annotate-in-place over move for v1 — a move could relocate a legitimately abstractive but grounded sentence. False-positive risk on the hedge half (a hedge in the best-matching segment does not always govern the claim) → annotation-only, opt-in.

**Why this matters more than any cost item:** today, receipts do **not** repair it. An unsourced line simply gets no `ClaimAlignment` — absence of a chip, indistinguishable from a paraphrase.

---

#### R4 · Fact-registry integrity: no junk owners, and make junk correctable
**Status:** **MERGED in PR #482.** The body below is the pre-merge design and
RED specification.
**Defects:** #17
**Size:** S (guard) + M (forget command) · **Deps:** none · **Lock review:** no

**Touch list**
- `src-tauri/src/facts.rs::candidates_from_triples` (+ signature: accept a person roster)
- `src-tauri/src/commands/mod.rs::build_and_persist_entities` (pass `payload.people`, not the merged vec)
- `src-tauri/src/commands/facts.rs::persist_facts_for_meeting`
- `src-tauri/src/summarize/template.rs::front_matter_list` (roster source, already `pub(crate)`)
- `src-tauri/src/summarize/mod.rs` provider ids (`PROVIDER_CLAUDE_CODE`, `ollama`, `gateway`, `anthropic`) for the blocklist
- `src-tauri/src/storage/facts_store.rs` + `lib.rs::generate_handler!` (new `forget_entity_fact`)

**Fix**
Today `candidates_from_triples` validates **only the entity** (resolved against the meeting's known entities, "never invent"); `predicate` and `object` are taken verbatim with `.trim()` + non-empty, and `confidence` is **hardcoded 1.0**. There is no roster check, allowlist, or blocklist anywhere. The ingestion path is real and traceable: `pipeline.rs::fold_manual_notes` folds the user's typed companion-note text (including pasted agent/CLI output) into the markdown that `extract_fact_candidates` consumes.
1. Extend `candidates_from_triples(triples, entities, roster)`; for a person-valued predicate set (`owner`, `assignee`, `właściciel`, `odpowiedzialny`, `role`, `rola`), reject objects not plausibly a person.
2. Belt-and-braces 5-line static blocklist of tool/provider ids — this alone would have stopped `owner: claude_code`.
3. **Add `forget_entity_fact(id)`** mirroring `forget_user_fact` (bitemporal invalidate, registered in `generate_handler!`). Today entity facts are **uncorrectable** — `facts_store.rs` exposes forget/clear for *user* facts only; only a later meeting asserting a different owner for the same `(entity, subject, predicate)` key can close one.

**RED-before-GREEN** — triple `(M1 Advanced Mode, owner, claude_code)` with
roster `["Person A","Person B"]` yields zero candidates (RED on trunk). Second
test: `forget_entity_fact` closes an open row and
`dossier.rs::render_structured` stops printing it.

**Risk** — **The one to be careful about:** a legitimate owner who did not
attend ("the external owner will pick it up") would be dropped — silently
losing a real fact is worse than the junk it prevents. Mitigate with roster =
union of note participants **+ all visible Person entities** (a superset that
still excludes `claude_code`), or keep the fact with reduced `confidence` (the
column exists, currently a constant 1.0) and have `render_structured` mark
low-confidence rows. No lock risk — `candidates_from_triples` is a pure pre-DB
filter that can only narrow writes. No wire risk (`FactCandidate` is internal).
The extractor is best-effort by contract, so a stricter filter can never fail
the note pipeline.

**Note the vault-only surface:** `export/obsidian.rs::inject_provenance_frontmatter` writes `ai-provider: claude_code`, but `pipeline.rs` applies it to a **local** binding *after* the DB `upsert_note`, so the DB copy has no provider key today. It would become live the moment anything re-ingests the vault file — and `commands/links.rs::link_meeting_entities` already re-feeds the stored `note.markdown` to the same extractor. **The guard therefore belongs in the extractor, not the note writer.**

---

#### R5 · Commitments: recall, owner normalization, navigable source
**Status:** **MERGED in PR #484.** The body below is the pre-merge design and
RED specification.
**Defects:** #16
**Size:** XS/S (part A) + M (part B) · **Deps:** complementary to R2; independent to land · **Lock review:** no

**Touch list**
- `src-tauri/src/tools.rs::format_commitments`, `tools_spec`, `ToolCall::GetOpenCommitments`
- `src-tauri/src/mcp.rs` inputSchema + `dispatch_tool` `"get_open_commitments"` arm
- `src-tauri/src/storage/db.rs::list_open_commitments`
- `src-tauri/src/storage/db.rs::list_people` (`open_commitment_count`) and `src-tauri/src/summarize/dossier.rs::{build_dossier_data, render_structured}` — **these three predicates are bound identical by `list_people`'s own doc comment; they move together or the /people badge and the dossier disagree**
- `src-tauri/src/summarize/action_items.rs::{extract_owner, find_date, is_iso_date}` (+ new `normalize_owner`)
- `src-tauri/src/settings/config.rs::AppConfig` (new `user_display_name`)
- `src-tauri/src/commands/tests/ask_vault_tests.rs` (the in-app agentic loop parses the same string)

**Fix — part A (ship first, XS/S)**
- Emit `id:{c.meeting_id}` alongside `[[{c.meeting_title}]]` — `Commitment` already carries `meeting_id` (populated at `db.rs::list_open_commitments`), `format_commitments` just never prints it, so an agent cannot navigate programmatically.
- Emit an explicit `due —` when `due_date` is None instead of silently omitting the segment.
- Add `since` / `until` / `meetingId` / `overdueOnly` / `limit` / `offset`; disclose the total **and** the currently-silent `list_meetings_visible(1000, …)` cap in the reply header, the way `page_text_disclosed` discloses `TOTAL_CHARS`.
- Fix the MCP description, which over-promises "owner, due date and source meeting".

**Fix — part B (M, the recall fix)**
`list_open_commitments` filters with raw exact equality:
`o.trim().to_lowercase() == want`. So `owner="Person A"` drops
`"Person A (others-9)"` and `"others-10 -> Person A"` — the reported 4-of-7
(57%) recall is exactly what this code produces. The `(others-9)` residue is
the **diarization cluster tag leaking through the prompt**:
`speaker_attribution_directive` says *"otherwise keep the tag label"*, and
`action_items.rs::extract_owner` takes the head before the em-dash verbatim
with only a ≤40-char guard.
- Add `normalize_owner(raw)`: strip `[[ ]]`/`| alias`, strip leading/trailing `(others-N)`/`(me)`, strip an `others-N ->`/`others-N:` prefix, collapse whitespace.
- Apply at **read time** in `list_open_commitments` (this is what fixes *existing* vaults) **and** at ingest in `extract_owner` (fixes future notes). Do both.
- Keep the match case-insensitive **equality on the normalized form** — do not go fuzzy.
- `me` is genuinely ambiguous: there is no recording-account identity on this path (`AppConfig` has no user/display name; `AccountStatus` is opt-in and unused here). Add optional `user_display_name` (Settings → "Your name", falling back to the signed-in account's local-part) and resolve normalized `me`/`ja` to it. When unset, **do not merge** — label it `me (unresolved — set your name in Settings)`.

**RED-before-GREEN** — seed notes with `Person A`,
`Person A (others-9)`, `others-10 -> Person A`; `owner="Person A"` must return
3/3 (RED: returns 1). Second test: `me` from two different meetings is NOT
merged when `user_display_name` is unset. Unit tests on `normalize_owner` for
all three reported variants.

**Risk** — No lock risk (stays inside the `list_meetings_visible` + `get_note_if_visible` double gate; normalization widens *which visible items match a filter*, never which meetings are read). `format_commitments` output shape changes → an MCP parsing change for regex-consuming clients, and the in-app loop consumes the same string. `user_display_name` follows the additive `set_setting` + default `""` rule and **must never be logged**.

**Does NOT solve:** low due-date coverage (~1 of ~110). `find_date` only matches a literal `YYYY-MM-DD` substring — no natural-language or relative dates. Scope NL date parsing separately, or just stop over-promising in the description (part A does the latter).

---

#### R6 · Decisions & risks become first-class registry rows
**Status:** **OPEN as PR #491; do not merge as-is.** Rebuild as a bounded
read-time projection from canonical visible notes. The table/migration design
below is pre-audit provenance and is not the current implementation direction.
**Defects:** #6
**Size:** M · **Deps:** R2 + R3 (garbage-in otherwise) · **Lock review:** **YES**

**Touch list**
- new `src-tauri/src/summarize/note_sections.rs::parse_labeled_bullets` (sibling of `action_items.rs::parse_action_items`)
- `src-tauri/src/commands/facts.rs::persist_facts_for_meeting`
- `src-tauri/src/storage/db.rs::Db::migrate` (additive `CREATE TABLE IF NOT EXISTS note_decisions`)
- `src-tauri/src/facts.rs::{EntityKnowledgeDiff, build_knowledge_diff}`
- `src-tauri/src/tools.rs::format_knowledge_diff`
- `src-tauri/src/storage/seal_store.rs` (purge in the seal tx)

**Fix**
The only writer into the fact registry is the LLM triple extractor, and `facts.rs::EXTRACT_SYSTEM` narrows it by design to short key-value attributes (*"predicate is a short, stable attribute (e.g. \"status\", \"owner\", \"deadline\", \"role\")"*, *"Only durable state worth tracking across meetings"*). The note **does** carry `## Decisions` / `## Risks & open questions`, but `grep -rl Decisions src-tauri/src` returns only prompt-authoring sites — **zero parsers**. Hence a 5-decision note legitimately reports `0 total decision(s) on record` (`format_knowledge_diff` prints `kd.ledger.len()`, which is supersessions of extracted triples).
- Deterministic pure parser returning `(section, bullet_text)` pairs.
- **Prefer a new additive table** over routing into `facts`: keys on `meeting_id`, guarded `CREATE TABLE IF NOT EXISTS` per rust-tauri §4.
- Surface as a fourth list on `EntityKnowledgeDiff` (serde camelCase → additive for the FE) and render it.

**RED-before-GREEN** — a note with 3 `## Decisions` bullets must report 3, not 0 (RED on trunk). Plus the lock test below.

**Risk — LOCK MODEL (blocking review).** Facts are purged on seal and every read is gated via the `meeting_id` anchor (`Db::list_facts_visible`). Any new decisions store **must** carry `meeting_id`, be purged in the **same atomic seal transaction**, and be read through a `visibility_clause` gate — otherwise it is a straight sealed-content leak. Second risk: routing free-text decision bullets through `reconcile_facts` would mint spurious supersessions (every re-wording under the same `(subject, predicate)` key closes the previous one → false bitemporal history). A reserved predicate namespace or the separate table avoids polluting the `status`/`owner` key space.

**Does NOT solve:** decision *quality*. If R2/R3 have not landed, this makes a wrong decision durable and queryable — which is why it is sequenced after them.

---

### PHASE 2 — payload cost

---

#### R7 · get_meeting payload budget
**Status:** **MERGED in PR #485.** The body below is the pre-merge design and
RED specification; later work must preserve these bounds.
**Defects:** #1 (surviving half), #13 (compact format), #10 (format-stamped header)
**Size:** S · **Deps:** **R1** (reuse `secs()`; land after so `TOTAL_CHARS` shifts once) · **Lock review:** no

**Touch list**
- `src-tauri/src/tools.rs::ToolCall::GetMeeting` (variant fields + arm), `format_structured_transcript`, new `format_compact_transcript`, `page_text_disclosed`, `tool_specs`
- `src-tauri/src/mcp.rs` `get_meeting` inputSchema (~:245), description (~:244), `dispatch_tool` arm, `mcp_body_window`
- in-app arg parser at `tools.rs` (~:2170)
- tests: `mcp.rs::mcp_get_meeting_transcript_format_switches` (exact-string), `tools.rs::get_meeting_note_only_on_first_window`, `get_meeting_structured_transcript_default`, `get_meeting_transcript_format_plain_is_byte_identical`

**Fix**
1. **#1a — the advertised bound is a lie for the note.** `page_text_disclosed(&full_transcript, offset, max_chars)` windows the *transcript only*; `n.markdown` is interpolated whole, so `get_meeting(id, maxChars: 200)` still ships ~19.5k. Window the note too (`NOTE (TOTAL_CHARS: N (showing 0..M))`), keeping the `(0,0)` default unwindowed so the legacy in-app path stays byte-identical.
2. **#1b — add `include_note: bool` (default `true`)**; guard becomes `Some(n) if *offset == 0 && *include_note`. Plumb from **both** parsers. Do **not** split into `get_meeting_note`/`get_meeting_transcript` tools — that doubles an 11-tool catalog and duplicates the `meeting_is_visible` gate for what one bool solves.
3. **#13 — add an opt-in `"compact"` transcriptFormat**: fold runs of consecutive same-speaker segments into `[run_start–run_end] Speaker: <joined text>`. Switch the 2-way `if transcript_format == "plain"` to a 3-way match; **default stays `structured`** — making compact the default would move every agent's offset space with no version signal.
4. **#10 — stamp the format into the header**: `TRANSCRIPT (format={transcript_format}, {h}):`, plus one sentence in both descriptions that offset/maxChars/TOTAL_CHARS live in the *currently selected* format's char space.

**RED-before-GREEN**
- `includeNote: false` at `offset == 0` returns no `NOTE:` section (RED).
- `maxChars: 200` yields a reply whose note section is ≤ ~200 chars (RED: ~19.5k).
- Compact: two consecutive `me` segments + one `others` yields **2** lines, not 3; `plain` and `structured` byte-unchanged.

**Risk** — Free-text MCP payload, not `murmur-protocol` → no client/server compat coupling. **The exact-string assertion in `mcp_get_meeting_transcript_format_switches` must be updated in the same commit.** Lock: the note still comes from `db.get_note_if_visible(mid, unlocked)` **inside** the `meeting_is_visible` `Ok(true)` arm — do not move that read out of the arm. Compact merges segment boundaries → never use a compact reply as a seek/citation source (grounding already works off typed Segments, so nothing regresses).

**Overlap warning the report missed:** R1 alone reclaims ~28k of the claimed 46k. **Re-measure #13's incremental value (~18k) against a real vault before sizing it as a win.**

---

#### R8 · Bound get_entity_dossier
**Status:** **MERGED in PR #485.** The body below is the pre-merge design and
RED specification; later work must preserve these bounds.
**Defects:** #15
**Size:** S · **Deps:** none · **Lock review:** no

**Touch list**
- `src-tauri/src/tools.rs::{ToolCall::GetEntityDossier, tools_spec}` + the three other constructors: `orchestrate.rs:212`, `voice_action.rs:858`, `tools.rs` in-app dispatch (~:2226)
- `src-tauri/src/mcp.rs` inputSchema + `dispatch_tool` `"get_entity_dossier"` arm (reuse `mcp_body_window`)
- `src-tauri/src/summarize/dossier.rs::{build_dossier_data, format_dossier_client}`

**Fix** — `build_dossier_data` does `corpus.push_str(&note.markdown)` — the **whole note body per mentioning meeting**, capped only by `format_dossier_client`'s `let budget = 200_000usize;` (~50k tokens). The paging machinery already exists (`mcp_body_window`, `MCP_DEFAULT_WINDOW_CHARS = 6000`, `page_text_disclosed`) and is applied to `get_meeting`/`get_document` — the dossier arm simply bypasses both. Add `noteDetail: none|summary|full` (default **summary**), `offset`, `maxChars`. Always emit `render_structured` (small, deterministic). `full` routes the corpus through `page_text_disclosed`, whose `TOTAL_CHARS` **is** the requested `estimatedFullChars` — no new format needed. Put the un-windowed total in the overview header regardless of mode.

**RED-before-GREEN** — a 3-meeting entity in `summary` mode returns a reply an order of magnitude shorter than the full corpus, and the disclosed total equals the un-windowed length (RED on trunk). `mcp.rs::get_entity_dossier_is_visibility_gated_and_egress_free` stays green.

**Risk** — No lock risk: every gate (`entity_is_visible`, `entity_mentions_visible`, `get_note_if_visible`, `list_facts_visible`) is untouched, and shrinking a payload can only reduce disclosure. **Deliberate default change** from full-corpus to `summary` for existing MCP clients — same class as `MCP_DEFAULT_WINDOW_CHARS`; note it in the tool description. `ToolCall` field additions are compile-error-visible across the three other constructors.

---

### PHASE 3 — composability (search and read must connect)

---

#### R9 · Chapters over MCP
**Status:** **OPEN as PR #492; rebuild with R10.** Chapter, search, and read
must share one stable segment-address space; do not merge this parallel head.
**Defects:** #8
**Size:** M · **Deps:** none · **Lock review:** **YES**

**Touch list**
- `src-tauri/src/storage/db.rs` — new gated `get_timeline_data_visible(meeting_id, unlocked)` (**`get_timeline_data` is a raw ungated `SELECT data FROM timelines` — never call it from `execute_tool`**)
- `src-tauri/src/tools.rs` — `ToolCall::GetMeetingChapters`, `tool_specs`, arm; new shared char-offset prefix-sum helper
- `src-tauri/src/summarize/timeline.rs::repair_coverage` (reuse)
- `src-tauri/src/commands/export.rs::export_canvas_inner` (the precedent: gate → `get_timeline_data` → `repair_coverage` → topic spans)
- `src-tauri/src/mcp.rs::tools_spec` + `GatedToolExecutor::run` dispatch

**Fix** — Chapters exist and are persisted (`timelines` table, `MeetingTimeline { speakers, topics }`; the FE "chapter list" is literally `meeting-timeline.component.ts::chapters` over `topics`) and are absent from all 11 MCP tools. Mirror `ToolCall::GetDocumentOutline`: gate first, on not-visible return the **same masking sentinel shape** as `get_meeting`; else parse, `repair_coverage`, render. Derive char offsets by prefix-summing `format_structured_transcript` per segment and mapping each `TopicSpan` `[startS,endS]` to first/last overlapping segment → `offsetStart`/`offsetEnd`/`charCount` — **build this helper once and share it with R10**. Add `chapter: Option<usize>` to `ToolCall::GetMeeting` resolving to the same window.

**RED-before-GREEN** — sealed-not-unlocked meeting → sentinel with **no topic label leaked** (copy `get_document_outline_tool_is_gated_and_maps_sections`). A chapter window round-trips to the same chars as an explicit `offset`/`maxChars` call.

**Risk — LOCK MODEL (blocking review).** Topic labels + speaker names are LLM-derived **content**; this is a NEW content read path and must route through `meeting_is_visible`/`visibility_clause`. `timelines.data` is blanked only while sealed — relying on blanking alone is exactly the mistake `get_meeting_detail`'s masked-DTO comment warns about. Masked-reply indistinguishability must hold.

**Availability honesty (not a bug, a UX contract):** `commands/meetings.rs::get_timeline` is READ-ONLY cached-or-empty and **never generates** (generation moved to `commands::generate_timeline`, auto-fired by the FE only for cheap cloud providers, behind a click for on-device, per the OOM fix). On-device users have no cached timeline → the tool must say so honestly, not imply the meeting has no chapters.

---

#### R10 · Segment-level search (search and read compose)
**Status:** **OPEN as PR #486; rebuild with R9.** The replacement must return
an address accepted by `get_meeting`, disclose global totals, and preserve
strict response caps.
**Defects:** #9
**Size:** M · **Deps:** **R9** (shared offset helper + per-hit chapter label) · **Lock review:** **YES**

**Touch list**
- `src-tauri/src/storage/mcp_store.rs` — new `search_segments_visible(query, meeting_id, limit, max_per_meeting, unlocked)`
- `src-tauri/src/storage/db.rs::{fts_match_query, fts_match_query_any, visibility_clause, search_visible_impl, search_snippet, excerpt}` (copy the predicate, **do not re-derive**)
- `src-tauri/src/storage/models.rs` — new `SegmentHit`
- `src-tauri/src/tools.rs` — `ToolCall::SearchTranscript`, `tool_specs`, arm; mirror `format_hit_location`
- `src-tauri/src/mcp.rs::tools_spec` + dispatch

**Fix** — The data is already there: `fts_segments` is a real FTS5 virtual table with ai/ad/au triggers, and `segments` carries `idx`, `start_s`, `end_s`, `text`, `speaker`. The reader throws it away: `search_visible_impl` `GROUP BY meeting_id` with `MIN(rank)`, then `search_snippet` re-finds the text with `… WHERE meeting_id=?1 AND text LIKE ?2 ORDER BY idx LIMIT 1` → exactly one ~130-char excerpt from the *first* matching segment, no idx/time/count. `SearchHit` has no position field and `format_hits` prints none. **The asymmetry proves the fix is expected:** document hits in the same output already carry `(§section · p.page)` via `format_hit_location`.
New tool renders `- [meeting:{id}] @{mm:ss} (seg {idx}, offset {n}) {Speaker}: {snippet}` plus `({shown} of {hitCount} matches)` per meeting.

**Cheaper partial if M is too big (S):** keep `search_meetings`' shape but thread the matching segment's `start_s` + `idx` + total match count through `SearchHit` and render them. That alone kills the blind-116k-char-paging problem.

**RED-before-GREEN** — a query matching 5 segments in one meeting returns 5 located hits with a disclosed count (RED: one unlocated snippet). Sealed-not-unlocked meeting contributes nothing. Empty query matches nothing (preserve the R1/`da63840` guard).

**Risk — LOCK MODEL (blocking review).** Raw segment text is sealed content; the new reader must carry the **identical** `visibility_clause` predicate as `search_visible_impl` — copy it. `fts_segments_au` re-indexing blanked text on seal is defense-in-depth, **not** the gate. Additive tool → no wire impact; the cheaper variant adds optional fields to a `Serialize`d `SearchHit`.

---

#### R11 · Triage columns on list_recent_meetings
**Status:** **OPEN design with no standalone PR.** Re-scope it with the
discovery/navigation replacement; #487's raw-size field is not this batch.
**Defects:** #20
**Size:** S · **Deps:** none · **Lock review:** **YES**

**Touch list**
- `src-tauri/src/storage/meetings_store.rs::list_meetings_visible` → new `list_meetings_visible_triage` (**reuse the exact predicate, do not fork the SQL**)
- `src-tauri/src/storage/models.rs` — new `MeetingTriageRow`
- `src-tauri/src/storage/graph_store.rs::{list_entities_visible, entity_mentions_visible}` (participants)
- `src-tauri/src/tools.rs::ToolCall::ListRecentMeetings` arm; `src-tauri/src/mcp.rs` description

**Fix** — The arm formats exactly `- {title} · {started_at} · {status:?} · id:{id}`. **`duration_s` and `ended_at` are already fetched and thrown away.** Add: `transcriptChars` (`SELECT COALESCE(SUM(LENGTH(s.text)),0) … WHERE s.meeting_id = m.id` — a PK-prefix range scan, cheap at the clamped `limit` 1..=100), `hasNote` (`EXISTS` — `notes` is already joined for the visibility predicate), and ≤5 participant names via the gated `entity_mentions` path. `statusDetail` has **no backing column** (`MeetingStatus` is a bare enum; `recording_store.rs`/`audio/spill.rs` set `Error` with no reason) — derive it (`Error (no transcript)` / `Error (partial transcript)`), which answers the real triage question. A real reason string needs an additive `meetings.status_detail TEXT` via `add_column_if_missing` — scope separately.

**RED-before-GREEN** — a sealed-and-not-unlocked meeting contributes **no** triage row (not a masked one). A meeting with a note and 12k of transcript renders both figures.

**Risk — LOCK MODEL (blocking review); this is why it is S, not XS.** Every added field is metadata about a meeting and must ride the same `visibility_clause`-derived predicate; forking the SQL is exactly how size/participants leak. Two traps: (a) `list_meetings_visible` keeps a meeting with **zero** notes, so one recorded into an already-sealed folder can appear — its `segments.text` is blanked on seal so `SUM(LENGTH(text))` correctly reports ~0, but participants must go through the gated mention path; (b) a per-meeting attendee list is exactly the metadata the masked DTO withholds elsewhere — never emit it for a non-visible meeting.

---

### PHASE 4 — discovery (stop dead-ending the agent)

---

#### R12 · Folder discovery + the meeting-id dead end
**Status:** **OPEN as part of PR #487; do not merge as-is.** Rebuild after
closing the sealed-folder existence oracle and discovery-after-cap gaps.
**Defects:** #21, #11
**Size:** S · **Deps:** R9 (only to name `get_meeting_chapters` in the redirect; otherwise none) · **Lock review:** **YES**

**Touch list**
- `src-tauri/src/storage/folders_store.rs` — new `list_note_folders_visible(unlocked)` wrapping `list_note_folders` (**which is ungated and returns sealed folders — the same shape as the `list_folders` trap from PR #321**)
- `src-tauri/src/storage/notes_store.rs::{get_note_folder_schema, list_notes_visible}` (reuse)
- `src-tauri/src/tools.rs` — `ToolCall::ListNoteFolders` + arm; `QueryDatabase` `Ok(None)` message; `GetDocumentOutline` empty arm
- `src-tauri/src/storage/meetings_store.rs::meeting_is_visible`, `db.get_meeting`
- `src-tauri/src/mcp.rs::tools_spec` (12th tool) + dispatch

**Fix**
1. **#21** — new `list_note_folders` tool emitting `name · <n> rows · columns: <key:kind, …>` from `list_notes_visible(Some(id), unlocked).len()` + `get_note_folder_schema(id)`, filtered to `!(locked && !unlocked.contains(id))`. Append **visible-only** names to `No note folder named "X". Available: A, B, C.`
2. **#11** — inside the **existing** `Ok(entries) if entries.is_empty()` arm only (never before the doc gate), probe `db.meeting_is_visible(id, unlocked)? && db.get_meeting(id).ok().flatten().is_some()` → redirect `"{id} is a MEETING, not a document — read it with get_meeting."`; otherwise return the existing sentinel **byte-identical**. Ids are NOT distinguishable by format (both are `uuid::Uuid::new_v4()`), only by table lookup — and `meeting_is_visible` returns **true for a nonexistent id**, which is precisely why the `get_meeting` conjunct is mandatory.

**RED-before-GREEN**
- Sealed meeting id → output **equals** the old sentinel, contains neither `"MEETING"` nor the title.
- Random uuid → old sentinel.
- Visible meeting id → the redirect.
- Sealed note folder's name appears in **neither** the tool output **nor** the not-found error; its row count and schema never surface.
- The two existing sealed-document assertions pass unchanged.

**Risk — LOCK MODEL (blocking review); this is the one place where the naive fix IS a leak.** The masked sentinel is *deliberately* indistinguishable between locked / absent / heading-less (pinned by `get_document_outline_tool_is_gated_and_maps_sections` + the `mcp.rs` twin). A type-check that answers "this is a meeting" for a **sealed** meeting newly discloses its existence — the guard ordering above closes that. Three leaks to avoid on the folder side: sealed folder **name** (MCP is a different trust boundary from the FE, and `query_database` already promises "Sealed-and-locked note folders are excluded"), sealed folder **row count** (volume/existence metadata), sealed folder **column schema** (explicitly refused today by `commands/notes.rs::get_note_folder_schema` — exposing it would contradict a shipped gate). **The "available names" error message is the easiest place to regress.**

---

#### R13 · Entity discovery + did-you-mean
**Status:** **OPEN as part of PR #487; do not merge as-is.** Its replacement
must preserve every visibility gate and operate before bounded discovery is
truncated.
**Defects:** #7 (parts 1 and 2; part 3 → R14)
**Size:** S · **Deps:** none · **Lock review:** **YES**

**Touch list**
- `src-tauri/src/tools.rs` — `ToolCall::ListEntities { query: Option<String> }` + arm + `tool_specs`; the `Ok(None)` branches of the `GetEntityDossier` / `KnowledgeDiff` arms
- `src-tauri/src/storage/db.rs::list_entities_visible` (**already gated, already mention-counted, capped at `MAX_VISIBLE_ENTITIES = 500`** — reuse, never `list_entities`)
- `src-tauri/src/mcp.rs::tools_spec` + dispatch

**Fix** — There is no entity lister among the 11 tools, and resolution is exact-token: `graph_store.rs::entities_matching_query` filters with `db.rs::name_matches_query_tokens`, which requires the entity's name tokens to appear as a **contiguous window** inside the query (`name_tokens.len() > query_tokens.len() ⇒ false`, then exact `a == b`), and additionally drops names shorter than `MIN_ENTITY_NAME_LEN = 3` — so an entity literally named "KO" is **unreachable by name from every tool, by design, forever**. Zero-hit replies are the bare `No visible entity matching "{entity}".` with no suggestions.
Add the lister; replace both bare not-found strings with a top-k suggestion list scored by prefix / initialism / edit distance over `list_entities_visible`.

**RED-before-GREEN** — `get_entity_dossier("Kong Operatorr")` returns a suggestion list containing "Kong Operator" (RED: bare not-found). A sealed-only entity's name appears in neither the lister nor the suggestions.

**Risk — LOCK MODEL (blocking review).** `entities_matching_query`'s visibility predicate (`visibility_clause("n", unlocked)` over `entity_mentions → meetings → notes`) is what stops a sealed-only entity's **name** from leaking. A did-you-mean list built from anything other than `list_entities_visible` is an existence oracle for sealed content.

**Does NOT solve:** the fragmentation itself. "Konnect" and "Connect" remain two permanent rows until R14 lands aliases + canonicalization.

---

### PHASE 5 — upstream root causes (XL / needs a real Mac)

---

#### R14 · Domain glossary program (+ entity aliases)
**Status:** **OPEN as PR #493; do not merge as-is.** Rebuild a bounded,
omission-safe glossary through the existing redaction/accounting seam, then
canonicalize whole aliases locally before persistence. The broad staged design
below predates the egress and settings-loss audit.
**Defects:** #18, #7 part 3
**Size:** **L (4 stages, ship each alone)** · **Deps:** Stage 1(d) canonicalization **before** the alias table · **Lock review:** advisory · **Egress review: MANDATORY**

**Stage 1 (M) — fix the NOTE, where the durable damage is authored. ~60% of the value, zero ASR risk, fully headless.**
- `settings/config.rs::AppConfig` — `glossary: Vec<GlossaryTerm { canonical, aliases }>`, `#[serde(default)]`, **user-authored only**. Not a DB table: it is workspace-scoped, must survive folder seals, and must never derive from sealed content.
- `summarize/template.rs::render_user_content` — emit a `CANONICAL SPELLINGS (use these exact forms)` block right after the existing `EXISTING NOTE TITLES` block (same renderer → the byte-identical-when-empty contract already used for `related_context`/`user_notes`/`live_bullets` applies verbatim).
- `summarize/template.rs` formatting rules (every `template_for_style` variant) — *preserve proper-noun spellings exactly; never introduce an abbreviation or acronym that does not appear in the transcript.* **This alone kills "Kong Operator" → "KO".**
- **`fn canonicalize(name, glossary)`** (case/diacritic-folded, longest-alias-wins) applied in `summarize/graph.rs::clean` **and** again in `commands/mod.rs::build_and_persist_entities` immediately before each `upsert_entity`. This is the real fix for #7: `graph_store.rs::upsert_entity` keys on `(name_ci, kind)` where `name_ci = name.to_lowercase()` and keeps first-seen casing; `graph.rs::clean` only trims/dedups with `eq_ignore_ascii_case`; `facts.rs::norm` is `trim().to_lowercase()`. There is **no rename/merge/alias command anywhere** (`commands/graph.rs` exposes only reads).
- Backfill: re-point `entity_mentions` from alias rows onto the canonical row — **`INSERT OR IGNORE` remap, leave the orphan row in place, never a `DELETE`** (additive-migration rule). This is the only user-row mutation in the whole program.

**Stage 2 (S) — ASR bias. Real but secondary.** Thread `Option<&str>` into `transcribe/whisper.rs::{build_params, transcribe_with}` and call `params.set_initial_prompt(...)`. The seam exists and is unused (`wake.rs` header: *"⚠️ NOT WIRED: … the Whisper `set_initial_prompt` bias …"*). **Hard budget: whisper.cpp caps the prompt at `min(n_max_text_ctx, n_text_ctx/2)` = 224 tokens** (`n_text_ctx = 448`) regardless of our `BATCH_N_MAX_TEXT_CTX = 16384` — roughly 30–50 terms, then it silently truncates. Needs an explicit cap + truncation-order rule. **Good news the report did not know:** batch transcription is already chunked (`pipeline.rs` VAD `speech_regions` → `decode_windows` with `MAX_WINDOW_S = 120`, and `transcribe_with` does `create_state()` per call), so the prompt re-seeds every ≤120 s — the "prompt decays over a long recording" objection is largely neutralized. Just as well, since `carry_initial_prompt` is **not reachable**: whisper-rs 0.16 keeps `FullParams.fp` `pub(crate)` with no setter.

**Stage 3 (S)** — `ToolCall::ListGlossary` + `mcp.rs::tools_spec` entry returning canonical→aliases so an agent can resolve a surface form. Read-only, no meeting content → **no `visibility_clause` gate needed; state that explicitly in the doc comment** so a future reviewer does not read it as an omission.

**Stage 4 (S)** — Settings list editor: component directory, signals, `mur-input` + `primitives.css`, no new deps.

**Stage 5 (M, folds in #7 part 3)** — additive `CREATE TABLE IF NOT EXISTS entity_aliases (entity_id, alias_ci, created_at, PRIMARY KEY(entity_id, alias_ci))`; widen `entities_matching_query` with a LEFT JOIN **under the identical `visibility_clause("n", unlocked)` predicate**; relax `MIN_ENTITY_NAME_LEN` **for alias hits only** (a global relaxation re-admits the noise it was added to suppress).

**RED-before-GREEN** — `canonicalize("Konnect", [{canonical:"Kong Connect", aliases:["Konnect","KO"]}])` → `"Kong Connect"`; two notes spelling it differently produce ONE entity row (RED: two). Glossary block byte-absent when the glossary is empty. Alias JOIN: a sealed-only entity is not reachable by alias.

**Risk — EGRESS is the load-bearing risk and the report ignores it.** The
glossary rides `render_user_content` to a cloud provider exactly like
`vault_titles`, which already needed a dedicated **VAULT-TITLE FIREWALL** in
`summarize/redact.rs` (design B — FILTER, not mask+restore) with a RED
regression for an `[[Example Person]].md` person-page leak. A glossary is *by
construction* the user's most sensitive internal proper nouns (codenames,
clients, unreleased projects). **`RedactingProvider` MUST be extended to filter
person-like glossary terms on the same path, with its own RED test, and the
egress ledger must account for it. Shipping Stage 1(b) without touching
`redact.rs` opens a new silent egress channel.**
**Lock:** low *if and only if* the glossary stays user-authored. Auto-deriving aliases from meeting text would make a workspace-scoped, seal-surviving artifact carry sealed-derived strings — a leak (contrast `correction_log`, which is per-meeting derived and correctly purged on seal).
**⚠️ NOT PROVABLE HEADLESS:** Stage 2 effectiveness. `set_initial_prompt` is a soft probabilistic bias with a known failure mode of hallucinating prompt terms into silence/low-confidence spans, and it interacts with the existing anti-hallucination temperature ladder (`BATCH_ENTROPY_THOLD` / `BATCH_LOGPROB_THOLD` / `BATCH_NO_SPEECH_THOLD`). **Needs a real recording on a real Mac, A/B with the prompt off.**

---

#### R15 · Dual-channel echo: render-time mitigation, then per-region arming
**Status:** **OPEN as PR #489; defer or rebuild.** Do not merge the default
heuristic: independent audit showed it can hide real short or repeated speech.
The proposal below is historical design, not a current ship recommendation.
**Defects:** #14
**Size:** S (step 1) + M (step 2) · **Deps:** R1 (shares the renderer) · **Lock review:** advisory (step 1), **warranted (step 2)**

**Step 1 (historical proposal; do not ship before real loss evidence)**
- `tools.rs::ToolCall::GetMeeting` — add `channel: 'merged'|'mic'|'system'` (default `merged`) + `mcp.rs` inputSchema.
- The original proposal added pure `tools.rs::dedup_rendered(segs)` using
  `merge.rs::{norm_tokens, token_lcs, jaccard}` and assumed that suppressing a
  rendering was non-destructive. That assumption is false at the consumption
  boundary: a heuristic can hide genuine short or repeated speech from the
  caller even when stored bytes survive. Keep merged rendering byte-identical
  by default unless real dual-stream evidence establishes an opt-in rule.

**Step 2 (M, root cause)** — `audio/align.rs::estimate_stream_offset` returns `None` (⇒ zero dedup for the **entire** meeting) when `spread > MAX_SPREAD_S` (0.2 s) between three disjoint 30 s windows. `merge.rs`'s own header states the cpal and ScreenCaptureKit clocks drift *"seconds per hour"* — so on a ~1 h recording the start-vs-end lag exceeds 0.2 s (often exceeds the ±2 s `MAX_LAG_ENV` search range entirely) and the estimator **rejects its own measurement**. `suppress_cross_stream_echo` then short-circuits: `if !leak_armed || others_refs.is_empty() { return (segments, 0); }`. **Long meetings — the ones with the most bleed — are exactly the ones guaranteed to have the deduper off.** The same `leak` also gates offline AEC in `pipeline.rs`, so on the reported recording **both were silently inert**.
Replace the global estimate with a **piecewise profile**: rolling window (e.g. 30 s every 60 s), per-window arming, drop the `MAX_SPREAD_S` all-or-nothing rejection (a drifting lag is a legitimate measurement, not a failed one), widen/drift-adjust `MAX_LAG_ENV`. `suppress_cross_stream_echo` arms per segment via `leak_at(seg.start_s)`.
**CRITICAL CONSTRAINT:** keep a `fn global_leak(&self) -> Option<EchoLeak>` reproducing today's median-plus-spread semantics **exactly** and feed *that* to `align.rs::archive_delays`, so the playback mix is bit-for-bit unchanged.

**RED-before-GREEN** — Step 1: the exact `merge.rs::no_dedup_without_leak_evidence` fixture (byte-identical text at 5.0/`others` and 5.4/`me`, both surviving with `n == 0`) must render the sentence **once**. Step 2: a new synthetic fixture with a deliberately **drifting** delay must fail on current code and pass after; the three existing `align.rs` tests (`recovers_synthetic_echo_offset`, `independent_streams_yield_none`, `archive_delays_prefers_measured_leak`) stay green.

**Risk** — Step 1 is lock-neutral (inside the gated arm, mutates no stored data, additive schema field) but **the "only drop `me`" invariant must hold or genuine user speech disappears** — the same class as the lock-security finding recorded in `ebe9068`; review it as such. **The gate was tightened deliberately** (`99632bb` "gate ALL dedup on leak evidence", `ebe9068` "relaxed-tier dedup uses order-preserving token-LCS only") after a content-loss finding — **do not simply loosen it in the stored pipeline.** Step 2 is higher risk: `estimate_stream_offset` is load-bearing for `archive_delays` (wrong padding desyncs playback — the documented 51 s→8 s class) and for AEC; recordings that get zero dedup today will start getting some, so more `me` segments will be dropped. **⚠️ Final proof needs real dual-stream recordings on a real Mac.**

**Does NOT solve — two things, and the report is right about both:**
1. **Step 1 does not improve the note.** The summarizer consumes the stored merged segments, not the MCP rendering. Only step 2 improves note quality.
2. **Divergence is unreachable by any text rule.** `[271.76] Others: "I built them."` vs `[271.85] Me: "I don't know."` is 3 tokens (below `ECHO_MIN_TOKENS = 4`) and shares no content — Whisper hallucinating on low-SNR acoustic bleed. Only acoustic cancellation or headphones fixes it.

---

#### R17 · Product decision: default diarization ON
**Status:** **OPEN as part of PR #494; rebuild only after the safe #493
replacement.** Keep the default OFF until real DER/EER, model-download, and RAM
evidence exists; the current head also fails to wire grounding through Rust.
**Defects:** #2(a) — the reason the reporter saw Me/Others at all
**Size:** XS code, **XL to verify** · **Deps:** R1 (else enabling it makes MCP worse), R2 (attribution contract keys off lane shape) · **Lock review:** no

`settings/config.rs` defaults `diarize_others: false` and `voiceprint_enabled: false`, and nothing prompts the user — a fresh install **never** diarizes. Flipping the default (or adding a one-time prompt when a system stream is present) is a one-line change but carries: a ~40 MB first-use model download (`transcribe/model.rs::ensure_diarization_models`), a second ML runtime load (mitigated — the diarizer loads lazily only after Whisper is dropped, never co-resident), and interaction with the known recording-time RAM pressure.
**⚠️ Cannot be validated headless.** Requires a real multi-speaker recording on a real Mac, with an RSS-over-time measurement, before flipping. **This is the single highest-leverage fix for #3/#16 quality — every downstream mitigation is second-best to it.** Treat R2's collapsed-lane directive as the safe interim.

---

### PHASE 6 — calibration-dependent

---

#### R16 · org_search: keyword floor, exposed score, boilerplate
**Status:** **OPEN as PR #488; do not merge as-is.** Rebuild only the
SQLite/FTS exact-token coverage floor (strict AND, then OR requiring
`ceil(unique_terms/2)` before ordering and limit). Defer raw scores and
boilerplate heuristics; the broader proposal below is pre-audit provenance.
**Defects:** #19 (surviving three-quarters)
**Size:** M · **Deps:** none · **Lock review:** no

**Touch list**
- `src-tauri/src/storage/models.rs::OrgChunkHit` (+ `score: f32`)
- `src-tauri/src/storage/org_store.rs::{search_org_chunks_knn, search_org_chunks_fts}`
- `src-tauri/src/embed.rs::{fuse_org_hits, ORG_KNN_SEARCH_COSINE_FLOOR}` (+ new `ORG_REL_FLOOR`)
- `src-tauri/src/tools.rs::{search_org_brain_hits, format_org_hits, gather_note_enhance_citations}` (shares the reader)
- `src-tauri/src/mcp.rs` tool descriptions + `mcp.rs::tool_catalog_nudges_org_search_as_a_fallback`

**Fix**
1. **No floor on the keyword leg.** `search_org_chunks_fts` orders by `bm25(...) ASC LIMIT ?2` with no minimum — and `83b40c2` *added* the AND→OR fallback (`fts_match_query_any`, stopwords + <3-char tokens dropped). The reported 7-token query is near-certain to fire the OR leg, so **any** org note containing "kong" matches with no floor. **The reported symptom plausibly still reproduces, now via FTS rather than KNN.** Add a *relative* cut in `search_org_brain_hits` (`score >= TOP_SCORE * ORG_REL_FLOOR`) — absolute bm25 is corpus-dependent — plus gate the OR fallback behind a minimum matched-content-word ratio.
2. **No score anywhere.** `OrgChunkHit` is `{item_id, author_hint, title, snippet, content_sha256}`, and `fuse_org_hits` computes an RRF score then **discards it** (`filter_map(|(id, _score)| …)`). Carry it through and render it (`- [org · author] (0.61) title — snippet`) so an agent can self-triage.
3. **Boilerplate filter** (`grep -rn boilerplate src-tauri/src` → zero hits): index-time chunk-quality heuristic. **Ship as a separate follow-up** — riskier than 1–2, do not couple.

**RED-before-GREEN** — seed one on-topic item + one whose body is UI chrome; the chrome item must be absent for the multi-token query (RED on trunk).

**Risk** — No lock model (org items live outside the folder-lock domain; keep the `tombstoned = 0 AND os.context_enabled = 1` predicate untouched). **Real risk is recall regression** — a floor that is too aggressive silently hides legitimate colleague notes, and the existing 0.78 KNN floor is itself **PROVISIONAL and uncalibrated** (per MEMORY). `OrgChunkHit`'s shape change also touches `gather_note_enhance_citations` (the Notes find-related path). **⚠️ Needs a real org corpus to calibrate — do not tune the constants headless.** The doc-nudge complaint is accurate and *enforced by a test* (`tool_catalog_nudges_org_search_as_a_fallback`): revisit the nudge only after the floor lands.

---

## 3. Dependency graph

The graph is retained as design provenance. Edges labelled **LANDED** were
satisfied by #480–#485; they are not queued work.

```
[LANDED] R1 ──────────► R7 (#485)
R1 (#481) ────────────► R15 step1 (open/defer)
R1 (#481) ────────────► R17 (open; diarization ON requires renderer truth)

[LANDED] R2 (#480) ───► R3 (#483)
R2/R3 (landed) ───────► R6 (#491 open; rebuild required)
R2 (#480) ────────────► R17 (#494 open; rebuild required)

[LANDED SOURCE] R4 (#482) ──► R6 (#491 open; rebuild required)

R9 (chapters) ──► R10 (shared char-offset prefix-sum helper + per-hit chapter label)
             └──► R12 (redirect can name get_meeting_chapters; optional)

R14 Stage 1(d) canonicalize ──► R14 Stage 5 aliases ──► R13 did-you-mean gets materially better
R14 Stage 1(b) glossary-in-prompt ──► MUST NOT LAND without redact.rs firewall extension

[LANDED] R5 (#484): read-time normalization and its coordinated readers shipped.
```

---

## 4. Lock-security review required (blocking)

| Batch | Why | The indistinguishability rule to hold |
|---|---|---|
| **R6** | new decisions store | `meeting_id`-anchored, purged in the **same atomic seal tx**, read via `visibility_clause` |
| **R9** | topic labels + speaker names are LLM-derived content; `get_timeline_data` is ungated | sealed → the **same sentinel** as `get_meeting`, no label leaks |
| **R10** | raw segment text | copy `search_visible_impl`'s predicate verbatim; empty-query guard preserved |
| **R11** | size/participants are metadata about a possibly-sealed meeting | sealed → **no row at all**, not a masked one |
| **R12** | **highest leak risk in the program** | sealed meeting id → byte-identical old sentinel; sealed folder name/rowcount/schema in neither output nor error |
| **R13** | entity names are an existence oracle | suggestions sourced only from `list_entities_visible` |
| **R14 St.5** | alias JOIN | identical `visibility_clause("n", unlocked)`; `MIN_ENTITY_NAME_LEN` relaxed for alias rows only |

Advisory (content read path touched, not a new gate): **R15 step 1**, **R7**.

---

## 5. Cannot be verified headless — say so plainly

| Item | Why | The honest bar |
|---|---|---|
| **R14 Stage 2** (whisper `set_initial_prompt`) | soft probabilistic bias; interacts with the temperature ladder; known hallucinate-into-silence failure mode | real recording, real Mac, A/B with the prompt off |
| **R15 step 2** (piecewise alignment) | clock drift is a property of real cpal↔ScreenCaptureKit capture; `archive_delays` desync is audible, not assertable | real dual-stream recordings ≥1 h; synthetic drift fixture for RED, real recording for the verdict |
| **R17** (diarization default ON) | model download, second ML runtime, RAM interaction with the known recording-time pressure | real multi-speaker recording + RSS-over-time on a real Mac |
| **R16** (org thresholds) | bm25 is corpus-dependent; the existing 0.78 KNN floor is itself uncalibrated | a real org/vault corpus; do not tune constants headless |
| **R5** due-date coverage | depends on how people actually write dates | real vault sample before scoping NL date parsing |

The deterministic portions of the remaining batches are checkable with
`cargo test --lib` plus the applicable Angular gates. Those checks do not by
themselves prove runtime composition, content preservation, lock
indistinguishability, or real-model quality; the exact-diff reviewer,
lock-security specialist, and the real-Mac bars above remain separate evidence.

---

## 6. What this program does NOT fix

1. **Who said what.** R2 removes false name attribution; it does not add attribution. Only R17 + voiceprints do.
2. **The note still sees duplicated speech.** R15 step 1 cleans the *MCP rendering* only — the summarizer consumes stored segments. Note quality improves only with R15 step 2.
3. **Whisper divergence hallucinations** (`"I built them."` / `"I don't know."`) are unreachable by any text-similarity rule. Acoustic cancellation or headphones.
4. **Existing junk facts** survive R4's guard. They need `forget_entity_fact` (in R4) plus a manual sweep; there is no automatic repair.
5. **Existing fragmented entities** ("Konnect" vs "Connect") survive R13; only R14's canonicalize + backfill merges them, and the backfill is remap-only — orphan rows remain by design.
6. **R7's compact format is worth ~18k, not ~46k**, once R1 has landed. Re-measure before selling it.
7. **`## Decisions` correctness** is bounded by R2/R3. R6 makes decisions queryable; it does not make them true.
8. **`me` stays ambiguous** across users until `user_display_name` is actually set; R5 deliberately refuses to merge rather than merge wrongly.
