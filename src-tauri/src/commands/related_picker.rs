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
    /// DESTINATION mode only. Absent (and omitted from the JSON entirely) in link mode, so the
    /// shipped link wire shape stays byte-identical.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub availability: Option<PickerAvailability>,
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
    /// DESTINATION mode only: where the moved item lives now, what its root target is, and — for a
    /// container source — its own id/level/path/subtree, so the modal can refuse self + descendants
    /// without ever walking a cached FE forest. Omitted from the JSON in link mode.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub destination: Option<PickerDestinationContext>,
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
    /// DESTINATION mode only: the CONTAINER matches of this page, which sort BEFORE `hits` in the
    /// rendered list. `total` counts containers + leaves together, so `offset + containers.len() +
    /// hits.len() >= total` is still the caller's stop condition. Omitted in link mode.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub containers: Option<Vec<PickerContainerHit>>,
}

/// Which job this picker is doing. Absent on the wire ⇒ [`PickerMode::Link`], so every shipped
/// link caller keeps working unchanged and the link reply keeps its exact JSON shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PickerMode {
    /// Pick something to LINK to. Leaves are the targets; containers are linkable endpoints.
    #[default]
    Link,
    /// Pick a place to MOVE to. Containers (and the root) are the targets; leaves are inert
    /// context.
    Destination,
}

/// The seven inputs of ONE picker search, behind the Tauri boundary.
///
/// Internal only: the shipped `search_related_picker` command keeps its flat argument contract, so
/// this struct changes no wire shape. It borrows every string, so passing it allocates nothing.
pub(crate) struct PickerSearchQuery<'a> {
    /// Kind of the anchor the picker was opened from — gated before any row is read.
    pub(crate) anchor_kind: &'a str,
    /// Id of that anchor.
    pub(crate) anchor_id: &'a str,
    /// Raw user query; trimmed and lowercased inside, never pre-normalized by the caller.
    pub(crate) query: &'a str,
    /// Page offset, counted over containers + leaves together in destination mode.
    pub(crate) offset: u32,
    /// Requested page size, clamped to [`MAX_SEARCH_PAGE`] inside.
    pub(crate) limit: u32,
    /// Which job the picker is doing; [`PickerMode::Link`] is the default on the wire.
    pub(crate) mode: PickerMode,
    /// Org scope for a shared anchor, or `None` for a local one.
    pub(crate) org_id: Option<&'a str>,
}

/// Destination identities are deliberately separate from link endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DestinationAnchorKind {
    Meeting,
    Note,
    Document,
    Container,
    Task,
    Dashboard,
    Org,
    SharedContainer,
    SharedDoc,
}
impl DestinationAnchorKind {
    fn parse(value: &str) -> Result<Self, AppError> {
        Ok(match value {
            "meeting" => Self::Meeting,
            "note" => Self::Note,
            "document" => Self::Document,
            "container" => Self::Container,
            "task" => Self::Task,
            "dashboard" => Self::Dashboard,
            "org" => Self::Org,
            "sharedContainer" => Self::SharedContainer,
            "sharedDoc" => Self::SharedDoc,
            _ => {
                return Err(AppError::InvalidArg(
                    "unknown destination anchor kind".into(),
                ))
            }
        })
    }
    fn leaf(self) -> Option<PickerItemKind> {
        match self {
            Self::Meeting => Some(PickerItemKind::Meeting),
            Self::Note => Some(PickerItemKind::Note),
            Self::Document => Some(PickerItemKind::Document),
            _ => None,
        }
    }
}

/// Org-backed sources additionally require the live account witness, exactly as their writers do.
/// Called under lifecycle, before any hierarchy is read; all denial paths use the anchor refusal.
pub(super) fn require_destination_session(
    state: &AppState,
    kind: &str,
    id: &str,
    org_id: Option<&str>,
) -> Result<(), AppError> {
    let source_org = match DestinationAnchorKind::parse(kind)? {
        DestinationAnchorKind::Task => state.db.org_task_org_for_id(id)?,
        DestinationAnchorKind::Org => state.db.org_item_edit_ctx(id)?.map(|ctx| ctx.org_id),
        DestinationAnchorKind::SharedContainer | DestinationAnchorKind::SharedDoc => {
            org_id.map(str::to_string)
        }
        _ => return Ok(()),
    }
    .ok_or_else(anchor_unavailable)?;
    super::tasks::require_task_read_context(state, &source_org).map_err(|_| anchor_unavailable())
}

