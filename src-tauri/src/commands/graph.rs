//! Graph / entity / people / dossier read commands — extracted verbatim from `commands` (God-file
//! split, a PURE MOVE — the visibility-gate logic is UNCHANGED, only relocated). This is the
//! self-assembling knowledge-graph read surface: the entity co-occurrence graph, the full-brain
//! typed graph, `/people`, per-entity detail + knowledge-diff + backlinks, and the structured/
//! synthesized dossiers. EVERY read here is GATED: it snapshots the LIVE session `unlocked` set via
//! `super::unlocked_snapshot` and pushes it through the DB visibility predicate
//! (`build_graph` / `build_full_graph` / `list_people` / `build_entity_detail` /
//! `backlinks_for_visible` / `resolve_entity_id` / `build_dossier_data`), so a
//! sealed-and-not-session-unlocked meeting/note/document contributes NOTHING — the gate is
//! byte-identical to its pre-move form. Every symbol keeps its EXACT prior body/signature and is
//! re-exported at `crate::commands` via `pub use graph_commands::*;` in `commands/mod.rs`, so
//! `generate_handler![commands::get_graph]` in `lib.rs` and every `crate::commands::…` caller resolve
//! UNCHANGED. `use super::*` brings in the shared types + the gate helper `unlocked_snapshot` and the
//! `get_person_dossier_inner` core (both kept in `commands/mod.rs`); `build_entity_dossier_prompt` is
//! promoted to `pub(crate)` so the `entity_dossier` command + `entity_dossier_tests` (kept in
//! `commands/mod.rs`) still reach it through the re-export.

use super::*;

/// Max co-occurring neighbors returned with an entity's detail (the neighborhood satellites).
const ENTITY_NEIGHBOR_LIMIT: i64 = 12;

/// The self-assembling graph: all VISIBLE entity nodes (with their visible mention counts) + all
/// VISIBLE co-occurrence edges. Snapshots the live session `unlocked` set (same as `list_folders`)
/// and pushes it through the visibility predicate, so sealed-and-not-unlocked meetings contribute
/// nothing — the graph can never disagree with Library/MCP about what's visible.
#[tauri::command]
pub async fn get_graph(app: AppHandle) -> Result<GraphData, AppError> {
    offload_read(app, |state| {
        let unlocked = unlocked_snapshot(state)?;
        state.db.build_graph(&unlocked)
    })
    .await
}

/// The FULL-BRAIN graph (Brain v3 PR-4): a SEPARATE, additive read that unifies entities + VISIBLE
/// meetings + notes + documents as TYPED nodes and every relation (entity co-occurrence + entity→
/// meeting mentions + `links` rows) as TYPED edges. Snapshots the live session `unlocked` set exactly
/// like `get_graph`, so a sealed-and-not-session-unlocked meeting/note/document contributes NOTHING —
/// no node, and no edge touching it (BOTH endpoints are gated against the visible-node set). `opts`
/// defaults to all-off; `includeSuggested` admits un-accepted (`status='suggested'`) semantic links.
/// Pure read: no writes, no new storage. `get_graph` is untouched + byte-compatible for its FE.
#[tauri::command]
pub async fn get_full_graph(
    app: AppHandle,
    opts: Option<FullGraphOpts>,
) -> Result<FullGraphData, AppError> {
    offload_read(app, move |state| {
        let unlocked = unlocked_snapshot(state)?;
        state
            .db
            .build_full_graph(&unlocked, opts.unwrap_or_default())
    })
    .await
}

/// `/people` personal CRM: one card per VISIBLE Person entity, rolled up over the SAME gated
/// graph/facts/commitment readers as the graph + rollup views (`list_entities_visible` filtered to
/// people, `entity_mentions_visible`, `list_facts_visible`, `list_open_commitments`). Snapshots the
/// live session `unlocked` set like `get_graph`, so a person whose only mentions are in
/// sealed-and-not-session-unlocked meetings never appears and every count reflects visible sources
/// only. Read-only, no model, no new egress.
#[tauri::command]
pub fn list_people(state: State<'_, AppState>) -> Result<PeopleList, AppError> {
    let unlocked = unlocked_snapshot(state.inner())?;
    state.db.list_people(&unlocked)
}

