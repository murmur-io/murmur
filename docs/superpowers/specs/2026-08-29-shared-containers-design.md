# Shared containers — sharing whole Folders and Spaces to an Org

Date: 2026-08-29
Status: approved design, ready for implementation
Supersedes nothing; extends `2026-07-10-shared-brain-v1-spec.md` and
`docs/research/2026-08-12-shared-brain-permissions-and-links.md`.

## 1. What this adds

Today a user publishes ONE note or ONE meeting to an Org Brain. This design adds the container:
a user shares a whole **Folder** or a whole **Space**, and every eligible descendant — notes,
meeting notes, sub-folders — appears in every other member's app under the same structure, with a
single inherited permission.

It also replaces the standalone "Shared Brains" destination with a place in the Spaces sidebar:

- a received **Space** becomes its own top-level Space row;
- a received **Folder**, and every received item with no container at all, lives inside one virtual
  Space named **Shared Brains**;
- every shared row — sent or received — carries one subtle glyph saying so.

Dashboards are out of scope: a dashboard can reference items that were never shared, and no answer
to that is obviously right yet.

### Decisions taken by the user

| Question | Decision |
|---|---|
| Content added to an already-shared container | **Auto** — publishes itself; the folder is live |
| What a `Can edit` member may do | Edit the **content** of existing documents; container structure stays with the owner |
| Recipient rearranging received content | **Local and private** — a placement only that user sees |
| Owner deletes a note from a shared folder | **Withdrawn** for everyone; the folder mirrors the owner's tree |

### Decisions taken in this design

1. A **sealed container cannot be shared**. The sheet refuses, and sealed descendants are skipped
   and named in the preview.
2. **Sealing an already-shared container stops the share** — every published descendant is
   withdrawn, confirmed explicitly in the lock dialog. Publishing what you just sealed is not a
   coherent state.
3. **A meeting travels as its note only.** Transcript and audio never leave the device.
4. A single share publishes at most **500** items; past that the sheet refuses and says so.

## 2. The load-bearing decision: no server change

The relay stores ciphertext blobs, wrapped keys and opaque ids. Container structure is content, so
it belongs inside the OCK-sealed envelope — where the relay cannot read it, and where no new
endpoint, table or authorization rule is needed.

`OrgItemKind::Task` (tag 3) already establishes the shape: a structured item whose canonical JSON
lives in `OrgEnvelope.markdown` while the outer envelope owns the revision and the authenticated
attachment bundle. Containers reuse it exactly.

### 2.1 `OrgItemKind::Container` (tag 4)

A new kind and a matching `OrgSourceKind::Container` (tag 4). `markdown` carries a canonical
`ContainerEnvelope`:

```rust
pub struct ContainerEnvelope {
    pub v: u16,                              // CONTAINER_ENVELOPE_VERSION = 1
    pub container_id: String,                // stable, client-generated; equals the org docId
    pub level: ContainerLevel,               // Space | Folder
    pub name: String,
    pub emoji: Option<String>,
    pub tint: Option<String>,
    pub parent_container_id: Option<String>, // None at the share root
    pub position: i64,
}
```

`container_id` is opaque to the relay and is NEVER the local `folders.id` — a local identifier
would leak a stable cross-org correlator for the owner's vault.

### 2.2 Structure is a parent pointer, not a child list

Each content document carries its own placement; the manifest carries none.

```rust
// OrgEnvelope, new optional field group (v4)
pub struct OrgPlacement {
    pub parent_container_id: String,
    pub position: i64,
}
```

The alternative — a `children: Vec<docId>` list inside the manifest — makes every add, move and
remove a compare-and-swap against one contended document. With a parent pointer, adding a note is
one ordinary publish of that note, and the manifest changes only when the folder is renamed,
re-tinted or re-ordered. Ordering falls out of `position`, with `(position, created_at, title)` as
the recipient's stable sort.

### 2.3 Envelope version v4 is conditional

