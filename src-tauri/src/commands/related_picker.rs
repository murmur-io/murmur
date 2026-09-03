//! RELATED-PICKER command surface — the gated reader behind the "Add related" hierarchy modal.
//!
//! Three commands, one job: let the user find ANY linkable thing in a large vault without the
//! frontend ever holding a cached copy of the truth.
//!
//! * [`get_related_picker_bootstrap`] — the metadata hierarchy (Spaces › folders, plus the
//!   synthetic "Not classified" node), the anchor's ancestor path, and a BOUNDED window of the
//!   anchor's own siblings CENTRED on it. The anchor may be item #150; the reply is still bounded.
//! * [`list_related_picker_items`] — one lazy, stable page of a scope's leaves. Because bootstrap
//!   centres, both `Load earlier` (a lower offset) and `Load more` (a higher one) are ordinary
//!   calls against the same ordering.
//! * [`search_related_picker`] — bounded, gated matches with full `Space / folder` breadcrumbs.
//!
//! # Lock model
//!
//! This is a NEW content-revealing read path, so it carries the shipped posture rather than
//! inventing one:
//!
//! * every read takes [`lifecycle_guard`] and snapshots [`unlocked_snapshot`] under it, so a
//!   concurrent lock/relock cannot land between the gate and the rows;
//! * a SEALED-and-not-session-unlocked container discloses its NAME (which the sidebar already
//!   discloses — you must see a container in order to unlock it) and NOTHING else: no child
//!   titles, no ids, no totals, no search hits, and no `linkable` affordance;
//! * an anchor that is itself sealed — or simply unknown — fails CLOSED and
//!   INDISTINGUISHABLY: `AppError::Locked` with no hierarchy, no window and no counts, so the
//!   modal cannot be used as an existence oracle;
//! * `search_related_picker` refuses the same way for a sealed/unknown anchor, so an unlocked
//!   sibling surface cannot be used to walk around the anchor gate;
//! * nothing here logs an id, a title or a count.
//!
//! Every DTO is camelCase and PATH-FREE (`related_picker_tests::…_wire_shape`), because the FE
//! feeds any path it receives into `convertFileSrc` — the one read that bypasses the command gate.

use super::*;
use crate::storage::models::{PickerItemKind, PickerRow, PickerScope, PickerSearchRow};

/// How many siblings the bootstrap window carries around the anchor. Small enough that the reply
/// stays bounded on a 5000-item container, large enough that the row above and below the current
/// item are both on screen before any paging happens.
const ANCHOR_WINDOW: u32 = 24;

/// Upper bound on ONE [`list_related_picker_items`] page, so a caller cannot ask for the vault.
const MAX_PICKER_PAGE: u32 = 100;

/// Upper bound on ONE [`search_related_picker`] page. Deliberately tighter than the item page: a
/// search reply is a cross-container disclosure, and 50 rows is already more than a picker shows.
const MAX_SEARCH_PAGE: u32 = 50;

// ── DTOs ─────────────────────────────────────────────────────────────────────────────────────────

/// One container in the picker's hierarchy: a Space (`level = "project"`) or a folder.
///
/// Metadata only — it carries no item rows. Leaves arrive through
/// [`list_related_picker_items`], which is what keeps the bootstrap bounded no matter how large a
/// container grows.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PickerContainerNode {
    pub id: String,
    /// The container's CURRENT visible name. A locked container still reports it — the sidebar
    /// already does, and a user has to see a place in order to unlock it.
    pub name: String,
    /// `"project"` (a Space) or `"folder"`.
    pub level: String,
    pub emoji: Option<String>,
    /// Sealed on disk (the `folders.locked` column).
    pub locked: bool,
    /// Sealed AND session-unlocked (decrypted for this session only).
    pub unlocked: bool,
    /// Whether this container is a valid `container` LINK ENDPOINT right now. False for a
    /// sealed-not-unlocked container: the write gate would refuse it, and offering an action that
    /// always errors is worse than not offering it.
    pub linkable: bool,
    /// Which leaf kinds this container holds, in [`PickerItemKind::ORDER`], with their visible
    /// totals. EMPTY for a sealed-not-unlocked container — not even a zero, because a zero is
    /// still an answer about what is inside.
    pub groups: Vec<PickerGroup>,
    pub folders: Vec<PickerContainerNode>,
}