/// Detail for one entity: the entity, its VISIBLE backlinked meetings (as `VaultSource` chips),
/// and its top co-occurring neighbors. Snapshots the live `unlocked` set like `get_graph`.
/// Errors with `InvalidArg` if the entity id is unknown.
#[tauri::command]
pub fn get_entity_detail(
    state: State<'_, AppState>,
    entity_id: String,
) -> Result<EntityDetail, AppError> {
    let unlocked = unlocked_snapshot(state.inner())?;
    state
        .db
        .build_entity_detail(&entity_id, &unlocked, ENTITY_NEIGHBOR_LIMIT)?
        .ok_or_else(|| AppError::InvalidArg(format!("no entity with id {entity_id}")))
}

/// Brain v3 PR-6 — the KNOWLEDGE DIFF / decision ledger for one entity: what the vault knew AS OF
/// `from` vs AS OF `to` (added / removed / changed facts), plus the full chronological supersession
/// ledger (each: old object → new object, when it took effect, the source meeting). `entity` is a
/// name OR an id — resolved through the GATED [`crate::summarize::dossier::resolve_entity_id`] (an
/// entity mentioned only in sealed-not-unlocked meetings never resolves). The facts themselves are
/// read through the visibility-gated [`crate::storage::Db::list_facts_visible`] inside
/// [`crate::facts::build_knowledge_diff`] — a sealed-and-not-session-unlocked meeting's fact enters
/// no snapshot, diff entry, or ledger row. Snapshots the live session unlock set like `get_graph`.
/// Read-only, deterministic, no model, no new egress. `InvalidArg` if the entity does not resolve.
#[tauri::command]
pub fn get_entity_knowledge_diff(
    state: State<'_, AppState>,
    entity: String,
    from: String,
    to: String,
) -> Result<crate::facts::EntityKnowledgeDiff, AppError> {
    let unlocked = unlocked_snapshot(state.inner())?;
    // GATE (anti-leak, FIRST): resolve name/id → a VISIBLE entity id, or refuse. An entity whose
    // mentions are all sealed-not-unlocked is indistinguishable from an unknown one.
    let id = crate::summarize::dossier::resolve_entity_id(&state.db, &entity, &unlocked)?
        .ok_or_else(|| AppError::InvalidArg(format!("no visible entity matching {entity:?}")))?;
    crate::facts::build_knowledge_diff(&state.db, &id, &from, &to, &unlocked)
}

/// "What links here" for a note or meeting: every VISIBLE meeting-note / standalone-note whose body
/// carries a `[[<this row's title>]]` wikilink. GATE is folded into the DB builder
/// (`Db::backlinks_for_visible`) exactly like [`get_entity_detail`] — no separate command-layer
/// pre-check. Snapshots the LIVE session unlock set, so a sealed target yields `[]` (never reveals it
/// HAS backlinks) and a sealed source never contributes. `target_kind` is `"meeting"` | `"note"`.
#[tauri::command]
pub fn get_backlinks(
    state: State<'_, AppState>,
    target_kind: String,
    target_id: String,
) -> Result<Vec<crate::storage::models::BacklinkSource>, AppError> {
    let kind = match target_kind.as_str() {
        "meeting" => crate::storage::models::SourceKind::Meeting,
        "note" => crate::storage::models::SourceKind::Note,
        other => {
            return Err(AppError::InvalidArg(format!(
                "unknown backlink target_kind {other:?} (expected \"meeting\" or \"note\")"
            )))
        }
    };
    let unlocked = unlocked_snapshot(state.inner())?;
    state.db.backlinks_for_visible(kind, &target_id, &unlocked)
}

/// Structured, GATED, egress-free person dossier for the `/people` detail pane. Unlike
/// [`entity_dossier`] (which CLOUD-synthesizes a markdown String via the provider and discards the
/// struct), this returns the STRUCTURED [`DossierData`](crate::summarize::dossier::DossierData) with
/// NO provider/cloud call — deterministic DB assembly, strictly MORE local-first. Gated exactly like
/// [`get_entity_detail`]/[`list_people`]: it snapshots the LIVE session unlock set and reuses
/// `build_dossier_data` VERBATIM, so a sealed-and-not-session-unlocked meeting contributes NOTHING
/// (its title, note body, commitments, and facts all stay invisible until the folder is
/// session-unlocked). `corpus` is `#[serde(skip)]`, so meeting note bodies never reach the FE.
#[tauri::command]
pub fn get_person_dossier(
    state: State<'_, AppState>,
    entity_id: String,
) -> Result<crate::summarize::dossier::DossierData, AppError> {
    get_person_dossier_inner(state.inner(), &entity_id)
}

