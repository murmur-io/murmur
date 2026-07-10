<!-- Generated 2026-07-10 via /research (murmur-researcher fan-out: 5 angles + cross-cutting critic; workflow wf_2f5e0417-05f). Pricing/versions = point-in-time. -->
# Research: Shared Brain — organization-wide shared context over murmur-server

## TL;DR / Verdict

**Build the Shared Brain as a zero-knowledge "replicated brain", not a hosted one.** An org gets a symmetric **Org Content Key (OCK)**, HPKE-wrapped to each member's existing identity key; "Share to Brain" publishes an E2EE envelope (cleaned note + summary, transcript opt-in, never audio) to an org-scoped append-only ciphertext feed on murmur-server; **every member's app syncs the feed, decrypts locally, and indexes it into a local org partition of SQLite using the already-shipped embed/FTS/hybrid pipeline**. The server stays a dumb relay — it maps/indexes nothing and can read nothing.

The decisive insight (3 of 5 independent researchers converged on it; the critic confirmed): **nothing in the feature requires server-side plaintext.** Every consumption surface Murmur has is local — person B's Claude Code talks to person B's **local** MCP server (`mcp.rs`, `127.0.0.1:8765`), which serves the locally-synced org partition exactly as it serves local meetings today. "Local brain first, then org brain" becomes two local queries. The user's framing ("backend processes/maps/indexes") is a *means*; the *end* (org context available to all members from the app and Claude Code) is fully met by replication — while preserving the one moat a 1–2-person team can credibly defend: *"we architecturally cannot read your org's brain."* No competitor can say that (all are per-seat, cloud-resident plaintext).

Server-side plaintext processing is **rejected for v1** (kept as a separately-named, separately-consented contingency behind a client-side seam): it converts the deployed zero-knowledge relay — with real users — from "cannot read your data" into "chooses not to", a one-way trust/marketing door. Uploading "just embeddings" is disqualified twice over: vec2text inverts embeddings back to text (~92% exact recovery of 32-token inputs), and the repo's own lock model already treats vectors as PII (`lock_folder` purges them: "a vector is PII derived from the plaintext").

**Bonus finding (verified real bug):** `lock_folder_inner` never warns about or revokes active outbound shares — the spec decided "warn + offer revoke" but it was never implemented. Org sharing multiplies this hole N-fold; the fix ships as part of this feature.

## Co już mamy (from the repos; symbols > line numbers, lines drift)

**App (`meetnotes/src-tauri/src/`)** — every compute block the design needs is shipped, on-device:
- Embedder: multilingual-e5-small, 384-dim, candle/Metal (`embed.rs` `EMBED_DIM`, `embed/candle_bert.rs`); chunking (`chunk_note` ~800 chars, `chunk_transcript` ~1000+150 overlap), RRF/score fusion (`rrf_fuse`, `score_fuse`), hybrid recall@5 ≈ 0.90 (eval harness). **Traps:** silent `StubEmbedder` fallback when the model is absent (`embed.rs:349-367`), and two user-selectable models incl. Polish-first `mmlw-retrieval-e5-small` — "vectors from a different model are not comparable".
- Gated retrieval: `search_hybrid_visible` (`db.rs:3391`), `visibility_clause` everywhere; vec0 + FTS5 (`unicode61 remove_diacritics 2` — no stemming).
- Agentic loop: `run_agentic_loop` + `GatedToolExecutor` (`agent.rs`, `tools.rs`); tiered cascade CurrentMeeting → Vault → Connectors with the `__ESCALATE__` sentinel — **the local-first cascade already exists structurally**. Connector tools get consent-fail-closed + query redaction (`redact_connector_query`) + egress ledger + fence-neutralizing formatter.
- Local MCP: loopback `127.0.0.1:8765`, 6 read-only visibility-gated tools (`search_meetings`, `search_semantic`, `get_entity_dossier`…), module contract **"NO egress"** (`mcp.rs:1-3`) — load-bearing.
- E2EE share client (mode A/B): `share_note_to_user_inner` — `meeting_is_unlocked` gate as **statement (1)**, fail-closed `share_egress_consented` as (2), `clean_note_body` sanitization (the vault-titles-leak fix), HPKE seal + TOFU, content-free egress ledger, `outbound_shares` state machine with `rev` + `content_hash`; offline sweep precedent `share_rewrap_pending_inner`; inbound ingest precedent `accept_share` → `ingest_shared_note`.
- Auth: OPAQUE login, Keychain tokens, 30-min access + single-use rotating refresh; **reusing an old refresh token triggers family revocation** (`share/client.rs:227-229`) — any external credential (Claude Code helper) must never share the app's token family.

