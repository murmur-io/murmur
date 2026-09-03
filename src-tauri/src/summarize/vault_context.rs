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
use crate::links::LinkKind;
use crate::storage::models::{SourceRef, VaultSource};
use crate::storage::Db;

/// note↔meeting-links PR-2 — cap on the TOTAL number of link-expanded neighbours a PINNED Ask
/// pulls in (deduped across all explicit sources AND against the explicit set). The explicit
/// sources are packed FIRST at full budget; the neighbours fill only the remaining budget. Bounds
/// the "brain knows the connections" auto-expansion so a densely-linked item can't drag the whole
/// graph into one prompt.
const LINK_CONTEXT_CAP: usize = 8;

/// The exact, gated source graph selected for a pinned provider corpus. Keeping selection separate
/// from packing lets lifecycle witnesses hash every selected input even when the provider budget
/// later truncates all of its bytes.
#[derive(Debug, Clone)]
pub(crate) struct PinnedVisibleInputs {
    pub(crate) explicit_sources: Vec<ResolvedPinnedSource>,
    pub(crate) neighbours: Vec<ResolvedPinnedSource>,
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedPinnedSource {
    pub(crate) source: SourceRef,
    header: String,
    body: String,
    body_required: bool,
    pub(crate) manifest_digest: [u8; 32],
    pub(crate) vault_sources: Vec<VaultSource>,
}

fn hash_manifest_field(hasher: &mut sha2::Sha256, label: &str, value: &[u8]) {
    use sha2::Digest;

    let label_len = u64::try_from(label.len()).unwrap_or(u64::MAX);
    let value_len = u64::try_from(value.len()).unwrap_or(u64::MAX);
    hasher.update(label_len.to_be_bytes());
    hasher.update(label.as_bytes());
    hasher.update(value_len.to_be_bytes());
    hasher.update(value);
}

fn source_manifest_digest(
    effective_title: &str,
    started_at: Option<&str>,
    body_present: bool,
    body: &str,
) -> [u8; 32] {
    use sha2::Digest;

    let mut hasher = sha2::Sha256::new();
    hash_manifest_field(&mut hasher, "domain", b"murmur:pinned-source-input:v1");
    hash_manifest_field(&mut hasher, "effective_title", effective_title.as_bytes());
    hash_manifest_field(
        &mut hasher,
        "started_at",
        started_at.unwrap_or("").as_bytes(),
    );
    hash_manifest_field(
        &mut hasher,
        "body_present",
        if body_present { b"1" } else { b"0" },
    );
    if body_present {
        hash_manifest_field(&mut hasher, "full_body", body.as_bytes());
    }
    hasher.finalize().into()
}

impl ResolvedPinnedSource {
    /// Preserve the established packer contract: a source needs room for its header plus at least
    /// 200 body characters; otherwise it contributes no truncated header and no source chip.
    fn section_for_budget(&self, budget: usize) -> Option<String> {
        let remaining = budget.saturating_sub(self.header.len());
        if remaining < 200 {
            return None;
        }
        let body: String = self.body.chars().take(remaining).collect();
        if self.body_required && body.trim().is_empty() {
            return None;
        }
        Some(format!("{}{body}", self.header))
    }
}

/// Char budget for the corpus, by provider. Weak on-device models (the explicit local Brain,
/// Apple Foundation Models, and Ollama) have small context windows, so cap them much tighter;
/// API/CLI models get headroom. Reuse `related_context`'s canonical classification so a new
/// on-device role cannot silently receive the cloud-sized Ask corpus.
///
/// `pub` so a caller assembling a pinned corpus across MULTIPLE packers (e.g. the Ask floor packing
/// a pinned org item AND explicit sources) can honor ONE shared budget instead of letting each
/// packer spend the full budget independently.
pub fn budget_for(provider_id: &str) -> usize {
    if crate::summarize::related_context::is_weak_provider(provider_id) {
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
    let mut hits = db.search_hybrid_visible(
        query,
        query_vec,
        40,
        crate::embed::KNN_SEARCH_COSINE_FLOOR,
        unlocked,
        date_filter,
    )?;

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
        db.search_hybrid_visible(
            query,
            query_vec,
            limit,
            crate::embed::KNN_SEARCH_COSINE_FLOOR,
            unlocked,
            date_filter,
        )?
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
        db.search_doc_chunks_visible(query_vec, 20, crate::embed::KNN_SEARCH_COSINE_FLOOR, unlocked)?
    };
    let fts = db.search_doc_chunks_fts_visible(query, 20, unlocked)?;
    let mut hits = crate::embed::fuse_doc_hits(knn, fts);
    if hits.is_empty() {
        return Ok(());
    }
    // Brain v3 audit Fix 1 — GATED, HIT-ALIGNED PARENT EXPANSION: for a top-3 fused doc hit whose
    // section was CORROBORATED by a second sibling leaf in the same retrieval (auto-merging), swap
    // the leaf snippet for the WINNING chunk's own L1 section-parent text (coherent section beats a
    // fragment; context-rot lesson keeps it to the top few). `expand_doc_parents_visible` re-applies
    // the visibility gate, so a sealed-not-unlocked doc contributes nothing. A single-leaf hit and
    // a flat/legacy doc keep their original leaf snippet unchanged — expansion may only ever add
    // the section AROUND what was retrieved, never substitute a different section.
    const EXPAND_TOP_N: usize = 3;
    let parents = db.expand_doc_parents_visible(&hits[..hits.len().min(EXPAND_TOP_N)], unlocked)?;
    for p in parents {
        if let Some(h) = hits.iter_mut().find(|h| h.document_id == p.document_id) {
            if !p.snippet.trim().is_empty() {
                h.snippet = p.snippet;
            }
        }
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
            origin: None,
        });
    }

    Ok((corpus, sources))
}

