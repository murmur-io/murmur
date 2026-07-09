<!-- Generated 2026-07-05 via /research (murmur-researcher fan-out, 3 angles). Pricing/competitor facts = point-in-time mid-2026. -->
# Research: Connector-driven note enrichment (fold live Jira/Slack context INTO the note)

## Scope note (2026-07-05, user clarification)

**The vision is EVERY connector, not Jira.** Enrichment must be a **connector-agnostic engine over the `ConnectorRegistry`** — any consented connector (Jira, Slack, web, calendar, and future Linear/ClickUp/Drive) can contribute a `> [!context] (via <source>)` block. The architecture already supports this: the `Connector` trait (`connectors/mod.rs`) is generic (`search` + optional `lookup`), the registry dispatches to any registered connector, and redaction/consent/ledger/`source_label`/`egress_attribution` are all per-connector and generic. Jira is only sequenced FIRST because Verify-with-Jira already ships — NOT because the design is Jira-specific. Build the engine source-agnostic from day one (matches the standing "connectors = live source-agnostic tools" decision).

**The one axis that matters for a generic engine — connectors split into two enrichment modes:**
- **Identifier lookup** (Jira `lookup(PROJ-123)`): a strict identifier already *in the note* → deterministic, high-precision, tiny egress, no LLM, low noise. This is the safe, high-signal mode.
- **Free-text search** (Slack/web `search(note-derived query)`): a query derived from the meeting → fuzzy relevance, larger redacted-egress surface, note-spam risk. Higher-risk mode.
- **Local** (calendar, `EgressClass::Local`): zero egress, but still the async path.

The write path, lock gate, consent, redaction, ledger, and `> [!context] (via …)` callout are **identical and generic** across all connectors (`apply_context_markers` is already `source`-parameterized). Only the *candidate-detection + noise/privacy policy* differs by mode: lean into identifier-lookup enrichment (precise), gate free-text-search enrichment harder (show the query, tighter relevance threshold, caps).

## TL;DR / Verdict

**The idea is sound and ~90% already built — but the AUTOMATIC "enrich every note on finalize" trigger the user imagined is the wrong first move; build the on-demand, review-first, dated, CONNECTOR-AGNOSTIC version instead.**

