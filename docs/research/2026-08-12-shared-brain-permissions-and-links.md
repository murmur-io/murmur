# Shared Brain permissions and durable links

Date: 2026-08-12
Scope: received Shared Brain items, collaboration permissions, stable local links, zero-knowledge relay

## Decision

Ship one bounded collaboration model:

- every Shared Brain document is readable by active members of its organization;
- its author chooses **View only** (default) or **Can edit** for the other members;
- the original author and organization Owner retain management: change access and withdraw;
- an Editor may publish a new revision but cannot change access or withdraw the document;
- a user may create a **private local link** between any readable Shared Brain document and an
  unlocked local meeting, note, or document;
- anonymous bearer-link shares stay read-only.

Do not call the second level “Full access”. That phrase commonly implies sharing and permission
management, while Murmur has a separate organization Owner boundary. “Can edit” states the actual
capability and avoids granting more than the user selected.

## Why linking fails today

The picker is not the missing feature. It already returns Shared Brain candidates from the local,
membership-gated replica. The failure is after selection:

1. Angular explicitly rejects candidate kind `org` in `ConnectionsComponent`.
2. Rust `LinkKind` accepts only `meeting | note | document`.
3. Link write/read gates only understand folder-owned endpoints.
4. An org edit publishes a new immutable `itemId` and tombstones the previous item, so an item-id
   link would break on the next edit.
5. The org viewer does not mount the Connections panel.

The server protocol already contains the missing stable identity: optional opaque `docId`. It is
preserved across revisions and is intentionally not part of key derivation. The client has not
adopted it yet. Using that identifier does not require adding collaboration metadata to the encrypted
content envelope and avoids exposing local meeting IDs, vault paths, titles, or relationship edges
to the relay.

Accepted direct Mode-B shares are a different path: acceptance materializes a new owned local
meeting, so they already use normal local linking. This change targets Shared Brain replicas that
remain remote-owned.

## Permission model

| Actor | Read | Create private local link | Edit content | Change access | Withdraw |
|---|---:|---:|---:|---:|---:|
| Active member, View only | yes | yes | no | no | no |
| Active member, Can edit | yes | yes | yes | no | no |
| Original author | yes | yes | yes | yes | yes |
| Organization Owner | yes | yes | yes | yes | yes |
| Removed/non-member | no | no | no | no | no |
| Anonymous link recipient | link payload only | local-copy rules | no | no | no |

This is per-document access, not a new organization-wide admin hierarchy. The relay can enforce it
without plaintext: it already sees authenticated user IDs, org membership, opaque document/item IDs,
revision numbers, hashes, sizes, and timestamps. It still never receives title, markdown, transcript,
audio, vault path, or the user's private link graph.

The effective permission must be checked on every server mutation, not inferred from a UI flag.
OWASP's authorization guidance recommends deny-by-default and validation on each request. A viewer
write must fail even if a modified client calls the endpoint directly.

## Revision safety

The current author edit sequence is two independent requests: publish `rev + 1`, then tombstone the
old item. It has two correctness holes:

- two devices can both publish the same next revision;
- a crash between requests leaves two live heads.

Add an opaque document-head record keyed by `(orgId, docId)` and update it with compare-and-swap:

```text
PUT /v1/orgs/{orgId}/documents/{docId}
{
  expectedRev,
  contentCell,
  contentSha256,
  generation
}
```

The relay locks the head, repeats membership and permission checks inside the transaction, inserts
the encrypted next revision, re-sequences/tombstones the old feed row, destroys its ciphertext blob,
and advances the head. A stale `expectedRev` returns `409 Conflict`; the client retains the draft and
asks the user to reload instead of silently merging. This is sufficient for safe asynchronous
collaboration; CRDT/live co-editing is not required.

The encrypted content-envelope format is unchanged. This design does not rely on a specific envelope
version: all new collaboration fields are content-free relay metadata or local SQLCipher state.

## Local link model

Store an org endpoint in the existing SQLCipher `links` graph as:

```text
(kind = "org", id = orgId:docId)
```

The composite prevents two organizations that happen to reuse the same random `docId` from sharing
an endpoint. The relation is graph-only and private to the device. It does not copy the remote
markdown into the local `notes` table and does not create a second writable vault file. At display
time the gated reader resolves `(orgId, docId)` to the current live `itemId` and title.

Both endpoints must pass their own visibility gate. A private relation may connect a local item to a
Shared Brain item or two Shared Brain items (including items from different joined organizations):

- a local meeting/note/document must be unsealed or session-unlocked;
- each org document must belong to its own joined organization, have context enabled, and have a
  live authoritative-current decrypted replica;
- a tombstone, leave, revoked membership, or missing replica removes/hides the chip without exposing
  its former title;
- disabling Shared Brain context hides the link reversibly;
- creating a link never changes the target's permission and never grants another user access.

The relationship never grants access to its target. Keeping link identity separate from disclosure
authority also prevents a relation or parent surface from broadening effective visibility.

## Design rationale

- Separate read, edit, and management authority so every label describes its actual capability.
- Keep relationship identity independent from disclosure authority; a private link never grants
  access and cannot broaden the target's visibility.
