//! Durable Ask Brain conversation store.
//!
//! V1 conversations are conservatively `globalDerived`: Ask can inject cross-vault memory and its
//! agentic citations are not a complete typed access log. Every visibility reduction purges all v1
//! rows. For defense in depth, each new thread snapshots every folder visible while its context was
//! assembled; a reader hides it if any such dependency is later invisible. Folders already hidden at
//! creation are excluded, so an unrelated pre-existing lock does not disable Ask.

use std::collections::HashSet;

use rusqlite::{params, OptionalExtension};

use crate::error::{AppError, Result};
use crate::storage::db::{map_err, visibility_clause, Db};
use crate::storage::models::{
    AskConversation, AskConversationMessage, AskConversationScope, AskConversationSourceRef,
    AskConversationSummary, DashboardScopeRef, SourceRef, VaultSource,
};

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedAskSourceRef {
    kind: crate::links::LinkKind,
    id: String,
}

pub(crate) const ASK_CONVERSATION_LIST_LIMIT: usize = 50;
pub(crate) const ASK_CONVERSATION_LOAD_LIMIT: usize = 100;
pub(crate) const ASK_CONVERSATION_CONTEXT_TURNS: usize = 12;
pub(crate) const ASK_CONVERSATION_CONTEXT_CHARS: usize = 64_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AskConversationCursor {
    pub(crate) expected_next_ordinal: i64,
    pub(crate) expected_revision: i64,
}

/// Canonical identities committed by one atomic durable Ask exchange.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PersistedAskExchange {
    pub(crate) conversation_id: String,
    pub(crate) user_message_id: String,
    pub(crate) assistant_message_id: String,
}

#[derive(Debug, Clone)]
pub(crate) struct AskConversationContext {
    pub(crate) turns: Vec<crate::storage::models::ChatTurn>,
    pub(crate) cursor: AskConversationCursor,
    pub(crate) dashboard: Option<AskDashboardProvenance>,
}

/// Content-free durable-thread preflight. Dashboard provenance must be revalidated before any
/// message `content` column is hydrated.
#[derive(Debug, Clone)]
pub(crate) struct AskConversationPreflight {
    pub(crate) cursor: AskConversationCursor,
    pub(crate) dashboard: Option<AskDashboardProvenance>,
    pub(crate) ask_dispatch_generation: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AskDashboardProvenance {
    pub(crate) dashboard_id: String,
    pub(crate) generation: i64,
    pub(crate) input_digest: String,
    pub(crate) selected_sources: Vec<SourceRef>,
}

struct ExistingConversationRow {
    scope_kind: String,
    scope_ref: Option<String>,
    provenance_mode: String,
    visibility_generation: i64,
    revision: i64,
    dependencies_ok: bool,
    dashboard_id: Option<String>,
    dashboard_context_generation: Option<i64>,
    dashboard_context_digest: Option<String>,
    ask_dispatch_generation: Option<i64>,
}

fn decode_json<T: serde::de::DeserializeOwned>(value: &str, label: &str) -> Result<T> {
    serde_json::from_str(value).map_err(|e| AppError::Storage(format!("decode {label}: {e}")))
}

impl Db {
    /// Conservative dependency snapshot: every folder visible while context is assembled. A folder
    /// already hidden at creation is deliberately absent, so unrelated locks do not disable Ask.
    pub(crate) fn visible_folder_ids(&self, unlocked: &HashSet<String>) -> Result<Vec<String>> {
        let conn = self.lock();
        let visible = visibility_clause("f", unlocked);
        let mut stmt = conn
            .prepare(&format!(
                "SELECT f.id FROM folders f WHERE {visible} ORDER BY f.id"
            ))
            .map_err(map_err)?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(map_err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_err)?;
        Ok(rows)
    }

    /// Content-free list preflight. Callers validate dashboard provenance before asking for the
    /// title-bearing summary.
    pub(crate) fn list_ask_conversation_ids(
        &self,
        scope: &AskConversationScope,
        unlocked: &HashSet<String>,
    ) -> Result<Vec<String>> {
        let (kind, scope_ref) = scope.storage_parts();
        let conn = self.lock();
        let visible = visibility_clause("f", unlocked);
        let mut stmt = conn
            .prepare(&format!(
                "SELECT c.id FROM ask_conversations c
                  WHERE scope_kind=?1 AND scope_ref IS ?2
                    AND provenance_mode='globalDerived'
                    AND c.visibility_generation=(SELECT visibility_generation FROM ask_history_state WHERE singleton=1)
                    AND typeof(c.ask_dispatch_generation)='integer'
                    AND c.ask_dispatch_generation>=0
                    AND c.ask_dispatch_generation=(SELECT generation FROM ask_dispatch_state WHERE singleton=1
                      AND typeof(generation)='integer' AND generation>=0)
                    AND EXISTS (SELECT 1 FROM ask_conversation_messages m WHERE m.conversation_id=c.id)
                    AND (c.dashboard_id IS NULL OR EXISTS (
                      SELECT 1 FROM dashboards board JOIN dashboard_context_state state ON state.dashboard_id=board.id
                       WHERE board.id=c.dashboard_id AND state.exists_now=1
                         AND state.generation=c.dashboard_context_generation))
                    AND NOT EXISTS (
                      SELECT 1 FROM ask_conversation_dependencies d LEFT JOIN folders f ON f.id=d.dependency_ref
                       WHERE d.conversation_id=c.id AND d.dependency_kind='folder'
                         AND (f.id IS NULL OR NOT {visible}))
                  ORDER BY updated_at DESC, id DESC LIMIT ?3"
            ))
            .map_err(map_err)?;
        let rows = stmt
            .query_map(
                params![kind, scope_ref, ASK_CONVERSATION_LIST_LIMIT as i64],
                |row| row.get(0),
            )
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_err)?;
        Ok(rows)
    }

    pub(crate) fn ask_conversation_summary_after_preflight(
        &self,
        scope: &AskConversationScope,
        id: &str,
    ) -> Result<Option<AskConversationSummary>> {
        let (kind, scope_ref) = scope.storage_parts();
        self.lock()
            .query_row(
                "SELECT title,created_at,updated_at,
                        (SELECT COUNT(*) FROM ask_conversation_messages m WHERE m.conversation_id=c.id)
                   FROM ask_conversations c WHERE c.id=?1 AND scope_kind=?2 AND scope_ref IS ?3
                    AND typeof(c.ask_dispatch_generation)='integer'
                    AND c.ask_dispatch_generation=(SELECT generation FROM ask_dispatch_state WHERE singleton=1
                      AND typeof(generation)='integer' AND generation>=0)",
                params![id, kind, scope_ref],
                |row| {
                    Ok(AskConversationSummary {
                        id: id.to_string(),
                        scope: scope.clone(),
                        title: row.get(0)?,
                        created_at: row.get(1)?,
                        updated_at: row.get(2)?,
                        message_count: row.get::<_, i64>(3)?.max(0) as u32,
                    })
                },
            )
            .optional()
            .map_err(map_err)
    }

