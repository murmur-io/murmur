//! Durable Ask Brain conversation commands for the three v1 scopes.
//!
//! Org/dashboard/record-time assistant callers keep using their existing stateless commands and
//! `assistant_interactions`; this module is the only durable conversation seam.

use super::*;
use crate::storage::models::{
    AskConversation, AskConversationScope, AskConversationSendResult, AskConversationSummary,
    SourceRef,
};

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

pub(crate) fn require_durable_scope_for_dispatch(
    state: &AppState,
    snapshot: &DurableScopeSnapshot,
) -> Result<(), AppError> {
    let _lifecycle = lifecycle_guard(state);
    require_durable_scope_under_lifecycle(state, snapshot)
}

pub(crate) fn durable_dispatch_admission(
    app: &AppHandle,
    snapshot: DurableScopeSnapshot,
) -> crate::state::ContentDispatchAdmission {
    crate::state::ContentDispatchAdmission::new(app, move |state| {
        require_durable_scope_under_lifecycle(state, &snapshot)
    })
}

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
) -> Result<crate::storage::ask_conversation_store::PersistedAskExchange, AppError> {
    let _lifecycle = lifecycle_guard(state);
    require_durable_scope_under_lifecycle(state, snapshot)?;
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
    state.db.persist_ask_exchange_cas(
        scope,
        conversation_id,
        resume_cursor,
        question,
        answer,
        selected_sources,
        assistant_sources,
        assistant_citations,
        &dependency_folders,
        &chrono::Utc::now().to_rfc3339(),
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

/// Bounded, exact-scope, newest-first list. Unknown and invisible scopes both return an empty list.
#[tauri::command]
pub fn list_ask_conversations(
    state: State<'_, AppState>,
    scope: AskConversationScope,
) -> Result<Vec<AskConversationSummary>, AppError> {
    let _lifecycle = lifecycle_guard(state.inner());
    let unlocked = unlocked_snapshot(state.inner())?;
    match capture_durable_scope_under_lifecycle(state.inner(), &scope) {
        Ok(_) => state.db.list_ask_conversations(&scope, &unlocked),
        Err(AppError::Locked(_) | AppError::InvalidArg(_)) => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}

/// Bounded exact-scope load. Unknown, wrong-scope and invisible IDs deliberately share one error.
#[tauri::command]
pub fn load_ask_conversation(
    state: State<'_, AppState>,
    scope: AskConversationScope,
    conversation_id: String,
) -> Result<AskConversation, AppError> {
    let _lifecycle = lifecycle_guard(state.inner());
    let unlocked = unlocked_snapshot(state.inner())?;
    match capture_durable_scope_under_lifecycle(state.inner(), &scope) {
        Ok(_) => {}
        Err(AppError::Locked(_) | AppError::InvalidArg(_)) => {
            return Err(AppError::Locked("conversation is unavailable".into()));
        }
        Err(error) => return Err(error),
    }
    state
        .db
        .load_ask_conversation(&scope, &conversation_id, &unlocked)?
        .ok_or_else(|| AppError::Locked("conversation is unavailable".into()))
}

/// Durable top-level/note Ask. The scope is restricted to vault or authored note; org and dashboard
/// stay on the legacy stateless command. Canonical history comes from SQLite, never the WebView.
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
) -> Result<AskConversationSendResult, AppError> {
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
    let (snapshot, history, resume_cursor, dependency_folders) = {
        let _lifecycle = lifecycle_guard(state.inner());
        let unlocked = unlocked_snapshot(state.inner())?;
        let snapshot = capture_durable_scope_under_lifecycle(state.inner(), &scope)?;
        let (history, resume_cursor) = match conversation_id.as_deref() {
            Some(id) => {
                let context = state
                    .db
                    .ask_conversation_context(&scope, id, &unlocked)?
                    .ok_or_else(|| AppError::Locked("conversation is unavailable".into()))?;
                (context.turns, Some(context.cursor))
            }
            None => (Vec::new(), None),
        };
        let dependencies = state.db.visible_folder_ids(&unlocked)?;
        (snapshot, history, resume_cursor, dependencies)
    };
    // An authored-note conversation must be grounded in that exact note. Do not trust the WebView
    // to remember the anchor: inject it backend-side when absent, while preserving any additional
    // user-selected sources.
    let explicit_sources = normalized_sources_for_scope(&scope, explicit_sources)?;
    let selected_sources = explicit_sources.clone().unwrap_or_default();
    let result = ask_vault_inner(
        &app,
        state.inner(),
        question.clone(),
        history,
        ask_trace_id,
        explicit_sources,
        None,
        None,
        Some(snapshot.clone()),
    )
    .await?;
    let committed = persist_ask_exchange_after_await(
        state.inner(),
        &snapshot,
        &scope,
        conversation_id.as_deref(),
        resume_cursor,
        &question,
        &result.answer,
        &selected_sources,
        &result.sources,
        &result.citations,
        &dependency_folders,
    )?;
    Ok(AskConversationSendResult {
        conversation_id: committed.conversation_id,
        user_message_id: committed.user_message_id,
        assistant_message_id: committed.assistant_message_id,
        answer: result.answer,
        sources: result.sources,
        citations: result.citations,
    })
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
) -> Result<AskConversationSendResult, AppError> {
    if question.trim().is_empty() {
        return Err(AppError::InvalidArg("question is empty".into()));
    }
    let scope = AskConversationScope::Meeting {
        ref_id: meeting_id.clone(),
    };
    let (snapshot, history, resume_cursor, dependency_folders) = {
        let _lifecycle = lifecycle_guard(state.inner());
        let unlocked = unlocked_snapshot(state.inner())?;
        let snapshot = capture_durable_scope_under_lifecycle(state.inner(), &scope)?;
        let (history, resume_cursor) = match conversation_id.as_deref() {
            Some(id) => {
                let context = state
                    .db
                    .ask_conversation_context(&scope, id, &unlocked)?
                    .ok_or_else(|| AppError::Locked("conversation is unavailable".into()))?;
                (context.turns, Some(context.cursor))
            }
            None => (Vec::new(), None),
        };
        let dependencies = state.db.visible_folder_ids(&unlocked)?;
        (snapshot, history, resume_cursor, dependencies)
    };
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
        Some(meeting_snapshot),
    )
    .await?;
    let committed = persist_ask_exchange_after_await(
        state.inner(),
        &snapshot,
        &scope,
        conversation_id.as_deref(),
        resume_cursor,
        &question,
        &answer,
        &selected_sources,
        &[],
        &[],
        &dependency_folders,
    )?;
    Ok(AskConversationSendResult {
        conversation_id: committed.conversation_id,
        user_message_id: committed.user_message_id,
        assistant_message_id: committed.assistant_message_id,
        answer,
        sources: Vec::new(),
        citations: Vec::new(),
    })
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