/// One leaf kind a scope holds, plus its true visible total.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PickerGroup {
    pub kind: PickerItemKind,
    pub total: u32,
}

/// Where the anchor sits, and the bounded slice of its neighbourhood the modal opens on.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PickerAnchorLocation {
    /// The anchor's own leaf kind — which group inside its scope it belongs to.
    pub kind: PickerItemKind,
    /// The container the anchor lives in; `None` ⇒ the synthetic "Not classified" node.
    pub container_id: Option<String>,
    /// Its ancestors, ROOT-FIRST, so the modal can expand exactly that path and nothing else.
    /// Empty when the anchor is unclassified.
    pub path: Vec<String>,
    /// The anchor's 0-based position in the stable ordering of its `(scope, kind)`.
    pub index: u32,
    /// Where the returned window starts. `index - offset` is the anchor's row inside `items`.
    pub offset: u32,
    /// The bounded window CONTAINING the anchor — never the whole container.
    pub items: Vec<PickerRow>,
    /// The scope's full visible total, so the modal knows whether `Load earlier`/`Load more` apply.
    pub total: u32,
}

/// Everything the modal needs to render its first frame.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RelatedPickerBootstrap {
    /// Top-level Spaces and their folders, metadata only.
    pub spaces: Vec<PickerContainerNode>,
    /// The synthetic "Not classified" node's groups: unfiled recordings, and reserved-root
    /// notes/documents. DISCLOSURE-ONLY — it is not a container and can never be linked, which is
    /// why it is a `Vec<PickerGroup>` here rather than a [`PickerContainerNode`].
    pub unclassified: Vec<PickerGroup>,
    /// Where the anchor is. `None` when the anchor is a kind with no place in the local hierarchy
    /// (a Shared Brain item, or a container anchor) — the modal then simply opens collapsed.
    pub anchor: Option<PickerAnchorLocation>,
}

/// One page of a scope's leaves.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RelatedPickerPage {
    pub kind: PickerItemKind,
    pub offset: u32,
    pub items: Vec<PickerRow>,
    /// The full visible count for `(scope, kind)` — the caller stops paging when
    /// `offset + items.len() >= total`.
    pub total: u32,
}

/// One search hit, with its full breadcrumb.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RelatedPickerHit {
    pub kind: PickerItemKind,
    pub id: String,
    pub title: String,
    /// `Space / folder`, root-first, or `"Not classified"` for an unfiled hit. Built from the
    /// hierarchy this command already resolved, so a hit can never advertise a place the tree
    /// would refuse to show.
    pub breadcrumb: Vec<String>,
}

/// One bounded page of search results.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RelatedPickerSearchPage {
    pub offset: u32,
    pub hits: Vec<RelatedPickerHit>,
    pub total: u32,
}

// ── Shared helpers ───────────────────────────────────────────────────────────────────────────────

/// Parse the wire scope: `None` ⇒ the synthetic "Not classified" node, `Some(id)` ⇒ a container.
fn picker_scope(container_id: Option<String>) -> PickerScope {
    match container_id {
        Some(id) => PickerScope::Container(id),
        None => PickerScope::Unclassified,
    }
}

/// Parse the wire anchor kind. The picker's own three-kind enum, NOT the global `ItemKind` — an
/// anchor is always a meeting/note/document, and a caller naming anything else gets a clean
/// `InvalidArg` instead of being silently coerced into one.
fn parse_picker_kind(s: &str) -> Result<PickerItemKind, AppError> {
    match s {
        "meeting" => Ok(PickerItemKind::Meeting),
        "note" => Ok(PickerItemKind::Note),
        "document" => Ok(PickerItemKind::Document),
        _ => Err(AppError::InvalidArg(format!(
            "unknown picker item kind {s:?} (expected \"meeting\", \"note\", or \"document\")"
        ))),
    }
}

/// The indistinguishable refusal for a sealed OR unknown anchor.
///
/// ONE message and ONE variant for both, deliberately: a caller must not be able to tell "this
/// meeting is locked" from "this meeting does not exist" by reading the error, or the modal becomes
/// an existence oracle for content behind a lock.
fn anchor_unavailable() -> AppError {
    AppError::Locked("this item is locked — unlock it to browse related items".into())
}

