# Org ("Shared Brain") sharing — current end-to-end analysis

**Original research:** 2026-07-26
**Refreshed:** 2026-07-28
**Operator source snapshot:** `meetnotes@cc09eedff6d550d45b134a4dd6e421ff41442a9d`
and `murmur-server@8504c7bfb894007534676e7023eca0c022f17318`

`meetnotes` is the local checkout name for the GitHub `murmur` repository; both
names below refer to the same client history.

The client pins that exact server revision in `.murmur-server-revision`. The sibling server checkout
was on a dirty, older `feat/entitlement-backbone` worktree, so server claims below were read from the
immutable `origin/main:<path>` objects at `8504c7b`, not from that checkout's working-tree files. A
read-only operator `git ls-remote origin refs/heads/main` also resolved to
`8504c7b` during this refresh. That network observation is explicitly
unattested by this docs task; the committed `.murmur-server-revision` pin is the
repository-local source used below.

This is a source-and-history refresh, not a new certification run. Existing regression tests are
named as code evidence, but no Rust/Angular/E2E gate and no live two-account Railway scenario was run
for this document.

---

## 0. Current verdict

| Area | Current state at the snapshots above |
|---|---|
| Org-route authentication | **Landed.** Org handlers take `AuthedUser`; per-org reads and writes use active membership or owner checks. |
| Org-blob authorization | **Landed.** A protected org blob is available only to an active member. Anonymous, non-member, and removed-member reads collapse to 404. The former "public org blob" finding was false. |
| Tombstone delivery | **Landed server-side.** A withdrawal is re-sequenced to the feed head, so a member whose cursor passed the live item can receive the tombstone. |
| Local withdrawal convergence | **Landed client-side.** Feed tombstones, the slow anti-entropy pass, and local revoke share one eviction primitive that blanks plaintext and removes chunks, vectors, FTS tokens, and attachment BLOBs. |
| Revoke crash recovery | **Landed client-side.** `revoke_pending -> server delete -> local eviction -> revoked`; an interruption remains re-drivable. |
| Member-removal rotation | **Still open.** The client still sends no JSON body to a JSON-extracting endpoint and prepares only the owner's next-generation grant. |
| Removed-from-only-org purge | **Still open.** The client's all-empty membership-list safety guard also prevents the legitimate last-org purge. |
| Blob integrity | **Fail-closed at the client, incomplete at the relay.** AES-GCM open plus a plaintext-envelope hash check prevents corrupt bytes from being ingested. The server stores a separately computed ciphertext SHA-256 but does not re-check it on GET. |
| Revision-stable identity | **Server transport only.** `docId` landed in server schema/DTO/feed, but the current client DTO neither sends nor reads it, so there is no end-to-end stable identity yet. |
| Roles / history / controls | `owner|member` only; audit exists only on the server; unshare and org delete remain unavailable in the normal UI/client surface. |
| Brain reach | Org content remains a separate retrieval partition. The dedicated org search can see it; normal vault retrieval, most MCP tools, graph, facts, export, and analytics cannot. |

The headline from the original report therefore no longer holds: deletion is not structurally broken
for new withdrawals on these revisions. The highest-severity remaining lifecycle issue is generation
rotation after member removal, followed by the single-org membership-purge ambiguity.

---

## 1. Synchronization and withdrawal

### 1.1 Publish and pull are still snapshot/poll based

The author publishes a frozen, cleaned note body through
`commands/org.rs::publish_org_body_with_policy`. The gate order remains:

1. source lock/read gate;
2. global org-egress consent;
3. clean + optional regex scrub;
4. seal an `OrgEnvelope` under the current OCK and open-verify it locally;
5. atomically publish inline ciphertext;
6. write a content-free egress-ledger row.

An edit is still append-and-withdraw: publish `rev + 1` as a new item, then tombstone the predecessor.
There is no PUT/PATCH item route, parent CAS, vector clock, CRDT, or server-side ciphertext merge.

Inbound sync is still polling:

- first background delay: 20 seconds;
- steady tick: 60 seconds;
- client page: `ORG_FEED_PAGE = 4`;
- untargeted tick: one round-robin org;
- background work is deferred during active recording.

Evidence: `commands/org.rs::{ORG_SYNC_FIRST_DELAY_SECS,ORG_SYNC_TICK_SECS,ORG_FEED_PAGE,
org_background_sync_tick,org_sync_one}` and `lib.rs`'s org-sync loop. The Settings "Sync now" path is
targeted; `org_refresh` remains membership reconciliation rather than a feed drain.