`ORG_ENVELOPE_VERSION` becomes 4, but `OrgEnvelope::new` keeps stamping the version it stamps
today. `with_placement(..)` upgrades to v4, exactly as `with_attachments(..)` upgrades to v3.

This matters for compatibility. A reader that cannot parse an envelope skips the item, so if every
republish suddenly emitted v4, a member on an older build would stop receiving updates to notes
they already have. Conditional versioning confines that cost to content that is genuinely new:
container-shared items require a current client, standalone shares keep working unchanged.

v4 is a superset of v3 — an envelope with both attachments and placement is v4 and carries both.

## 3. Local schema — additive, guarded, idempotent

All of it lands in `Db::migrate()` through `add_column_if_missing` and `CREATE TABLE IF NOT EXISTS`.
No destructive statement, no rewrite of a user row.

### 3.1 `org_container_shares` — the outbound journal (owner side)

```sql
CREATE TABLE IF NOT EXISTS org_container_shares (
  id             TEXT PRIMARY KEY,
  org_id         TEXT NOT NULL,
  folder_id      TEXT NOT NULL,            -- local folders.id
  container_id   TEXT NOT NULL,            -- stable manifest doc id
  access         TEXT NOT NULL DEFAULT 'view' CHECK(access IN ('view','edit')),
  scrub          INTEGER NOT NULL DEFAULT 1 CHECK(scrub IN (0,1)),
  is_root        INTEGER NOT NULL DEFAULT 0 CHECK(is_root IN (0,1)),
  state          TEXT NOT NULL DEFAULT 'queued'
                 CHECK(state IN ('queued','published','failed','revoke_pending','revoked')),
  item_id        TEXT,
  rev            INTEGER NOT NULL DEFAULT 1,
  generation     INTEGER NOT NULL DEFAULT 1,
  content_sha256 BLOB,
  position       INTEGER NOT NULL DEFAULT 0,
  last_error     TEXT,
  created_at     TEXT NOT NULL,
  updated_at     TEXT NOT NULL,
  UNIQUE(org_id, folder_id)
);
```

`is_root = 1` marks the container the user actually picked. Descendant folders get their own rows
with `is_root = 0`, so unsharing the root cascades and a descendant cannot be unshared on its own
while its root is live.

### 3.2 `org_shares` gains two columns

- `container_id TEXT` — the manifest this document is filed under, when the container sweep owns it.
- `explicit INTEGER NOT NULL DEFAULT 1` — 1 when the user shared this document themselves, 0 when
  it exists only because its container is shared.

Unsharing a container withdraws its `explicit = 0` rows and merely clears `container_id` on the
`explicit = 1` rows, so a note that was already shared standalone survives, unshared-container or
not. `DEFAULT 1` is correct for every pre-existing row: each of them came from an explicit
`Add to Org Brain`.

### 3.3 `org_containers` — the inbound manifest replica (recipient side)

```sql
CREATE TABLE IF NOT EXISTS org_containers (
  org_id                 TEXT NOT NULL,
  container_id           TEXT NOT NULL,
  item_id                TEXT NOT NULL,
  level                  TEXT NOT NULL CHECK(level IN ('space','folder')),
  name                   TEXT NOT NULL DEFAULT '',
  emoji                  TEXT,
  tint                   TEXT,
  parent_container_id    TEXT,
  position               INTEGER NOT NULL DEFAULT 0,
  access                 TEXT NOT NULL DEFAULT 'view' CHECK(access IN ('view','edit')),
  author_hint            TEXT NOT NULL DEFAULT '',
  author_user_id         TEXT,
  document_owner_user_id TEXT,
  seq                    INTEGER NOT NULL DEFAULT 0,
  rev                    INTEGER NOT NULL DEFAULT 1,
  generation             INTEGER NOT NULL DEFAULT 1,
  created_at             TEXT NOT NULL DEFAULT '',
  tombstoned             INTEGER NOT NULL DEFAULT 0 CHECK(tombstoned IN (0,1)),
  PRIMARY KEY (org_id, container_id)
);
```