/// Pack ONE PINNED org item (a read-only SHARED org-feed note being VIEWED in the org-item viewer)
/// into a budget-capped corpus chunk, headed by a `### [[Title]] · shared · id:` citation mirroring
/// [`pack_notes`]. Used by the Ask floor so "Ask about this shared note" is ALWAYS grounded in the
/// item — the local Brain's FTS/vector search never retrieves org-feed content (it lives in the
/// separate `org_chunks`/`org_vec_chunks` partition, queried only by the agentic cloud tool), so
/// pinning is the ONLY way a local-Brain Ask sees it.
///
/// GATE: reads through [`Db::get_org_item`], whose SQL requires `tombstoned = 0 AND
/// context_enabled = 1` — a tombstoned item, or an item whose origin org has its context DISABLED,
/// yields `None` and contributes NOTHING (no stale/withdrawn content ever reaches the prompt). Org
/// items are deliberately org-disclosed content (no folder lock gate applies to them), so
/// `get_org_item`'s gate is the WHOLE gate here — there is no session-`unlocked` dimension. Returns
/// the corpus chunk, or an empty string when nothing was packed.
pub fn pack_pinned_org_item(db: &Db, item_id: &str, provider_id: &str) -> Result<String> {
    let budget = budget_for(provider_id);
    // GATE: tombstoned item / disabled-org item ⇒ `None` ⇒ contributes nothing.
    let Some(item) = db.get_org_item(item_id)? else {
        return Ok(String::new());
    };
    let title = if item.title.trim().is_empty() {
        "Shared note".to_string()
    } else {
        item.title
    };
    let header = format!("### [[{title}]] · shared · id:{}\n", item.item_id);
    // Mirror pack_notes' 200-char floor: don't emit a header with essentially no body.
    if budget <= header.len() + 200 {
        return Ok(String::new());
    }
    let remaining = budget.saturating_sub(header.len());
    let body: String = item.markdown.chars().take(remaining).collect();
    if body.trim().is_empty() {
        return Ok(String::new());
    }
    Ok(format!("{header}{body}"))
}

/// Fairly pack already-gated explicit-source sections into one caller-provided budget. Each
/// section gets a deterministic share before any earlier source can consume the remainder.
fn fair_pack_explicit_sections(
    sections: Vec<(String, Vec<VaultSource>)>,
    budget: usize,
) -> (String, Vec<VaultSource>) {
    const SEPARATOR: &str = "\n\n";

    if sections.is_empty() || budget == 0 {
        return (String::new(), Vec::new());
    }
    let separator_chars = SEPARATOR
        .chars()
        .count()
        .saturating_mul(sections.len().saturating_sub(1))
        .min(budget);
    let content_budget = budget.saturating_sub(separator_chars);
    let per_section = content_budget / sections.len();
    let mut remainder = content_budget % sections.len();
    let mut corpus = String::new();
    let mut sources = Vec::new();

    for (section, section_sources) in sections {
        let extra = usize::from(remainder > 0);
        remainder = remainder.saturating_sub(extra);
        let quota = per_section.saturating_add(extra);
        if quota == 0 {
            continue;
        }
        let chunk: String = section.chars().take(quota).collect();
        if chunk.trim().is_empty() {
            continue;
        }
        if !corpus.is_empty() {
            corpus.push_str(SEPARATOR);
        }
        corpus.push_str(&chunk);
        sources.extend(section_sources);
    }
    (corpus, sources)
}

/// Budgeted core for the pinned Ask corpus. Explicit sources are deduped and packed fairly under
/// `budget`; then ONE global neighbour pass applies ONE dedupe set and ONE [`LINK_CONTEXT_CAP`].
/// Callers with a tighter surface cap (meeting chat) use this directly.
pub(crate) fn build_vault_context_pinned_visible_with_budget(
    db: &Db,
    sources: &[SourceRef],
    budget: usize,
    unlocked: &HashSet<String>,
) -> Result<(String, Vec<VaultSource>)> {
    if budget == 0 || sources.is_empty() {
        return Ok((String::new(), Vec::new()));
    }

    let inputs = resolve_vault_context_pinned_visible_inputs(db, sources, unlocked)?;
    build_vault_context_resolved_visible_with_budget(db, &inputs, budget, unlocked)
}

/// Resolve the typed-visible explicit set and its ordered, deduped active-neighbour expansion once.
/// Both the provider packer and the dashboard's exact lifecycle manifest consume this value, so
/// neither can silently select a different ninth neighbour or a different typed endpoint.
pub(crate) fn resolve_vault_context_pinned_visible_inputs(
    db: &Db,
    sources: &[SourceRef],
    unlocked: &HashSet<String>,
) -> Result<PinnedVisibleInputs> {
    if sources.is_empty() {
        return Ok(PinnedVisibleInputs {
            explicit_sources: Vec::new(),
            neighbours: Vec::new(),
        });
    }

    // Dedupe explicit identities while preserving picker order.
    let mut explicit_keys: HashSet<(String, String)> = HashSet::new();
    let mut explicit_refs: Vec<SourceRef> = Vec::new();
    for source in sources {
        let key = (source.kind.as_str().to_string(), source.id.clone());
        let typed_visible = match source.kind {
            LinkKind::Meeting => {
                db.meeting_is_visible(&source.id, unlocked)?
                    && db.dashboard_ref_exists("meeting", &source.id)?
            }
            LinkKind::Note => db.note_is_visible(&source.id, unlocked)?,
            LinkKind::Document => db.document_is_visible(&source.id, unlocked)?,
            // A Shared Brain edge is a private graph relation, not provider material. Exclude it
            // before the typed snapshot so it cannot enter the corpus, lifecycle manifest, or
            // neighbour expansion.
            LinkKind::Org => false,
        };
        if typed_visible && explicit_keys.insert(key) {
            explicit_refs.push(source.clone());
        }
    }

    let mut seen_neighbours: HashSet<(String, String)> = HashSet::new();
    let mut neighbours = Vec::new();
    'outer: for source in &explicit_refs {
        let edges = db.links_for_visible(source.kind, &source.id, unlocked)?;
        for edge in edges {
            if edge.status != "active" {
                continue;
            }
            let Some(other_kind) = LinkKind::parse(&edge.other_kind) else {
                continue;
            };
            if other_kind == LinkKind::Org {
                // Private Shared Brain relations are visible in the graph UI only. They never
                // broaden a local or cloud provider's source snapshot.
                continue;
            }
            let key = (edge.other_kind, edge.other_id);
            if explicit_keys.contains(&key) || !seen_neighbours.insert(key.clone()) {
                continue;
            }
            neighbours.push(SourceRef {
                kind: other_kind,
                id: key.1,
            });
            if neighbours.len() >= LINK_CONTEXT_CAP {
                break 'outer;
            }
        }
    }
    let explicit_sources = explicit_refs
        .iter()
        .map(|source| resolve_pinned_source(db, source, unlocked))
        .collect::<Result<Vec<_>>>()?;
    let neighbours = neighbours
        .iter()
        .map(|source| resolve_pinned_source(db, source, unlocked))
        .collect::<Result<Vec<_>>>()?;
    Ok(PinnedVisibleInputs {
        explicit_sources,
        neighbours,
    })
}

