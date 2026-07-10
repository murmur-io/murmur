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
/// reach the cloud prompt (E9). The live caller (`ask_vault`) now calls
/// [`build_vault_context_visible`] directly with the live `state.unlocked_folders` snapshot, so
/// session-unlocked folders ARE included; this empty-set shim remains only as a fail-closed
/// default for any caller that does not have an unlock set to pass.
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

    // Brain v2 L1.5 — time-aware expansion: a temporal phrase in the question windows the
    // candidate search on `started_at` (query-time `now` is the right anchor for a user query).
    let date_filter =
        crate::summarize::temporal::extract_date_filter(query, chrono::Utc::now().date_naive());

    // Relevance-first: VISIBLE search hits, else the most recent VISIBLE meetings. Both queries
    // apply the sealed-folder visibility clause against `unlocked`, so sealed-not-unlocked
    // meetings are filtered out of the candidate set before any content is read.
    let mut meetings: Vec<crate::storage::models::Meeting> = db
        .search_visible_in_range(query, 40, unlocked, date_filter)?
        .into_iter()
        .map(|h| h.meeting)
        .collect();
    if meetings.is_empty() {
        meetings = db.list_meetings_visible(30, unlocked)?;
    }

    let (mut corpus, sources) = pack_meetings(db, meetings, budget, unlocked)?;
    // Documents/brain notes must be reachable WITHOUT the e5 model too (the default install):
    // append the gated `## Documents` section from the keyword (FTS/BM25) leg alone — an empty
    // `query_vec` means no KNN leg. Same `visibility_clause` gate as every meeting leg, so a
    // sealed-not-unlocked folder's document chunks never enter the cloud-bound corpus.
    pack_doc_chunks(db, query, &[], budget, &mut corpus, unlocked)?;
    Ok((corpus, sources))
}

/// brain2 RAG Phase 2b — HYBRID candidate selection for Ask-My-Vault, GATED by the caller (only
/// invoked when `semantic_search_enabled` is on). Picks candidate meetings via
/// [`Db::search_hybrid_visible`] (FTS5 ∪ vector KNN, fused by RRF), which is ALREADY gated by the
/// SAME `visibility_clause` against `unlocked` as the FTS path — so a sealed-and-not-unlocked folder
/// is excluded identically. The notes are packed with the EXACT same budget + `[[Title]]` citation
/// logic as the FTS path ([`pack_meetings`]), so the only change is which meetings are chosen.
///
/// Graceful fallback: when the vector index is empty the hybrid query degenerates to the FTS ranking
/// (RRF over a single non-empty list preserves its order), and when there are no hybrid hits at all
/// it falls back to the most recent VISIBLE meetings — identical behavior to the FTS path.
pub fn build_vault_context_hybrid_visible(
    db: &Db,
    query: &str,
    provider_id: &str,
    query_vec: &[f32],
    unlocked: &HashSet<String>,
    reranker: Option<&dyn crate::rerank::Reranker>,
) -> Result<(String, Vec<VaultSource>)> {
    let budget = budget_for(provider_id);
    // Brain v2 L1.5 — temporal window (all hybrid legs apply it; query-time `now` anchor).
    let date_filter =
        crate::summarize::temporal::extract_date_filter(query, chrono::Utc::now().date_naive());
    let mut hits = db.search_hybrid_visible(query, query_vec, 40, unlocked, date_filter)?;

    // Brain v2 L1.4 — the RERANKER seam (Ask-only): reorder the TOP-K fused candidates before
    // packing. The reranker sees ONLY already-gated hits (id + title/snippet — content the caller
    // may already pack into the prompt) and MUST degrade to input order on failure/timeout, so
    // this can only reorder, never widen, the candidate set. `None` (no local model / cloud
    // brain / non-Ask callers) = byte-identical to the un-reranked path.
    if let Some(rr) = reranker {
        let k = crate::rerank::RERANK_TOP_K.min(hits.len());
        if k >= 2 {
            let candidates: Vec<(String, String)> = hits[..k]
                .iter()
                .map(|h| {
                    let title = h.meeting.title.clone().unwrap_or_default();
                    (h.meeting.id.clone(), format!("{title}\n{}", h.snippet))
                })
                .collect();
            let order = rr.rerank(query, &candidates, crate::rerank::RERANK_TIMEOUT_MS);
            let mut head: Vec<crate::storage::models::SearchHit> = Vec::with_capacity(k);
            let mut pool: Vec<crate::storage::models::SearchHit> = hits.drain(..k).collect();
            for id in order {
                if let Some(pos) = pool.iter().position(|h| h.meeting.id == id) {
                    head.push(pool.remove(pos));
                }
            }
            head.extend(pool); // degrade-safety: an id the reranker lost keeps its slot.
            head.extend(hits);
            hits = head;
        }
    }

    let mut meetings: Vec<crate::storage::models::Meeting> =
        hits.into_iter().map(|h| h.meeting).collect();
    if meetings.is_empty() {
        meetings = db.list_meetings_visible(30, unlocked)?;
    }
    let (mut corpus, sources) = pack_meetings(db, meetings, budget, unlocked)?;
    // Document ingestion: APPEND a gated `## Documents` section so the brain/Ask can also ground on
    // uploaded md/txt. Both retrieval legs (vector KNN + keyword FTS) re-apply the SAME
    // `visibility_clause` against `unlocked` (joined doc_chunks → documents → folders), so a
    // sealed-and-not-session-unlocked folder's document chunks are NEVER returned — identical gate
    // to the meeting legs. Each hit contributes its document name + best chunk snippet (no meeting
    // citation — documents are not meetings, so they don't add a `VaultSource`). Same `budget`.
    pack_doc_chunks(db, query, query_vec, budget, &mut corpus, unlocked)?;
    Ok((corpus, sources))
}