- Prefer explicit compare-and-swap conflicts over implicit live merging for the first version.
- Defer comments and finer-grained roles until their key distribution, audit, and recovery models
  can be specified without weakening the zero-knowledge boundary.

## Rejected alternatives

1. **Link by current `itemId`.** Small change, but every edit breaks the relationship.
2. **Copy received markdown into My Brain automatically.** It creates two writable truths and can
   retain content after org access is revoked. A deliberate “Save a copy” fork may be added later.
3. **Anonymous editor links.** A bearer token lacks a stable accountable editor identity and granular
   revocation/provenance.
4. **Commenter, Admin, CRDT, or per-recipient ACL now.** Each expands protocol, key-management, merge,
   audit, and recovery semantics without being needed for the reported use case.
5. **Organization-wide Editor role only.** It is too coarse: joining one Shared Brain should not make
   every item writable. Permission belongs to the document being shared.

## Rollout order

Deploy the relay before releasing the client. The new server remains compatible with historical
publish requests and rows, while an older relay rejects the client's new `access` field. Once the
relay is live, old clients continue to read and publish with the least-privileged `view` default and
new clients can use stable-document CAS updates and permission management.

Compatibility is fail-closed at a feed-page boundary. A missing `access` field means `view`, matching
the relay's least-privileged default; an unknown access value rejects the response. For a durable
`docId`, `isCurrent` and `documentOwnerUserId` must both be present. If either is absent, the client
rejects the whole page before advancing its cursor or changing the local head, preserving the last
known-good replica. An explicit `isCurrent:false` remains an authoritative demotion. Historical items
without `docId` keep their legacy behavior and cannot acquire durable-document permissions.

## Verification bar

- protocol serialization compatibility and additive Postgres migration;
- server tests: viewer rejected, editor accepted, author/Owner management, stale CAS, exactly one
  concurrent winner, feed tombstone delivery, ciphertext destruction;
- Rust tests: `LinkKind::Org`, both-endpoint write/read gates, revision survival, sealed-local hiding,
  context disable/re-enable, tombstone/leave cleanup;
- mocked-Tauri Playwright: received candidate links, current-revision navigation, reverse link from
  org viewer, permission selector and edit controls, revoked item disappearance;
- independent adversarial verification and mandatory lock-security review.

## Historical pinned relay receipt (not Harness evidence)

The following is provenance from the separate relay implementation and its GitHub merge. It was
recorded by the implementation session, not executed by the client Harness runner, and must not be
used as a substitute for the checks bound into a client Harness attempt.

At the time of this historical receipt, the client pinned relay revision
`005dc4565955c0cfcf991d75488f4f4dc0f2cb12`. The current compatibility pin is
`3ef670d889c6feacbc81be567a0d9ad73c8aee5d`; that exact revision is verified separately by the
client Harness protocol check. The older permission foundation was verified in a clean
`murmur-server` worktree with this command completing with exit 0:

```text
env DATABASE_URL=postgres://postgres:postgres@localhost:5433/urc_dev \
  CARGO_BUILD_JOBS=2 cargo test --workspace -- --test-threads=1
```

That run passed 124 tests: protocol 24, golden 5, server unit 10, auth 13, auth_b 10, blobs 6,
organizations 20, shares 14, sharing_b 16, and quota 6. The organization suite includes the durable
permission/CAS/ciphertext lifecycle, exactly-one concurrent CAS winner, fail-closed permission
backfill, idempotent POST/PUT retries, revision-stable document links through tombstones,
organization-owner management of member-owned documents, legacy-delete/update races, legacy editor
compatibility, and document garbage collection.

The pinned revision also includes the follow-up that binds a revision-1 POST replay to the stable
owner and exact durable access. That follow-up passed 125 workspace tests locally and was merged as
server PR #19; its GitHub Actions `gate`, `supply-chain`, and `docker-build` checks all completed
successfully before merge. This corroborates the isolated local Postgres runs without substituting a
different source revision.

This is historical source/test evidence for the then-pinned relay revision only. It does not prove
that either revision has been deployed to Railway; relay deployment remains a release-order prerequisite,
and merging this client source is not a client release.

## Sources

- Murmur client: `src-tauri/src/commands/links.rs`, `src-tauri/src/storage/links.rs`,
  `src-tauri/src/commands/org.rs`, `src-tauri/src/storage/org_store.rs`,
  `src/app/shared/connections/connections.component.ts`,
  `src/app/features/org/org-item-viewer/org-item-viewer.component.ts`.
- Murmur server: `crates/murmur-protocol/src/dto.rs`,
  `crates/murmur-server/migrations/0009_org_doc_id.sql`,
  `crates/murmur-server/src/routes/orgs.rs`, `crates/murmur-server/src/store/orgs.rs`.
- [OWASP Authorization Cheat Sheet](https://cheatsheetseries.owasp.org/cheatsheets/Authorization_Cheat_Sheet.html)
- [RFC 9110: HTTP Semantics](https://www.rfc-editor.org/rfc/rfc9110)