    pub(crate) fn load_ask_conversation(
        &self,
        scope: &AskConversationScope,
        conversation_id: &str,
        unlocked: &HashSet<String>,
    ) -> Result<Option<AskConversation>> {
        let (kind, scope_ref) = scope.storage_parts();
        let conn = self.lock();
        let visible = visibility_clause("f", unlocked);
        let thread = conn
            .query_row(
                &format!(
                    "SELECT title, selected_sources_json, dashboard_id, created_at, updated_at
                   FROM ask_conversations c
                  WHERE c.id = ?1 AND c.scope_kind = ?2 AND c.scope_ref IS ?3
                    AND c.provenance_mode = 'globalDerived'
                    AND c.visibility_generation =
                        (SELECT visibility_generation FROM ask_history_state WHERE singleton = 1)
                    AND c.ask_dispatch_generation =
                        (SELECT generation FROM ask_dispatch_state WHERE singleton = 1
                          AND typeof(generation)='integer' AND generation>=0)
                    AND typeof(c.ask_dispatch_generation)='integer'
                    AND EXISTS (SELECT 1 FROM ask_conversation_messages m0
                                 WHERE m0.conversation_id = c.id)
                    AND (c.dashboard_id IS NULL OR EXISTS (
                      SELECT 1 FROM dashboards board
                      JOIN dashboard_context_state state ON state.dashboard_id = board.id
                       WHERE board.id = c.dashboard_id AND state.exists_now = 1
                         AND state.generation = c.dashboard_context_generation
                    ))
                    AND NOT EXISTS (
                      SELECT 1 FROM ask_conversation_dependencies d
                      LEFT JOIN folders f ON f.id = d.dependency_ref
                       WHERE d.conversation_id = c.id
                         AND d.dependency_kind = 'folder'
                         AND (f.id IS NULL OR NOT {visible})
                    )"
                ),
                params![conversation_id, kind, scope_ref],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()
            .map_err(map_err)?;
        let Some((title, selected_sources_json, dashboard_id, created_at, updated_at)) = thread
        else {
            return Ok(None);
        };
        let mut stmt = conn
            .prepare(
                "SELECT id, ordinal, role, content, sources_json, citations_json, created_at
                   FROM (
                     SELECT id, ordinal, role, content, sources_json, citations_json, created_at
                       FROM ask_conversation_messages
                      WHERE conversation_id = ?1
                        AND EXISTS (SELECT 1 FROM ask_conversations c
                          WHERE c.id=conversation_id AND c.ask_dispatch_generation=
                            (SELECT generation FROM ask_dispatch_state WHERE singleton=1
                              AND typeof(generation)='integer' AND generation>=0)
                            AND typeof(c.ask_dispatch_generation)='integer')
                      ORDER BY ordinal DESC LIMIT ?2
                   ) newest
                  ORDER BY ordinal ASC",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(
                params![conversation_id, ASK_CONVERSATION_LOAD_LIMIT as i64],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, String>(5)?,
                        r.get::<_, String>(6)?,
                    ))
                },
            )
            .map_err(map_err)?;
        let raw_messages = rows
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_err)?;
        drop(stmt);
        drop(conn);

        let persisted_selected: Vec<PersistedAskSourceRef> =
            decode_json(&selected_sources_json, "Ask selected sources")?;
        let mut selected_sources = Vec::new();
        for source in persisted_selected {
            if let Some(title) =
                self.link_endpoint_title_visible(source.kind, &source.id, unlocked)?
            {
                selected_sources.push(AskConversationSourceRef {
                    kind: source.kind,
                    id: source.id,
                    title,
                });
            }
        }
        let dashboard = match dashboard_id.as_deref() {
            Some(id) => self.get_dashboard(id)?.map(|dashboard| DashboardScopeRef {
                id: dashboard.id,
                title: dashboard.title,
                emoji: dashboard.emoji,
            }),
            None => None,
        };
        let mut messages = Vec::new();
        for (id, ordinal, role, content, sources_json, citations_json, created_at) in raw_messages {
            messages.push(AskConversationMessage {
                id,
                ordinal: ordinal.max(0) as u32,
                role,
                content,
                sources: decode_json(&sources_json, "Ask message sources")?,
                citations: decode_json(&citations_json, "Ask message citations")?,
                created_at,
            });
        }
        Ok(Some(AskConversation {
            id: conversation_id.to_string(),
            scope: scope.clone(),
            title,
            selected_sources,
            dashboard,
            messages,
            created_at,
            updated_at,
        }))
    }

    pub(crate) fn ask_conversation_preflight(
        &self,
        scope: &AskConversationScope,
        conversation_id: &str,
        unlocked: &HashSet<String>,
    ) -> Result<Option<AskConversationPreflight>> {
        let (kind, scope_ref) = scope.storage_parts();
        let conn = self.lock();
        let visible = visibility_clause("f", unlocked);
        let exists = conn
            .query_row(
                &format!(
                    "SELECT c.revision, c.dashboard_id, c.dashboard_context_generation,
                            c.dashboard_context_digest, c.selected_sources_json,
                            c.ask_dispatch_generation
                       FROM ask_conversations c
                  WHERE c.id = ?1 AND c.scope_kind = ?2 AND c.scope_ref IS ?3
                    AND c.provenance_mode = 'globalDerived'
                    AND c.visibility_generation =
                        (SELECT visibility_generation FROM ask_history_state WHERE singleton = 1)
                    AND c.ask_dispatch_generation =
                        (SELECT generation FROM ask_dispatch_state WHERE singleton = 1
                          AND typeof(generation)='integer' AND generation>=0)
                    AND typeof(c.ask_dispatch_generation)='integer'
                    AND EXISTS (SELECT 1 FROM ask_conversation_messages m0
                                 WHERE m0.conversation_id = c.id)
                    AND (c.dashboard_id IS NULL OR EXISTS (
                      SELECT 1 FROM dashboards board
                      JOIN dashboard_context_state state ON state.dashboard_id = board.id
                       WHERE board.id = c.dashboard_id AND state.exists_now = 1
                         AND state.generation = c.dashboard_context_generation
                    ))
                    AND NOT EXISTS (
                      SELECT 1 FROM ask_conversation_dependencies d
                      LEFT JOIN folders f ON f.id = d.dependency_ref
                       WHERE d.conversation_id = c.id
                         AND d.dependency_kind = 'folder'
                         AND (f.id IS NULL OR NOT {visible})
                    )"
                ),
                params![conversation_id, kind, scope_ref],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5)?,
                    ))
                },
            )
            .optional()
            .map_err(map_err)?;
        let Some((
            expected_revision,
            dashboard_id,
            dashboard_generation,
            dashboard_digest,
            selected_json,
            ask_dispatch_generation,
        )) = exists
        else {
            return Ok(None);
        };
        let dashboard = match (dashboard_id, dashboard_generation, dashboard_digest) {
            (Some(dashboard_id), Some(generation), Some(input_digest)) => {
                let persisted: Vec<PersistedAskSourceRef> =
                    decode_json(&selected_json, "Ask selected sources")?;
                Some(AskDashboardProvenance {
                    dashboard_id,
                    generation,
                    input_digest,
                    selected_sources: persisted
                        .into_iter()
                        .map(|source| SourceRef {
                            kind: source.kind,
                            id: source.id,
                        })
                        .collect(),
                })
            }
            (None, None, None) => None,
            _ => return Ok(None),
        };
        let expected_next_ordinal = conn
            .query_row(
                "SELECT COALESCE(MAX(ordinal) + 1, 0)
                   FROM ask_conversation_messages WHERE conversation_id = ?1",
                params![conversation_id],
                |row| row.get::<_, i64>(0),
            )
            .map_err(map_err)?;
        Ok(Some(AskConversationPreflight {
            cursor: AskConversationCursor {
                expected_next_ordinal,
                expected_revision,
            },
            dashboard,
            ask_dispatch_generation,
        }))
    }

    /// Hydrate canonical bounded prompt history only after the caller validated preflight
    /// provenance under the current lifecycle guard.
    pub(crate) fn ask_conversation_context_after_preflight(
        &self,
        conversation_id: &str,
        preflight: AskConversationPreflight,
    ) -> Result<AskConversationContext> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT m.role, m.content FROM ask_conversation_messages m
                  JOIN ask_conversations c ON c.id=m.conversation_id
                  WHERE m.conversation_id = ?1
                    AND c.ask_dispatch_generation=?3
                    AND c.ask_dispatch_generation=
                        (SELECT generation FROM ask_dispatch_state WHERE singleton=1
                          AND typeof(generation)='integer' AND generation>=0)
                    AND typeof(c.ask_dispatch_generation)='integer'
                  ORDER BY m.ordinal DESC LIMIT ?2",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(
                params![
                    conversation_id,
                    ASK_CONVERSATION_CONTEXT_TURNS as i64,
                    preflight.ask_dispatch_generation
                ],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .map_err(map_err)?;
        let mut newest_first = rows
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_err)?;
        newest_first.reverse();
        let mut remaining = ASK_CONVERSATION_CONTEXT_CHARS;
        let mut bounded_newest_first = Vec::new();
        for (role, content) in newest_first.into_iter().rev() {
            if remaining == 0 {
                break;
            }
            let bounded: String = content.chars().take(remaining).collect();
            remaining = remaining.saturating_sub(bounded.chars().count());
            bounded_newest_first.push(crate::storage::models::ChatTurn {
                role,
                content: bounded,
            });
        }
        bounded_newest_first.reverse();
        Ok(AskConversationContext {
            turns: bounded_newest_first,
            cursor: preflight.cursor,
            dashboard: preflight.dashboard,
        })
    }

    /// Test-fixture convenience for non-dashboard conversations.
    #[cfg(test)]
    pub(crate) fn ask_conversation_context(
        &self,
        scope: &AskConversationScope,
        conversation_id: &str,
        unlocked: &HashSet<String>,
    ) -> Result<Option<AskConversationContext>> {
        let Some(preflight) = self.ask_conversation_preflight(scope, conversation_id, unlocked)?
        else {
            return Ok(None);
        };
        self.ask_conversation_context_after_preflight(conversation_id, preflight)
            .map(Some)
    }

    /// Atomically create-or-continue a conversation and append exactly one user/assistant pair.
    /// The backend generates the UUID; provider failures never call this method, so no orphan user
    /// row or empty conversation can exist.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn persist_ask_exchange_cas_with_dispatch(
        &self,
        scope: &AskConversationScope,
        conversation_id: Option<&str>,
        resume_cursor: Option<AskConversationCursor>,
        question: &str,
        answer: &str,
        selected_sources: &[SourceRef],
        assistant_sources: &[VaultSource],
        assistant_citations: &[String],
        visible_folder_ids: &[String],
        dashboard_id: Option<&str>,
        dashboard_context_generation: Option<i64>,
        dashboard_context_digest: Option<&str>,
        expected_ask_dispatch_generation: i64,
        now: &str,
    ) -> Result<PersistedAskExchange> {
        let provenance_fields = usize::from(dashboard_id.is_some())
            + usize::from(dashboard_context_generation.is_some())
            + usize::from(dashboard_context_digest.is_some());
        if provenance_fields != 0 && provenance_fields != 3 {
            return Err(AppError::InvalidArg(
                "dashboard provenance must be complete".into(),
            ));
        }
        let question = question.trim();
        let answer = answer.trim();
        if question.is_empty() || answer.is_empty() {
            return Err(AppError::InvalidArg(
                "conversation exchange must contain a question and answer".into(),
            ));
        }
        let persisted_selected = selected_sources
            .iter()
            .map(|source| PersistedAskSourceRef {
                kind: source.kind,
                id: source.id.clone(),
            })
            .collect::<Vec<_>>();
        let selected_sources_json = serde_json::to_string(&persisted_selected)
            .map_err(|e| AppError::Storage(format!("serialize selected Ask sources: {e}")))?;
        let assistant_sources_json = serde_json::to_string(assistant_sources)
            .map_err(|e| AppError::Storage(format!("serialize Ask answer sources: {e}")))?;
        let assistant_citations_json = serde_json::to_string(assistant_citations)
            .map_err(|e| AppError::Storage(format!("serialize Ask citations: {e}")))?;
        let (kind, scope_ref) = scope.storage_parts();
        let id = conversation_id
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| uuid::Uuid::new_v4().hyphenated().to_string());
        let user_message_id = uuid::Uuid::new_v4().hyphenated().to_string();
        let assistant_message_id = uuid::Uuid::new_v4().hyphenated().to_string();
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let current_generation = tx
            .query_row(
                "SELECT visibility_generation FROM ask_history_state WHERE singleton = 1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map_err(map_err)?;
        let current_ask_dispatch_generation = tx
            .query_row(
                "SELECT generation FROM ask_dispatch_state WHERE singleton=1
                   AND typeof(generation)='integer' AND generation>=0",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map_err(map_err)?;
        if current_ask_dispatch_generation != expected_ask_dispatch_generation {
            return Err(AppError::Locked(
                "Ask provider changed while generating the answer".into(),
            ));
        }
        let next_ordinal = if conversation_id.is_some() {
            let existing: Option<ExistingConversationRow> = tx
                .query_row(
                    "SELECT c.scope_kind, c.scope_ref, c.provenance_mode,
                            c.visibility_generation, c.revision,
                            NOT EXISTS (SELECT 1 FROM ask_conversation_dependencies d
                                       LEFT JOIN folders f ON f.id=d.dependency_ref
                                      WHERE d.conversation_id = c.id
                                        AND d.dependency_kind='folder'
                                        AND f.id IS NULL),
                            c.dashboard_id, c.dashboard_context_generation,
                            c.dashboard_context_digest, c.ask_dispatch_generation
                       FROM ask_conversations c WHERE c.id = ?1",
                    params![id],
                    |r| {
                        Ok(ExistingConversationRow {
                            scope_kind: r.get(0)?,
                            scope_ref: r.get(1)?,
                            provenance_mode: r.get(2)?,
                            visibility_generation: r.get(3)?,
                            revision: r.get(4)?,
                            dependencies_ok: r.get(5)?,
                            dashboard_id: r.get(6)?,
                            dashboard_context_generation: r.get(7)?,
                            dashboard_context_digest: r.get(8)?,
                            ask_dispatch_generation: r.get(9)?,
                        })
                    },
                )
                .optional()
                .map_err(map_err)?;
            let Some(existing) = existing
            else {
                return Err(AppError::InvalidArg("conversation is unavailable".into()));
            };
            if existing.scope_kind != kind
                || existing.scope_ref.as_deref() != scope_ref
                || existing.provenance_mode != "globalDerived"
                || existing.visibility_generation != current_generation
                || !existing.dependencies_ok
                || existing.dashboard_id.as_deref() != dashboard_id
                || existing.dashboard_context_generation != dashboard_context_generation
                || existing.dashboard_context_digest.as_deref() != dashboard_context_digest
                || existing.ask_dispatch_generation != Some(expected_ask_dispatch_generation)
            {
                return Err(AppError::InvalidArg("conversation is unavailable".into()));
            }
            let next = tx
                .query_row(
                    "SELECT COALESCE(MAX(ordinal) + 1, 0)
                   FROM ask_conversation_messages WHERE conversation_id = ?1",
                    params![id],
                    |r| r.get::<_, i64>(0),
                )
                .map_err(map_err)?;
            if resume_cursor
                != Some(AskConversationCursor {
                    expected_next_ordinal: next,
                    expected_revision: existing.revision,
                })
            {
                return Err(AppError::InvalidArg(
                    "conversation changed; reload before sending".into(),
                ));
            }
            next
        } else {
            if resume_cursor.is_some() {
                return Err(AppError::InvalidArg(
                    "new conversation cannot carry a resume cursor".into(),
                ));
            }
            let title = ask_conversation_title(question);
            tx.execute(
                "INSERT INTO ask_conversations
                    (id, scope_kind, scope_ref, title, selected_sources_json, dashboard_id,
                     dashboard_context_generation, dashboard_context_digest,
                     ask_dispatch_generation, provenance_mode, visibility_generation, revision,
                     created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'globalDerived', ?10, 1, ?11, ?11)",
                params![
                    id,
                    kind,
                    scope_ref,
                    title,
                    selected_sources_json,
                    dashboard_id,
                    dashboard_context_generation,
                    dashboard_context_digest,
                    expected_ask_dispatch_generation,
                    current_generation,
                    now,
                ],
            )
            .map_err(map_err)?;
            0
        };
        // A resumed turn may see folders that were hidden when the thread was created and have
        // since been session-unlocked. Union the current conservative dependency snapshot so a
        // later relock cannot be missed by the defensive reader.
        for folder_id in visible_folder_ids {
            tx.execute(
                "INSERT OR IGNORE INTO ask_conversation_dependencies
                    (conversation_id, dependency_kind, dependency_ref)
                 VALUES (?1, 'folder', ?2)",
                params![id, folder_id],
            )
            .map_err(map_err)?;
        }
        tx.execute(
            "INSERT INTO ask_conversation_messages
                (id, conversation_id, ordinal, role, content, sources_json, citations_json, created_at)
             VALUES (?1, ?2, ?3, 'user', ?4, '[]', '[]', ?5)",
            params![user_message_id, id, next_ordinal, question, now],
        )
        .map_err(map_err)?;
        tx.execute(
            "INSERT INTO ask_conversation_messages
                (id, conversation_id, ordinal, role, content, sources_json, citations_json, created_at)
             VALUES (?1, ?2, ?3, 'assistant', ?4, ?5, ?6, ?7)",
            params![
                assistant_message_id,
                id,
                next_ordinal + 1,
                answer,
                assistant_sources_json,
                assistant_citations_json,
                now
            ],
        )
        .map_err(map_err)?;
        if let Some(cursor) = resume_cursor {
            let updated = tx
                .execute(
                    "UPDATE ask_conversations
                        SET selected_sources_json = ?2, updated_at = ?3, revision = revision + 1
                      WHERE id = ?1 AND revision = ?4 AND visibility_generation = ?5",
                    params![
                        id,
                        selected_sources_json,
                        now,
                        cursor.expected_revision,
                        current_generation
                    ],
                )
                .map_err(map_err)?;
            if updated != 1 {
                return Err(AppError::InvalidArg(
                    "conversation changed; reload before sending".into(),
                ));
            }
        }
        tx.commit().map_err(map_err)?;
        Ok(PersistedAskExchange {
            conversation_id: id,
            user_message_id,
            assistant_message_id,
        })
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn persist_ask_exchange_cas(
        &self,
        scope: &AskConversationScope,
        conversation_id: Option<&str>,
        resume_cursor: Option<AskConversationCursor>,
        question: &str,
        answer: &str,
        selected_sources: &[SourceRef],
        assistant_sources: &[VaultSource],
        assistant_citations: &[String],
        visible_folder_ids: &[String],
        dashboard_id: Option<&str>,
        dashboard_context_generation: Option<i64>,
        dashboard_context_digest: Option<&str>,
        now: &str,
    ) -> Result<PersistedAskExchange> {
        self.persist_ask_exchange_cas_with_dispatch(
            scope,
            conversation_id,
            resume_cursor,
            question,
            answer,
            selected_sources,
            assistant_sources,
            assistant_citations,
            visible_folder_ids,
            dashboard_id,
            dashboard_context_generation,
            dashboard_context_digest,
            self.ask_dispatch_generation()?,
            now,
        )
    }

    /// Test-fixture convenience only. Production durable sends must carry the cursor returned by
    /// `ask_conversation_context` into `persist_ask_exchange_cas`; this helper snapshots it
    /// immediately and therefore cannot model a provider-await race.
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn persist_ask_exchange(
        &self,
        scope: &AskConversationScope,
        conversation_id: Option<&str>,
        question: &str,
        answer: &str,
        selected_sources: &[SourceRef],
        assistant_sources: &[VaultSource],
        assistant_citations: &[String],
        visible_folder_ids: &[String],
        now: &str,
    ) -> Result<String> {
        let cursor = match conversation_id {
            Some(id) => self
                .ask_conversation_context(scope, id, &HashSet::new())?
                .map(|context| context.cursor),
            None => None,
        };
        self.persist_ask_exchange_cas_with_dispatch(
            scope,
            conversation_id,
            cursor,
            question,
            answer,
            selected_sources,
            assistant_sources,
            assistant_citations,
            visible_folder_ids,
            None,
            None,
            None,
            self.ask_dispatch_generation()?,
            now,
        )
        .map(|exchange| exchange.conversation_id)
    }

    pub(crate) fn purge_all_ask_conversations(&self) -> Result<usize> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let purged = Self::purge_all_ask_conversations_tx(&tx)?;
        tx.commit().map_err(map_err)?;
        Ok(purged)
    }

    /// Destroy EVERY durable Ask conversation, whatever it was derived from.
    ///
    /// Still correct — and still required — wherever the caller cannot name the folders whose
    /// content is going away: a deleted meeting or note, an org item withdrawn, a fact retracted.
    /// Those callers have no folder set to scope by, and a conversation may paraphrase content
    /// whose id no longer exists, so the global sweep is the only fail-closed option there.
    ///
    /// It is NOT correct for the seal paths — see
    /// [`Self::purge_ask_conversations_for_folders_tx`], which replaced it in those three.
    pub(crate) fn purge_all_ask_conversations_tx(tx: &rusqlite::Transaction<'_>) -> Result<usize> {
        let advanced = tx
            .execute(
                "UPDATE ask_history_state
                    SET visibility_generation = visibility_generation + 1
                  WHERE singleton = 1",
                [],
            )
            .map_err(map_err)?;
        if advanced != 1 {
            return Err(AppError::Storage(
                "Ask history visibility generation is unavailable".into(),
            ));
        }
        tx.execute(
            "DELETE FROM ask_conversations WHERE provenance_mode = 'globalDerived'",
            [],
        )
        .map_err(map_err)
    }

    /// Destroy every durable Ask conversation that could have drawn on one of `folder_ids`, and
    /// leave every other conversation intact AND READABLE.
    ///
    /// This replaced a GLOBAL `DELETE FROM ask_conversations WHERE provenance_mode='globalDerived'`
    /// (2.0 audit). The schema CHECK pins `provenance_mode` to that single value, so the predicate
    /// matched every row: one locked folder anywhere in the vault wiped ALL durable history — on
    /// every lock, every relock (including the screen-share auto-relock) and, via
    /// `reblank_locked_folders_at_rest`, on EVERY app launch. A feature whose headline is that it
    /// persists did not survive a restart.
    ///
    /// Scoping is sound because the dependency snapshot is CONSERVATIVE, not analytical:
    /// `commands/ask_history.rs` records `db.visible_folder_ids(&unlocked)` — every folder that was
    /// READABLE when the exchange was written, not the subset the answer happened to cite. So any
    /// folder whose content could have reached a conversation is in that conversation's dependency
    /// set, and sealing it destroys the conversation. A folder that was not readable could not have
    /// contributed. (This is exactly why the sibling purges here stay GLOBAL: a pending audit
    /// finding may cite a title with no matching id, so it has no dependency set to scope by.)
    ///
    /// The generation still advances, because it is the barrier against a conversation written
    /// concurrently with the seal. Survivors are then carried onto the NEW generation — but only
    /// those that were on the immediately-preceding one, so a row racing this transaction is not
    /// resurrected. Advancing the generation WITHOUT carrying survivors forward would be the same
    /// data loss by another route: `list_ask_conversation_ids` admits a row only while
    /// `c.visibility_generation` equals the current one, so a surviving-but-unreadable row is, to
    /// the user, an erased one.
    pub(crate) fn purge_ask_conversations_for_folders_tx(
        tx: &rusqlite::Transaction<'_>,
        folder_ids: &HashSet<String>,
    ) -> Result<usize> {
        if folder_ids.is_empty() {
            return Ok(0);
        }
        let previous: i64 = tx
            .query_row(
                "SELECT visibility_generation FROM ask_history_state WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .map_err(map_err)?;
        let advanced = tx
            .execute(
                "UPDATE ask_history_state
                    SET visibility_generation = visibility_generation + 1
                  WHERE singleton = 1",
                [],
            )
            .map_err(map_err)?;
        if advanced != 1 {
            return Err(AppError::Storage(
                "Ask history visibility generation is unavailable".into(),
            ));
        }
        // `repeat(..).take(n)` rather than `repeat_n`: the latter is stable only since 1.82 and
        // this crate's MSRV is 1.77, which `clippy::incompatible_msrv` enforces as a hard error.
        let placeholders = std::iter::repeat("?")
            .take(folder_ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let deleted = tx
            .execute(
                &format!(
                    "DELETE FROM ask_conversations
                      WHERE provenance_mode = 'globalDerived'
                        AND id IN (SELECT d.conversation_id
                                     FROM ask_conversation_dependencies d
                                    WHERE d.dependency_kind = 'folder'
                                      AND d.dependency_ref IN ({placeholders}))"
                ),
                rusqlite::params_from_iter(folder_ids.iter()),
            )
            .map_err(map_err)?;
        tx.execute(
            "UPDATE ask_conversations
                SET visibility_generation =
                      (SELECT visibility_generation FROM ask_history_state WHERE singleton = 1)
              WHERE provenance_mode = 'globalDerived'
                AND visibility_generation = ?1",
            rusqlite::params![previous],
        )
        .map_err(map_err)?;
        Ok(deleted)
    }

    /// Purge scoped when the caller could name the folders, globally when it could not.
    ///
    /// `None` is NOT "nothing to do" — it is "I cannot name the scope", and the only fail-closed
    /// answer there is still the global sweep. Keeping that decision in one place is what stops a
    /// caller quietly passing an empty set and silently purging nothing.
    pub(crate) fn purge_ask_conversations_for_scope_tx(
        tx: &rusqlite::Transaction<'_>,
        scope: Option<&HashSet<String>>,
    ) -> Result<usize> {
        match scope {
            Some(folders) => Self::purge_ask_conversations_for_folders_tx(tx, folders),
            None => Self::purge_all_ask_conversations_tx(tx),
        }
    }

    /// Every folder these meetings live in, or `None` if ANY of them is unfiled.
    ///
    /// Unfiled content has no folder row, so `visible_folder_ids` never records a dependency naming
    /// it, so a conversation that drew on an unfiled meeting cannot be matched by folder. That is
    /// exactly the case the global sweep exists for — see
    /// [`Self::purge_ask_conversations_for_scope_tx`].
    pub(crate) fn ask_scope_for_meetings_tx(
        tx: &rusqlite::Transaction<'_>,
        meeting_ids: &[String],
    ) -> Result<Option<HashSet<String>>> {
        if meeting_ids.is_empty() {
            return Ok(Some(HashSet::new()));
        }
        let placeholders = std::iter::repeat("?")
            .take(meeting_ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let mut stmt = tx
            .prepare(&format!(
                "SELECT folder_id FROM meetings WHERE id IN ({placeholders})"
            ))
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(meeting_ids.iter()), |row| {
                row.get::<_, Option<String>>(0)
            })
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_err)?;
        let mut folders = HashSet::new();
        for row in rows {
            match row {
                Some(folder) => {
                    folders.insert(folder);
                }
                // One unfiled meeting is enough to make the whole batch unnameable.
                None => return Ok(None),
            }
        }
        Ok(Some(folders))
    }

    /// Every folder these documents live in. `documents.folder_id` is `NOT NULL`, so unlike
    /// meetings this is always nameable — but it is still written to fail closed if that ever
    /// changes.
    pub(crate) fn ask_scope_for_documents_tx(
        tx: &rusqlite::Transaction<'_>,
        document_ids: &[String],
    ) -> Result<Option<HashSet<String>>> {
        if document_ids.is_empty() {
            return Ok(Some(HashSet::new()));
        }
        let placeholders = std::iter::repeat("?")
            .take(document_ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let mut stmt = tx
            .prepare(&format!(
                "SELECT folder_id FROM documents WHERE id IN ({placeholders})"
            ))
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(document_ids.iter()), |row| {
                row.get::<_, Option<String>>(0)
            })
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_err)?;
        let mut folders = HashSet::new();
        for row in rows {
            match row {
                Some(folder) => {
                    folders.insert(folder);
                }
                None => return Ok(None),
            }
        }
        Ok(Some(folders))
    }

    /// A folder and every folder beneath it.
    ///
    /// Descendants belong in the scope because their content goes away with the parent — a child
    /// folder's documents cascade off `documents.folder_id`. Scoping to the named folder alone
    /// would leave a conversation that drew on a grandchild's content readable after the content
    /// itself was gone.
    pub(crate) fn ask_scope_for_folder_tree_tx(
        tx: &rusqlite::Transaction<'_>,
        folder_id: &str,
    ) -> Result<HashSet<String>> {
        let mut stmt = tx
            .prepare(
                "WITH RECURSIVE tree(id) AS (
                     SELECT ?1
                     UNION
                     SELECT f.id FROM folders f JOIN tree t ON f.parent_id = t.id
                 )
                 SELECT id FROM tree",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![folder_id], |row| row.get::<_, String>(0))
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_err)?;
        Ok(rows.into_iter().collect())
    }

    /// The [`Self::purge_ask_conversations_for_folders_tx`] scope for a startup reconcile: every
    /// folder that is sealed AT REST. Nothing was derived from a folder while it was sealed, so a
    /// conversation that never saw one of these is not the reconcile's business.
    pub(crate) fn purge_ask_conversations_for_locked_folders_tx(
        tx: &rusqlite::Transaction<'_>,
    ) -> Result<usize> {
        let locked: HashSet<String> = {
            let mut stmt = tx
                .prepare("SELECT id FROM folders WHERE locked = 1")
                .map_err(map_err)?;
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(map_err)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(map_err)?;
            rows.into_iter().collect()
        };
        Self::purge_ask_conversations_for_folders_tx(tx, &locked)
    }
}