/// Content admission precedes hierarchy loading. Use an EMPTY unlock set deliberately: a source
/// that remains sealed on disk cannot be moved out, even during a biometric-unlocked session.
fn require_destination_anchor(
    db: &crate::storage::db::Db,
    kind: &str,
    id: &str,
    org_id: Option<&str>,
) -> Result<DestinationAnchorKind, AppError> {
    let kind = DestinationAnchorKind::parse(kind)?;
    let open = std::collections::HashSet::new();
    let visible = match kind {
        DestinationAnchorKind::Meeting => db.meeting_is_visible(id, &open)?,
        DestinationAnchorKind::Note => db.note_is_visible(id, &open)?,
        DestinationAnchorKind::Document => db.document_is_visible(id, &open)?,
        DestinationAnchorKind::Container => db.container_endpoint_visible(id, &open)?.is_some(),
        DestinationAnchorKind::Dashboard => db
            .get_dashboard_visible(id, &open)?
            .is_some_and(|board| !board.locked),
        DestinationAnchorKind::Task => {
            // The org gate excludes stale/disabled membership projections, and the content-free
            // placement lookup refuses a task still placed behind a durable lock.
            db.related_picker_task_owner(id)?.is_some()
        }
        DestinationAnchorKind::Org => match db.org_item_edit_ctx(id)? {
            Some(ctx) => {
                db.get_org_state(&ctx.org_id)?
                    .is_some_and(|org| org.context_enabled)
                    && !matches!(ctx.source_kind.as_deref(), Some("task" | "container"))
            }
            None => false,
        },
        DestinationAnchorKind::SharedContainer => match org_id {
            Some(org) => db
                .list_org_containers(org)?
                .iter()
                .any(|row| row.container_id == id),
            None => false,
        },
        DestinationAnchorKind::SharedDoc => match org_id {
            Some(org) => db
                .org_link_target_visible(&format!("{org}:{id}"))?
                .is_some(),
            None => false,
        },
    };
    if !visible {
        return Err(anchor_unavailable());
    }
    if matches!(
        kind,
        DestinationAnchorKind::SharedContainer | DestinationAnchorKind::SharedDoc
    ) {
        let target_kind = if kind == DestinationAnchorKind::SharedContainer {
            "container"
        } else {
            "doc"
        };
        if let Some(parent) = db
            .list_local_placements()?
            .into_iter()
            .find(|row| {
                Some(row.org_id.as_str()) == org_id
                    && row.target_kind == target_kind
                    && row.target_id == id
            })
            .and_then(|row| row.local_parent_id)
        {
            if !db.folder_by_id(&parent)?.is_some_and(|row| !row.locked) {
                return Err(anchor_unavailable());
            }
        }
    }
    Ok(kind)
}

/// Revalidate received-object placement at the write boundary, under org mutation + lifecycle.
/// This uses the same source identities and renderable destination set as the chooser.
pub(super) fn validate_shared_destination(
    db: &crate::storage::db::Db,
    org_id: &str,
    target_kind: &str,
    target_id: &str,
    parent_id: Option<&str>,
) -> Result<(), AppError> {
    let kind = match target_kind {
        "container" => "sharedContainer",
        "doc" => "sharedDoc",
        _ => return Err(AppError::InvalidArg("unknown placement target".into())),
    };
    require_destination_anchor(db, kind, target_id, Some(org_id))?;
    if let Some(parent) = parent_id {
        let index = ContainerIndex::load(db)?;
        let row = index
            .get(parent)
            .filter(|_| index.is_reachable(parent))
            .ok_or_else(scope_unavailable)?;
        if row.locked || index.has_sealed_ancestor(parent, &std::collections::HashSet::new()) {
            return Err(scope_unavailable());
        }
    }
    Ok(())
}

/// Why a container is (or is not) a legal move target — decided in the BACKEND so the modal renders
/// a decision instead of re-deriving one from a cached forest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PickerAvailability {
    /// The one field the modal acts on. False whenever any refusal below holds.
    pub selectable: bool,
    /// This IS the source's current place — the `Here` row.
    pub here: bool,
    /// This IS the container being moved.
    #[serde(rename = "self")]
    pub is_self: bool,
    /// This is INSIDE the container being moved.
    pub descendant: bool,
    /// Sealed and not session-unlocked: name-only, never a target.
    pub locked: bool,
    /// Encrypted but session-unlocked: a legal target for meeting/note/document sources ONLY, and
    /// only behind the cross-encryption-boundary confirmation, because the write seals the item and
    /// removes its plaintext Markdown from the vault.
    pub confirm: bool,
    /// Structurally impossible for this source kind (e.g. a note container under a meeting tree).
    pub incompatible: bool,
}

/// The backend-typed root row ("All notes" / "Not classified" / "Workspace") — the modal renders
/// this label and hands the id back, so root semantics live in exactly one place.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PickerRootTarget {
    /// `"unfiled"` (write `null`), `"notesRoot"` (write `containerId`), `"workspace"` (a top-level
    /// Space: write `null` and promote).
    pub kind: String,
    pub label: String,
    /// The container id the writer must be given, when the root is a real row. `None` ⇒ the writer
    /// takes `null`.
    pub container_id: Option<String>,
    pub availability: PickerAvailability,
}

/// A CONTAINER source's own coordinates: enough for the modal to grey out self + subtree without
/// reading the hierarchy a second time.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PickerContainerAnchor {
    pub id: String,
    pub level: String,
    pub parent_id: Option<String>,
    /// Root-first ancestor ids INCLUDING itself (the hidden Notes root is skipped, as everywhere).
    pub path_ids: Vec<String>,
    /// Itself plus every descendant — the ids that can never be a target.
    pub subtree_ids: Vec<String>,
}