fn resolve_pinned_source(
    db: &Db,
    source: &SourceRef,
    unlocked: &HashSet<String>,
) -> Result<ResolvedPinnedSource> {
    let (header, body, effective_title, started_at, body_present, vault_sources) = match source.kind
    {
        LinkKind::Meeting => match db.get_meeting_if_visible(&source.id, unlocked)? {
            Some(meeting) => {
                let title = meeting
                    .title
                    .clone()
                    .unwrap_or_else(|| "(untitled)".to_string());
                let Some(note) = db.get_note_if_visible(&source.id, unlocked)? else {
                    return Ok(ResolvedPinnedSource {
                        source: source.clone(),
                        header: String::new(),
                        body: String::new(),
                        body_required: false,
                        manifest_digest: source_manifest_digest(
                            &title,
                            Some(&meeting.started_at),
                            false,
                            "",
                        ),
                        vault_sources: Vec::new(),
                    });
                };
                let date = meeting
                    .started_at
                    .split(['T', ' '])
                    .next()
                    .unwrap_or("")
                    .to_string();
                let header = format!("\n\n### [[{title}]] · {date} · id:{}\n", meeting.id);
                let source = VaultSource {
                    meeting_id: meeting.id,
                    title: title.clone(),
                    started_at: meeting.started_at,
                    origin: None,
                };
                (
                    header,
                    note.markdown,
                    title,
                    Some(source.started_at.clone()),
                    true,
                    vec![source],
                )
            }
            None => (
                String::new(),
                String::new(),
                String::new(),
                None,
                false,
                Vec::new(),
            ),
        },
        LinkKind::Note | LinkKind::Document => {
            let Some(document) =
                db.get_document_if_visible_kind(&source.id, source.kind.as_str(), unlocked)?
            else {
                return Ok(ResolvedPinnedSource {
                    source: source.clone(),
                    header: String::new(),
                    body: String::new(),
                    body_required: true,
                    manifest_digest: source_manifest_digest("", None, false, ""),
                    vault_sources: Vec::new(),
                });
            };
            let title = document
                .title
                .filter(|title| !title.trim().is_empty())
                .unwrap_or(document.name);
            (
                format!("\n\n### [[{title}]] · id:{}\n", document.id),
                document.markdown,
                title,
                None,
                true,
                Vec::new(),
            )
        }
        LinkKind::Org => {
            // All public callers exclude Org before reaching this resolver. Keep the fallback
            // content-free and fail-closed if a future internal caller bypasses that selection.
            return Ok(ResolvedPinnedSource {
                source: source.clone(),
                header: String::new(),
                body: String::new(),
                body_required: true,
                manifest_digest: source_manifest_digest("", None, false, ""),
                vault_sources: Vec::new(),
            });
        }
    };
    let manifest_digest =
        source_manifest_digest(&effective_title, started_at.as_deref(), body_present, &body);
    Ok(ResolvedPinnedSource {
        source: source.clone(),
        header,
        body,
        body_required: source.kind != LinkKind::Meeting,
        manifest_digest,
        vault_sources,
    })
}

pub(crate) fn build_vault_context_resolved_visible_with_budget(
    _db: &Db,
    inputs: &PinnedVisibleInputs,
    budget: usize,
    _unlocked: &HashSet<String>,
) -> Result<(String, Vec<VaultSource>)> {
    if budget == 0 || inputs.explicit_sources.is_empty() {
        return Ok((String::new(), Vec::new()));
    }

    // Pack each EXPLICIT source from the already gated snapshot. No database read or link
    // expansion happens here; the lifecycle manifest consumes the same resolved source bytes.
    let mut sections: Vec<(String, Vec<VaultSource>)> = Vec::new();
    for source in &inputs.explicit_sources {
        if let Some(section) = source.section_for_budget(budget) {
            sections.push((section, source.vault_sources.clone()));
        }
    }
    let (mut corpus, mut vault_sources) = fair_pack_explicit_sections(sections, budget);

    // Neighbours fill only budget left after every explicit source got its fair share.
    for neighbour in &inputs.neighbours {
        let remaining = budget.saturating_sub(corpus.chars().count());
        if remaining == 0 {
            break;
        }
        if let Some(chunk) = neighbour.section_for_budget(remaining) {
            corpus.push_str(&chunk);
            vault_sources.extend(neighbour.vault_sources.clone());
        }
    }
    Ok((corpus, vault_sources))
}

/// Pack exactly the caller-selected sources under one fair budget, without the neighbour expansion
/// used by Ask. Convert-to-note uses this for a meeting's ACTIVE Related edges: the linked items are
/// useful secondary context, but a link-of-a-link was never selected for this conversion and must
/// not silently enter the provider prompt. Every content read is resolved once through the same
/// typed, visibility-gated snapshot as [`build_vault_context_pinned_visible_with_budget`].
pub(crate) fn build_vault_context_exact_visible_with_budget(
    db: &Db,
    sources: &[SourceRef],
    budget: usize,
    unlocked: &HashSet<String>,
) -> Result<String> {
    if budget == 0 || sources.is_empty() {
        return Ok(String::new());
    }

    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut sections = Vec::new();
    for source in sources {
        if source.kind == LinkKind::Org {
            // Conversion accepts only local typed sources. Shared Brain relations stay private
            // graph metadata and are never resolved into provider context.
            continue;
        }
        let key = (source.kind.as_str().to_string(), source.id.clone());
        if !seen.insert(key) {
            continue;
        }
        let resolved = resolve_pinned_source(db, source, unlocked)?;
        if let Some(section) = resolved.section_for_budget(budget) {
            if !section.trim().is_empty() {
                sections.push((section, resolved.vault_sources));
            }
        }
    }
    Ok(fair_pack_explicit_sections(sections, budget).0)
}