/// Is the ANCHOR itself visible in this session?
///
/// The db-level restatement of `commands::links::link_endpoint_is_unlocked`, taking the same
/// snapshot the rest of the read already holds — so bootstrap and search gate identically, and both
/// stay unit-testable without a `tauri::State`. Every arm delegates to the shipped gated reader for
/// its kind; an unknown id is `false` on every one of them, which is what makes the refusal
/// indistinguishable from a sealed one.
fn anchor_is_visible(
    db: &crate::storage::db::Db,
    kind: crate::links::LinkKind,
    id: &str,
    unlocked: &std::collections::HashSet<String>,
) -> Result<bool, AppError> {
    Ok(match kind {
        crate::links::LinkKind::Meeting => db.meeting_is_visible(id, unlocked)?,
        crate::links::LinkKind::Note => db.note_is_visible(id, unlocked)?,
        crate::links::LinkKind::Document => db.document_is_visible(id, unlocked)?,
        crate::links::LinkKind::Org => db.org_link_target_visible(id)?.is_some(),
        crate::links::LinkKind::Container => db.container_endpoint_visible(id, unlocked)?.is_some(),
    })
}

/// Parse and gate the anchor before any hierarchy, page, or search reader runs.
fn require_visible_anchor(
    db: &crate::storage::db::Db,
    unlocked: &std::collections::HashSet<String>,
    anchor_kind: &str,
    anchor_id: &str,
) -> Result<crate::links::LinkKind, AppError> {
    let Some(kind) = crate::links::LinkKind::parse(anchor_kind) else {
        return Err(AppError::InvalidArg(format!(
            "unknown anchor kind {anchor_kind:?}"
        )));
    };
    if !anchor_is_visible(db, kind, anchor_id, unlocked)? {
        return Err(anchor_unavailable());
    }
    Ok(kind)
}

/// A refusal for a sealed-or-unknown SCOPE, worded the same way `list_container_items` words it.
fn scope_unavailable() -> AppError {
    AppError::Locked("this container is locked — unlock it to see what is inside".into())
}

/// The renderable containers, indexed by id, plus the parent map — resolved ONCE per command.
struct ContainerIndex {
    rows: Vec<crate::storage::models::ContainerRow>,
    /// The storage-owned reserved Notes root. `is_root` alone is not identity: a corrupt/legacy DB
    /// may contain another flagged row, and that row must never become a hidden hierarchy gateway.
    canonical_notes_root_id: Option<String>,
}

impl ContainerIndex {
    fn load(db: &crate::storage::db::Db) -> Result<Self, AppError> {
        Ok(Self {
            rows: db.list_containers()?,
            canonical_notes_root_id: db.note_root_id()?,
        })
    }

    fn get(&self, id: &str) -> Option<&crate::storage::models::ContainerRow> {
        self.rows.iter().find(|row| row.id == id)
    }

    fn is_canonical_notes_root(&self, row: &crate::storage::models::ContainerRow) -> bool {
        self.canonical_notes_root_id.as_deref() == Some(row.id.as_str())
            && row.is_root
            && row.kind == "note"
    }

    /// Is this id a container the picker RENDERS — i.e. a real Space/folder that is not the
    /// reserved always-open note root? `list_containers` already excludes machine-owned kinds and
    /// the `.murmur/` subtree, so this adds only the two hierarchy facts on top of it.
    fn is_structurally_renderable(&self, id: &str) -> bool {
        self.get(id).is_some_and(|row| {
            !row.is_root && (row.level == LEVEL_PROJECT || row.level == LEVEL_FOLDER)
        })
    }

    /// Is this selectable container actually reachable from a top-level Space in the hierarchy the
    /// modal renders? The exact canonical note root may be an intermediate (its children are
    /// hoisted), or the terminal root in a valid legacy DB that declined hierarchy adoption. An
    /// arbitrary `is_root`, orphan, cycle, invalid level, or top-level folder is not reachable.
    fn is_reachable(&self, id: &str) -> bool {
        if !self.is_structurally_renderable(id) {
            return false;
        }
        let mut seen = std::collections::HashSet::new();
        let mut cursor = id;
        loop {
            if !seen.insert(cursor.to_string()) {
                return false;
            }
            let Some(row) = self.get(cursor) else {
                return false;
            };
            if row.level != LEVEL_PROJECT && row.level != LEVEL_FOLDER {
                return false;
            }
            if row.is_root && !self.is_canonical_notes_root(row) {
                return false;
            }
            match row.parent_id.as_deref() {
                Some(parent) => cursor = parent,
                None => {
                    return (!row.is_root && row.level == LEVEL_PROJECT)
                        || self.is_canonical_notes_root(row)
                }
            }
        }
    }

