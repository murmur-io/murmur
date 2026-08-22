//! Workspace-hierarchy command surface — the container forest (Projects › Folders › items) the new
//! sidebar renders, plus its paged item reader.
//!
//! READ-ONLY. Nothing here seals, unseals, mints a key, or writes a row. The gate is the SAME one
//! every shipped reader uses: the storage layer runs `visibility_clause` (expressed exactly as
//! `list_meetings_visible` and `list_notes_visible` express it), and this layer refuses a
//! sealed-and-not-session-unlocked container outright rather than answering with an empty page —
//! an empty page and a refusal are distinguishable by a prober, and only the refusal is honest.
//!
//! Nothing here changes behaviour for a database as it exists today: a container is a Project only
//! when `folders.level = 'project'`, which no row carries until the separate `hierarchy_v1` data
//! migration runs. Until then [`list_workspace_tree`] truthfully returns an empty forest.

use super::*;
use crate::storage::models::{
    ContainerDto, ContainerNode, ContainerRow, ItemKind, ItemPage, TypeGroup,
};

/// How many items of each kind the TREE carries per container. Everything beyond this is reached
/// through [`list_container_items`], so the tree payload stays bounded no matter how large a
/// container grows — `list_notes` has no LIMIT at all and `list_meetings` is a flat 200, so an
/// unbounded tree would be the worst payload in the app.
const TREE_ITEMS_PER_GROUP: u32 = 8;

/// Upper bound on one [`list_container_items`] page, so a caller cannot ask for the whole vault.
const MAX_ITEM_PAGE: u32 = 200;

/// The container forest: projects, their child folders, and each container's per-kind item groups.
///
/// Groups appear in [`ItemKind::ORDER`] and an EMPTY group is omitted entirely rather than sent
/// with a zero count, so the UI's "hide an empty type" rule needs no client-side filtering.
///
/// A sealed-and-not-session-unlocked container reports `locked: true` and carries NO groups — no
/// item rows and no totals. Its child folders are still listed by NAME, which is the policy the
/// shipped tree already follows (`list_folders` returns locked folders with their names, and
/// `count_notes_per_folder` gates their counts to zero): a user has to be able to see the container
/// in order to unlock it.
#[tauri::command]
pub fn list_workspace_tree(state: State<'_, AppState>) -> Result<Vec<ContainerNode>, AppError> {
    let unlocked = unlocked_snapshot(state.inner())?;
    workspace_tree_inner(&state.db, &unlocked)
}

/// Inner of [`list_workspace_tree`], taking the pieces directly so the tree assembly — the gate
/// restatement, the empty-group rule, the depth walk — is unit-testable without a `tauri::State`.
pub(crate) fn workspace_tree_inner(
    db: &crate::storage::db::Db,
    unlocked: &std::collections::HashSet<String>,
) -> Result<Vec<ContainerNode>, AppError> {
    let containers = db.list_containers()?;

    // One windowed page query + one grouped COUNT per KIND — not per container. The number of
    // statements is therefore constant in the number of projects and folders.
    let mut pages_by_kind = std::collections::HashMap::new();
    let mut totals_by_kind = std::collections::HashMap::new();
    for kind in ItemKind::ORDER {
        pages_by_kind.insert(
            kind,
            db.container_group_pages(kind, TREE_ITEMS_PER_GROUP, unlocked)?,
        );
        totals_by_kind.insert(kind, db.container_group_totals(kind, unlocked)?);
    }

    let groups_for = |row: &ContainerRow| -> Vec<TypeGroup> {
        // The gate, restated at the assembly layer so a future reader change cannot silently start
        // disclosing totals for a sealed container. The storage legs already return nothing for it;
        // this makes the intent unmissable rather than emergent.
        if row.locked && !unlocked.contains(&row.id) {
            return Vec::new();
        }
        let key = Some(row.id.clone());
        ItemKind::ORDER
            .iter()
            .filter_map(|kind| {
                let total = totals_by_kind
                    .get(kind)
                    .and_then(|t| t.get(&key))
                    .copied()
                    .unwrap_or(0);
                if total == 0 {
                    return None; // an empty type group is ABSENT, not a zero.
                }
                let items = pages_by_kind
                    .get(kind)
                    .and_then(|p| p.get(&key))
                    .cloned()
                    .unwrap_or_default();
                Some(TypeGroup {
                    kind: *kind,
                    total,
                    items,
                })
            })
            .collect()
    };

    let node = |row: &ContainerRow, folders: Vec<ContainerNode>| ContainerNode {
        id: row.id.clone(),
        name: row.name.clone(),
        level: row.level.clone(),
        emoji: row.emoji.clone(),
        tint: row.tint.clone(),
        locked: row.locked,
        unlocked: row.locked && unlocked.contains(&row.id),
        is_root: row.is_root,
        folders,
        groups: groups_for(row),
    };

    // Children by parent, so assembling the forest is one pass rather than a scan per container.
    let mut children: std::collections::HashMap<String, Vec<&ContainerRow>> =
        std::collections::HashMap::new();
    for row in &containers {
        if let Some(parent) = row.parent_id.as_deref() {
            children.entry(parent.to_string()).or_default().push(row);
        }
    }

    // Sub-folders are out of scope for the hierarchy, but a database may already contain depth the
    // backend has always supported, so the reader RENDERS whatever depth exists rather than
    // silently dropping rows. `seen` makes a corrupted parent cycle terminate instead of hanging.
    fn descend(
        row: &ContainerRow,
        children: &std::collections::HashMap<String, Vec<&ContainerRow>>,
        seen: &mut std::collections::HashSet<String>,
        node: &dyn Fn(&ContainerRow, Vec<ContainerNode>) -> ContainerNode,
    ) -> ContainerNode {
        if !seen.insert(row.id.clone()) {
            return node(row, Vec::new());
        }
        let kids = children
            .get(&row.id)
            .map(|rows| {
                rows.iter()
                    .map(|child| descend(child, children, seen, node))
                    .collect()
            })
            .unwrap_or_default();
        node(row, kids)
    }

    let mut seen = std::collections::HashSet::new();
    // A root is a project row that is nobody's child. The `parent_id` half is belt-and-braces: the
    // migration only ever promotes rows that already had no parent, but a project row that somehow
    // carried one would otherwise render TWICE — once as a root and once inside its parent.
    Ok(containers
        .iter()
        .filter(|row| row.level == LEVEL_PROJECT && row.parent_id.is_none())
        .map(|row| descend(row, &children, &mut seen, &node))
        .collect())
}

