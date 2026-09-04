//! Durable Ask Brain conversation commands for vault, note, and meeting anchors. An optional
//! dashboard ID composes current board material/derived views into any of those scopes while the
//! backend remains the sole authority for history and immutable first-turn provenance.

use super::*;
use crate::storage::models::{AskConversationScope, AskConversationSendResult, SourceRef};
use tauri::ipc::Response;

#[derive(Debug, Clone)]
pub(crate) enum DurableScopeSnapshot {
    Vault(ContentVisibilitySnapshot),
    Note {
        note_id: String,
        snapshot: DocumentContentSnapshot,
    },
    Meeting {
        meeting_id: String,
        snapshot: MeetingContentSnapshot,
    },
}

pub(crate) fn capture_durable_scope_under_lifecycle(
    state: &AppState,
    scope: &AskConversationScope,
) -> Result<DurableScopeSnapshot, AppError> {
    scope.validate()?;
    let visibility = ContentVisibilitySnapshot {
        seal_epoch: state.seal_epoch.load(std::sync::atomic::Ordering::SeqCst),
    };
    match scope {
        AskConversationScope::Vault => Ok(DurableScopeSnapshot::Vault(visibility)),
        AskConversationScope::Note { ref_id } => {
            let Some((folder_id, _created_at, _updated_at)) = state.db.note_gate_anchor(ref_id)?
            else {
                return Err(AppError::InvalidArg(
                    "conversation scope is unavailable".into(),
                ));
            };
            if !folder_is_unlocked(state, &folder_id)? {
                return Err(AppError::Locked("conversation scope is unavailable".into()));
            }
            Ok(DurableScopeSnapshot::Note {
                note_id: ref_id.clone(),
                snapshot: DocumentContentSnapshot {
                    folder_id,
                    visibility,
                },
            })
        }
        AskConversationScope::Meeting { ref_id } => {
            if !meeting_is_unlocked(state, ref_id)? {
                return Err(AppError::Locked("conversation scope is unavailable".into()));
            }
            // Bind the current folder association so an ordinary open-folder move also invalidates
            // a result generated from the pre-move scope.
            let folder_id = state.db.folder_for_meeting(ref_id)?;
            if state.db.get_meeting(ref_id)?.is_none() {
                return Err(AppError::InvalidArg(
                    "conversation scope is unavailable".into(),
                ));
            }
            Ok(DurableScopeSnapshot::Meeting {
                meeting_id: ref_id.clone(),
                snapshot: MeetingContentSnapshot {
                    folder_id,
                    visibility,
                    active_related: active_related_witness(state, ref_id)?,
                },
            })
        }
    }
}

#[cfg(test)]
pub(crate) fn serialize_durable_send_response_with_dispatch(
    state: &AppState,
    snapshot: &DurableScopeSnapshot,
    ask_dispatch: &AskDispatchSnapshot,
    dashboard: Option<&crate::commands::dashboards::DashboardContextWitness>,
    payload: &AskConversationSendResult,
) -> Result<Response, AppError> {
    let _lifecycle = lifecycle_guard(state);
    require_durable_scope_under_lifecycle(state, snapshot)?;
    require_current_ask_dispatch_under_lifecycle(state, ask_dispatch)?;
    if let Some(witness) = dashboard {
        let unlocked = unlocked_snapshot(state)?;
        crate::commands::dashboards::require_current_dashboard_context_witness_under_lifecycle(
            state, witness, &unlocked,
        )?;
    }
    serde_json::to_string(payload)
        .map(Response::new)
        .map_err(|_| AppError::Unavailable("conversation response encoding failed".into()))
}

#[cfg(test)]
pub(crate) fn serialize_durable_send_response(
    state: &AppState,
    snapshot: &DurableScopeSnapshot,
    dashboard: Option<&crate::commands::dashboards::DashboardContextWitness>,
    payload: &AskConversationSendResult,
) -> Result<Response, AppError> {
    let ask_dispatch = {
        let _lifecycle = lifecycle_guard(state);
        let config = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?
            .clone();
        capture_ask_dispatch_snapshot_under_lifecycle(state, &config)?
    };
    serialize_durable_send_response_with_dispatch(
        state,
        snapshot,
        &ask_dispatch,
        dashboard,
        payload,
    )
}