/// note↔meeting-links PR-2 — build a PINNED corpus from an EXPLICIT source list plus its capped,
/// gated link-expansion. This is the source-scoped Ask path: the corpus is EXACTLY the listed
/// `sources` (packed FIRST, fairly under the provider budget) plus up to [`LINK_CONTEXT_CAP`] of
/// their ACTIVE linked neighbours (packed AFTER, filling remaining budget only) — NEVER a
/// vault-wide search.
///
/// GATE (E9, load-bearing): explicit sources and capped neighbours are first resolved into one
/// typed, visibility-gated [`PinnedVisibleInputs`] snapshot against the live `unlocked` set. The
/// provider packer and dashboard lifecycle manifest then consume those same bytes, so there is no
/// second content read between authorization and dispatch. Link expansion uses
/// [`Db::links_for_visible`], whose endpoints are already gated; a sealed explicit source or sealed
/// neighbour contributes nothing and is never enumerated.
///
/// Returns `(corpus, sources)` — `sources` carries the VaultSource chips for the MEETING sources
/// that actually packed (notes/documents are not meetings, so they add no chip, matching
/// `pack_doc_chunks`).
pub fn build_vault_context_pinned_visible(
    db: &Db,
    sources: &[SourceRef],
    provider_id: &str,
    unlocked: &HashSet<String>,
) -> Result<(String, Vec<VaultSource>)> {
    build_vault_context_pinned_visible_with_budget(db, sources, budget_for(provider_id), unlocked)
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

    /// Regression: every existing on-device connection must share the small Ask corpus budget,
    /// while every current cloud-capable connection and an unknown future identifier retain the
    /// large-window budget. The explicit local Brain used to miss the Ollama-only check and
    /// therefore received 200k chars despite its 4096-token plan. Binding both sides here also
    /// makes the reused classification seam reviewable without duplicating its string literals.
    #[test]
    fn vault_budget_covers_the_complete_on_device_provider_matrix() {
        for provider_id in [
            crate::summarize::roles::CONN_LOCAL,
            crate::summarize::roles::CONN_AFM,
            crate::summarize::PROVIDER_OLLAMA,
        ] {
            assert_eq!(
                budget_for(provider_id),
                4_000,
                "on-device provider {provider_id} must use the compact Ask corpus budget"
            );
        }
        for provider_id in [
            crate::summarize::PROVIDER_CLAUDE_CODE,
            crate::summarize::PROVIDER_CODEX_CLI,
            crate::summarize::PROVIDER_ANTHROPIC,
            crate::summarize::PROVIDER_GATEWAY,
            "future-remote-provider",
        ] {
            assert_eq!(
                budget_for(provider_id),
                200_000,
                "non-weak provider {provider_id} must retain the large Ask corpus budget"
            );
        }
    }

    /// RED-before-GREEN resource-bound oracle: both the whole-vault and explicit-source paths must
    /// apply the resolved local provider's compact budget to the corpus they actually construct.
    #[test]
    fn explicit_local_ask_corpora_are_capped_at_the_on_device_budget() {
        let db = temp_db();
        seed_note(
            &db,
            "local-long",
            "Local budget",
            &format!("LOCAL-BUDGET-EVIDENCE {}", "ż".repeat(12_000)),
            None,
        );

        let nothing = HashSet::new();
        let (whole, whole_sources) =
            build_vault_context_visible(&db, "", crate::summarize::roles::CONN_LOCAL, &nothing)
                .unwrap();
        assert!(whole.contains("LOCAL-BUDGET-EVIDENCE"));
        assert_eq!(whole_sources.len(), 1);
        assert!(
            whole.chars().count() <= 4_000,
            "local whole-vault corpus exceeded 4k chars: {}",
            whole.chars().count()
        );

        let (pinned, pinned_sources) = build_vault_context_pinned_visible(
            &db,
            &[m_src("local-long")],
            crate::summarize::roles::CONN_LOCAL,
            &nothing,
        )
        .unwrap();
        assert!(pinned.contains("LOCAL-BUDGET-EVIDENCE"));
        assert_eq!(pinned_sources.len(), 1);
        assert!(
            pinned.chars().count() <= 4_000,
            "local pinned corpus exceeded 4k chars: {}",
            pinned.chars().count()
        );
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
        assert!(
            listing.contains("2026-06-26"),
            "date column present: {listing}"
        );
        assert!(
            !listing.contains("sealed"),
            "sealed id must not be listed: {listing}"
        );
        assert!(
            !listing.contains("Sealed Secret"),
            "sealed title must not leak: {listing}"
        );
        // Content-free: never note text, even for visible meetings.
        assert!(
            !listing.contains("OPEN-SECRET"),
            "note content must never enter the listing"
        );

        // Session-unlock ⇒ the sealed meeting's line legitimately reappears.
        let mut unlocked = HashSet::new();
        unlocked.insert("f-locked".to_string());
        let listing2 = build_meeting_listing_visible(&db, "Secret", &[], 30, &unlocked).unwrap();
        assert!(
            listing2.contains("Sealed Secret"),
            "unlocked meeting reappears: {listing2}"
        );

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
        assert!(
            line.len() <= 90,
            "listing line stays compact, got {}: {line}",
            line.len()
        );
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

    // ── note↔meeting-links PR-2 — source-scoped + link-aware PINNED retrieval ──

    /// Seed a standalone note (`documents` `kind='note'`) into a folder. Returns nothing; the note is
    /// addressable by `id` in the `documents` id space (the `LinkKind::Note` id space).
    fn seed_doc_note(db: &Db, id: &str, folder_id: &str, name: &str, body: &str) {
        db.insert_document(id, folder_id, name, body, "note", 1_700_000_000)
            .unwrap();
    }

    fn m_src(id: &str) -> SourceRef {
        SourceRef {
            kind: LinkKind::Meeting,
            id: id.to_string(),
        }
    }
    fn n_src(id: &str) -> SourceRef {
        SourceRef {
            kind: LinkKind::Note,
            id: id.to_string(),
        }
    }

    fn d_src(id: &str) -> SourceRef {
        SourceRef {
            kind: LinkKind::Document,
            id: id.to_string(),
        }
    }

    #[test]
    fn resolved_snapshot_preserves_the_legacy_200_character_pack_floor() {
        let db = temp_db();
        seed_folder(&db, "floor");
        seed_note(&db, "empty-meeting", "Empty meeting", "", Some("floor"));
        db.insert_note(
            "hub",
            "floor",
            "Hub",
            "Hub",
            &"H".repeat(260),
            1_700_000_000,
        )
        .unwrap();
        db.insert_note(
            "near",
            "floor",
            "Near",
            "Near",
            "NEIGHBOUR MUST NOT FIT",
            1_700_000_000,
        )
        .unwrap();
        db.insert_link_for_test(
            "note", "hub", "note", "near", "manual", 1.0, "user", "active",
        );

        let too_small = build_vault_context_pinned_visible_with_budget(
            &db,
            &[n_src("hub")],
            100,
            &HashSet::new(),
        )
        .unwrap()
        .0;
        assert!(
            too_small.is_empty(),
            "no partial header below the 200-char floor"
        );

        let tight = build_vault_context_pinned_visible_with_budget(
            &db,
            &[n_src("hub")],
            300,
            &HashSet::new(),
        )
        .unwrap()
        .0;
        assert!(tight.contains("Hub"));
        assert!(!tight.contains("NEIGHBOUR MUST NOT FIT"));

        let (empty_meeting, sources) = build_vault_context_pinned_visible_with_budget(
            &db,
            &[m_src("empty-meeting")],
            300,
            &HashSet::new(),
        )
        .unwrap();
        assert!(empty_meeting.contains("Empty meeting"));
        assert_eq!(
            sources.len(),
            1,
            "an empty meeting note retains its source chip"
        );
    }

    #[test]
    fn pinned_provider_corpus_enforces_note_document_kind_before_body_read() {
        let db = temp_db();
        seed_folder(&db, "typed-open");
        db.insert_document(
            "real-note",
            "typed-open",
            "Note",
            "note",
            "note",
            1_700_000_000,
        )
        .unwrap();
        db.insert_document(
            "real-doc",
            "typed-open",
            "Document",
            "doc",
            "document",
            1_700_000_000,
        )
        .unwrap();
        db.insert_document(
            "neighbour",
            "typed-open",
            "Neighbour",
            "FORGED-NEIGHBOUR-SENTINEL",
            "document",
            1_700_000_000,
        )
        .unwrap();
        db.insert_link_for_test(
            "document",
            "real-note",
            "document",
            "neighbour",
            "manual",
            1.0,
            "user",
            "active",
        );
        db.insert_link_for_test(
            "note",
            "real-doc",
            "document",
            "neighbour",
            "manual",
            1.0,
            "user",
            "active",
        );
        db.lock()
            .execute(
                "UPDATE documents SET text=x'00' WHERE id IN ('real-note','real-doc')",
                [],
            )
            .unwrap();

        let (forged, _) = build_vault_context_pinned_visible(
            &db,
            &[d_src("real-note"), n_src("real-doc")],
            "anthropic",
            &HashSet::new(),
        )
        .unwrap();
        assert!(forged.is_empty());
        assert!(!forged.contains("FORGED-NEIGHBOUR-SENTINEL"));

        db.lock()
            .execute(
                "UPDATE documents SET text=CASE id WHEN 'real-note' THEN 'NOTE-KIND-SENTINEL' ELSE 'DOCUMENT-KIND-SENTINEL' END WHERE id IN ('real-note','real-doc')",
                [],
            )
            .unwrap();

        let (valid, _) = build_vault_context_pinned_visible(
            &db,
            &[n_src("real-note"), d_src("real-doc")],
            "anthropic",
            &HashSet::new(),
        )
        .unwrap();
        assert!(valid.contains("NOTE-KIND-SENTINEL"));
        assert!(valid.contains("DOCUMENT-KIND-SENTINEL"));
    }

    #[test]
    fn pinned_provider_corpus_typed_control_fixture() {
        let db = temp_db();
        seed_folder(&db, "typed-control");
        db.insert_document(
            "real-note",
            "typed-control",
            "Note",
            "NOTE-KIND-SENTINEL",
            "note",
            1_700_000_000,
        )
        .unwrap();
        db.insert_document(
            "real-doc",
            "typed-control",
            "Document",
            "DOCUMENT-KIND-SENTINEL",
            "document",
            1_700_000_000,
        )
        .unwrap();

        let (valid, _) = build_vault_context_pinned_visible(
            &db,
            &[n_src("real-note"), d_src("real-doc")],
            "anthropic",
            &HashSet::new(),
        )
        .unwrap();
        assert!(valid.contains("NOTE-KIND-SENTINEL"));
        assert!(valid.contains("DOCUMENT-KIND-SENTINEL"));
    }

    /// Meeting-chat regression: a note the user EXPLICITLY picked must still reach the provider
    /// prompt when the anchor meeting transcript is longer than the transcript budget. The old
    /// command appended the pinned corpus after the transcript and then truncated the combined
    /// string from the front, dropping the picked note in full for every >40k-char meeting.
    #[test]
    fn pinned_note_survives_long_meeting_chat_prompt_budget() {
        let db = temp_db();
        seed_note(
            &db,
            "anchor",
            "Anchor meeting",
            &format!("ANCHOR-NOTE {}", "a".repeat(50_000)),
            None,
        );
        seed_folder(&db, "notes-open");
        seed_doc_note(
            &db,
            "note-picked",
            "notes-open",
            "Picked plan",
            "The launch codename is SOURCE-ONLY-ORCHID.",
        );

        let pinned = crate::commands::pack_chat_pinned_sources(
            &db,
            "anchor",
            &[m_src("anchor"), n_src("note-picked")],
            "anthropic",
            &HashSet::new(),
        )
        .unwrap();
        assert!(
            pinned.contains("SOURCE-ONLY-ORCHID"),
            "fixture: the gated explicit-source packer must return the picked note"
        );
        assert!(
            !pinned.contains("ANCHOR-NOTE"),
            "the primary anchor is already represented by the transcript and must not consume the source budget"
        );

        let transcript = "transcript ".repeat(4_000);
        let (system, _) = crate::summarize::chat::build_with_sources(
            &transcript,
            &pinned,
            &[],
            "What is the codename?",
            "",
        );

        assert!(
            system.contains("SOURCE-ONLY-ORCHID"),
            "an explicitly picked note must never be truncated away by the anchor transcript"
        );
    }

    #[test]
    fn budgeted_pinned_corpus_fairly_keeps_each_deduped_explicit_source() {
        let db = temp_db();
        seed_folder(&db, "notes-open");
        seed_doc_note(
            &db,
            "large-a",
            "notes-open",
            "Large A",
            &format!("EXPLICIT-A-FACT {}", "a".repeat(2_000)),
        );
        seed_doc_note(
            &db,
            "large-b",
            "notes-open",
            "Large B",
            &format!("EXPLICIT-B-FACT {}", "b".repeat(2_000)),
        );

        let (corpus, _) = build_vault_context_pinned_visible_with_budget(
            &db,
            &[n_src("large-a"), n_src("large-a"), n_src("large-b")],
            500,
            &HashSet::new(),
        )
        .unwrap();
        assert!(corpus.contains("EXPLICIT-A-FACT"));
        assert!(
            corpus.contains("EXPLICIT-B-FACT"),
            "a large first explicit source must not erase the next one: {corpus}"
        );
        assert_eq!(
            corpus.matches("id:large-a").count(),
            1,
            "duplicate explicit identities must pack once"
        );
        assert!(corpus.chars().count() <= 500);
    }

    /// The pinned corpus contains EXACTLY the listed sources — a meeting NOT in `sources` never
    /// appears, even though it exists and would match a whole-vault search.
    #[test]
    fn pinned_corpus_only_includes_listed_sources() {
        let db = temp_db();
        seed_note(
            &db,
            "m-in",
            "In Meeting",
            "PINNED-BODY project apollo",
            None,
        );
        seed_note(
            &db,
            "m-out",
            "Out Meeting",
            "UNLISTED-BODY project zeus",
            None,
        );

        let nothing = HashSet::new();
        let (corpus, sources) =
            build_vault_context_pinned_visible(&db, &[m_src("m-in")], "anthropic", &nothing)
                .unwrap();
        assert!(corpus.contains("PINNED-BODY"), "listed source is packed");
        assert!(
            !corpus.contains("UNLISTED-BODY"),
            "an unlisted meeting must NEVER enter a pinned corpus: {corpus}"
        );
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].meeting_id, "m-in");

        // A note source packs the standalone note body; the unlisted meeting still absent.
        seed_folder(&db, "f-open");
        seed_doc_note(
            &db,
            "note-in",
            "f-open",
            "Pinned Note",
            "NOTE-BODY design decisions",
        );
        let (corpus2, _) =
            build_vault_context_pinned_visible(&db, &[n_src("note-in")], "anthropic", &nothing)
                .unwrap();
        assert!(corpus2.contains("NOTE-BODY"), "note source packs its body");
        assert!(!corpus2.contains("UNLISTED-BODY"));
    }

    /// A SEALED explicit source contributes NOTHING to the pinned corpus (E9) — pinning a locked
    /// item can never leak it.
    #[test]
    fn pinned_corpus_sealed_source_contributes_nothing() {
        let db = temp_db();
        seed_folder(&db, "f-locked");
        seed_note(
            &db,
            "m-sealed",
            "Sealed Meeting",
            "SEALED-BODY acquisition price",
            Some("f-locked"),
        );
        seed_doc_note(
            &db,
            "note-sealed",
            "f-locked",
            "Sealed Note",
            "SEALED-NOTE-BODY roadmap",
        );
        db.set_folder_locked("f-locked", true, None).unwrap();

        // Nothing session-unlocked: both a sealed MEETING source and a sealed NOTE source pack nothing.
        let nothing = HashSet::new();
        let (corpus, sources) = build_vault_context_pinned_visible(
            &db,
            &[m_src("m-sealed"), n_src("note-sealed")],
            "anthropic",
            &nothing,
        )
        .unwrap();
        assert!(
            !corpus.contains("SEALED-BODY"),
            "sealed meeting source leaked into pinned corpus (E9): {corpus}"
        );
        assert!(
            !corpus.contains("SEALED-NOTE-BODY"),
            "sealed note source leaked into pinned corpus (E9): {corpus}"
        );
        assert!(sources.is_empty(), "no source chip for a sealed source");
        let chat_corpus = crate::commands::pack_chat_pinned_sources(
            &db,
            "open-anchor",
            &[m_src("m-sealed"), n_src("note-sealed")],
            "anthropic",
            &nothing,
        )
        .unwrap();
        assert!(
            chat_corpus.is_empty(),
            "meeting-chat's fair source packer must preserve the same fail-closed gate"
        );

        // Session-unlock the folder ⇒ the pinned sources legitimately reappear.
        let mut unlocked = HashSet::new();
        unlocked.insert("f-locked".to_string());
        let (corpus2, _) = build_vault_context_pinned_visible(
            &db,
            &[m_src("m-sealed"), n_src("note-sealed")],
            "anthropic",
            &unlocked,
        )
        .unwrap();
        assert!(
            corpus2.contains("SEALED-BODY"),
            "unlocked meeting source reappears"
        );
        assert!(
            corpus2.contains("SEALED-NOTE-BODY"),
            "unlocked note source reappears"
        );
        let chat_corpus2 = crate::commands::pack_chat_pinned_sources(
            &db,
            "open-anchor",
            &[m_src("m-sealed"), n_src("note-sealed")],
            "anthropic",
            &unlocked,
        )
        .unwrap();
        assert!(chat_corpus2.contains("SEALED-BODY"));
        assert!(chat_corpus2.contains("SEALED-NOTE-BODY"));
    }

    #[test]
    fn gated_meeting_row_hides_residual_title_audio_and_plaintext_until_unlocked() {
        let db = temp_db();
        seed_folder(&db, "f-locked-row");
        db.insert_meeting(&Meeting {
            id: "m-residual".into(),
            started_at: "2026-06-26T09:00:00Z".into(),
            ended_at: None,
            title: Some("RESIDUAL-TITLE-SENTINEL".into()),
            duration_s: 60,
            audio_path: Some("/tmp/RESIDUAL-AUDIO-SENTINEL.wav".into()),
            status: MeetingStatus::Summarized,
            folder_id: None,
        })
        .unwrap();
        db.upsert_note(&NoteRecord {
            meeting_id: "m-residual".into(),
            provider_id: "test".into(),
            markdown: "RESIDUAL-BODY-SENTINEL".into(),
            created_at: "2026-06-26T09:05:00Z".into(),
            exported_path: None,
            model_requested: None,
            model_served: None,
            gateway_host: None,
        })
        .unwrap();
        db.set_note_folder("m-residual", Some("f-locked-row"))
            .unwrap();
        db.set_folder_locked("f-locked-row", true, None).unwrap();

        let nothing = HashSet::new();
        assert!(db
            .get_meeting_if_visible("m-residual", &nothing)
            .unwrap()
            .is_none());
        let (packed, sources) =
            build_vault_context_pinned_visible(&db, &[m_src("m-residual")], "anthropic", &nothing)
                .unwrap();
        assert!(packed.is_empty());
        assert!(sources.is_empty());
        assert!(!packed.contains("RESIDUAL-TITLE-SENTINEL"));
        assert!(!packed.contains("RESIDUAL-AUDIO-SENTINEL"));
        assert!(!packed.contains("RESIDUAL-BODY-SENTINEL"));

        let unlocked: HashSet<String> = ["f-locked-row".to_string()].into_iter().collect();
        let visible = db
            .get_meeting_if_visible("m-residual", &unlocked)
            .unwrap()
            .expect("session unlock restores the full meeting row");
        assert_eq!(visible.title.as_deref(), Some("RESIDUAL-TITLE-SENTINEL"));
        let (packed, sources) =
            build_vault_context_pinned_visible(&db, &[m_src("m-residual")], "anthropic", &unlocked)
                .unwrap();
        assert!(packed.contains("RESIDUAL-TITLE-SENTINEL"));
        assert!(packed.contains("RESIDUAL-BODY-SENTINEL"));
        assert_eq!(sources.len(), 1);
    }

    /// Link-aware expansion pulls in an explicit source's ACTIVE linked neighbour's content — and a
    /// SEALED neighbour is DROPPED (the both-endpoint gate holds through the expansion).
    #[test]
    fn pinned_corpus_expands_active_links_gated() {
        let db = temp_db();
        // One explicit meeting, manually linked to (a) an OPEN neighbour meeting and (b) a SEALED one.
        seed_note(
            &db,
            "m-anchor",
            "Anchor Meeting",
            "ANCHOR-BODY kickoff",
            None,
        );
        seed_note(
            &db,
            "m-open-neighbour",
            "Open Neighbour",
            "OPEN-NEIGHBOUR-BODY specs",
            None,
        );
        seed_folder(&db, "f-locked");
        seed_note(
            &db,
            "m-sealed-neighbour",
            "Sealed Neighbour",
            "SEALED-NEIGHBOUR-BODY numbers",
            Some("f-locked"),
        );
        // Manual links from the anchor to both neighbours (rows written BEFORE sealing).
        db.upsert_manual_link("meeting", "m-anchor", "meeting", "m-open-neighbour")
            .unwrap();
        db.upsert_manual_link("meeting", "m-anchor", "meeting", "m-sealed-neighbour")
            .unwrap();
        db.set_folder_locked("f-locked", true, None).unwrap();

        let nothing = HashSet::new();
        let (corpus, _) =
            build_vault_context_pinned_visible(&db, &[m_src("m-anchor")], "anthropic", &nothing)
                .unwrap();
        assert!(corpus.contains("ANCHOR-BODY"), "explicit source packed");
        assert!(
            corpus.contains("OPEN-NEIGHBOUR-BODY"),
            "an ACTIVE linked neighbour's body is auto-expanded into the corpus: {corpus}"
        );
        assert!(
            !corpus.contains("SEALED-NEIGHBOUR-BODY"),
            "a SEALED linked neighbour must be dropped by the gate (E9): {corpus}"
        );
    }

    #[test]
    fn exact_conversion_context_is_visible_and_never_expands_links_of_links() {
        let db = temp_db();
        seed_note(&db, "open-linked", "Open linked", "OPEN-LINKED-BODY", None);
        seed_folder(&db, "f-conversion-open");
        seed_doc_note(
            &db,
            "exact-note",
            "f-conversion-open",
            "Exact note",
            "EXACT-NOTE-BODY",
        );
        db.insert_document(
            "exact-document",
            "f-conversion-open",
            "Exact document",
            "EXACT-DOCUMENT-BODY",
            "document",
            1_700_000_000,
        )
        .unwrap();
        seed_folder(&db, "f-conversion-locked");
        seed_note(
            &db,
            "sealed-linked",
            "SEALED-TITLE-MUST-NOT-ENTER",
            "SEALED-LINKED-BODY",
            Some("f-conversion-locked"),
        );
        seed_note(&db, "second-hop", "Second hop", "SECOND-HOP-BODY", None);
        db.upsert_manual_link("meeting", "open-linked", "meeting", "second-hop")
            .unwrap();
        db.set_folder_locked("f-conversion-locked", true, None)
            .unwrap();

        let corpus = build_vault_context_exact_visible_with_budget(
            &db,
            &[
                m_src("open-linked"),
                n_src("exact-note"),
                n_src("exact-note"),
                d_src("exact-document"),
                n_src("exact-document"),
                m_src("sealed-linked"),
            ],
            20_000,
            &HashSet::new(),
        )
        .unwrap();
        assert!(corpus.contains("OPEN-LINKED-BODY"));
        assert_eq!(corpus.matches("EXACT-NOTE-BODY").count(), 1);
        assert_eq!(corpus.matches("EXACT-DOCUMENT-BODY").count(), 1);
        let meeting_pos = corpus.find("OPEN-LINKED-BODY").unwrap();
        let note_pos = corpus.find("EXACT-NOTE-BODY").unwrap();
        let document_pos = corpus.find("EXACT-DOCUMENT-BODY").unwrap();
        assert!(meeting_pos < note_pos && note_pos < document_pos);
        assert!(!corpus.contains("SEALED-LINKED-BODY"));
        assert!(!corpus.contains("SEALED-TITLE-MUST-NOT-ENTER"));
        assert!(
            !corpus.contains("SECOND-HOP-BODY"),
            "conversion context is exactly Related, never a link-of-link expansion: {corpus}"
        );
    }

    /// Meeting-chat must invoke ONE global pinned builder, not one builder per explicit source:
    /// two sources with >8 ACTIVE neighbours each still expand at most LINK_CONTEXT_CAP neighbours
    /// TOTAL, and a neighbour shared by both sources is packed only once.
    #[test]
    fn chat_pinned_sources_share_one_global_link_cap_and_neighbour_dedupe() {
        let db = temp_db();
        seed_note(&db, "explicit-a", "Explicit A", "EXPLICIT-A-BODY", None);
        seed_note(&db, "explicit-b", "Explicit B", "EXPLICIT-B-BODY", None);
        seed_note(
            &db,
            "shared-n",
            "Shared neighbour",
            "NEIGHBOUR-SHARED",
            None,
        );
        // Insert the shared edge first for BOTH sources so the pre-fix N-builders path packs it
        // twice; the corrected single builder's global seen-set must collapse it.
        db.upsert_manual_link("meeting", "explicit-a", "meeting", "shared-n")
            .unwrap();
        db.upsert_manual_link("meeting", "explicit-b", "meeting", "shared-n")
            .unwrap();

        for owner in ["a", "b"] {
            for i in 0..10 {
                let id = format!("{owner}-neighbour-{i}");
                let marker = format!("NEIGHBOUR-{owner}-{i}");
                seed_note(&db, &id, &format!("{owner} neighbour {i}"), &marker, None);
                db.upsert_manual_link("meeting", &format!("explicit-{owner}"), "meeting", &id)
                    .unwrap();
            }
        }

        let corpus = crate::commands::pack_chat_pinned_sources(
            &db,
            "primary-anchor",
            &[m_src("explicit-a"), m_src("explicit-b")],
            "anthropic",
            &HashSet::new(),
        )
        .unwrap();
        assert!(corpus.contains("EXPLICIT-A-BODY"));
        assert!(corpus.contains("EXPLICIT-B-BODY"));

        let mut expanded_unique = 0usize;
        let mut expanded_occurrences = corpus.matches("NEIGHBOUR-SHARED").count();
        expanded_unique += usize::from(corpus.contains("NEIGHBOUR-SHARED"));
        for owner in ["a", "b"] {
            for i in 0..10 {
                let marker = format!("NEIGHBOUR-{owner}-{i}");
                let occurrences = corpus.matches(&marker).count();
                expanded_unique += usize::from(occurrences > 0);
                expanded_occurrences += occurrences;
            }
        }
        assert_eq!(
            expanded_unique, LINK_CONTEXT_CAP,
            "all explicit sources must share exactly ONE global neighbour cap in this full-budget fixture: {corpus}"
        );
        assert_eq!(
            expanded_occurrences, expanded_unique,
            "the same neighbour must never be packed twice across explicit sources"
        );
    }

    /// Link-expansion is capped at LINK_CONTEXT_CAP: more active neighbours than the cap are
    /// truncated to exactly the cap (the explicit source is always packed on top).
    #[test]
    fn link_expansion_capped_at_link_context_cap() {
        let db = temp_db();
        seed_note(&db, "m-hub", "Hub Meeting", "HUB-BODY central", None);
        // Seed CAP + 3 neighbours, each with a UNIQUELY greppable body, all actively linked to the hub.
        let extra = 3usize;
        for i in 0..(LINK_CONTEXT_CAP + extra) {
            let id = format!("m-nb-{i}");
            let body = format!("NEIGHBOUR-MARK-{i} content");
            seed_note(&db, &id, &format!("Neighbour {i}"), &body, None);
            db.upsert_manual_link("meeting", "m-hub", "meeting", &id)
                .unwrap();
        }

        let nothing = HashSet::new();
        let (corpus, _) =
            build_vault_context_pinned_visible(&db, &[m_src("m-hub")], "anthropic", &nothing)
                .unwrap();
        assert!(corpus.contains("HUB-BODY"), "hub packed");
        let packed = (0..(LINK_CONTEXT_CAP + extra))
            .filter(|i| corpus.contains(&format!("NEIGHBOUR-MARK-{i} ")))
            .count();
        assert_eq!(
            packed, LINK_CONTEXT_CAP,
            "exactly LINK_CONTEXT_CAP neighbours are expanded (got {packed}); the rest are truncated"
        );
    }

    /// Regression guard: the whole-vault (`None`-sources) corpus is unchanged by PR-2 — the pinned
    /// builder is a NEW, separate path. A whole-vault build over the same DB still contains every
    /// visible meeting (the pre-change behavior), whereas a pinned build over a single source does
    /// NOT. This binds "None preserves the whole vault" at the vault_context level.
    #[test]
    fn pinned_vs_whole_vault_none_preserves_whole_vault() {
        let db = temp_db();
        seed_note(&db, "m-a", "Meeting A", "WHOLE-A body one", None);
        seed_note(&db, "m-b", "Meeting B", "WHOLE-B body two", None);

        let nothing = HashSet::new();
        // Whole-vault (the `None`/no-picker path): BOTH meetings present (empty query → recent list).
        let (whole, whole_sources) =
            build_vault_context_visible(&db, "", "anthropic", &nothing).unwrap();
        assert!(whole.contains("WHOLE-A"), "whole-vault contains meeting A");
        assert!(whole.contains("WHOLE-B"), "whole-vault contains meeting B");
        assert_eq!(whole_sources.len(), 2);

        // Pinned to A only: A present, B ABSENT — the scoped path is strictly narrower.
        let (pinned, pinned_sources) =
            build_vault_context_pinned_visible(&db, &[m_src("m-a")], "anthropic", &nothing)
                .unwrap();
        assert!(pinned.contains("WHOLE-A"));
        assert!(
            !pinned.contains("WHOLE-B"),
            "pinned corpus must not contain the unlisted meeting"
        );
        assert_eq!(pinned_sources.len(), 1);
    }
}