    fn reachable_ids(&self) -> std::collections::HashSet<String> {
        self.rows
            .iter()
            .filter(|row| self.is_reachable(&row.id))
            .map(|row| row.id.clone())
            .collect()
    }

    /// Storage queries need the selectable hierarchy plus the exact hidden Notes root so they can
    /// classify its direct documents as `Not classified`. The root remains absent from
    /// [`Self::reachable_ids`] and every selectable-scope check.
    fn storage_scope_ids(&self) -> std::collections::HashSet<String> {
        let mut ids = self.reachable_ids();
        if let Some(root_id) = self.canonical_notes_root_id.as_deref() {
            if self
                .get(root_id)
                .is_some_and(|row| self.is_canonical_notes_root(row))
            {
                ids.insert(root_id.to_string());
            }
        }
        ids
    }

    /// The ancestor chain of `id`, ROOT-FIRST, INCLUDING `id` itself. The reserved note root is
    /// SKIPPED — the tree hides it and hoists its children, so a path through it would name a row
    /// the modal never draws. A parent cycle terminates instead of hanging.
    fn path_to(&self, id: &str) -> Vec<String> {
        let mut chain = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut cursor = Some(id.to_string());
        while let Some(current) = cursor {
            if !seen.insert(current.clone()) {
                break;
            }
            let Some(row) = self.get(&current) else { break };
            if !self.is_canonical_notes_root(row) {
                chain.push(row.id.clone());
            }
            cursor = row.parent_id.clone();
        }
        chain.reverse();
        chain
    }

    /// The display breadcrumb for a container id — the same chain as [`Self::path_to`], as names.
    fn breadcrumb(&self, id: &str) -> Vec<String> {
        self.path_to(id)
            .into_iter()
            .filter_map(|cid| self.get(&cid).map(|row| row.name.clone()))
            .collect()
    }

    /// Every rendered scope whose FULL visible breadcrumb contains `query`, case-insensitively.
    /// Matching is intentionally done over the root-first display labels, not `folders.path`: the
    /// latter is a storage path and may disagree with renamed hierarchy labels. A Space match also
    /// selects every descendant scope because that Space's name occurs in each descendant's full
    /// breadcrumb.
    fn breadcrumb_matched_ids(&self, query: &str) -> std::collections::HashSet<String> {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return std::collections::HashSet::new();
        }
        self.reachable_ids()
            .into_iter()
            .filter(|id| {
                self.breadcrumb(id)
                    .join(" / ")
                    .to_lowercase()
                    .contains(&query)
            })
            .collect()
    }
}

/// `folders.level` values. `LEVEL_PROJECT` is shared with the workspace surface; the folder twin is
/// declared next to its only readers rather than duplicating the literal at each call site.
const LEVEL_FOLDER: &str = "folder";

/// Per-kind visible totals for one scope. Returns `None` for a sealed-not-unlocked container —
/// which is how "no groups at all, not even a zero" is expressed at the type level rather than by
/// remembering to clear a vector later.
fn groups_for_scope(
    db: &crate::storage::db::Db,
    scope: &PickerScope,
    unlocked: &std::collections::HashSet<String>,
    reachable: &std::collections::HashSet<String>,
) -> Result<Vec<PickerGroup>, AppError> {
    let mut out = Vec::new();
    for kind in PickerItemKind::ORDER {
        let (_rows, total) = db.related_picker_page(scope, kind, 0, 0, unlocked, reachable)?;
        if total > 0 {
            out.push(PickerGroup { kind, total });
        }
    }
    Ok(out)
}

