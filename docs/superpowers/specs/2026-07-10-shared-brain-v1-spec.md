# Shared Brain v1 — build spec / seam contract (2026-07-10)

Binding contract for the parallel builders of `feat/shared-brain` (app) + `feat/orgs` (murmur-server).
Architecture + evidence: `docs/research/2026-07-10-shared-brain-org-context.md` (decisions LOCKED, spikes MEASURED).
One-line scope: **org-wide E2EE replicated brain** — explicit per-item "Share to Brain" publishes an
OCK-encrypted envelope to an org ciphertext feed on murmur-server; every member's app syncs, decrypts,
and indexes into a local **int8** org partition; queries stay local (app cascade + local MCP `org_search`).

## Trust model (normative)

- Server = zero-knowledge dumb relay. Org tables carry **no content-derived strings** (0003/0005 discipline).
- OCK = random 32-byte org content key, generation-numbered. Grants = HPKE-wrapped OCK per member,
  stored opaquely server-side, wrapped/unwrapped ONLY client-side via the existing `e2ee` machinery.
  OCK lives in RAM (`AppState`), unwrapped on demand after login; never in the DB, keychain, or logs.
- Envelope `OrgEnvelope v1` (new module beside `share/envelope.rs`): `{version:1, kind:"note"|"summary",
  title, markdown, author_hint, created_at, source_rev}` — canonical serialization, AES-256-GCM under the
  OCK with an AAD domain (`org-item` + org_id + item nonce), sealed/opened via `crypto.rs` primitives.
  `content_sha256` = SHA-256 of the canonical plaintext envelope. Reserved (NOT sent in v1):
  `embed_model_id` + `author_vectors` (big-org optimization; vectors never leave in plaintext regardless).
- Redaction policy (user-approved 2026-07-10): regex PII scrub (emails/phones/cards) default-ON with a
  visible toggle; person names KEPT; transcript sharing NOT in v1 (checkbox reserved). Audio never.
- **Lock-domain statement (for the lock review):** inbound org items are *deliberately org-disclosed*
  content living OUTSIDE the folder-lock domain, in dedicated `org_*` tables (not `meetings`/`documents`),
  DB-only (no vault export in v1), protected at rest by SQLCipher. The lock gates protect the user's OWN
  sealed content: egress is gated (`meeting_is_unlocked` FIRST statement — a sealed item is unshareable),
  and lock_folder gains the shares warn/revoke flow. Org reads need no `visibility_clause` because no
  sealed state exists for org items — this is a documented design decision, not an oversight.
- Prompt-injection defense: org text reaches the agent/MCP ONLY through the fence-neutralizing connector
  formatter with `[org · <author_hint>]` provenance labels; NEVER into system prompts.

## App: local schema (additive, in `Db::migrate()`)

- `org_state(org_id TEXT PK, name TEXT, role TEXT, joined_at, consented INTEGER, last_seq INTEGER)`
- `org_shares(id PK, org_id, meeting_id TEXT NULL, document_id TEXT NULL, kind, rev, content_sha256,
  item_id TEXT NULL, state TEXT CHECK(queued|uploaded|failed|revoke_pending|revoked), last_error, ts…)`
  (mirror `outbound_shares` conventions; launch sweep = `share_rewrap_pending` pattern)
- `org_items(item_id TEXT PK, org_id, seq INTEGER, author_hint TEXT, title, markdown, created_at, rev,
  tombstoned INTEGER)` — replica of the decrypted feed
