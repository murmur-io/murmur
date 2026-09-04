//! brain2 RAG Phase 4 — RETRIEVAL-AUGMENTED NOTE GENERATION.
//!
//! Ground each NEW meeting note in a small corpus of related PRIOR notes so notes compound
//! ("last time you decided X / you still owe Y") instead of being isolated. Uses the LIVE FTS5
//! retrieval (already shipped) + the existing provider — NO local/embedding model. This is ALWAYS
//! ON (no config flag): the pipeline unconditionally builds + injects the gated corpus via
//! `pipeline::build_grounding_context`, best-effort (an empty result or a retrieval error yields
//! `related_context = None`, byte-identical to the no-context prompt).
//!
//! LOCK INVARIANT (load-bearing): the corpus this builds is injected into the summarization prompt
//! and therefore EGRESSES to the cloud provider. It MUST contain ONLY visible (not
//! sealed-and-not-session-unlocked) prior notes. We enforce that exactly like `vault_context.rs`:
//! candidate selection goes through `Db::search_visible` and EVERY note body is pulled through the
//! second gate `Db::get_note_if_visible` — both keyed on the live `unlocked` session set. A
//! sealed-not-unlocked related meeting contributes NOTHING. We also EXCLUDE the meeting being
//! summarized itself, so a note is never grounded in its own (yet-to-exist) prior self.

use std::collections::{HashMap, HashSet};

use crate::enrich::ContextHit;
use crate::error::Result;
use crate::storage::models::VaultSource;
use crate::storage::Db;

/// Max related notes to pack into the grounding corpus. Kept small (focused, high-precision
/// grounding) and well under the Ask-My-Vault corpus size — this is context, not a research dump.
const MAX_RELATED_NOTES: usize = 4;

/// Min token length kept for the salient query (drops "the", "do", "i", short filler).
const MIN_TERM_LEN: usize = 3;

/// Cap on the number of salient terms in the derived FTS query.
const MAX_QUERY_TERMS: usize = 8;

/// WEAK on-device providers: tiny-context, low-instruction-following local models whose failure mode
/// is COPYING corpus text verbatim (the confirmed cross-meeting bleed where a qwen-4B pasted another
/// note's `## Action items` into a new note). For these the grounding corpus carries ONLY LINKABLE
/// context (a `[[Title]]` header) — never a copyable body — so there is nothing to plagiarize.
/// `pub(crate)` so the pipeline/tests can reason about the same classification.
pub(crate) fn is_weak_provider(provider_id: &str) -> bool {
    provider_id == crate::summarize::roles::CONN_LOCAL
        || provider_id == crate::summarize::roles::CONN_AFM
        || provider_id == crate::summarize::PROVIDER_OLLAMA
}

/// Char budget for the grounding corpus, by provider. Smaller than the Ask-My-Vault budget on
/// purpose — this rides ON TOP of the full transcript in the same prompt, so it must stay lean.
/// Weak local models (Ollama / on-device brain / Apple) have tiny context windows → cap much tighter.
pub(crate) fn budget_for(provider_id: &str) -> usize {
    if is_weak_provider(provider_id) {
        3_000
    } else {
        24_000
    }
}

/// Basic EN + PL stopword set. Deliberately small + deterministic (no external word-list dep); it
/// strips the highest-frequency function words so the salient query keys off content terms.
/// `pub(crate)` so the grounding pass (`summarize::grounding`) reuses the SAME word list — one
/// source of truth, no second stopword set to drift.
pub(crate) fn is_stopword(t: &str) -> bool {
    const STOP: &[&str] = &[
        // English
        "the", "and", "for", "are", "but", "not", "you", "your", "all", "can", "had", "her", "was",
        "one", "our", "out", "has", "him", "his", "how", "man", "new", "now", "old", "see", "two",
        "way", "who", "did", "get", "may", "she", "use", "that", "this", "with", "have", "from",
        "they", "will", "would", "there", "their", "what", "about", "which", "when", "were",
        "been", "them", "then", "than", "some", "into", "just", "like", "also", "well", "yeah",
        "okay", "going", "gonna", "really", "actually", "think", "know", "want", "need", "let",
        "yes", "kind", "sort", "thing", "things", "stuff", "much", "very", "more", "most", "such",
        "only", // Polish
        "nie", "tak", "jest", "się", "sie", "tego", "tym", "tych", "tam", "jak", "czy", "ale",
        "lub", "albo", "oraz", "dla", "tylko", "też", "tez", "już", "juz", "bez", "być", "byc",
        "jako", "który", "ktory", "która", "ktora", "które", "ktore", "tutaj", "teraz", "wtedy",
        "bardzo", "trochę", "troche", "może", "moze", "żeby", "zeby", "przez", "przy", "pod",
        "nad", "tu", "to", "co", "na", "we", "do", "od", "po", "za", "ze", "oraz", "więc", "wiec",
        // Polish question words + the "być" copula's inflected forms — high-frequency function words
        // that over-constrain an exact-term FTS query for a spoken question ("jaka była pogoda" must
        // key off "pogoda", not the auxiliary "jaka"/"była"). Diacritic-stripped forms too.
        "jaka", "jaki", "jakie", "jacy", "jakas", "jakaś", "kto", "kogo", "kim", "gdzie", "kiedy",
        "ile", "dlaczego", "czyj", "czyja", "był", "byl", "była", "byla", "było", "bylo", "były",
        "byly", "byłem", "bylem", "byłam", "bylam", "jestem", "jesteś", "jestes", "będzie",
        "bedzie",
    ];
    STOP.contains(&t)
}

/// Tokenize to lowercased alphanumeric tokens (Unicode-aware; handles PL diacritics). `pub(crate)`
/// so the grounding pass (`summarize::grounding`) tokenizes summary units + transcript segments the
/// SAME way the retrieval query does — no divergent tokenizer.
pub(crate) fn tokenize(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .collect()
}

/// Max chars of `reference_gist` output — a short, linkable one-liner, never a copyable block.
const GIST_MAX_CHARS: usize = 280;

/// The lowercased, `#`-stripped title of a markdown heading line (e.g. `"## Action Items"` →
/// `"action items"`). One source of truth for the EN+PL heading matchers below.
fn section_title(heading_line: &str) -> String {
    heading_line.trim_start_matches('#').trim().to_lowercase()
}

/// A heading that opens a SUMMARY-style section whose prose is the ideal task-free gist. EN + PL,
/// suffix-tolerant (`starts_with`) so `"Summary of the call"` / `"Podsumowanie spotkania"` match.
fn is_summary_heading(heading_line: &str) -> bool {
    let t = section_title(heading_line);
    const KEYS: &[&str] = &[
        // English
        "summary",
        "tl;dr",
        "overview", // Polish
        "podsumowanie",
        "streszczenie",
        "przegląd",
    ];
    KEYS.iter().any(|k| t.starts_with(k))
}