### 3.4 `org_items` gains placement

`parent_container_id TEXT` and `position INTEGER NOT NULL DEFAULT 0`, read straight off the v4
envelope. NULL means "no container" — the honest state for every item published before this
existed and for every standalone share, never a guess.

### 3.5 `org_local_placements` — the recipient's private arrangement

```sql
CREATE TABLE IF NOT EXISTS org_local_placements (
  placement_key   TEXT PRIMARY KEY,          -- '<org_id>|c|<container_id>' | '<org_id>|d|<doc_id>'
  org_id          TEXT NOT NULL,
  target_kind     TEXT NOT NULL CHECK(target_kind IN ('container','doc')),
  target_id       TEXT NOT NULL,
  local_parent_id TEXT,                      -- folders.id; NULL = Shared Brains root
  position        INTEGER NOT NULL DEFAULT 0,
  updated_at      TEXT NOT NULL,
  UNIQUE(org_id, target_kind, target_id)
);
```

A row here changes where a received object is DRAWN in this user's sidebar and nothing else. It
never leaves the device, never reaches the relay, and never alters ownership: the content keeps
updating from the org feed exactly as before. Placing a received container under a local folder does
not put org content into that folder's lock domain — see §6.

## 4. Reading it back

### 4.1 The shared read model is separate from the gated local one

`workspace_store` and `visibility_clause` are untouched. A new read model answers a different
question over different tables:

```rust
pub struct SharedWorkspace {
    /// Received Spaces — each becomes its own top-level sidebar row.
    pub spaces: Vec<SharedContainerNode>,
    /// Received Folders with no shared-Space parent, plus every container-less received item.
    pub shared_brains: SharedContainerNode,
}

pub struct SharedContainerNode {
    pub container_id: Option<String>,   // None for the synthetic Shared Brains root
    pub org_id: String,
    pub org_name: String,
    pub name: String,
    pub level: SharedContainerLevel,    // space | folder | virtual
    pub emoji: Option<String>,
    pub tint: Option<String>,
    pub access: OrgItemAccess,
    pub author_hint: String,
    pub folders: Vec<SharedContainerNode>,
    pub items: Vec<SharedItemRow>,
    pub local_parent_id: Option<String>,  // this user's private placement
}
```

Every row respects `org_state.context_enabled` and `tombstoned = 0`, exactly like `list_org_items`.

### 4.2 The frontend merges; the backend does not

`SharedWorkspaceService` (root-provided, per `angular-zoneless.md` §8) holds the shared forest;
`workspace-tree` renders local rows and shared rows from two sources and interleaves them using
`local_parent_id`.

Merging in the backend would mean returning org content from `list_workspace_tree` — the gated
reader whose entire job is to answer what the folder lock permits. Org items deliberately live
outside that domain, and mixing the two would put content the gate does not govern behind a
function every reviewer reads as governed. Keeping the merge in the view layer leaves
`visibility_clause` and all its `*_visible` readers exactly as they are.

## 5. Publishing and keeping it in sync

### 5.1 `share_container_to_org(org_id, folder_id, access, scrub)`

Gate order mirrors `share_to_org_notifying`:

1. refuse if the container or the org is unavailable, or if the container is sealed;
2. consent, fail-closed;
3. enumerate descendants through the existing gated readers, skipping sealed ones and dashboards;
4. refuse over 500 items;
5. publish each manifest (root first, then descendants in tree order), then each document with its
   placement — every one sealed under the OCK, locally re-opened, and verified byte-identical
   before upload, exactly as a single-note share is today;
6. one content-free egress-ledger entry per published item.

Progress is emitted as a content-free `{done, total}` event so the sheet can show a real bar.

### 5.2 `reconcile_container_shares` — the sweep that keeps it live

Computes the desired set from the local tree and diffs it against the journal:

| Local change | Sweep action |
|---|---|
| Note created in a shared container | insert `org_shares(explicit = 0)`, publish |
| Note moved into a shared container | publish, or adopt an existing row and set `container_id` |
| Note moved out | withdraw when `explicit = 0`; clear `container_id` when `explicit = 1` |
| Note deleted | withdraw |
| Folder renamed / re-tinted / re-ordered | republish that manifest |
| Sub-folder created | publish a new manifest |
| Sub-folder deleted | tombstone its manifest and every descendant document |
| Container sealed | stop the share: withdraw everything under it |

Triggers: after a workspace mutation (`workspace_mutation_revision` already exists), on
`org_sync_now`, and on the launch sweep. Content republishing needs nothing new — the
`org_source_versions` triggers already mark a shared source dirty on edit.

### 5.3 Permissions

The manifest and every descendant publish with the same `access`. The relay already enforces
per-`docId` access, so no new authorization path appears anywhere. Changing a container's access
walks its documents and PATCHes each, which is why the change is an explicit action with its own
progress rather than a silent toggle.

On the recipient side `view` renders the whole subtree without edit affordances, and `edit` opens
documents in the editable org surface that `org_update_item` already backs. Structure affordances —
rename, delete, create-here — never appear on a received container regardless of access.

## 6. Lock model

Org items are org-disclosed content living outside the folder-lock domain in dedicated `org_*`
tables, protected at rest by whole-DB SQLCipher. This design adds tables to that same domain and
changes no gate.

The three invariants that must hold, and how:

1. **Every content read is gated.** The publish path reads local content only through the existing
   read-gated helpers, so a sealed meeting or note can never reach an envelope. A sealed container
   is refused before enumeration begins.
2. **No new ungated read.** `SharedWorkspace` reads `org_*` tables only. It returns no on-disk path
   for anything, so no `convertFileSrc` surface is opened.
3. **Verify before destroy.** Every new publish reuses `open_org_envelope` round-trip verification
   before upload. Nothing local is blanked or deleted by this feature at all.

A local placement row is a rendering hint. It does not move an org item into a local folder, does
not give it a `folder_id`, and cannot cause a locked folder's seal to cover — or fail to cover —
org content.

## 7. Frontend

| Surface | Change |
|---|---|
| Global rail | the Shared Brains entry is removed |
| `/shared-brains` | kept as the virtual Space's page, reached from the sidebar |
| `/shared/:orgId/:containerId` | new — a received container's page |
| Spaces sidebar | received Spaces as top-level rows; a virtual **Shared Brains** Space; shared glyph on every shared row |
| Space/Folder row menu | **Share to Org…**, and **Sharing…** once shared |
| `app-container-share-sheet` | new — org picker, View only / Can edit, one scrub toggle, a preview naming counts and skipped sealed items, progress |

The shared glyph needs no change to `mur-tree-row`: trailing affordances are already projected
content, which is how `unlocked-mark` renders. It is one dim glyph with the sentence in its
`title` and `aria-label` — "Shared to Siema · View only" when sent, "From Siema · kgm004a" when
received.

## 8. Verification

**Rust.** Canonical byte-identical round-trip for `ContainerEnvelope`; v1/v2/v3 envelopes still
parse unchanged and v4 round-trips; unknown versions and tags still fail closed; camelCase wire
assertions on every new IPC DTO (`rust-tauri.md` §2b); sweep diff tests for create, move-in,
move-out, delete, rename and seal; unshare cascade honouring `explicit`; a sealed descendant never
reaching the publish path, written RED first.

**Frontend.** `ng lint` and `ng build`; e2e covering the received Space row, the virtual Shared
Brains Space, the shared glyph, and a `view` subtree offering no edit affordance.

**Live.** Two accounts in one Org on a signed build: A shares a Space, B sees it with its content;
A adds a note, it appears for B; A deletes it, it disappears for B; B rearranges locally and A sees
no change. This is the one part that cannot be proven in CI.