/// Everything DESTINATION mode adds to the bootstrap. Never present in link mode.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PickerDestinationContext {
    /// `"meeting" | "note" | "document" | "container"`.
    pub source_kind: String,
    /// The source is ENCRYPTED (session-unlocked, or a leaf inside an encrypted container). Purely
    /// informational — a still-SEALED source never gets this far, it refuses like an unknown one.
    pub source_locked: bool,
    /// Where it lives now; `None` ⇒ the root row is `Here`.
    pub current_container_id: Option<String>,
    /// Root-first ancestor ids of the current place, so the modal expands exactly that path.
    pub current_path: Vec<String>,
    pub root: PickerRootTarget,
    /// Present only for a container source.
    pub container: Option<PickerContainerAnchor>,
}

/// One CONTAINER search hit in destination mode, with its full `Space / folder` breadcrumb. Empty
/// containers match too — the old FE search could only find leaves, which is half of the reported
/// "the picker shows only part of the tree" defect.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PickerContainerHit {
    pub id: String,
    pub name: String,
    pub level: String,
    pub breadcrumb: Vec<String>,
    pub availability: PickerAvailability,
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

/// The anchor's own LEAF kind, or `None` for an `org` / `container` anchor, which has no position
/// among the leaves. The link modal then simply opens without a Current row; destination mode gets
/// the container's place from [`destination_rules`] instead.
fn leaf_kind_of(kind: crate::links::LinkKind) -> Option<PickerItemKind> {
    match kind {
        crate::links::LinkKind::Meeting => Some(PickerItemKind::Meeting),
        crate::links::LinkKind::Note => Some(PickerItemKind::Note),
        crate::links::LinkKind::Document => Some(PickerItemKind::Document),
        crate::links::LinkKind::Org | crate::links::LinkKind::Container => None,
    }
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
    by_id: std::collections::HashMap<String, usize>,
    children: std::collections::HashMap<String, Vec<usize>>,
}

impl ContainerIndex {
    fn load(db: &crate::storage::db::Db) -> Result<Self, AppError> {
        let rows = db.list_containers()?;
        let mut by_id = std::collections::HashMap::new();
        let mut children = std::collections::HashMap::<String, Vec<usize>>::new();
        for (position, row) in rows.iter().enumerate() {
            by_id.insert(row.id.clone(), position);
            if let Some(parent) = &row.parent_id {
                children.entry(parent.clone()).or_default().push(position);
            }
        }
        Ok(Self {
            rows,
            canonical_notes_root_id: db.note_root_id()?,
            by_id,
            children,
        })
    }

    fn get(&self, id: &str) -> Option<&crate::storage::models::ContainerRow> {
        self.by_id.get(id).map(|position| &self.rows[*position])
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

    fn has_sealed_ancestor(&self, id: &str, unlocked: &std::collections::HashSet<String>) -> bool {
        let mut seen = std::collections::HashSet::new();
        let mut cursor = self.get(id).and_then(|row| row.parent_id.as_deref());
        while let Some(current) = cursor {
            if !seen.insert(current) {
                return true;
            }
            let Some(row) = self.get(current) else {
                return true;
            };
            if row.locked && !unlocked.contains(current) {
                return true;
            }
            cursor = row.parent_id.as_deref();
        }
        false
    }

    /// Every direct child row of `id`, in the table's own deterministic order.
    fn children_of<'a>(
        &'a self,
        id: &'a str,
    ) -> impl Iterator<Item = &'a crate::storage::models::ContainerRow> + 'a {
        self.children
            .get(id)
            .into_iter()
            .flatten()
            .map(|position| &self.rows[*position])
    }

    /// `id` plus every descendant. Cycle-safe (a corrupt parent loop terminates instead of
    /// hanging) and independent of `is_reachable`, because an UNREACHABLE descendant must still be
    /// refused as a move target.
    fn subtree_ids(&self, id: &str) -> std::collections::HashSet<String> {
        let mut out = std::collections::HashSet::new();
        let mut stack = vec![id.to_string()];
        while let Some(current) = stack.pop() {
            if !out.insert(current.clone()) {
                continue;
            }
            for child in self.children_of(&current) {
                stack.push(child.id.clone());
            }
        }
        out
    }
}

/// `folders.level` values. `LEVEL_PROJECT` is shared with the workspace surface; the folder twin is
/// declared next to its only readers rather than duplicating the literal at each call site.
const LEVEL_FOLDER: &str = "folder";

