//! SHARED CONTAINERS — publish a whole Folder or Space to an org, keep it live, and read back the
//! containers other members shared.
//!
//! Design: `docs/superpowers/specs/2026-08-29-shared-containers-design.md`.
//!
//! ## Why this is a separate module from `org.rs`
//!
//! A container manifest has no local `meeting_id` or `document_id`, so it cannot ride the
//! `org_shares` logical key that every helper in the note publish path is built around. It gets its
//! own journal (`org_container_shares`) and its own publisher instead. What it does NOT get is its
//! own security posture — the gate order is the same one `publish_org_body_with_policy` follows:
//!
//!   1. refuse a sealed container before reading anything;
//!   2. org-egress consent, fail-closed;
//!   3. clean + scrub;
//!   4. journal the intent BEFORE the socket, so a crash mid-publish is recoverable;
//!   5. seal under the OCK and re-open it locally — never upload a blob we cannot decrypt back;
//!   6. size pre-check against the relay's cap;
//!   7. a content-free egress-ledger row, committed with the intent.
//!
//! ## What never happens here
//!
//! No transcript and no audio. A meeting travels as its note, exactly as a single meeting share
//! does today. No dashboard: a dashboard can reference items that were never shared, and there is
//! no honest answer to that yet. No sealed content, ever — a sealed descendant is skipped and
//! counted, never read.

use std::collections::{HashMap, HashSet};

use tauri::{AppHandle, State};

use super::org_commands::{
    acquire_org_ock, authenticated_org_actor, delete_legacy_org_item, org_author_hint,
    org_dispatch_cell_sha256, org_item_nonce, org_set_item_access_inner, permit_simple_org_dispatch,
    persist_container_publish_intent, require_org_egress_consent, resolve_org,
    revoke_org_share_inner_with_policy, scrub_org_text, share_to_org_placed_notifying, ContainerPlacement,
    OrgDispatchOperation, OrgWorkPolicy, CONTAINER_SHARE_SEAL_FAILED,
};
use super::share_base_url;
use crate::error::{AppError, Result};
use crate::share::container_envelope::{ContainerEnvelope, ContainerLevel};
use crate::share::org_dto::OrgItemAccess;
use crate::share::org_envelope::{OrgItemKind, OrgSourceKind};
use crate::state::AppState;
use crate::storage::models::{ContainerShareRow, ItemKind, ItemRow};

/// The most items one container share may publish in a single action.
///
/// A cap exists because a share is N sequential network round-trips and an unbounded one would look
/// like a hang with no way to tell how far it got. The refusal is raised during PLANNING, before any
/// egress, so an oversized container costs nothing.
pub const MAX_CONTAINER_SHARE_ITEMS: usize = 500;

/// A container the plan will publish a manifest for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlannedContainer {
    pub(crate) folder_id: String,
    pub(crate) parent_folder_id: Option<String>,
    pub(crate) container_id: String,
    pub(crate) parent_container_id: Option<String>,
    pub(crate) level: ContainerLevel,
    pub(crate) name: String,
    pub(crate) emoji: Option<String>,
    pub(crate) tint: Option<String>,
    pub(crate) position: i64,
    pub(crate) is_root: bool,
}

/// A document the plan will publish into one of those containers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlannedDocument {
    pub(crate) meeting_id: Option<String>,
    pub(crate) document_id: Option<String>,
    pub(crate) parent_container_id: String,
    pub(crate) position: i64,
}

/// Everything one container share will publish, and everything it deliberately will not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ContainerPlan {
    /// Root first, then every descendant after its own parent.
    pub(crate) containers: Vec<PlannedContainer>,
    pub(crate) documents: Vec<PlannedDocument>,
    pub(crate) note_count: u32,
    pub(crate) meeting_count: u32,
    pub(crate) skipped_sealed: u32,
    pub(crate) skipped_dashboards: u32,
}

impl ContainerPlan {
    fn total_items(&self) -> usize {
        self.containers.len() + self.documents.len()
    }
}