/// Assemble one container's node (and, recursively, its children).
fn container_node(
    db: &crate::storage::db::Db,
    index: &ContainerIndex,
    row: &crate::storage::models::ContainerRow,
    unlocked: &std::collections::HashSet<String>,
    reachable: &std::collections::HashSet<String>,
) -> Result<PickerContainerNode, AppError> {
    let sealed = row.locked && !unlocked.contains(&row.id);
    // The gate, RESTATED at the assembly layer. The storage legs already return nothing for a
    // sealed container; saying so here makes the intent unmissable rather than emergent, so a
    // future reader change cannot quietly start disclosing totals behind a lock.
    let groups = if sealed {
        Vec::new()
    } else {
        groups_for_scope(
            db,
            &PickerScope::Container(row.id.clone()),
            unlocked,
            reachable,
        )?
    };
    // Child folders are still listed BY NAME even under a sealed parent — the same policy the
    // shipped tree follows (`list_folders` returns locked folders with their names).
    let mut folders = Vec::new();
    for child in index
        .rows
        .iter()
        .filter(|c| c.parent_id.as_deref() == Some(row.id.as_str()))
    {
        if index.is_canonical_notes_root(child) {
            // The reserved note root is the "Not classified · Notes" SECTION, not a folder. Hide
            // the row and HOIST its real folder children to this depth, exactly as the shipped
            // workspace tree does — a container the user created must never become unreachable
            // because its parent stopped being drawn.
            for grandchild in index
                .rows
                .iter()
                .filter(|c| c.parent_id.as_deref() == Some(child.id.as_str()))
                .filter(|c| index.is_reachable(&c.id))
            {
                folders.push(container_node(db, index, grandchild, unlocked, reachable)?);
            }
            continue;
        }
        if index.is_reachable(&child.id) {
            folders.push(container_node(db, index, child, unlocked, reachable)?);
        }
    }
    Ok(PickerContainerNode {
        id: row.id.clone(),
        name: row.name.clone(),
        level: row.level.clone(),
        emoji: row.emoji.clone(),
        locked: row.locked,
        unlocked: row.locked && unlocked.contains(&row.id),
        // A sealed container is not a valid endpoint: `link_endpoint_is_unlocked` would refuse it.
        linkable: !sealed && index.is_reachable(&row.id),
        groups,
        folders,
    })
}

// ── Commands ─────────────────────────────────────────────────────────────────────────────────────

/// The picker's first frame: the metadata hierarchy, the anchor's ancestor path, and a BOUNDED
/// window of the anchor's siblings centred on the anchor itself.
///
/// Fails CLOSED and indistinguishably (`AppError::Locked`) for a sealed OR unknown local anchor —
/// before any hierarchy, window or total is computed, so a refused call discloses nothing at all.
/// An `org` anchor (a Shared Brain item, which has no place in the local hierarchy) is accepted and
/// simply returns `anchor: None`.
#[tauri::command]
pub async fn get_related_picker_bootstrap(
    app: AppHandle,
    anchor_kind: String,
    anchor_id: String,
) -> Result<RelatedPickerBootstrap, AppError> {
    offload_read(app, move |state| {
        // The guard is held across the gate AND the read: a concurrent lock/relock cannot land
        // between "the anchor is visible" and the rows that answer for it.
        let _lifecycle = lifecycle_guard(state);
        let unlocked = unlocked_snapshot(state)?;
        related_picker_bootstrap_inner(&state.db, &unlocked, &anchor_kind, &anchor_id)
    })
    .await
}