/// The DESTINATION decision, resolved once per command and applied to every rendered row.
///
/// One struct, one `availability` function: the tree, the root row and the search hits all get
/// their verdict from here, so a container can never be refusable in one list and selectable in
/// another.
struct DestinationRules {
    /// `"meeting" | "note" | "document" | "container"`.
    source_kind: String,
    /// Where the source lives now — the `Here` row.
    current_container_id: Option<String>,
    /// For a container source: its own id. `None` for a leaf source.
    source_self: Option<String>,
    /// For a container source: itself plus every descendant. Empty for a leaf source.
    subtree: std::collections::HashSet<String>,
    /// For a container source: its `folders.kind`, so a note folder is not offered a meeting
    /// parent (and vice versa) — the mixed-kind workspace forest makes that a real shape.
    source_container_kind: Option<String>,
}

impl DestinationRules {
    /// Can this source be moved into an ENCRYPTED-but-session-unlocked container at all?
    ///
    /// Only the three writers that actually seal on move (`move_note`, `move_note_doc`) can, and
    /// only behind the explicit confirmation. A container move into a locked parent would have to
    /// seal a whole subtree, which no shipped writer does, so it stays refused.
    fn confirmable_into_locked(&self) -> bool {
        matches!(self.source_kind.as_str(), "meeting" | "note" | "document")
    }

    fn availability(
        &self,
        index: &ContainerIndex,
        row: &crate::storage::models::ContainerRow,
        unlocked: &std::collections::HashSet<String>,
    ) -> PickerAvailability {
        let sealed = row.locked && !unlocked.contains(&row.id);
        let here = self.current_container_id.as_deref() == Some(row.id.as_str());
        let is_self = self.subtree.contains(&row.id) && self.source_id_is(&row.id);
        let descendant = !is_self && self.subtree.contains(&row.id);
        // The unified workspace accepts both legacy folder kinds under any user container.
        let incompatible = false;
        let confirm = row.locked && !sealed && self.confirmable_into_locked();
        let blocked_by_lock = sealed || (row.locked && !self.confirmable_into_locked());
        PickerAvailability {
            selectable: !sealed
                && !here
                && !is_self
                && !descendant
                && !incompatible
                && !blocked_by_lock
                && index.is_reachable(&row.id),
            here,
            is_self,
            descendant,
            locked: sealed,
            confirm,
            incompatible,
        }
    }

    /// Is this row the moved container itself (as opposed to one of its descendants)?
    fn source_id_is(&self, id: &str) -> bool {
        self.source_self.as_deref() == Some(id)
    }
}

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
    dest: Option<&DestinationRules>,
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
        .children_of(&row.id)
        .filter(|_| dest.is_none() || !sealed)
    {
        if index.is_canonical_notes_root(child) {
            // The reserved note root is the "Not classified · Notes" SECTION, not a folder. Hide
            // the row and HOIST its real folder children to this depth, exactly as the shipped
            // workspace tree does — a container the user created must never become unreachable
            // because its parent stopped being drawn.
            for grandchild in index
                .children_of(&child.id)
                .filter(|c| index.is_reachable(&c.id))
            {
                folders.push(container_node(
                    db, index, grandchild, unlocked, reachable, dest,
                )?);
            }
            continue;
        }
        if index.is_reachable(&child.id) {
            folders.push(container_node(db, index, child, unlocked, reachable, dest)?);
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
        availability: dest.map(|rules| rules.availability(index, row, unlocked)),
    })
}

/// The one label for the root row when the source belongs to the NOTES tree.
pub(crate) const NOTES_ROOT_LABEL: &str = "All notes";
/// The one label for the root row when a meeting container is being promoted to a top-level Space.
pub(crate) const WORKSPACE_ROOT_LABEL: &str = "Workspace";

