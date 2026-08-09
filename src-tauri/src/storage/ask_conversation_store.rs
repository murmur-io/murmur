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
    AskConversationSummary, SourceRef, VaultSource,
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

    pub(crate) fn list_ask_conversations(
        &self,
        scope: &AskConversationScope,
        unlocked: &HashSet<String>,
    ) -> Result<Vec<AskConversationSummary>> {
        let (kind, scope_ref) = scope.storage_parts();
        let conn = self.lock();
        let visible = visibility_clause("f", unlocked);
        let mut stmt = conn
            .prepare(&format!(
                "SELECT id, title, created_at, updated_at,
                        (SELECT COUNT(*) FROM ask_conversation_messages m
                          WHERE m.conversation_id = c.id)
                   FROM ask_conversations c
                  WHERE scope_kind = ?1 AND scope_ref IS ?2
                    AND provenance_mode = 'globalDerived'
                    AND c.visibility_generation =
                        (SELECT visibility_generation FROM ask_history_state WHERE singleton = 1)
                    AND EXISTS (SELECT 1 FROM ask_conversation_messages m0
                                 WHERE m0.conversation_id = c.id)
                    AND NOT EXISTS (
                      SELECT 1 FROM ask_conversation_dependencies d
                      LEFT JOIN folders f ON f.id = d.dependency_ref
                       WHERE d.conversation_id = c.id
                         AND d.dependency_kind = 'folder'
                         AND (f.id IS NULL OR NOT {visible})
                    )
                  ORDER BY updated_at DESC, id DESC LIMIT ?3"
            ))
            .map_err(map_err)?;
        let rows = stmt
            .query_map(
                params![kind, scope_ref, ASK_CONVERSATION_LIST_LIMIT as i64],
                |r| {
                    Ok(AskConversationSummary {
                        id: r.get(0)?,
                        scope: scope.clone(),
                        title: r.get(1)?,
                        created_at: r.get(2)?,
                        updated_at: r.get(3)?,
                        message_count: r.get::<_, i64>(4)?.max(0) as u32,
                    })
                },
            )
            .map_err(map_err)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
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
                    "SELECT title, selected_sources_json, created_at, updated_at
                   FROM ask_conversations c
                  WHERE c.id = ?1 AND c.scope_kind = ?2 AND c.scope_ref IS ?3
                    AND c.provenance_mode = 'globalDerived'
                    AND c.visibility_generation =
                        (SELECT visibility_generation FROM ask_history_state WHERE singleton = 1)
                    AND EXISTS (SELECT 1 FROM ask_conversation_messages m0
                                 WHERE m0.conversation_id = c.id)
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
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()
            .map_err(map_err)?;
        let Some((title, selected_sources_json, created_at, updated_at)) = thread else {
            return Ok(None);
        };
        let mut stmt = conn
            .prepare(
                "SELECT id, ordinal, role, content, sources_json, citations_json, created_at
                   FROM (
                     SELECT id, ordinal, role, content, sources_json, citations_json, created_at
                       FROM ask_conversation_messages
                      WHERE conversation_id = ?1
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
            messages,
            created_at,
            updated_at,
        }))
    }

    /// Canonical bounded prompt history. Unknown/wrong-scope/invisible IDs all return `None`.
    pub(crate) fn ask_conversation_context(
        &self,
        scope: &AskConversationScope,
        conversation_id: &str,
        unlocked: &HashSet<String>,
    ) -> Result<Option<AskConversationContext>> {
        let (kind, scope_ref) = scope.storage_parts();
        let conn = self.lock();
        let visible = visibility_clause("f", unlocked);
        let exists = conn
            .query_row(
                &format!(
                    "SELECT c.revision FROM ask_conversations c
                  WHERE c.id = ?1 AND c.scope_kind = ?2 AND c.scope_ref IS ?3
                    AND c.provenance_mode = 'globalDerived'
                    AND c.visibility_generation =
                        (SELECT visibility_generation FROM ask_history_state WHERE singleton = 1)
                    AND EXISTS (SELECT 1 FROM ask_conversation_messages m0
                                 WHERE m0.conversation_id = c.id)
                    AND NOT EXISTS (
                      SELECT 1 FROM ask_conversation_dependencies d
                      LEFT JOIN folders f ON f.id = d.dependency_ref
                       WHERE d.conversation_id = c.id
                         AND d.dependency_kind = 'folder'
                         AND (f.id IS NULL OR NOT {visible})
                    )"
                ),
                params![conversation_id, kind, scope_ref],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(map_err)?;
        let Some(expected_revision) = exists else {
            return Ok(None);
        };
        let expected_next_ordinal = conn
            .query_row(
                "SELECT COALESCE(MAX(ordinal) + 1, 0)
                   FROM ask_conversation_messages WHERE conversation_id = ?1",
                params![conversation_id],
                |row| row.get::<_, i64>(0),
            )
            .map_err(map_err)?;
        let mut stmt = conn
            .prepare(
                "SELECT role, content FROM ask_conversation_messages
                  WHERE conversation_id = ?1 ORDER BY ordinal DESC LIMIT ?2",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(
                params![conversation_id, ASK_CONVERSATION_CONTEXT_TURNS as i64],
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
        Ok(Some(AskConversationContext {
            turns: bounded_newest_first,
            cursor: AskConversationCursor {
                expected_next_ordinal,
                expected_revision,
            },
        }))
    }

    /// Atomically create-or-continue a conversation and append exactly one user/assistant pair.
    /// The backend generates the UUID; provider failures never call this method, so no orphan user
    /// row or empty conversation can exist.
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
        now: &str,
    ) -> Result<PersistedAskExchange> {
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
        let next_ordinal = if conversation_id.is_some() {
            let existing: Option<(String, Option<String>, String, i64, i64, bool)> = tx
                .query_row(
                    "SELECT c.scope_kind, c.scope_ref, c.provenance_mode,
                            c.visibility_generation, c.revision,
                            NOT EXISTS (SELECT 1 FROM ask_conversation_dependencies d
                                       LEFT JOIN folders f ON f.id=d.dependency_ref
                                      WHERE d.conversation_id = c.id
                                        AND d.dependency_kind='folder'
                                        AND f.id IS NULL)
                       FROM ask_conversations c WHERE c.id = ?1",
                    params![id],
                    |r| {
                        Ok((
                            r.get(0)?,
                            r.get(1)?,
                            r.get(2)?,
                            r.get(3)?,
                            r.get(4)?,
                            r.get(5)?,
                        ))
                    },
                )
                .optional()
                .map_err(map_err)?;
            let Some((
                existing_kind,
                existing_ref,
                provenance,
                generation,
                revision,
                dependencies_ok,
            )) = existing
            else {
                return Err(AppError::InvalidArg("conversation is unavailable".into()));
            };
            if existing_kind != kind
                || existing_ref.as_deref() != scope_ref
                || provenance != "globalDerived"
                || generation != current_generation
                || !dependencies_ok
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
                    expected_revision: revision,
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
                    (id, scope_kind, scope_ref, title, selected_sources_json,
                     provenance_mode, visibility_generation, revision, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'globalDerived', ?6, 1, ?7, ?7)",
                params![
                    id,
                    kind,
                    scope_ref,
                    title,
                    selected_sources_json,
                    current_generation,
                    now
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
        self.persist_ask_exchange_cas(
            scope,
            conversation_id,
            cursor,
            question,
            answer,
            selected_sources,
            assistant_sources,
            assistant_citations,
            visible_folder_ids,
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
            let listed = db.list_ask_conversations(scope, &unlocked).unwrap();
            assert_eq!(listed.len(), 1);
            assert_eq!(listed[0].id, ids[scope_index]);
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
        for index in 0..55 {
            save(&db, &scope, None, &format!("2026-08-06T10:{index:02}:00Z"));
        }
        let unlocked = HashSet::new();
        let list = db.list_ask_conversations(&scope, &unlocked).unwrap();
        assert_eq!(list.len(), ASK_CONVERSATION_LIST_LIMIT);
        assert!(list[0].updated_at > list[1].updated_at);

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
            .list_ask_conversations(&scope, &HashSet::new())
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
        let list = db.list_ask_conversations(&scope, &HashSet::new()).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, id);
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
            .list_ask_conversations(&scope, &HashSet::new())
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
            .list_ask_conversations(&scope, &HashSet::new())
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
}
