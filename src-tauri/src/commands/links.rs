//! LINKS command surface — the note↔meeting↔document link-engine commands, extracted VERBATIM from
//! `commands` (God-file split, a PURE MOVE — every read-gate / endpoint-gate / scoped-lifecycle-guard
//! body is UNCHANGED, only relocated). This is the link domain: `link_related_notes` (the manual
//! Stage-2 re-link trigger), `link_meeting_entities` (entity extraction + graph persist),
//! `list_links`, `accept_link`, `dismiss_link`, `link_items`, `unlink_items`, `resolve_wikilink`,
//! `list_link_candidates`, plus the link-only helpers (`parse_link_kind`,
//! `link_endpoint_is_unlocked`, `prepare_manual_marker_seals`).
//!
//! LOCK-MODEL (byte-identical to the pre-move form): every READ gates — `list_links` returns edges
//! only when BOTH endpoints are session-VISIBLE (`Db::links_for_visible`, no existence leak),
//! `resolve_wikilink`/`list_link_candidates` route through the `*_visible` readers +
//! `search_org_brain_hits` (a sealed-not-unlocked target stays "not found"). Every WRITE gates BEFORE
//! mutating: `accept_link`/`dismiss_link` refuse anything but a `semantic`+`suggested` edge and gate
//! BOTH endpoints (`link_endpoint_is_unlocked`, fail-closed on a sealed/unknown endpoint —
//! `AppError::Locked`); `link_items`/`unlink_items` gate both endpoints before the row write.
//! `link_items`/`accept_link` are GRAPH-ONLY (they persist/flip the edge and write NO note body) —
//! the machine `murmur:links` block that used to mirror links into a note body was removed (it went
//! stale + rendered as raw junk in the plain-text editor; the live `links` table drives the Related
//! panel). `unlink_items` still strips any PRE-EXISTING manual `[[Title]]` marker, but now prepares
//! that DB scrub + its durable vault-cleanup outbox inside the SAME transaction as the exact edge
//! delete. The lifecycle guard is released only after that transaction commits; filesystem drain is
//! retryable and propagates failures instead of silently leaving a plaintext marker behind.
//! `link_meeting_entities` READ-GATES the meeting's note (`meeting_is_unlocked`) before any
//! extraction/egress.
//!
//! The SHARED write-time index hooks (`index_wikilinks_best_effort`, `auto_link_semantic_best_effort`
//! — called from every note-save funnel), the unlock re-derive (`rederive_links_for_folder` — called
//! from the lock unseal restore), the entity persist (`build_and_persist_entities` — pipeline-called),
//! the `link_related_notes_inner` core (pipeline-called), and every gate/DB helper
//! (`meeting_is_unlocked`, `folder_is_unlocked`, `unlocked_snapshot`, `lifecycle_guard`,
//! `update_note_inner`, `update_note_doc_inner`, the `crate::links`/`crate::enrich` facades) STAY in
//! `commands/mod.rs` (or their crate module) — this module reads them through `use super::*` (a
//! `commands` submodule sees its parent's private items). The moved `*_inner` cores stay `pub(crate)`
//! (unchanged) so the STAYING test modules keep calling them via the `pub use links_commands::*;`
//! re-export. Every symbol keeps its EXACT prior body + signature; nothing changed except its file —
//! no gate/guard/seal LOGIC changed, only relocation.

use super::*;

/// `link_related_notes(meeting_id)` — MANUAL re-link / backfill trigger for the Stage 2 / Lane A
/// pass. The AUTO pipeline runs the same [`link_related_notes_inner`] as a deferred post-`Exported`
/// pass; this command lets the user (or a backfill over old notes) re-run it on demand. Lock-gated +
/// seal-safe (a sealed meeting is a silent no-op) and ZERO egress.
#[tauri::command]
pub fn link_related_notes(state: State<'_, AppState>, meeting_id: String) -> Result<(), AppError> {
    link_related_notes_inner(state.inner(), &meeting_id)
}