/// A heading that opens a COPYABLE, task-like section whose contents must NEVER enter the gist —
/// action items / decisions / tasks / next-steps / follow-ups / risks. EN + PL, suffix-tolerant.
/// This is the anti-bleed core: a weak model copies these verbatim, so the corpus never carries them.
fn is_forbidden_section_heading(heading_line: &str) -> bool {
    let t = section_title(heading_line);
    const KEYS: &[&str] = &[
        // English
        "action items",
        "decisions",
        "tasks",
        "next steps",
        "follow-ups",
        "follow ups",
        "risks",
        // Polish
        "zadania",
        "działania",
        "do zrobienia",
        "elementy do wykonania",
        "decyzje",
        "ustalenia",
        "postanowienia",
        "następne kroki",
        "kolejne kroki",
        "dalsze kroki",
        "działania następcze",
        "ryzyka",
        "ryzyko",
    ];
    KEYS.iter().any(|k| t.starts_with(k))
}

/// Whether a trimmed line is a Markdown CHECKLIST item (`- [ ]` / `- [x]` / `* [ ]` / `+ [X]` …) —
/// a language-independent task marker that must never appear in the gist.
fn is_checklist_line(trimmed: &str) -> bool {
    let b = trimmed.as_bytes();
    b.len() >= 5
        && matches!(b[0], b'-' | b'*' | b'+')
        && b[1] == b' '
        && b[2] == b'['
        && matches!(b[3], b' ' | b'x' | b'X')
        && b[4] == b']'
}

/// Strip a leading plain-bullet / numbered-list marker so a summary bullet reads as prose. Leaves a
/// non-list line untouched.
fn strip_list_marker(trimmed: &str) -> &str {
    for b in ["- ", "* ", "+ "] {
        if let Some(r) = trimmed.strip_prefix(b) {
            return r.trim_start();
        }
    }
    let digits = trimmed.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0 {
        let after = &trimmed[digits..];
        if let Some(r) = after
            .strip_prefix(". ")
            .or_else(|| after.strip_prefix(") "))
        {
            return r.trim_start();
        }
    }
    trimmed
}

/// Strip a leading YAML front-matter block (`---\n … \n---\n`), returning the body. A note that is
/// only an unterminated front-matter block yields an empty body.
fn strip_frontmatter(md: &str) -> &str {
    let Some(rest) = md.strip_prefix("---\n") else {
        return md;
    };
    match rest.find("\n---\n") {
        Some(pos) => &rest[pos + "\n---\n".len()..],
        None => "",
    }
}

/// A SHORT (≤ [`GIST_MAX_CHARS`], char-boundary-safe), TASK-FREE gist of a note — the LINKABLE
/// context a related-note corpus may carry WITHOUT handing a weak model something copyable.
///
/// Anti-bleed contract (the fix for the confirmed cross-meeting `## Action items` copy): the gist is
/// the note's SUMMARY-section prose, or — absent a summary — the FIRST non-heading prose paragraph
/// OUTSIDE any task/decision section. It NEVER contains an action-items / decisions / tasks /
/// next-steps / follow-ups / risks section (EN or PL — see [`is_forbidden_section_heading`]) nor any
/// checklist line. Pure + dependency-free (no DB, no egress); YAML front-matter is stripped first.
pub fn reference_gist(markdown: &str) -> String {
    let body = strip_frontmatter(markdown);

    // Section kinds we walk through: Summary (preferred prose), Forbidden (never entered), Other
    // (preamble + any non-task heading — the fallback source). We collect the summary prose AND the
    // first Other/preamble prose paragraph in one pass; summary wins if present.
    #[derive(PartialEq)]
    enum Kind {
        Summary,
        Forbidden,
        Other,
    }
    let mut kind = Kind::Other; // preamble (before any heading) is fallback-eligible
    let mut in_code = false;
    let mut summary: Vec<String> = Vec::new();
    let mut fallback: Vec<String> = Vec::new();
    let mut fallback_done = false;

    for raw in body.split('\n') {
        let t = raw.trim();

        if t.starts_with("```") || t.starts_with("~~~") {
            in_code = !in_code;
            if !fallback.is_empty() {
                fallback_done = true; // a fence ends the current fallback paragraph
            }
            continue;
        }
        if in_code {
            continue;
        }
        if t.starts_with('#') {
            kind = if is_summary_heading(t) {
                Kind::Summary
            } else if is_forbidden_section_heading(t) {
                Kind::Forbidden
            } else {
                Kind::Other
            };
            if !fallback.is_empty() {
                fallback_done = true; // a heading ends the current fallback paragraph
            }
            continue;
        }
        if t.is_empty() {
            if !fallback.is_empty() {
                fallback_done = true; // a blank line ends the current fallback paragraph
            }
            continue;
        }
        // Never let a checklist line or a blockquote/callout into the gist.
        if is_checklist_line(t) || t.starts_with('>') {
            continue;
        }
        let text = strip_list_marker(t).trim();
        if text.is_empty() {
            continue;
        }
        match kind {
            Kind::Summary => summary.push(text.to_string()),
            Kind::Forbidden => {} // task/decision content is NEVER copyable context
            Kind::Other => {
                if !fallback_done {
                    fallback.push(text.to_string());
                }
            }
        }
    }

    let gist = if !summary.is_empty() {
        summary.join(" ")
    } else {
        fallback.join(" ")
    };
    let gist = gist.trim();
    // Char-boundary-safe truncation (collect by chars, never slice mid-codepoint — PL diacritics).
    if gist.chars().count() <= GIST_MAX_CHARS {
        gist.to_string()
    } else {
        gist.chars().take(GIST_MAX_CHARS).collect()
    }
}

/// Derive a short, deterministic FTS query from a meeting: the title's content tokens first (the
/// strongest signal), then the top-frequency non-stopword tokens from the transcript, deduped and
/// capped at [`MAX_QUERY_TERMS`]. Pure + unit-testable. Ties in frequency break by first-appearance
/// order, so the output is fully deterministic for a given input. An empty/`None` title is fine —
/// the transcript alone drives the query.
pub fn salient_query(title: Option<&str>, transcript: &str) -> String {
    let mut terms: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    // 1. Title content tokens, in order — the most salient signal for "what is this about".
    if let Some(t) = title {
        for tok in tokenize(t) {
            if terms.len() >= MAX_QUERY_TERMS {
                break;
            }
            if tok.chars().count() < MIN_TERM_LEN || is_stopword(&tok) {
                continue;
            }
            if seen.insert(tok.clone()) {
                terms.push(tok);
            }
        }
    }

    // 2. Top-frequency transcript content tokens (count desc, then first-seen asc for determinism).
    let mut counts: Vec<(String, usize, usize)> = Vec::new(); // (token, count, first_index)
    let mut index: HashMap<String, usize> = HashMap::new();
    for (i, tok) in tokenize(transcript).into_iter().enumerate() {
        if tok.chars().count() < MIN_TERM_LEN || is_stopword(&tok) {
            continue;
        }
        match index.get(&tok) {
            Some(&pos) => counts[pos].1 += 1,
            None => {
                index.insert(tok.clone(), counts.len());
                counts.push((tok, 1, i));
            }
        }
    }
    counts.sort_by(|a, b| b.1.cmp(&a.1).then(a.2.cmp(&b.2)));
    for (tok, _, _) in counts {
        if terms.len() >= MAX_QUERY_TERMS {
            break;
        }
        if seen.insert(tok.clone()) {
            terms.push(tok);
        }
    }

    terms.join(" ")
}

