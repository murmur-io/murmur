# Unreleased-changes audit — v0.8.0 → HEAD + working tree (2026-07-11)

Full adversarial audit of everything merged since the shipped v0.8.0 tag (56 commits, PRs
#221–#241, ~60k inserted lines across 198 files) plus the uncommitted working tree
(note-editor rework + new `note-selection-toolbar` + e2e edits). 17 review units, every
critical/major finding independently re-verified against HEAD by a refuting agent
(33 agents total). Gates were run for real. **Analysis only — nothing was fixed.**

## Gate results (run on the actual tree, working-tree changes included)

| Gate | Result |
| --- | --- |
| `cargo test --lib` | **GREEN** — 1466 passed, 0 failed, 10 ignored |
| `npx ng lint` | **GREEN** |
| `npx ng build` | **GREEN** (1 warn: `library.component.scss` 12.75 kB > 12 kB warn budget; error budget 16 kB) |
| `npm run test:e2e` | **RED — 2 failed / 33 passed** (both explained below) |

### Red gate 1 — `e2e/detail/timeline-defer.spec.ts` (committed code, PR #234)
Not "timeline fires on Note tab" — worse: after opening the Audio tab the
`_timelineOnAudioTab` effect enters an **infinite retry loop** (`__tlCalls` reached 4627 in
5 s). Mechanism: the #234 split of `get_timeline` (read) vs `generate_timeline` (heavy
derive) added `deriveTimeline`, which does `this.timeline.set(tl)` without validating `tl`.
In the harness the demo mock resolves unmocked `generate_timeline` to `null` →
`timeline` stays falsy, `timelineError` never set → the effect guard
(`!timeline() && !timelineError() && !timelineNeedsGeneration()`) re-fires forever.
In production Rust always returns an object or throws, so the prod risk is latent
(any falsy resolution loops forever), but the committed P0.1 OOM regression pin is broken
on trunk — a red gate.

### Red gate 2 — `e2e/notes/brain-popover.spec.ts` (uncommitted working tree)
Matches verified finding WT-F1 below: the new selection bubble re-floats after Accept.

## Verified CRITICAL findings (both real at HEAD; PR #228 notes feature)

1. **NOTES-1 — `delete_note_folder` permanently deletes every authored note while the UI
   promises they reparent to the default folder.** `delete_folder_inner` reparents only
   MEETING rows (`notes` table); `documents(kind='note')` rows are purged
   (`purge_doc_chunks_tx` + `DELETE FROM documents WHERE folder_id = ?1`). FE copy +
   `NotesService.deleteFolder` promise a move. **User data loss behind a reassuring dialog.**

2. **NOTES-2 — `move_note_folder` / `rename_note_folder` strand `documents.exported_path`;
   a later lock leaves the real plaintext `.md` on disk while the folder is sealed.**
   `reparent_note_folder_paths` physically renames the vault dir but the DB keeps the stale
   `exported_path`; `lock_folder_inner`'s .md cleanup deletes the stale path (nothing) and
   the live plaintext file survives at rest. **Sealed-content leak on disk.**

## Verified MAJOR findings (all re-confirmed real at HEAD)

