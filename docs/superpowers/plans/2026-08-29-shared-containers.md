# Shared Containers Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a user publish a whole Folder or Space to an Org Brain, so every member sees the same structure and content under one inherited permission, and rearrange received content privately in their own sidebar.

**Architecture:** Container structure travels as content inside the existing OCK-sealed `OrgEnvelope` — a new `OrgItemKind::Container` (tag 4) carrying a canonical `ContainerEnvelope` manifest, plus a conditional v4 envelope field group that gives each document a `parentContainerId` + `position`. Structure is a parent pointer, never a child list, so adding a note stays one ordinary publish instead of a compare-and-swap against a contended manifest. **No server change.**

**Tech Stack:** Rust / Tauri 2.11 (`meetnotes_lib`), rusqlite + SQLCipher, Angular 22 zoneless (signals, standalone), Playwright.

**Spec:** `docs/superpowers/specs/2026-08-29-shared-containers-design.md`

## Global Constraints

- Every fallible Rust fn returns `crate::error::Result<T>`; the only error type is `AppError`. No `unwrap()`/`expect()` outside tests.
- Every new `#[tauri::command]` MUST be added to `tauri::generate_handler![…]` in `src-tauri/src/lib.rs` or it is silently un-callable.
- Every DTO crossing IPC needs `#[serde(rename_all = "camelCase")]`; an enum with data-carrying variants needs **both** `rename_all` and `rename_all_fields`. Every new DTO needs a test asserting the SERIALIZED key names match `^[a-z][a-zA-Z0-9]*$` (rust-tauri.md §2b).
- Migrations are ADDITIVE and guarded (`add_column_if_missing`, `CREATE TABLE IF NOT EXISTS`). No `DROP`, no `ALTER … DROP COLUMN`, no `DELETE` of user rows. `migrate()` stays idempotent.
- No PII in logs — ids, stage names, counts, durations only. Never a folder name, note title, markdown, or key material.
- Every content read/export goes through `meeting_is_unlocked` (commands) or `visibility_clause` (db/MCP). No new ungated read path.
- Egress publishes only after `open_org_envelope` round-trips the sealed bytes locally (verify-before-egress).
- Angular: standalone, `OnPush`, `inject()`, signals only, `@if`/`@for` with `track`, `input()`/`output()`/`viewChild()`, no `setTimeout` in components (`afterNextRender(fn, { injector })`), `var(--token)` for every design value, floating overlays use `var(--surface-overlay)` + `backdrop-filter: none`, list state in a `providedIn: "root"` service.
- No new npm packages. No new Rust dependencies.
- Test loop is `(cd src-tauri && cargo test --lib)` — never `cargo clippy --all-targets` while iterating.
- Share cap: **500** items per container share. Sealed containers refuse; sealed descendants are skipped. Dashboards are never published.

---

### Task 1: `ContainerEnvelope` — the canonical manifest

**Files:**
- Create: `src-tauri/src/share/container_envelope.rs`
- Modify: `src-tauri/src/share/mod.rs` (add `pub mod container_envelope;`)