- `org_chunks(id PK, item_id, chunk_idx, text)` + `org_vec_chunks` **vec0 int8[384]** (values wrapped in
  `vec_int8(?)`) + `fts_org_chunks` FTS5 `unicode61 remove_diacritics 2` + the production trigger trio.
  Vectors computed LOCALLY by the member's own active embedder (StubEmbedder ⇒ FTS-only, flagged in sync
  report, re-embed when a real model appears). **FTS query must have SQL LIMIT pushdown** (don't copy the
  unbounded production reader — spike finding #2).

## App: Tauri commands (each: `commands.rs` + `generate_handler!` in `lib.rs`; DTO conventions match existing)

`org_create(name)` → OrgStatus · `org_status()` → OrgStatus|null · `org_invite_member(email)` ·
`org_list_members()` → OrgMember[] · `org_remove_member(user_id)` (owner; drives OCK rotation) ·
`org_leave()` · `consent_to_org_egress()` / `revoke_org_egress()` (preserve-only config keys, mirror
`share_egress_consented`) · `preview_org_share(meeting_id?, document_id?, scrub:bool)` → OrgSharePreview ·
`share_meeting_to_org(meeting_id, scrub:bool)` / `share_document_to_org(document_id, scrub:bool)`
(**gate order: (1) `meeting_is_unlocked` / sealed-doc refusal, (2) consent fail-closed, (3) clean via
`clean_note_body`, (4) regex scrub, (5) seal under OCK, (6) upload blob + publish item, (7) egress-ledger
entry (content-free)**) · `list_org_shares()` → OrgShareEntry[] · `revoke_org_share(item_id)` ·
`org_sync_now()` → OrgSyncReport · `org_get_item(item_id)` → OrgItemDetail ·
`folder_active_shares(folder_id)` → ActiveSharesReport · `revoke_shares_for_folder(folder_id)` ·
plus internal launch sweep (org upload/revoke queue + feed pull).

DTO shapes (FE mirrors in `core/models.ts`; wire casing = repo's existing DTO convention):
- `OrgStatus{orgId,name,role,memberCount,consented,lastSeq,itemCount,pendingShares}`
- `OrgMember{userId,email,role,addedAt,removed}`
- `OrgSharePreview{title,markdown,bytes,chunkCount,scrubbed{emails,phones,cards},scrub}`
- `OrgShareEntry{itemId,kind,title,sharedAt,rev,state}`
- `OrgItemSummary{itemId,authorHint,title,createdAt,rev}` / `OrgItemDetail{…,markdown}`
- `OrgSyncReport{pulled,ingested,tombstoned,lastSeq,ftsOnly,errors[]}`
- `ActiveSharesReport{links,users,org:[{itemId,title}]}` (content-free enough for a dialog; titles OK —
  they render only to the local owner who can already read them)

## App: retrieval + MCP + agent

- Org partition = third leg alongside notes/docs in the hybrid path: org vec (int8 KNN) + org FTS,
  fused with the existing `rrf_fuse`/`score_fuse`; results carry `origin:{kind:'org',author,orgItemId}`.
- Dedup "I shared it myself": drop/relabel org hits whose `content_sha256` matches a row in `org_shares`.
- Agent tool `org_brain_search` (connector-class tier; advertised only when org joined + consented) +
  local MCP tool `org_search` in `mcp.rs` (loopback, read-only, same payload conventions as the 6 tools).
- Citations: `[[Title]] (org: <author_hint>)` convention; `VaultSource.origin?` in `models.ts`.

## App: FE (Angular 22 zoneless; FE agent OWNS `ipc.service.ts` + `models.ts` + `src/app/**`; BE agents do NOT touch them)

1. Share-panel third flow "Org Brain" (detail `share-panel/`, notes twin) + post-processing CTA.
2. Preview sheet (opaque overlay per T3): exact outgoing markdown, byte count, scrub toggle + counts.
3. Settings › Organization section (pattern `settings-account-section`): create org, invite by email,
   member list (+ remove for owner), leave, consent toggle, sync status/`Sync now`.
4. Ask provenance: org chips in `SourcesComponent` (author + date), org-item read-only viewer route.
5. Share badges: `MyShareEntry.mode` gains `org`; `.pill` badge in detail/library.
6. Lock×shares dialog: before `lock_folder`, call `folder_active_shares`; if any → blocking dialog
   "N items shared (M in Org Brain) — [Revoke & lock] (default for org) [Lock anyway] [Cancel]".
All views: Liquid Glass tokens, `mur-*` catalog, dir-per-component, signals-only, mocked-invoke Playwright
smoke (`e2e/` harness, port :4210 pattern).

## Server (`feat/orgs`, additive on the spike): v1 hardening

- `orgs.current_generation INT` + `POST /v1/orgs/{id}/generation` (owner; monotonic +1, requires grants
  for gen N+1 already PUT for every active member — server checks counts only, verifies nothing).
- `POST /v1/orgs/{id}/leave` (member self-removal) · `DELETE /v1/orgs/{id}` (owner; soft-delete + GC).
- `org_audit(org_id, actor_user_id, event, target, created_at)` — membership/generation/tombstone events
  only, no names/content; queryable by members `GET /v1/orgs/{id}/audit`.
- Keep: uniform 404, per-member daily quota, tombstone feed. Fix the harness mail race (join/poll the
  `deliver` spawn_blocking) so the suite is deterministic.
- Protocol-version negotiation: DEFERRED (org endpoints are new+additive; old clients never call them) —
  note in PR body.

## Order & ownership

- srv (server repo) ∥ be-core (app: consent/preview/share/state machine/org client/OCK) ∥ fe (app FE vs
  this contract, mocked invoke) → be-sync (feed sync/ingest/int8 partition/retrieval/MCP) → be-lock
  (lock×shares warn/revoke + the pre-existing 1:1 fix) → spec review → adversarial-verifier +
  lock-security-reviewer (MANDATORY) → `scripts/ci.sh` → QueaT commits → PRs (`murmur-io/murmur` base
  `murmur`; `murmur-io/murmur-server` base `main`).
- Honest boundary: OCK-in-Keychain UX, Touch ID, live 2-account sync vs Railway = signed build + second
  machine; headless proof = unit/integration tests + Playwright FE + dev-app boot.