/// Deterministic, Unicode scalar-safe title derived from the first question.
pub(crate) fn ask_conversation_title(question: &str) -> String {
    const MAX_CHARS: usize = 56;
    let normalized = question.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = normalized.chars();
    let title: String = chars.by_ref().take(MAX_CHARS).collect();
    if chars.next().is_some() {
        format!("{}…", title.trim_end())
    } else {
        title
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::links::LinkKind;

    const TEST_DEK: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn db(label: &str) -> Db {
        let path = std::env::temp_dir().join(format!(
            "murmur-ask-history-{label}-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        Db::open_with_key(&path, TEST_DEK).expect("test db opens")
    }

    fn save(db: &Db, scope: &AskConversationScope, id: Option<&str>, at: &str) -> String {
        db.persist_ask_exchange(
            scope,
            id,
            "What changed?",
            "The launch date changed.",
            &[],
            &[],
            &[],
            &[],
            at,
        )
        .expect("exchange persists")
    }

    #[test]
    fn migration_is_additive_idempotent_and_creates_normalized_tables() {
        let db = db("migration");
        db.migrate().expect("second migrate is idempotent");
        db.migrate().expect("third migrate is idempotent");
        let conn = db.lock();
        for table in [
            "ask_history_state",
            "ask_conversations",
            "ask_conversation_messages",
            "ask_conversation_dependencies",
        ] {
            let exists: bool = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
                    params![table],
                    |r| r.get(0),
                )
                .expect("schema query");
            assert!(exists, "missing {table}");
        }
        let columns = conn
            .prepare("PRAGMA table_info(ask_conversations)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert!(columns
            .iter()
            .any(|column| column == "visibility_generation"));
        assert!(columns.iter().any(|column| column == "revision"));
        assert_eq!(
            conn.query_row(
                "SELECT visibility_generation FROM ask_history_state WHERE singleton=1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
            0
        );
    }

    #[test]
    fn first_exchange_is_atomic_uuid_pair_and_scope_isolated() {
        let db = db("atomic");
        let vault = AskConversationScope::Vault;
        let note = AskConversationScope::Note {
            ref_id: "note-a".into(),
        };
        let id = save(&db, &vault, None, "2026-08-06T10:00:00Z");
        assert_eq!(uuid::Uuid::parse_str(&id).unwrap().to_string(), id);
        let unlocked = HashSet::new();
        let loaded = db
            .load_ask_conversation(&vault, &id, &unlocked)
            .unwrap()
            .unwrap();
        assert_eq!(loaded.messages.len(), 2);
        assert_eq!(loaded.messages[0].role, "user");
        assert_eq!(loaded.messages[1].role, "assistant");
        let message_ids = loaded
            .messages
            .iter()
            .map(|message| {
                uuid::Uuid::parse_str(&message.id)
                    .expect("message id must be a backend-minted UUID")
                    .to_string()
            })
            .collect::<Vec<_>>();
        assert_ne!(message_ids[0], message_ids[1]);
        let wire = serde_json::to_value(&loaded).unwrap();
        assert!(
            wire["messages"][0]["id"].is_string() && wire["messages"][1]["id"].is_string(),
            "canonical message ids must stay UUID strings on the IPC wire"
        );
        assert!(db
            .load_ask_conversation(&note, &id, &unlocked)
            .unwrap()
            .is_none());

        {
            let conn = db.lock();
            let orphan = conn
                .execute(
                    "INSERT INTO ask_conversation_messages
                       (id,conversation_id,ordinal,role,content,sources_json,citations_json,created_at)
                     VALUES ('00000000-0000-4000-8000-000000000099','missing',0,'user','x','[]','[]','now')",
                    [],
                )
                .expect_err("FK must reject orphan");
            assert!(orphan.to_string().contains("FOREIGN KEY"));
        }

        let failure = db.persist_ask_exchange(
            &AskConversationScope::Meeting {
                ref_id: "m-failed".into(),
            },
            None,
            "question",
            "   ",
            &[],
            &[],
            &[],
            &[],
            "now",
        );
        assert!(matches!(failure, Err(AppError::InvalidArg(_))));
        let failed_rows: i64 = db
            .lock()
            .query_row(
                "SELECT COUNT(*) FROM ask_conversations WHERE scope_ref='m-failed'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(failed_rows, 0, "failed answer creates no orphan thread");
    }

    #[test]
    fn stale_resume_cursor_cannot_semantically_fork_a_conversation() {
        let db = db("resume-cas");
        let scope = AskConversationScope::Vault;
        let id = save(&db, &scope, None, "2026-08-06T10:00:00Z");
        let first_mount = db
            .ask_conversation_context(&scope, &id, &HashSet::new())
            .unwrap()
            .unwrap();
        let second_mount = first_mount.clone();
        assert_eq!(first_mount.cursor.expected_next_ordinal, 2);
        assert_eq!(first_mount.cursor.expected_revision, 1);

        db.persist_ask_exchange_cas(
            &scope,
            Some(&id),
            Some(first_mount.cursor),
            "first concurrent question",
            "first concurrent answer",
            &[],
            &[],
            &[],
            &[],
            None,
            None,
            None,
            "2026-08-06T10:01:00Z",
        )
        .unwrap();
        let stale = db.persist_ask_exchange_cas(
            &scope,
            Some(&id),
            Some(second_mount.cursor),
            "stale concurrent question",
            "stale concurrent answer",
            &[],
            &[],
            &[],
            &[],
            None,
            None,
            None,
            "2026-08-06T10:01:01Z",
        );
        assert!(matches!(stale, Err(AppError::InvalidArg(_))), "{stale:?}");

        let loaded = db
            .load_ask_conversation(&scope, &id, &HashSet::new())
            .unwrap()
            .unwrap();
        assert_eq!(
            loaded.messages.len(),
            4,
            "stale pair must roll back atomically"
        );
        assert!(loaded
            .messages
            .iter()
            .any(|message| message.content == "first concurrent question"));
        assert!(!loaded
            .messages
            .iter()
            .any(|message| message.content == "stale concurrent question"));
        let refreshed = db
            .ask_conversation_context(&scope, &id, &HashSet::new())
            .unwrap()
            .unwrap();
        assert_eq!(refreshed.cursor.expected_next_ordinal, 4);
        assert_eq!(refreshed.cursor.expected_revision, 2);
    }

    #[test]
    fn exact_scope_matrix_never_crosses_vault_note_or_meeting_threads() {
        let db = db("scope-matrix");
        let scopes = [
            AskConversationScope::Vault,
            AskConversationScope::Note {
                ref_id: "note-a".into(),
            },
            AskConversationScope::Note {
                ref_id: "note-b".into(),
            },
            AskConversationScope::Meeting {
                ref_id: "meeting-a".into(),
            },
            AskConversationScope::Meeting {
                ref_id: "meeting-b".into(),
            },
        ];
        let ids = scopes
            .iter()
            .enumerate()
            .map(|(index, scope)| save(&db, scope, None, &format!("2026-08-06T12:0{index}:00Z")))
            .collect::<Vec<_>>();
        let unlocked = HashSet::new();

        for (scope_index, scope) in scopes.iter().enumerate() {
            let listed = db.list_ask_conversation_ids(scope, &unlocked).unwrap();
            assert_eq!(listed.len(), 1);
            assert_eq!(listed[0], ids[scope_index]);
            for (id_index, id) in ids.iter().enumerate() {
                let expected = scope_index == id_index;
                assert_eq!(
                    db.load_ask_conversation(scope, id, &unlocked)
                        .unwrap()
                        .is_some(),
                    expected
                );
                assert_eq!(
                    db.ask_conversation_context(scope, id, &unlocked)
                        .unwrap()
                        .is_some(),
                    expected
                );
            }
        }
    }

    #[test]
    fn title_is_whitespace_normalized_unicode_safe_and_char_bounded() {
        let title = ask_conversation_title(&format!("  {}  end  ", "🙂ą".repeat(40)));
        assert!(title.is_char_boundary(title.len()));
        assert!(title.chars().count() <= 57);
        assert!(title.ends_with('…'));
        assert_eq!(
            ask_conversation_title("  one\n two\tthree "),
            "one two three"
        );
    }

    #[test]
    fn list_order_cap_and_load_keep_the_newest_window() {
        let db = db("bounds");
        let scope = AskConversationScope::Vault;
        let saved = (0..55)
            .map(|index| {
                save(
                    &db,
                    &scope,
                    None,
                    &format!("2026-08-06T10:{index:02}:00Z"),
                )
            })
            .collect::<Vec<_>>();
        let unlocked = HashSet::new();
        let list = db.list_ask_conversation_ids(&scope, &unlocked).unwrap();
        assert_eq!(list.len(), ASK_CONVERSATION_LIST_LIMIT);
        assert_eq!(list[0], saved[54]);
        assert_eq!(list[1], saved[53]);
        assert_eq!(list.last(), Some(&saved[5]));

        let thread = save(&db, &scope, None, "2026-08-06T11:00:00Z");
        for index in 0..55 {
            db.persist_ask_exchange(
                &scope,
                Some(&thread),
                &format!("q-{index}"),
                &format!("a-{index}"),
                &[],
                &[],
                &[],
                &[],
                &format!("2026-08-06T11:{index:02}:00Z"),
            )
            .unwrap();
        }
        let loaded = db
            .load_ask_conversation(&scope, &thread, &unlocked)
            .unwrap()
            .unwrap();
        assert_eq!(loaded.messages.len(), ASK_CONVERSATION_LOAD_LIMIT);
        assert_eq!(loaded.messages.first().unwrap().content, "q-5");
        assert_eq!(loaded.messages.last().unwrap().content, "a-54");
        let context = db
            .ask_conversation_context(&scope, &thread, &unlocked)
            .unwrap()
            .unwrap();
        assert_eq!(context.turns.len(), ASK_CONVERSATION_CONTEXT_TURNS);
        assert_eq!(context.turns.first().unwrap().content, "q-49");
        assert_eq!(context.turns.last().unwrap().content, "a-54");
    }

    #[test]
    fn malformed_metadata_fails_closed_instead_of_changing_scope() {
        let db = db("json");
        let scope = AskConversationScope::Vault;
        let id = save(&db, &scope, None, "2026-08-06T10:00:00Z");
        db.lock()
            .execute(
                "UPDATE ask_conversations SET selected_sources_json='not-json' WHERE id=?1",
                params![id],
            )
            .unwrap();
        let error = db
            .load_ask_conversation(&scope, &id, &HashSet::new())
            .expect_err("corrupt metadata must fail closed");
        assert!(matches!(error, AppError::Storage(_)));
    }

    #[test]
    fn hidden_folder_defense_gate_and_empty_folder_lock_purge_global_rows() {
        let db = db("lock");
        let scope = AskConversationScope::Vault;
        save(&db, &scope, None, "2026-08-06T10:00:00Z");
        db.lock()
            .execute(
                "INSERT INTO folders(id,name,path,locked,created_at)
                 VALUES ('f-empty','Empty','Empty',0,'now')",
                [],
            )
            .unwrap();
        db.publish_fresh_folder_lock_and_purge_reminder_derived("f-empty", &[7; 48])
            .unwrap();
        assert!(db
            .list_ask_conversation_ids(&scope, &HashSet::new())
            .unwrap()
            .is_empty());

        let count: i64 = db
            .lock()
            .query_row("SELECT COUNT(*) FROM ask_conversations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "empty-folder lock must purge global chats");
    }

    #[test]
    fn unrelated_folder_hidden_at_creation_does_not_disable_history() {
        let db = db("prelocked");
        db.lock()
            .execute(
                "INSERT INTO folders(id,name,path,locked,created_at)
                 VALUES ('f-hidden','Hidden','Hidden',1,'now')",
                [],
            )
            .unwrap();
        let scope = AskConversationScope::Vault;
        let id = save(&db, &scope, None, "2026-08-06T10:00:00Z");
        let list = db
            .list_ask_conversation_ids(&scope, &HashSet::new())
            .unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0], id);
    }

    #[test]
    fn dependency_gate_hides_a_row_if_future_purge_is_missed() {
        let db = db("dependency-gate");
        db.lock()
            .execute(
                "INSERT INTO folders(id,name,path,locked,created_at)
                 VALUES ('f-visible','Visible','Visible',0,'now')",
                [],
            )
            .unwrap();
        let scope = AskConversationScope::Vault;
        let id = db
            .persist_ask_exchange(
                &scope,
                None,
                "question",
                "answer",
                &[],
                &[],
                &[],
                &["f-visible".into()],
                "2026-08-06T10:00:00Z",
            )
            .unwrap();
        // Deliberately bypass the lifecycle purge to exercise only the defensive reader.
        db.lock()
            .execute("UPDATE folders SET locked=1 WHERE id='f-visible'", [])
            .unwrap();
        assert!(db
            .load_ask_conversation(&scope, &id, &HashSet::new())
            .unwrap()
            .is_none());
    }

    #[test]
    fn advanced_visibility_generation_hides_a_missed_delete_across_restart() {
        let path = std::env::temp_dir().join(format!(
            "murmur-ask-history-generation-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let scope = AskConversationScope::Vault;
        let id = {
            let db = Db::open_with_key(&path, TEST_DEK).unwrap();
            let id = save(&db, &scope, None, "2026-08-06T10:00:00Z");
            // Simulate the crash boundary the generation exists to cover: the revocation marker
            // committed, but a legacy/missed delete left the plaintext row physically present.
            db.lock()
                .execute(
                    "UPDATE ask_history_state SET visibility_generation=visibility_generation+1
                      WHERE singleton=1",
                    [],
                )
                .unwrap();
            id
        };

        let reopened = Db::open_with_key(&path, TEST_DEK).unwrap();
        assert_eq!(
            reopened
                .lock()
                .query_row(
                    "SELECT COUNT(*) FROM ask_conversations WHERE id=?1",
                    params![id],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1,
            "oracle requires the simulated missed-delete row to survive physically"
        );
        assert!(reopened
            .list_ask_conversation_ids(&scope, &HashSet::new())
            .unwrap()
            .is_empty());
        assert!(reopened
            .load_ask_conversation(&scope, &id, &HashSet::new())
            .unwrap()
            .is_none());
        assert!(reopened
            .ask_conversation_context(&scope, &id, &HashSet::new())
            .unwrap()
            .is_none());
    }

    #[test]
    fn resumed_turn_unions_newly_visible_folder_dependency() {
        let db = db("dependency-union");
        db.lock()
            .execute(
                "INSERT INTO folders(id,name,path,locked,created_at)
                 VALUES ('f-later','Later','Later',1,'now')",
                [],
            )
            .unwrap();
        let scope = AskConversationScope::Vault;
        let id = save(&db, &scope, None, "2026-08-06T10:00:00Z");
        // Simulate a session unlock: the folder remains locked at rest but enters the unlocked set.
        let unlocked = HashSet::from(["f-later".to_string()]);
        let deps = db.visible_folder_ids(&unlocked).unwrap();
        db.persist_ask_exchange(
            &scope,
            Some(&id),
            "new context",
            "new answer",
            &[],
            &[],
            &[],
            &deps,
            "2026-08-06T10:01:00Z",
        )
        .unwrap();
        let count: i64 = db
            .lock()
            .query_row(
                "SELECT COUNT(*) FROM ask_conversation_dependencies
                  WHERE conversation_id=?1 AND dependency_ref='f-later'",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
        assert!(db
            .load_ask_conversation(&scope, &id, &HashSet::new())
            .unwrap()
            .is_none());
    }

    #[test]
    fn cascade_removes_messages_and_dependencies() {
        let db = db("cascade");
        let id = save(
            &db,
            &AskConversationScope::Vault,
            None,
            "2026-08-06T10:00:00Z",
        );
        let generation_before: i64 = db
            .lock()
            .query_row(
                "SELECT visibility_generation FROM ask_history_state WHERE singleton=1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        db.purge_all_ask_conversations().unwrap();
        let conn = db.lock();
        assert_eq!(
            conn.query_row(
                "SELECT visibility_generation FROM ask_history_state WHERE singleton=1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
            generation_before + 1,
            "revocation generation advances in the same purge transaction"
        );
        for table in ["ask_conversation_messages", "ask_conversation_dependencies"] {
            let count: i64 = conn
                .query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE conversation_id=?1"),
                    params![id],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(count, 0, "{table} must cascade");
        }
    }

    #[test]
    fn crash_left_zero_message_row_is_hidden_from_every_reader() {
        let db = db("zero-message");
        db.lock()
            .execute(
                "INSERT INTO ask_conversations
                    (id,scope_kind,scope_ref,title,selected_sources_json,provenance_mode,created_at,updated_at)
                 VALUES ('crash-row','vault',NULL,'Invisible','[]','globalDerived','now','now')",
                [],
            )
            .unwrap();
        let scope = AskConversationScope::Vault;
        assert!(db
            .list_ask_conversation_ids(&scope, &HashSet::new())
            .unwrap()
            .is_empty());
        assert!(db
            .load_ask_conversation(&scope, "crash-row", &HashSet::new())
            .unwrap()
            .is_none());
        assert!(db
            .ask_conversation_context(&scope, "crash-row", &HashSet::new())
            .unwrap()
            .is_none());
    }

    #[test]
    fn dto_wire_is_camel_case_and_source_selection_round_trips() {
        let db = db("wire");
        db.lock()
            .execute(
                "INSERT INTO folders(id,name,path,locked,created_at)
                 VALUES ('f-source','Sources','Sources',0,'now')",
                [],
            )
            .unwrap();
        db.insert_document("note-a", "f-source", "Current title", "body", "note", 1)
            .unwrap();
        let scope = AskConversationScope::Note {
            ref_id: "note-a".into(),
        };
        let source = SourceRef {
            kind: LinkKind::Note,
            id: "note-a".into(),
        };
        let id = db
            .persist_ask_exchange(
                &scope,
                None,
                "Question",
                "Answer",
                &[source],
                &[],
                &[],
                &[],
                "2026-08-06T10:00:00Z",
            )
            .unwrap();
        let loaded = db
            .load_ask_conversation(&scope, &id, &HashSet::new())
            .unwrap()
            .unwrap();
        let wire = serde_json::to_value(loaded).unwrap();
        assert_eq!(wire["scope"]["kind"], "note");
        assert_eq!(wire["scope"]["refId"], "note-a");
        assert!(wire.get("selectedSources").is_some());
        assert_eq!(wire["selectedSources"][0]["title"], "Current title");
        assert!(wire["messages"][0].get("createdAt").is_some());
        assert!(wire.get("selected_sources").is_none());

        let persisted: String = db
            .lock()
            .query_row(
                "SELECT selected_sources_json FROM ask_conversations WHERE id=?1",
                params![id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(persisted, r#"[{"kind":"note","id":"note-a"}]"#);

        // Deliberately bypass the normal globalDerived purge to exercise the load DTO's own
        // title gate. The thread remains readable, but the now-hidden source is omitted entirely.
        db.lock()
            .execute("UPDATE folders SET locked=1 WHERE id='f-source'", [])
            .unwrap();
        let loaded = db
            .load_ask_conversation(&scope, &id, &HashSet::new())
            .unwrap()
            .unwrap();
        assert!(loaded.selected_sources.is_empty());
    }

    #[test]
    fn dashboard_history_persists_id_only_and_live_resolves_chrome() {
        let db = db("dashboard-id-only");
        db.insert_dashboard(
            "board-a",
            "Original title",
            Some("🧭"),
            None,
            "2026-08-06T09:00:00Z",
        )
        .unwrap();
        let scope = AskConversationScope::Vault;
        let exchange = db
            .persist_ask_exchange_cas(
                &scope,
                None,
                None,
                "Question",
                "Answer",
                &[],
                &[],
                &[],
                &[],
                Some("board-a"),
                Some(1),
                Some("exact-composite-digest"),
                "2026-08-06T10:00:00Z",
            )
            .unwrap();

        let stored: (Option<String>, String) = db
            .lock()
            .query_row(
                "SELECT dashboard_id, selected_sources_json FROM ask_conversations WHERE id=?1",
                [&exchange.conversation_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(stored.0.as_deref(), Some("board-a"));
        assert_eq!(
            stored.1, "[]",
            "board children are never persisted as sources"
        );

        db.update_dashboard(
            "board-a",
            Some("Live title"),
            Some("✨"),
            None,
            None,
            "2026-08-06T11:00:00Z",
        )
        .unwrap();
        let loaded = db
            .load_ask_conversation(&scope, &exchange.conversation_id, &HashSet::new())
            .unwrap()
            .unwrap();
        let dashboard = loaded.dashboard.expect("live dashboard metadata");
        assert_eq!(dashboard.id, "board-a");
        assert_eq!(dashboard.title, "Live title");
        assert_eq!(dashboard.emoji.as_deref(), Some("✨"));

        db.delete_dashboard("board-a").unwrap();
        assert!(db
            .load_ask_conversation(&scope, &exchange.conversation_id, &HashSet::new())
            .unwrap()
            .is_none());
        assert!(db
            .list_ask_conversation_ids(&scope, &HashSet::new())
            .unwrap()
            .is_empty());

        // ABA: the public id is reusable, but the tombstone generation must keep history from
        // the previous incarnation unavailable through every raw storage reader.
        db.insert_dashboard(
            "board-a",
            "Recreated title",
            Some("🧟"),
            None,
            "2026-08-06T12:00:00Z",
        )
        .unwrap();
        assert!(db
            .load_ask_conversation(&scope, &exchange.conversation_id, &HashSet::new())
            .unwrap()
            .is_none());
        assert!(db
            .ask_conversation_context(&scope, &exchange.conversation_id, &HashSet::new())
            .unwrap()
            .is_none());
        assert!(db
            .list_ask_conversation_ids(&scope, &HashSet::new())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn partial_dashboard_provenance_is_rejected_before_storage() {
        let db = db("dashboard-partial-provenance");
        let error = db
            .persist_ask_exchange_cas(
                &AskConversationScope::Vault,
                None,
                None,
                "Question",
                "Answer",
                &[],
                &[],
                &[],
                &[],
                Some("board-a"),
                None,
                None,
                "2026-08-06T10:00:00Z",
            )
            .unwrap_err();
        assert!(matches!(error, AppError::InvalidArg(_)));
        let count: i64 = db
            .lock()
            .query_row("SELECT COUNT(*) FROM ask_conversations", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn dashboard_preflight_reads_no_message_content_before_exact_validation() {
        let db = db("dashboard-content-preflight");
        db.insert_dashboard("board", "Board", None, None, "2026-08-06T09:00:00Z")
            .unwrap();
        let scope = AskConversationScope::Vault;
        let exchange = db
            .persist_ask_exchange_cas(
                &scope,
                None,
                None,
                "question",
                "answer",
                &[],
                &[],
                &[],
                &[],
                Some("board"),
                Some(1),
                Some("digest"),
                "2026-08-06T10:00:00Z",
            )
            .unwrap();
        db.lock()
            .execute(
                "UPDATE ask_conversation_messages SET content=x'FF' WHERE conversation_id=?1",
                [&exchange.conversation_id],
            )
            .unwrap();
        db.lock()
            .execute(
                "UPDATE ask_conversations SET title=x'01' WHERE id=?1",
                [&exchange.conversation_id],
            )
            .unwrap();

        assert_eq!(
            db.list_ask_conversation_ids(&scope, &HashSet::new())
                .unwrap(),
            vec![exchange.conversation_id.clone()]
        );
        let preflight = db
            .ask_conversation_preflight(&scope, &exchange.conversation_id, &HashSet::new())
            .unwrap()
            .expect("metadata-only preflight must not hydrate poisoned content");
        assert_eq!(preflight.dashboard.unwrap().input_digest, "digest");
        let preflight = db
            .ask_conversation_preflight(&scope, &exchange.conversation_id, &HashSet::new())
            .unwrap()
            .unwrap();
        assert!(matches!(
            db.ask_conversation_context_after_preflight(&exchange.conversation_id, preflight),
            Err(AppError::Storage(_))
        ));
        assert!(matches!(
            db.ask_conversation_summary_after_preflight(&scope, &exchange.conversation_id),
            Err(AppError::Storage(_))
        ));
    }
}