/// Resolve the DESTINATION source: what it is, where it is, and (for a container) what it drags
/// with it. Runs AFTER the anchor gate, so a sealed or unknown source has already refused.
#[allow(clippy::too_many_arguments)]
fn destination_rules(
    db: &crate::storage::db::Db,
    index: &ContainerIndex,
    unlocked: &std::collections::HashSet<String>,
    kind: DestinationAnchorKind,
    anchor_id: &str,
    org_id: Option<&str>,
    anchor_leaf: Option<PickerItemKind>,
    reachable: &std::collections::HashSet<String>,
) -> Result<DestinationRules, AppError> {
    match kind {
        DestinationAnchorKind::Container => {
            // The anchor gate proved the container is VISIBLE (not sealed, or session-unlocked).
            // It must also be a container this picker actually draws: an unrenderable row would
            // otherwise get a `Here` marker on a tree that never shows it. Same refusal as a
            // sealed one, so the modal stays a non-oracle.
            let Some(row) = index
                .get(anchor_id)
                .filter(|_| index.is_reachable(anchor_id))
            else {
                return Err(anchor_unavailable());
            };
            if index
                .subtree_ids(anchor_id)
                .iter()
                .any(|id| index.get(id).is_some_and(|row| row.locked))
            {
                return Err(anchor_unavailable());
            }
            let parent = row.parent_id.as_deref().filter(|parent| {
                // A note folder sitting directly under the hidden Notes root is at the ROOT row as
                // far as the modal is concerned — the tree never draws that parent.
                !index
                    .get(parent)
                    .is_some_and(|p| index.is_canonical_notes_root(p))
            });
            Ok(DestinationRules {
                source_kind: "container".into(),
                current_container_id: parent.map(str::to_string),
                source_self: Some(row.id.clone()),
                subtree: index.subtree_ids(&row.id),
                source_container_kind: Some(row.kind.clone()),
            })
        }
        DestinationAnchorKind::Meeting
        | DestinationAnchorKind::Note
        | DestinationAnchorKind::Document => {
            let kind = anchor_leaf.ok_or_else(anchor_unavailable)?;
            let current = db.related_picker_owner_of(kind, anchor_id, unlocked, reachable)?;
            Ok(DestinationRules {
                source_kind: match kind {
                    PickerItemKind::Meeting => "meeting".into(),
                    PickerItemKind::Note => "note".into(),
                    PickerItemKind::Document => "document".into(),
                },
                current_container_id: current,
                source_self: None,
                subtree: std::collections::HashSet::new(),
                source_container_kind: None,
            })
        }
        DestinationAnchorKind::Task
        | DestinationAnchorKind::Dashboard
        | DestinationAnchorKind::Org
        | DestinationAnchorKind::SharedContainer
        | DestinationAnchorKind::SharedDoc => {
            let (source_kind, current, root_source_kind) = match kind {
                DestinationAnchorKind::Task => (
                    "task",
                    db.related_picker_task_owner(anchor_id)?
                        .ok_or_else(anchor_unavailable)?,
                    None,
                ),
                DestinationAnchorKind::Dashboard => (
                    "dashboard",
                    db.get_dashboard_visible(anchor_id, unlocked)?
                        .and_then(|row| row.folder_id),
                    None,
                ),
                DestinationAnchorKind::Org => {
                    let ctx = db
                        .org_item_edit_ctx(anchor_id)?
                        .ok_or_else(anchor_unavailable)?;
                    ("org", None, ctx.source_kind)
                }
                _ => {
                    let target_kind = if kind == DestinationAnchorKind::SharedContainer {
                        "container"
                    } else {
                        "doc"
                    };
                    let current = db
                        .list_local_placements()?
                        .into_iter()
                        .find(|row| {
                            Some(row.org_id.as_str()) == org_id
                                && row.target_kind == target_kind
                                && row.target_id == anchor_id
                        })
                        .and_then(|row| row.local_parent_id);
                    (
                        if target_kind == "container" {
                            "sharedContainer"
                        } else {
                            "sharedDoc"
                        },
                        current,
                        None,
                    )
                }
            };
            Ok(DestinationRules {
                source_kind: source_kind.into(),
                current_container_id: current,
                source_self: None,
                subtree: std::collections::HashSet::new(),
                source_container_kind: root_source_kind,
            })
        }
    }
}

/// The backend-typed destination context: the root row, the current place, and the container
/// source's own coordinates.
fn destination_context(
    index: &ContainerIndex,
    rules: &DestinationRules,
    unlocked: &std::collections::HashSet<String>,
) -> PickerDestinationContext {
    let notes_root = index.canonical_notes_root_id.clone();
    let (root_kind, root_label, root_container) = match rules.source_kind.as_str() {
        // An unfiled recording is genuinely parentless: `move_note(null)`.
        "meeting" | "task" | "dashboard" => ("unfiled", UNCLASSIFIED_LABEL, None),
        "sharedContainer" | "sharedDoc" => ("shared", "Shared", None),
        "org" => match rules.source_container_kind.as_deref() {
            Some("meeting") => ("unfiled", UNCLASSIFIED_LABEL, None),
            _ => ("notesRoot", NOTES_ROOT_LABEL, notes_root.clone()),
        },
        // A note/document at the top of the notes tree still has a row — the reserved Notes root —
        // because `move_note_doc` is not nullable.
        "note" | "document" => ("notesRoot", NOTES_ROOT_LABEL, notes_root.clone()),
        _ => match rules.source_container_kind.as_deref() {
            Some("note") => ("notesRoot", NOTES_ROOT_LABEL, notes_root.clone()),
            // A meeting container at top level IS a Space.
            _ => ("workspace", WORKSPACE_ROOT_LABEL, None),
        },
    };
    let here = rules.source_kind != "org" && rules.current_container_id.is_none();
    let root_row = root_container.as_deref().and_then(|id| index.get(id));
    let root_locked = root_row.is_some_and(|row| row.locked);
    let root_sealed = root_row.is_some_and(|row| row.locked && !unlocked.contains(&row.id));
    let root_confirm = root_locked && !root_sealed && rules.confirmable_into_locked();
    let root_lock_blocked = root_sealed || (root_locked && !rules.confirmable_into_locked());
    let root_incompatible = (root_kind == "notesRoot" && root_container.is_none())
        || (rules.source_kind == "org" && root_kind == "unfiled");
    let current_row = rules
        .current_container_id
        .as_deref()
        .and_then(|id| index.get(id));
    PickerDestinationContext {
        source_kind: rules.source_kind.clone(),
        source_locked: current_row.is_some_and(|row| row.locked),
        current_container_id: rules.current_container_id.clone(),
        current_path: rules
            .current_container_id
            .as_deref()
            .map(|id| index.path_to(id))
            .unwrap_or_default(),
        root: PickerRootTarget {
            kind: root_kind.into(),
            label: root_label.into(),
            container_id: root_container,
            availability: PickerAvailability {
                selectable: !here && !root_incompatible && !root_lock_blocked,
                here,
                is_self: false,
                descendant: false,
                locked: root_sealed,
                confirm: root_confirm,
                incompatible: root_incompatible,
            },
        },
        container: rules.source_self.as_deref().and_then(|id| {
            index.get(id).map(|row| PickerContainerAnchor {
                id: row.id.clone(),
                level: row.level.clone(),
                parent_id: rules.current_container_id.clone(),
                path_ids: index.path_to(id),
                subtree_ids: {
                    let mut ids: Vec<String> = rules.subtree.iter().cloned().collect();
                    ids.sort();
                    ids
                },
            })
        }),
    }
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
    mode: Option<PickerMode>,
    org_id: Option<String>,
) -> Result<RelatedPickerBootstrap, AppError> {
    offload_read(app, move |state| {
        // The guard is held across the gate AND the read: a concurrent lock/relock cannot land
        // between "the anchor is visible" and the rows that answer for it.
        let _lifecycle = lifecycle_guard(state);
        let unlocked = unlocked_snapshot(state)?;
        if mode == Some(PickerMode::Destination) {
            require_destination_session(state, &anchor_kind, &anchor_id, org_id.as_deref())?;
        }
        related_picker_bootstrap_with_org(
            &state.db,
            &unlocked,
            &anchor_kind,
            &anchor_id,
            mode.unwrap_or_default(),
            org_id.as_deref(),
        )
    })
    .await
}