/// Enumerate what sharing `folder_id` would publish.
///
/// REFUSES a sealed root: a sealed container's content is not readable, and returning an empty plan
/// instead of a refusal would let the user "share" a folder and see nothing arrive, with no
/// explanation. SKIPS a sealed descendant and everything beneath it, counting it so the preview can
/// say so out loud.
pub(crate) fn plan_container_share(
    state: &AppState,
    org_id: &str,
    folder_id: &str,
) -> Result<ContainerPlan> {
    let containers = state.db.list_containers()?;
    let root = containers
        .iter()
        .find(|c| c.id == folder_id)
        .ok_or_else(|| AppError::InvalidArg("no such Space or folder".into()))?;
    if root.locked {
        return Err(AppError::Locked(
            "unlock this Space before sharing it".into(),
        ));
    }
    if root.is_root {
        return Err(AppError::InvalidArg(
            "the Notes root is not a shareable container".into(),
        ));
    }

    // Reuse the identity this device already published for a container, so a re-share supersedes
    // the same document instead of minting a second one in every member's sidebar.
    let existing: HashMap<String, String> = state
        .db
        .list_container_shares(Some(org_id))?
        .into_iter()
        .map(|row| (row.folder_id, row.container_id))
        .collect();
    let container_id_for = |folder: &str| -> String {
        existing
            .get(folder)
            .cloned()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
    };

    let by_parent: HashMap<Option<String>, Vec<&crate::storage::models::ContainerRow>> =
        containers.iter().fold(HashMap::new(), |mut acc, c| {
            acc.entry(c.parent_id.clone()).or_default().push(c);
            acc
        });

    let mut plan = ContainerPlan::default();
    // Breadth-first from the root, so a parent's manifest is always planned before its children's.
    // A recipient that receives them out of order still reassembles the tree (structure is a parent
    // pointer), but publishing in tree order means a partial share is never a forest of orphans.
    let mut queue = vec![(root, None::<String>, true)];
    while let Some((container, parent_container_id, is_root)) = queue.pop() {
        if container.locked && !is_root {
            // Sealed subtree: skipped whole. Its items are not read, not counted, not published.
            plan.skipped_sealed += 1;
            continue;
        }
        let container_id = container_id_for(&container.id);
        plan.containers.push(PlannedContainer {
            folder_id: container.id.clone(),
            parent_folder_id: container.parent_id.clone(),
            container_id: container_id.clone(),
            parent_container_id: parent_container_id.clone(),
            level: ContainerLevel::from_local_level(&container.level),
            name: container.name.clone(),
            emoji: container.emoji.clone(),
            tint: container.tint.clone(),
            position: container.position,
            is_root,
        });

        for (position, item) in container_documents(state, &container.id, &mut plan)?
            .into_iter()
            .enumerate()
        {
            plan.documents.push(PlannedDocument {
                meeting_id: item.meeting_id,
                document_id: item.document_id,
                parent_container_id: container_id.clone(),
                position: position as i64,
            });
        }

        if let Some(children) = by_parent.get(&Some(container.id.clone())) {
            // Reverse so the pop order above stays the display order.
            for child in children.iter().rev() {
                queue.push((child, Some(container_id.clone()), false));
            }
        }
    }

    if plan.total_items() > MAX_CONTAINER_SHARE_ITEMS {
        return Err(AppError::InvalidArg(format!(
            "this Space holds {} items; a single share publishes at most {MAX_CONTAINER_SHARE_ITEMS} — share a smaller folder instead",
            plan.total_items()
        )));
    }
    Ok(plan)
}

/// One publishable document inside a container.
struct ContainerDocument {
    meeting_id: Option<String>,
    document_id: Option<String>,
}