Three things all three research angles agree on:
1. **Jira enrichment already ships as "Verify-with-Jira"** (0.7.5 / PR #214): extract issue keys → live `jira_lookup` → append `> ✓ PROJ-1 · Status: Done (via Jira)` into the note, persisted + re-exported, gated + ledgered. The user's Jira ask is a cosmetic reframe of that marker into a `> [!context]` callout.
2. **Use Verify's write path (`upsert_note` → DB canonical → seals with the note), NOT Re-Truth's** — Re-Truth writes the vault `.md` only (`overwrite_note`, never `upsert_note`), so its callout is **not** in `notes.markdown` and is dropped on seal. Copying Re-Truth's path would silently lose the enrichment on lock. (This trap is the single most important finding.)
3. **Do NOT auto-fire connector calls on note-finalize.** It trades away the literal "nothing leaves unless you ask" promise (one-time consent replaces per-action), re-imports the staleness-inverts-verify trap the team already reasoned away, courts the #1 documented AI-notes complaint (verbosity/clutter), and adds N blocking external calls to the finalize path — for a thin delta over one click. **No competitor auto-injects live ticket/Slack context into the note body** (they push OUT, or enrich the Q&A answer, not the document).

**Recommendation:** build one **connector-agnostic enrichment engine over the registry** — on-demand + review-first + dated snapshot + append-only `> [!context] (via <source>)`. Sequence by RISK, not by connector identity: **identifier-lookup enrichment first** (Jira keys — already 90% shipped as Verify; the highest-precision, lowest-egress mode), then **free-text-search enrichment** (Slack/web — gated harder). Add a **zero-egress candidate-detector card** (Re-Truth shape) as the differentiated follow-on. **Before deepening at all, dogfood the shipped Verify on a real workspace** — we don't yet know if users keep connector markers.

## Co już mamy (z repo)

- **Verify-with-Jira = Jira enrichment, minus the callout styling.** `verify_note_sources_inner` (`commands.rs:1234`, `meeting_is_unlocked`-gated `:1238`) extracts keys (`verify.rs::extract_issue_keys`, `MAX_KEYS=10`), calls `registry.jira_lookup` per key, pure `judge()` verdict (LLM never judges); `apply_note_verify_markers_inner` (`commands.rs:1288`, gated `:1293`) writes via `apply_verify_markers` → **`upsert_note` (DB canonical, `:1314`)** → `overwrite_note` re-export only if `exported_path` set (`:1324`). FE: `features/detail/verify-panel/verify-panel.component.ts`.
- **`apply_verify_markers` (`verify.rs:187`)** — idempotent strip-then-append (`is_verify_marker` owns the `(via …)` fence), byte-preserving every original line. Unit-proven idempotency + non-destruction (`verify.rs:276,296,304`).
- **Re-Truth = the review-card + byte-exact-undo UX** — `preview/apply/undo_supersessions` (`commands.rs:~2698/2753`), append-only `append_under_section` under a managed `## Re-Truth updates` heading (`obsidian.rs:398/407/444`), durable pristine pre-image undo. **BUT writes vault-file-only (`overwrite_note`, never `upsert_note`)** → dropped on seal. FE hides-on-empty card: `features/record/re-truth-card/`.
- **ConnectorRegistry (`connectors/mod.rs`)** — every outbound query redacted (regex + on-device NER, `:247`) BEFORE the connector sees it; exactly one **content-free egress-ledger row** per attempt (`:257`, truthful `egress_attribution`); fail-closed unless `enabled && consented && key-in-Keychain`; 20s HTTP timeout (`:45`). Jira `jira_lookup` → tight `IssueSnapshot {key,summary,status,due,url}`. Slack = free-text `search.messages` (`xoxp-`, `search:read`, `count=8`, `SNIPPET_MAX=300`).
- **Seal truth** — `lock_folder_inner` encrypts `n.markdown` (the DB column) with verify-before-destroy (`commands.rs:6989`); `seal_note` sets `content_blob`, `markdown=''` (`db.rs:3905`). So **enrichment in `notes.markdown` seals for free — no new table, no new purge path** (the opposite of Re-Truth, which needed `supersessions` wired into 3 purge paths).
- **Prior team decisions:** `docs/research/2026-07-05-connectors-live-vs-rag.md` already chose connectors **live, not vectorized** (a persisted stale copy **inverts verify**), option C = pin-to-note, option D = on-demand verify. `docs/research/2026-07-05-competing-with-clickup-brain.md`: sequence Jira→Slack→verify (shipped) → Linear + dogfooding.

## Findings (per angle)

### A. Architecture fit — build it as generalized Verify, not copied Re-Truth (confidence: high)
- **F1** Jira enrichment is ~90% shipped as Verify; the new surface is Slack + the trigger UX.
- **F2 (load-bearing)** Verify persists to DB (`upsert_note`, seals with the note); Re-Truth persists vault-only (dropped on seal). Enrichment is note content → **must use Verify's DB-persist shape.**
- **F3** Slack enrichment egresses **free text** (note-derived query), a materially larger privacy surface than Jira's bare validated key. Show the derived query before egress.
- **F4** Single-line callout `> [!context] … (via Jira/Slack)` reuses the existing line-strip undo; a multi-line block needs a managed-section strip. Either gives strip-based undo with **no pre-image machinery** (simpler than Re-Truth).
- **F5** Because enrichment is note markdown, it inherits every gate for free (seal/blank/restore/read-gate) — no new leak/loss path.

### B. Trigger/UX/prior art — on-demand, never auto (confidence: high)
- **Industry consensus = user-triggered pull-in.** Fellow: `/jira` slash-command + user-confirmed AI suggestion, **explicitly does not auto-inject**. Granola/Fireflies/Circleback push OUT (create tickets). Notion/Linear/Slack = user-pasted live-syncing embeds. Notion AI/Glean enrich the **answer, not the document**. **Nobody auto-appends live context to the note body** — the market already rejected it. [5][7][8][9]
- **Staleness fix everyone uses = a live-refreshing embed, which an inert `.md` structurally cannot be.** So the only honest options are **idempotent re-enrich + "as of <ts>" timestamp** (what `apply_verify_markers` already does) — lean into dated snapshots, don't fake "live". [7][8][9]
- **Verbosity/clutter is the #1 documented AI-notes complaint** (Otter "overly verbose"; users left for Granola's concision) → argues for TIGHT blocks + default-to-silence. Obsidian **foldable `> [!context]-` (collapsed by default)** is the native noise-control primitive. [10][11]
- UX: **on-demand "Enrich" button (clone verify-panel) first; then a zero-egress candidate-detector card (clone re-truth-card, hides-on-empty)** — detection is free/local, egress+write require a click.

### C. Cost/privacy/worth-it — defer the auto trigger (confidence: high)
- **Review-first gates the WRITE, not the EGRESS.** For Re-Truth the supersession is computed from *local* facts (zero egress) so review-first gates everything; a *connector* fetch has already left the Mac by the time the user reviews. You cannot have both "automatic/no-per-action-ask" AND "egress-gated-by-review". Auto ⇒ standing consent replaces per-action consent — a real weakening of the promise.
- **Latency:** Verify loops keys **sequentially** awaiting each 20s-timeout `jira_lookup` (`commands.rs:1261`) → up to ~200s worst case; must be async-after-note-shown + parallelized, never blocking note availability.
- **Jira enrichment needs no LLM** (pure `judge()` + string-format an `IssueSnapshot`); Slack-thread ranking optionally does → another reason the two sub-features aren't one.
- **The auto version re-imports the staleness trap** the team rejected (`connectors-live-vs-rag.md`): "In Progress" baked into a note nobody re-runs. On-demand verify is self-correcting (idempotent, re-runnable); a passive auto-block is not. **Mitigation: date it as a snapshot, never as a live truth claim.**
- **Note-spam line = precision vs recall.** Explicit `PROJ-123` → status = high-precision, earns its place. Meeting-topic → "8 maybe-relevant Slack messages" = low-signal bloat. Jira-key enrichment is safe; fuzzy Slack enrichment is where it becomes spam.
- **Worth-it:** the delta of AUTO over the shipped on-demand Verify is thin (live-ask already works; Verify already persists on demand). The genuine wedge — **user-initiated persistence of high-precision DATED connector snapshots into owned, offline, searchable notes** — is real and moat-aligned, and is delivered by Verify + a pin-to-note action. The automatic firing is the part that adds egress/staleness/clutter for marginal gain.

## Fit z ograniczeniami Murmur

| Constraint | Verdict |
|---|---|
| Local-first / "nothing leaves unless you ask" | **Fits on-demand; STRAINS if auto** (one-time replaces per-action consent). Keep per-action ask; if auto ever built → separate default-OFF opt-in + posture downgrade + loud ledger. |
| Obsidian-native / owned files | **Fits** — append-only `> [!context]-` callout, prose byte-preserved; the actual wedge. |
| SQLite canonical | **Fits with discipline** — persist via `upsert_note` (DB), connector hits RAM-only, no new at-rest external store. |
| Provider seam + redaction | **Fits** — rides `ConnectorRegistry::search` (redact + ledger) unchanged; v1 needs no LLM. |
| Lock model | **Fits with discipline** — write `meeting_is_unlocked`-gated; enrichment seals with the note (verify-before-destroy) since it's in `notes.markdown`. |
| macOS / CI honesty | **Partial** — the pure marker function is headless-testable RED-before-GREEN; real Jira/Slack round-trips + detector precision + "which Slack thread" need a signed build + real workspaces. |

## Opcje i tradeoffy

- **Option A — On-demand "Add live context" button (generalized Verify). Effort: S–M.** `apply_context_markers` (source-parameterized, idempotent, byte-preserving) + `enrich_note_context_inner`/`apply_note_context_inner` (clone Verify's gates, `upsert_note` + re-export). Jira keys first. Industry-aligned, zero auto-egress, lowest risk. **Captures most of the value.**
- **Option B — Zero-egress candidate-detector card (Re-Truth shape) + Option A. Effort: M.** Local scan detects Jira keys / connector-mapped `[[entities]]`; if a connector is consented, surface a dismissible card ("3 tickets referenced — pull current status? (1 Jira call)"); accept → egress → preview → append/Undo. **The differentiated version.** Risk = detector precision (mitigated by default-to-silence + dismissible + per-note "don't ask again").
- **Option C — Auto-enrich on note-finalize. Effort: M. DEFER/REJECT for now.** Standing auto-egress + staleness + clutter for a thin delta. Only revisit if users explicitly ask, and even then: Jira-keys-only, dated snapshots, separate default-OFF opt-in, non-blocking async, posture-downgraded, ledgered, write-gated.
- **Slack enrichment** = a separate increment on top of A (free-text `registry.search("slack", q)`), carefully scoped (show query pre-egress, caps, dedup) — higher privacy + noise risk than Jira.

## Rekomendacja i pierwszy krok

**Ship A, spec B, defer C. Slack after Jira. Never auto-on-finalize (for now).** Unify enrichment with Verify: both are faces of one "live context" callout — verify *compares a claim* to live truth; enrich *appends context that wasn't claimed*. Share the marker-fence, idempotency, gating, and ledger.

**Noise-control rules to bake into the writer:** one consolidated collapsed `> [!context]-` callout under a stable heading (not one-per-reference); idempotent strip-and-reapply (re-enrich replaces, never stacks); default to silence below a relevance threshold (a real Jira key / connector-mapped `[[entity]]`); caps + dedup (≤5 tickets, ≤3 Slack hits, dedup by key/permalink); honest "as of <ts>" timestamp; prose never touched.

**Smallest verifiable first slice (headless, zero network/credentials):** the pure `apply_context_markers(note_md, hits) -> String` — appends ONE collapsed `> [!context]-` callout, idempotent (strip-and-reapply its own fence), byte-preserving every original line. RED-before-GREEN on idempotency + non-destruction (mirror `verify.rs:276/296`), **plus a seal round-trip test** that a note carrying a `[!context]` block seals → `content_blob` → restores byte-identical (reuse `db.rs:2672` pattern — this is the exact thing Re-Truth's vault-only path would fail). That one test captures the entire formatting + lock risk.

**But the honest zeroth step (cheapest de-risk):** dogfood the **already-shipped Verify-with-Jira** on a real Jira workspace on a signed build and measure (a) key-extraction accuracy on real ticket text, (b) how often users KEEP the markers, (c) how many note lines even have a verifiable connector counterpart. If users keep them → spec B. Only if they then say "do this automatically" does C earn a spike.

## Otwarte pytania / czego nie udało się zweryfikować

- **Do users want enrich (append context) vs just verify (check a claim)?** Unvalidated; verify may be the higher-value sibling. Needs dogfood, not headless.
- **Detector precision on a real vault** (false-candidate → card annoyance) + **which Slack thread is "the relevant one"** — search-quality questions, not headless-provable; need a real Obsidian vault + real Slack workspace.
- **Is Re-Truth's vault-only callout intended or a latent loss-on-seal?** Verified it never calls `upsert_note` and `seal_note` seals the DB markdown → the `[!superseded]` callout isn't in the seal blob. Deliberate (cross-note pointer not preserved into an encrypted island) vs gap = a separate audit; not a blocker here, but it's WHY enrichment must use Verify's DB-persist shape.
- **Real connector OAuth round-trips, rate limits, prompt-injection on fetched ticket/message text** — deterministic `judge()` neutralizes injection for verify; an LLM-summarized Slack block would re-open it. Needs real workspaces + a red-team harness (recorded).
- Competitor facts are point-in-time (mid-2026), vendor/marketing-sourced; directionally cross-confirmed but treat exact claims as claims.

## Sources

**Code (this repo):**
- `src-tauri/src/connectors/mod.rs` — `ConnectorRegistry::{search:240, jira_lookup:278}` (redact-before-egress + content-free ledger + fail-closed), 20s `http_client:45`, `egress_attribution`.
- `src-tauri/src/verify.rs` — `extract_issue_keys:60`, pure `judge:163`, `apply_verify_markers:187` (idempotent/byte-preserving), `is_verify_marker:51`, `MAX_KEYS:48`; tests `:276,296,304`.
- `src-tauri/src/commands.rs` — `verify_note_sources_inner:1234` (gate `:1238`), `apply_note_verify_markers_inner:1288` (gate `:1293`, `upsert_note:1314`, re-export `:1324`), `apply_supersessions_inner:2753` (vault-only `overwrite_note:2846`, **no `upsert_note`**), `source_is_stampable:2649`, `persist_facts_for_meeting:2416`.
- `src-tauri/src/export/obsidian.rs` — `append_under_section:444` / `append_supersession_callout:407` (append-only, idempotent, managed heading `:398`).
- `src-tauri/src/connectors/slack.rs:39,108` (free-text `search.messages`), `connectors/jira.rs:47,159` (`from_config_if_available`, `lookup`→`IssueSnapshot`).
- `src-tauri/src/storage/db.rs:3905,3916` (`seal_note`/`restore_note_markdown`), `commands.rs:6989` (seal encrypts `n.markdown`), `db.rs:2672` (seal round-trip test pattern).
- `src-tauri/src/proactive.rs:10` (contract D1 zero-egress — enrichment egresses, can never live there).
- FE: `src/app/features/detail/verify-panel/verify-panel.component.ts`, `src/app/features/record/re-truth-card/re-truth-card.component.ts`, `src/app/core/ipc.service.ts:187,192`.
- `docs/research/2026-07-05-connectors-live-vs-rag.md`, `docs/research/2026-07-05-competing-with-clickup-brain.md`.

**Web (point-in-time mid-2026):**
- [5] https://fellow.ai/blog/fellow-jira-integration/ — Fellow `/jira` slash-command + user-confirmed, does not auto-inject.
- [7] https://linear.app/integrations/notion — Linear→Notion "paste as preview" live-syncing embed.
- [8] https://api.slack.com/reference/messaging/link-unfurling — Slack unfurl.
- [9] https://www.notion.com/help/guides/notion-api-link-previews-feature — Notion live synced link previews.
- [10] https://circleback.ai/compare/granola-vs-otter-ai — Otter "overly verbose"; Granola concision.
- [11] https://www.obsibrain.com/blog/obsidian-callouts-complete-guide-syntax-and-customization — foldable `[!type]-` collapsed-by-default = clutter-free metadata.
- [1] https://www.granola.ai/blog/granola-integrations-complete-guide-connecting-meeting-tools · [3] https://techcrunch.com/2025/04/23/ai-powered-notetaker-fireflies-ai-release-mini-apps-to-extract-insights-out-of-meeting-notes/ · [4] https://circleback.ai/how-to/best-ai-meeting-notes-software-with-crm-integrations — push-OUT patterns.