**Interfaces:**
- Consumes: nothing.
- Produces:
  ```rust
  pub const CONTAINER_ENVELOPE_VERSION: u16 = 1;
  pub const MAX_CONTAINER_NAME_BYTES: usize = 512;
  pub enum ContainerLevel { Space, Folder }          // as_str() -> "space" | "folder"
  pub struct ContainerEnvelope {
      pub v: u16,
      pub container_id: String,
      pub level: ContainerLevel,
      pub name: String,
      pub emoji: Option<String>,
      pub tint: Option<String>,
      pub parent_container_id: Option<String>,
      pub position: i64,
  }
  impl ContainerEnvelope {
      pub fn to_json(&self) -> String;
      pub fn from_json(s: &str) -> Result<Self>;
  }
  ```

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ContainerEnvelope {
        ContainerEnvelope {
            v: CONTAINER_ENVELOPE_VERSION,
            container_id: "c-1".into(),
            level: ContainerLevel::Space,
            name: "Klienci".into(),
            emoji: Some("📁".into()),
            tint: Some("teal".into()),
            parent_container_id: None,
            position: 3,
        }
    }

    #[test]
    fn round_trips_byte_identical() {
        let e = sample();
        let json = e.to_json();
        let back = ContainerEnvelope::from_json(&json).unwrap();
        assert_eq!(back, e);
        assert_eq!(back.to_json(), json);
    }

    #[test]
    fn wire_keys_are_camel_case() {
        let json = sample().to_json();
        assert!(json.contains("\"containerId\""));
        assert!(json.contains("\"parentContainerId\"") || sample().parent_container_id.is_none());
        assert!(!json.contains('_'));
    }

    #[test]
    fn unknown_version_fails_closed() {
        let json = r#"{"v":99,"containerId":"c","level":"space","name":"n","position":0}"#;
        assert!(ContainerEnvelope::from_json(json).is_err());
    }

    #[test]
    fn unknown_level_fails_closed() {
        let json = r#"{"v":1,"containerId":"c","level":"galaxy","name":"n","position":0}"#;
        assert!(ContainerEnvelope::from_json(json).is_err());
    }

    #[test]
    fn oversized_name_fails_closed() {
        let mut e = sample();
        e.name = "x".repeat(MAX_CONTAINER_NAME_BYTES + 1);
        assert!(ContainerEnvelope::from_json(&e.to_json()).is_err());
    }

    #[test]
    fn empty_container_id_fails_closed() {
        let json = r#"{"v":1,"containerId":"","level":"folder","name":"n","position":0}"#;
        assert!(ContainerEnvelope::from_json(json).is_err());
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `(cd src-tauri && cargo test --lib container_envelope)`
Expected: FAIL — module does not exist.

- [ ] **Step 3: Implement**

Serde struct with `#[serde(rename_all = "camelCase", deny_unknown_fields)]`; `level` as `#[serde(rename_all = "lowercase")]` enum; `emoji`/`tint`/`parent_container_id` with `skip_serializing_if = "Option::is_none"`. `from_json` validates: `v == CONTAINER_ENVELOPE_VERSION`, non-empty trimmed `container_id`, `name.len() <= MAX_CONTAINER_NAME_BYTES`, and that `parent_container_id`, when present, is non-empty. Every rejection is `AppError::InvalidArg` with a content-free message.

- [ ] **Step 4: Run to verify pass**

Run: `(cd src-tauri && cargo test --lib container_envelope)` → PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/share/container_envelope.rs src-tauri/src/share/mod.rs
git commit -m "feat: canonical ContainerEnvelope manifest for shared containers"
```

---

### Task 2: `OrgEnvelope` v4 — container kind and placement

**Files:**
- Modify: `src-tauri/src/share/org_envelope.rs`

**Interfaces:**
- Consumes: `ContainerEnvelope` (Task 1) — only as the payload string; no type dependency.
- Produces:
  ```rust
  pub const ORG_ENVELOPE_VERSION: u16 = 4;          // was 3
  const ORG_ENVELOPE_VERSION_V3: u16 = 3;           // new named constant for the old value
  OrgItemKind::Container                            // tag 4, as_str() -> "container"
  OrgSourceKind::Container                          // tag 4, as_str() -> "container"
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct OrgPlacement { pub parent_container_id: String, pub position: i64 }
  impl OrgEnvelope { pub fn with_placement(self, placement: Option<OrgPlacement>) -> Self; }
  pub struct OrgEnvelope { /* … */ pub placement: Option<OrgPlacement> }
  ```

**Canonical layout.** v4 appends, after the v3 attachment block: `has_placement(u8)`, and when 1, `len(u32 LE) || parent_container_id utf8` then `position(i64 LE)`. Nothing before it moves, so v1/v2/v3 bytes re-serialize byte-identically.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn v3_bytes_round_trip_unchanged_after_v4_exists() {
    let env = OrgEnvelope::new(
        OrgItemKind::Note, "t", "m", "hint", "2026-08-29T10:00:00Z", 1,
        OrgSourceKind::Document,
    );
    assert_eq!(env.version, 2, "text-only publish must stay on the v2 wire shape");
    assert!(env.placement.is_none());
    let bytes = env.to_canonical_bytes();
    assert_eq!(OrgEnvelope::from_canonical_bytes(&bytes).unwrap(), env);
}

#[test]
fn placement_upgrades_to_v4_and_round_trips() {
    let env = OrgEnvelope::new(
        OrgItemKind::Note, "t", "m", "hint", "2026-08-29T10:00:00Z", 1,
        OrgSourceKind::Document,
    )
    .with_placement(Some(OrgPlacement {
        parent_container_id: "c-1".into(),
        position: 7,
    }));
    assert_eq!(env.version, ORG_ENVELOPE_VERSION);
    let back = OrgEnvelope::from_canonical_bytes(&env.to_canonical_bytes()).unwrap();
    assert_eq!(back, env);
    assert_eq!(back.placement.unwrap().position, 7);
}

#[test]
fn with_placement_none_keeps_the_lower_wire_version() {
    let env = OrgEnvelope::new(
        OrgItemKind::Note, "t", "m", "hint", "now", 1, OrgSourceKind::Document,
    )
    .with_placement(None);
    assert_eq!(env.version, 2);
    assert!(env.placement.is_none());
}

#[test]
fn container_kind_and_source_kind_round_trip() {
    let env = OrgEnvelope::new(
        OrgItemKind::Container, "Klienci", "{}", "hint", "now", 1,
        OrgSourceKind::Container,
    );
    let back = OrgEnvelope::from_canonical_bytes(&env.to_canonical_bytes()).unwrap();
    assert_eq!(back.kind, OrgItemKind::Container);
    assert_eq!(back.source_kind, Some(OrgSourceKind::Container));
    assert_eq!(OrgItemKind::Container.as_str(), "container");
    assert_eq!(OrgSourceKind::Container.as_str(), "container");
}

#[test]
fn unknown_kind_tag_and_version_still_fail_closed() {
    assert!(OrgItemKind::from_tag(5).is_err());
    assert!(OrgSourceKind::from_tag(5).is_err());
    let mut bytes = OrgEnvelope::new(
        OrgItemKind::Note, "t", "m", "h", "now", 1, OrgSourceKind::Document,
    )
    .to_canonical_bytes();
    bytes[0] = 99;
    assert!(OrgEnvelope::from_canonical_bytes(&bytes).is_err());
}

#[test]
fn truncated_placement_fails_closed() {
    let env = OrgEnvelope::new(
        OrgItemKind::Note, "t", "m", "h", "now", 1, OrgSourceKind::Document,
    )
    .with_placement(Some(OrgPlacement { parent_container_id: "c".into(), position: 1 }));
    let bytes = env.to_canonical_bytes();
    assert!(OrgEnvelope::from_canonical_bytes(&bytes[..bytes.len() - 3]).is_err());
}
```

- [ ] **Step 2: Run to verify failure**

Run: `(cd src-tauri && cargo test --lib org_envelope)` → FAIL (`with_placement` undefined, `Container` variant missing).

- [ ] **Step 3: Implement**

Add `Container` to both enums (tag 4 both, `from_tag` accepts it). Add `placement: Option<OrgPlacement>` to the struct, defaulted `None` in `new()` and in `from_canonical_bytes` for versions < 4. `with_placement(Some(p))` sets `self.version = ORG_ENVELOPE_VERSION` and stores it; `with_placement(None)` clears and leaves the version alone. Rename the current `ORG_ENVELOPE_VERSION` usages inside `to_canonical_bytes`/`from_canonical_bytes` that gate the attachment block to `ORG_ENVELOPE_VERSION_V3`, then gate the new placement block on `>= ORG_ENVELOPE_VERSION`. Extend the accepted-version check to include v3 and v4. Add `take_i64` to the `Cursor` helper, mirroring `take_u32`. Bound `parent_container_id` at `MAX_CONTAINER_NAME_BYTES` on read.

- [ ] **Step 4: Run to verify pass**

Run: `(cd src-tauri && cargo test --lib org_envelope)` → PASS, including every pre-existing test in the module.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/share/org_envelope.rs
git commit -m "feat: OrgEnvelope v4 carries container kind and document placement"
```

---

### Task 3: Schema — five additive migrations

**Files:**
- Modify: `src-tauri/src/storage/db.rs` (inside `migrate_org_ingest`, after the existing `add_column_if_missing` block)
- Test: `src-tauri/src/storage/db_tests/` — extend the existing `migrate_is_idempotent` coverage with a new test file `container_share_tests.rs` (register it in the `db_tests` mod file).

**Interfaces:**
- Produces: tables `org_container_shares`, `org_containers`, `org_local_placements`; columns `org_items.parent_container_id`, `org_items.position`, `org_shares.parent_container_id`, `org_shares.position`, `org_shares.explicit`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn container_share_schema_is_present_and_idempotent() {
    let db = test_db();
    // Re-running migrate() must be a no-op.
    db.migrate().unwrap();
    db.migrate().unwrap();
    let conn = db.lock();
    for table in ["org_container_shares", "org_containers", "org_local_placements"] {
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                [table],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "{table} must exist exactly once");
    }
    for (table, column) in [
        ("org_items", "parent_container_id"),
        ("org_items", "position"),
        ("org_shares", "parent_container_id"),
        ("org_shares", "position"),
        ("org_shares", "explicit"),
    ] {
        let found: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name=?1"),
                [column],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(found, 1, "{table}.{column} must exist");
    }
}

#[test]
fn pre_existing_org_share_rows_default_to_explicit() {
    let db = test_db();
    {
        let conn = db.lock();
        conn.execute(
            "INSERT INTO org_shares(id,org_id,document_id,kind,state,created_at,updated_at)
             VALUES('s1','o1','d1','note','uploaded','t','t')",
            [],
        )
        .unwrap();
    }
    db.migrate().unwrap();
    let conn = db.lock();
    let explicit: i64 = conn
        .query_row("SELECT explicit FROM org_shares WHERE id='s1'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(explicit, 1, "an existing share was published deliberately");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `(cd src-tauri && cargo test --lib container_share_schema)` → FAIL (no such table).

- [ ] **Step 3: Implement**

Append to `migrate_org_ingest`, verbatim from the spec §3.1/§3.3/§3.5 DDL, plus:

```rust
Self::add_column_if_missing(conn, "org_items", "parent_container_id", "TEXT")?;
Self::add_column_if_missing(conn, "org_items", "position", "INTEGER NOT NULL DEFAULT 0")?;
Self::add_column_if_missing(conn, "org_shares", "parent_container_id", "TEXT")?;
Self::add_column_if_missing(conn, "org_shares", "position", "INTEGER NOT NULL DEFAULT 0")?;
Self::add_column_if_missing(
    conn,
    "org_shares",
    "explicit",
    "INTEGER NOT NULL DEFAULT 1 CHECK(explicit IN (0,1))",
)?;
conn.execute_batch(
    "CREATE INDEX IF NOT EXISTS idx_org_items_container
       ON org_items(org_id, parent_container_id);
     CREATE INDEX IF NOT EXISTS idx_org_containers_parent
       ON org_containers(org_id, parent_container_id);
     CREATE INDEX IF NOT EXISTS idx_org_container_shares_org
       ON org_container_shares(org_id);
     CREATE INDEX IF NOT EXISTS idx_org_shares_container
       ON org_shares(org_id, parent_container_id);",
)
.map_err(map_err)?;
```

Each block carries a comment naming why it is additive and what NULL means (honest "no container", never a guess) — matching the register of the surrounding migration comments.

- [ ] **Step 4: Run to verify pass**

Run: `(cd src-tauri && cargo test --lib)` → PASS (the whole suite, since `migrate()` runs in every DB test).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/storage/db.rs src-tauri/src/storage/db_tests/
git commit -m "feat: additive schema for shared containers, placement and private arrangement"
```

---

### Task 4: `container_store.rs` — storage for the three new tables

**Files:**
- Create: `src-tauri/src/storage/container_store.rs`
- Modify: `src-tauri/src/storage/mod.rs` (add `pub mod container_store;`), `src-tauri/src/storage/models.rs` (add the row structs)

**Interfaces:**
- Consumes: `Db` (`storage::db`), `ContainerLevel` (Task 1).
- Produces, all as `impl Db`:
  ```rust
  // outbound
  pub fn upsert_container_share(&self, row: &ContainerShareRow) -> Result<()>;
  pub fn get_container_share(&self, org_id: &str, folder_id: &str) -> Result<Option<ContainerShareRow>>;
  pub fn container_share_by_container(&self, org_id: &str, container_id: &str) -> Result<Option<ContainerShareRow>>;
  pub fn list_container_shares(&self, org_id: Option<&str>) -> Result<Vec<ContainerShareRow>>;
  pub fn list_container_share_roots(&self) -> Result<Vec<ContainerShareRow>>;
  pub fn set_container_share_state(&self, id: &str, state: &str, item_id: Option<&str>, rev: u32, content_sha256: Option<&[u8]>, last_error: Option<&str>, now: &str) -> Result<()>;
  pub fn set_container_share_access(&self, id: &str, access: &str, now: &str) -> Result<()>;
  pub fn delete_container_share(&self, id: &str) -> Result<()>;
  pub fn shared_container_folder_ids(&self, org_id: &str) -> Result<Vec<String>>;

  // inbound
  pub fn upsert_org_container(&self, row: &OrgContainerRow) -> Result<()>;
  pub fn tombstone_org_container(&self, org_id: &str, container_id: &str) -> Result<()>;
  pub fn list_org_containers(&self, org_id: &str) -> Result<Vec<OrgContainerRow>>;

  // private placement
  pub fn set_local_placement(&self, org_id: &str, target_kind: &str, target_id: &str, local_parent_id: Option<&str>, position: i64, now: &str) -> Result<()>;
  pub fn clear_local_placement(&self, org_id: &str, target_kind: &str, target_id: &str) -> Result<()>;
  pub fn list_local_placements(&self) -> Result<Vec<LocalPlacementRow>>;
  ```
  Row structs `ContainerShareRow`, `OrgContainerRow`, `LocalPlacementRow` in `storage/models.rs`, plain Rust (not IPC DTOs — no serde rename needed).

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn container_share_round_trips_and_upsert_is_idempotent() {
    let db = test_db();
    let row = ContainerShareRow {
        id: "cs1".into(), org_id: "o1".into(), folder_id: "f1".into(),
        container_id: "c1".into(), access: "view".into(), scrub: true, is_root: true,
        state: "queued".into(), item_id: None, rev: 1, generation: 1,
        content_sha256: None, position: 0, last_error: None,
        created_at: "t".into(), updated_at: "t".into(),
    };
    db.upsert_container_share(&row).unwrap();
    db.upsert_container_share(&row).unwrap();
    assert_eq!(db.list_container_shares(Some("o1")).unwrap().len(), 1);
    let got = db.get_container_share("o1", "f1").unwrap().unwrap();
    assert_eq!(got.container_id, "c1");
    assert!(got.is_root);
}

#[test]
fn container_share_roots_exclude_descendants() {
    let db = test_db();
    db.upsert_container_share(&share_row("cs1", "o1", "f1", "c1", true)).unwrap();
    db.upsert_container_share(&share_row("cs2", "o1", "f2", "c2", false)).unwrap();
    let roots = db.list_container_share_roots().unwrap();
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].folder_id, "f1");
}