/// PHASE 7 — the SHARED, gated candidate retriever behind BOTH [`build_related_context`] (the
/// grounding corpus) and [`related_note_links`] (Lane A links). ONE source of truth for the
/// FTS↔hybrid switch + the fallback rules, so the two callers can never drift.
///
/// - `embedder = Some(e)` (the real e5 model is present) ⇒ embed `query` with the e5 `query:` prefix
///   and retrieve via [`Db::search_hybrid_visible`] (FTS ⊕ semantic-KNN ⊕ entity-graph, RRF-fused).
/// - `embedder = None` (no model), OR an embed that yields an empty/degenerate vector, OR an embedder
///   error ⇒ EXACTLY today's [`Db::search_visible`] FTS path — NO vector is computed, NO `vec_chunks`
///   row is touched (NEVER a stub vector), byte-identical to the pre-Phase-7 behaviour. A semantic
///   error degrades to FTS, never fails the caller.
///
/// LOCK INVARIANT: both retrievers apply the SAME `visibility_clause` (semantic + FTS + graph legs are
/// each gated), so every candidate is FIRST-gate visible on the live `unlocked` set regardless of
/// which branch runs. The SECOND gate ([`Db::get_note_if_visible`]) stays in each caller.
fn gated_candidates(
    db: &Db,
    query: &str,
    limit: i64,
    unlocked: &HashSet<String>,
    embedder: Option<&dyn crate::embed::Embedder>,
) -> Result<Vec<crate::storage::SearchHit>> {
    // CONTRACT (both paths): a query with no usable terms yields NO candidates. The FTS path honors
    // this via `fts_match_query("")==None`, but the semantic path would NOT: an empty/whitespace query
    // still embeds to a NON-empty vector, and `search_semantic_visible`'s KNN has no similarity floor,
    // so it would return an ARBITRARY nearest note — a spurious cross-meeting link/grounding for a
    // low-content (all-stopwords → `salient_query==""`) meeting whenever the e5 model is present
    // (Phase-7 precision finding). Short-circuit here so the contract holds identically on both paths
    // (and a construction-failed stub embedder can't turn a degenerate query into garbage KNN either).
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }
    let Some(e) = embedder else {
        // No real model ⇒ the pre-Phase-7 FTS path, no vector touched.
        return db.search_visible(query, limit, unlocked);
    };
    // QUERY side: e5 `query:` prefix (asymmetric with the `passage:` index side), matching the
    // MCP/Ask query path in `tools.rs`.
    match e.embed_query(std::slice::from_ref(&query.to_string())) {
        Ok(v) => match v.into_iter().next() {
            // No temporal window here: the grounding query is a derived salient-term string, not
            // a user question (a temporal phrase in it would be note prose, not intent).
            Some(qv) if !qv.is_empty() => {
                db.search_hybrid_visible(query, &qv, limit, 0.0, unlocked, None, None)
            }
            // Empty/degenerate query vector → FTS (never a stub-vector KNN).
            _ => db.search_visible(query, limit, unlocked),
        },
        Err(err) => {
            tracing::warn!(
                target: "embed",
                error = %err,
                "related-context: query embed failed; falling back to FTS candidate retrieval"
            );
            db.search_visible(query, limit, unlocked)
        }
    }
}

/// Build the GATED related-prior-notes corpus for grounding a new note. Returns `(corpus, sources)`.
///
/// - Candidate meetings come from [`Db::search_visible`] against the live `unlocked` set (FIRST
///   gate) — a sealed-not-unlocked meeting is never even a candidate.
/// - The meeting being summarized (`this_meeting_id`) is EXCLUDED — a note is never grounded in
///   itself.
/// - Each note body is pulled through [`Db::get_note_if_visible`] (SECOND gate) before any of its
///   content enters the corpus, so a sealed-not-unlocked note contributes nothing even if it
///   slipped through the candidate filter.
/// - The top [`MAX_RELATED_NOTES`] visible notes are packed under [`budget_for`], each headed by a
///   `### [[Title]] · date · id:` citation so the model can cite `[[Title]]`.
///
/// An empty/whitespace `query` (e.g. a transcript that was all stopwords) yields an empty corpus —
/// the retriever returns nothing for a query with no usable terms.
///
/// PHASE 7 — SEMANTIC RECALL (candidate retrieval only): mirrors [`related_note_links`]. WHEN the
/// real e5 model is present ([`crate::embed::embed_model_present`]) candidates come from the gated
/// HYBRID retriever ([`Db::search_hybrid_visible`]); ABSENT the model, EXACTLY today's gated FTS
/// [`Db::search_visible`] with NO vector computed (byte-identical, never a stub vector). The double
/// visibility gate, self-exclusion, budget, weak-provider header-only rule and [`reference_gist`] are
/// all UNCHANGED — only the CANDIDATE query switches. The embedder is resolved internally, so this
/// signature (and the `pipeline` call site) is untouched.
pub fn build_related_context(
    db: &Db,
    this_meeting_id: &str,
    query: &str,
    unlocked: &HashSet<String>,
    provider_id: &str,
) -> Result<(String, Vec<VaultSource>)> {
    let budget = budget_for(provider_id);
    // Over-fetch a little: the self-meeting and any second-gate misses are dropped below, so ask for
    // more candidates than we intend to keep.
    let embedder = crate::embed::embed_model_present().then(crate::embed::active_embedder);
    let hits = gated_candidates(
        db,
        query,
        (MAX_RELATED_NOTES as i64) + 6,
        unlocked,
        embedder.as_deref(),
    )?;

    let mut corpus = String::new();
    let mut sources: Vec<VaultSource> = Vec::new();
    for hit in hits {
        if sources.len() >= MAX_RELATED_NOTES || corpus.len() >= budget {
            break;
        }
        let m = hit.meeting;
        // Never ground a note in itself.
        if m.id == this_meeting_id {
            continue;
        }
        // Second gate: only pull the body if the note is visible under the live unlock set.
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
        corpus.push_str(&header);
        // ANTI-BLEED: WEAK providers (see `is_weak_provider`) get the LINKABLE `[[Title]]` header
        // ONLY — nothing copyable, so a low-instruction-following local model cannot paste another
        // meeting's `## Action items` into this note. STRONG providers additionally get a SHORT,
        // task-free `reference_gist` (never a full note body, never a task/decision section).
        if !is_weak_provider(provider_id) {
            let gist = reference_gist(&note.markdown);
            let chunk: String = gist.chars().take(remaining).collect();
            corpus.push_str(&chunk);
        }
        sources.push(VaultSource {
            meeting_id: m.id,
            title,
            started_at: m.started_at,
            origin: None,
        });
    }

    Ok((corpus, sources))
}