#[cfg(test)]
mod knn_floor_tests {
    /// A stub embedder must yield NO handle, so the Ask path gets no query vector rather than a
    /// fake one.
    ///
    /// This is the half that actually mattered. `active_admitted_embedder` falls back to
    /// `StubEmbedder`, a hash bag whose "similarity" carries no semantics, and the old code fed that
    /// straight into the KNN leg. Combined with a `0.0` floor — which cannot reject anything — the
    /// result was noise fused into the answer as if it were a semantic signal. Two bugs compounding:
    /// a fake vector and a threshold unable to reject it.
    ///
    /// `active_persistence_embedder_if_available` returns `None` for a stub snapshot, so the vector
    /// is empty, `search_hybrid_visible` treats the KNN leg as absent, and `score_fuse`
    /// redistributes its weight to the legs that have something to say.
    ///
    /// NOTE on what this can and cannot assert. `embed_model_present()` is NOT the right predicate
    /// here and using it was my first mistake: `active_embedder_snapshot` carries a `#[cfg(test)]`
    /// guard that forces a stub unless `MURMUR_TEST_REAL_EMBED` is set, so a test build reports the
    /// model absent even on a machine whose model directory is fully populated — deliberately, so
    /// the suite never loads 470 MB of weights. The two therefore disagree BY DESIGN in test builds,
    /// and a test that asserted they agree fails on exactly the machines that have the model. What
    /// is deterministic, and what this pins, is the stub branch — the branch every user without the
    /// model is on, and the one that was producing the noise.
    #[test]
    fn a_stub_embedder_yields_no_handle_rather_than_fake_vectors() {
        if std::env::var_os("MURMUR_TEST_REAL_EMBED").is_some() {
            // Opted into the real model: the handle must then exist, or Ask would silently lose its
            // semantic leg on the machines that paid for it.
            assert!(
                crate::embed::active_persistence_embedder_if_available().is_some(),
                "with the real embedder opted in, the real-only handle must be available"
            );
            return;
        }
        assert!(
            crate::embed::active_persistence_embedder_if_available().is_none(),
            "a stub snapshot must produce NO handle. If this returns a stub embedder, the Ask path \
             embeds the question into hash noise and fuses it into the answer as if it meant \
             something"
        );
    }