#[test]
fn tombstoned_org_container_is_not_listed() {
    let db = test_db();
    db.upsert_org_container(&container_row("o1", "c1", None)).unwrap();
    assert_eq!(db.list_org_containers("o1").unwrap().len(), 1);
    db.tombstone_org_container("o1", "c1").unwrap();
    assert!(db.list_org_containers("o1").unwrap().is_empty());
}

#[test]
fn local_placement_is_one_row_per_target_and_clears() {
    let db = test_db();
    db.set_local_placement("o1", "container", "c1", Some("local-f"), 2, "t").unwrap();
    db.set_local_placement("o1", "container", "c1", Some("other"), 5, "t2").unwrap();
    let rows = db.list_local_placements().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].local_parent_id.as_deref(), Some("other"));
    assert_eq!(rows[0].position, 5);
    db.clear_local_placement("o1", "container", "c1").unwrap();
    assert!(db.list_local_placements().unwrap().is_empty());
}

#[test]
fn local_placement_rejects_an_unknown_target_kind() {
    let db = test_db();
    assert!(db.set_local_placement("o1", "galaxy", "x", None, 0, "t").is_err());
}
```

- [ ] **Step 2: Run to verify failure**

Run: `(cd src-tauri && cargo test --lib container_store)` → FAIL.

- [ ] **Step 3: Implement**

Plain rusqlite, `map_err(map_err)`, `ON CONFLICT … DO UPDATE` for the upserts. `set_local_placement` builds `placement_key` as `format!("{org_id}|{tag}|{target_id}")` with `tag` `"c"`/`"d"`, and rejects a `target_kind` outside `{"container","doc"}` with `AppError::InvalidArg`. `list_org_containers` filters `tombstoned = 0` and joins `org_state` on `context_enabled = 1`, exactly like `list_org_items`.

- [ ] **Step 4: Run to verify pass**

Run: `(cd src-tauri && cargo test --lib container_store)` → PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/storage/container_store.rs src-tauri/src/storage/mod.rs src-tauri/src/storage/models.rs
git commit -m "feat: storage for container shares, received containers and private placement"
```

---

### Task 5: Thread placement through the four publish sites

**Files:**
- Modify: `src-tauri/src/commands/org.rs` (lines ~6708, ~7512, ~7660, ~11477 — the four `OrgEnvelope::new` call sites), `src-tauri/src/storage/org_store.rs` (placement readers)

**Interfaces:**
- Consumes: `OrgPlacement`, `with_placement` (Task 2); `org_shares` / `org_items` placement columns (Task 3).
- Produces:
  ```rust
  // storage/org_store.rs
  pub fn org_share_placement(&self, share_id: &str) -> Result<Option<OrgPlacement>>;
  pub fn org_item_placement(&self, item_id: &str) -> Result<Option<OrgPlacement>>;
  pub fn set_org_share_placement(&self, share_id: &str, parent_container_id: Option<&str>, position: i64) -> Result<()>;
  // commands/org.rs — one new parameter, threaded from the single caller
  async fn publish_org_body_with_policy(/* … */, placement: Option<OrgPlacement>, /* … */)
  ```

**Why all four.** Site 1 is the initial publish. Sites 2 and 3 are the republish comparison envelope and the republish publish envelope — the comparison hash must be computed over the same fields as the publish hash, or every republish would look changed. Site 4 is `org_update_item`, where a member edits a received document: it must PRESERVE the placement the document already has, or one edit would silently evict the note from its shared folder.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn republish_comparison_envelope_includes_placement() {
    // Two envelopes identical except for placement must NOT share a content hash,
    // or a moved note would be seen as unchanged and never republished.
    let base = OrgEnvelope::new(
        OrgItemKind::Note, "t", "m", "h", "now", 1, OrgSourceKind::Document,
    );
    let placed = base.clone().with_placement(Some(OrgPlacement {
        parent_container_id: "c-1".into(),
        position: 0,
    }));
    assert_ne!(base.content_sha256(), placed.content_sha256());
}