/// Inner of [`get_related_picker_bootstrap`], taking the pieces directly so the gate ordering, the
/// centred window and the reserved-root hoist are unit-testable without a `tauri::State`.
pub(crate) fn related_picker_bootstrap_inner(
    db: &crate::storage::db::Db,
    unlocked: &std::collections::HashSet<String>,
    anchor_kind: &str,
    anchor_id: &str,
) -> Result<RelatedPickerBootstrap, AppError> {
    // ── ANCHOR GATE, FIRST — before ANY hierarchy, window or total is computed, so a refused call
    //    discloses nothing at all, and a sealed anchor is indistinguishable from an unknown one. ──
    let link_kind = require_visible_anchor(db, unlocked, anchor_kind, anchor_id)?;

    let anchor_leaf = match link_kind {
        crate::links::LinkKind::Meeting => Some(PickerItemKind::Meeting),
        crate::links::LinkKind::Note => Some(PickerItemKind::Note),
        crate::links::LinkKind::Document => Some(PickerItemKind::Document),
        // An `org` / `container` anchor has no position in the LOCAL hierarchy. The modal still
        // opens (it can link local things FROM a Shared Brain item), just without a Current row.
        crate::links::LinkKind::Org | crate::links::LinkKind::Container => None,
    };

    let index = ContainerIndex::load(db)?;
    let reachable = index.storage_scope_ids();
    let anchor = match anchor_leaf {
        None => None,
        Some(kind) => {
            let container_id = db.related_picker_owner_of(kind, anchor_id, unlocked, &reachable)?;
            let scope = picker_scope(container_id.clone());
            let Some(item_index) =
                db.related_picker_index_of(&scope, kind, anchor_id, unlocked, &reachable)?
            else {
                // Visible as an endpoint but absent from the picker's own leaf set (a companion
                // note, a machine container). Refuse identically rather than opening on nothing.
                return Err(anchor_unavailable());
            };
            // A BOUNDED window CENTRED on the anchor: this is what makes item #150 openable
            // without shipping the other 149. `saturating_sub` keeps an anchor near the top of its
            // container starting at offset 0 rather than wrapping.
            let offset = item_index.saturating_sub(ANCHOR_WINDOW / 2);
            let (items, total) =
                db.related_picker_page(&scope, kind, offset, ANCHOR_WINDOW, unlocked, &reachable)?;
            Some(PickerAnchorLocation {
                kind,
                container_id: container_id.clone(),
                path: container_id
                    .as_deref()
                    .map(|cid| index.path_to(cid))
                    .unwrap_or_default(),
                index: item_index,
                offset,
                items,
                total,
            })
        }
    };

    let mut spaces = Vec::new();
    for row in index
        .rows
        .iter()
        .filter(|row| row.level == LEVEL_PROJECT && row.parent_id.is_none() && !row.is_root)
    {
        spaces.push(container_node(db, &index, row, unlocked, &reachable)?);
    }
    // A valid legacy database may have declined hierarchy adoption, leaving the canonical Notes
    // root parentless. It is still a hidden structural section, so hoist its user-created children
    // into the picker's top level rather than orphaning every note below it.
    if let Some(root_id) = index.canonical_notes_root_id.as_deref() {
        if index
            .get(root_id)
            .is_some_and(|root| root.parent_id.is_none() && index.is_canonical_notes_root(root))
        {
            for child in index
                .rows
                .iter()
                .filter(|row| row.parent_id.as_deref() == Some(root_id))
                .filter(|row| index.is_reachable(&row.id))
            {
                spaces.push(container_node(db, &index, child, unlocked, &reachable)?);
            }
        }
    }

    Ok(RelatedPickerBootstrap {
        spaces,
        unclassified: groups_for_scope(db, &PickerScope::Unclassified, unlocked, &reachable)?,
        anchor,
    })
}

/// One lazy, stable PAGE of a scope's linkable leaves.
///
/// `containerId = null` is the synthetic "Not classified" node. Refuses a sealed-or-unknown
/// container with [`AppError::Locked`] — the gate below would return an empty page anyway, but an
/// empty page and a refusal are distinguishable to a prober, and only the refusal is honest.
#[tauri::command]
pub async fn list_related_picker_items(
    app: AppHandle,
    anchor_kind: String,
    anchor_id: String,
    container_id: Option<String>,
    kind: String,
    offset: u32,
    limit: u32,
) -> Result<RelatedPickerPage, AppError> {
    offload_read(app, move |state| {
        let _lifecycle = lifecycle_guard(state);
        let unlocked = unlocked_snapshot(state)?;
        related_picker_items_inner(
            &state.db,
            &unlocked,
            &anchor_kind,
            &anchor_id,
            container_id.as_deref(),
            &kind,
            offset,
            limit,
        )
    })
    .await
}