/// Read one container's publishable items THROUGH THE GATED READER.
///
/// `container_items_page` takes the session unlock set and applies `visibility_clause`, so a sealed
/// item cannot appear here even if the walk above somehow reached a sealed container. Dashboards
/// are counted and dropped: they are the one kind this feature deliberately does not publish.
fn container_documents(
    state: &AppState,
    folder_id: &str,
    plan: &mut ContainerPlan,
) -> Result<Vec<ContainerDocument>> {
    let unlocked = unlocked_folder_ids(state);
    let mut out = Vec::new();
    for kind in [ItemKind::Meeting, ItemKind::Note] {
        let (rows, _total) = state.db.container_items_page(
            Some(folder_id),
            kind,
            0,
            MAX_CONTAINER_SHARE_ITEMS as u32,
            &unlocked,
        )?;
        for row in rows {
            match kind {
                ItemKind::Meeting => {
                    plan.meeting_count += 1;
                    out.push(ContainerDocument {
                        meeting_id: Some(row.id),
                        document_id: None,
                    });
                }
                _ => {
                    plan.note_count += 1;
                    out.push(ContainerDocument {
                        meeting_id: None,
                        document_id: Some(row.id),
                    });
                }
            }
        }
    }
    let (dashboards, _) = state.db.container_items_page(
        Some(folder_id),
        ItemKind::Dashboard,
        0,
        MAX_CONTAINER_SHARE_ITEMS as u32,
        &unlocked,
    )?;
    plan.skipped_dashboards += dashboards.len() as u32;
    Ok(out)
}

fn unlocked_folder_ids(state: &AppState) -> HashSet<String> {
    state
        .unlocked_folders
        .lock()
        .map(|set| set.clone())
        .unwrap_or_default()
}

/// A stable, deterministic creation timestamp for a manifest.
///
/// Deliberately NOT `Utc::now()`: a manifest republished with an unchanged name would otherwise
/// hash differently on every sweep and re-publish forever.
fn manifest_created_at(container_id: &str) -> String {
    let _ = container_id;
    "1970-01-01T00:00:00Z".to_string()
}