/// Inner of [`get_related_picker_bootstrap`], taking the pieces directly so the gate ordering, the
/// centred window and the reserved-root hoist are unit-testable without a `tauri::State`.
#[cfg(test)]
pub(crate) fn related_picker_bootstrap_inner(
    db: &crate::storage::db::Db,
    unlocked: &std::collections::HashSet<String>,
    anchor_kind: &str,
    anchor_id: &str,
    mode: PickerMode,
) -> Result<RelatedPickerBootstrap, AppError> {
    related_picker_bootstrap_with_org(db, unlocked, anchor_kind, anchor_id, mode, None)
}

fn related_picker_bootstrap_with_org(
    db: &crate::storage::db::Db,
    unlocked: &std::collections::HashSet<String>,
    anchor_kind: &str,
    anchor_id: &str,
    mode: PickerMode,
    org_id: Option<&str>,
) -> Result<RelatedPickerBootstrap, AppError> {
    // ── ANCHOR GATE, FIRST — before ANY hierarchy, window or total is computed, so a refused call
    //    discloses nothing at all, and a sealed anchor is indistinguishable from an unknown one. ──
    let destination_kind = if mode == PickerMode::Destination {
        Some(require_destination_anchor(
            db,
            anchor_kind,
            anchor_id,
            org_id,
        )?)
    } else {
        None
    };
    let anchor_leaf = match destination_kind {
        Some(kind) => kind.leaf(),
        None => leaf_kind_of(require_visible_anchor(
            db,
            unlocked,
            anchor_kind,
            anchor_id,
        )?),
    };

    let index = ContainerIndex::load(db)?;
    let mut reachable = index.storage_scope_ids();
    if mode == PickerMode::Destination {
        reachable.retain(|id| !index.has_sealed_ancestor(id, unlocked));
    }
    // DESTINATION mode resolves WHERE THE SOURCE IS before any row is rendered, because every
    // availability verdict below (`Here`, self, descendant) is relative to it.
    let dest = match mode {
        PickerMode::Link => None,
        PickerMode::Destination => Some(destination_rules(
            db,
            &index,
            unlocked,
            destination_kind.ok_or_else(anchor_unavailable)?,
            anchor_id,
            org_id,
            anchor_leaf,
            &reachable,
        )?),
    };
    if let Some(rules) = &dest {
        if let Some(current) = rules
            .source_self
            .as_deref()
            .or(rules.current_container_id.as_deref())
        {
            if index.has_sealed_ancestor(current, &std::collections::HashSet::new()) {
                return Err(anchor_unavailable());
            }
        }
    }
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
        spaces.push(container_node(
            db,
            &index,
            row,
            unlocked,
            &reachable,
            dest.as_ref(),
        )?);
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
                spaces.push(container_node(
                    db,
                    &index,
                    child,
                    unlocked,
                    &reachable,
                    dest.as_ref(),
                )?);
            }
        }
    }

    Ok(RelatedPickerBootstrap {
        spaces,
        unclassified: groups_for_scope(db, &PickerScope::Unclassified, unlocked, &reachable)?,
        anchor,
        destination: dest
            .as_ref()
            .map(|rules| destination_context(&index, rules, unlocked)),
    })
}

