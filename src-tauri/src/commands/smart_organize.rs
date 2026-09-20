//! Deterministic, bounded, review-first organization. Renderer IDs select from RAM-only authority;
//! no renderer destination is accepted. Every move shares its final gates with the canonical writer.
use super::*;
use crate::storage::models::{
    SmartOrganizeApplied, SmartOrganizeApplyRequest, SmartOrganizeBucket,
    SmartOrganizeBucketStatus, SmartOrganizeDiscardRequest, SmartOrganizeFailure,
    SmartOrganizeKind, SmartOrganizeMove, SmartOrganizePreview, SmartOrganizeReceipt,
    SmartOrganizeRequest, SmartOrganizeRule, SmartOrganizeSkip, SmartOrganizeSkipCode,
};
use crate::storage::smart_organize_store::{
    smart_organize_names_equal, SmartOrganizeCandidateRow, SmartOrganizeEndpointRow,
};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};
pub(crate) const SMART_ORGANIZE_BATCH_LIMIT: usize = 50;
const SMART_ORGANIZE_PLAN_TTL: Duration = Duration::from_secs(15 * 60);
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MoveWitness {
    Day {
        key: String,
        event_at_ms: i64,
        offset_seconds: i32,
    },
    Relation {
        endpoint_kind: String,
        endpoint_id: String,
    },
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AuthorizedMove {
    pub item_id: String,
    pub kind: SmartOrganizeKind,
    pub source_container_id: Option<String>,
    pub bucket_id: String,
    pub name_hash: [u8; 32],
    pub existing_target_id: Option<String>,
    pub witness: MoveWitness,
    pub bundle: Option<BundleWitness>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BundleWitness {
    providers: Vec<(String, Option<String>)>,
    companion: Option<(String, String)>,
}
fn bundle_witness(state: &AppState, id: &str) -> Result<BundleWitness, AppError> {
    let companion = state
        .db
        .companion_note_for_meeting(id)?
        .map(|id| {
            let (owner, _, _) = state
                .db
                .note_gate_anchor(&id)?
                .ok_or_else(|| stale("The companion disappeared"))?;
            Ok::<_, AppError>((id, owner))
        })
        .transpose()?;
    Ok(BundleWitness {
        providers: state.db.filing_note_source_domains(id)?,
        companion,
    })
}
#[derive(Debug, Clone)]
pub(crate) struct StoredPlan {
    pub plan_id: String,
    pub created_at: Instant,
    pub visibility: ContentVisibilitySnapshot,
    pub destination_parent_id: String,
    pub source_container_id: Option<String>,
    pub include_descendants: bool,
    pub scope_container_ids: BTreeSet<String>,
    pub relation_candidate_ids: Vec<String>,
    pub relation_owners: BTreeMap<String, Option<String>>,
    pub moves: Vec<AuthorizedMove>,
}
#[derive(Default)]
pub struct SmartOrganizePlanRegistry {
    slot: Mutex<Option<StoredPlan>>,
}
impl SmartOrganizePlanRegistry {
    pub fn new() -> Self {
        Self::default()
    }
    fn guard(&self) -> std::sync::MutexGuard<'_, Option<StoredPlan>> {
        self.slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    pub(crate) fn insert(&self, plan: StoredPlan) {
        *self.guard() = Some(plan);
    }
    pub(crate) fn take_selected_at(
        &self,
        request: &SmartOrganizeApplyRequest,
        now: Instant,
    ) -> Result<(StoredPlan, Vec<AuthorizedMove>), AppError> {
        let mut slot = self.guard();
        let plan = slot
            .as_ref()
            .filter(|plan| plan.plan_id == request.plan_id)
            .ok_or_else(|| stale("This preview is no longer current"))?;
        if now.saturating_duration_since(plan.created_at) >= SMART_ORGANIZE_PLAN_TTL {
            *slot = None;
            return Err(stale("This preview expired"));
        }
        let selected = select_reviewed_moves(plan, &request.selected_item_ids)?;
        let plan = slot
            .take()
            .ok_or_else(|| stale("This preview is no longer current"))?;
        Ok((plan, selected))
    }
    pub(crate) fn discard(&self, id: &str) {
        let mut slot = self.guard();
        if slot.as_ref().is_some_and(|plan| plan.plan_id == id) {
            *slot = None;
        }
    }
}
fn stale(message: &str) -> AppError {
    AppError::InvalidArg(format!("{message}; preview again"))
}
fn name_hash(name: &str) -> [u8; 32] {
    Sha256::digest(name.as_bytes()).into()
}
/// The local calendar day of a UTC instant in a FIXED offset, as `YYYY-MM-DD`.
///
/// The offset is a parameter rather than a call to `chrono::Local` so the plan can freeze it: a
/// DST change or a travelling laptop between review and Apply must not re-bucket anything.
pub(crate) fn day_bucket_key(event_at_ms: i64, offset_seconds: i32) -> Result<String, AppError> {
    let offset = chrono::FixedOffset::east_opt(offset_seconds)
        .ok_or_else(|| AppError::InvalidArg("invalid local time offset".into()))?;
    let utc = chrono::DateTime::from_timestamp_millis(event_at_ms)
        .ok_or_else(|| AppError::InvalidArg("the item has an unreadable timestamp".into()))?;
    Ok(utc.with_timezone(&offset).format("%Y-%m-%d").to_string())
}

/// Render a fixed offset the way the preview shows it, e.g. `+02:00`.
pub(crate) fn render_offset(offset_seconds: i32) -> String {
    let sign = if offset_seconds < 0 { '-' } else { '+' };
    let total = offset_seconds.abs();
    format!("{sign}{:02}:{:02}", total / 3600, (total % 3600) / 60)
}

/// The grouping outcome for one candidate before any destination is touched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Grouping {
    Bucket { name: String, witness: MoveWitness },
    Skip(&'static str),
}

/// `byRelation`: group recordings that share exactly ONE eligible direct endpoint.
///
/// Deliberately NOT connected components: transitivity would sweep unrelated recordings into one
/// folder through a chain of intermediaries. A recording with two qualifying anchors is
/// `ambiguousRelation` and moves nowhere — an arbitrary tie-break would file someone's recording
/// under a relation they never chose. A group of one is `lonelyRelation`: a folder holding a single
/// recording is not aggregation.
pub(crate) fn group_by_relation(
    candidate_ids: &[String],
    endpoints: &[SmartOrganizeEndpointRow],
    endpoint_label: &dyn Fn(&str, &str) -> Option<String>,
) -> BTreeMap<String, Grouping> {
    let in_batch: HashSet<&str> = candidate_ids.iter().map(String::as_str).collect();
    let mut by_item: BTreeMap<&str, BTreeSet<(String, String)>> = BTreeMap::new();
    for row in endpoints {
        if !in_batch.contains(row.meeting_id.as_str()) {
            continue;
        }
        by_item
            .entry(row.meeting_id.as_str())
            .or_default()
            .insert((row.endpoint_kind.clone(), row.endpoint_id.clone()));
    }

    // Only shared endpoints qualify; an unrelated singleton link cannot make a group ambiguous.
    let mut counts = BTreeMap::new();
    for anchors in by_item.values() {
        for anchor in anchors {
            *counts.entry(anchor.clone()).or_insert(0usize) += 1;
        }
    }
    for anchors in by_item.values_mut() {
        anchors.retain(|anchor| counts.get(anchor).copied().unwrap_or(0) >= 2);
    }
    let mut result: BTreeMap<String, Grouping> = BTreeMap::new();
    let mut unique_anchor: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    for id in candidate_ids {
        match by_item.get(id.as_str()) {
            None => {
                result.insert(id.clone(), Grouping::Skip("noRelation"));
            }
            Some(anchors) if anchors.is_empty() => {
                result.insert(id.clone(), Grouping::Skip("noRelation"));
            }
            Some(anchors) if anchors.len() > 1 => {
                result.insert(id.clone(), Grouping::Skip("ambiguousRelation"));
            }
            Some(anchors) => {
                if let Some(anchor) = anchors.iter().next().cloned() {
                    unique_anchor.entry(anchor).or_default().push(id.clone());
                }
            }
        }
    }

    // A bucket needs at least two in-batch recordings, and its name must be unambiguous. Two
    // different anchors whose labels case-fold together would merge unrelated groups, so both are
    // skipped rather than merged or suffixed with an unstable counter.
    let mut named: Vec<((String, String), String, Vec<String>)> = Vec::new();
    for (anchor, mut items) in unique_anchor {
        items.sort();
        if items.len() < 2 {
            for id in items {
                result.insert(id, Grouping::Skip("lonelyRelation"));
            }
            continue;
        }
        let Some(label) = endpoint_label(&anchor.0, &anchor.1)
            .and_then(|label| crate::summarize::organize::sanitize_folder(&label))
        else {
            for id in items {
                result.insert(id, Grouping::Skip("noRelation"));
            }
            continue;
        };
        named.push((anchor, label, items));
    }
    // There are at most 25 viable anchors in a 50-recording batch; pairwise comparison keeps
    // canonical equivalence exact without retaining or inventing a normalized label as identity.
    let collisions = named
        .iter()
        .enumerate()
        .map(|(index, (_, label, _))| {
            named.iter().enumerate().any(|(other, (_, candidate, _))| {
                index != other && smart_organize_names_equal(label, candidate)
            })
        })
        .collect::<Vec<_>>();
    for ((anchor, label, items), collides) in named.into_iter().zip(collisions) {
        for id in items {
            if collides {
                result.insert(id, Grouping::Skip("nameCollision"));
            } else {
                result.insert(
                    id,
                    Grouping::Bucket {
                        name: label.clone(),
                        witness: MoveWitness::Relation {
                            endpoint_kind: anchor.0.clone(),
                            endpoint_id: anchor.1.clone(),
                        },
                    },
                );
            }
        }
    }
    result
}

// ------------------------------------------------------------------------------------------- //
// Scope resolution                                                                             //
// ------------------------------------------------------------------------------------------- //

/// The reviewed scope: the anchor plus, optionally, its open descendants. A locked container and
/// everything under it is EXCLUDED, never merely hidden: Smart organize is raw-open-only, so no
/// plan of its can de-seal content.
pub(crate) fn resolve_scope(
    db: &crate::storage::db::Db,
    container_id: &str,
    include_descendants: bool,
) -> Result<BTreeSet<String>, AppError> {
    ensure_open_user_container_chain(db, container_id)?;
    let mut scope = BTreeSet::new();
    scope.insert(container_id.to_string());
    if !include_descendants {
        return Ok(scope);
    }
    let containers = db.list_containers()?;
    let mut children: HashMap<&str, Vec<&crate::storage::models::ContainerRow>> = HashMap::new();
    for row in &containers {
        if let Some(parent) = row.parent_id.as_deref() {
            children.entry(parent).or_default().push(row);
        }
    }
    let mut frontier = vec![container_id.to_string()];
    let mut seen = HashSet::new();
    while let Some(current) = frontier.pop() {
        // Cycle protection: a corrupted parent chain must not spin here.
        if !seen.insert(current.clone()) {
            continue;
        }
        for child in children.get(current.as_str()).into_iter().flatten() {
            if ensure_open_user_container_chain(db, &child.id).is_err() {
                continue;
            }
            if scope.insert(child.id.clone()) {
                frontier.push(child.id.clone());
            }
        }
    }
    Ok(scope)
}

fn breadcrumb(state: &AppState, id: Option<&str>) -> Result<String, AppError> {
    let Some(id) = id else {
        return Ok("Not classified".into());
    };
    ensure_open_user_container_chain(&state.db, id)?;
    let containers = state.db.list_containers()?;
    let mut cursor = Some(id);
    let mut names = Vec::new();
    let mut seen = HashSet::new();
    while let Some(id) = cursor {
        if !seen.insert(id) {
            return Err(stale("The hierarchy changed"));
        }
        let row = containers
            .iter()
            .find(|row| row.id == id)
            .ok_or_else(|| stale("The folder disappeared"))?;
        names.push(row.name.clone());
        cursor = row.parent_id.as_deref();
    }
    names.reverse();
    Ok(names.join(" / "))
}
fn endpoint_label(state: &AppState, kind: &str, id: &str) -> Result<String, AppError> {
    if kind == "container" {
        ensure_open_user_container_chain(&state.db, id)?;
        return state
            .db
            .list_containers()?
            .into_iter()
            .find(|row| row.id == id)
            .map(|row| row.name)
            .ok_or_else(|| stale("The relation folder disappeared"));
    }
    let owner = state
        .db
        .smart_organize_document_owner(id, kind)?
        .ok_or_else(|| stale("The relation endpoint disappeared"))?;
    ensure_open_user_container_chain(&state.db, &owner)?;
    state
        .db
        .document_title(id)?
        .ok_or_else(|| stale("The relation endpoint disappeared"))
}
fn relation_groups(
    state: &AppState,
    ids: &[String],
    source: Option<&str>,
    destination: &str,
) -> Result<BTreeMap<String, Grouping>, AppError> {
    let mut endpoints = state.db.smart_organize_relation_endpoints(ids)?;
    endpoints.retain(|row| {
        !(row.endpoint_kind == "container"
            && (Some(row.endpoint_id.as_str()) == source || row.endpoint_id == destination))
    });
    // Gate endpoint metadata before naming it; locked endpoints never become evidence or labels.
    endpoints.retain(|row| endpoint_label(state, &row.endpoint_kind, &row.endpoint_id).is_ok());
    Ok(group_by_relation(ids, &endpoints, &|kind, id| {
        endpoint_label(state, kind, id).ok()
    }))
}
struct CandidateBatch {
    rows: Vec<(SmartOrganizeKind, SmartOrganizeCandidateRow)>,
    total: u32,
    already_there: u32,
}
fn collect_candidates(
    state: &AppState,
    scope: &[String],
    request: &SmartOrganizeRequest,
    offset_for_event: &dyn Fn(i64) -> Result<i32, AppError>,
) -> Result<CandidateBatch, AppError> {
    let mut rows = Vec::new();
    let mut total = 0;
    let mut already = 0;
    for kind in &request.kinds {
        let mut offset = request.page_offset as usize;
        let mut retained = 0;
        loop {
            let (page, count) = match kind {
                SmartOrganizeKind::Meeting => state.db.smart_organize_meeting_candidates(
                    scope,
                    SMART_ORGANIZE_BATCH_LIMIT,
                    offset,
                )?,
                SmartOrganizeKind::Note => state.db.smart_organize_note_candidates(
                    scope,
                    SMART_ORGANIZE_BATCH_LIMIT,
                    offset,
                )?,
            };
            if offset == request.page_offset as usize {
                total += count;
            }
            let page_len = page.len();
            for row in page {
                // A later review must reach deferred items even when previous successes remain
                // descendants of the scope. Read metadata in pages; retain at most 50 per kind.
                let is_already = if request.rule == SmartOrganizeRule::ByDay {
                    if let (Some(owner), Ok(seconds)) = (
                        row.container_id.as_deref(),
                        offset_for_event(row.event_at_ms),
                    ) {
                        if let Ok(key) = day_bucket_key(row.event_at_ms, seconds) {
                            let targets = state.db.smart_organize_child_folders_case_folded(
                                &request.destination_parent_id,
                                &key,
                            )?;
                            targets.len() == 1
                                && targets[0].0 == owner
                                && item_readiness(state, *kind, &row.id).is_ok()
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                } else {
                    false
                };
                offset += 1;
                if is_already {
                    already += 1;
                    continue;
                }
                rows.push((*kind, row));
                retained += 1;
                if retained == SMART_ORGANIZE_BATCH_LIMIT {
                    break;
                }
            }
            if retained == SMART_ORGANIZE_BATCH_LIMIT || page_len < SMART_ORGANIZE_BATCH_LIMIT {
                break;
            }
        }
    }
    rows.sort_by(|a, b| {
        a.1.event_at_ms
            .cmp(&b.1.event_at_ms)
            .then(a.0.cmp(&b.0))
            .then(a.1.id.cmp(&b.1.id))
    });
    rows.truncate(SMART_ORGANIZE_BATCH_LIMIT);
    Ok(CandidateBatch {
        rows,
        total,
        already_there: already,
    })
}
fn item_readiness(state: &AppState, kind: SmartOrganizeKind, id: &str) -> Result<(), AppError> {
    if kind == SmartOrganizeKind::Meeting {
        ensure_raw_open_recording_source(state, id)?;
        ensure_no_active_salvage_for_meeting(state, id)?;
        if let Some(companion) = state.db.companion_note_for_meeting(id)? {
            let (owner, _, _) = state
                .db
                .note_gate_anchor(&companion)?
                .ok_or_else(|| stale("The companion disappeared"))?;
            ensure_raw_open_companion_source(state, &owner)?;
        }
        let meeting = state
            .db
            .get_meeting(id)?
            .ok_or_else(|| stale("The recording disappeared"))?;
        if !matches!(
            meeting.status,
            MeetingStatus::Transcribed
                | MeetingStatus::Summarized
                | MeetingStatus::Exported
                | MeetingStatus::Error
        ) || state.db.meeting_has_recording_recovery_ownership(id)?
        {
            return Err(AppError::Unavailable(
                "The recording must finish processing or recovery first".into(),
            ));
        }
    } else if state.db.authored_note_is_companion(id)? {
        return Err(stale("Companion notes follow their recording"));
    }
    Ok(())
}
fn skip(
    row: &SmartOrganizeCandidateRow,
    code: SmartOrganizeSkipCode,
    reason: &str,
) -> SmartOrganizeSkip {
    SmartOrganizeSkip {
        item_id: Some(row.id.clone()),
        title: row.title.clone(),
        code,
        reason: reason.into(),
    }
}
/// Caller holds lifecycle throughout all metadata reads. Offset is calculated for EACH event,
/// not for today: winter and summer recordings may belong to different local offsets.
pub(crate) fn build_plan(
    state: &AppState,
    request: &SmartOrganizeRequest,
    offset: &dyn Fn(i64) -> Result<i32, AppError>,
    visibility: ContentVisibilitySnapshot,
) -> Result<(StoredPlan, SmartOrganizePreview), AppError> {
    if request.kinds.is_empty()
        || request.kinds.len() > 2
        || request.kinds.iter().collect::<HashSet<_>>().len() != request.kinds.len()
    {
        return Err(stale("Choose the item kinds"));
    }
    if request.rule == SmartOrganizeRule::ByDay && request.page_offset != 0 {
        return Err(stale(
            "Day batches automatically reach the next unfiled items",
        ));
    }
    ensure_open_user_container_chain(&state.db, &request.destination_parent_id)?;
    if request
        .source_container_id
        .as_ref()
        .is_some_and(|id| id != &request.destination_parent_id)
    {
        return Err(stale(
            "Buckets must be direct children of the selected place",
        ));
    }
    if (request.source_container_id.is_none() || request.rule == SmartOrganizeRule::ByRelation)
        && request.kinds != [SmartOrganizeKind::Meeting]
    {
        return Err(stale("This scope or rule organizes recordings only"));
    }
    let scope = match request.source_container_id.as_deref() {
        Some(id) => resolve_scope(&state.db, id, request.include_descendants)?,
        None => BTreeSet::new(),
    };
    let CandidateBatch {
        rows: candidates,
        total,
        already_there: previously_filed,
    } = collect_candidates(
        state,
        &scope.iter().cloned().collect::<Vec<_>>(),
        request,
        offset,
    )?;
    let relation_ids = candidates
        .iter()
        .filter(|(kind, row)| {
            *kind == SmartOrganizeKind::Meeting && item_readiness(state, *kind, &row.id).is_ok()
        })
        .map(|(_, row)| row.id.clone())
        .collect::<Vec<_>>();
    let groups = if request.rule == SmartOrganizeRule::ByRelation {
        relation_groups(
            state,
            &relation_ids,
            request.source_container_id.as_deref(),
            &request.destination_parent_id,
        )?
    } else {
        candidates
            .iter()
            .map(|(_, row)| {
                let grouped = offset(row.event_at_ms).and_then(|seconds| {
                    day_bucket_key(row.event_at_ms, seconds).map(|key| Grouping::Bucket {
                        name: key.clone(),
                        witness: MoveWitness::Day {
                            key,
                            event_at_ms: row.event_at_ms,
                            offset_seconds: seconds,
                        },
                    })
                });
                (row.id.clone(), grouped.unwrap_or(Grouping::Skip("noDate")))
            })
            .collect()
    };
    let mut buckets = BTreeMap::<String, SmartOrganizeBucket>::new();
    let mut moves = Vec::new();
    let mut skipped = Vec::new();
    let mut already_there = previously_filed;
    let parent_breadcrumb = breadcrumb(state, Some(&request.destination_parent_id))?;
    for (kind, original_row) in &candidates {
        let mut row = original_row.clone();
        if let Err(error) = item_readiness(state, *kind, &row.id) {
            let code = if matches!(error, AppError::Locked(_)) {
                SmartOrganizeSkipCode::Locked
            } else {
                SmartOrganizeSkipCode::NotReady
            };
            // Protected-domain failures have no title-bearing preview row.
            let mut skipped_row = skip(&row, code, "The item is unavailable for filing");
            if code == SmartOrganizeSkipCode::Locked {
                skipped_row.title = None;
                skipped_row.item_id = None;
            }
            skipped.push(skipped_row);
            continue;
        }
        if *kind == SmartOrganizeKind::Meeting {
            row.title = state
                .db
                .get_meeting(&row.id)?
                .and_then(|meeting| meeting.title);
        }
        let Some(Grouping::Bucket { name, witness }) = groups.get(&row.id) else {
            let reason = match groups.get(&row.id) {
                Some(Grouping::Skip(reason)) => *reason,
                _ => "noRelation",
            };
            let code = match reason {
                "ambiguousRelation" => SmartOrganizeSkipCode::AmbiguousRelation,
                "nameCollision" => SmartOrganizeSkipCode::NameCollision,
                "noDate" => SmartOrganizeSkipCode::NoDate,
                _ => SmartOrganizeSkipCode::NoMatch,
            };
            skipped.push(skip(
                &row,
                code,
                match code {
                    SmartOrganizeSkipCode::AmbiguousRelation => {
                        "More than one shared relation; left in place"
                    }
                    SmartOrganizeSkipCode::NameCollision => {
                        "Different relations produce the same folder name"
                    }
                    SmartOrganizeSkipCode::NoDate => "The item has no readable date",
                    _ => "No unambiguous relation shared by at least two recordings",
                },
            ));
            continue;
        };
        let existing = state
            .db
            .smart_organize_child_folders_case_folded(&request.destination_parent_id, name)?;
        if existing.len() > 1 {
            skipped.push(skip(
                &row,
                SmartOrganizeSkipCode::NameCollision,
                "Multiple folders have this name",
            ));
            continue;
        }
        if let Some((id, _)) = existing.first() {
            if ensure_open_user_container_chain(&state.db, id).is_err() {
                skipped.push(skip(
                    &row,
                    SmartOrganizeSkipCode::TargetUnavailable,
                    "The destination is unavailable",
                ));
                continue;
            }
            if row.container_id.as_deref() == Some(id) {
                already_there += 1;
                continue;
            }
        }
        let bucket_id = match witness {
            MoveWitness::Day { key, .. } => format!("day:{key}"),
            MoveWitness::Relation {
                endpoint_kind,
                endpoint_id,
            } => format!("{endpoint_kind}:{endpoint_id}"),
        };
        let display = existing
            .first()
            .map(|(_, name)| name.clone())
            .unwrap_or_else(|| name.clone());
        let reason=match witness {MoveWitness::Day{key,offset_seconds,..}=>format!("{} on {key} (UTC{})",if *kind==SmartOrganizeKind::Note {"Created"}else{"Started"},render_offset(*offset_seconds)),MoveWitness::Relation{..}=>format!("Direct manual, wikilink or accepted relation to {name}, shared with another recording")};
        buckets
            .entry(bucket_id.clone())
            .or_insert_with(|| SmartOrganizeBucket {
                bucket_id: bucket_id.clone(),
                folder_name: display.clone(),
                destination_breadcrumb: format!("{parent_breadcrumb} / {display}"),
                status: if existing.is_empty() {
                    SmartOrganizeBucketStatus::New
                } else {
                    SmartOrganizeBucketStatus::Existing
                },
                items: Vec::new(),
            })
            .items
            .push(SmartOrganizeMove {
                item_id: row.id.clone(),
                kind: *kind,
                title: row.title.clone().unwrap_or_else(|| "Untitled".into()),
                from_container_id: row.container_id.clone(),
                from_breadcrumb: breadcrumb(state, row.container_id.as_deref())?,
                reason,
            });
        moves.push(AuthorizedMove {
            item_id: row.id.clone(),
            kind: *kind,
            source_container_id: row.container_id.clone(),
            bucket_id,
            name_hash: name_hash(name),
            existing_target_id: existing.first().map(|(id, _)| id.clone()),
            witness: witness.clone(),
            bundle: if *kind == SmartOrganizeKind::Meeting {
                Some(bundle_witness(state, &row.id)?)
            } else {
                None
            },
        });
    }
    let plan_id = uuid::Uuid::new_v4().to_string();
    let buckets = buckets.into_values().collect::<Vec<_>>();
    let preview = SmartOrganizePreview {
        plan_id: plan_id.clone(),
        total_scanned: total,
        already_there,
        deferred: total.saturating_sub(
            request
                .page_offset
                .saturating_add(candidates.len() as u32 + previously_filed),
        ),
        next_page_offset: if request.rule == SmartOrganizeRule::ByRelation
            && !candidates.is_empty()
            && request.page_offset.saturating_add(candidates.len() as u32) < total
        {
            Some(request.page_offset.saturating_add(candidates.len() as u32))
        } else {
            None
        },
        new_folders: buckets
            .iter()
            .filter(|b| b.status == SmartOrganizeBucketStatus::New)
            .count() as u32,
        reused_folders: buckets
            .iter()
            .filter(|b| b.status == SmartOrganizeBucketStatus::Existing)
            .count() as u32,
        timezone_label: "This Mac's local calendar; each item's UTC offset is frozen below".into(),
        buckets,
        skipped,
    };
    Ok((
        StoredPlan {
            plan_id,
            created_at: Instant::now(),
            visibility,
            destination_parent_id: request.destination_parent_id.clone(),
            source_container_id: request.source_container_id.clone(),
            include_descendants: request.include_descendants,
            scope_container_ids: scope,
            relation_owners: candidates
                .iter()
                .filter(|(_, row)| relation_ids.contains(&row.id))
                .map(|(_, row)| (row.id.clone(), row.container_id.clone()))
                .collect(),
            relation_candidate_ids: relation_ids,
            moves,
        },
        preview,
    ))
}
#[tauri::command]
pub async fn plan_smart_organize(
    state: State<'_, AppState>,
    registry: State<'_, SmartOrganizePlanRegistry>,
    request: SmartOrganizeRequest,
) -> Result<SmartOrganizePreview, AppError> {
    let _lifecycle = lifecycle_guard(state.inner());
    let visibility = capture_content_visibility_snapshot_under_lifecycle(state.inner());
    let (plan, preview) = build_plan(
        state.inner(),
        &request,
        &|millis| {
            let instant = chrono::DateTime::from_timestamp_millis(millis)
                .ok_or_else(|| stale("Unreadable date"))?;
            Ok(instant
                .with_timezone(&chrono::Local)
                .offset()
                .local_minus_utc())
        },
        visibility,
    )?;
    require_current_content_visibility_snapshot_under_lifecycle(state.inner(), visibility)?;
    registry.insert(plan);
    Ok(preview)
}
#[tauri::command]
pub async fn discard_smart_organize_plan(
    registry: State<'_, SmartOrganizePlanRegistry>,
    request: SmartOrganizeDiscardRequest,
) -> Result<(), AppError> {
    registry.discard(&request.plan_id);
    Ok(())
}

pub(crate) fn select_reviewed_moves(
    plan: &StoredPlan,
    ids: &[String],
) -> Result<Vec<AuthorizedMove>, AppError> {
    if ids.len() > SMART_ORGANIZE_BATCH_LIMIT {
        return Err(stale("Too many selected items"));
    }
    let mut seen = HashSet::new();
    for id in ids {
        if !seen.insert(id) || !plan.moves.iter().any(|row| row.item_id == *id) {
            return Err(stale("Selection is not a unique subset of this preview"));
        }
    }
    Ok(plan
        .moves
        .iter()
        .filter(|row| seen.contains(&row.item_id))
        .cloned()
        .collect())
}
fn current_owner(state: &AppState, item: &AuthorizedMove) -> Result<Option<String>, AppError> {
    match item.kind {
        SmartOrganizeKind::Note => state
            .db
            .note_gate_anchor(&item.item_id)?
            .map(|(owner, _, _)| Some(owner))
            .ok_or_else(|| stale("The note disappeared")),
        SmartOrganizeKind::Meeting => {
            if state.db.meeting_started_at_ms(&item.item_id)?.is_none() {
                return Err(stale("The recording disappeared"));
            }
            state.db.smart_organize_meeting_owner(&item.item_id)
        }
    }
}
fn validate_source(
    state: &AppState,
    plan: &StoredPlan,
    item: &AuthorizedMove,
) -> Result<(), AppError> {
    require_current_content_visibility_snapshot_under_lifecycle(state, plan.visibility)?;
    if current_owner(state, item)? != item.source_container_id {
        return Err(stale("The item left its reviewed source"));
    }
    match item.source_container_id.as_deref() {
        Some(owner) => {
            ensure_open_user_container_chain(&state.db, owner)?;
            let root = plan
                .source_container_id
                .as_deref()
                .ok_or_else(|| stale("The scope changed"))?;
            if !plan.scope_container_ids.contains(owner)
                || !resolve_scope(&state.db, root, plan.include_descendants)?.contains(owner)
            {
                return Err(stale("The item left the reviewed scope"));
            }
        }
        None => {
            if plan.source_container_id.is_some() || item.kind != SmartOrganizeKind::Meeting {
                return Err(stale("The source changed"));
            }
        }
    }
    item_readiness(state, item.kind, &item.item_id)?;
    if let Some(expected) = &item.bundle {
        if bundle_witness(state, &item.item_id)? != *expected {
            return Err(stale("The recording bundle changed after review"));
        }
    }
    Ok(())
}
fn validate_witness(
    state: &AppState,
    plan: &StoredPlan,
    item: &AuthorizedMove,
    completed: &HashMap<String, String>,
) -> Result<String, AppError> {
    let name = match &item.witness {
        MoveWitness::Day {
            key,
            event_at_ms,
            offset_seconds,
        } => {
            let current = match item.kind {
                SmartOrganizeKind::Note => state.db.document_created_at_ms(&item.item_id)?,
                SmartOrganizeKind::Meeting => state.db.meeting_started_at_ms(&item.item_id)?,
            };
            if current != Some(*event_at_ms)
                || day_bucket_key(*event_at_ms, *offset_seconds)? != *key
            {
                return Err(stale("The reviewed date changed"));
            }
            key.clone()
        }
        MoveWitness::Relation { .. } => {
            let current_scope = plan
                .source_container_id
                .as_deref()
                .map(|root| resolve_scope(&state.db, root, plan.include_descendants))
                .transpose()?;
            let peer_ids = plan
                .relation_candidate_ids
                .iter()
                .filter(|id| {
                    if item_readiness(state, SmartOrganizeKind::Meeting, id).is_err() {
                        return false;
                    }
                    let Ok(owner) = state.db.smart_organize_meeting_owner(id) else {
                        return false;
                    };
                    if let Some(target) = completed.get(*id) {
                        return owner.as_deref() == Some(target);
                    }
                    if plan.relation_owners.get(*id) != Some(&owner) {
                        return false;
                    }
                    match owner.as_deref() {
                        Some(owner) => current_scope
                            .as_ref()
                            .is_some_and(|scope| scope.contains(owner)),
                        None => plan.source_container_id.is_none(),
                    }
                })
                .cloned()
                .collect::<Vec<_>>();
            let groups = relation_groups(
                state,
                &peer_ids,
                plan.source_container_id.as_deref(),
                &plan.destination_parent_id,
            )?;
            match groups.get(&item.item_id) {
                Some(Grouping::Bucket { name, witness }) if witness == &item.witness => {
                    name.clone()
                }
                _ => return Err(stale("The reviewed relation is no longer eligible")),
            }
        }
    };
    if name_hash(&name) != item.name_hash {
        return Err(stale("The relation name changed"));
    }
    Ok(name)
}
fn resolve_bucket(
    state: &AppState,
    lifecycle: &std::sync::MutexGuard<'_, ()>,
    plan: &StoredPlan,
    item: &AuthorizedMove,
    name: &str,
    created: &mut HashMap<String, String>,
) -> Result<(String, String, SmartOrganizeBucketStatus), AppError> {
    ensure_open_user_container_chain(&state.db, &plan.destination_parent_id)?;
    let matching = state
        .db
        .smart_organize_child_folders_case_folded(&plan.destination_parent_id, name)?;
    let expected = item
        .existing_target_id
        .as_ref()
        .or_else(|| created.get(&item.bucket_id));
    if let Some(expected) = expected {
        if matching.len() != 1 || matching[0].0 != *expected {
            return Err(stale(
                "The reviewed destination was renamed, moved or replaced",
            ));
        }
        ensure_open_user_container_chain(&state.db, expected)?;
        return Ok((
            expected.clone(),
            matching[0].1.clone(),
            if item.existing_target_id.is_some() {
                SmartOrganizeBucketStatus::Existing
            } else {
                SmartOrganizeBucketStatus::New
            },
        ));
    }
    // A same-name folder created by somebody else after review is a DIFFERENT target.
    if !matching.is_empty() {
        return Err(stale("A destination appeared after review"));
    }
    let folder = create_note_folder_under_lifecycle(
        state,
        lifecycle,
        name,
        Some(&plan.destination_parent_id),
    )?;
    ensure_open_user_container_chain(&state.db, &folder.id)?;
    created.insert(item.bucket_id.clone(), folder.id.clone());
    Ok((folder.id, name.to_string(), SmartOrganizeBucketStatus::New))
}
pub(crate) fn apply_plan_inner_with_hook<F: FnMut(&str)>(
    state: &AppState,
    plan: StoredPlan,
    selected: Vec<AuthorizedMove>,
    mut hook: F,
) -> Result<SmartOrganizeReceipt, AppError> {
    // Even internal callers cannot substitute a target or witness in a selected row.
    if selected.len() > SMART_ORGANIZE_BATCH_LIMIT
        || selected.iter().any(|item| !plan.moves.contains(item))
        || selected
            .iter()
            .map(|item| &item.item_id)
            .collect::<HashSet<_>>()
            .len()
            != selected.len()
    {
        return Err(stale("Moves differ from the approved plan"));
    }
    let mut applied = Vec::new();
    let mut failures = Vec::new();
    let mut created = HashMap::new();
    let mut completed = HashMap::new();
    for item in selected {
        let admission = {
            let _lifecycle = lifecycle_guard(state);
            validate_source(state, &plan, &item)
                .and_then(|()| validate_witness(state, &plan, &item, &completed).map(|_| ()))
        };
        if admission.is_ok() {
            hook(&item.item_id);
        }
        let result = admission.and_then(|()| {
            let lifecycle = lifecycle_guard(state);
            validate_source(state, &plan, &item)?;
            let name = validate_witness(state, &plan, &item, &completed)?;
            let from_breadcrumb = breadcrumb(state, item.source_container_id.as_deref())?;
            let title = match item.kind {
                SmartOrganizeKind::Note => state.db.document_title(&item.item_id)?,
                SmartOrganizeKind::Meeting => state
                    .db
                    .get_meeting(&item.item_id)?
                    .and_then(|meeting| meeting.title),
            }
            .unwrap_or_else(|| "Untitled".into());
            let (target, folder_name, bucket_status) =
                resolve_bucket(state, &lifecycle, &plan, &item, &name, &mut created)?;
            match item.kind {
                SmartOrganizeKind::Note => {
                    move_note_doc_under_lifecycle(state, &lifecycle, &item.item_id, &target)?
                }
                SmartOrganizeKind::Meeting => move_note_under_lifecycle_with_staging(
                    state,
                    &lifecycle,
                    item.item_id.clone(),
                    Some(target.clone()),
                    |_| Ok(()),
                    |db, move_| db.move_open_recording_bundle(move_),
                )?,
            }
            if current_owner(state, &item)?.as_deref() != Some(&target) {
                return Err(stale("The move could not be confirmed"));
            }
            Ok(SmartOrganizeApplied {
                item_id: item.item_id.clone(),
                kind: item.kind,
                title,
                from_container_id: item.source_container_id.clone(),
                to_container_id: target.clone(),
                from_breadcrumb,
                to_breadcrumb: breadcrumb(state, Some(&target))?,
                bucket_id: item.bucket_id.clone(),
                folder_name,
                bucket_status,
            })
        });
        match result {
            Ok(row) => {
                completed.insert(row.item_id.clone(), row.to_container_id.clone());
                applied.push(row)
            }
            Err(error) => failures.push(SmartOrganizeFailure {
                item_id: item.item_id,
                title: None,
                reason: match error {
                    AppError::Locked(_) => "Content visibility changed; preview again",
                    AppError::Unavailable(_) => {
                        "The item is temporarily unavailable; preview again"
                    }
                    _ => "The reviewed source, rule or destination changed; preview again",
                }
                .into(),
                retryable: true,
            }),
        }
    }
    let _lifecycle = lifecycle_guard(state);
    require_current_content_visibility_snapshot_under_lifecycle(state, plan.visibility)?;
    Ok(SmartOrganizeReceipt { applied, failures })
}
#[tauri::command]
pub async fn apply_smart_organize_plan(
    state: State<'_, AppState>,
    registry: State<'_, SmartOrganizePlanRegistry>,
    request: SmartOrganizeApplyRequest,
) -> Result<SmartOrganizeReceipt, AppError> {
    let (plan, selected) = registry.take_selected_at(&request, Instant::now())?;
    let _mutation = state.lock_org_mutation().await;
    apply_plan_inner_with_hook(state.inner(), plan, selected, |_| {})
}
#[cfg(test)]
#[path = "tests/smart_organize_tests.rs"]
mod tests;