/// Publish (or re-publish) one container manifest.
///
/// Returns the server item id. The journal row is written BEFORE the socket and flipped to
/// `published` only on an authenticated success, so an interrupted publish is left `failed` for the
/// sweep rather than silently lost.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn publish_container_manifest(
    state: &AppState,
    org_id: &str,
    planned: &PlannedContainer,
    access: OrgItemAccess,
    scrub: bool,
) -> Result<String> {
    require_org_egress_consent(state)?;
    let org = resolve_org(state, org_id)?;
    let base = share_base_url(state)?;
    let (access_token, publisher_user_id) = authenticated_org_actor(state).await?;
    let client = crate::share::client::ShareClient::new(&base)?;
    let generation = org.generation;
    let now = chrono::Utc::now().to_rfc3339();

    // A container NAME is user-authored text that crosses to another device, so it passes the same
    // redaction the note body passes when the user asked for scrubbing.
    let name = if scrub {
        scrub_org_text(&planned.name)
    } else {
        planned.name.clone()
    };

    let existing = state.db.get_container_share(org_id, &planned.folder_id)?;
    let rev = existing.as_ref().map(|row| row.rev).unwrap_or(0) + 1;
    let share_id = existing
        .as_ref()
        .map(|row| row.id.clone())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    let manifest = ContainerEnvelope {
        v: crate::share::container_envelope::CONTAINER_ENVELOPE_VERSION,
        container_id: planned.container_id.clone(),
        level: planned.level,
        name: name.clone(),
        emoji: planned.emoji.clone(),
        tint: planned.tint.clone(),
        parent_container_id: planned.parent_container_id.clone(),
        position: planned.position,
    };
    let env = crate::share::org_envelope::OrgEnvelope::new(
        OrgItemKind::Container,
        name.clone(),
        manifest.to_json(),
        org_author_hint_for(state),
        manifest_created_at(&planned.container_id),
        rev,
        OrgSourceKind::Container,
    );
    let content_sha = env.content_sha256();

    // Journal the row FIRST. A crash between here and the socket leaves a recoverable `queued` row
    // rather than a container nobody knows was half-published.
    state.db.upsert_container_share(&ContainerShareRow {
        id: share_id.clone(),
        org_id: org_id.to_string(),
        folder_id: planned.folder_id.clone(),
        container_id: planned.container_id.clone(),
        access: access.as_str().to_string(),
        scrub,
        is_root: planned.is_root,
        state: "queued".into(),
        item_id: existing.as_ref().and_then(|row| row.item_id.clone()),
        rev,
        generation,
        content_sha256: Some(content_sha.clone()),
        position: planned.position,
        last_error: None,
        created_at: existing
            .as_ref()
            .map(|row| row.created_at.clone())
            .unwrap_or_else(|| now.clone()),
        updated_at: now.clone(),
    })?;

    // Seal under the OCK and re-open it locally — verify-before-egress. The AAD item nonce is the
    // plaintext content hash, which rides the feed, so every member reconstructs the same AAD.
    let ock = acquire_org_ock(state, &org.org_id, generation).await?;
    let item_nonce = org_item_nonce(&content_sha);
    let (ciphertext, _sha) =
        match crate::share::org_envelope::seal_org_envelope(&ock, &env, &org.org_id, &item_nonce) {
            Ok(sealed) => sealed,
            Err(e) => {
                state.db.set_container_share_state(
                    &share_id,
                    "failed",
                    None,
                    rev,
                    None,
                    Some(CONTAINER_SHARE_SEAL_FAILED),
                    &now,
                )?;
                return Err(e);
            }
        };
    if ciphertext.len() > murmur_protocol::caps::MAX_ORG_ITEM_BLOB_BYTES {
        state.db.set_container_share_state(
            &share_id,
            "failed",
            None,
            rev,
            None,
            Some("too_large"),
            &now,
        )?;
        return Err(AppError::InvalidArg(
            "this container's name is too large to share".into(),
        ));
    }

    let cell_sha = org_dispatch_cell_sha256(&ciphertext);
    let (permit, _dispatch_id) = persist_container_publish_intent(
        state,
        &share_id,
        &org.org_id,
        &planned.container_id,
        access,
        rev,
        generation,
        &content_sha,
        &publisher_user_id,
        &now,
        &client.host(),
        ciphertext.len(),
        cell_sha,
    )?;

    let published = client
        .org_publish_item(
            &access_token,
            &org.org_id,
            crate::share::org_dto::PublishItemRequest {
                mutation_id: None,
                doc_id: Some(planned.container_id.clone()),
                access: Some(access),
                blob_id: None,
                content_cell: Some(ciphertext.clone()),
                content_sha256: content_sha.clone(),
                rev,
                generation,
            },
            permit,
        )
        .await?;

    let previous_item = existing.as_ref().and_then(|row| row.item_id.clone());
    state.db.set_container_share_state(
        &share_id,
        "published",
        Some(&published.item_id),
        rev,
        Some(&content_sha),
        None,
        &chrono::Utc::now().to_rfc3339(),
    )?;

    // A manifest revision supersedes its predecessor. Tombstone the old item so a member who
    // already synced it does not keep rendering the stale name alongside the new one. Best-effort:
    // a failed tombstone leaves a duplicate for the sweep, never a failed publish.
    if let Some(old_item) = previous_item {
        if old_item != published.item_id {
            let _ = tombstone_container_item(state, &client, &access_token, &org.org_id, &old_item)
                .await;
        }
    }
    Ok(published.item_id)
}

async fn tombstone_container_item(
    state: &AppState,
    client: &crate::share::client::ShareClient,
    access_token: &str,
    org_id: &str,
    item_id: &str,
) -> Result<()> {
    let permit = permit_simple_org_dispatch(
        state,
        &client.host(),
        "org_share_revoke",
        OrgDispatchOperation::Tombstone {
            org_id: org_id.to_string(),
            item_id: item_id.to_string(),
        },
    )?;
    delete_legacy_org_item(state, client, access_token, org_id, item_id, permit).await
}