#[test]
fn moving_a_note_between_containers_changes_the_content_hash() {
    let mk = |c: &str| {
        OrgEnvelope::new(OrgItemKind::Note, "t", "m", "h", "now", 1, OrgSourceKind::Document)
            .with_placement(Some(OrgPlacement {
                parent_container_id: c.into(),
                position: 0,
            }))
            .content_sha256()
    };
    assert_ne!(mk("c-1"), mk("c-2"));
}
```

Plus, in `src-tauri/src/commands/tests/`, a test that `org_update_item` on a placed document re-publishes an envelope whose placement is unchanged — asserted by reading `org_shares.parent_container_id` before and after and by decoding the sealed envelope.

- [ ] **Step 2: Run to verify failure**

Run: `(cd src-tauri && cargo test --lib placement)` → FAIL.

- [ ] **Step 3: Implement**

Add the three storage readers. In `publish_org_body_with_policy`, take `placement: Option<OrgPlacement>`, persist it onto the row via `set_org_share_placement` right after `set_org_share_document_metadata`, and append `.with_placement(placement.clone())` to the envelope builder. In the republish loop, read the placement from the already-loaded `row` (its `parent_container_id`/`position` columns) and apply it to BOTH `cmp_env` and `env`. In `org_update_item`, read `org_item_placement(&ctx.item_id)` and apply it to the new envelope.

- [ ] **Step 4: Run to verify pass**

Run: `(cd src-tauri && cargo test --lib)` → PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/commands/org.rs src-tauri/src/storage/org_store.rs
git commit -m "feat: carry document placement through publish, republish and org edit"
```

---

### Task 6: `commands/org_containers.rs` — publish and withdraw a manifest

**Files:**
- Create: `src-tauri/src/commands/org_containers.rs`
- Modify: `src-tauri/src/commands/mod.rs` (add `pub mod org_containers;` and re-export the commands)

**Interfaces:**
- Consumes: `ContainerEnvelope` (1), `OrgEnvelope`/`OrgItemKind::Container` (2), `container_store` (4), and from `commands/org.rs` these existing `pub(crate)` helpers — mark any that are private as `pub(crate)` in this task: `resolve_org`, `share_base_url`, `authenticated_org_actor`, `acquire_org_ock`, `org_item_nonce`, `org_author_hint`, `record_org_egress_ledger_entry` (the ledger call used by `publish_org_body_with_policy`).
- Produces:
  ```rust
  pub(crate) async fn publish_container_manifest(
      state: &AppState, org_id: &str, folder_id: &str, container_id: &str,
      level: ContainerLevel, name: &str, emoji: Option<&str>, tint: Option<&str>,
      parent_container_id: Option<&str>, position: i64,
      access: OrgItemAccess, scrub: bool, is_root: bool,
  ) -> Result<String>;                          // returns the server item id
  pub(crate) async fn withdraw_container_manifest(state: &AppState, share_id: &str) -> Result<()>;
  ```

**Sequence (mirrors `publish_org_body_with_policy`, minus what a manifest has no use for):** consent fail-closed → `resolve_org` → journal row `state='queued'` FIRST → build `ContainerEnvelope` → scrub the name when `scrub` is on (reuse the same redactor the note path uses) → wrap in `OrgEnvelope::new(OrgItemKind::Container, name, manifest_json, author_hint, created_at, rev, OrgSourceKind::Container)` → `seal_org_envelope` → `open_org_envelope` verify → size check against `MAX_ORG_ITEM_BLOB_BYTES` → `org_publish_item` with `doc_id = container_id` and the chosen `access` → flip the row to `published` with the returned `item_id` → one content-free egress-ledger entry. Any failure before dispatch flips the row to `failed` with a content-free reason, which the launch sweep retries.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn publishing_a_manifest_refuses_without_consent() {
    let (state, _tmp) = test_state_without_org_consent();
    let err = block_on(publish_container_manifest(
        &state, "o1", "f1", "c1", ContainerLevel::Space, "Klienci",
        None, None, None, 0, OrgItemAccess::View, true, true,
    ))
    .unwrap_err();
    assert!(matches!(err, AppError::Unavailable(_)));
    assert!(state.db.get_container_share("o1", "f1").unwrap().is_none(),
        "a refused publish must leave no journal row");
}

#[test]
fn a_queued_journal_row_exists_before_any_egress() {
    // The fake share client fails the upload; the row must survive as `failed`,
    // which is what makes a crash between seal and publish recoverable.
    let (state, _tmp) = test_state_with_failing_share_client();
    let _ = block_on(publish_container_manifest(
        &state, "o1", "f1", "c1", ContainerLevel::Folder, "Notatki",
        None, None, None, 0, OrgItemAccess::View, true, true,
    ));
    let row = state.db.get_container_share("o1", "f1").unwrap().unwrap();
    assert_eq!(row.state, "failed");
    assert!(row.item_id.is_none());
}

#[test]
fn manifest_seals_and_reopens_byte_identical() {
    let ock = [7u8; 32];
    let manifest = ContainerEnvelope {
        v: CONTAINER_ENVELOPE_VERSION, container_id: "c1".into(),
        level: ContainerLevel::Space, name: "Klienci".into(),
        emoji: None, tint: None, parent_container_id: None, position: 0,
    };
    let env = OrgEnvelope::new(
        OrgItemKind::Container, "Klienci", manifest.to_json(), "h", "now", 1,
        OrgSourceKind::Container,
    );
    let nonce = org_item_nonce(&env.content_sha256());
    let (ct, _) = seal_org_envelope(&ock, &env, "o1", &nonce).unwrap();
    let back = open_org_envelope(&ock, &ct, "o1", &nonce).unwrap();
    assert_eq!(back, env);
    assert_eq!(ContainerEnvelope::from_json(&back.markdown).unwrap(), manifest);
}

#[test]
fn a_manifest_sealed_for_one_org_never_opens_for_another() {
    let ock = [7u8; 32];
    let env = OrgEnvelope::new(
        OrgItemKind::Container, "n", "{}", "h", "now", 1, OrgSourceKind::Container,
    );
    let nonce = org_item_nonce(&env.content_sha256());
    let (ct, _) = seal_org_envelope(&ock, &env, "o1", &nonce).unwrap();
    assert!(open_org_envelope(&ock, &ct, "o2", &nonce).is_err());
}
```

- [ ] **Step 2: Run to verify failure**

Run: `(cd src-tauri && cargo test --lib org_containers)` → FAIL.

- [ ] **Step 3: Implement** the sequence above.

- [ ] **Step 4: Run to verify pass**

Run: `(cd src-tauri && cargo test --lib org_containers)` → PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/commands/org_containers.rs src-tauri/src/commands/mod.rs src-tauri/src/commands/org.rs
git commit -m "feat: publish and withdraw a container manifest through the org egress gates"
```

---

### Task 7: Enumerate a container — preview and the eligibility rules

**Files:**
- Modify: `src-tauri/src/commands/org_containers.rs`, `src-tauri/src/storage/container_store.rs`

**Interfaces:**
- Consumes: `Db::list_containers` (`workspace_store`), `visibility_clause`.
- Produces:
  ```rust
  pub(crate) struct ContainerPlan {
      pub containers: Vec<PlannedContainer>,   // root first, then descendants in tree order
      pub documents: Vec<PlannedDocument>,     // { meeting_id | document_id, parent_container_id, position }
      pub skipped_sealed: u32,
      pub skipped_dashboards: u32,
  }
  pub(crate) fn plan_container_share(state: &AppState, org_id: &str, folder_id: &str) -> Result<ContainerPlan>;
  pub const MAX_CONTAINER_SHARE_ITEMS: usize = 500;
  ```

**Eligibility, in order:** the root container itself must not be `locked`; a sealed descendant container and everything under it is skipped and counted; dashboards are skipped and counted; items come only from the gated readers, so a sealed item cannot appear. A plan whose `containers.len() + documents.len()` exceeds `MAX_CONTAINER_SHARE_ITEMS` is `AppError::InvalidArg`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn planning_a_sealed_container_refuses() {
    let (state, folder_id) = seeded_sealed_space();
    let err = plan_container_share(&state, "o1", &folder_id).unwrap_err();
    assert!(matches!(err, AppError::Locked(_)));
}