/// Brain v2 L3 (JIT retrieval, behind `ask_jit_retrieval`) — a COMPACT, GATED meeting LISTING for
/// the agentic Ask persona: one `- id | title | date` line per candidate (title char-capped so a
/// line stays ~80 chars), top `limit` hits for `query` — HYBRID (FTS ∪ vector, RRF-fused) when
/// `query_vec` is non-empty, gated FTS otherwise, falling back to the most recent VISIBLE meetings
/// when nothing matches (the same candidate discipline as the corpus builders above).
///
/// GATE (load-bearing): every candidate comes from `search_hybrid_visible` /
/// `search_visible_in_range` / `list_meetings_visible` — all `visibility_clause`-backed against the
/// live `unlocked` set — so a sealed-and-not-unlocked meeting contributes NO line (not even its
/// title or id). The listing carries titles/ids/dates only, NEVER note or transcript content: the
/// agent must `get_meeting` (itself gated) to read anything.
pub fn build_meeting_listing_visible(
    db: &Db,
    query: &str,
    query_vec: &[f32],
    limit: i64,
    unlocked: &HashSet<String>,
) -> Result<String> {
    /// Keep a whole listing line at ~80 chars: id (36) + separators + date (10) leave ~28 for the title.
    const TITLE_CAP: usize = 28;
    let date_filter =
        crate::summarize::temporal::extract_date_filter(query, chrono::Utc::now().date_naive());
    let mut meetings: Vec<crate::storage::models::Meeting> = if query_vec.is_empty() {
        db.search_visible_in_range(query, limit, unlocked, date_filter)?
            .into_iter()
            .map(|h| h.meeting)
            .collect()
    } else {
        db.search_hybrid_visible(query, query_vec, limit, unlocked, date_filter)?
            .into_iter()
            .map(|h| h.meeting)
            .collect()
    };
    if meetings.is_empty() {
        meetings = db.list_meetings_visible(limit, unlocked)?;
    }
    let lines: Vec<String> = meetings
        .iter()
        .map(|m| {
            let title: String = m
                .title
                .clone()
                .unwrap_or_else(|| "(untitled)".to_string())
                .chars()
                .take(TITLE_CAP)
                .collect();
            let date = m.started_at.split(['T', ' ']).next().unwrap_or("");
            format!("- {} | {} | {}", m.id, title.trim(), date)
        })
        .collect();
    Ok(lines.join("\n"))
}