/// Withdraw one container manifest and forget its journal row.
pub(crate) async fn withdraw_container_manifest(state: &AppState, share_id: &str) -> Result<()> {
    let Some(row) = state
        .db
        .list_container_shares(None)?
        .into_iter()
        .find(|r| r.id == share_id)
    else {
        return Ok(());
    };
    let now = chrono::Utc::now().to_rfc3339();
    if let Some(item_id) = row.item_id.clone() {
        state.db.set_container_share_state(
            &row.id,
            "revoke_pending",
            None,
            row.rev,
            None,
            None,
            &now,
        )?;
        let base = share_base_url(state)?;
        let (access_token, _me) = authenticated_org_actor(state).await?;
        let client = crate::share::client::ShareClient::new(&base)?;
        tombstone_container_item(state, &client, &access_token, &row.org_id, &item_id).await?;
    }
    state.db.delete_container_share(&row.id)?;
    Ok(())
}

fn org_author_hint_for(state: &AppState) -> String {
    state
        .account_session
        .lock()
        .ok()
        .and_then(|guard| {
            crate::share::require_login(&guard)
                .ok()
                .map(|session| org_author_hint(&session.email))
        })
        .unwrap_or_else(|| "member".to_string())
}

// ── Commands ──────────────────────────────────────────────────────────────────────────────────

/// What sharing this container would publish — counts only, no content.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerSharePreview {
    pub folder_id: String,
    pub name: String,
    /// `"space"` or `"folder"`.
    pub level: String,
    pub note_count: u32,
    pub meeting_count: u32,
    /// Sub-folders whose own manifest will be published (the root is not counted).
    pub folder_count: u32,
    pub skipped_sealed: u32,
    pub skipped_dashboards: u32,
    pub total_items: u32,
}

/// The outcome of one container share.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerShareResult {
    pub container_id: String,
    pub published: u32,
    pub failed: u32,
}

/// One shared container, as the sidebar needs to know about it.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerShareStatus {
    pub org_id: String,
    pub org_name: String,
    pub folder_id: String,
    pub container_id: String,
    /// `"view"` or `"edit"`.
    pub access: String,
    pub is_root: bool,
    pub state: String,
}

#[tauri::command]
pub async fn preview_container_share(
    state: State<'_, AppState>,
    org_id: String,
    folder_id: String,
) -> Result<ContainerSharePreview> {
    let st = state.inner();
    let containers = st.db.list_containers()?;
    let row = containers
        .iter()
        .find(|c| c.id == folder_id)
        .ok_or_else(|| AppError::InvalidArg("no such Space or folder".into()))?;
    let level = ContainerLevel::from_local_level(&row.level);
    let name = row.name.clone();
    let plan = plan_container_share(st, &org_id, &folder_id)?;
    Ok(ContainerSharePreview {
        folder_id,
        name,
        level: level.as_str().to_string(),
        note_count: plan.note_count,
        meeting_count: plan.meeting_count,
        folder_count: plan.containers.len().saturating_sub(1) as u32,
        skipped_sealed: plan.skipped_sealed,
        skipped_dashboards: plan.skipped_dashboards,
        total_items: plan.total_items() as u32,
    })
}

#[tauri::command]
pub async fn share_container_to_org(
    app: AppHandle,
    state: State<'_, AppState>,
    org_id: String,
    folder_id: String,
    access: OrgItemAccess,
    scrub: bool,
) -> Result<ContainerShareResult> {
    let result =
        share_container_to_org_inner(state.inner(), &org_id, &folder_id, access, scrub, Some(&app))
            .await?;
    crate::events::emit_org_feed_updated(&app, 1);
    Ok(result)
}