#[test]
fn a_sealed_subfolder_and_its_contents_are_skipped_not_published() {
    let (state, root, sealed_child_note_id) = seeded_space_with_sealed_subfolder();
    let plan = plan_container_share(&state, "o1", &root).unwrap();
    assert!(plan.skipped_sealed >= 1);
    assert!(
        !plan.documents.iter().any(|d| d.document_id.as_deref() == Some(&sealed_child_note_id)),
        "a sealed descendant must never reach the publish plan"
    );
}

#[test]
fn dashboards_are_never_planned() {
    let (state, root) = seeded_space_with_dashboard();
    let plan = plan_container_share(&state, "o1", &root).unwrap();
    assert_eq!(plan.skipped_dashboards, 1);
    assert!(plan.documents.is_empty());
}

#[test]
fn the_root_is_planned_before_its_descendants() {
    let (state, root) = seeded_space_with_nested_folders();
    let plan = plan_container_share(&state, "o1", &root).unwrap();
    assert_eq!(plan.containers[0].folder_id, root);
    for (i, c) in plan.containers.iter().enumerate().skip(1) {
        let parent = c.parent_folder_id.clone().unwrap();
        assert!(
            plan.containers[..i].iter().any(|p| p.folder_id == parent),
            "a child manifest must be planned after its parent"
        );
    }
}