/// Resolve the people + projects in a meeting note → persist them to the encrypted DB graph
/// (always) and mirror them as `[[Person]]` / `[[Project]]` vault stubs (only when a vault is
/// configured + the meeting's folder is unsealed). The graph self-assembles. The DB sink works
/// even with no vault set — hence no hard "set a vault folder" error anymore.
#[tauri::command]
pub async fn link_meeting_entities(
    app: AppHandle,
    state: State<'_, AppState>,
    meeting_id: String,
) -> Result<crate::summarize::graph::GraphPayload, AppError> {
    // BLK-2b READ-GATE: a sealed-and-not-unlocked meeting's note is blanked; refuse to extract
    // entities from it (would feed a cloud provider blank text + re-write vault stubs). Fail closed.
    if !meeting_is_unlocked(state.inner(), &meeting_id)? {
        return Err(AppError::Locked(crate::errcode::tag(
                crate::errcode::MEETING_LOCKED,
                "this meeting's folder is locked — unlock it to link entities",
            )));
    }
    let meeting = state
        .db
        .get_meeting(&meeting_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no meeting with id {meeting_id}")))?;
    let note = state
        .db
        .get_latest_note_for_meeting(&meeting_id)?
        .ok_or_else(|| AppError::InvalidArg("this meeting has no note yet".into()))?;
    let title = meeting
        .title
        .clone()
        .unwrap_or_else(|| "Meeting".to_string());
    build_and_persist_entities(&app, &state, &meeting_id, &title, &note.markdown, None).await
}

/// Parse an IPC link-endpoint kind string into [`crate::links::LinkKind`], or a clean `InvalidArg`.
fn parse_link_kind(s: &str) -> Result<crate::links::LinkKind, AppError> {
    crate::links::LinkKind::parse(s).ok_or_else(|| {
        AppError::InvalidArg(format!(
            "unknown link kind {s:?} (expected \"meeting\", \"note\", \"document\", or \"org\")"
        ))
    })
}

/// Brain v3 PR-3 — every persisted link edge incident on `(kind, id)`, BOTH endpoints
/// visibility-gated in [`crate::storage::db::Db::links_for_visible`] (the queried item must be
/// visible or the list is empty — no existence leak — and each edge's neighbour is dropped unless it
/// too is visible). Snapshots the LIVE session unlock set. `kind` is `"meeting" | "note" |
/// "document"`. Dismissed edges are never returned; suggested (semantic) edges are, so the FE can
/// render Accept/Dismiss.
#[tauri::command]
pub fn list_links(
    state: State<'_, AppState>,
    kind: String,
    id: String,
) -> Result<Vec<crate::storage::models::LinkEdge>, AppError> {
    let _lifecycle = lifecycle_guard(state.inner());
    let link_kind = parse_link_kind(&kind)?;
    let unlocked = unlocked_snapshot(state.inner())?;
    state.db.links_for_visible(link_kind, &id, &unlocked)
}

/// Brain v3 PR-3 — ACCEPT a suggested (semantic) link: flip it `status='active'`,
/// `created_by='accepted'`. GRAPH-ONLY — the accept writes NO note body. (It used to also
/// materialize the neighbour's `[[Title]]` into an owned note's managed `murmur:links` block; that
/// block was removed — it went stale + rendered as raw junk in the plain-text editor. The live
/// `links` table drives the Related panel, so the flipped edge alone is the surface.)
///
/// GATED (PR-5): accept is ONLY for an unconfirmed SUGGESTION (`edge_type='semantic'`,
/// `status='suggested'`) — anything else (a deterministic wikilink/companion, an already-active
/// manual/accepted, or a dismissed tombstone) is refused `InvalidArg`. BOTH endpoints must be
/// session-visible (`link_endpoint_is_unlocked` on each) — if EITHER is sealed-and-not-unlocked the
/// accept is refused `AppError::Locked` (never activate an edge behind a lock, never reveal a locked
/// neighbour). Idempotent (re-accepting an already-active edge is a no-op; the DB DO-UPDATE guard
/// never downgrades it).
#[tauri::command]
pub fn accept_link(state: State<'_, AppState>, id: i64) -> Result<(), AppError> {
    accept_link_inner(state.inner(), id)
}

pub(crate) fn accept_link_inner(state: &AppState, id: i64) -> Result<(), AppError> {
    // BLK-1 / TOCTOU: SCOPE the lifecycle guard tightly around validate + gate + row-flip so a
    // concurrent lock/relock cannot land between the endpoint gate and the flip. The accept is
    // GRAPH-ONLY now (no note-body materialize), so nothing after the guard re-takes the
    // non-reentrant `Mutex<()>` — but the tight scope is kept (fail-closed + no wider hold than the
    // check-then-write needs). Lock order `lifecycle ⊃ db`.
    {
        let _lifecycle = lifecycle_guard(state);
        let Some((src_kind, src_id, dst_kind, dst_id, et, status)) = state.db.link_by_id(id)?
        else {
            return Err(AppError::InvalidArg(format!("no link {id}")));
        };
        // ── GATE both endpoints FIRST — before the row is described back to the caller. The type
        //    refusal below names `edge_type` and `status`, so running it first answered "this id is
        //    an active manual edge" for a row whose endpoint is sealed. That mattered little while a
        //    seal purged manual rows outright; now that a manual decision SURVIVES a seal, the ids
        //    outlive the lock and the ordering becomes the difference between a probe that answers
        //    and one that refuses. Fail-closed on a sealed/unknown endpoint. ──
        let (Some(sk), Some(dk)) = (
            crate::links::LinkKind::parse(&src_kind),
            crate::links::LinkKind::parse(&dst_kind),
        ) else {
            return Err(AppError::InvalidArg(format!(
                "link {id} has a corrupt endpoint kind"
            )));
        };
        if !link_endpoint_is_unlocked(state, sk, &src_id)?
            || !link_endpoint_is_unlocked(state, dk, &dst_id)?
        {
            return Err(AppError::Locked(
                "one of these items is locked — unlock it to accept the link".into(),
            ));
        }
        // ── Fix 5 (brain-v3 audit): accept is ONLY for an unconfirmed SUGGESTION. Refuse anything else
        //    (a deterministic wikilink/companion, an already-active manual/accepted, or a dismissed
        //    tombstone) — "accepting" those is meaningless and would let a caller flip arbitrary rows. ──
        if !(et == "semantic" && status == "suggested") {
            return Err(AppError::InvalidArg(format!(
                "link {id} is not an acceptable suggestion (edge_type={et}, status={status})"
            )));
        }
        // Flip the row active — that edge is the AUTHORITATIVE link. The accept is GRAPH-ONLY: we do
        // NOT materialize the neighbour's `[[Title]]` / `murmur:links` block into any note body (that
        // machine block was removed — it went stale, its wikilinks are excluded from edge-indexing,
        // and it rendered as raw junk in the plain-text editor). The live `links` table drives the
        // Related panel, so the flipped edge alone is the surface; any pre-existing block is stripped
        // on the next `get_note` read (natural save-time cleanup).
        state.db.accept_link(id)?;
    }
    tracing::info!(target: "links", link_id = id, "accept_link");
    Ok(())
}

/// Brain v3 PR-3 — DISMISS a suggested link: TOMBSTONE it so no later auto pass re-suggests it. No
/// markdown is touched (dismiss is graph-only). Idempotent.
///
/// Fix 5 (brain-v3 audit): dismissal is for SUGGESTIONS only, and is GATED. Refuse a dismiss on a
/// DETERMINISTIC edge (`wikilink`/`companion`) — those are re-derived from the note body / companion
/// link on every save, so a tombstone would be resurrected next save (a confusing no-op) AND could
/// be abused to silently suppress a real structural link. A `manual` edge is removed via
/// `unlink_items`, not dismissed. Both endpoints are gated (fail-closed) so a caller can neither
/// dismiss behind a lock nor probe a locked neighbour's existence via the accept/refuse response.
#[tauri::command]
pub fn dismiss_link(state: State<'_, AppState>, id: i64) -> Result<(), AppError> {
    dismiss_link_inner(state.inner(), id)
}

pub(crate) fn dismiss_link_inner(state: &AppState, id: i64) -> Result<(), AppError> {
    let _lifecycle = lifecycle_guard(state);
    let Some((src_kind, src_id, dst_kind, dst_id, et, _status)) = state.db.link_by_id(id)? else {
        return Err(AppError::InvalidArg(format!("no link {id}")));
    };
    // ── GATE both endpoints FIRST, for the reason spelled out in `accept_link_inner`: the type
    //    refusal below names `edge_type`, and preserved manual rows now outlive a seal. ──
    let (Some(sk), Some(dk)) = (
        crate::links::LinkKind::parse(&src_kind),
        crate::links::LinkKind::parse(&dst_kind),
    ) else {
        return Err(AppError::InvalidArg(format!(
            "link {id} has a corrupt endpoint kind"
        )));
    };
    if !link_endpoint_is_unlocked(state, sk, &src_id)?
        || !link_endpoint_is_unlocked(state, dk, &dst_id)?
    {
        return Err(AppError::Locked(
            "one of these items is locked — unlock it to dismiss the suggestion".into(),
        ));
    }
    // Only a suggestion (semantic) or an accepted-then-regretted semantic edge is dismissable. A
    // deterministic wikilink/companion edge is NOT (it would just come back); a manual edge is
    // removed by `unlink_items`. Refuse the rest with a clear InvalidArg.
    if et != "semantic" {
        return Err(AppError::InvalidArg(format!(
            "link {id} is a deterministic {et} edge — dismissal is for semantic suggestions (remove a manual link via unlink)"
        )));
    }
    state.db.dismiss_link(id)?;
    tracing::info!(target: "links", link_id = id, "dismiss_link");
    Ok(())
}

/// note↔meeting-links PR-1 — is a link ENDPOINT `(kind, id)` session-VISIBLE right now? Gate order:
/// a `Meeting` endpoint gates on [`meeting_is_unlocked`];
/// a `Note`/`Document` endpoint resolves its owning folder via `get_note_row` and gates on
/// [`folder_is_unlocked`]. An UNKNOWN endpoint (no such note/document) reports `false` — fail-closed,
/// there is nothing legitimate to link. Used by `link_items`/`unlink_items` to refuse `AppError::Locked`
/// before any write, so a manual edge is never created behind a lock and never reveals a locked
/// neighbour.
fn link_endpoint_is_unlocked(
    state: &AppState,
    kind: crate::links::LinkKind,
    id: &str,
) -> Result<bool, AppError> {
    match kind {
        crate::links::LinkKind::Meeting => meeting_is_unlocked(state, id),
        crate::links::LinkKind::Note => match state.db.note_gate_anchor(id)? {
            Some((folder_id, _created_at, _updated_at)) => folder_is_unlocked(state, &folder_id),
            None => Ok(false), // unknown note → nothing to surface. Fail-closed.
        },
        crate::links::LinkKind::Document => {
            // An imported `document` (kind != 'note') is NOT a `get_note_row` row, so routing it
            // through `get_note_row` refused it fail-closed EVEN WHEN VISIBLE (a spurious `Locked`
            // on the +Link chooser). Gate it via the canonical visibility reader
            // (`get_document_if_visible` applies `visibility_clause` against the live unlock set):
            // `Some` ⇒ visible/unlocked, `None` ⇒ sealed-or-unknown ⇒ refuse. Documents stay
            // linkable (the chooser AND the Ask source-picker both offer them) and still fail-closed.
            let unlocked = unlocked_snapshot(state)?;
            Ok(state.db.document_is_visible(id, &unlocked)?)
        }
        // An org endpoint id is the strict stable `org_id:doc_id` composite. This reader joins the
        // locally-held replica to a still-joined, context-enabled org and requires a current
        // non-tombstoned revision.
        crate::links::LinkKind::Org => Ok(state.db.org_link_target_visible(id)?.is_some()),
    }
}

/// note↔meeting-links PR-1 — USER-INITIATED link: persist ONE directed `manual` edge `(src → dst)`.
/// GRAPH-ONLY — writes NO note body.
///
/// GATE (BEFORE any write): BOTH endpoints must be session-VISIBLE — a `meeting` via
/// [`meeting_is_unlocked`], a `note`/`document` via its folder ([`folder_is_unlocked`]), and an `org`
/// endpoint through its joined, context-enabled current replica. If either is unavailable →
/// `AppError::Locked` (never link behind a lock, never reveal a withheld neighbour). Unknown kinds
/// are `AppError::InvalidArg`.
///
/// The `manual` row (`created_by='user'`, `status='active'`, `score=1.0`) is idempotent on the
/// table's UNIQUE key. It is the AUTHORITATIVE record of the link; the live `links` table drives the
/// Related panel. We DO NOT write the neighbour's `[[Title]]` / `murmur:links` block into a note body
/// — that machine block was removed (it went stale + rendered as raw junk in the plain-text editor).
#[tauri::command]
pub fn link_items(
    state: State<'_, AppState>,
    src_kind: String,
    src_id: String,
    dst_kind: String,
    dst_id: String,
) -> Result<(), AppError> {
    link_items_inner(state.inner(), &src_kind, &src_id, &dst_kind, &dst_id)
}

pub(crate) fn link_items_inner(
    state: &AppState,
    src_kind: &str,
    src_id: &str,
    dst_kind: &str,
    dst_id: &str,
) -> Result<(), AppError> {
    let src = parse_link_kind(src_kind)?;
    let dst = parse_link_kind(dst_kind)?;
    // Refuse a self-link (a pair pointing at itself is meaningless and would pollute the graph).
    if src == dst && src_id == dst_id {
        return Err(AppError::InvalidArg("cannot link an item to itself".into()));
    }
    // BLK-1 / TOCTOU: SCOPE the lifecycle guard tightly around gate + row-write so a concurrent
    // lock/relock cannot land between the visibility check and the edge upsert. It is RELEASED before
    // the note-body materialize below — that callee (`update_note_doc_inner`) takes the guard ITSELF
    // and re-checks the folder gate, so composing them under one held guard would re-enter a
    // non-reentrant `Mutex<()>` (the lifecycle_guard doc: never hold it around a callee that takes
    // it). Lock order `lifecycle ⊃ db`.
    {
        let _lifecycle = lifecycle_guard(state);
        // ── GATE both endpoints BEFORE any write (fail-closed on a sealed/unknown endpoint). ──
        if !link_endpoint_is_unlocked(state, src, src_id)?
            || !link_endpoint_is_unlocked(state, dst, dst_id)?
        {
            return Err(AppError::Locked(
                "one of these items is locked — unlock it to link".into(),
            ));
        }
        // ── Persist the directed manual edge (idempotent on the UNIQUE key). ──
        state
            .db
            .upsert_manual_link(src.as_str(), src_id, dst.as_str(), dst_id)?;
    }
    // The manual edge is the AUTHORITATIVE record of the link — the live `links` table drives the
    // Related panel. We DO NOT materialize a `[[Title]]` / `murmur:links` block into the note body:
    // that machine block existed only to surface links inside Obsidian, but it went stale (its
    // wikilinks are excluded from edge-indexing) and showed as raw junk in the plain-text editor.
    // Stop writing it here; the edge is created exactly as before. Existing blocks are stripped on
    // the next read via `get_note` (natural save-time cleanup). See `commands/notes.rs::get_note_inner`.
    tracing::info!(
        target: "links",
        src_kind = src.as_str(),
        dst_kind = dst.as_str(),
        "link_items"
    );
    Ok(())
}

/// note↔meeting-links PR-1 — REMOVE a user-initiated link: delete every exact directed `manual` row
/// carried by the collapsed chip (one or both directions), and when a source is an OWNED note strip
/// its matching legacy `[[dst Title]]` marker. NEVER touches a `wikilink`/`companion`/`semantic` row
/// for the pair. A legacy caller that omits `manual_edges` removes the one `(src → dst)` tuple.
///
/// GATE: BOTH endpoints must be session-VISIBLE (same gate as `link_items`) — never mutate a note's
/// body behind a lock, never reveal a locked neighbour's title. Legacy marker preparation is part of
/// the edge-delete transaction; a failure rolls back the full unlink. Unknown kinds are
/// `AppError::InvalidArg`.
#[tauri::command]
pub fn unlink_items(
    state: State<'_, AppState>,
    src_kind: String,
    src_id: String,
    dst_kind: String,
    dst_id: String,
    manual_edges: Option<Vec<crate::storage::models::ManualLinkEdge>>,
) -> Result<(), AppError> {
    let manual_edges = manual_edges.unwrap_or_else(|| {
        vec![crate::storage::models::ManualLinkEdge {
            src_kind: src_kind.clone(),
            src_id: src_id.clone(),
            dst_kind: dst_kind.clone(),
            dst_id: dst_id.clone(),
        }]
    });
    unlink_manual_edges_inner(
        state.inner(),
        &src_kind,
        &src_id,
        &dst_kind,
        &dst_id,
        &manual_edges,
    )
}

#[cfg(test)]
pub(crate) fn unlink_items_inner(
    state: &AppState,
    src_kind: &str,
    src_id: &str,
    dst_kind: &str,
    dst_id: &str,
) -> Result<(), AppError> {
    unlink_manual_edges_inner(
        state,
        src_kind,
        src_id,
        dst_kind,
        dst_id,
        &[crate::storage::models::ManualLinkEdge {
            src_kind: src_kind.to_string(),
            src_id: src_id.to_string(),
            dst_kind: dst_kind.to_string(),
            dst_id: dst_id.to_string(),
        }],
    )
}

/// Prepare verify-before-destroy ciphertext for a legacy marker that lives in a session-unlocked
/// locked note. The storage transaction independently recomputes the exact stripped plaintext and
/// accepts this blob only when it matches byte-for-byte, preventing a stale preparation from being
/// paired with the edge delete.
fn prepare_manual_marker_seals(
    state: &AppState,
    manual_edges: &[crate::storage::models::ManualLinkEdge],
) -> Result<Vec<crate::storage::links::PreparedManualMarkerSeal>, AppError> {
    let unlocked = unlocked_snapshot(state)?;
    let mut titles_by_note: std::collections::HashMap<String, std::collections::HashSet<String>> =
        std::collections::HashMap::new();
    for edge in manual_edges {
        let src = parse_link_kind(&edge.src_kind)?;
        if src != crate::links::LinkKind::Note {
            continue;
        }
        let dst = parse_link_kind(&edge.dst_kind)?;
        let title = state
            .db
            .link_endpoint_title_visible(dst, &edge.dst_id, &unlocked)?
            .ok_or_else(|| {
                AppError::Locked("one of these items is no longer available to unlink".into())
            })?;
        let title = crate::enrich::sanitize(&title);
        if title.trim().is_empty() {
            return Err(AppError::InvalidArg(
                "a linked item has no usable marker title".into(),
            ));
        }
        titles_by_note
            .entry(edge.src_id.clone())
            .or_default()
            .insert(title);
    }

    let mut prepared = Vec::new();
    for (note_id, titles) in titles_by_note {
        let row = state.db.get_note_row(&note_id)?.ok_or_else(|| {
            AppError::Locked("a linked note is no longer available to unlink".into())
        })?;
        let Some(stripped_text) =
            crate::storage::Db::strip_titles_from_managed_block(&row.text, &titles)
        else {
            continue;
        };
        let folder_locked = state
            .db
            .folder_by_id(&row.folder_id)?
            .map(|folder| folder.locked)
            .ok_or_else(|| AppError::Locked("a linked note folder is unavailable".into()))?;
        if folder_locked {
            let text_blob = sealed_document_blob(state, &row.folder_id, &note_id, &stripped_text)?;
            let content_key = session_folder_ck(state, &row.folder_id)?;
            prepared.push(crate::storage::links::PreparedManualMarkerSeal {
                note_id,
                folder_id: row.folder_id,
                stripped_text,
                text_blob,
                content_key,
            });
        }
    }
    Ok(prepared)
}

/// Remove every exact directed manual row hidden behind one displayed neighbour chip. The display
/// representative may be an oppositely-directed wikilink, so `manual_edges` is authoritative; the
/// legacy `(src, dst)` arguments bind that list to one unordered endpoint pair and prevent a caller
/// from smuggling unrelated deletes into the same transaction.
pub(crate) fn unlink_manual_edges_inner(
    state: &AppState,
    src_kind: &str,
    src_id: &str,
    dst_kind: &str,
    dst_id: &str,
    manual_edges: &[crate::storage::models::ManualLinkEdge],
) -> Result<(), AppError> {
    let src = parse_link_kind(src_kind)?;
    let dst = parse_link_kind(dst_kind)?;
    if manual_edges.is_empty() || manual_edges.len() > 2 {
        return Err(AppError::InvalidArg(
            "a collapsed pair must contain one or two manual link edges".into(),
        ));
    }
    for (index, edge) in manual_edges.iter().enumerate() {
        if manual_edges[..index].contains(edge) {
            return Err(AppError::InvalidArg(
                "duplicate manual link edge in unlink request".into(),
            ));
        }
        let edge_src = parse_link_kind(&edge.src_kind)?;
        let edge_dst = parse_link_kind(&edge.dst_kind)?;
        let direct =
            edge_src == src && edge.src_id == src_id && edge_dst == dst && edge.dst_id == dst_id;
        let reverse =
            edge_src == dst && edge.src_id == dst_id && edge_dst == src && edge.dst_id == src_id;
        if !(direct || reverse) {
            return Err(AppError::InvalidArg(
                "manual link edge does not belong to the displayed endpoint pair".into(),
            ));
        }
    }
    let cleanup_queued = {
        let _lifecycle = lifecycle_guard(state);
        // ── GATE every exact tuple's endpoints before the atomic delete. ──
        for edge in manual_edges {
            let edge_src = parse_link_kind(&edge.src_kind)?;
            let edge_dst = parse_link_kind(&edge.dst_kind)?;
            if !link_endpoint_is_unlocked(state, edge_src, &edge.src_id)?
                || !link_endpoint_is_unlocked(state, edge_dst, &edge.dst_id)?
            {
                return Err(AppError::Locked(
                    "one of these items is locked — unlock it to unlink".into(),
                ));
            }
        }
        // Delete the COMPLETE collapsed manual set atomically. The DB transaction repeats its
        // at-rest endpoint checks before deleting and never touches a derived edge.
        let prepared_seals = prepare_manual_marker_seals(state, manual_edges)?;
        // The DB transaction first strips any legacy source marker and enqueues the exact vault
        // cleanup intent, then deletes every requested manual tuple. Preparation failure rolls back
        // all mutations, so relock can never lose the last edge before a durable scrub exists.
        state
            .db
            .delete_manual_links_with_marker_seals(manual_edges, &prepared_seals)?
    };
    // Filesystem publication runs after the DB commit, from a SQLCipher-backed retryable outbox.
    // Propagate failure: the DB marker is already safe and the outbox remains for relock/startup,
    // but callers must never receive false success while an exact vault export still needs scrub.
    if cleanup_queued {
        // Re-acquire (rather than retain) the lifecycle guard: a destination relock is allowed to
        // win this boundary and drain the same durable intent first, but two filesystem drainers
        // can never race one another's authenticated stage reservation.
        let _lifecycle = lifecycle_guard(state);
        drain_lock_marker_export_cleanup(&state.db)?;
    }
    tracing::info!(
        target: "links",
        src_kind = src.as_str(),
        dst_kind = dst.as_str(),
        count = manual_edges.len(),
        "unlink_items"
    );
    Ok(())
}

/// Resolve a clicked `[[Title]]` wikilink to a VISIBLE note/meeting/org-item to navigate to.
/// Returns `None` when nothing matches OR the only local match is a
/// sealed-and-not-session-unlocked target (gated in `Db::resolve_wikilink`) — so a wikilink click
/// never reveals or opens locked content. The org leg (2026-07-15) is gated by membership +
/// per-instance-enabled, the same seam `list_link_candidates`/`search_org_brain_hits` use. The FE
/// routes on `kind` (`"meeting"` | `"note"` | `"org"`), or offers to create a note when `None`.
#[tauri::command]
pub fn resolve_wikilink(
    state: State<'_, AppState>,
    title: String,
) -> Result<Option<crate::storage::models::WikiTarget>, AppError> {
    let _lifecycle = lifecycle_guard(state.inner());
    let unlocked = unlocked_snapshot(state.inner())?;
    state.db.resolve_wikilink(&title, &unlocked)
}

/// Live keystroke-prefix candidates for the inline `[[` / slash-menu link-insertion autocomplete
/// (note-editor Fix 2). Distinct from [`resolve_wikilink`] (exact-title resolve on Enter/click) and
/// from `note_assistant_action`'s `find_related` (SELECTION+semantic retrieval — the wrong shape
/// for filtering on a short, growing keystroke prefix): this is a lightweight, gated title-prefix
/// scan. GATED exactly like every other content read: notes/meetings go through
/// `Db::list_link_candidates_visible` (`visibility_clause` on both legs, same as `resolve_wikilink`);
/// org items go through `search_org_brain_hits`, the SAME retrieval-only, membership+enabled-gated,
/// zero-egress reader `find_related` already uses (never a provider/egress call). Reuses
/// [`crate::storage::models::NoteCitation`] — the popover renders it exactly like a `find_related`
/// citation row. For `kind == "org"`, `id` carries the stable `org_id:doc_id` link identity;
/// navigation/display readers resolve that composite to the current item revision.
///
/// PAGINATED (2026-07-17 — the picker is an infinite scroll over the whole vault now, not a fixed
/// top-8): one call returns the `limit`-sized page at `offset` of the stable combined ordering
/// [visible notes] ++ [visible meetings] ++ [org hits] (org only for a non-empty prefix, folded in
/// after the local total the Db reader reports). The FE owns its page size; the clamp keeps one
/// IPC reply from ever dumping an unbounded slice of the vault.
#[tauri::command]
pub fn list_link_candidates(
    state: State<'_, AppState>,
    prefix: String,
    offset: Option<u32>,
    limit: Option<u32>,
) -> Result<Vec<crate::storage::models::NoteCitation>, AppError> {
    let _lifecycle = lifecycle_guard(state.inner());
    const DEFAULT_PAGE: i64 = 40;
    const MAX_PAGE: i64 = 100;
    let limit = limit.map_or(DEFAULT_PAGE, i64::from).clamp(1, MAX_PAGE);
    let offset = i64::from(offset.unwrap_or(0));
    let unlocked = unlocked_snapshot(state.inner())?;
    let (mut out, local_total) = state
        .db
        .list_link_candidates_visible(&prefix, limit, offset, &unlocked)?;
    if (out.len() as i64) < limit {
        let config = {
            state
                .config
                .lock()
                .map_err(|_| AppError::Config("config mutex poisoned".into()))?
                .clone()
        };
        let q = prefix.trim();
        if !q.is_empty() && crate::tools::org_brain_available(&state.db, &config) {
            // Earlier pages consumed `local_total` local rows, then `offset - local_total`
            // org rows once the offset ran past the local legs — skip exactly those. The
            // org reader is bounded (≤20 per leg pre-fusion), so skip/take over its Vec
            // is real pagination, not a hidden re-scan.
            let org_skip = (offset - local_total).max(0) as usize;
            let remaining = (limit - out.len() as i64).max(0) as usize;
            let org_hits = crate::tools::search_org_brain_hits(&state.db, &config, q)?;
            for hit in org_hits.into_iter().skip(org_skip).take(remaining) {
                let Some(doc_id) = state.db.org_link_doc_id_for_item_visible(&hit.item_id)? else {
                    continue;
                };
                out.push(crate::storage::models::NoteCitation {
                    kind: "org".into(),
                    id: doc_id,
                    title: hit.title,
                    snippet: hit.snippet,
                });
            }
        }
    }
    Ok(out)
}