pub(crate) async fn share_container_to_org_inner(
    state: &AppState,
    org_id: &str,
    folder_id: &str,
    access: OrgItemAccess,
    scrub: bool,
    app: Option<&AppHandle>,
) -> Result<ContainerShareResult> {
    let plan = plan_container_share(state, org_id, folder_id)?;
    let total = plan.total_items() as u32;
    let mut done = 0u32;
    let mut failed = 0u32;
    let root_container_id = plan
        .containers
        .first()
        .map(|c| c.container_id.clone())
        .ok_or_else(|| AppError::InvalidArg("nothing to share in this container".into()))?;

    for container in &plan.containers {
        match publish_container_manifest(state, org_id, container, access, scrub).await {
            Ok(_) => done += 1,
            Err(e) => {
                // A manifest that will not publish takes its subtree with it — publishing the
                // documents anyway would file them under a container nobody can see.
                if container.is_root {
                    return Err(e);
                }
                failed += 1;
            }
        }
        emit_progress(app, done + failed, total);
    }

    let live: HashSet<String> = state
        .db
        .list_container_shares(Some(org_id))?
        .into_iter()
        .filter(|row| row.state == "published")
        .map(|row| row.container_id)
        .collect();

    for document in &plan.documents {
        if !live.contains(&document.parent_container_id) {
            failed += 1;
            emit_progress(app, done + failed, total);
            continue;
        }
        let placement = ContainerPlacement {
            parent_container_id: document.parent_container_id.clone(),
            position: document.position,
            // The container sweep owns this share: unsharing the container may withdraw it.
            // An already-explicit row keeps its own flag — `share_to_org_placed_notifying`
            // returns the existing row untouched when the source is already live.
            explicit: false,
        };
        match share_to_org_placed_notifying(
            state,
            org_id,
            document.meeting_id.clone(),
            document.document_id.clone(),
            scrub,
            access,
            Some(placement),
            app,
        )
        .await
        {
            Ok(_) => done += 1,
            Err(_) => failed += 1,
        }
        emit_progress(app, done + failed, total);
    }

    Ok(ContainerShareResult {
        container_id: root_container_id,
        published: done,
        failed,
    })
}

fn emit_progress(app: Option<&AppHandle>, done: u32, total: u32) {
    if let Some(handle) = app {
        crate::events::emit_container_share_progress(handle, done, total);
    }
}

#[tauri::command]
pub async fn unshare_container(
    app: AppHandle,
    state: State<'_, AppState>,
    org_id: String,
    folder_id: String,
) -> Result<()> {
    unshare_container_inner(state.inner(), &org_id, &folder_id, Some(&app)).await?;
    crate::events::emit_org_feed_updated(&app, 1);
    Ok(())
}

/// Stop sharing a container.
///
/// Withdraws every descendant manifest and every document the container sweep published under it.
/// A document the user shared THEMSELVES (`explicit`) only loses its placement and stays live — the
/// user asked for that share separately, and unsharing a folder must not silently revoke it.
pub(crate) async fn unshare_container_inner(
    state: &AppState,
    org_id: &str,
    folder_id: &str,
    app: Option<&AppHandle>,
) -> Result<()> {
    let Some(root) = state.db.get_container_share(org_id, folder_id)? else {
        return Ok(());
    };
    for share in descendant_container_shares(state, org_id, &root)? {
        for doc in state
            .db
            .org_shares_in_container(org_id, &share.container_id)?
        {
            if doc.explicit {
                state.db.set_org_share_placement(&doc.id, None, 0, true)?;
                continue;
            }
            if let Some(item_id) = doc.item_id.clone() {
                let _ = revoke_org_share_inner_with_policy(state, item_id, OrgWorkPolicy::manual(), app).await;
            } else {
                state.db.set_org_share_state(
                    &doc.id,
                    "revoked",
                    &chrono::Utc::now().to_rfc3339(),
                )?;
            }
        }
        withdraw_container_manifest(state, &share.id).await?;
    }
    Ok(())
}

/// A container share and every share beneath it, deepest LAST so a caller can withdraw children
/// before their parent.
fn descendant_container_shares(
    state: &AppState,
    org_id: &str,
    root: &ContainerShareRow,
) -> Result<Vec<ContainerShareRow>> {
    let all = state.db.list_container_shares(Some(org_id))?;
    let containers = state.db.list_containers()?;
    let mut wanted: HashSet<String> = HashSet::new();
    wanted.insert(root.folder_id.clone());
    // Walk the LOCAL tree: a descendant share is one whose folder sits under the root folder.
    let mut changed = true;
    while changed {
        changed = false;
        for container in &containers {
            if let Some(parent) = container.parent_id.as_ref() {
                if wanted.contains(parent) && wanted.insert(container.id.clone()) {
                    changed = true;
                }
            }
        }
    }
    Ok(all
        .into_iter()
        .filter(|row| wanted.contains(&row.folder_id))
        .collect())
}