    /// Every hybrid/doc search on the Ask path uses the REAL cosine floor, never `0.0`.
    ///
    /// `0.0` admits every vector in the index, however unrelated. That is harmless when the vector
    /// is meaningful and actively harmful when it is not: without the embedding model installed the
    /// old code fell back to `StubEmbedder`, a hash bag whose "similarity" carries no semantics, and
    /// a floor of `0.0` then fused that noise into the answer as if it were a real leg. Two bugs
    /// compounding — a fake vector and a threshold that could not reject it.
    ///
    /// The floor is only half the fix; the other half is that `commands::ask` now uses a real-only
    /// embedder handle, so with no model the vector is EMPTY and the KNN leg drops out entirely
    /// rather than being filtered. This test pins the half that lives in this file, by reading the
    /// source rather than by driving a search — a behavioural test would need the real 384-dim model
    /// present, which CI does not have, and would silently pass on the stub.
    #[test]
    fn the_ask_path_never_searches_with_a_zero_cosine_floor() {
        let src = include_str!("vault_context.rs");
        for (call, needle) in [
            ("search_hybrid_visible", "search_hybrid_visible"),
            ("search_doc_chunks_visible", "search_doc_chunks_visible"),
        ] {
            for (i, line) in src.lines().enumerate() {
                if !line.contains(needle) {
                    continue;
                }
                // The floor may sit on the call line or a few lines below it once rustfmt splits a
                // long call across lines.
                // Match the argument in BOTH shapes rustfmt produces: inline (`, 0.0,`) and, once
                // the call is split across lines, as a line that is nothing but `0.0,`. The first
                // version of this test only knew the inline form, so reverting the floor — which
                // rustfmt then reformatted onto its own line — left it green. A guard whose needle
                // depends on formatting is not a guard.
                let window: Vec<&str> = src.lines().skip(i).take(8).collect();
                let joined = window.join(" ");
                let lone_zero = window.iter().any(|l| l.trim() == "0.0,");
                assert!(
                    !joined.contains(", 0.0,") && !lone_zero,
                    "{call} at line {} passes a 0.0 cosine floor — that admits every vector in the \
                     index, which is exactly how stub-embedder noise reached the answer. Use \
                     `crate::embed::KNN_SEARCH_COSINE_FLOOR`.",
                    i + 1
                );
            }
        }
    }
}
