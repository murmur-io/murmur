# Org ("Shared Brain") — current repair plan

**Original plan:** 2026-07-26
**Refreshed:** 2026-07-28
**Baseline:** `meetnotes@cc09eed` + client-pinned `murmur-server@8504c7b`

`meetnotes` is the local checkout name for the GitHub `murmur` repository.

This plan supersedes the original phase status. It separates code that is in the baseline from work
that remains open. "Landed" means present in the cited commit ancestry; it does not mean this refresh
reran tests or live-reproduced the behavior.

The plan intentionally does not preserve an old harness task id or any captured harness lifecycle
recipe. Execution must use the repository's current workflow and derive checks from the actual diff
at implementation time.

---

## 0. Status ledger

### Landed in the baseline

| Item | State and evidence |
|---|---|
| Tombstones reach already-synced members | **Landed server-side.** `store::orgs::tombstone_item` sets `seq = DEFAULT`; server `d76311a`, merged in PR #15 (`8ae01cd`). |
| Masking tombstone test | **Landed.** `tests/orgs.rs::org_two_account_feed_round_trip` now pulls from the fully advanced reader cursor and matches the re-sequenced withdrawal by item id. |
| Org-blob membership authorization | **Was already landed, not a defect.** `routes/blobs.rs::fetch` consults `store::orgs::blob_access`; server `e357883`. PR #16 retained `org_item_blobs_are_membership_gated`. |
| Key-grant write authorization | **Landed.** `PUT /key-grants` is owner-only; server PR #16 (`c0a2ff1`). |
| Removed member's stored grants | **Landed.** `store::orgs::remove_member` deletes their grant rows in the removal transaction; PR #16. |
| Client anti-entropy and complete local eviction | **Landed.** Slow reconcile cursor, plaintext/chunk/vector/FTS/attachment eviction, org-scoped share list, and viewer revalidation; client `303a4c7`, merged in PR #461. |
| Revoke crash window and event refresh | **Landed.** Re-drivable ordering plus orphan repair and immediate FE event; client `7ae780d`, merged in PR #464. |
| Server `docId` transport | **Landed only on the server.** Schema/DTO/feed/tombstone preservation; server PR #17 (`8504c7b`). Client adoption remains open. |
| Server revision pin | **Landed.** client `a395f09` pins `8504c7b`, merged in PR #467. |

### Still open

| Priority | Work |
|---|---|
| P0 | Make OCK rotation after member removal actually complete and enforce the active generation. |
| P0 | Purge the local replica when the removed org was the user's only org, without trusting one unauthenticated-looking empty response. |
| P1 | Correct and harden the plaintext-hash vs ciphertext-hash contract. |
| P1 | Adopt server `docId` end to end in the client. |
| P1 | Expose unshare, org deletion, and org audit in the product. |
| P2 | Improve feed catch-up and unread state. |
| P2 | Widen Shared Brain retrieval only with colleague-content egress consent/provenance. |
| P3 | Reader/publisher roles, signed authorship, parent CAS, history/diff, comments, and folder subscriptions. |

---

## PHASE A — Finish removal and rotation

### A1. Build the complete next-generation grant set before mutation

Current defect:

- `commands/org.rs::org_remove_member_inner` removes the member first;
- it creates a new OCK but wraps it only to the owner;
- server `store::orgs::bump_generation` requires a grant for every active member.

The members response now includes email, but online key lookup is capped below the maximum org size
(`KEY_LOOKUPS_PER_DAY = 20`, `MAX_ORG_MEMBERS = 50`). Do not implement a partial PUT loop.

Required design:

1. persist each verified member identity key when it is already fetched for invite/grant work;
2. resolve and validate every remaining member's identity key before the destructive remove;
3. build every N+1 wrapped grant in memory;
4. refuse before server mutation if the set is incomplete;
5. remove the member;
6. PUT the complete N+1 grant set;
7. bump generation;
8. persist/cache the new generation only after server success.

If atomic server orchestration is added instead, preserve the zero-knowledge boundary: the server may
validate coverage and switch metadata, but never open OCK material.

### A2. Send the generation JSON body

`share/client.rs::org_bump_generation` currently POSTs an empty request.
`routes/orgs.rs::bump_generation` requires:

```json
{ "generation": 2 }
```

Add the typed request and content type. The current source predicts 415 before the handler; this
refresh did not live-reproduce it.

### A3. Enforce active generation on publish

`routes/orgs.rs::publish_item` currently clamps the supplied generation to at least one but does not
compare it with `orgs.current_generation`. Once rotation works, reject an old/future generation with
a content-free conflict. Otherwise an honest stale client or modified active client can keep
creating old-generation items.

### A4. Recovery semantics

The present client order can leave "member removed, rotation not advanced." Make that state explicit
and re-drivable:

- store a pending rotation intent before the server remove;
- retry only from durable, complete grant material;
- never invent a second OCK after some N+1 grants were already written;
- report membership removal and generation activation as distinct operator-visible states.

### A5. Proof required

Add/retain tests for:

- 3-member org: remove one, remaining two have valid N+1 grants, generation advances once;
- missing remaining-member identity: no member removal and no grant write;
- interruption after remove or grant PUT: retry converges on the same N+1 intent;
- removed member: feed/blob/grant reads remain 404;
- stale-generation publish is rejected;
- current-generation publish remains readable by every remaining member.

A real two-account (preferably three-account) run against the deployed server remains required before
claiming end-to-end rotation. Unit/SQLx tests alone are not that proof.

---

## PHASE B — Make membership loss purge locally

### B1. Replace the ambiguous all-empty guard

Current code in `commands/org.rs::reconcile_org_state_into_db_with_policy` keeps all local orgs when
the server returns a successful empty list. This prevents a single hostile/transient empty response
from wiping local replicas, but it also prevents the legitimate purge when the user was removed from
their only org.

Do not simply delete the guard. Give the client an authenticated/authoritative distinction, for
example:

- a signed/session-bound membership snapshot with a monotonic revision; or
- targeted confirmation for each cached org after an empty list, requiring uniform member-gate 404
  before local purge; or
- two-phase quarantine followed by a confirmed purge, with org search disabled immediately.

Network errors, invalid sessions, and parse failures must continue to keep cached state without
destructive mutation.

### B2. Proof required

- valid empty membership response after last-org removal purges `org_state`, OCK cache, plaintext,
  attachments, chunks, FTS, and vectors;
- network/5xx/invalid JSON/expired session preserves the cached replica;
- a non-empty list that omits one of several orgs purges only the omitted org;
- no removed org remains searchable during any quarantine interval.

---

## PHASE C — Clarify and harden blob integrity

### C1. Fix the contract text first

There are two hashes:

- feed `contentSha256`: canonical **plaintext `OrgEnvelope`** hash, supplied by the client;
- `blobs.sha256`: opaque **ciphertext** hash, computed by the server during inline publish.

Correct `murmur-protocol/src/dto.rs::OrgItemEntry::content_sha256`, whose current comment incorrectly
calls the feed field a ciphertext hash. Keep client `share/org_dto.rs` and server migration/route
comments consistent.

### C2. Decide relay at-rest verification

`store::get_blob` does not return `blobs.sha256`, and `routes/blobs.rs::fetch` does not recompute it.
Client AES-GCM open plus post-open plaintext-hash comparison already fails closed, so this is
hardening/diagnostics rather than a silent-plaintext-ingest vulnerability.

Recommended small change:

1. load the stored ciphertext SHA with the blob;
2. recompute before serving;
3. collapse mismatch to a content-free failure and log only blob id/stage;
4. test DB corruption without returning corrupted bytes.

Do not compare feed `contentSha256` to ciphertext bytes; that field is required to reconstruct the
current AAD.

### C3. Migrate away from the plaintext-hash oracle separately

The relay-visible unkeyed plaintext hash is a confirm-the-guess oracle. Replacing it changes AAD and
mixed-version decrypt behavior. Before that change:

- move the shared org wire contract into the shared protocol boundary;
- add frozen cross-repo org-envelope vectors;
- define old/new reader behavior;
- use an OCK-keyed or random nonce;
- keep server ciphertext integrity separate from plaintext identity.

---

## PHASE D — Adopt revision-stable identity end to end

Server `docId` support is present, nullable, and metadata-only. The client still omits the field.

Required client slice:

1. add optional `doc_id` to local publish/feed DTOs;
2. create one stable random UUID per outbound source/share lineage;
3. send the same value on republish/edit;
4. persist `doc_id` on local org share/replica rows;
5. map `doc_id -> current live item_id` transactionally when a successor arrives;
6. make open viewer/pinned source/wikilink resolution follow the current live item;
7. preserve item-id behavior for legacy rows with no `docId`.

This slice does **not** require an envelope/AAD version bump. It exposes revision linkage metadata to
the relay, as documented in server migration `0009_org_doc_id.sql`.

Later, separately:

- signed author identity per revision;
- `parent_item_id` or opaque parent token;
- server CAS with 409 on stale parent;
- local revision history/diff and conflict UX.

Do not call server `docId` alone conflict protection or cryptographic lineage.

---

## PHASE E — Give users the controls the server already supports

### E1. Unshare

Wire existing `IpcService.revokeOrgShare` into the normal share/detail UI. Use the already-landed
crash-safe backend path and show `revoke_pending` vs complete honestly.

### E2. Delete org

Add the missing client HTTP method, Tauri command, IPC method, owner confirmation, and local purge
for server `DELETE /v1/orgs/{id}`.

### E3. Activity

Expose member-readable `GET /v1/orgs/{id}/audit` through one client method, command, and panel.
Keep events content-free and paginate from the server cursor.