/// Append a budget-capped `## Documents` section of gated document-chunk snippets to `corpus`. The
/// retrieval is the RRF fusion of `search_doc_chunks_visible` (vector KNN — skipped when
/// `query_vec` is empty, i.e. the flag-off / model-less path) and `search_doc_chunks_fts_visible`
/// (keyword BM25), both gated by `visibility_clause`; a locked-and-not-unlocked folder's chunks are
/// invisible in either leg. Best-effort: an empty doc index simply adds nothing.
fn pack_doc_chunks(
    db: &Db,
    query: &str,
    query_vec: &[f32],
    budget: usize,
    corpus: &mut String,
    unlocked: &HashSet<String>,
) -> Result<()> {
    let knn = if query_vec.is_empty() {
        Vec::new()
    } else {
        db.search_doc_chunks_visible(query_vec, 20, unlocked)?
    };
    let fts = db.search_doc_chunks_fts_visible(query, 20, unlocked)?;
    let hits = crate::embed::fuse_doc_hits(knn, fts);
    if hits.is_empty() {
        return Ok(());
    }
    for h in hits {
        if corpus.len() >= budget {
            break;
        }
        let header = format!("\n\n### Document: {}\n", h.name);
        let remaining = budget.saturating_sub(corpus.len() + header.len());
        if remaining < 100 {
            break;
        }
        let snippet: String = h.snippet.chars().take(remaining).collect();
        corpus.push_str(&header);
        corpus.push_str(&snippet);
    }
    Ok(())
}