#[test]
fn an_oversized_container_refuses_before_any_egress() {
    let (state, root) = seeded_space_with_items(MAX_CONTAINER_SHARE_ITEMS + 1);
    let err = plan_container_share(&state, "o1", &root).unwrap_err();
    assert!(matches!(err, AppError::InvalidArg(_)));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `(cd src-tauri && cargo test --lib plan_container_share)` → FAIL.

- [ ] **Step 3: Implement** a breadth-first walk over `list_containers`, filtering with the existing gated item readers, assigning each planned container a fresh `container_id` (`uuid::Uuid::new_v4()`) unless `get_container_share` already has one for that `(org, folder)`.

- [ ] **Step 4: Run to verify pass** → PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/commands/org_containers.rs src-tauri/src/storage/container_store.rs
git commit -m "feat: plan a container share, refusing sealed roots and skipping sealed descendants"
```

---

### Task 8: `share_container_to_org` and `unshare_container` commands

**Files:**
- Modify: `src-tauri/src/commands/org_containers.rs`, `src-tauri/src/lib.rs`, `src-tauri/src/events.rs`
- Test: `src-tauri/src/commands/tests/container_share_cmd_tests.rs` (new; register in the tests mod)

**Interfaces:**
- Produces:
  ```rust
  #[derive(Serialize)] #[serde(rename_all = "camelCase")]
  pub struct ContainerSharePreview {
      pub folder_id: String, pub name: String, pub level: String,
      pub note_count: u32, pub meeting_count: u32, pub folder_count: u32,
      pub skipped_sealed: u32, pub skipped_dashboards: u32, pub total_items: u32,
  }
  #[derive(Serialize)] #[serde(rename_all = "camelCase")]
  pub struct ContainerShareResult { pub container_id: String, pub published: u32, pub failed: u32 }

  #[tauri::command] pub async fn preview_container_share(state, org_id, folder_id) -> Result<ContainerSharePreview>;
  #[tauri::command] pub async fn share_container_to_org(app, state, org_id, folder_id, access, scrub) -> Result<ContainerShareResult>;
  #[tauri::command] pub async fn unshare_container(app, state, org_id, folder_id) -> Result<()>;
  #[tauri::command] pub async fn set_container_share_access(app, state, org_id, folder_id, access) -> Result<()>;
  #[tauri::command] pub fn list_container_share_status(state) -> Result<Vec<ContainerShareStatus>>;
  // events.rs
  pub fn emit_container_share_progress(app: &AppHandle, done: u32, total: u32);
  ```

**Unshare semantics.** Withdraw every descendant manifest and every `org_shares` row under the container that has `explicit = 0`; for `explicit = 1` rows, clear `parent_container_id` and leave the item live. Then delete the container-share rows.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn every_new_dto_serializes_camel_case() {
    let preview = ContainerSharePreview {
        folder_id: "f".into(), name: "n".into(), level: "space".into(),
        note_count: 1, meeting_count: 2, folder_count: 3,
        skipped_sealed: 0, skipped_dashboards: 0, total_items: 6,
    };
    let value = serde_json::to_value(&preview).unwrap();
    for key in value.as_object().unwrap().keys() {
        assert!(
            !key.contains('_') && key.chars().next().unwrap().is_ascii_lowercase(),
            "{key} must be camelCase on the wire"
        );
    }
    let result = ContainerShareResult { container_id: "c".into(), published: 1, failed: 0 };
    for key in serde_json::to_value(&result).unwrap().as_object().unwrap().keys() {
        assert!(!key.contains('_'), "{key} must be camelCase on the wire");
    }
}

#[test]
fn unsharing_withdraws_implicit_rows_and_keeps_explicit_ones() {
    let (state, folder_id, implicit_doc, explicit_doc) = seeded_shared_space_with_both();
    block_on(unshare_container_inner(&state, "o1", &folder_id)).unwrap();
    let implicit = state.db.org_shares_for_source(None, Some(&implicit_doc)).unwrap();
    assert!(implicit.iter().all(|r| r.state == "revoked" || r.state == "revoke_pending"));
    let explicit = state.db.org_shares_for_source(None, Some(&explicit_doc)).unwrap();
    assert_eq!(explicit[0].state, "uploaded", "an explicit share outlives its container");
    assert!(explicit[0].parent_container_id.is_none());
}

#[test]
fn sharing_a_container_refuses_when_it_is_sealed() {
    let (state, folder_id) = seeded_sealed_space();
    let err = block_on(share_container_to_org_inner(
        &state, "o1", &folder_id, OrgItemAccess::View, true,
    ))
    .unwrap_err();
    assert!(matches!(err, AppError::Locked(_)));
    assert!(state.db.list_container_shares(Some("o1")).unwrap().is_empty());
}

#[test]
fn changing_access_walks_every_descendant() {
    let (state, folder_id, doc_ids) = seeded_shared_space_with_three_notes();
    block_on(set_container_share_access_inner(&state, "o1", &folder_id, OrgItemAccess::Edit)).unwrap();
    for id in doc_ids {
        let row = state.db.org_shares_for_source(None, Some(&id)).unwrap().remove(0);
        assert_eq!(row.access, "edit");
    }
    assert_eq!(state.db.get_container_share("o1", &folder_id).unwrap().unwrap().access, "edit");
}
```

- [ ] **Step 2: Run to verify failure** → FAIL.

- [ ] **Step 3: Implement.** `share_container_to_org` runs `plan_container_share`, then publishes each manifest in plan order, then each document through `share_to_org_notifying` extended with the placement and `explicit = 0`, emitting `emit_container_share_progress` after each. Register all five commands in `lib.rs` `generate_handler!`.

- [ ] **Step 4: Run to verify pass** → PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/commands/org_containers.rs src-tauri/src/lib.rs src-tauri/src/events.rs src-tauri/src/commands/tests/
git commit -m "feat: share, unshare and re-permission a whole container"
```

---

### Task 9: Ingest — receive container manifests and placement

**Files:**
- Modify: `src-tauri/src/commands/org.rs` (the reconcile loop's `ReconcileAction::Ingest` arm and its `ReconcileAction` enum), `src-tauri/src/storage/org_store.rs` (`upsert_org_item` gains placement)

**Interfaces:**
- Consumes: `ContainerEnvelope` (1), `org_containers` (4).
- Produces: a feed item whose `OrgEnvelope.kind == Container` lands in `org_containers` instead of `org_items`; a tombstone for such an item calls `tombstone_org_container`; every other item's `placement` is written to `org_items.parent_container_id` / `.position`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn a_container_item_lands_in_org_containers_not_org_items() {
    let (state, _tmp) = test_state();
    let manifest = ContainerEnvelope { /* space "Klienci", container_id "c1" */ };
    ingest_test_item(&state, "o1", "item-1", OrgItemKind::Container, &manifest.to_json());
    assert_eq!(state.db.list_org_containers("o1").unwrap().len(), 1);
    assert!(state.db.list_org_items("o1").unwrap().is_empty());
}

#[test]
fn a_malformed_manifest_is_skipped_and_never_half_written() {
    let (state, _tmp) = test_state();
    ingest_test_item(&state, "o1", "item-1", OrgItemKind::Container, "{not json");
    assert!(state.db.list_org_containers("o1").unwrap().is_empty());
    assert!(state.db.list_org_items("o1").unwrap().is_empty());
}

#[test]
fn a_documents_placement_is_ingested() {
    let (state, _tmp) = test_state();
    ingest_test_placed_note(&state, "o1", "item-2", "c1", 4);
    let item = state.db.get_org_item("item-2").unwrap().unwrap();
    assert_eq!(item.parent_container_id.as_deref(), Some("c1"));
    assert_eq!(item.position, 4);
}

#[test]
fn withdrawing_a_container_tombstones_it_locally() {
    let (state, _tmp) = test_state();
    ingest_test_item(&state, "o1", "item-1", OrgItemKind::Container, &manifest_json());
    ingest_test_tombstone(&state, "o1", "item-1");
    assert!(state.db.list_org_containers("o1").unwrap().is_empty());
}

#[test]
fn an_orphan_placement_does_not_hide_the_document() {
    // A note whose parent manifest has not arrived yet must still be reachable —
    // it falls back to Shared Brains rather than vanishing.
    let (state, _tmp) = test_state();
    ingest_test_placed_note(&state, "o1", "item-2", "not-yet-synced", 0);
    let ws = build_shared_workspace(&state).unwrap();
    assert_eq!(ws.shared_brains.items.len(), 1);
}
```

- [ ] **Step 2: Run to verify failure** → FAIL.

- [ ] **Step 3: Implement.** In the reconcile APPLY loop, branch on `env.kind == OrgItemKind::Container`: parse the manifest, and on parse failure record `ReconcileAction::Skip` (never a partial write). Feed `env.placement` into `upsert_org_item` for every other kind. Tombstones branch on whether the item id is known to `org_containers`.

- [ ] **Step 4: Run to verify pass** → PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/commands/org.rs src-tauri/src/storage/org_store.rs
git commit -m "feat: ingest container manifests and document placement from the org feed"
```

---

### Task 10: The reconcile sweep — keep a shared container live

**Files:**
- Modify: `src-tauri/src/commands/org_containers.rs`, `src-tauri/src/commands/org.rs` (call it from `org_sync_now` and the launch sweep), `src-tauri/src/commands/workspace.rs` (call it after a workspace mutation), `src-tauri/src/commands/lock.rs` (stop the share when a shared container is sealed)

**Interfaces:**
- Produces: `pub(crate) async fn reconcile_container_shares(state: &AppState, app: Option<&AppHandle>) -> Result<u32>` — returns the number of mutations it performed.

**The diff, per spec §5.2:** for each `is_root` share, re-plan the container and compare with the journal. New folder → publish a manifest. New document → publish with `explicit = 0`. Document no longer in the container → withdraw when `explicit = 0`, else clear `parent_container_id`. Folder gone → tombstone its manifest and everything under it. Name/emoji/tint/position changed → republish that manifest. Root sealed → unshare the whole thing.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn a_note_created_in_a_shared_folder_is_published() {
    let (state, folder_id) = seeded_shared_space();
    let note = create_note_in(&state, &folder_id, "Nowa");
    block_on(reconcile_container_shares(&state, None)).unwrap();
    let row = state.db.org_shares_for_source(None, Some(&note)).unwrap().remove(0);
    assert_eq!(row.explicit, 0, "it exists only because its container is shared");
    assert_eq!(row.parent_container_id.as_deref(), Some(container_id_of(&state, &folder_id).as_str()));
}

#[test]
fn a_note_moved_out_of_a_shared_folder_is_withdrawn() {
    let (state, folder_id, note, elsewhere) = seeded_shared_space_with_note_and_target();
    move_note(&state, &note, &elsewhere);
    block_on(reconcile_container_shares(&state, None)).unwrap();
    let row = state.db.org_shares_for_source(None, Some(&note)).unwrap().remove(0);
    assert!(row.state == "revoked" || row.state == "revoke_pending");
}

#[test]
fn a_deleted_note_is_withdrawn() {
    let (state, folder_id, note) = seeded_shared_space_with_note();
    delete_note(&state, &note);
    block_on(reconcile_container_shares(&state, None)).unwrap();
    assert!(state
        .db
        .org_shares_for_source(None, Some(&note))
        .unwrap()
        .iter()
        .all(|r| r.state != "uploaded"));
}

#[test]
fn renaming_a_shared_folder_republishes_only_its_manifest() {
    let (state, folder_id, note) = seeded_shared_space_with_note();
    let note_rev_before = state.db.org_shares_for_source(None, Some(&note)).unwrap()[0].rev;
    rename_folder(&state, &folder_id, "Klienci 2026");
    block_on(reconcile_container_shares(&state, None)).unwrap();
    let share = state.db.get_container_share("o1", &folder_id).unwrap().unwrap();
    assert_eq!(share.rev, 2);
    assert_eq!(
        state.db.org_shares_for_source(None, Some(&note)).unwrap()[0].rev,
        note_rev_before,
        "an unchanged note must not be republished by a folder rename"
    );
}

#[test]
fn sealing_a_shared_container_stops_the_share() {
    let (state, folder_id, note) = seeded_shared_space_with_note();
    seal_folder(&state, &folder_id);
    block_on(reconcile_container_shares(&state, None)).unwrap();
    assert!(state.db.get_container_share("o1", &folder_id).unwrap().is_none());
    assert!(state
        .db
        .org_shares_for_source(None, Some(&note))
        .unwrap()
        .iter()
        .all(|r| r.state != "uploaded"));
}

#[test]
fn the_sweep_is_idempotent() {
    let (state, folder_id) = seeded_shared_space();
    let first = block_on(reconcile_container_shares(&state, None)).unwrap();
    let second = block_on(reconcile_container_shares(&state, None)).unwrap();
    assert_eq!(second, 0, "a settled container must produce no further mutations");
    let _ = first;
}
```

- [ ] **Step 2: Run to verify failure** → FAIL.

- [ ] **Step 3: Implement** the diff, then wire the four call sites.

- [ ] **Step 4: Run to verify pass** → PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/commands/
git commit -m "feat: reconcile sweep keeps a shared container live as its contents change"
```

---

### Task 11: `list_shared_workspace` read model and private placement command

**Files:**
- Modify: `src-tauri/src/storage/container_store.rs`, `src-tauri/src/commands/org_containers.rs`, `src-tauri/src/lib.rs`

**Interfaces:**
- Produces:
  ```rust
  #[derive(Serialize)] #[serde(rename_all = "camelCase")]
  pub struct SharedItemRow { pub doc_id: String, pub item_id: String, pub title: String,
      pub kind: Option<String>, pub author_hint: String, pub created_at: String,
      pub access: String, pub org_id: String, pub org_name: String, pub position: i64 }
  #[derive(Serialize)] #[serde(rename_all = "camelCase")]
  pub struct SharedContainerNode { pub container_id: Option<String>, pub org_id: String,
      pub org_name: String, pub name: String, pub level: String, pub emoji: Option<String>,
      pub tint: Option<String>, pub access: String, pub author_hint: String,
      pub folders: Vec<SharedContainerNode>, pub items: Vec<SharedItemRow>,
      pub local_parent_id: Option<String>, pub position: i64 }
  #[derive(Serialize)] #[serde(rename_all = "camelCase")]
  pub struct SharedWorkspace { pub spaces: Vec<SharedContainerNode>, pub shared_brains: SharedContainerNode }

  #[tauri::command] pub fn list_shared_workspace(state) -> Result<SharedWorkspace>;
  #[tauri::command] pub fn set_shared_placement(state, org_id, target_kind, target_id, local_parent_id: Option<String>, position: i64) -> Result<()>;
  #[tauri::command] pub fn clear_shared_placement(state, org_id, target_kind, target_id) -> Result<()>;
  ```

**Assembly.** Read `org_containers` per enabled org, build the forest by `parent_container_id`. A `level='space'` root becomes a `spaces` entry; a `level='folder'` root (no parent in the replica) and every container-less `org_items` row go into `shared_brains`, whose `container_id` is `None` and `level` is `"virtual"`. Apply `org_local_placements` last: a placed node keeps its subtree but reports `local_parent_id`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn a_received_space_is_a_top_level_node_and_a_folder_is_not() {
    let (state, _tmp) = test_state_with_received_space_and_loose_folder();
    let ws = list_shared_workspace_inner(&state).unwrap();
    assert_eq!(ws.spaces.len(), 1);
    assert_eq!(ws.spaces[0].level, "space");
    assert_eq!(ws.shared_brains.folders.len(), 1, "a loose folder lives in Shared Brains");
    assert_eq!(ws.shared_brains.level, "virtual");
    assert!(ws.shared_brains.container_id.is_none());
}

#[test]
fn a_disabled_org_contributes_nothing() {
    let (state, _tmp) = test_state_with_received_space();
    state.db.set_org_context_enabled("o1", false).unwrap();
    let ws = list_shared_workspace_inner(&state).unwrap();
    assert!(ws.spaces.is_empty());
    assert!(ws.shared_brains.items.is_empty());
    assert!(ws.shared_brains.folders.is_empty());
}

#[test]
fn no_node_or_row_carries_an_on_disk_path() {
    let (state, _tmp) = test_state_with_received_space();
    let json = serde_json::to_string(&list_shared_workspace_inner(&state).unwrap()).unwrap();
    assert!(!json.contains("/Users/"));
    assert!(!json.contains(".wav"));
    assert!(!json.contains(".md"));
}

#[test]
fn every_shared_workspace_key_is_camel_case() {
    let (state, _tmp) = test_state_with_received_space();
    let value = serde_json::to_value(list_shared_workspace_inner(&state).unwrap()).unwrap();
    fn walk(v: &serde_json::Value) {
        match v {
            serde_json::Value::Object(map) => {
                for (k, child) in map {
                    assert!(!k.contains('_'), "{k} must be camelCase on the wire");
                    walk(child);
                }
            }
            serde_json::Value::Array(items) => items.iter().for_each(walk),
            _ => {}
        }
    }
    walk(&value);
}

#[test]
fn a_private_placement_is_reported_and_never_leaves_the_device() {
    let (state, _tmp) = test_state_with_received_space();
    state.db.set_local_placement("o1", "container", "c1", Some("local-folder"), 2, "t").unwrap();
    let ws = list_shared_workspace_inner(&state).unwrap();
    assert_eq!(ws.spaces[0].local_parent_id.as_deref(), Some("local-folder"));
    // A placement mutates no share journal row, so nothing is queued for egress.
    assert!(state.db.list_container_shares(None).unwrap().is_empty());
}

#[test]
fn a_cycle_in_the_replica_cannot_hang_the_reader() {
    let (state, _tmp) = test_state();
    seed_container(&state, "o1", "a", Some("b"));
    seed_container(&state, "o1", "b", Some("a"));
    let ws = list_shared_workspace_inner(&state).unwrap();
    assert!(ws.spaces.is_empty(), "a cycle contributes no root");
}
```

- [ ] **Step 2: Run to verify failure** → FAIL.

- [ ] **Step 3: Implement.** Build with an explicit visited-set so a malicious or corrupt parent cycle terminates. Register all three commands in `lib.rs`.

- [ ] **Step 4: Run to verify pass** → PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/storage/container_store.rs src-tauri/src/commands/org_containers.rs src-tauri/src/lib.rs
git commit -m "feat: shared-workspace read model and private placement commands"
```

---

### Task 12: Frontend models and IPC methods

**Files:**
- Modify: `src/app/core/models.ts`, `src/app/core/ipc.service.ts`

**Interfaces:**
- Produces (TypeScript mirrors of Task 8 and 11 DTOs):
  ```ts
  export interface SharedItemRow { docId: string; itemId: string; title: string;
    kind: "document" | "meeting" | null; authorHint: string; createdAt: string;
    access: OrgAccess; orgId: string; orgName: string; position: number; }
  export interface SharedContainerNode { containerId: string | null; orgId: string;
    orgName: string; name: string; level: "space" | "folder" | "virtual";
    emoji: string | null; tint: string | null; access: OrgAccess; authorHint: string;
    folders: SharedContainerNode[]; items: SharedItemRow[];
    localParentId: string | null; position: number; }
  export interface SharedWorkspace { spaces: SharedContainerNode[]; sharedBrains: SharedContainerNode; }
  export interface ContainerSharePreview { folderId: string; name: string; level: string;
    noteCount: number; meetingCount: number; folderCount: number;
    skippedSealed: number; skippedDashboards: number; totalItems: number; }
  export interface ContainerShareResult { containerId: string; published: number; failed: number; }
  export interface ContainerShareStatus { orgId: string; orgName: string; folderId: string;
    containerId: string; access: OrgAccess; isRoot: boolean; state: string; }
  ```
  and on `IpcService`: `listSharedWorkspace()`, `previewContainerShare(orgId, folderId)`, `shareContainerToOrg(orgId, folderId, access, scrub)`, `unshareContainer(orgId, folderId)`, `setContainerShareAccess(orgId, folderId, access)`, `listContainerShareStatus()`, `setSharedPlacement(...)`, `clearSharedPlacement(...)`, `onContainerShareProgress(cb)`.

- [ ] **Step 1: Add the interfaces to `models.ts`** with doc comments naming what `null` means (never a guess).

- [ ] **Step 2: Add one typed method per command to `ipc.service.ts`**, following the existing `invoke<T>("cmd", { camelCaseArgs })` shape, plus one `listen`-based `onContainerShareProgress` returning `Promise<UnlistenFn>`.

- [ ] **Step 3: Run the gates**

Run: `npx ng lint && npx ng build` → clean.

- [ ] **Step 4: Commit**

```bash
git add src/app/core/models.ts src/app/core/ipc.service.ts
git commit -m "feat: frontend models and IPC for shared containers"
```

---

### Task 13: `SharedWorkspaceService` and the merged sidebar tree

**Files:**
- Create: `src/app/services/shared-workspace.service.ts`
- Modify: `src/app/features/workspace/workspace-tree/workspace-tree.component.ts` + `.html` + `.scss`

**Interfaces:**
- Consumes: `IpcService` (Task 12).
- Produces:
  ```ts
  @Injectable({ providedIn: "root" })
  export class SharedWorkspaceService {
    readonly spaces: Signal<SharedContainerNode[]>;
    readonly sharedBrains: Signal<SharedContainerNode | null>;
    readonly loading: Signal<boolean>;
    readonly containerShares: Signal<ContainerShareStatus[]>;   // owner-side badges
    load(): Promise<void>;
    place(target, localParentId, position): Promise<void>;
  }
  ```

**Rules.** Root-provided, so the tree survives the sidebar's destroy/recreate cycle (`angular-zoneless.md` §8). Subscribes `onOrgFeedUpdated` ONCE and re-loads. Loading is a hint, never a render gate — the template keeps showing cached rows.

The tree component gains a `sharedLines()` computed that appends, after the local lines: each received Space as a top-level row, then the virtual Shared Brains Space, each with the same `mur-tree-row` shape. A node whose `localParentId` matches a local container is rendered under that container instead.

- [ ] **Step 1: Write the service** with `_spaces`/`_sharedBrains`/`_loading` private writable signals published `.asReadonly()`, a `loadSeq` stale guard, and the feed subscription in the constructor with `DestroyRef.onDestroy` teardown — the exact shape of `OrgBrainService`.

- [ ] **Step 2: Extend `lines()`** in the tree component to interleave shared nodes, keying every `@for` on a stable `line.key`.

- [ ] **Step 3: Render the shared glyph.** Project into `mur-tree-row` (no change to that component — trailing affordances are already `<ng-content>`, which is how `unlocked-mark` works):

```html
@if (sharedMark(line); as mark) {
  <span class="shared-mark" role="img" [attr.aria-label]="mark" [title]="mark">
    <svg viewBox="0 0 16 16" width="12" height="12" fill="none" aria-hidden="true">
      <circle cx="8" cy="4.6" r="2.1" stroke="currentColor" stroke-width="1.3" />
      <path d="M3.4 12.6a4.6 4.6 0 0 1 9.2 0" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" />
    </svg>
  </span>
}
```

```scss
.shared-mark {
  display: inline-flex;
  align-items: center;
  color: var(--text-dim);
  opacity: 0.75;
}
```

`sharedMark()` returns `"Shared to Siema · View only"` for an owned shared container and `"From Siema · kgm004a"` for a received one — one glyph, the sentence in the tooltip and for a screen reader, exactly the lesson the `unlocked-mark` comment records.

- [ ] **Step 4: Run the gates**

Run: `npx ng lint && npx ng build` → clean, including the 16 kB per-component style budget.

- [ ] **Step 5: Commit**

```bash
git add src/app/services/shared-workspace.service.ts src/app/features/workspace/workspace-tree/
git commit -m "feat: shared containers in the Spaces sidebar with a subtle shared glyph"
```

---

### Task 14: The container share sheet and the row menu

**Files:**
- Create: `src/app/features/workspace/container-share-sheet/container-share-sheet.component.{ts,html,scss}`
- Modify: `src/app/features/workspace/workspace-tree/workspace-tree.component.{ts,html}`

**Interfaces:**
- Consumes: `IpcService`, `SharedWorkspaceService`.
- Produces: `<app-container-share-sheet [container]="…" (shared)="…" (cancelled)="…" />`, with inputs `container: { id, name, level }`.

**Shape.** Modelled on `org-share-sheet.component.ts`: OPAQUE `var(--surface-overlay)` + `backdrop-filter: none` + `var(--border-strong)` + `var(--shadow-lg)` (trap T3 — never the frosted `.card`). Org picker via `<mur-select>`, collapsing to a label in a single-org account. Two access cards — **View only** / **Can edit** — with the copy from the shipped note sheet. One scrub toggle, default ON, applying to the whole container. A preview line built from `previewContainerShare`: counts, plus "N sealed items skipped" and "Dashboards aren't shared yet" when non-zero. A determinate progress bar driven by `onContainerShareProgress`.

- [ ] **Step 1: Write the component** — standalone, `OnPush`, `inject()`, signals, `afterNextRender(fn, { injector })` for the initial focus, `@if`/`@for`, `var(--token)` for every value.

- [ ] **Step 2: Wire the row menu.** In the container row menu, under the existing manage group, add:

```html
@if (canShare(line.container)) {
  <button type="button" class="menu-item" role="menuitem" (click)="openShare(line.container)">
    <mur-icon icon="shared-brains" />
    {{ isShared(line.container) ? "Sharing…" : "Share to Org…" }}
  </button>
}
```

`canShare()` is false for a sealed container and for the reserved note root; `isShared()` reads `SharedWorkspaceService.containerShares()`.

- [ ] **Step 3: Host the sheet** once for the whole tree inside `<div class="workspace-overlay-host" appTeleportToBody>`, matching how the organize/move/manage sheets are hosted — one instance, not one per row.

- [ ] **Step 4: Run the gates**

Run: `npx ng lint && npx ng build` → clean.

- [ ] **Step 5: Commit**

```bash
git add src/app/features/workspace/
git commit -m "feat: share a Space or Folder to an Org from its row menu"
```

---

### Task 15: Retire the Shared Brains rail entry; add the container page

**Files:**
- Modify: `src/app/app-shell/app-shell.component.html` (remove the rail anchor), `src/app/app-shell/app-shell.component.ts` (remove `isSharedBrainsActive` and its two other references), `src/app/app.routes.ts`
- Create: `src/app/features/shared-brains/shared-container-view/shared-container-view.component.{ts,html,scss}`

**Interfaces:**
- Produces: route `shared/:orgId/:containerId` → `SharedContainerViewComponent`; `/shared-brains` keeps rendering `SharedBrainsComponent` and is now reached from the sidebar row.

- [ ] **Step 1: Remove the rail entry** and every reference to `isSharedBrainsActive`, leaving the route intact.

- [ ] **Step 2: Add the container page** — a read-only listing of one received container's folders and items, with the org name, the author hint, and an access pill reading `View only` or `Can edit`. It offers no rename, delete or create-here affordance regardless of access: structure belongs to the owner.

- [ ] **Step 3: Run the gates**

Run: `npx ng lint && npx ng build` → clean.

- [ ] **Step 4: Commit**

```bash
git add src/app/app-shell/ src/app/app.routes.ts src/app/features/shared-brains/
git commit -m "feat: reach Shared Brains from the sidebar and open a received container"
```

---

### Task 16: End-to-end coverage and the full gate

**Files:**
- Create: `e2e/shared-containers.spec.ts`
- Modify: `scripts/screenshots/mock-tauri.js` (add `list_shared_workspace` and the container-share commands behind the existing `__demoRich` flag, since this file is the base fixture ~460 Playwright tests boot into)

- [ ] **Step 1: Write the specs**

```ts
test("a received Space is a top-level sidebar row and carries the shared glyph", async ({ page }) => { /* … */ });
test("received loose items live inside the virtual Shared Brains Space", async ({ page }) => { /* … */ });
test("a view-only received container offers no rename, delete or create action", async ({ page }) => { /* … */ });
test("the share sheet names skipped sealed items and refuses a sealed container", async ({ page }) => { /* … */ });
```

- [ ] **Step 2: Run the chromium project in full**, not just the new file — the mock is the world every spec boots into.

Run: `npx playwright test --project=chromium`
Expected: PASS, with no pre-existing spec disturbed.

- [ ] **Step 3: Run the control-plane audit and the full gate**

```bash
.agents/h/mirror-check
(cd src-tauri && cargo test --lib) && npx ng lint && npx ng build
bash scripts/ci.sh
```

- [ ] **Step 4: Commit and open the PR**

```bash
git add e2e/ scripts/screenshots/mock-tauri.js
git commit -m "test: end-to-end coverage for shared containers"
gh pr create -R murmur-io/murmur --base murmur --title "feat: share whole Folders and Spaces to an Org" --body-file …
```

---

## Self-Review

**Spec coverage.** §2.1 → Task 1; §2.2/§2.3 → Task 2; §3 → Tasks 3–4; §4.1 → Task 11; §4.2 → Task 13; §5.1 → Tasks 7–8; §5.2 → Task 10; §5.3 → Tasks 8 and 15; §6 → the sealed-refusal tests in Tasks 7, 8 and 10 plus the no-path assertion in Task 11; §7 → Tasks 13–15; §8 → Tasks 1–11 inline and Task 16.

**Ingest of container items** is spec §2.1's other half and gets its own Task 9 — it would otherwise fall between "publish" and "read".

**Type consistency.** `container_id` is the manifest identity everywhere; `parent_container_id` is always the placement pointer; `folder_id` is always the LOCAL `folders.id`. `access` is `"view" | "edit"` on every surface. `SharedContainerNode.level` is `"space" | "folder" | "virtual"` in both Rust and TypeScript. `explicit` is `0/1` in SQL and `bool` in Rust.

**Known limit.** The two-account live behaviour cannot be proven in CI; Task 16 ends the automatable work, and the live pass is the user's.