/// One lazy, stable PAGE of a scope's linkable leaves.
///
/// `containerId = null` is the synthetic "Not classified" node. Refuses a sealed-or-unknown
/// container with [`AppError::Locked`] — the gate below would return an empty page anyway, but an
/// empty page and a refusal are distinguishable to a prober, and only the refusal is honest.
#[tauri::command]
#[allow(clippy::too_many_arguments)] // Preserve the shipped flat IPC argument contract.
pub async fn list_related_picker_items(
    app: AppHandle,
    anchor_kind: String,
    anchor_id: String,
    container_id: Option<String>,
    kind: String,
    offset: u32,
    limit: u32,
    mode: Option<PickerMode>,
    org_id: Option<String>,
) -> Result<RelatedPickerPage, AppError> {
    offload_read(app, move |state| {
        let _lifecycle = lifecycle_guard(state);
        let unlocked = unlocked_snapshot(state)?;
        if mode == Some(PickerMode::Destination) {
            require_destination_session(state, &anchor_kind, &anchor_id, org_id.as_deref())?;
        }
        related_picker_items_with_org(
            &state.db,
            &unlocked,
            &anchor_kind,
            &anchor_id,
            container_id.as_deref(),
            &kind,
            offset,
            limit,
            mode.unwrap_or_default(),
            org_id.as_deref(),
        )
    })
    .await
}