### Lock model — the #229 seal-on-write discipline has three uncovered write paths
- **SEAM-F1 (#234)** — `generate_timeline` plaintext-upserts via `Db::set_timeline_data`
  with no `lifecycle_guard`, no seal-epoch re-check across the provider `.await`, and no
  sealed variant: a timeline generated for a session-unlocked locked meeting is
  **permanent plaintext at rest after relock**.
- **SEAM-F2** — `rename_speaker` in a session-unlocked locked folder plaintext-upserts the
  renamed timeline without reseal → **the rename is silently destroyed on relock**
  (relock re-blanks from the old sealed blob).
- **LOCK-SHARE-INGEST-1 (#229/#231 gap)** — `ingest_shared_note` (share accept) writes a raw
  `Db::upsert_note` + plaintext `.md` into a session-unlocked LOCKED target folder,
  bypassing `upsert_note_reseal_if_locked` (used at its 6 other call sites) → plaintext row
  + file **survive every relock at rest**.

### Shared Brain (#237) — `implementsIntent: FALSE` (only unit to fail intent)
- **SB-1 — the OCK grant signature can never verify cross-member/cross-session:** the wrap
  sites sign `recipient_acct_id` = server account id (the login email) while
  `acquire_org_ock` reconstructs the signed view with the key fingerprint (and a mismatched
  generation). **Members cannot decrypt the org feed; the owner loses the OCK after
  restart.** The replicated-brain promise works only inside the owner's creating session.
- **SB-2 — "org provenance chips in Ask" are unwired:** `SourceOrigin::org()` has zero call
  sites; every `VaultSource` sets `origin: None`; `OrgItemSummary` / `Db::org_item_count`
  are dead code. Org content reaches the model only as Tier-3 tool text.
- **SB-3 — `org_sweep_pending` retry amplification:** every failed retry inserts a fresh
  `org_shares` row and keeps the old one (old row marked revoked only on `is_ok()`) →
  row growth per retry tick + duplicate publishes on recovery.
- Also (intent gaps): `org_leave` never purges the decrypted `org_items`/`org_chunks`
  replica and MCP `org_search` stays advertised/dispatched with no org/consent gate —
  a departed user keeps colleagues' content locally searchable indefinitely.

### Transcription defaults (#233) — fresh-install regression
- **TP-F1 — fresh turbo-default install: first recording has a recorder but NO live loop,
  yet `begin_voice_command` still arms** — `start_recording`'s heavy-model arm sets
  `live_model=None` and only fires `spawn_live_pin_download` (which never spawns the loop
  mid-meeting). The shipped fresh-install default (≥12 GB → `large-v3-turbo-q8_0`,
  pinned `small` absent) hits exactly this: **the Ask-AI voice flow wedges with no consumer
  and no backstop on the exact target population.** (isRegression: true)

### Lock × shares (#236) — the closed hole is only half-closed
- **PK-F1 — `NotesHomeComponent.lockFolder` bypasses the lock×shares dialog:** it calls
  `FoldersService.lock` → `lock_folder` directly with no `folderActiveShares` probe; only
  `FolderRowComponent.onLock` got the warn/revoke flow, and the backend performs no share
  check. Locking a shared note-folder from the Notes section still leaves shares live.

### Memory L2 (#224)
- **MEM-1 — memory import "delete to undo" is partial:** a superseding import emits
  `FactOp::Invalidate` on pre-existing user facts anchored to OTHER meetings;
  `delete_meeting → purge_user_facts_tx` deletes only rows with the synthetic import
  meeting id → **pre-existing facts stay permanently closed after the undo**.

### Brain sidecar (#238)
- **BS-1 — undisclosed serialization + reload regression:** one process-global mutex
  serializes ALL on-device generations (held across the entire generation), and light↔heavy
  alternation kills + fully reloads the multi-GB model each swap. The old `model_cache`
  held both resident and generated concurrently. Latency/heat regression the PR body does
  not disclose (the RAM win is real and was the goal — but the trade should be documented
  and ideally the deadline should start before the lock wait).

### Working tree (uncommitted — never gated)
- **WT-F1 — post-Accept dismiss of the selection bubble is undone by the queued textarea
  `select` event:** `applyEdit` → `replaceRange` ends with
  `setSelectionRange(start, start+len)` + `focus()`, which queues a `select` event;
  `(select)="onBodySelect()"` re-floats the bubble after `clearSelection()` already ran
  (the null-`sel()` guard can't help). This is exactly the red
  `brain-popover.spec.ts` assertion. Also: `codeblock` (wrap-selection) and `divider`
  FormatOps became UI-unreachable dead code after the toolbar removal.

## Intent verdicts per unit

| Unit (PRs) | Implements intent? | Notes |
| --- | --- | --- |
| brain-p0-eval (#221/#222) | YES | honest scoping; 6 minors/nits |
| retrieval-l1-fusion (#223/#235) | YES | #235 empty-leg fix measurably a no-op on real vault (disclosed) |
| memory-l2 (#224) | YES | but MEM-1 breaks the undo claim; consolidation has no UI off-switch |
| orchestration-l3 (#225) | YES | 3 new flags have zero FE/DTO surface (deliberate); grammar path no-op today |
| live-l4 + agents-l5 (#226/#227) | YES | MCP server config UI absent (disclosed); HTTP body uncapped |
| notes-feature (#228) | YES* | *core yes, but NOTES-1/2/3 break folder-lifecycle claims |
| lock-reseal (#229/#231) | YES | except share-ingest bypass (LOCK-SHARE-INGEST-1) |
| transcribe-perf (#230/#232/#233) | YES | TP-F1 regression; claimed FE RED→GREEN test not committed |
| hardening-sweep (#234) | YES | timeline write path missed the seal discipline it applied elsewhere |
| parakeet-lockshares (#236) | YES | progress bar effectively non-functional; PK-F1 half-closed hole |
| **shared-brain-be (#237)** | **NO** | SB-1 breaks the core cross-member promise; provenance unwired |
| shared-brain-fe (#237) | YES | badge is a title-match heuristic; per-item revoke unreachable |
| brain-sidecar (#238/#241) | YES | heartbeat liveness claimed but inert; BS-1 undisclosed trade |
| working tree | YES* | *WT-F1 defeats its own new e2e assertion |

## Notable minors (selection — full list in the workflow transcript)

- **NOTES-3 (major)** — `Db::reparent_note_folder` passes Rust **byte** length into SQLite's
  **character**-based `substr` → descendant paths corrupt for non-ASCII folder names
  (e.g. "Sprzedaż" — Polish names are the primary user population).
- `folder_active_shares` is an ungated read returning org-share **titles** for a
  sealed-not-unlocked folder (verifier downgraded critical→minor: titles only, webview-only).
- KNN retrieval leg invisible to transcript-only meetings (stricter visibility arm than FTS).
- `AppConfig::default()` now spawns `sysctl` + lists the models dir (IO in a default ctor).
- Under Critical thermal, voice-wake detection is fully dead (suspend skips the decode that
  produces wake text) until the bypass arms.
- Sidecar: idle self-exit leaves an unreaped zombie until the next call; `murmur-brain`
  crate is outside every CI gate (its 5 tests + clippy never run); no backoff after
  mid-generation crash.
- L5 MCP HTTP transport reads response bodies unbounded (stdio caps at 1 MiB).
- Verify-callout cache never invalidated on note edit/resummarize → stale inline markers.
- FE: `refreshOrgShared` lacks a stale-result guard (project failure mode #4); hardcoded
  rgba scrims in new overlay scss; tracked `setTimeout` in `note-brain-popover` component
  (rule §5 sanctions that only in root services); invalid two-easing animation shorthand →
  notes overlay entrance animations never run.
- Working tree: the `deploy-murmur-server` SKILL.md edit embeds a bare 64-hex
  `railway-verify=` token — **`secret-scan.sh` will block the commit** (0.7.5 repeat).
- Eval baseline provenance is `ddfbad0-dirty` (exact generating state unrecoverable).

## Coverage-critic gaps (not covered by any unit)

- PR #239/#240 (agent/skill fleet, ~60 cited code symbols) — no one verified the citations
  against the tree; steers future automated work.
- Sidecar release/build plumbing (workspace move, dual-target beforeBuildCommand + lipo,
  bundle.resources, sign-script path change) — only a real `tauri build` + sign + notarize
  proves it. CSP/identifier spot-checked untouched (T4 safe).
- `crates/murmur-brain` version pinned at 0.8.0 — release-bump runbook doesn't cover it.
- Untracked `e2e/org/__screens__/` PNGs not gitignored — accidental-commit risk.
- Cargo.lock transitive drift (~306 lines) — gated only by ci.sh cargo audit/deny when run.

## Honest-bar (unverifiable headless — needs a signed build / real Mac)

Touch ID KEK release, real screen-share auto-relock, lock-at-rest on disk, real
turbo/parakeet decode wall-times + Polish caption quality under flash-attn+VAD, sidecar RAM
reclaim + 90 s ready bound with a real GGUF, notarized fat `Resources/murmur-brain`,
watts via powermetrics, live L4 bullet quality, real MCP servers.

## Release-blocker shortlist (before any 0.8.x/0.9 cut)

1. NOTES-1 (note deletion data loss) + NOTES-2 (plaintext .md survives lock).
2. The three seal-on-write gaps: generate_timeline, rename_speaker, ingest_shared_note.
3. SB-1 (Shared Brain cross-member decryption broken — or ship with the feature flagged off).
4. TP-F1 (fresh-install voice-command wedge under the new turbo default).
5. Both red e2e gates (timeline-defer infinite loop; working-tree bubble re-float).
6. PK-F1 (Notes-section lock bypasses the shares dialog).

Workflow run: `wf_b79518b0-9df` (33 agents, ~3.7M tokens, 727 tool calls). Every finding
above carries a refuting-verifier verdict of `real` at HEAD; severities shown are
post-verification.