/// Stage 2 / Lane A — build the DETERMINISTIC, ZERO-EGRESS cross-meeting LINK set for a FINISHED
/// note. Mirrors [`build_related_context`]'s double gate EXACTLY — candidates via
/// [`Db::search_visible`] (FIRST gate on the live `unlocked` set), self-exclusion of the note being
/// linked, and each body pulled through [`Db::get_note_if_visible`] (SECOND gate) — but emits
/// LINKABLE [`ContextHit`]s (a `[[Title]]` wikilink + a TASK-FREE [`reference_gist`]) instead of a
/// copyable grounding corpus.
///
/// LOCK INVARIANT: a sealed-and-NOT-session-unlocked related meeting contributes NO link (the two
/// gates on the live `unlocked` set), and the meeting being linked is never linked to itself. Because
/// the detail is a `reference_gist` (task-free by construction — never an action-items / decisions /
/// tasks section, EN+PL), a linked note can never drag its checklist into this note.
///
/// NO provider, NO network — Lane A egresses NOTHING (search over OWNED notes + local gist only), so
/// it is auto-eligible on finalize. Notes with an empty gist (nothing task-free to show) are skipped.
///
/// PHASE 7 — SEMANTIC RECALL (candidate retrieval only): WHEN the REAL on-device e5 model is present
/// ([`crate::embed::embed_model_present`]) the candidate list comes from the HYBRID retriever
/// ([`Db::search_hybrid_visible`] = FTS ⊕ semantic-KNN ⊕ entity-graph, RRF-fused) so a
/// same-meaning-different-words prior note is found (embedding recall ≈ 1.0 in the repo bake-off vs
/// ≈ 0.42 keyword FTS). WHEN the model is ABSENT the retriever is EXACTLY today's FTS
/// [`Db::search_visible`] — no vector is embedded, no `vec_chunks` row is touched, byte-identical to
/// the pre-Phase-7 behaviour (NEVER a stub vector: a no-model install runs pure FTS). The embedder is
/// resolved HERE (no signature change ripples to the pipeline/command call sites), and any
/// embedder-construction / embed error falls back to FTS rather than failing the pass. The DOUBLE
/// visibility gate is unchanged: the retriever gates every candidate (`visibility_clause`) and each
/// body is re-checked through [`Db::get_note_if_visible`] on the live `unlocked` set.
pub fn related_note_links(
    db: &Db,
    this_meeting_id: &str,
    query: &str,
    unlocked: &HashSet<String>,
    max: usize,
) -> Result<Vec<ContextHit>> {
    // Activate semantic recall ONLY on real-model presence; else the embedder is None → FTS.
    let embedder = crate::embed::embed_model_present().then(crate::embed::active_embedder);
    related_note_links_with_embedder(
        db,
        this_meeting_id,
        query,
        unlocked,
        max,
        embedder.as_deref(),
    )
}

