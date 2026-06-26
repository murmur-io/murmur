//! Build a cross-meeting context corpus for "Ask-My-Vault": gather the most relevant past
//! meetings' notes into a provider-budget-capped corpus, each headed by a [[Title]] citation,
//! so the LLM can answer questions across the user's whole history — fully on-device.

use crate::error::Result;
use crate::storage::models::VaultSource;
use crate::storage::Db;

/// Char budget for the corpus, by provider. Local quantized models (Ollama) have tiny
/// default context windows, so cap much tighter; API/Claude models get headroom.
fn budget_for(provider_id: &str) -> usize {
    if provider_id == "ollama" {
        4_000
    } else {
        80_000
    }
}

/// Returns (corpus, sources). Picks meetings relevant to `query` (full-text search), falling
/// back to the most recent, and packs their notes until the budget is hit.
pub fn build_vault_context(
    db: &Db,
    query: &str,
    provider_id: &str,
) -> Result<(String, Vec<VaultSource>)> {
    let budget = budget_for(provider_id);

    // Relevance-first: search hits, else the most recent meetings.
    let mut meetings: Vec<crate::storage::models::Meeting> = db
        .search(query, 40)?
        .into_iter()
        .map(|h| h.meeting)
        .collect();
    if meetings.is_empty() {
        meetings = db.list_meetings(30)?;
    }

    let mut corpus = String::new();
    let mut sources: Vec<VaultSource> = Vec::new();
    for m in meetings {
        if corpus.len() >= budget {
            break;
        }
        let Some(note) = db.get_latest_note_for_meeting(&m.id)? else {
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