/// One page of a single container's items of a single kind. `container_id = None` is the INBOX —
/// items with no container at all, which after the hierarchy migration means unfiled meetings.
///
/// Refuses a sealed-and-not-session-unlocked container with [`AppError::Locked`].
#[tauri::command]
pub fn list_container_items(
    state: State<'_, AppState>,
    container_id: Option<String>,
    kind: ItemKind,
    offset: u32,
    limit: u32,
) -> Result<ItemPage, AppError> {
    let unlocked = unlocked_snapshot(state.inner())?;
    container_items_inner(
        &state.db,
        &unlocked,
        container_id.as_deref(),
        kind,
        offset,
        limit,
    )
}

/// Inner of [`list_container_items`] — the refusal and the clamp, unit-testable without a
/// `tauri::State`.
pub(crate) fn container_items_inner(
    db: &crate::storage::db::Db,
    unlocked: &std::collections::HashSet<String>,
    container_id: Option<&str>,
    kind: ItemKind,
    offset: u32,
    limit: u32,
) -> Result<ItemPage, AppError> {
    if let Some(id) = container_id {
        // Resolve through `list_containers` rather than `folder_by_id`, so this reader applies the
        // SAME system-container exclusion the tree applies. `folder_by_id` would happily resolve a
        // machine-owned `.murmur/` container that the tree deliberately hides, making the paged
        // reader a SECOND sink on rows the first sink refuses — two sinks that disagree are how a
        // hidden container becomes visible through the back door.
        //
        // Fail-CLOSED for anything not found, mirroring `commands::folder_is_unlocked`: a caller must
        // not learn "this container does not exist" (or "is a system container") by getting an empty
        // page instead of a refusal.
        let visible = db.list_containers()?;
        let sealed = match visible.iter().find(|c| c.id == id) {
            None => true,
            Some(row) => row.locked && !unlocked.contains(id),
        };
        if sealed {
            return Err(AppError::Locked(
                "this container is locked — unlock it to see what is inside".into(),
            ));
        }
    }
    let limit = limit.clamp(1, MAX_ITEM_PAGE);
    let (items, total) = db.container_items_page(container_id, kind, offset, limit, unlocked)?;
    Ok(ItemPage { kind, items, total })
}

/// Resolve one container for a breadcrumb or header, including its owning project's name.
///
/// A sealed-and-not-session-unlocked container still resolves: its NAME is what the user needs in
/// order to unlock it, and the shipped folder tree already discloses locked folder names. Nothing
/// about its CONTENTS is returned here.
#[tauri::command]
pub fn get_container(
    state: State<'_, AppState>,
    id: String,
) -> Result<Option<ContainerDto>, AppError> {
    let unlocked = unlocked_snapshot(state.inner())?;
    get_container_inner(&state.db, &unlocked, &id)
}

/// Inner of [`get_container`].
pub(crate) fn get_container_inner(
    db: &crate::storage::db::Db,
    unlocked: &std::collections::HashSet<String>,
    id: &str,
) -> Result<Option<ContainerDto>, AppError> {
    let containers = db.list_containers()?;
    let Some(row) = containers.iter().find(|c| c.id == id) else {
        return Ok(None);
    };
    let parent = row
        .parent_id
        .as_deref()
        .and_then(|pid| containers.iter().find(|c| c.id == pid));
    Ok(Some(ContainerDto {
        id: row.id.clone(),
        name: row.name.clone(),
        level: row.level.clone(),
        emoji: row.emoji.clone(),
        tint: row.tint.clone(),
        locked: row.locked,
        unlocked: row.locked && unlocked.contains(&row.id),
        is_root: row.is_root,
        parent_id: parent.map(|p| p.id.clone()),
        parent_name: parent.map(|p| p.name.clone()),
    }))
}

/// The `folders.level` value that marks a Project. Kept next to its only readers so the string
/// literal is never duplicated at a call site.
pub(crate) const LEVEL_PROJECT: &str = "project";