/// The testable, SYNCHRONOUS core of [`entity_dossier`]: resolve the entity + gated local dossier
/// data, ALSO run the READ-ONLY org leg (B2), and assemble the (system, user, has_org_context)
/// synthesis-prompt triple — everything short of the actual `provider.complete` cloud call, so this
/// can be tested without a real provider/network.
///
/// B2 (Shared Brain, READ-ONLY): when the caller has joined an org (`org_brain_available`), ALSO
/// searches the org partition ONCE for this entity and folds `[org · author]`-labelled hits into
/// the synthesis prompt ONLY — an entity with ZERO local facts/mentions can still resolve here
/// purely from org content (`data` may be `None` while `org_context` is `Some`). This function NEVER
/// calls `build_and_persist_entities` and NEVER writes to `entities`/`entity_mentions`/`facts` — it
/// only reads. `has_org_context` is `true` iff org-sourced lines were folded in, so the caller can
/// honestly signal that the dossier drew on colleague content.
pub(crate) fn build_entity_dossier_prompt(
    db: &crate::storage::Db,
    entity: &str,
    unlocked: &std::collections::HashSet<String>,
    config: &AppConfig,
    notes_conn: &str,
) -> Result<(String, String, bool), AppError> {
    if entity.trim().is_empty() {
        return Err(AppError::InvalidArg("entity is empty".into()));
    }
    // B2: resolve the ORG leg once, up front — an entity that is ORG-ONLY (no local entity row at
    // all) must still be resolvable, so the local-entity lookup below is allowed to miss as long as
    // org content is present. Gated on the SAME `org_brain_available` predicate as every other org
    // leg (A1/A2); one call, no per-query looping. READ-ONLY: `search_org_brain` never writes.
    let org_query = entity.trim().to_string();
    let org_context = if crate::tools::org_brain_available(db, config) {
        match crate::tools::search_org_brain(db, config, &org_query) {
            Ok(text) => {
                let text = text.trim();
                // `search_org_brain` degrades to a "No org-brain results …" sentinel (never an Err)
                // when there is nothing to find — that carries no `- [org` line, so it is dropped
                // here exactly like every other "nothing found" sentinel this codebase filters
                // (`voice_action::is_empty_tool_result`). A real hit's text is passed THROUGH
                // WHOLE (never re-split by line) — a snippet can legitimately wrap onto a second
                // line that doesn't itself start with "- [org", and truncating there would silently
                // drop cited content from the synthesis prompt.
                if text.is_empty() || !text.contains("- [org") {
                    None
                } else {
                    Some(text.to_string())
                }
            }
            Err(e) => {
                tracing::debug!(
                    target: "dossier",
                    error = %e,
                    "entity dossier org-brain search failed; continuing without org context"
                );
                None
            }
        }
    } else {
        None
    };

    let entity_id = crate::summarize::dossier::resolve_entity_id(db, entity, unlocked)?;
    let data = match entity_id {
        Some(id) => crate::summarize::dossier::build_dossier_data(db, &id, unlocked)?,
        None => None,
    };
    // Neither a local entity NOR any org context ⇒ genuinely unknown — the pre-org error semantics.
    if data.is_none() && org_context.is_none() {
        return Err(AppError::InvalidArg(format!(
            "no visible entity matching \"{entity}\""
        )));
    }
    let mut system = crate::summarize::dossier::dossier_system_prompt(&config.note_language);
    let mut user = match &data {
        Some(d) => crate::summarize::dossier::render_dossier_user(d, notes_conn),
        // Org-only entity: no local dossier data at all — hand the model just the entity name so it
        // has a subject to synthesize the org section against.
        None => format!(
            "ENTITY: {}\n(no local meetings/facts found for this entity)\n",
            entity.trim()
        ),
    };
    let has_org_context = if let Some(org_lines) = &org_context {
        system.push_str(
            " Additional context may include lines starting \"- [org ·\" — these are colleague-\
             shared facts from your organization's Shared Brain, NOT the user's own verified data. \
             Attribute any claim drawn from them by their \"[org · author]\" provenance, clearly \
             distinguished from the user's own meetings/facts.",
        );
        user.push_str(&format!(
            "\n\nADDITIONAL CONTEXT FROM YOUR ORGANIZATION'S SHARED BRAIN (colleague-shared, \
             attribute by provenance):\n{org_lines}\n"
        ));
        true
    } else {
        false
    };
    Ok((system, user, has_org_context))
}
