//! Build a cross-meeting context corpus for "Ask-My-Vault": gather the most relevant past
//! meetings' notes into a provider-budget-capped corpus, each headed by a [[Title]] citation,
//! so the LLM can answer questions across the user's whole history — fully on-device.
//!
//! E9 anti-leak invariant: this corpus is fed straight into a cloud LLM prompt (claude_code /
//! anthropic) by `ask_vault`, so it MUST exclude any sealed-and-not-session-unlocked folder's
//! content. We enforce that at assembly by querying ONLY the visibility-filtered Db methods
//! (`search_visible` / `list_meetings_visible` / `get_note_if_visible`) against the live
//! `unlocked` session set — never the raw `search` / `list_meetings` / `get_latest_note_for_meeting`
//! (which ignore sealing). Relying on the at-rest blanking of sealed plaintext alone is NOT
//! enough: this is the defense-in-depth gate that holds even across a relock race.

use std::collections::HashSet;

use crate::error::Result;
use crate::storage::models::VaultSource;
use crate::storage::Db;

/// Char budget for the corpus, by provider. Local quantized models (Ollama) have tiny
/// default context windows, so cap much tighter; API/Claude models get headroom.
fn budget_for(provider_id: &str) -> usize {
    if provider_id == "ollama" {
        4_000
    } else {
        // Cited Ask: pack toward the model context window so more relevant notes (each kept under its
        // `### [[Title]] · date · id:` header, so the answer can cite [[Title]]) fit. ~200k chars ≈
        // 50k tokens, comfortably inside Claude's 200k-token window with room for the system prompt,
        // chat history, and the answer. Still bounded so a huge vault can't blow the prompt.
        200_000
    }
}

/// Backward-compatible entry point (original 3-arg signature). Delegates to the
/// visibility-aware [`build_vault_context_visible`] with an EMPTY unlock set, i.e. it is
/// **fail-closed**: every sealed folder is treated as not-unlocked, so no sealed content can
/// reach the cloud prompt (E9) even from a caller that hasn't yet been migrated.
///
/// TODO(owner of commands.rs): migrate `ask_vault` / `pre_meeting_brief` to call
/// [`build_vault_context_visible`] with the live `state.unlocked_folders` snapshot, so a folder
/// the user has *session-unlocked* is included again. Until then those flows simply omit
/// session-unlocked folders from Ask-My-Vault — the safe (no-leak) direction.
pub fn build_vault_context(
    db: &Db,
    query: &str,
    provider_id: &str,
) -> Result<(String, Vec<VaultSource>)> {
    build_vault_context_visible(db, query, provider_id, &HashSet::new())
}