**Server (`murmur-server/`)** — deployed on Railway, real users:
- Zero-knowledge dumb relay: opaque ciphertext blobs ("the server treats bytes as opaque — it cannot read them", `routes/blobs.rs:1`); OPAQUE + TOTP auth; key directory with **append-only identity-key generations** (`routes/keys.rs:37-55` — exactly the rotation substrate an org key needs); signed-grant protocol (`murmur-protocol/identity.rs` `ShareGrantSignedView`) that generalizes near-verbatim to an `OrgKeyGrant`; invite/suppression/rate-cap machinery + Resend mail.
- **No org concept anywhere** (grep = zero hits; migrations 0001–0005 only). Caps sized for 1:1 shares must be renegotiated: 1 MiB/blob, 100 MiB/account, 50 shares, 100 uploads/day (`caps.rs`).
- `PROTOCOL_VERSION` exists but is consumed only by `/health` — **no request-level version negotiation** (a real hazard with deployed 0.7.x/0.8.0 clients).

## Findings (per angle, synthesized)

### 1. Trust model — the load-bearing fork (angle: security)

Option space examined: (a) full ZK with client-side compute, (b) Bitwarden/Ente-shape org-key E2EE, (c) plaintext/encrypted vectors server-side, (d) trusted-service plaintext tier, (e) confidential computing.