/// Deterministic core of [`related_note_links`] with the embedder injected (so gating/semantic tests
/// can drive the HYBRID branch with a known [`crate::embed::Embedder`] + pre-indexed vectors WITHOUT
/// a real model on disk — mirrors `Db::related_meetings_visible`'s injected-embedder idiom).
///
/// `embedder = Some(e)` ⇒ embed `query` (e5 `query:` prefix) and retrieve candidates via the gated
/// [`Db::search_hybrid_visible`]. `embedder = None` (or an embed that yields an empty/failed vector)
/// ⇒ the pre-Phase-7 [`Db::search_visible`] FTS path, byte-identical, with NO vector computed. The
/// self-exclusion, [`Db::get_note_if_visible`] body gate, task-free [`reference_gist`] and `max` cap
/// are IDENTICAL on both branches — only the CANDIDATE query changes.
pub(crate) fn related_note_links_with_embedder(
    db: &Db,
    this_meeting_id: &str,
    query: &str,
    unlocked: &HashSet<String>,
    max: usize,
    embedder: Option<&dyn crate::embed::Embedder>,
) -> Result<Vec<ContextHit>> {
    // Over-fetch a little: the self-meeting, second-gate misses, and empty-gist notes are dropped
    // below, so ask for more candidates than we intend to keep.
    let hits = gated_candidates(db, query, (max as i64) + 6, unlocked, embedder)?;
    let mut out: Vec<ContextHit> = Vec::new();
    for hit in hits {
        if out.len() >= max {
            break;
        }
        let m = hit.meeting;
        // Never link a note to itself.
        if m.id == this_meeting_id {
            continue;
        }
        // SECOND gate: only pull the body if the note is visible under the live unlock set — a
        // sealed-not-unlocked note contributes NO link even if it slipped the candidate filter.
        let Some(note) = db.get_note_if_visible(&m.id, unlocked)? else {
            continue;
        };
        // The link detail is the TASK-FREE gist — never a copyable action-items/decisions section.
        let gist = reference_gist(&note.markdown);
        if gist.trim().is_empty() {
            continue; // nothing task-free to show for this note → no link.
        }
        let title = m.title.clone().unwrap_or_else(|| "(untitled)".to_string());
        out.push(ContextHit {
            source: "Murmur".to_string(),
            detail: gist,
            url: Some(format!("[[{title}]]")),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::models::{Folder, Meeting, MeetingStatus, NoteRecord};

    const TEST_DEK: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn temp_db() -> Db {
        let p = crate::storage::db::unique_temp_path("meetnotes-relctx-test", "sqlite");
        Db::open_with_key(&p, TEST_DEK).unwrap()
    }

    fn seed_note(db: &Db, id: &str, title: &str, markdown: &str, folder: Option<&str>) {
        db.insert_meeting(&Meeting {
            id: id.to_string(),
            started_at: "2026-06-20T09:00:00Z".to_string(),
            ended_at: None,
            title: Some(title.to_string()),
            duration_s: 60,
            audio_path: None,
            status: MeetingStatus::Summarized,
            folder_id: None,
        })
        .unwrap();
        db.upsert_note(&NoteRecord {
            meeting_id: id.to_string(),
            provider_id: "claude_code".to_string(),
            markdown: markdown.to_string(),
            created_at: "2026-06-20T09:05:00Z".to_string(),
            exported_path: None,
            model_requested: None,
            model_served: None,
            gateway_host: None,
        })
        .unwrap();
        db.set_note_folder(id, folder).unwrap();
    }

    fn seed_folder(db: &Db, id: &str, locked: bool) {
        db.insert_folder(&Folder {
            id: id.to_string(),
            name: id.to_string(),
            path: id.to_string(),
            parent_id: None,
            locked,
            created_at: "2026-06-19T00:00:00Z".to_string(),
        })
        .unwrap();
    }

    // ── salient_query: deterministic, stopword-stripped, capped ──────────────────────────────────

    #[test]
    fn salient_query_strips_stopwords_and_keeps_content() {
        let q = salient_query(
            Some("Budget Planning"),
            "We decided the budget and the runway for hiring. The budget matters for hiring.",
        );
        // Title content terms appear; stopwords ("the", "and", "for", "we") are gone.
        assert!(q.contains("budget"));
        assert!(q.contains("planning"));
        assert!(q.contains("hiring"));
        for stop in ["the", "and", "for", " we "] {
            assert!(!format!(" {q} ").contains(stop), "stopword leaked: {stop}");
        }
    }

    #[test]
    fn salient_query_caps_term_count() {
        let transcript = "alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima";
        let q = salient_query(Some("Mike November Oscar Papa Quebec"), transcript);
        assert!(
            q.split_whitespace().count() <= MAX_QUERY_TERMS,
            "query must be capped at {MAX_QUERY_TERMS} terms, got: {q}"
        );
    }

    #[test]
    fn salient_query_handles_empty_title() {
        let q = salient_query(None, "Apollo migration migration migration deadline");
        // Most frequent content term ("migration") leads; deterministic for the same input.
        assert!(q.starts_with("migration"), "got: {q}");
        assert_eq!(
            q,
            salient_query(None, "Apollo migration migration migration deadline")
        );
    }

    #[test]
    fn salient_query_empty_when_all_stopwords() {
        assert_eq!(salient_query(Some("the and"), "the and for we to do"), "");
    }

    // ── build_related_context: GATED retrieval + self-exclusion ──────────────────────────────────

    /// Flag-ON happy path: a related VISIBLE note is retrieved + cited, and the meeting being
    /// summarized is EXCLUDED (never grounds itself).
    #[test]
    fn build_related_context_cites_visible_and_excludes_self() {
        let db = temp_db();
        // The meeting we're "summarizing" (its own note already exists in this test fixture).
        seed_note(
            &db,
            "this",
            "Q3 Planning",
            "ACME quarterly planning roadmap",
            None,
        );
        // A genuinely related prior, open folder → visible.
        seed_note(
            &db,
            "prior",
            "Q2 Planning",
            "ACME quarterly planning roadmap decisions",
            None,
        );

        let nothing = HashSet::new();
        let (corpus, sources) = build_related_context(
            &db,
            "this",
            "ACME quarterly planning",
            &nothing,
            "anthropic",
        )
        .unwrap();

        assert!(
            corpus.contains("[[Q2 Planning]]"),
            "related note must be cited"
        );
        assert!(corpus.contains("id:prior"));
        assert!(sources.iter().any(|s| s.meeting_id == "prior"));
        // Self-exclusion: the meeting being summarized is never in its own grounding corpus.
        assert!(
            !corpus.contains("id:this"),
            "a note must never be grounded in itself"
        );
        assert!(sources.iter().all(|s| s.meeting_id != "this"));
    }

    /// LOCK INVARIANT (RED-if-ungated): a sealed-and-NOT-session-unlocked related meeting must
    /// contribute NOTHING to the corpus (it would otherwise egress to the cloud provider), and must
    /// reappear once its folder is session-unlocked. This is the gate that an ungated `search` +
    /// `get_latest_note_for_meeting` would fail.
    #[test]
    fn build_related_context_excludes_sealed_until_unlocked() {
        let db = temp_db();
        seed_note(
            &db,
            "this",
            "Acquisition Talk",
            "PROJECT atlas acquisition terms",
            None,
        );
        seed_folder(&db, "f-locked", false);
        seed_note(
            &db,
            "sealed",
            "Secret Acquisition",
            "PROJECT atlas acquisition price 5_000_000 SEALED-SECRET",
            Some("f-locked"),
        );
        // Seal the folder (flip locked=1). visibility_clause keys off folders.locked.
        db.set_folder_locked("f-locked", true, None).unwrap();

        // Not session-unlocked → the sealed related note MUST be absent from the cloud-bound corpus.
        let nothing = HashSet::new();
        let (corpus, sources) = build_related_context(
            &db,
            "this",
            "PROJECT atlas acquisition",
            &nothing,
            "anthropic",
        )
        .unwrap();
        assert!(
            !corpus.contains("SEALED-SECRET"),
            "sealed-not-unlocked related content leaked into the cloud grounding corpus (gate violation)"
        );
        assert!(sources.iter().all(|s| s.meeting_id != "sealed"));

        // Session-unlock the folder → the related note is now legitimately available + cited.
        let mut unlocked = HashSet::new();
        unlocked.insert("f-locked".to_string());
        let (corpus2, sources2) = build_related_context(
            &db,
            "this",
            "PROJECT atlas acquisition",
            &unlocked,
            "anthropic",
        )
        .unwrap();
        assert!(
            corpus2.contains("SEALED-SECRET"),
            "unlocked related content must reappear"
        );
        assert!(sources2.iter().any(|s| s.meeting_id == "sealed"));
    }

    // ── reference_gist: task-free, EN+PL heading-aware, char-boundary-safe ────────────────────────

    /// The gist keeps the SUMMARY prose and DROPS the `## Action items` section + its checklist
    /// lines (the copyable content that bled cross-meeting). Front-matter is stripped first.
    #[test]
    fn reference_gist_keeps_summary_drops_tasks() {
        let note = "---\ntitle: X\n---\n\n## Summary\n\nWe reviewed the bed comfort design and the memory foam felt better.\n\n## Action items\n\n- [ ] Weronika weryfikować Alcon\n- [ ] Order more foam samples\n";
        let gist = reference_gist(note);
        assert!(
            gist.contains("memory foam felt better"),
            "summary prose must be kept; got: {gist}"
        );
        assert!(
            !gist.contains("Weronika"),
            "action items must be excluded; got: {gist}"
        );
        assert!(
            !gist.contains("foam samples"),
            "checklist tasks must be excluded; got: {gist}"
        );
        assert!(
            !gist.contains("Action items"),
            "the heading must not be in the gist; got: {gist}"
        );
    }

    /// CORRECTION #3 (Polish leak), RED-before-GREEN: a Polish note whose FIRST section is a prose
    /// `## Decyzje` (decisions) must NOT leak the decision text into the gist — with PL recognition
    /// the gist falls through to the next non-task section. Without `"decyzje"` in the forbidden
    /// list this test goes RED (the decision bullet becomes the fallback prose).
    #[test]
    fn reference_gist_polish_decyzje_never_leaks() {
        let note = "## Decyzje\n\n- Weronika ma weryfikować rampę Alcon do piątku.\n\n## Notatki\n\nJakieś inne uwagi na później.\n";
        let gist = reference_gist(note);
        assert!(
            !gist.contains("Weronika"),
            "PL decisions must not leak into the gist; got: {gist}"
        );
        assert!(
            !gist.contains("Alcon"),
            "PL decisions must not leak into the gist; got: {gist}"
        );
        assert!(
            gist.contains("Jakieś inne uwagi"),
            "the first non-task prose must be the gist; got: {gist}"
        );
    }

    /// A Polish SUMMARY heading (`## Podsumowanie`) is recognized, and a trailing PL `## Zadania`
    /// checklist section never leaks.
    #[test]
    fn reference_gist_polish_summary_recognized() {
        let note = "## Podsumowanie\n\nRozmawialiśmy o komforcie łóżka i wygodzie materaca.\n\n## Zadania\n\n- [ ] Weronika weryfikować rampę Alcon\n";
        let gist = reference_gist(note);
        assert!(
            gist.contains("komforcie łóżka"),
            "PL summary prose must be kept; got: {gist}"
        );
        assert!(
            !gist.contains("Weronika"),
            "PL tasks must not leak; got: {gist}"
        );
    }

    /// Char-boundary-safe truncation: a long multi-byte (PL diacritic) summary is capped at 280
    /// CHARS without panicking on a codepoint boundary.
    #[test]
    fn reference_gist_truncates_on_char_boundary() {
        let body = "ą".repeat(300); // 300 two-byte chars
        let note = format!("## Summary\n\n{body}\n");
        let gist = reference_gist(&note);
        assert!(gist.chars().count() <= 280, "gist must be ≤ 280 chars");
        assert!(
            gist.chars().count() >= 200,
            "a long summary must still produce a gist"
        );
    }

    // ── build_related_context: weak providers get NOTHING copyable ────────────────────────────────

    /// A WEAK provider (ollama / on-device brain / apple) gets the `[[Title]]` header ONLY — never
    /// the summary prose, and NEVER an action item — so it cannot copy another meeting's tasks.
    #[test]
    fn build_related_context_weak_provider_header_only() {
        let db = temp_db();
        seed_note(
            &db,
            "this",
            "Bed Comfort",
            "unrelated bed comfort trial",
            None,
        );
        seed_note(
            &db,
            "prior",
            "Bed Comfort Review",
            "## Summary\n\nThe bed comfort trial went well and the SUPERCOMFY foam won.\n\n## Action items\n\n- [ ] Weronika weryfikować Alcon\n",
            None,
        );
        let nothing = HashSet::new();
        // provider_id = ollama → weak.
        let (corpus, sources) =
            build_related_context(&db, "this", "bed comfort trial foam", &nothing, "ollama")
                .unwrap();
        assert!(
            corpus.contains("[[Bed Comfort Review]]"),
            "header must be cited; got: {corpus}"
        );
        assert!(
            corpus.contains("id:prior"),
            "header id must be cited; got: {corpus}"
        );
        assert!(
            !corpus.contains("SUPERCOMFY"),
            "weak provider must NOT get the gist body; got: {corpus}"
        );
        assert!(
            !corpus.contains("Weronika"),
            "weak provider must NEVER get an action item; got: {corpus}"
        );
        assert!(sources.iter().any(|s| s.meeting_id == "prior"));
    }

    /// A STRONG provider DOES get the task-free gist prose (but still never the action items).
    #[test]
    fn build_related_context_strong_provider_gets_gist() {
        let db = temp_db();
        seed_note(
            &db,
            "this",
            "Bed Comfort",
            "unrelated bed comfort trial",
            None,
        );
        seed_note(
            &db,
            "prior",
            "Bed Comfort Review",
            "## Summary\n\nThe bed comfort trial went well and the SUPERCOMFY foam won.\n\n## Action items\n\n- [ ] Weronika weryfikować Alcon\n",
            None,
        );
        let nothing = HashSet::new();
        let (corpus, _sources) =
            build_related_context(&db, "this", "bed comfort trial foam", &nothing, "anthropic")
                .unwrap();
        assert!(
            corpus.contains("SUPERCOMFY"),
            "strong provider gets the gist prose; got: {corpus}"
        );
        assert!(
            !corpus.contains("Weronika"),
            "even a strong provider never gets copyable action items; got: {corpus}"
        );
    }

    // ── related_note_links (Lane A): GATED, self-excluded, task-free, zero-egress ─────────────────

    /// LOCK INVARIANT (RED-if-ungated) + self-exclusion + task-free gist, in one:
    /// - a sealed-and-NOT-session-unlocked related note contributes NO link, and reappears once its
    ///   folder is session-unlocked (the double gate an ungated `search`+`get_note` would fail);
    /// - the meeting being linked is never linked to itself;
    /// - the link detail is a task-free gist (no action items), and its url is `[[Title]]`.
    #[test]
    fn related_note_links_gated_self_excluded_and_task_free() {
        let db = temp_db();
        // The note we're linking FROM (its own content also matches the query → must be self-excluded).
        seed_note(
            &db,
            "this",
            "Acquisition Talk",
            "PROJECT atlas acquisition terms roadmap",
            None,
        );
        seed_folder(&db, "f-locked", false);
        seed_note(
            &db,
            "sealed",
            "Secret Acquisition",
            "## Summary\n\nPROJECT atlas acquisition roadmap review.\n\n## Action items\n\n- [ ] SEALED-SECRET follow-up\n",
            Some("f-locked"),
        );
        db.set_folder_locked("f-locked", true, None).unwrap();

        // Not session-unlocked → the sealed related note yields NO link, and "this" links nothing to itself.
        let nothing = HashSet::new();
        let links = related_note_links(
            &db,
            "this",
            "PROJECT atlas acquisition roadmap",
            &nothing,
            4,
        )
        .unwrap();
        assert!(
            links
                .iter()
                .all(|h| h.url.as_deref() != Some("[[Secret Acquisition]]")),
            "sealed-not-unlocked related note must not be linked (gate violation)"
        );
        assert!(
            links
                .iter()
                .all(|h| h.url.as_deref() != Some("[[Acquisition Talk]]")),
            "a note must never be linked to itself"
        );
        assert!(
            links.iter().all(|h| !h.detail.contains("SEALED-SECRET")),
            "sealed content must not leak into a link detail"
        );

        // Session-unlock the folder → the related note is now legitimately linkable.
        let mut unlocked = HashSet::new();
        unlocked.insert("f-locked".to_string());
        let links2 = related_note_links(
            &db,
            "this",
            "PROJECT atlas acquisition roadmap",
            &unlocked,
            4,
        )
        .unwrap();
        let link = links2
            .iter()
            .find(|h| h.url.as_deref() == Some("[[Secret Acquisition]]"))
            .expect("unlocked related note must be linked");
        assert_eq!(link.source, "Murmur");
        assert!(
            link.detail.contains("acquisition roadmap review"),
            "the task-free gist prose must be the link detail; got: {}",
            link.detail
        );
        assert!(
            !link.detail.contains("SEALED-SECRET"),
            "the link detail must be TASK-FREE (no action items); got: {}",
            link.detail
        );
    }

    /// An empty query (transcript was all stopwords) yields no links — never a panic, never a leak.
    #[test]
    fn related_note_links_empty_query_is_empty() {
        let db = temp_db();
        seed_note(&db, "this", "Standup", "daily standup notes", None);
        seed_note(
            &db,
            "prior",
            "Old Standup",
            "## Summary\n\nolder standup summary\n",
            None,
        );
        let nothing = HashSet::new();
        assert!(related_note_links(&db, "this", "", &nothing, 4)
            .unwrap()
            .is_empty());
    }

    /// An empty query (transcript was all stopwords) yields an empty corpus — never a panic, never
    /// a leak.
    #[test]
    fn build_related_context_empty_query_is_empty() {
        let db = temp_db();
        seed_note(&db, "this", "Standup", "daily standup notes", None);
        seed_note(&db, "prior", "Old Standup", "older standup notes", None);
        let nothing = HashSet::new();
        let (corpus, sources) =
            build_related_context(&db, "this", "", &nothing, "anthropic").unwrap();
        assert!(corpus.is_empty());
        assert!(sources.is_empty());
    }

    // ── PHASE 7: semantic recall for Lane A candidate retrieval ───────────────────────────────────
    //
    // The switch is `related_note_links` → `related_note_links_with_embedder(embedder)`:
    //   - `embedder = None` (real model absent) → EXACTLY today's FTS `search_visible` path, NO
    //     vector computed (a no-model install is byte-identical);
    //   - `embedder = Some(e)` (real model present) → gated HYBRID `search_hybrid_visible`.
    // We inject a KNOWN embedder + pre-index vectors so the hybrid branch is deterministic WITHOUT a
    // model on disk (mirrors `Db::related_meetings_visible`'s injected-embedder test idiom).

    /// A deterministic test embedder that maps by CONCEPT, not by literal keyword: any text about the
    /// "sleep comfort" concept (the marker tokens) → `one_hot(0)`; everything else → `one_hot(7)`.
    /// This lets a SEMANTICALLY related note (mattress ↔ sleeping surface) align with a query that
    /// shares NO literal FTS token — the exact case pure-keyword FTS misses. Deterministic, dim ==
    /// EMBED_DIM. It overrides `embed` (the raw method both `embed_query`/`embed_passage` route
    /// through), so the e5 `query: `/`passage: ` prefixes are handled by the substring match.
    struct ConceptEmbedder;
    impl crate::embed::Embedder for ConceptEmbedder {
        fn dim(&self) -> usize {
            crate::embed::EMBED_DIM
        }
        fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            const SLEEP_CONCEPT: &[&str] = &[
                "mattress",
                "sleeping",
                "sleep",
                "bed",
                "comfort",
                "comfortable",
                "ergonomic",
                "ergonomics",
                "surface",
            ];
            Ok(texts
                .iter()
                .map(|t| {
                    let lc = t.to_lowercase();
                    let dim = if SLEEP_CONCEPT.iter().any(|k| lc.contains(k)) {
                        0
                    } else {
                        7
                    };
                    let mut v = vec![0f32; crate::embed::EMBED_DIM];
                    v[dim] = 1.0;
                    v
                })
                .collect())
        }
    }

    /// Index a meeting's note into the vector layer with the injected embedder — the passage side of
    /// the same `ConceptEmbedder` that will embed the query, so vectors align by concept.
    fn index_with(db: &Db, meeting_id: &str, embedder: &dyn crate::embed::Embedder) {
        db.index_meeting_chunks(meeting_id, &[], embedder).unwrap();
    }

    /// PHASE 7 core (RED-before-GREEN): a SEMANTICALLY related prior note whose words DON'T overlap
    /// the query is found by the HYBRID (model-present) branch but MISSED by the FTS-only branch.
    ///   - RED assertion: `embedder = None` (today's FTS path) does NOT link the mattress note for the
    ///     keyword-disjoint query "sleeping surface ergonomics" — proving FTS alone misses it.
    ///   - GREEN assertion: `embedder = Some(ConceptEmbedder)` (model present) DOES link it via the
    ///     semantic KNN leg of `search_hybrid_visible`.
    #[test]
    fn related_note_links_semantic_finds_what_fts_misses() {
        let db = temp_db();
        // Current meeting we link FROM (its own words don't matter — it is self-excluded).
        seed_note(&db, "this", "Product Sync", "roadmap sync agenda", None);
        // A genuinely related PRIOR note — about the SAME concept, but with NO word the query uses.
        seed_note(
            &db,
            "prior",
            "Mattress Review",
            "## Summary\n\nThe mattress felt very comfortable and supportive overnight.\n",
            None,
        );
        // Index the prior note's vectors with the concept embedder (passage side).
        index_with(&db, "prior", &ConceptEmbedder);

        // The query shares the CONCEPT (sleep/comfort) but ZERO literal token with the prior note.
        let query = "sleeping surface ergonomics";
        let nothing = HashSet::new();

        // RED: FTS-only (real model absent) must MISS the semantically-related note (disjoint words).
        let fts_only =
            related_note_links_with_embedder(&db, "this", query, &nothing, 4, None).unwrap();
        assert!(
            fts_only
                .iter()
                .all(|h| h.url.as_deref() != Some("[[Mattress Review]]")),
            "FTS-only must MISS the keyword-disjoint semantic match (this is the recall gap Phase 7 closes); got: {fts_only:?}"
        );

        // GREEN: HYBRID (model present) FINDS it via the semantic KNN leg.
        let embedder = ConceptEmbedder;
        let hybrid = related_note_links_with_embedder(
            &db,
            "this",
            query,
            &nothing,
            4,
            Some(&embedder as &dyn crate::embed::Embedder),
        )
        .unwrap();
        let link = hybrid
            .iter()
            .find(|h| h.url.as_deref() == Some("[[Mattress Review]]"))
            .expect("hybrid (semantic) branch must find the concept-related note FTS missed");
        assert_eq!(link.source, "Murmur");
        assert!(
            link.detail.contains("comfortable and supportive"),
            "the task-free gist prose must be the link detail; got: {}",
            link.detail
        );
    }

    /// PHASE-7 PRECISION FINDING: an empty/whitespace query yields NO links EVEN WITH THE MODEL
    /// PRESENT. Without the `gated_candidates` short-circuit, the empty query would still embed to a
    /// non-empty vector and the floor-less semantic KNN would return an ARBITRARY indexed note — a
    /// spurious cross-meeting link for a low-content (all-stopwords → `salient_query==""`) meeting.
    /// RED before the short-circuit (a `[[Mattress Review]]` link appears for `""`/`"   "`); GREEN now.
    #[test]
    fn related_note_links_empty_query_is_empty_even_with_model_present() {
        let db = temp_db();
        seed_note(&db, "this", "Product Sync", "roadmap sync agenda", None);
        seed_note(
            &db,
            "prior",
            "Mattress Review",
            "## Summary\n\nThe mattress felt very comfortable and supportive overnight.\n",
            None,
        );
        index_with(&db, "prior", &ConceptEmbedder);
        let nothing = HashSet::new();
        let embedder = ConceptEmbedder;
        for degenerate in ["", "   ", "\n\t "] {
            let links = related_note_links_with_embedder(
                &db,
                "this",
                degenerate,
                &nothing,
                4,
                Some(&embedder as &dyn crate::embed::Embedder),
            )
            .unwrap();
            assert!(
                links.is_empty(),
                "a degenerate query {degenerate:?} must yield NO links even with the model present (no floor-less KNN spurious match); got: {links:?}"
            );
        }
    }

    /// NEVER A STUB VECTOR: with the real model ABSENT (`embedder = None`) the candidate retrieval is
    /// BYTE-IDENTICAL to the historical FTS `search_visible` path — same links, same order — as the
    /// public `related_note_links` (which resolves `embed_model_present()` = false on a no-model test
    /// machine). A keyword match IS still found; a semantic-only match is NOT (no vector is computed).
    #[test]
    fn related_note_links_model_absent_is_byte_identical_fts() {
        let db = temp_db();
        seed_note(&db, "this", "Budget Talk", "budget planning agenda", None);
        // A KEYWORD-overlapping prior (shares "budget"/"planning") — FTS finds this.
        seed_note(
            &db,
            "kw",
            "Q2 Budget",
            "## Summary\n\nWe reviewed the budget planning for the next quarter.\n",
            None,
        );
        // A SEMANTIC-only prior (concept match, no shared word) with vectors indexed — FTS must NOT
        // find it, and the model-absent path must NOT reach the vectors (no stub-vector KNN).
        seed_note(
            &db,
            "sem",
            "Mattress Review",
            "## Summary\n\nThe mattress felt very comfortable overnight.\n",
            None,
        );
        index_with(&db, "sem", &ConceptEmbedder);

        let query = "budget planning";
        let nothing = HashSet::new();

        // The injected-None core and the public entry point agree (public resolves model-absent → None
        // on a no-model test machine). Assert byte-identical link sets.
        let via_core =
            related_note_links_with_embedder(&db, "this", query, &nothing, 4, None).unwrap();
        // Keyword prior is found; semantic-only prior is NOT (no vector touched on the None path).
        assert!(
            via_core
                .iter()
                .any(|h| h.url.as_deref() == Some("[[Q2 Budget]]")),
            "FTS must still find the keyword-overlapping prior; got: {via_core:?}"
        );
        assert!(
            via_core
                .iter()
                .all(|h| h.url.as_deref() != Some("[[Mattress Review]]")),
            "model-absent path must NOT reach the semantic vectors (no stub vector); got: {via_core:?}"
        );

        // On a no-model machine the public entry resolves to the SAME (None) path → identical output.
        if !crate::embed::embed_model_present() {
            let via_public = related_note_links(&db, "this", query, &nothing, 4).unwrap();
            let core_urls: Vec<_> = via_core.iter().map(|h| h.url.clone()).collect();
            let public_urls: Vec<_> = via_public.iter().map(|h| h.url.clone()).collect();
            assert_eq!(
                core_urls, public_urls,
                "public related_note_links must be byte-identical to the FTS path when no model is present"
            );
        }
    }

    /// LOCK INVARIANT under the HYBRID branch: a sealed-and-NOT-session-unlocked related note yields
    /// NO link even when the semantic model is present + its vectors are indexed. Mirrors
    /// `related_note_links_gated_self_excluded_and_task_free` for the model-present path — the
    /// double gate (semantic leg's `visibility_clause` FIRST + `get_note_if_visible` SECOND) must
    /// exclude it, and it must reappear once its folder is session-unlocked.
    #[test]
    fn related_note_links_hybrid_respects_lock_gate() {
        let db = temp_db();
        seed_note(&db, "this", "Product Sync", "roadmap sync agenda", None);
        seed_folder(&db, "f-locked", false);
        seed_note(
            &db,
            "sealed",
            "Mattress Review",
            "## Summary\n\nThe mattress felt very comfortable and supportive. SEALED-SECRET price.\n",
            Some("f-locked"),
        );
        // Index the sealed note's vectors WHILE it is still open (a chunk exists in vec_chunks); then
        // seal it. The gate — not purge — must exclude it under the hybrid semantic leg.
        index_with(&db, "sealed", &ConceptEmbedder);
        db.set_folder_locked("f-locked", true, None).unwrap();

        let query = "sleeping surface ergonomics";
        let embedder = ConceptEmbedder;

        // Not session-unlocked → the sealed semantic match yields NO link.
        let nothing = HashSet::new();
        let hidden = related_note_links_with_embedder(
            &db,
            "this",
            query,
            &nothing,
            4,
            Some(&embedder as &dyn crate::embed::Embedder),
        )
        .unwrap();
        assert!(
            hidden
                .iter()
                .all(|h| h.url.as_deref() != Some("[[Mattress Review]]")),
            "sealed-not-unlocked note must not be linked via the hybrid semantic leg (gate violation)"
        );
        assert!(
            hidden.iter().all(|h| !h.detail.contains("SEALED-SECRET")),
            "sealed content must not leak into a hybrid link detail"
        );

        // Session-unlock → reappears (note plaintext is restored on real unlock; here it was never
        // blanked at the column level in this fixture, so the second gate admits it once unlocked).
        let mut unlocked = HashSet::new();
        unlocked.insert("f-locked".to_string());
        let shown = related_note_links_with_embedder(
            &db,
            "this",
            query,
            &unlocked,
            4,
            Some(&embedder as &dyn crate::embed::Embedder),
        )
        .unwrap();
        assert!(
            shown
                .iter()
                .any(|h| h.url.as_deref() == Some("[[Mattress Review]]")),
            "unlocked semantic match must be linkable"
        );
    }

    /// The HYBRID branch keeps SELF-EXCLUSION: even if the current meeting's own note is the nearest
    /// semantic neighbour, it is never linked to itself.
    #[test]
    fn related_note_links_hybrid_self_excluded() {
        let db = temp_db();
        seed_note(
            &db,
            "this",
            "Mattress Review",
            "## Summary\n\nThe mattress felt very comfortable and supportive.\n",
            None,
        );
        index_with(&db, "this", &ConceptEmbedder);
        let embedder = ConceptEmbedder;
        let nothing = HashSet::new();
        let links = related_note_links_with_embedder(
            &db,
            "this",
            "sleeping surface ergonomics",
            &nothing,
            4,
            Some(&embedder as &dyn crate::embed::Embedder),
        )
        .unwrap();
        assert!(
            links
                .iter()
                .all(|h| h.url.as_deref() != Some("[[Mattress Review]]")),
            "a note must never be linked to itself under the hybrid branch; got: {links:?}"
        );
    }
}