#[tauri::command]
pub async fn set_container_share_access(
    app: AppHandle,
    state: State<'_, AppState>,
    org_id: String,
    folder_id: String,
    access: OrgItemAccess,
) -> Result<()> {
    set_container_share_access_inner(state.inner(), &org_id, &folder_id, access).await?;
    crate::events::emit_org_feed_updated(&app, 0);
    Ok(())
}

/// Re-permission a whole container: the manifest and every document filed under it.
///
/// Walked explicitly rather than inferred, because the relay authorizes per document. A container
/// whose manifest says `edit` while its notes still say `view` would be a UI that lies about what
/// members can do.
pub(crate) async fn set_container_share_access_inner(
    state: &AppState,
    org_id: &str,
    folder_id: &str,
    access: OrgItemAccess,
) -> Result<()> {
    let Some(root) = state.db.get_container_share(org_id, folder_id)? else {
        return Err(AppError::InvalidArg("this container is not shared".into()));
    };
    let now = chrono::Utc::now().to_rfc3339();
    for share in descendant_container_shares(state, org_id, &root)? {
        for doc in state
            .db
            .org_shares_in_container(org_id, &share.container_id)?
        {
            if let Some(item_id) = doc.item_id.clone() {
                org_set_item_access_inner(state, &item_id, access).await?;
            }
        }
        state
            .db
            .set_container_share_access(&share.id, access.as_str(), &now)?;
        // Re-publishing the manifest is what carries the new access to the relay for the container
        // document itself; the descendants above carry their own.
        if let Some(planned) = planned_from_share(state, &share)? {
            publish_container_manifest(state, org_id, &planned, access, share.scrub).await?;
        }
    }
    Ok(())
}

/// Rebuild the manifest inputs for an already-shared container from the CURRENT local folder row.
///
/// Returns `None` when the local folder is gone — the caller treats that as "withdraw", never as
/// "publish an empty name".
fn planned_from_share(
    state: &AppState,
    share: &ContainerShareRow,
) -> Result<Option<PlannedContainer>> {
    let containers = state.db.list_containers()?;
    let Some(row) = containers.iter().find(|c| c.id == share.folder_id) else {
        return Ok(None);
    };
    let parent_container_id = row.parent_id.as_ref().and_then(|parent| {
        state
            .db
            .get_container_share(&share.org_id, parent)
            .ok()
            .flatten()
            .map(|p| p.container_id)
    });
    Ok(Some(PlannedContainer {
        folder_id: row.id.clone(),
        parent_folder_id: row.parent_id.clone(),
        container_id: share.container_id.clone(),
        parent_container_id,
        level: ContainerLevel::from_local_level(&row.level),
        name: row.name.clone(),
        emoji: row.emoji.clone(),
        tint: row.tint.clone(),
        position: row.position,
        is_root: share.is_root,
    }))
}

#[tauri::command]
pub fn list_container_share_status(
    state: State<'_, AppState>,
) -> Result<Vec<ContainerShareStatus>> {
    let st = state.inner();
    let org_names: HashMap<String, String> = st
        .db
        .list_org_states()?
        .into_iter()
        .map(|org| (org.org_id, org.name))
        .collect();
    Ok(st
        .db
        .list_container_shares(None)?
        .into_iter()
        .map(|row| ContainerShareStatus {
            org_name: org_names.get(&row.org_id).cloned().unwrap_or_default(),
            org_id: row.org_id,
            folder_id: row.folder_id,
            container_id: row.container_id,
            access: row.access,
            is_root: row.is_root,
            state: row.state,
        })
        .collect())
}

/// One item row is unused today but keeps the planner honest about what it consumed.
#[allow(dead_code)]
fn _assert_item_row_shape(row: &ItemRow) -> &str {
    &row.id
}