pub(crate) fn require_durable_scope_under_lifecycle(
    state: &AppState,
    snapshot: &DurableScopeSnapshot,
) -> Result<(), AppError> {
    match snapshot {
        DurableScopeSnapshot::Vault(visibility) => {
            require_current_content_visibility_snapshot_under_lifecycle(state, *visibility)
        }
        DurableScopeSnapshot::Note { note_id, snapshot } => {
            require_current_document_content_snapshot_under_lifecycle(state, note_id, snapshot)
        }
        DurableScopeSnapshot::Meeting {
            meeting_id,
            snapshot,
        } => require_current_meeting_content_snapshot_under_lifecycle(state, meeting_id, snapshot),
    }
}

pub(crate) fn require_durable_scope_for_dispatch_with_ask(
    state: &AppState,
    snapshot: &DurableScopeSnapshot,
    ask_dispatch: &AskDispatchSnapshot,
    dashboard: Option<&crate::commands::dashboards::DashboardContextWitness>,
) -> Result<(), AppError> {
    let _lifecycle = lifecycle_guard(state);
    require_durable_scope_under_lifecycle(state, snapshot)?;
    require_current_ask_dispatch_under_lifecycle(state, ask_dispatch)?;
    if let Some(witness) = dashboard {
        let unlocked = unlocked_snapshot(state)?;
        crate::commands::dashboards::require_current_dashboard_context_witness_under_lifecycle(
            state, witness, &unlocked,
        )?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn require_durable_scope_for_dispatch(
    state: &AppState,
    snapshot: &DurableScopeSnapshot,
    dashboard: Option<&crate::commands::dashboards::DashboardContextWitness>,
) -> Result<(), AppError> {
    let ask_dispatch = {
        let _lifecycle = lifecycle_guard(state);
        let config = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?
            .clone();
        capture_ask_dispatch_snapshot_under_lifecycle(state, &config)?
    };
    require_durable_scope_for_dispatch_with_ask(state, snapshot, &ask_dispatch, dashboard)
}

pub(crate) fn durable_dispatch_admission(
    app: &AppHandle,
    snapshot: DurableScopeSnapshot,
    ask_dispatch: AskDispatchSnapshot,
    dashboard: Option<crate::commands::dashboards::DashboardContextWitness>,
) -> crate::state::ContentDispatchAdmission {
    crate::state::ContentDispatchAdmission::new(app, move |state| {
        require_durable_scope_under_lifecycle(state, &snapshot)?;
        require_current_ask_dispatch_under_lifecycle(state, &ask_dispatch)?;
        if let Some(witness) = dashboard.as_ref() {
            let unlocked = unlocked_snapshot(state)?;
            crate::commands::dashboards::require_current_dashboard_context_witness_under_lifecycle(
                state, witness, &unlocked,
            )?;
        }
        Ok(())
    })
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn persist_ask_exchange_after_await_with_dispatch(
    state: &AppState,
    snapshot: &DurableScopeSnapshot,
    ask_dispatch: &AskDispatchSnapshot,
    scope: &AskConversationScope,
    conversation_id: Option<&str>,
    resume_cursor: Option<crate::storage::ask_conversation_store::AskConversationCursor>,
    question: &str,
    answer: &str,
    selected_sources: &[SourceRef],
    assistant_sources: &[crate::storage::models::VaultSource],
    assistant_citations: &[String],
    dependency_folders: &[String],
    dashboard: Option<&crate::commands::dashboards::DashboardContextWitness>,
) -> Result<crate::storage::ask_conversation_store::PersistedAskExchange, AppError> {
    let _lifecycle = lifecycle_guard(state);
    persist_ask_exchange_under_lifecycle(
        state,
        snapshot,
        ask_dispatch,
        scope,
        conversation_id,
        resume_cursor,
        question,
        answer,
        selected_sources,
        assistant_sources,
        assistant_citations,
        dependency_folders,
        dashboard,
    )
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn persist_ask_exchange_after_await(
    state: &AppState,
    snapshot: &DurableScopeSnapshot,
    scope: &AskConversationScope,
    conversation_id: Option<&str>,
    resume_cursor: Option<crate::storage::ask_conversation_store::AskConversationCursor>,
    question: &str,
    answer: &str,
    selected_sources: &[SourceRef],
    assistant_sources: &[crate::storage::models::VaultSource],
    assistant_citations: &[String],
    dependency_folders: &[String],
    dashboard: Option<&crate::commands::dashboards::DashboardContextWitness>,
) -> Result<crate::storage::ask_conversation_store::PersistedAskExchange, AppError> {
    let ask_dispatch = {
        let _lifecycle = lifecycle_guard(state);
        let config = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?
            .clone();
        capture_ask_dispatch_snapshot_under_lifecycle(state, &config)?
    };
    persist_ask_exchange_after_await_with_dispatch(
        state,
        snapshot,
        &ask_dispatch,
        scope,
        conversation_id,
        resume_cursor,
        question,
        answer,
        selected_sources,
        assistant_sources,
        assistant_citations,
        dependency_folders,
        dashboard,
    )
}

#[allow(clippy::too_many_arguments)]
fn persist_ask_exchange_under_lifecycle(
    state: &AppState,
    snapshot: &DurableScopeSnapshot,
    ask_dispatch: &AskDispatchSnapshot,
    scope: &AskConversationScope,
    conversation_id: Option<&str>,
    resume_cursor: Option<crate::storage::ask_conversation_store::AskConversationCursor>,
    question: &str,
    answer: &str,
    selected_sources: &[SourceRef],
    assistant_sources: &[crate::storage::models::VaultSource],
    assistant_citations: &[String],
    dependency_folders: &[String],
    dashboard: Option<&crate::commands::dashboards::DashboardContextWitness>,
) -> Result<crate::storage::ask_conversation_store::PersistedAskExchange, AppError> {
    require_durable_scope_under_lifecycle(state, snapshot)?;
    require_current_ask_dispatch_under_lifecycle(state, ask_dispatch)?;
    if let Some(witness) = dashboard {
        let unlocked = unlocked_snapshot(state)?;
        crate::commands::dashboards::require_current_dashboard_context_witness_under_lifecycle(
            state, witness, &unlocked,
        )?;
    }
    // The provider seam may assemble its final retrieval context after the caller's initial
    // dependency snapshot. Visibility reductions bump `seal_epoch` and fail the revalidation
    // above; a visibility increase does not. Union the NOW-visible folders while the same
    // lifecycle guard is held, so a folder session-unlocked during inference becomes a durable
    // dependency and a later relock cannot expose this answer through the defensive reader gate.
    let unlocked = unlocked_snapshot(state)?;
    let current_visible = state
        .db
        .visible_folder_ids(&unlocked)?
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    if dependency_folders
        .iter()
        .any(|folder_id| !current_visible.contains(folder_id))
    {
        return Err(AppError::Locked(
            "content visibility changed while generating the answer".into(),
        ));
    }
    let mut dependency_folders = dependency_folders
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    dependency_folders.extend(current_visible);
    let dependency_folders = dependency_folders.into_iter().collect::<Vec<_>>();
    state.db.persist_ask_exchange_cas_with_dispatch(
        scope,
        conversation_id,
        resume_cursor,
        question,
        answer,
        selected_sources,
        assistant_sources,
        assistant_citations,
        &dependency_folders,
        dashboard.map(|witness| witness.dashboard_id.as_str()),
        dashboard.map(|witness| witness.generation),
        dashboard.map(|witness| witness.input_digest.as_str()),
        ask_dispatch.generation,
        &chrono::Utc::now().to_rfc3339(),
    )
}

/// Commit the canonical exchange and encode the exact committed identities without releasing the
/// lifecycle mutex between those two operations. Config/consent writers therefore cannot create a
/// hidden committed turn whose stale result is refused only at IPC serialization.
#[allow(clippy::too_many_arguments)]
fn finish_persisted_ask_send_after_await(
    state: &AppState,
    snapshot: &DurableScopeSnapshot,
    ask_dispatch: &AskDispatchSnapshot,
    scope: &AskConversationScope,
    conversation_id: Option<&str>,
    resume_cursor: Option<crate::storage::ask_conversation_store::AskConversationCursor>,
    question: &str,
    answer: String,
    selected_sources: &[SourceRef],
    assistant_sources: Vec<crate::storage::models::VaultSource>,
    assistant_citations: Vec<String>,
    dependency_folders: &[String],
    dashboard: Option<&crate::commands::dashboards::DashboardContextWitness>,
) -> Result<Response, AppError> {
    finish_persisted_ask_send_after_await_core(
        state,
        snapshot,
        ask_dispatch,
        scope,
        conversation_id,
        resume_cursor,
        question,
        answer,
        selected_sources,
        assistant_sources,
        assistant_citations,
        dependency_folders,
        dashboard,
        || {},
    )
}

#[allow(clippy::too_many_arguments)]
fn finish_persisted_ask_send_after_await_core(
    state: &AppState,
    snapshot: &DurableScopeSnapshot,
    ask_dispatch: &AskDispatchSnapshot,
    scope: &AskConversationScope,
    conversation_id: Option<&str>,
    resume_cursor: Option<crate::storage::ask_conversation_store::AskConversationCursor>,
    question: &str,
    answer: String,
    selected_sources: &[SourceRef],
    assistant_sources: Vec<crate::storage::models::VaultSource>,
    assistant_citations: Vec<String>,
    dependency_folders: &[String],
    dashboard: Option<&crate::commands::dashboards::DashboardContextWitness>,
    after_commit: impl FnOnce(),
) -> Result<Response, AppError> {
    let _lifecycle = lifecycle_guard(state);
    let committed = persist_ask_exchange_under_lifecycle(
        state,
        snapshot,
        ask_dispatch,
        scope,
        conversation_id,
        resume_cursor,
        question,
        &answer,
        selected_sources,
        &assistant_sources,
        &assistant_citations,
        dependency_folders,
        dashboard,
    )?;
    after_commit();
    let payload = AskConversationSendResult {
        conversation_id: committed.conversation_id,
        user_message_id: committed.user_message_id,
        assistant_message_id: committed.assistant_message_id,
        answer: answer.trim().to_string(),
        sources: assistant_sources,
        citations: assistant_citations,
    };
    serde_json::to_string(&payload)
        .map(Response::new)
        .map_err(|_| AppError::Unavailable("conversation response encoding failed".into()))
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn finish_persisted_ask_send_after_await_with_hook(
    state: &AppState,
    snapshot: &DurableScopeSnapshot,
    ask_dispatch: &AskDispatchSnapshot,
    scope: &AskConversationScope,
    question: &str,
    answer: String,
    after_commit: impl FnOnce(),
) -> Result<Response, AppError> {
    finish_persisted_ask_send_after_await_core(
        state,
        snapshot,
        ask_dispatch,
        scope,
        None,
        None,
        question,
        answer,
        &[],
        Vec::new(),
        Vec::new(),
        &[],
        None,
        after_commit,
    )
}

fn normalized_sources_for_scope(
    scope: &AskConversationScope,
    explicit_sources: Option<Vec<SourceRef>>,
) -> Result<Option<Vec<SourceRef>>, AppError> {
    match scope {
        AskConversationScope::Note { ref_id } => {
            let mut sources = explicit_sources.unwrap_or_default();
            if !sources
                .iter()
                .any(|source| source.kind == crate::links::LinkKind::Note && source.id == *ref_id)
            {
                sources.insert(
                    0,
                    SourceRef {
                        kind: crate::links::LinkKind::Note,
                        id: ref_id.clone(),
                    },
                );
            }
            Ok(Some(sources))
        }
        AskConversationScope::Vault => Ok(explicit_sources),
        AskConversationScope::Meeting { .. } => Err(AppError::InvalidArg(
            "conversation scope is unavailable".into(),
        )),
    }
}

fn resolve_conversation_dashboard_id(
    continuing: bool,
    requested: Option<&str>,
    persisted: Option<String>,
) -> Result<Option<String>, AppError> {
    let requested = requested
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string);
    if !continuing {
        return Ok(requested);
    }
    if requested.is_some() && requested != persisted {
        return Err(AppError::InvalidArg(
            "conversation dashboard scope cannot be changed".into(),
        ));
    }
    Ok(persisted)
}

pub(crate) fn require_persisted_dashboard_provenance_under_lifecycle(
    state: &AppState,
    scope: &AskConversationScope,
    provenance: &crate::storage::ask_conversation_store::AskDashboardProvenance,
    config: &AppConfig,
    unlocked: &std::collections::HashSet<String>,
) -> Result<(), AppError> {
    let ask_conn =
        crate::summarize::roles::provider_target(crate::summarize::roles::Role::Ask, config)
            .connection;
    let excluded_meeting = match scope {
        AskConversationScope::Meeting { ref_id } => Some(ref_id.as_str()),
        AskConversationScope::Vault | AskConversationScope::Note { .. } => None,
    };
    let budget = excluded_meeting.map_or_else(
        || crate::summarize::vault_context::budget_for(&ask_conn),
        |_| {
            crate::summarize::vault_context::budget_for(&ask_conn)
                .min(crate::summarize::chat::MAX_PINNED_SOURCE_CHARS)
        },
    );
    let current = crate::commands::dashboards::dashboard_composite_context(
        &state.db,
        &provenance.dashboard_id,
        unlocked,
        budget,
        &provenance.selected_sources,
        excluded_meeting,
    )?;
    let (_, exists_now) = state.db.dashboard_context_state(&provenance.dashboard_id)?;
    if !exists_now
        || current.witness.generation != provenance.generation
        || current.witness.input_digest != provenance.input_digest
    {
        return Err(AppError::Locked("conversation is unavailable".into()));
    }
    Ok(())
}

/// Bounded, exact-scope, newest-first list. Unknown and invisible scopes both return an empty list.
#[tauri::command]
pub fn list_ask_conversations(
    state: State<'_, AppState>,
    scope: AskConversationScope,
) -> Result<Response, AppError> {
    list_ask_conversations_inner(state.inner(), &scope)
}

pub(crate) fn list_ask_conversations_inner(
    state: &AppState,
    scope: &AskConversationScope,
) -> Result<Response, AppError> {
    let _lifecycle = lifecycle_guard(state);
    let config = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?
        .clone();
    let unlocked = unlocked_snapshot(state)?;
    let visible = match capture_durable_scope_under_lifecycle(state, scope) {
        Ok(_) => {
            let ids = state.db.list_ask_conversation_ids(scope, &unlocked)?;
            let mut visible = Vec::new();
            for id in ids {
                let Some(preflight) = state
                    .db
                    .ask_conversation_preflight(scope, &id, &unlocked)?
                else {
                    continue;
                };
                if preflight.dashboard.as_ref().map_or(true, |provenance| {
                    require_persisted_dashboard_provenance_under_lifecycle(
                        state,
                        scope,
                        provenance,
                        &config,
                        &unlocked,
                    )
                    .is_ok()
                }) {
                    if let Some(summary) = state
                        .db
                        .ask_conversation_summary_after_preflight(scope, &id)?
                    {
                        visible.push(summary);
                    }
                }
            }
            visible
        }
        Err(AppError::Locked(_) | AppError::InvalidArg(_)) => Vec::new(),
        Err(error) => return Err(error),
    };
    serde_json::to_string(&visible)
        .map(Response::new)
        .map_err(|_| AppError::Unavailable("conversation response encoding failed".into()))
}

/// Bounded exact-scope load. Unknown, wrong-scope and invisible IDs deliberately share one error.
#[tauri::command]
pub fn load_ask_conversation(
    state: State<'_, AppState>,
    scope: AskConversationScope,
    conversation_id: String,
) -> Result<Response, AppError> {
    load_ask_conversation_inner(state.inner(), &scope, &conversation_id)
}

pub(crate) fn load_ask_conversation_inner(
    state: &AppState,
    scope: &AskConversationScope,
    conversation_id: &str,
) -> Result<Response, AppError> {
    let _lifecycle = lifecycle_guard(state);
    let config = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?
        .clone();
    let unlocked = unlocked_snapshot(state)?;
    match capture_durable_scope_under_lifecycle(state, scope) {
        Ok(_) => {}
        Err(AppError::Locked(_) | AppError::InvalidArg(_)) => {
            return Err(AppError::Locked("conversation is unavailable".into()));
        }
        Err(error) => return Err(error),
    }
    let preflight = state
        .db
        .ask_conversation_preflight(scope, conversation_id, &unlocked)?
        .ok_or_else(|| AppError::Locked("conversation is unavailable".into()))?;
    if let Some(provenance) = preflight.dashboard.as_ref() {
        require_persisted_dashboard_provenance_under_lifecycle(
            state,
            scope,
            provenance,
            &config,
            &unlocked,
        )?;
    }
    let payload = state
        .db
        .load_ask_conversation(scope, conversation_id, &unlocked)?
        .ok_or_else(|| AppError::Locked("conversation is unavailable".into()))?;
    serde_json::to_string(&payload)
        .map(Response::new)
        .map_err(|_| AppError::Unavailable("conversation response encoding failed".into()))
}

/// Durable top-level/note Ask. Canonical history and optional dashboard provenance come from
/// SQLite, never the WebView.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn ask_vault_persisted(
    app: AppHandle,
    state: State<'_, AppState>,
    scope: AskConversationScope,
    question: String,
    conversation_id: Option<String>,
    ask_trace_id: Option<String>,
    explicit_sources: Option<Vec<SourceRef>>,
    dashboard_id: Option<String>,
    // CONTAINER SCOPE (FE `scopeFolderIds`) — narrow retrieval to these Spaces/folders, subtree
    // included, instead of pinning items. Empty/absent ⇒ the unchanged whole-vault behaviour.
    scope_folder_ids: Option<Vec<String>>,
) -> Result<Response, AppError> {
    if !matches!(
        scope,
        AskConversationScope::Vault | AskConversationScope::Note { .. }
    ) {
        return Err(AppError::InvalidArg(
            "conversation scope is unavailable".into(),
        ));
    }
    if question.trim().is_empty() {
        return Err(AppError::InvalidArg("question is empty".into()));
    }
    // An authored-note conversation must be grounded in that exact note. Do not trust the WebView
    // to remember the anchor: inject it backend-side when absent, while preserving any additional
    // user-selected sources.
    let mut explicit_sources = normalized_sources_for_scope(&scope, explicit_sources)?;
    let (
        snapshot,
        history,
        resume_cursor,
        dependency_folders,
        dashboard_id,
        canonical_sources,
        dashboard_witness,
        ask_dispatch,
    ) = {
        let _lifecycle = lifecycle_guard(state.inner());
        let config = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?
            .clone();
        let unlocked = unlocked_snapshot(state.inner())?;
        let ask_dispatch =
            capture_ask_dispatch_snapshot_under_lifecycle(state.inner(), &config)?;
        let snapshot = capture_durable_scope_under_lifecycle(state.inner(), &scope)?;
        let (history, resume_cursor, persisted_dashboard_id, canonical_sources) =
            match conversation_id.as_deref() {
                Some(id) => {
                    let preflight = state
                        .db
                        .ask_conversation_preflight(&scope, id, &unlocked)?
                        .ok_or_else(|| AppError::Locked("conversation is unavailable".into()))?;
                    if preflight.ask_dispatch_generation != ask_dispatch.generation {
                        return Err(AppError::Locked("conversation is unavailable".into()));
                    }
                    let canonical_sources = preflight
                        .dashboard
                        .as_ref()
                        .map(|provenance| provenance.selected_sources.clone());
                    if let Some(provenance) = preflight.dashboard.as_ref() {
                        require_persisted_dashboard_provenance_under_lifecycle(
                            state.inner(),
                            &scope,
                            provenance,
                            &config,
                            &unlocked,
                        )?;
                    }
                    let context = state
                        .db
                        .ask_conversation_context_after_preflight(id, preflight)?;
                    (
                        context.turns,
                        Some(context.cursor),
                        context.dashboard.map(|dashboard| dashboard.dashboard_id),
                        canonical_sources,
                    )
                }
                None => (Vec::new(), None, None, None),
            };
        let dashboard_id = resolve_conversation_dashboard_id(
            conversation_id.is_some(),
            dashboard_id.as_deref(),
            persisted_dashboard_id,
        )?;
        let sources_for_witness = canonical_sources
            .as_deref()
            .or(explicit_sources.as_deref())
            .unwrap_or_default();
        let dashboard_witness = if let Some(id) = dashboard_id.as_deref() {
            let ask_conn = crate::summarize::roles::provider_target(
                crate::summarize::roles::Role::Ask,
                &config,
            )
            .connection;
            Some(
                crate::commands::dashboards::dashboard_composite_context(
                    &state.db,
                    id,
                    &unlocked,
                    crate::summarize::vault_context::budget_for(&ask_conn),
                    sources_for_witness,
                    None,
                )?
                .witness,
            )
        } else {
            None
        };
        let dependencies = state.db.visible_folder_ids(&unlocked)?;
        (
            snapshot,
            history,
            resume_cursor,
            dependencies,
            dashboard_id,
            canonical_sources,
            dashboard_witness,
            ask_dispatch,
        )
    };
    if let Some(canonical_sources) = canonical_sources {
        explicit_sources = (!canonical_sources.is_empty()).then_some(canonical_sources);
    }
    let selected_sources = explicit_sources.clone().unwrap_or_default();
    let result = ask_vault_inner(
        &app,
        state.inner(),
        question.clone(),
        history,
        ask_trace_id,
        explicit_sources,
        None,
        scope_folder_ids,
        dashboard_id.clone(),
        Some(snapshot.clone()),
        dashboard_witness.clone(),
        Some(ask_dispatch.clone()),
    )
    .await?;
    finish_persisted_ask_send_after_await(
        state.inner(),
        &snapshot,
        &ask_dispatch,
        &scope,
        conversation_id.as_deref(),
        resume_cursor,
        &question,
        result.answer,
        &selected_sources,
        result.sources,
        result.citations,
        &dependency_folders,
        dashboard_witness.as_ref(),
    )
}

/// Durable exact-meeting Ask. Canonical history is loaded from SQLite and the successful answer is
/// revalidated + stored under the same lifecycle interval.
#[tauri::command]
pub async fn chat_meeting_persisted(
    app: AppHandle,
    state: State<'_, AppState>,
    meeting_id: String,
    question: String,
    conversation_id: Option<String>,
    explicit_sources: Option<Vec<SourceRef>>,
    dashboard_id: Option<String>,
) -> Result<Response, AppError> {
    if question.trim().is_empty() {
        return Err(AppError::InvalidArg("question is empty".into()));
    }
    let scope = AskConversationScope::Meeting {
        ref_id: meeting_id.clone(),
    };
    let mut explicit_sources = explicit_sources;
    let (
        snapshot,
        history,
        resume_cursor,
        dependency_folders,
        dashboard_id,
        canonical_sources,
        dashboard_witness,
        ask_dispatch,
    ) = {
        let _lifecycle = lifecycle_guard(state.inner());
        let config = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?
            .clone();
        let unlocked = unlocked_snapshot(state.inner())?;
        let ask_dispatch =
            capture_ask_dispatch_snapshot_under_lifecycle(state.inner(), &config)?;
        let snapshot = capture_durable_scope_under_lifecycle(state.inner(), &scope)?;
        let (history, resume_cursor, persisted_dashboard_id, canonical_sources) =
            match conversation_id.as_deref() {
                Some(id) => {
                    let preflight = state
                        .db
                        .ask_conversation_preflight(&scope, id, &unlocked)?
                        .ok_or_else(|| AppError::Locked("conversation is unavailable".into()))?;
                    if preflight.ask_dispatch_generation != ask_dispatch.generation {
                        return Err(AppError::Locked("conversation is unavailable".into()));
                    }
                    let canonical_sources = preflight
                        .dashboard
                        .as_ref()
                        .map(|provenance| provenance.selected_sources.clone());
                    if let Some(provenance) = preflight.dashboard.as_ref() {
                        require_persisted_dashboard_provenance_under_lifecycle(
                            state.inner(),
                            &scope,
                            provenance,
                            &config,
                            &unlocked,
                        )?;
                    }
                    let context = state
                        .db
                        .ask_conversation_context_after_preflight(id, preflight)?;
                    (
                        context.turns,
                        Some(context.cursor),
                        context.dashboard.map(|dashboard| dashboard.dashboard_id),
                        canonical_sources,
                    )
                }
                None => (Vec::new(), None, None, None),
            };
        let dashboard_id = resolve_conversation_dashboard_id(
            conversation_id.is_some(),
            dashboard_id.as_deref(),
            persisted_dashboard_id,
        )?;
        let sources_for_witness = canonical_sources
            .as_deref()
            .or(explicit_sources.as_deref())
            .unwrap_or_default();
        let dashboard_witness = if let Some(id) = dashboard_id.as_deref() {
            let ask_conn = crate::summarize::roles::provider_target(
                crate::summarize::roles::Role::Ask,
                &config,
            )
            .connection;
            let budget = crate::summarize::vault_context::budget_for(&ask_conn)
                .min(crate::summarize::chat::MAX_PINNED_SOURCE_CHARS);
            Some(
                crate::commands::dashboards::dashboard_composite_context(
                    &state.db,
                    id,
                    &unlocked,
                    budget,
                    sources_for_witness,
                    Some(&meeting_id),
                )?
                .witness,
            )
        } else {
            None
        };
        let dependencies = state.db.visible_folder_ids(&unlocked)?;
        (
            snapshot,
            history,
            resume_cursor,
            dependencies,
            dashboard_id,
            canonical_sources,
            dashboard_witness,
            ask_dispatch,
        )
    };
    if let Some(canonical_sources) = canonical_sources {
        explicit_sources = (!canonical_sources.is_empty()).then_some(canonical_sources);
    }
    let selected_sources = explicit_sources.clone().unwrap_or_default();
    let meeting_snapshot = match &snapshot {
        DurableScopeSnapshot::Meeting { snapshot, .. } => snapshot.clone(),
        DurableScopeSnapshot::Vault(_) | DurableScopeSnapshot::Note { .. } => {
            return Err(AppError::InvalidArg(
                "conversation scope is unavailable".into(),
            ));
        }
    };
    let answer = chat_meeting_inner(
        &app,
        state.inner(),
        meeting_id,
        question.clone(),
        history,
        explicit_sources,
        dashboard_id,
        Some(meeting_snapshot),
        dashboard_witness.clone(),
        Some(ask_dispatch.clone()),
    )
    .await?;
    finish_persisted_ask_send_after_await(
        state.inner(),
        &snapshot,
        &ask_dispatch,
        &scope,
        conversation_id.as_deref(),
        resume_cursor,
        &question,
        answer,
        &selected_sources,
        Vec::new(),
        Vec::new(),
        &dependency_folders,
        dashboard_witness.as_ref(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_scope_backend_injects_exact_note_anchor_once() {
        let scope = AskConversationScope::Note {
            ref_id: "note-a".into(),
        };
        let sources = normalized_sources_for_scope(&scope, None).unwrap().unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].kind, crate::links::LinkKind::Note);
        assert_eq!(sources[0].id, "note-a");

        let sources = normalized_sources_for_scope(&scope, Some(sources))
            .unwrap()
            .unwrap();
        assert_eq!(sources.len(), 1, "anchor must not duplicate on resume");
    }

    #[test]
    fn continuation_keeps_dashboard_identity_and_rejects_scope_switch() {
        assert_eq!(
            resolve_conversation_dashboard_id(true, None, Some("board-a".into())).unwrap(),
            Some("board-a".into())
        );
        assert_eq!(
            resolve_conversation_dashboard_id(true, Some("board-a"), Some("board-a".into()),)
                .unwrap(),
            Some("board-a".into())
        );
        assert!(matches!(
            resolve_conversation_dashboard_id(true, Some("board-b"), Some("board-a".into()),),
            Err(AppError::InvalidArg(_))
        ));
    }

    #[test]
    fn durable_and_trace_identifiers_are_distinct_on_the_wire() {
        let result = AskConversationSendResult {
            conversation_id: "durable-id".into(),
            user_message_id: "00000000-0000-4000-8000-000000000001".into(),
            assistant_message_id: "00000000-0000-4000-8000-000000000002".into(),
            answer: "answer".into(),
            sources: Vec::new(),
            citations: Vec::new(),
        };
        let wire = serde_json::to_value(result).unwrap();
        assert_eq!(wire["conversationId"], "durable-id");
        assert_eq!(
            wire["userMessageId"],
            "00000000-0000-4000-8000-000000000001"
        );
        assert_eq!(
            wire["assistantMessageId"],
            "00000000-0000-4000-8000-000000000002"
        );
        assert!(wire.get("askTraceId").is_none());
        assert!(wire.get("conversation_id").is_none());
    }
}