/// Inner of [`list_related_picker_items`] — the refusal and the clamp, unit-testable.
pub(crate) fn related_picker_items_inner(
    db: &crate::storage::db::Db,
    unlocked: &std::collections::HashSet<String>,
    anchor_kind: &str,
    anchor_id: &str,
    container_id: Option<&str>,
    kind: &str,
    offset: u32,
    limit: u32,
) -> Result<RelatedPickerPage, AppError> {
    // Gate the anchor before loading the hierarchy or counting/paging a scope. Otherwise a modal
    // opened while the anchor was visible could keep probing other containers after auto-relock.
    require_visible_anchor(db, unlocked, anchor_kind, anchor_id)?;
    let kind = parse_picker_kind(kind)?;
    let index = ContainerIndex::load(db)?;
    let reachable = index.storage_scope_ids();
    if let Some(id) = container_id {
        // Fail-CLOSED for anything not found, not renderable, or sealed-and-not-unlocked — one
        // refusal for all three, so a caller cannot learn which by reading the error.
        let refused = match index.get(id) {
            None => true,
            Some(row) => !index.is_reachable(id) || (row.locked && !unlocked.contains(id)),
        };
        if refused {
            return Err(scope_unavailable());
        }
    }
    let scope = picker_scope(container_id.map(str::to_string));
    let limit = limit.clamp(1, MAX_PICKER_PAGE);
    let (items, total) =
        db.related_picker_page(&scope, kind, offset, limit, unlocked, &reachable)?;
    Ok(RelatedPickerPage {
        kind,
        offset,
        items,
        total,
    })
}

/// BOUNDED, GATED search across every linkable local leaf, with full `Space / folder` breadcrumbs.
///
/// Carries the SAME anchor gate as the bootstrap: a sealed or unknown anchor refuses
/// indistinguishably before a single row is read, so an unlocked search surface can never be used
/// to walk around the anchor's own lock.
#[tauri::command]
pub async fn search_related_picker(
    app: AppHandle,
    anchor_kind: String,
    anchor_id: String,
    query: String,
    offset: u32,
    limit: u32,
) -> Result<RelatedPickerSearchPage, AppError> {
    offload_read(app, move |state| {
        let _lifecycle = lifecycle_guard(state);
        let unlocked = unlocked_snapshot(state)?;
        related_picker_search_inner(
            &state.db,
            &unlocked,
            &anchor_kind,
            &anchor_id,
            &query,
            offset,
            limit,
        )
    })
    .await
}

/// Inner of [`search_related_picker`].
pub(crate) fn related_picker_search_inner(
    db: &crate::storage::db::Db,
    unlocked: &std::collections::HashSet<String>,
    anchor_kind: &str,
    anchor_id: &str,
    query: &str,
    offset: u32,
    limit: u32,
) -> Result<RelatedPickerSearchPage, AppError> {
    // ── ANCHOR GATE, FIRST — before the query is even escaped, so a refusal costs no read at all. ──
    require_visible_anchor(db, unlocked, anchor_kind, anchor_id)?;
    let index = ContainerIndex::load(db)?;
    let reachable = index.storage_scope_ids();
    let breadcrumb_matched = index.breadcrumb_matched_ids(query);
    let normalized_query = query.trim().to_lowercase();
    let include_unclassified = !normalized_query.is_empty()
        && UNCLASSIFIED_LABEL
            .to_lowercase()
            .contains(&normalized_query);
    let limit = limit.clamp(1, MAX_SEARCH_PAGE);
    let (rows, total) = db.related_picker_search(
        query,
        offset,
        limit,
        unlocked,
        &reachable,
        &breadcrumb_matched,
        include_unclassified,
    )?;
    let hits = rows
        .into_iter()
        .map(|row: PickerSearchRow| RelatedPickerHit {
            kind: row.kind,
            id: row.id,
            title: row.title,
            // The breadcrumb comes from the hierarchy THIS command resolved, so a hit can never
            // advertise a place the tree would refuse to draw.
            breadcrumb: match row.container_id.as_deref() {
                Some(cid) => index.breadcrumb(cid),
                None => vec![UNCLASSIFIED_LABEL.to_string()],
            },
        })
        .collect();
    Ok(RelatedPickerSearchPage {
        offset,
        hits,
        total,
    })
}

/// The one label for the synthetic top-level node, resolved in the BACKEND so the tree, the search
/// breadcrumbs and the modal's copy cannot drift apart.
pub(crate) const UNCLASSIFIED_LABEL: &str = "Not classified";

#[cfg(test)]
#[path = "tests/related_picker_tests.rs"]
mod related_picker_tests;
