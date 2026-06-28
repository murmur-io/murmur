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

/// Char budget for the grounding corpus, by provider. Smaller than the Ask-My-Vault budget on
/// purpose — this rides ON TOP of the full transcript in the same prompt, so it must stay lean.
/// Local quantized models (Ollama) have tiny context windows → cap much tighter.
pub(crate) fn budget_for(provider_id: &str) -> usize {
    if provider_id == "ollama" {
        3_000
    } else {
        24_000
    }
}

/// Basic EN + PL stopword set. Deliberately small + deterministic (no external word-list dep); it
/// strips the highest-frequency function words so the salient query keys off content terms.
fn is_stopword(t: &str) -> bool {
    const STOP: &[&str] = &[
        // English
        "the", "and", "for", "are", "but", "not", "you", "your", "all", "can", "had", "her", "was",
        "one", "our", "out", "has", "him", "his", "how", "man", "new", "now", "old", "see", "two",
        "way", "who", "did", "get", "may", "she", "use", "that", "this", "with", "have", "from",
        "they", "will", "would", "there", "their", "what", "about", "which", "when", "were", "been",
        "them", "then", "than", "some", "into", "just", "like", "also", "well", "yeah", "okay",
        "going", "gonna", "really", "actually", "think", "know", "want", "need", "let", "yes",
        "kind", "sort", "thing", "things", "stuff", "much", "very", "more", "most", "such", "only",
        // Polish
        "nie", "tak", "jest", "się", "sie", "tego", "tym", "tych", "tam", "jak", "czy", "ale",
        "lub", "albo", "oraz", "dla", "tylko", "też", "tez", "już", "juz", "bez", "być", "byc",
        "jako", "który", "ktory", "która", "ktora", "które", "ktore", "tutaj", "teraz", "wtedy",
        "bardzo", "trochę", "troche", "może", "moze", "żeby", "zeby", "przez", "przy", "pod", "nad",
        "tu", "to", "co", "na", "we", "do", "od", "po", "za", "ze", "oraz", "więc", "wiec",
    ];
    STOP.contains(&t)
}

/// Tokenize to lowercased alphanumeric tokens (Unicode-aware; handles PL diacritics).
fn tokenize(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .collect()
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
/// `search_visible` returns nothing for a query with no usable terms.
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
    let hits = db.search_visible(query, (MAX_RELATED_NOTES as i64) + 6, unlocked)?;

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
            "meetnotes-relctx-test-{}-{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
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
        assert_eq!(q, salient_query(None, "Apollo migration migration migration deadline"));
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
        seed_note(&db, "this", "Q3 Planning", "ACME quarterly planning roadmap", None);
        // A genuinely related prior, open folder → visible.
        seed_note(&db, "prior", "Q2 Planning", "ACME quarterly planning roadmap decisions", None);

        let nothing = HashSet::new();
        let (corpus, sources) =
            build_related_context(&db, "this", "ACME quarterly planning", &nothing, "anthropic")
                .unwrap();

        assert!(corpus.contains("[[Q2 Planning]]"), "related note must be cited");
        assert!(corpus.contains("id:prior"));
        assert!(sources.iter().any(|s| s.meeting_id == "prior"));
        // Self-exclusion: the meeting being summarized is never in its own grounding corpus.
        assert!(!corpus.contains("id:this"), "a note must never be grounded in itself");
        assert!(sources.iter().all(|s| s.meeting_id != "this"));
    }

    /// LOCK INVARIANT (RED-if-ungated): a sealed-and-NOT-session-unlocked related meeting must
    /// contribute NOTHING to the corpus (it would otherwise egress to the cloud provider), and must
    /// reappear once its folder is session-unlocked. This is the gate that an ungated `search` +
    /// `get_latest_note_for_meeting` would fail.
    #[test]
    fn build_related_context_excludes_sealed_until_unlocked() {
        let db = temp_db();
        seed_note(&db, "this", "Acquisition Talk", "PROJECT atlas acquisition terms", None);
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
        let (corpus, sources) =
            build_related_context(&db, "this", "PROJECT atlas acquisition", &nothing, "anthropic")
                .unwrap();
        assert!(
            !corpus.contains("SEALED-SECRET"),
            "sealed-not-unlocked related content leaked into the cloud grounding corpus (gate violation)"
        );
        assert!(sources.iter().all(|s| s.meeting_id != "sealed"));

        // Session-unlock the folder → the related note is now legitimately available + cited.
        let mut unlocked = HashSet::new();
        unlocked.insert("f-locked".to_string());
        let (corpus2, sources2) =
            build_related_context(&db, "this", "PROJECT atlas acquisition", &unlocked, "anthropic")
                .unwrap();
        assert!(corpus2.contains("SEALED-SECRET"), "unlocked related content must reappear");
        assert!(sources2.iter().any(|s| s.meeting_id == "sealed"));
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
}