/// Pack the candidate `meetings`' VISIBLE notes into a `budget`-capped corpus, each headed by a
/// `### [[Title]] · date · id:` citation. Shared by the FTS and hybrid candidate selectors so the
/// packing / citation / second-gate logic is identical. The `get_note_if_visible` second gate means
/// a sealed-not-unlocked note's content can never enter the corpus even if it slipped into the
/// candidate list.
fn pack_meetings(
    db: &Db,
    meetings: Vec<crate::storage::models::Meeting>,
    budget: usize,
    unlocked: &HashSet<String>,
) -> Result<(String, Vec<VaultSource>)> {
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
        let p = crate::storage::db::unique_temp_path("meetnotes-vaultctx-test", "sqlite");
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
            model_requested: None,
            model_served: None,
            gateway_host: None,
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
        seed_note(
            &db,
            "open",
            "Open Meeting",
            "OPEN-SECRET project Apollo",
            None,
        );
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
        let (c0, _) = build_vault_context_visible(&db, "SECRET", "anthropic", &nothing).unwrap();
        assert!(!c0.contains("LOCKED-SECRET"));

        // Session-unlock the folder → its content is now legitimately available.
        let mut unlocked = HashSet::new();
        unlocked.insert("f-locked".to_string());
        let (corpus2, sources2) =
            build_vault_context_visible(&db, "SECRET", "anthropic", &unlocked).unwrap();
        assert!(
            corpus2.contains("LOCKED-SECRET"),
            "unlocked content must reappear"
        );
        assert!(sources2.iter().any(|s| s.meeting_id == "sealed"));
    }

    /// Brain v2 L3 (JIT) — the compact meeting LISTING is gated by the SAME visibility predicate:
    /// a sealed-and-not-unlocked meeting contributes NO line (not even its title/id — an id in the
    /// listing invites a `get_meeting` probe and a title alone is already a leak), and it reappears
    /// once session-unlocked. Lines carry id | title | date ONLY — never note content.
    #[test]
    fn meeting_listing_is_gated_and_content_free() {
        let db = temp_db();
        seed_note(&db, "open", "Open Meeting", "OPEN-SECRET apollo", None);
        seed_folder(&db, "f-locked");
        seed_note(
            &db,
            "sealed",
            "Sealed Secret Sync",
            "LOCKED-SECRET price",
            Some("f-locked"),
        );
        db.set_folder_locked("f-locked", true, None).unwrap();

        // Query "Secret" MATCHES the sealed meeting too (its title) — so only the visibility gate,
        // not query relevance, is what keeps it out of the listing.
        let nothing = HashSet::new();
        let listing = build_meeting_listing_visible(&db, "Secret", &[], 30, &nothing).unwrap();
        assert!(listing.contains("- open"), "open meeting listed: {listing}");
        assert!(listing.contains("Open Meeting"));
        assert!(listing.contains("2026-06-26"), "date column present: {listing}");
        assert!(!listing.contains("sealed"), "sealed id must not be listed: {listing}");
        assert!(!listing.contains("Sealed Secret"), "sealed title must not leak: {listing}");
        // Content-free: never note text, even for visible meetings.
        assert!(!listing.contains("OPEN-SECRET"), "note content must never enter the listing");

        // Session-unlock ⇒ the sealed meeting's line legitimately reappears.
        let mut unlocked = HashSet::new();
        unlocked.insert("f-locked".to_string());
        let listing2 = build_meeting_listing_visible(&db, "Secret", &[], 30, &unlocked).unwrap();
        assert!(listing2.contains("Sealed Secret"), "unlocked meeting reappears: {listing2}");

        // A long title is char-capped so a line stays compact (~80 chars).
        seed_note(
            &db,
            "long",
            "An Extremely Long Meeting Title That Would Blow The Line Budget Wide Open",
            "body",
            None,
        );
        let listing3 =
            build_meeting_listing_visible(&db, "Extremely Long Meeting", &[], 30, &nothing)
                .unwrap();
        let line = listing3
            .lines()
            .find(|l| l.contains("long |"))
            .expect("the long-title meeting is listed");
        assert!(line.len() <= 90, "listing line stays compact, got {}: {line}", line.len());
    }

    /// Phase 2b: the HYBRID corpus builder is gated by the SAME visibility predicate. A sealed-not-
    /// unlocked folder's content must be absent from the hybrid corpus and reappear once unlocked —
    /// the exact gate guarantee of the FTS path, now via `search_hybrid_visible`.
    #[test]
    fn hybrid_corpus_respects_visibility_gate() {
        let db = temp_db();
        seed_note(
            &db,
            "open",
            "Open Meeting",
            "OPEN-SECRET project Apollo budget",
            None,
        );
        seed_folder(&db, "f-locked");
        seed_note(
            &db,
            "sealed",
            "Sealed Meeting",
            "LOCKED-SECRET acquisition budget price",
            Some("f-locked"),
        );

        // Index BEFORE sealing (content visible) so a vec_chunks row for the sealed meeting exists —
        // proving the READ-time gate (not just absence of an index row) excludes it.
        let emb = crate::embed::active_embedder();
        db.index_meeting_chunks("open", &[], emb.as_ref()).unwrap();
        db.index_meeting_chunks("sealed", &[], emb.as_ref())
            .unwrap();
        db.set_folder_locked("f-locked", true, None).unwrap();

        let qv = emb
            .embed(std::slice::from_ref(&"SECRET budget".to_string()))
            .unwrap();
        let qvec = qv.into_iter().next().unwrap_or_default();

        let nothing = HashSet::new();
        let (corpus, sources) =
            build_vault_context_hybrid_visible(&db, "SECRET", "anthropic", &qvec, &nothing, None)
                .unwrap();
        assert!(
            corpus.contains("OPEN-SECRET"),
            "open note must be in the hybrid corpus"
        );
        assert!(
            !corpus.contains("LOCKED-SECRET"),
            "sealed-not-unlocked content leaked into the hybrid corpus (gate violation)"
        );
        assert!(sources.iter().all(|s| s.meeting_id != "sealed"));

        let mut unlocked = HashSet::new();
        unlocked.insert("f-locked".to_string());
        let (corpus2, _) =
            build_vault_context_hybrid_visible(&db, "SECRET", "anthropic", &qvec, &unlocked, None)
                .unwrap();
        assert!(
            corpus2.contains("LOCKED-SECRET"),
            "unlocked content must reappear in hybrid corpus"
        );
    }
}