### E4. Lock interaction

Decide explicitly whether "Lock anyway" leaves snapshots shared or requires revoke. Preserve the
locked-title read gate: do not make `folder_active_shares` leak sealed titles merely to improve audit
ergonomics.

### E5. Proof required

FE runtime coverage with mocked Tauri IPC:

- unshare shows pending/completed states and an open viewer closes on withdrawal;
- delete org removes it and local org content from every surface;
- audit shows publish/tombstone/member events without note content;
- a sealed-not-unlocked folder leaks no title or share existence detail.

---

## PHASE F — Catch up and signal

Current throughput remains four items per 60-second tick for one round-robin org, and background org
work pauses during recording.

Improvements:

1. bounded adaptive drain until caught up;
2. fair head checks across joined orgs;
3. immediate catch-up after recording stops;
4. unread/update state;
5. make "Refresh" terminology distinguish membership refresh from feed sync;
6. user-visible retry for pending outbound work.

Measure seeded 400-item and multi-org catch-up before/after. Do not infer performance from unit tests.

---

## PHASE G — One brain, with an egress boundary

Only after Phases A–E:

1. fuse org hits into selected vault/MCP retrieval as a distinct `SHARED BRAIN` provenance section;
2. add full-org-item MCP read only through the existing membership/context gates;
3. add an org-provenance field to the egress ledger;
4. require separate consent before colleague content reaches a cloud model;
5. keep `AssistantScope::Vault` free of org/connectors unless explicitly promoted;
6. keep graph/link/export/fact integration out until the lock-domain product decision is made.

This phase requires independent privacy/lock review because it expands where another person's
decrypted content can flow.

---

## PHASE H — Roles, history, and product extensions

Ordered later work:

1. server-enforced `reader|publisher|owner` ACLs, labelled honestly as non-cryptographic while all
   readers share the OCK;
2. ownership transfer/promote/demote;
3. signed org-item authorship;
4. parent CAS + history/diff + conflict UX;
5. OCK-sealed comments/annotations;
6. folder-scoped subscriptions only with standing-consent and lock-boundary design;
7. server quota/GC/rate-counter retention policy.

Real-time CRDT/OT remains out of scope: whole-document E2EE cells and a relay that cannot merge
plaintext make parent-CAS conflict handling the proportionate next step.

---

## Verification matrix for the next changes

| Claim | Minimum evidence |
|---|---|
| Rotation works | Server SQLx tests + client Rust tests + real multi-account deployed run |
| Removal purges locally | Rust lifecycle/DB tests across plaintext, attachments, FTS, vectors, OCK cache |
| Blob bytes are integrity-checked | Corrupt stored blob fixture; server refuses bytes; client tamper-open remains fail-closed |
| `docId` survives edit | Server + client mixed-version test; viewer/pinned link follows successor |
| UI controls work | Mocked-Tauri browser runtime test, not compilation alone |
| Org retrieval is privacy-safe | Egress-ledger/consent tests, isolation-tier tests, independent lock-security review |
| Sync is faster | Seeded backlog measurement, including during/after recording |

### Evidence present but not rerun during this refresh

- server: `org_two_account_feed_round_trip`,
  `org_item_blobs_are_membership_gated`,
  `org_key_grants_are_owner_only`,
  `org_member_removal_drops_key_grants`,
  `org_doc_id_links_revisions_and_survives_a_tombstone`;
- client:
  `reconcile_sweep_evicts_a_tombstone_the_live_cursor_can_never_see`,
  `revoke_org_share_evicts_the_local_replica_on_the_publishing_device`,
  `an_interrupted_revoke_leaves_a_re_drivable_row_not_a_live_orphaned_replica`,
  `evicting_an_org_item_purges_its_attachment_blobs_on_every_path`,
  and `e2e/org/org-item-withdrawn.spec.ts`.

No Rust, Angular, server SQLx, E2E, app-launch, or Railway scenario was
executed for this documentation refresh. The Harness control-plane selftests
listed in the task evidence are not product or server verification.

---

## Shipping order

| Ship | Contents | Exit condition |
|---|---|---|
| A | Rotation + active-generation enforcement | Multi-account removal and future-item decrypt verified |
| B | Authoritative membership-loss purge | Last-org removal purges; transient failure never wipes |
| C | Hash contract correction + optional relay verification | Plaintext/ciphertext semantics unambiguous; corruption fails closed |
| D | Client `docId` adoption | References survive a republish across mixed versions |
| E | Unshare, delete-org, audit, lock copy | User can see and control lifecycle from the app |
| F | Catch-up/unread | Backlog measurement meets an explicit target |
| G | Retrieval + egress provenance/consent | Privacy review passes and isolation tiers stay intact |
| H | Roles, signed lineage, history, comments | Each protocol change has vectors and compatibility proof |