### 1.2 New tombstones are cursor-visible

At server `8504c7b`, `store::orgs::tombstone_item` performs:

```sql
UPDATE org_items
   SET tombstoned_at = now(), seq = DEFAULT
 WHERE id = $1 AND org_id = $2 AND tombstoned_at IS NULL
   AND ($4 OR author_user_id = $3)
 RETURNING blob_id
```

It then nulls `blob_id` and deletes the ciphertext blob in the same transaction. The fresh identity
value moves the tombstone beyond already-synced cursors; `store::orgs::feed` projects tombstones with
no blob and no `content_sha256`.

Landed provenance:

- server `d76311a`, merged as PR #15 at `8ae01cd`;
- present regression:
  `crates/murmur-server/tests/orgs.rs::org_two_account_feed_round_trip`.

That test now models the meaningful case: the reader completes a full pull, the author withdraws an
older item, and the reader asks from the already-advanced cursor. This refresh inspected the test but
did not execute it.

### 1.3 The client now converges every local representation

Client commit `303a4c7` (merged in PR #461) added:

- a bounded, durable anti-entropy cursor (`org_state.reconcile_seq`) for tombstones created before
  the server re-sequencing fix and other stale replicas;
- `Db::evict_org_item`, the single withdrawal primitive;
- atomic removal of plaintext title/markdown, chunks, int8 vectors, trigger-backed FTS tokens, and
  `note_attachments`;
- org-scoped `list_org_shares`;
- a live `org-feed-updated` revalidation path in the open org-item viewer.

Client commit `7ae780d` (merged in PR #464) tightened revoke ordering and recovery:

```text
revoke_pending -> server tombstone -> local eviction -> revoked
```

It also emits the feed-update event after a successful local revoke and repairs old
`revoked + still-live replica` rows during the launch sweep.

Present tests, not rerun for this refresh:

- `commands/tests/lifecycle_tests.rs::
  reconcile_sweep_evicts_a_tombstone_the_live_cursor_can_never_see`;
- `commands/tests/lifecycle_tests.rs::
  revoke_org_share_evicts_the_local_replica_on_the_publishing_device`;
- `commands/tests/lifecycle_tests.rs::
  an_interrupted_revoke_leaves_a_re_drivable_row_not_a_live_orphaned_replica`;
- `storage/db_tests/tests.rs::evicting_an_org_item_purges_its_attachment_blobs_on_every_path`;
- `e2e/org/org-item-withdrawn.spec.ts`.

Some client comments and RED fixtures deliberately emulate the pre-fix server shape (a tombstone
below the live cursor). They describe why anti-entropy is retained; they are not evidence that server
`8504c7b` still fails to re-sequence new withdrawals.

### 1.4 What deletion/revocation still does not solve

- `IpcService.revokeOrgShare` still has no production caller in `src/app`; normal unshare remains
  absent from the primary share UI.
- The server has `DELETE /v1/orgs/{id}`, but the client has no corresponding HTTP method, Tauri
  command, IPC method, or owner UI.
- `org_leave` intentionally does not withdraw the departing author's already-published items.
- Locking a source folder does not withdraw an already-published org snapshot. The lock dialog can
  warn/revoke while unlocked, but `folder_active_shares_inner` returns an empty report once the folder
  is sealed and not session-unlocked to avoid leaking locked titles.
- Membership reconciliation still refuses to delete all local org rows when a successful server
  response is `{"orgs":[]}`. That fail-safe protects against a hostile/transient empty response, but
  also leaves a removed user's last org replica and cached OCK locally.

---

## 2. Authentication and authorization

### 2.1 Org routes

`auth/bearer.rs::AuthedUser::from_request_parts` hashes the bearer token, resolves the access session,
rejects missing/malformed/expired tokens uniformly, and returns the bound user/device/family ids.
Org routes then apply:

- `routes/orgs.rs::require_member` for status, members, own key grants, feed, audit, and tombstone
  entry;
- `store::orgs::is_owner` for add/remove member, key-grant writes, generation bump, and org delete;
- author-or-owner SQL authorization for item tombstones.

Server PR #16 (`c0a2ff1`) changed `PUT /v1/orgs/{id}/key-grants` from member-writable to owner-only
and deletes a removed member's grant rows in the same transaction as `removed_at`.

Present tests, not rerun:

- `orgs.rs::org_key_grants_are_owner_only`;
- `orgs.rs::org_member_removal_drops_key_grants`;
- the non-member and post-removal assertions in `org_two_account_feed_round_trip`.

### 2.2 Blob GET

`routes/blobs.rs::fetch` accepts an optional bearer because legacy unreferenced M0 blobs remain
anonymous-compatible. That does **not** make a live org item blob public:

1. `store::orgs::blob_access` first classifies whether a live, non-deleted org references the blob;
2. if protected, it permits only an active member of a referencing org;
3. missing/invalid bearer, non-member, and removed member all receive the same 404;
4. a tombstone deletes the blob.

`tests/orgs.rs::org_item_blobs_are_membership_gated` covers active member 200, non-member 404,
anonymous 404, and removed-member 404. The gate was already present in server commit `e357883`; PR
#16 retained the regression test. The original "unauthenticated org ciphertext" repair item must not
be resurrected.

One related compatibility boundary remains: truly unreferenced legacy M0 blobs are still served
without auth, while `POST /v1/blobs` is now a 403 retirement shim. That is a legacy-blob policy, not a
live-org-blob authorization defect.

---

## 3. Blob and content integrity — three values, not one

The old docs and one current protocol comment blur plaintext identity and ciphertext integrity. The
actual data flow has three separate checks:

| Value/check | Producer | Consumer | Current meaning |
|---|---|---|---|
| `org_items.content_sha256` / feed `contentSha256` | Client | Client | SHA-256 of canonical **plaintext `OrgEnvelope`**. It derives the AAD nonce and is checked again after decrypt. The relay length-checks it at 32 bytes but cannot verify its meaning. |
| `blobs.sha256` | Server inline publish | Currently no read path | SHA-256 of the opaque **ciphertext cell**, computed in `routes/orgs.rs::publish_item` and stored by `store::orgs::publish_inline_item`. |
| AES-256-GCM tag + post-open hash comparison | Client seal/open | Client ingest | Wrong OCK, wrong AAD, or modified ciphertext fails open; a successfully opened envelope must also hash to the feed value before any local write. |

Evidence:

- plaintext hash contract:
  `murmur-protocol/src/dto.rs::PublishItemRequest::content_sha256`,
  `migrations/0006_orgs.sql`, and client `share/org_dto.rs::PublishItemRequest`;
- separate ciphertext hash:
  `routes/orgs.rs::publish_item` and `store/orgs.rs::publish_inline_item`;
- fail-closed ingest:
  client `commands/org.rs::org_sync_one` and `share/org_envelope.rs::open_org_envelope`.

Two open issues remain:

1. `store::get_blob` selects only `id,data,size_bytes`, and `routes/blobs.rs::fetch` returns those
   bytes without recomputing/validating stored `blobs.sha256`. Client AEAD still prevents silent
   plaintext ingestion, but the relay does not detect at-rest corruption before transfer.
2. `murmur-protocol/src/dto.rs::OrgItemEntry::content_sha256` currently says "SHA-256 of the
   ciphertext." That comment is wrong: the route puts the client-supplied plaintext-envelope hash in
   that field. Treating it as ciphertext integrity in a future client would break AAD reconstruction.

Separately, exposing the unkeyed plaintext-envelope hash to the relay creates a
confirm-the-guess oracle for predictable note bodies. Removing it is a wire/AAD migration, not a
rename: use an OCK-keyed or random nonce, preserve old-reader compatibility, and add cross-repo
golden vectors first.

---

## 4. Member removal and key rotation remain incomplete

The server half is stricter than the client half:

- only the owner can write grants;
- removing a member deletes that member's stored grants;
- `store::orgs::bump_generation` requires exactly `current + 1` and a grant for every active member.

The current client cannot satisfy that contract:

1. `share/client.rs::org_bump_generation` POSTs no JSON body or content type, while
   `routes/orgs.rs::bump_generation` requires `Json<BumpGenerationRequest>`. Source inspection
   predicts an axum 415 before the handler runs.
2. `commands/org.rs::org_remove_member_inner` wraps the new OCK only to the owner. In an org with
   another active member, the server coverage check returns 409 even after the request-body fix.
3. The client removes the member first, then rotates. Therefore a rotation failure leaves membership
   and server blob access revoked and grant re-fetch closed, but future items remain on the old OCK
   generation until rotation is repaired.
4. Server `publish_item` records the request's generation but does not compare it with the org's
   current generation. After rotation exists, a stale/malicious active client can still publish an
   old-generation item unless the server enforces the active generation.

`OrgMemberEntry.email` now gives the owner a lookup key for remaining members, but
`KEY_LOOKUPS_PER_DAY = 20` is below `MAX_ORG_MEMBERS = 50`. A safe implementation must cache verified
member identity keys and resolve the complete next-generation grant set before writing or activating
any part of it.

The predicted 415/409 sequence was not live-reproduced during this refresh.

---

## 5. Revision identity, roles, audit, and editing

### 5.1 `docId` is only a server substrate today

Server PR #17 (`8504c7b`) added nullable `org_items.doc_id`, request/feed DTO fields, and preservation
of the value on tombstones. `tests/orgs.rs::org_doc_id_links_revisions_and_survives_a_tombstone`
exists.

The client at `cc09eed` still defines its own `share/org_dto.rs::{PublishItemRequest,OrgItemEntry}`
without `doc_id`, never sends a stable id, never persists one, and never maps a tombstoned item to a
successor. The server remains backward-compatible and stores `NULL`.

Therefore:

- a new server item id is still minted for every client edit;
- recipient references held by item id still die on edit;
- there is no end-to-end revision lineage or stable reference yet;
- `docId` is deliberately metadata-only and did **not** change the AAD, signing, or conflict model.

The original proposal to put `doc_id` directly into a new envelope/AAD version is no longer the
smallest first step. First adopt the landed metadata field end to end; signatures, parent CAS, and
cryptographic revision binding remain later work.

### 5.2 Roles and authorship

Roles remain `owner|member`; no route promotes, demotes, transfers ownership, or creates a reader.
Any active member can publish; the server attributes the new item from the bearer. Destructive
withdrawal is author-or-owner.

The client only exposes edit-in-place when stored `author_user_id` matches the current session, but
that is client policy over an append-only server API. `OrgEnvelope.author_hint` is a display label,
not a content signature. A reader role would initially be a server ACL, not a cryptographic
read-only guarantee, because every reader still has the shared OCK.

### 5.3 Audit and owner controls

The server records `org_audit` and exposes member-readable
`GET /v1/orgs/{id}/audit`. The current client has no audit HTTP method, Tauri command, IPC method, or
panel. The server also implements owner org deletion, but the client surface does not.

---

## 6. Brain, MCP, and egress remain deliberately separate

`storage/org_store.rs::{search_org_chunks_knn,search_org_chunks_fts}` still feed the dedicated
`tools.rs::search_org_brain_hits` seam. The main vault retrieval, graph, facts, Related, Obsidian
export, analytics, and most MCP tools do not join the org partition. MCP has snippet search but no
full-org-item read tool.

Widening reach is still downstream of lifecycle correctness:

- org content belongs to another member and needs distinct cloud-egress consent/provenance;
- the current egress ledger cannot identify colleague-derived context;
- in-meeting `AssistantScope::Vault` must not silently inherit connector/org content;
- org items remain outside folder-lock ownership, so graph/link/export work needs a separate product
  decision and lock-security review.

---

## 7. Current priority order

1. Repair rotation atomically: complete grant set, JSON generation request, active-generation
   enforcement, and multi-account tests.
2. Resolve the authoritative-empty-membership ambiguity so a removed last-org member is purged
   locally without allowing one bad response to wipe replicas.
3. Correct the plaintext/ciphertext hash contract in protocol prose; decide whether relay GET should
   verify `blobs.sha256`.
4. Adopt server `docId` end to end in the client before building history/diff/CAS.
5. Surface unshare, org delete, and audit activity.
6. Only then widen Shared Brain into default Ask/MCP retrieval with separate egress consent and
   provenance.

Detailed, executable work sequencing is in
`docs/research/2026-07-26-org-sharing-repair-plan.md`.

---

## 8. Verification boundary

Recorded by operator source/ancestry inspection only; these are not outcomes of
the Harness control-plane checks attached to this docs diff:

- client HEAD and ancestry contain the convergence/revoke commits;
- `.murmur-server-revision` pins `8504c7b`;
- remote `main` resolved to the same server commit;
- the cited implementations and regression tests are present.

Not performed in this refresh:

- `cargo test --lib`, Angular lint/build, server SQLx integration tests, or E2E;
- a real app launch;
- a real two-account Railway tombstone, blob-read, removal, or rotation scenario;
- any deployment-state comparison beyond the pinned/current server source revision.