- **(c) disqualified:** vec2text recovers ~92% of 32-token inputs exactly, incl. names from clinical notes ([arxiv 2310.06816](https://arxiv.org/abs/2310.06816)); replicated in/out-of-domain in 2025 ([arxiv 2507.07700](https://arxiv.org/abs/2507.07700)). Murmur's embedder is public → attacker query-access is free. The repo itself already treats vectors as PII (`commands.rs` lock purge comment). IronCore Cloaked AI (distance-preserving encryption) is a niche hedge, not an architecture — it only solves the vector leg, never the "map/index" leg.
- **(d) is what Slack AI / Glean / Notion AI actually sell** — contractual + architectural containment (escrow VPC, single-tenant, no-training, zero-retention), *never* E2EE (all three security pages fetched). That posture requires SOC 2, a security team, and breach-response capacity — precisely what a 1–2-person team cannot credibly operate for aggregated org meeting corpora. Breach of org plaintext = company-ending; ZK is the architecture that lets a tiny team hold org data *without being able to lose it*.
- **(e) Nitro Enclaves/SEV-SNP:** Railway→AWS migration, no GPU, attestation plumbing — XL effort for a guarantee target orgs won't audit. Parked.
- **(a)+(b) wins:** org key (random 32-byte, `e2ee/keys.rs` shape) HPKE-wrapped per member via the existing wrap + key-directory machinery; org-key **generations** (reuse the monotonic-append pattern) with rotation on member removal — the Vaultwarden lesson (org key never rotated on removal → departed members kept decrypting; fixed in 1.32.0). Honest residual, same as Slack: a leaver retains what they already synced. MLS (RFC 9420) considered, rejected for v1 — TreeKEM complexity is disproportionate for N≤50 orgs sharing a retained corpus.
- **Ente is the existence proof** that "shared E2EE content + ML search, zero-knowledge server" ships at production scale: all ML on-device, E2EE-synced indexes, search within shared libraries ([ente.com ML docs](https://ente.com/help/photos/features/search-and-discovery/machine-learning)).
- GDPR: ciphertext-only processor = lightest obligations; erasure = delete blobs + crypto-shred keys. The under-discussed exposure is **meeting participants'** data (third parties' words broadcast to an org) → default share unit = note/summary, transcript opt-in per item, optional NER pre-scrub.

### 2. Who computes embeddings — **vectors never sync** (angles: security + scale + client, arbitrated by the critic)

Three positions emerged (server-side fastembed / author-computed vectors in the envelope / local re-embed per member). **Local re-embedding per member wins**, strengthened by two *verified* code facts: the silent `StubEmbedder` fallback would let one model-less member poison a shared index invisibly, and per-user model selection (multilingual-e5 vs Polish-first mmlw) makes cross-member vectors incomparable. With vectors as derived, per-device data: no poisoning class, nothing vector-shaped ever leaves the device, and **Polish users keep mmlw** — the org index is *better* in the dominant language of the user's corpus, per member. Cost: N× redundant embed compute + one-time bootstrap (~17 min/100k chunks, estimate — spike verifies). Keep `embed_model_id` tagging in the envelope as a future optimization flag only.

### 3. Scale — "hundreds of thousands of contexts" (angle: scale, reframed by prior-art + critic)

The briefs' apparent disagreement dissolved on units and org size: a realistic 15-person org ≈ 19k items/*year*; "hundreds of thousands of items" is a 50+-seat multi-year scenario. Client-side math at the operative scale: 100k items ≈ 1M chunks × 384-dim ≈ 1.5 GB f32 (≈ 400 MB int8) + 2–10 GB ciphertext synced once — desktop-manageable; sqlite-vec brute-force ~100–400 ms/query at 200–500k chunks (third-party benchmarks, medium confidence; IVF/DiskANN are in-tree upstream). **The honest ceiling: client replication has a per-member disk/bootstrap wall somewhere between 100k and 1M items/org.** Mitigations, in order: date-window pre-filters, int8 quantization, ANN when it lands, and the `OrgBrainBackend` seam (below) as the escape hatch. Server caps: 1 MiB/blob forces transcript chunking; org-scoped quota constants are a mechanical `caps.rs` change.

**Contingency file (if a hosted tier is ever consciously chosen):** the scale-rag brief's full design is preserved in the workflow transcript — Postgres 17 + pgvector `halfvec(384)` HNSW + iterative scans, two-mode search (exact KNN ≤30k-chunk orgs / HNSW above), `to_tsvector('simple', unaccent())` FTS parity with local, RRF in SQL, SKIP LOCKED ingest worker, server-side fastembed-rs, org_id + composite indexes + RLS-as-defense-in-depth, hash-partitioning as the exit ramp; fits ~10M chunks on one Railway Postgres; exits to VectorChord/Qdrant beyond. Technically excellent — wrong *default* for this product.

### 4. Query orchestration + MCP (angle: query-mcp, topology corrected by the critic)

- **In-app cascade:** already structural. Org items land in a local org partition → they're one more partition of the same hybrid retrieval; "local first" becomes ranking + provenance, and at scope `Full` a one-line prompt nudge. Dedup of "I shared it myself": deterministic — record `(org_item_id, meeting_id, sha256(shared_markdown))` at share time; drop/relabel org hits matching a locally-owned hash (mirrors the existing `self_tag` filter in `orchestrate.rs`).
- **Claude Code:** **no remote MCP in v1.** The local MCP automatically sees the org partition; add one `org_search` tool (+ provenance in results) to the existing 6, gated like everything else. This kills the whole remote-auth problem (OAuth 2.1 AS, token families, headless CI limitations) and honors the local MCP's verified "NO egress" contract. A proxying `org_search` that egresses from the local MCP is the worst option — rejected.
- **Kept on the shelf** (from the query-mcp brief, for any future hosted tier): MCP spec rev 2025-11-25 mechanics (Streamable HTTP, plain-JSON POST allowed, Origin→403), Claude Code's `--transport http` + `--header` + `headersHelper` + OAuth `claude mcp login` support and its non-interactive limitation, the refresh-family trap (verified), the `org_tokens` scoped-token design, rmcp v2.2.0 vs the ~200-line hand-rolled axum handler.
- Provenance: extend `VaultSource` with `origin?: {kind:'local'|'org', author, orgItemId}`; org chips in `SourcesComponent`; citation convention `[[Title]] (org: Anna)`; org-item viewer route. Offline: connector-error degradation already built into the loop (failure markers steer the model back to local grounding).

### 5. Product model + prior art (angle: prior-art)

- **Market:** 100% of the field is per-seat, cloud-resident plaintext. Granola 2.0 markets *"a second brain for your team"* ($0/$14/$35 per seat; explicit folder-shares); Otter auto-shares to calendar invitees by default; Fireflies defaults to org-wide visibility; Gong = total capture + $3–4k/seat/yr effective; ClickUp Brain charges every paid member. **The feature is commodity; the ownable position is "the org brain whose server can't read your meetings."**
- **Auto-share is the trust catastrophe:** the Otter/Bilzerian incident (auto-emailed transcript incl. VCs' candid post-meeting talk → deal died) + a 2025 class action. Explicit "Share to Brain" is a *feature*, not friction — but opt-in knowledge bases die of starvation, so v1 must ship the anti-graveyard kit: share CTA **at note-completion** (zero navigation), per-folder standing **offer** (never auto-send), consumption pull (org answers with provenance + Claude Code access create sharer demand), target 2–15-seat teams where critical mass is two people.
- **v1 org primitives:** org create + email invites (reuse invite/suppression/Resend infra), roles = owner + member only, OCK wrap on join, rotate on removal, unshare = server delete + tombstone + best-effort member purge. **Defer:** channels/groups, admin console, SSO, retention policies, per-item recipient subsets, offboarding reassignment, any server-side AI.
- **Pricing:** because the server does no AI (relay + Postgres rows), marginal org cost is storage + bandwidth, not inference — competitors structurally cannot price flat without eating LLM COGS; we can (banded flat per-org; Basecamp-style comparable). Positioning line: *"Your team's meeting brain — on your team's Macs, not our servers."*

### 6. Client integration (angle: client-integration — adopt as the implementation map)

- **Share-to-Brain seam:** third flow in `share-panel` (detail) + `note-share-panel` (notes) + a post-processing CTA ("Processed — add to Org Brain?") that structurally satisfies "sync after processing". Payload: title + `clean_note_body` markdown + summary (+ topic-chunk texts v1.1; transcript = separate checkbox default OFF; audio never).
- **Consent stack:** one-time org egress consent key (preserve-only, mirror `share_egress_consented`) + explicit per-item action + **preview sheet** showing the exact post-clean text and byte counts (opaque overlay per T3). "Remember per folder" = auto-*offer* only.
- **Redaction (product decision, see forks):** regex PII scrub (emails/phones/cards) default-ON with visible toggle; **names kept** (they're the point of a team brain); member queries via the connector seam stay auto-redacted.
- **Lock model:** `share_meeting_to_org` copies the gate-first template; inbound org items live in normal local folders → inherit `visibility_clause`/sealing/MCP masking for free; **ship the `lock_folder` × active-shares warn/revoke dialog** (fixes the verified pre-existing 1:1 hole; for org items the default button is Revoke). Revoke propagates: server delete → `revoke_pending` offline state → member purge on sync; honest UI copy ("colleagues who already read it may retain it").
- **Sync:** `org_shares` outbound state machine (`queued→uploaded→failed→revoke_pending→revoked`) swept on launch (the `share_rewrap_pending` pattern); inbound per-org feed cursor on launch/foreground; edit-and-reshare = `rev` bump + `content_hash` no-op detection (columns already modeled).
- **~14 new commands** (each = `commands.rs` + `lib.rs` registry + typed `IpcService` + `models.ts`): `org_create/status/invite_member/list_members/leave/remove_member`, `consent_to_org_egress/revoke_org_egress`, `preview_org_share`, `share_meeting_to_org`, `share_document_to_org`, `list_org_shares`, `revoke_org_share`, `org_sync_now`, `org_get_item`.
- **FE:** Settings › Organization section (pattern: `settings-account-section`), share badges (`MyShareEntry.mode` gains `org`), org provenance chips in Ask, org-item viewer; all `mur-*`/design-token native. Obsidian export of colleagues' items: DB-only by default + optional per-org "export to vault folder" toggle (vault bloat + Re-Truth collisions).
- **`OrgBrainBackend` trait** (local-partition | remote-query) so a future hosted mode never rewrites FE/commands.

### 7. What everyone missed (critic's gaps — fold into the plan)

1. **Org-index prompt injection / poisoning:** org content is untrusted multi-writer input fed into every member's agent AND Claude Code. Mitigate: always render through the fence-neutralizing connector formatter, untrusted-provenance labels, never into system prompts, owner-level removal, and an adversarial test that a hostile org item cannot trigger tool egress.
2. **Per-member abuse economics:** per-member-within-org quotas (one member's 50k junk items = everyone's disk in the E2EE design), owner moderation, kick-then-purge semantics.
3. **Server observability:** explicit no-PII logging policy for the new surfaces + a security audit log (membership changes, key-generation bumps); metadata (who/when/sizes) needs a stated policy even in the E2EE design.
4. **Backup/restore vs revocation:** pg_dump restore can resurrect revoked items (tombstone replay) — GDPR conflict; org-key disaster recovery (all admins lose keys) undesigned — extend the `e2ee/recovery.rs` kit pattern.
5. **Protocol versioning:** add request-level version negotiation before shipping org DTOs to a deployed fleet; write the live Railway migration/rollback plan.
6. **Polish quality:** vectors-never-sync lets Polish members keep mmlw (a *benefit*); still run a Polish bake-off for the org corpus on the existing eval harness; check DeBERTa NER on inflected Polish names for the pre-scrub.
7. **Legal structure:** for org content the *org* is the controller → org-level DPA/ToS (B2B surface that doesn't exist), meeting-participant consent (PL/EU nuances), works-council exposure. Needed even for pure E2EE (membership metadata is server-visible).
8. **Cost-of-goods per org:** E2EE multiplies bootstrap egress by N — compute Railway $/org/month before committing flat pricing.

## Fit z ograniczeniami Murmur

- **Local-first/privacy:** extended, not violated — org egress is explicit per-item, E2EE, processed only on members' devices. Tier-0 untouched.
- **Obsidian-native:** own notes unchanged; colleagues' items DB-only by default, optional vault export.
- **SQLite-canonical (deliberate amendment — needs user sign-off):** for *org* data the canonical truth is the server's append-only ciphertext log; local org tables are an indexed replica. One canonical per datum preserved.
- **Provider seam + redaction firewall:** untouched for LLM egress; org share is a *new egress class* (human recipients) with its own consent stack — the "names kept" deviation is an explicit product call.
- **Lock model:** share gate = first statement (verified template); inbound items inherit gating by living in folders; lock×shares dialog ships here; **lock-security-reviewer is a required gate**.
- **macOS/CI honesty:** server + state machines + envelope transforms + gate ordering = headless-testable; OCK-in-Keychain UX, Touch ID, live two-account sync against Railway = signed build + second account/machine.

## Opcje i tradeoffy

| Option | Effort | Risk | Verdict |
|---|---|---|---|
| **A. E2EE replicated brain** (OCK + ciphertext feed + client-side indexing, vectors never sync) | **L** total (server M ~1–2 wk on existing patterns; client L ~3–5 wk in S/M slices) | Bootstrap/disk wall ~100k–1M items/org; rotation correctness; graveyard risk if share moment isn't frictionless | **WINNER** — only ZK team brain on the market; MCP for free; flat pricing story; lightest GDPR |
| B. Server-side plaintext org tier (pgvector/RAG per the scale brief) | M build, **L forever** (compliance/ops/liability) | One-way trust-brand door against real users; breach magnet; commoditizes the moat into Glean/Slack turf | Rejected as default; preserved as the separately-consented contingency behind `OrgBrainBackend` |
| C. Plaintext/PPE vectors server-side | M | vec2text inversion ≈ content leak; repo's own vectors-are-PII stance | Disqualified |
| D. Confidential compute | XL | Railway→AWS, no GPU, attestation | Parked |
| E. Stay 1:1-only (no org) | S | Cedes "team brain" to Granola entirely | Rejected |

## Rekomendacja i pierwszy krok

**Option A: E2EE org brain, replicated.** Architecture = security spine (OCK + generations + rotation-on-removal) + prior-art product scoping (owner/member, explicit share + anti-graveyard kit, 2–15-seat target) + the client implementation map (gate-first template, preview sheet, state machines, local org partition, `org_search` on the local MCP) — with **local re-embedding per member** (no vectors in envelopes) and the critic's 8 gaps folded into the plan. Present the two forks to the user before building (below).

**Two de-risking spikes before any product code (~1 week total):**
1. **Mac scale spike (1 day):** seed 300k–1M synthetic 384-dim chunks into vec0 + FTS5, measure hybrid query p50/p95 + index build + disk; settles the client-side ceiling with numbers.
2. **Two-account E2EE org round-trip (3–5 days):** local murmur-server branch — mint org, HPKE-wrap OCK to two members, publish 1k encrypted items, member B cold-bootstraps (pull → decrypt → embed with real e5/mmlw → index), answer a cross-member query through the local MCP; measure bootstrap wall-time; prove sealed-meeting share refusal RED→GREEN.

Then v1 slices in order: (1) S — consent keys + `preview_org_share` + preview sheet; (2) M — server org tables/routes/quotas + OCK grants (additive migrations 0006+); (3) M — `share_meeting_to_org` + `org_shares` state machine + launch sweep; (4) M — inbound feed sync + local ingestion + re-embed; (5) S/M — retrieval partition + provenance chips in Ask; (6) S — local MCP `org_search`; (7) M — lock×shares warn/revoke dialog (also fixes the shipped 1:1 hole); (8) S — org settings UI. Gates: adversarial-verifier + **lock-security-reviewer** (mandatory), plus the poisoning adversarial test.

## Spike results (MEASURED 2026-07-10 — both spikes done same day)

**Spike 1 — Mac scale bench** (branch `spike/org-scale` in `meetnotes-orgspike-wt`; real `Db::open_with_key`+`migrate()` schema, production gated readers, SQLCipher ON, sqlite/sqlite-vec at -O3, M-series; treat as ±30%, linear-in-N shape and int8 ratios are the robust findings):

| metric | 100k chunks | 300k | 1M |
|---|---|---|---|
| insert (chunk+vec+FTS, tx/10k) | 5 973 rows/s | 5 964 rows/s | 5 860 rows/s |
| DB size (f32) | 184 MB | 550 MB | 1 833 MB (1.88 KB/chunk) |
| raw vec0 KNN top-10 p50 | 208 ms | 620 ms | 2 016 ms |
| production KNN reader p50 | 606 ms | 1 839 ms | 5 941 ms |
| FTS top-10 p50/p95 | 70/919 ms | 138/2 659 ms | 446/8 772 ms |
| **int8[384] vec0 KNN p50** | ~60 ms (extrap.) | **~180 ms (extrap.)** | **598 ms** (table 3.7× smaller → ~0.82 KB/chunk) |

**Measured verdict (revises §3's estimate):** f32 brute-force blows the 100–400 ms budget from ~150–200k chunks; **int8 quantization makes 300k HOLD in-budget**; 1M is ~1.5× over with int8 alone — reachable with one more lever (int8-prefilter→f32-rescore, per-sharer table pruning, or Matryoshka dim truncation), i.e. v1 should ship int8 vectors for the org partition from day one. Embedder throughput (real multilingual-e5-small on Metal): 52.5 chunks/s → bootstrap-by-re-embedding ≈ **32 min/100k chunks** (fine at realistic org sizes), ~5.3 h/1M (painful) → at big-org scale the sanctioned lever is **E2EE-synced author vectors tagged with `embed_model_id`** (ciphertext to the server, so no inversion exposure; recipient re-embeds on model mismatch — the critic's "optimization flag" fallback, promoted to the large-org path).

**Spike 2 — server org half** (branch `spike/org-feed` in `murmur-server-orgspike-wt`, 4 commits, fmt+clippy clean): migration `0006_orgs` (orgs/members/opaque key-grants per generation/items with identity `seq` + soft tombstones), membership-gated `/v1/orgs/*` routes with uniform-404 anti-enumeration, per-member daily quota (429), two-account integration test 3/3 green. **Timings: publish 1000 items ≈ 2.84 s (2.84 ms/item, dominated by 2 HTTP round-trips — batch later); cursor pull of 1000 ≈ 26 ms. The ciphertext feed is effectively free.** ZK discipline proven: grants stored as opaque bytes, never verified.

**Repo follow-ups surfaced by the spikes (pre-existing, independent of Shared Brain):**
1. Production doc-KNN reader is ~3× the raw vec0 scan (join/CTE shape, not the scan) — profile + fix before Shared Brain ships.
2. Production FTS reader has no SQL `LIMIT` pushdown (bm25-sorts all matches, dedups in Rust) — 8.8 s p95 tail at 1M; needs LIMIT pushdown.
3. `cargo test --lib --release` is broken (keychain.rs tests call `#[cfg(debug_assertions)]`-only helpers without matching cfg — 33 E0425s).
4. murmur-server test harness mail race: `AppState::deliver` does `spawn_blocking` without joining → non-deterministic auth/shares test flakes; one-line harness fix.
5. Server v1 remainders: protocol-version negotiation, OCK generation-bump endpoint, org delete/leave, audit log, bulk purge of a removed member's items, consider per-org `seq`.

## Otwarte pytania (dwa forki wymagają decyzji użytkownika + reszta)

1. **FORK A — trust model: ✅ RESOLVED 2026-07-10 (user).** Every org member will run the Murmur app → **E2EE-replicated architecture confirmed**; server-processed tier stays a contingency only.
2. **FORK B — redaction deviation: ✅ RESOLVED 2026-07-10 (user).** Approved: org shares keep person names, regex-scrub (emails/phones/cards) default-ON, transcript opt-in per item.
3. SQLite-canonical amendment (server ciphertext log canonical for org data) — sign-off.
4. Vault export of colleagues' items: DB-only default OK?
5. Rotation policy on member removal: protect-future-only (recommended) vs re-encrypt history.
6. Real scale expectations: is 100k+ items/org a near-term profile or a stress ceiling? (Decides ANN urgency.)
7. Unverifiable headless: OCK Keychain UX on a signed build (the `-34018` class), live two-machine sync, Touch ID.
8. Legal pass (org-as-controller DPA/ToS, PL/EU participant consent, works councils) before launch.

## Sources

**Fetched:** arxiv.org/abs/2310.06816 (vec2text); arxiv.org/abs/2507.07700 (reproduction); bitwarden.com/help/bitwarden-security-white-paper; ente.com/help/photos/features/search-and-discovery/machine-learning (+ E2EE-synced shared indexes announcement); slack.engineering/how-we-built-slack-ai-to-be-secure-and-private; glean.com/security; notion.com/help/notion-ai-security-practices; granola.ai/pricing + /blog/two-dot-zero; meetingnotes.com/blog/granola-ai-teardown; docs.granola.ai sharing-controls; github.com/pgvector/pgvector; github.com/Anush008/fastembed-rs; github.com/pgmq/pgmq; qdrant.tech multitenancy; turbopuffer.com/docs/architecture; blog.vectorchord.ai; github.com/paradedb/paradedb; huggingface.co/intfloat/multilingual-e5-small; modelcontextprotocol.io spec 2025-11-25 (+ transports + auth changelogs); code.claude.com/docs/en/mcp; github.com/modelcontextprotocol/rust-sdk; github.com/asg017/sqlite-vec (+ issue #25); ironcorelabs.com/docs/cloaked-ai; docs.railway.com plans/volumes.
**Search-derived (medium confidence):** NPR Otter class action; AIAAIC/Entrepreneur Otter leak incident; Fireflies/Otter/Fathom KB + pricing teardowns; Gong pricing teardowns; ClickUp Brain pricing; Glean TCO; TechCrunch Limitless→Meta; mgm-sp.com Vaultwarden org-key advisory; basecamp.com/pricing; vectorlite benchmark; RFC 9420/MLS; GDPR crypto-shredding.
**Key code:** app — `embed.rs` (EMBED_DIM/chunkers/fusion/StubEmbedder/EMBED_MODELS), `embed/candle_bert.rs`, `storage/db.rs` (`search_hybrid_visible`, vec0/FTS5 tables, `outbound_shares`), `agent.rs` (loop/sentinel/citations), `tools.rs` (tiers/GatedToolExecutor/connector redaction+formatter), `mcp.rs` (loopback/6 tools/NO-egress contract), `commands.rs` (`share_note_to_user_inner` gate-first template, `share_rewrap_pending_inner`, `accept_share`/`ingest_shared_note`, `lock_folder_inner` — no share refs = the verified hole, vectors-are-PII purge), `share/envelope.rs` (`clean_note_body`), `share/client.rs` (refresh-family revocation), `summarize/mod.rs`+`redact.rs` (firewall), `e2ee/{keys,wrap,recovery}.rs`, `settings/config.rs`; server — `routes/blobs.rs:1`, `routes/keys.rs` (generations), `routes/mod.rs` (route surface), `migrations/0001–0005`, `murmur-protocol/{identity,caps,envelope,dto}.rs`, `mail.rs`, `store/sharing_b.rs`.
**Full agent briefs:** workflow `wf_2f5e0417-05f` transcript (5 angle briefs + critic), session task `wgj1f9guw`.