/// Returns (corpus, sources). Picks VISIBLE meetings relevant to `query` (full-text search),
/// falling back to the most recent visible ones, and packs their notes until the budget is hit.
///
/// `unlocked` is the live session unlock set: a sealed folder is included ONLY if its id is in
/// this set (i.e. the user session-unlocked it). Sealed-and-not-unlocked content is excluded so
/// it can never reach the cloud prompt (E9).
pub fn build_vault_context_visible(
    db: &Db,
    query: &str,
    provider_id: &str,
    unlocked: &HashSet<String>,
) -> Result<(String, Vec<VaultSource>)> {
    let budget = budget_for(provider_id);

    // Relevance-first: VISIBLE search hits, else the most recent VISIBLE meetings. Both queries
    // apply the sealed-folder visibility clause against `unlocked`, so sealed-not-unlocked
    // meetings are filtered out of the candidate set before any content is read.
    let mut meetings: Vec<crate::storage::models::Meeting> = db
        .search_visible(query, 40, unlocked)?
        .into_iter()
        .map(|h| h.meeting)
        .collect();
    if meetings.is_empty() {
        meetings = db.list_meetings_visible(30, unlocked)?;
    }

    let mut corpus = String::new();
    let mut sources: Vec<VaultSource> = Vec::new();
    for m in meetings {
        if corpus.len() >= budget {
            break;
        }
        // Second gate: pull the note ONLY if it is visible. `get_note_if_visible` returns `None`
        // for a sealed-and-not-unlocked note, so its (possibly-stale-plaintext) content never
        // enters the corpus even if the candidate filter and at-rest blanking both missed it.
        let Some(note) = db.get_note_if_visible(&m.id, unlocked)? else {
            continue;
        };
        let title = m.title.clone().unwrap_or_else(|| "(untitled)".to_string());
        let date = m
            .started_at
            .split(['T', ' '])
            .next()
            .unwrap_or("")
            .to_string();
        let header = format!("\n\n### [[{title}]] · {date} · id:{}\n", m.id);
        let remaining = budget.saturating_sub(corpus.len() + header.len());
        if remaining < 200 {
            break;
        }
        let chunk: String = note.markdown.chars().take(remaining).collect();
        corpus.push_str(&header);
        corpus.push_str(&chunk);
        sources.push(VaultSource {
            meeting_id: m.id,
            title,
            started_at: m.started_at,
        });
    }

    Ok((corpus, sources))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::models::{Folder, Meeting, MeetingStatus, NoteRecord};

    const TEST_DEK: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn temp_db() -> Db {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "meetnotes-vaultctx-test-{}-{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        Db::open_with_key(&p, TEST_DEK).unwrap()
    }

    fn seed_note(db: &Db, meeting_id: &str, title: &str, markdown: &str, folder_id: Option<&str>) {
        db.insert_meeting(&Meeting {
            id: meeting_id.to_string(),
            started_at: "2026-06-26T09:00:00Z".to_string(),
            ended_at: None,
            title: Some(title.to_string()),
            duration_s: 60,
            audio_path: None,
            status: MeetingStatus::Summarized,
            folder_id: None,
        })
        .unwrap();
        db.upsert_note(&NoteRecord {
            meeting_id: meeting_id.to_string(),
            provider_id: "claude_code".to_string(),
            markdown: markdown.to_string(),
            created_at: "2026-06-26T09:05:00Z".to_string(),
            exported_path: Some(format!("/vault/{meeting_id}.md")),
        })
        .unwrap();
        db.set_note_folder(meeting_id, folder_id).unwrap();
    }

    fn seed_folder(db: &Db, id: &str) {
        db.insert_folder(&Folder {
            id: id.to_string(),
            name: id.to_string(),
            path: id.to_string(),
            parent_id: None,
            locked: false,
            created_at: "2026-06-26T00:00:00Z".to_string(),
        })
        .unwrap();
    }

    /// E9: a sealed-and-not-session-unlocked folder's note must NEVER appear in the corpus that
    /// gets packed into a cloud prompt — and must reappear once the folder is session-unlocked.
    #[test]
    fn sealed_folder_content_excluded_until_unlocked() {
        let db = temp_db();
        // Open folder note (always visible) + a folder we will seal.
        seed_note(&db, "open", "Open Meeting", "OPEN-SECRET project Apollo", None);
        seed_folder(&db, "f-locked");
        seed_note(
            &db,
            "sealed",
            "Sealed Meeting",
            "LOCKED-SECRET acquisition price 5_000_000",
            Some("f-locked"),
        );
        // Seal the folder (flip locked=1). visibility_clause keys off folders.locked.
        db.set_folder_locked("f-locked", true, None).unwrap();

        // Fail-closed 3-arg shim: nothing session-unlocked. Sealed content MUST be absent; open
        // present. This is the exact path the (un-migrated) cloud callers use today.
        let (corpus, sources) = build_vault_context(&db, "SECRET", "anthropic").unwrap();
        assert!(
            corpus.contains("OPEN-SECRET"),
            "open-folder note must be included"
        );
        assert!(
            !corpus.contains("LOCKED-SECRET"),
            "sealed-not-unlocked content leaked into the cloud corpus (E9 violation)"
        );
        assert!(sources.iter().all(|s| s.meeting_id != "sealed"));

        // Visibility-aware variant with an empty set agrees with the shim.
        let nothing = HashSet::new();
        let (c0, _) =
            build_vault_context_visible(&db, "SECRET", "anthropic", &nothing).unwrap();
        assert!(!c0.contains("LOCKED-SECRET"));

        // Session-unlock the folder → its content is now legitimately available.
        let mut unlocked = HashSet::new();
        unlocked.insert("f-locked".to_string());
        let (corpus2, sources2) =
            build_vault_context_visible(&db, "SECRET", "anthropic", &unlocked).unwrap();
        assert!(corpus2.contains("LOCKED-SECRET"), "unlocked content must reappear");
        assert!(sources2.iter().any(|s| s.meeting_id == "sealed"));
    }
}