/// Inner of [`list_related_picker_items`] — the refusal and the clamp, unit-testable.
///
/// The arguments deliberately mirror the public IPC command one-for-one; keeping that boundary
/// explicit makes the lock refusal and page clamp directly testable without constructing an app.
#[allow(clippy::too_many_arguments)]
#[cfg(test)]
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
    related_picker_items_with_org(
        db,
        unlocked,
        anchor_kind,
        anchor_id,
        container_id,
        kind,
        offset,
        limit,
        PickerMode::Link,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn related_picker_items_with_org(
    db: &crate::storage::db::Db,
    unlocked: &std::collections::HashSet<String>,
    anchor_kind: &str,
    anchor_id: &str,
    container_id: Option<&str>,
    kind: &str,
    offset: u32,
    limit: u32,
    mode: PickerMode,
    org_id: Option<&str>,
) -> Result<RelatedPickerPage, AppError> {
    // Gate the anchor before loading the hierarchy or counting/paging a scope. Otherwise a modal
    // opened while the anchor was visible could keep probing other containers after auto-relock.
    if mode == PickerMode::Destination {
        require_destination_anchor(db, anchor_kind, anchor_id, org_id)?;
    } else {
        require_visible_anchor(db, unlocked, anchor_kind, anchor_id)?;
    }
    let kind = parse_picker_kind(kind)?;
    let index = ContainerIndex::load(db)?;
    let mut reachable = index.storage_scope_ids();
    if mode == PickerMode::Destination {
        reachable.retain(|id| !index.has_sealed_ancestor(id, unlocked));
    }
    if mode == PickerMode::Destination {
        let source_kind = require_destination_anchor(db, anchor_kind, anchor_id, org_id)?;
        let rules = destination_rules(
            db,
            &index,
            unlocked,
            source_kind,
            anchor_id,
            org_id,
            source_kind.leaf(),
            &reachable,
        )?;
        if let Some(current) = rules
            .source_self
            .as_deref()
            .or(rules.current_container_id.as_deref())
        {
            if index.has_sealed_ancestor(current, &std::collections::HashSet::new()) {
                return Err(anchor_unavailable());
            }
        }
    }
    if let Some(id) = container_id {
        // Fail-CLOSED for anything not found, not renderable, or sealed-and-not-unlocked — one
        // refusal for all three, so a caller cannot learn which by reading the error.
        let refused = match index.get(id) {
            None => true,
            Some(row) => {
                !index.is_reachable(id)
                    || (row.locked && !unlocked.contains(id))
                    || (mode == PickerMode::Destination && index.has_sealed_ancestor(id, unlocked))
            }
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
#[allow(clippy::too_many_arguments)] // Preserve the shipped flat IPC argument contract.
pub async fn search_related_picker(
    app: AppHandle,
    anchor_kind: String,
    anchor_id: String,
    query: String,
    offset: u32,
    limit: u32,
    mode: Option<PickerMode>,
    org_id: Option<String>,
) -> Result<RelatedPickerSearchPage, AppError> {
    offload_read(app, move |state| {
        let _lifecycle = lifecycle_guard(state);
        let unlocked = unlocked_snapshot(state)?;
        if mode == Some(PickerMode::Destination) {
            require_destination_session(state, &anchor_kind, &anchor_id, org_id.as_deref())?;
        }
        related_picker_search_with_org(
            &state.db,
            &unlocked,
            PickerSearchQuery {
                anchor_kind: &anchor_kind,
                anchor_id: &anchor_id,
                query: &query,
                offset,
                limit,
                mode: mode.unwrap_or_default(),
                org_id: org_id.as_deref(),
            },
        )
    })
    .await
}

/// Inner of [`search_related_picker`] — the same helper the command calls, minus the app handle,
/// so the anchor gate and the page clamp stay directly testable.
#[cfg(test)]
pub(crate) fn related_picker_search_inner(
    db: &crate::storage::db::Db,
    unlocked: &std::collections::HashSet<String>,
    request: PickerSearchQuery<'_>,
) -> Result<RelatedPickerSearchPage, AppError> {
    related_picker_search_with_org(db, unlocked, request)
}

fn related_picker_search_with_org(
    db: &crate::storage::db::Db,
    unlocked: &std::collections::HashSet<String>,
    request: PickerSearchQuery<'_>,
) -> Result<RelatedPickerSearchPage, AppError> {
    let PickerSearchQuery {
        anchor_kind,
        anchor_id,
        query,
        offset,
        limit,
        mode,
        org_id,
    } = request;
    // ── ANCHOR GATE, FIRST — before the query is even escaped, so a refusal costs no read at all. ──
    let destination_kind = if mode == PickerMode::Destination {
        Some(require_destination_anchor(
            db,
            anchor_kind,
            anchor_id,
            org_id,
        )?)
    } else {
        None
    };
    let anchor_leaf = match destination_kind {
        Some(kind) => kind.leaf(),
        None => leaf_kind_of(require_visible_anchor(
            db,
            unlocked,
            anchor_kind,
            anchor_id,
        )?),
    };
    let index = ContainerIndex::load(db)?;
    let mut reachable = index.storage_scope_ids();
    if mode == PickerMode::Destination {
        reachable.retain(|id| !index.has_sealed_ancestor(id, unlocked));
    }
    let breadcrumb_matched = index.breadcrumb_matched_ids(query);
    let normalized_query = query.trim().to_lowercase();
    let include_unclassified = !normalized_query.is_empty()
        && UNCLASSIFIED_LABEL
            .to_lowercase()
            .contains(&normalized_query);
    let limit = limit.clamp(1, MAX_SEARCH_PAGE);
    // DESTINATION mode searches CONTAINERS first — including empty ones, which no leaf query can
    // ever return. That is half of the reported "the picker only shows part of the tree": a folder
    // with nothing in it was unfindable, and so was every folder past the FE's 30-row slice.
    let dest = match mode {
        PickerMode::Link => None,
        PickerMode::Destination => Some(destination_rules(
            db,
            &index,
            unlocked,
            destination_kind.ok_or_else(anchor_unavailable)?,
            anchor_id,
            org_id,
            anchor_leaf,
            &reachable,
        )?),
    };
    if let Some(rules) = &dest {
        if let Some(current) = rules
            .source_self
            .as_deref()
            .or(rules.current_container_id.as_deref())
        {
            if index.has_sealed_ancestor(current, &std::collections::HashSet::new()) {
                return Err(anchor_unavailable());
            }
        }
    }
    let container_hits: Vec<PickerContainerHit> = match dest.as_ref() {
        None => Vec::new(),
        Some(rules) => {
            let mut ids: Vec<&String> = breadcrumb_matched.iter().collect();
            // Deterministic across pages: the same total order every call, so page 1 ∪ page 2 is
            // the complete set exactly once.
            ids.sort_by(|a, b| {
                let (ka, kb) = (
                    index.breadcrumb(a).join(" / ").to_lowercase(),
                    index.breadcrumb(b).join(" / ").to_lowercase(),
                );
                ka.cmp(&kb).then_with(|| a.cmp(b))
            });
            ids.into_iter()
                .filter(|id| !index.has_sealed_ancestor(id, unlocked))
                .filter_map(|id| index.get(id))
                .map(|row| PickerContainerHit {
                    id: row.id.clone(),
                    name: row.name.clone(),
                    level: row.level.clone(),
                    breadcrumb: index.breadcrumb(&row.id),
                    availability: rules.availability(&index, row, unlocked),
                })
                .collect()
        }
    };
    let container_total = container_hits.len() as u32;
    let container_page: Vec<PickerContainerHit> = container_hits
        .into_iter()
        .skip(offset as usize)
        .take(limit as usize)
        .collect();
    // Leaves fill whatever is left of THIS page, at the offset the caller has already consumed in
    // containers. `max(1)` only satisfies the store's own clamp; the slice is truncated back.
    let leaf_room = limit - container_page.len() as u32;
    let leaf_offset = offset.saturating_sub(container_total);
    let (rows, total) = db.related_picker_search(
        query,
        leaf_offset,
        leaf_room.max(1),
        unlocked,
        &reachable,
        &breadcrumb_matched,
        include_unclassified,
    )?;
    let hits = rows
        .into_iter()
        .take(leaf_room as usize)
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
        total: total + container_total,
        containers: dest.map(|_| container_page),
    })
}

/// The one label for the synthetic top-level node, resolved in the BACKEND so the tree, the search
/// breadcrumbs and the modal's copy cannot drift apart.
pub(crate) const UNCLASSIFIED_LABEL: &str = "Not classified";

#[cfg(test)]
#[path = "tests/related_picker_tests.rs"]
mod related_picker_tests;
