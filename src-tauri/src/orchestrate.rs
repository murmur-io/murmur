//! Phase B step 3 (Flow A) — LOCAL-BRAIN context orchestration for the NOTE PIPELINE.
//!
//! This replaces only the *deciding* step of today's deterministic salient-query retrieval: instead
//! of mechanically deriving one FTS query from the title+transcript, the on-device reasoner
//! ([`crate::reason::LocalReasoner`]) ANALYZES a transcript excerpt and emits a small RETRIEVAL PLAN
//! (which tools to call with which queries). Each planned query is mapped to a [`ToolCall`] and run
//! through the ONE gated registry [`crate::tools::execute_tool`], so the brain can never reach an
//! ungated read. The assembled, cited corpus feeds `SummarizeRequest.related_context` exactly as the
//! deterministic path's corpus did.
//!
//! ## The floor (zero-behavior-change default)
//! When the active reasoner is the dependency-free [`crate::reason::StubReasoner`] (`id() == "stub"`
//! — the default build, no model) this falls THROUGH to the EXISTING deterministic
//! [`crate::pipeline::build_grounding_context`] and returns its result byte-identical. The
//! deterministic salient-query path is ALSO the fallback floor for the brain path: any reasoner
//! error, plan-parse failure, or empty brain corpus degrades to it. This NEVER fails the pipeline
//! (returns `Option<String>`, never an `Err`).
//!
//! ## Lock invariant (load-bearing)
//! The corpus is injected into the summarization prompt and therefore EGRESSES to the provider, so
//! every retrieval is GATED. The brain path routes EXCLUSIVELY through `execute_tool`, whose every
//! branch is visibility-gated on the live `unlocked` session set (`search_visible` /
//! `search_hybrid_visible` / `meeting_is_visible` / `get_note_if_visible` / `list_open_commitments`
//! / `build_dossier_data`). The meeting being summarized is SELF-EXCLUDED. A sealed-and-not-session-
//! unlocked meeting contributes NOTHING — swapping `execute_tool` for any ungated read is a leak.
//!
//! ## No new egress class
//! Egress is the SAME provider call the summary already makes (`make_provider` → RedactingProvider +
//! fail-closed consent). The brain itself is on-device and makes no network call. No PII is logged
//! (target `rag`, ids/counts only).

use std::collections::HashSet;

use serde_json::Value;

use crate::reason::LocalReasoner;
use crate::settings::AppConfig;
use crate::storage::models::CorrectionRecord;
use crate::storage::Db;
use crate::tools::{execute_tool, ToolCall};

/// Chars of transcript handed to the reasoner for pre-analysis. Small + deterministic — a planning
/// signal, not the full document (the full transcript already rides in the summarize prompt).
const EXCERPT_CHARS: usize = 2_000;

/// System prompt for the brain's PRE-ANALYSIS step. It asks for ONLY a JSON object shaped like
/// [`pre_analysis_schema`]: the salient entities/topics plus a short list of retrieval queries, each
/// naming one of the four gated tools. The reasoner is told to plan retrieval, never to invent data.
const PRE_ANALYSIS_PROMPT: &str = "You are a retrieval planner for a local, private meeting-notes \
assistant. Given an excerpt of a NEW meeting transcript, identify the salient entities and topics, \
then propose the retrieval queries that would surface the most useful PRIOR context from the user's \
OWN meeting history to ground the new note. Respond with ONLY a single JSON object of the form: \
{\"entities\":[string],\"topics\":[string],\"retrieval_queries\":[{\"tool\":one of \
[\"search_meetings\",\"semantic_search\",\"get_dossier\",\"list_commitments\"],\"query\":string}]}. \
Propose at most 4 retrieval queries. Do not invent meetings or facts; only propose queries.";

/// JSON schema the PRE_ANALYSIS output must conform to. A grammar-constrained reasoner decodes to
/// this shape; the stub never reaches here (it takes the deterministic floor).
fn pre_analysis_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "entities": { "type": "array", "items": { "type": "string" } },
            "topics": { "type": "array", "items": { "type": "string" } },
            "retrieval_queries": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "tool": {
                            "type": "string",
                            "enum": ["search_meetings", "semantic_search", "get_dossier", "list_commitments"]
                        },
                        "query": { "type": "string" }
                    },
                    "required": ["tool", "query"]
                }
            }
        },
        "required": ["entities", "topics", "retrieval_queries"]
    })
}

/// One planned retrieval step from the brain's PRE_ANALYSIS output.
#[derive(Debug, Clone, serde::Deserialize)]
struct RetrievalQuery {
    tool: String,
    #[serde(default)]
    query: String,
}

/// The parsed PRE_ANALYSIS object — only `retrieval_queries` drives behavior here (entities/topics
/// are captured in the flywheel `model_output` for later use, not consumed yet).
#[derive(Debug, Clone, serde::Deserialize)]
struct PreAnalysis {
    #[serde(default)]
    retrieval_queries: Vec<RetrievalQuery>,
}

/// Flow A — orchestrate the related-context corpus for a new note.
///
/// Returns the assembled corpus for `SummarizeRequest.related_context`, or `None` when there is no
/// useful related context (a fresh vault, all-sealed candidates, an empty plan). NEVER fails the
/// pipeline.
///
/// - **Stub-shim (the floor):** `reasoner.id() == "stub"` → the EXACT deterministic path, returned
///   byte-identical to today (default build / no model ⇒ zero behavior change).
/// - **Brain path:** `reasoner.structured(...)` → parse the retrieval plan → map each query to a
///   gated [`ToolCall`] → [`execute_tool`] → assemble a cited corpus (self-excluding `meeting_id`),
///   capturing the plan into the correction-log flywheel. Any error/empty result → deterministic
///   floor.
pub fn orchestrate_context(
    reasoner: &dyn LocalReasoner,
    db: &Db,
    meeting_id: &str,
    title: Option<&str>,
    transcript: &str,
    unlocked: &HashSet<String>,
    config: &AppConfig,
) -> Option<String> {
    // STUB-SHIM (the floor): no real brain → fall through to today's deterministic salient-query
    // path, byte-identical. This is the default build / no-model branch ⇒ zero behavior change.
    if reasoner.id() == "stub" {
        return deterministic(db, unlocked, meeting_id, title, transcript, config);
    }

    // BRAIN path: the reasoner DECIDES what to retrieve from a transcript excerpt.
    let excerpt: String = transcript.chars().take(EXCERPT_CHARS).collect();
    let schema = pre_analysis_schema();
    let plan = match reasoner.structured(PRE_ANALYSIS_PROMPT, &excerpt, &schema) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(target: "rag", error = %e, "local-brain pre-analysis failed; deterministic context");
            return deterministic(db, unlocked, meeting_id, title, transcript, config);
        }
    };

    // FLYWHEEL capture (first prod writer; F0 made `log_correction` lock-safe via `meeting_id`). The
    // brain ran → record the plan it produced for later on-device fine-tuning. Best-effort: a log
    // error is swallowed (logged, no PII) and must NEVER fail the pipeline.
    let plan_json = plan.to_string();
    capture_context_plan(db, meeting_id, &excerpt, &plan_json);

    // Parse the retrieval plan; a malformed shape → no queries → deterministic floor.
    let queries: Vec<RetrievalQuery> = match serde_json::from_value::<PreAnalysis>(plan) {
        Ok(p) => p.retrieval_queries,
        Err(e) => {
            tracing::warn!(target: "rag", error = %e, "brain retrieval plan unparseable; deterministic context");
            return deterministic(db, unlocked, meeting_id, title, transcript, config);
        }
    };

    let corpus = assemble_brain_corpus(db, meeting_id, &queries, unlocked, config);
    if corpus.trim().is_empty() {
        // The brain ran but surfaced nothing visible (all sealed / no hits) → use the floor.
        return deterministic(db, unlocked, meeting_id, title, transcript, config);
    }
    tracing::info!(target: "rag", queries = queries.len(), "grounding note via local-brain retrieval plan");
    Some(corpus)
}

/// The deterministic salient-query path — today's `build_grounding_context`, shared (not duplicated)
/// so the stub-shim is provably byte-identical and the brain path's fallback floor is the same code.
fn deterministic(
    db: &Db,
    unlocked: &HashSet<String>,
    meeting_id: &str,
    title: Option<&str>,
    transcript: &str,
    config: &AppConfig,
) -> Option<String> {
    crate::pipeline::build_grounding_context(
        db,
        unlocked,
        meeting_id,
        title,
        transcript,
        // The corpus egresses to the NOTES-role provider — budget on its RESOLVED connection
        // (identical to `provider_id` while role keys are absent).
        &crate::summarize::roles::provider_target(crate::summarize::roles::Role::Notes, config)
            .connection,
    )
}

/// Append the brain's context plan to the local correction-log flywheel. Best-effort: a failure is
/// logged (no PII) and swallowed — the note pipeline must never fail over a flywheel write.
fn capture_context_plan(db: &Db, meeting_id: &str, excerpt: &str, plan_json: &str) {
    let rec = CorrectionRecord {
        id: 0, // ignored — SQLite assigns the autoincrement key.
        kind: "context_plan".to_string(),
        input: excerpt.to_string(),
        model_output: plan_json.to_string(),
        final_output: None,
        accepted: true,
        owner_id: "local".to_string(),
        created_at: chrono::Utc::now().to_rfc3339(),
        meeting_id: Some(meeting_id.to_string()),
    };
    if let Err(e) = db.log_correction(&rec) {
        tracing::warn!(target: "rag", error = %e, "context-plan flywheel capture failed (note unaffected)");
    }
}

/// Map a planned retrieval query to a gated [`ToolCall`]. An unknown tool, or an empty query for a
/// tool that needs one, yields `None` (skipped). `list_commitments` rolls up ALL open commitments
/// (the query is a topic, not an owner), so its owner filter stays `None`.
fn map_to_tool_call(q: &RetrievalQuery) -> Option<ToolCall> {
    let query = q.query.trim().to_string();
    match q.tool.trim() {
        "search_meetings" if !query.is_empty() => Some(ToolCall::SearchMeetings { query }),
        "semantic_search" if !query.is_empty() => Some(ToolCall::SearchSemantic { query }),
        "get_dossier" if !query.is_empty() => Some(ToolCall::GetEntityDossier { entity: query }),
        "list_commitments" => Some(ToolCall::GetOpenCommitments { owner: None }),
        _ => None,
    }
}

/// A short, stable provenance label for a tool's corpus section.
fn tool_label(call: &ToolCall) -> &'static str {
    match call {
        ToolCall::SearchMeetings { .. } => "Related meetings",
        ToolCall::SearchSemantic { .. } => "Semantically related",
        ToolCall::GetEntityDossier { .. } => "Entity dossier",
        ToolCall::ListEntities { .. } => "Entities",
        ToolCall::ListNoteFolders => "Note folders",
        // Brain v3 PR-6 — knowledge diff / decision ledger (explicit tool; label for completeness).
        ToolCall::KnowledgeDiff { .. } => "Knowledge diff",
        ToolCall::GetOpenCommitments { .. } => "Open commitments",
        ToolCall::GetMeeting { .. } => "Meeting",
        ToolCall::GetDocument { .. } => "Document",
        // Brain v3 audit Fix 3(b) — the document outline (structural map; explicit tool).
        ToolCall::GetDocumentOutline { .. } => "Document outline",
        ToolCall::ListRecentMeetings { .. } => "Recent meetings",
        ToolCall::WebSearch { .. } => "Web search",
        ToolCall::CalendarLookup { .. } => "Calendar",
        ToolCall::JiraSearch { .. } => "Jira",
        ToolCall::SlackSearch { .. } => "Slack",
        // Shared Brain — org search is an EXPLICIT interactive tool, never auto-planned into note
        // generation (`map_to_tool_call` doesn't map it), so this label is only for completeness.
        ToolCall::OrgBrainSearch { .. } => "Org brain",
        // Feature C — typed note-folder database query (explicit tool; label for completeness).
        ToolCall::QueryDatabase { .. } => "Note database",
    }
}

/// Is `payload` one of `execute_tool`'s "nothing found / disabled" sentinels? Such results carry no
/// content and must not pollute the corpus.
fn is_empty_result(payload: &str) -> bool {
    let p = payload.trim_start();
    p.starts_with("No ") || p.starts_with("Semantic search is disabled")
}

/// Assemble the cited corpus from the brain's retrieval plan. Every read goes through the GATED
/// [`execute_tool`]; the meeting being summarized is self-excluded by dropping any payload line that
/// cites its id. Sections are packed under the SAME per-provider budget as
/// [`crate::summarize::related_context::budget_for`]. A tool error is logged + skipped (never fatal).
fn assemble_brain_corpus(
    db: &Db,
    meeting_id: &str,
    queries: &[RetrievalQuery],
    unlocked: &HashSet<String>,
    config: &AppConfig,
) -> String {
    // Budget on the NOTES-role provider's RESOLVED connection — the corpus rides in ITS prompt
    // (identical to `provider_id` while role keys are absent).
    let budget = crate::summarize::related_context::budget_for(
        &crate::summarize::roles::provider_target(crate::summarize::roles::Role::Notes, config)
            .connection,
    );
    // The self id appears in tool payloads as `[id:<id>]` (hits) or `id:<id>` (lists); matching the
    // `id:<id>` token covers both, so a note is never grounded in its own prior self.
    let self_tag = format!("id:{meeting_id}");
    let mut corpus = String::new();

    for q in queries {
        if corpus.len() >= budget {
            break;
        }
        let Some(call) = map_to_tool_call(q) else {
            continue;
        };
        let payload = match execute_tool(&call, db, unlocked, config) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(target: "rag", tool = %q.tool, error = %e, "brain retrieval tool failed; skipping");
                continue;
            }
        };
        if is_empty_result(&payload) {
            continue;
        }
        // SELF-EXCLUDE: drop any line citing the meeting being summarized.
        let filtered: String = payload
            .lines()
            .filter(|line| !line.contains(&self_tag))
            .collect::<Vec<_>>()
            .join("\n");
        if filtered.trim().is_empty() {
            continue;
        }
        let header = format!("\n\n### {} · query:{}\n", tool_label(&call), q.query.trim());
        let remaining = budget.saturating_sub(corpus.len() + header.len());
        if remaining < 80 {
            break;
        }
        let chunk: String = filtered.chars().take(remaining).collect();
        corpus.push_str(&header);
        corpus.push_str(&chunk);
    }

    corpus.trim_start().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{AppError, Result};
    use crate::reason::StubReasoner;
    use crate::storage::models::{Folder, Meeting, MeetingStatus, NoteRecord};
    use crate::storage::Db;

    const TEST_DEK: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn temp_db(label: &str) -> Db {
        let p =
            crate::storage::db::unique_temp_path(&format!("murmur-orchestrate-{label}"), "sqlite");
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

    fn cfg() -> AppConfig {
        AppConfig {
            provider_id: "anthropic".to_string(),
            ..Default::default()
        }
    }

    /// A mock reasoner returning a CANNED retrieval plan (so the brain branch is exercised without a
    /// model). `id()` is NOT "stub", so `orchestrate_context` takes the brain path.
    struct MockReasoner {
        plan: Value,
    }
    impl LocalReasoner for MockReasoner {
        fn id(&self) -> &str {
            "mock"
        }
        fn reason(&self, _system: &str, _user: &str) -> Result<String> {
            Ok(String::new())
        }
        fn structured(&self, _system: &str, _user: &str, _schema: &Value) -> Result<Value> {
            Ok(self.plan.clone())
        }
    }

    /// A reasoner whose structured() always errors — exercises the best-effort fallback.
    struct ErrReasoner;
    impl LocalReasoner for ErrReasoner {
        fn id(&self) -> &str {
            "mock-err"
        }
        fn reason(&self, _system: &str, _user: &str) -> Result<String> {
            Ok(String::new())
        }
        fn structured(&self, _system: &str, _user: &str, _schema: &Value) -> Result<Value> {
            Err(AppError::Summarize("brain blew up".into()))
        }
    }

    fn plan_search(term: &str) -> Value {
        serde_json::json!({
            "entities": [],
            "topics": [],
            "retrieval_queries": [{ "tool": "search_meetings", "query": term }]
        })
    }

    /// STUB EQUIVALENCE: with the StubReasoner, `orchestrate_context` returns BYTE-IDENTICAL output
    /// to the deterministic `build_grounding_context` for the same inputs — the floor is exactly
    /// today's behavior (and the stub path captures NO flywheel correction).
    #[test]
    fn stub_orchestrate_is_byte_identical_to_deterministic() {
        let db = temp_db("stub-eq");
        seed_note(
            &db,
            "m-self",
            "Budget Planning",
            "Budget planning and hiring runway.",
            None,
        );
        seed_note(
            &db,
            "m-prior",
            "Q2 Budget",
            "Budget planning and hiring runway decisions.",
            None,
        );
        let nothing = HashSet::new();
        let config = cfg();

        let det = crate::pipeline::build_grounding_context(
            &db,
            &nothing,
            "m-self",
            Some("Budget Planning"),
            "Budget planning and hiring runway.",
            &config.provider_id,
        );
        let orch = orchestrate_context(
            &StubReasoner,
            &db,
            "m-self",
            Some("Budget Planning"),
            "Budget planning and hiring runway.",
            &nothing,
            &config,
        );

        assert_eq!(
            orch, det,
            "stub-shim must be byte-identical to the deterministic path"
        );
        assert!(
            orch.is_some(),
            "the related prior note should ground the new note"
        );
        // The stub path must NOT write a flywheel correction (only the brain path does).
        assert_eq!(
            db.list_corrections("context_plan", 100, &nothing)
                .unwrap()
                .len(),
            0,
            "stub path captures no context_plan correction"
        );
    }

    /// BRAIN BRANCH (via mock): a canned plan drives the GATED tool, assembles a cited corpus that
    /// self-excludes the meeting being summarized, and captures exactly ONE context_plan correction.
    #[test]
    fn brain_branch_runs_gated_tool_assembles_corpus_and_logs_one_plan() {
        let db = temp_db("brain");
        // Both notes match the search term; the self meeting must be filtered out of the corpus.
        seed_note(
            &db,
            "m-self",
            "Budget Planning",
            "ACME budget planning and hiring runway.",
            None,
        );
        seed_note(
            &db,
            "m-prior",
            "Q2 Budget",
            "ACME budget planning and hiring runway decisions.",
            None,
        );
        let nothing = HashSet::new();
        let config = cfg();

        let reasoner = MockReasoner {
            plan: plan_search("budget"),
        };
        let corpus = orchestrate_context(
            &reasoner,
            &db,
            "m-self",
            Some("Budget Planning"),
            "ACME budget planning and hiring runway.",
            &nothing,
            &config,
        )
        .expect("brain path should surface the related prior meeting");

        assert!(
            corpus.contains("id:m-prior"),
            "related prior meeting must be cited: {corpus}"
        );
        assert!(
            !corpus.contains("id:m-self"),
            "the meeting being summarized must be self-excluded: {corpus}"
        );

        // Exactly ONE flywheel correction, attributed to this meeting, accepted, owner local.
        let corrs = db.list_corrections("context_plan", 100, &nothing).unwrap();
        assert_eq!(
            corrs.len(),
            1,
            "the brain path must capture exactly one context_plan correction"
        );
        let c = &corrs[0];
        assert_eq!(c.kind, "context_plan");
        assert_eq!(c.meeting_id.as_deref(), Some("m-self"));
        assert!(c.accepted);
        assert_eq!(c.owner_id, "local");
        assert!(
            c.model_output.contains("retrieval_queries"),
            "plan JSON captured"
        );
    }

    /// GATE: with the brain plan targeting a SEALED-not-unlocked related meeting, its content must
    /// NOT appear in the corpus — `execute_tool`'s visibility gate binds. (RED if `execute_tool` is
    /// swapped for an ungated read: the sealed snippet would leak into the cloud-bound corpus.) Once
    /// the folder is session-unlocked the meeting legitimately reappears.
    #[test]
    fn brain_branch_gate_excludes_sealed_until_unlocked() {
        let db = temp_db("brain-gate");
        seed_note(
            &db,
            "m-self",
            "Acquisition",
            "PROJECT atlas acquisition terms.",
            None,
        );
        db.insert_folder(&Folder {
            id: "f-lock".to_string(),
            name: "Secret".to_string(),
            path: "Secret".to_string(),
            parent_id: None,
            locked: false,
            created_at: "2026-06-19T00:00:00Z".to_string(),
        })
        .unwrap();
        seed_note(
            &db,
            "m-sealed",
            "Secret Acquisition",
            "PROJECT atlas acquisition price SEALED-SECRET-XYZ.",
            Some("f-lock"),
        );
        db.set_folder_locked("f-lock", true, None).unwrap();
        let config = cfg();

        let reasoner = MockReasoner {
            plan: plan_search("atlas acquisition"),
        };

        // Sealed + not unlocked → the sealed meeting must not surface in the corpus.
        let nothing = HashSet::new();
        let sealed = orchestrate_context(
            &reasoner,
            &db,
            "m-self",
            Some("Acquisition"),
            "PROJECT atlas acquisition terms.",
            &nothing,
            &config,
        );
        assert!(
            sealed
                .as_deref()
                .map(|c| !c.contains("SEALED-SECRET-XYZ") && !c.contains("id:m-sealed"))
                .unwrap_or(true),
            "sealed-not-unlocked content leaked into the brain corpus (gate violation): {sealed:?}"
        );

        // Session-unlock the folder → the sealed meeting is now visible + cited.
        let mut unlocked = HashSet::new();
        unlocked.insert("f-lock".to_string());
        let opened = orchestrate_context(
            &reasoner,
            &db,
            "m-self",
            Some("Acquisition"),
            "PROJECT atlas acquisition terms.",
            &unlocked,
            &config,
        );
        assert!(
            opened
                .as_deref()
                .map(|c| c.contains("id:m-sealed"))
                .unwrap_or(false),
            "an unlocked folder's meeting must reappear in the brain corpus: {opened:?}"
        );
    }

    /// BEST-EFFORT: a reasoner whose structured() errors falls back to the deterministic floor
    /// (byte-identical to `build_grounding_context`) and NEVER fails — and the brain never ran, so no
    /// flywheel correction is captured.
    #[test]
    fn brain_error_falls_back_to_deterministic() {
        let db = temp_db("brain-err");
        seed_note(
            &db,
            "m-self",
            "Budget Planning",
            "Budget planning and hiring runway.",
            None,
        );
        seed_note(
            &db,
            "m-prior",
            "Q2 Budget",
            "Budget planning and hiring runway decisions.",
            None,
        );
        let nothing = HashSet::new();
        let config = cfg();

        let det = crate::pipeline::build_grounding_context(
            &db,
            &nothing,
            "m-self",
            Some("Budget Planning"),
            "Budget planning and hiring runway.",
            &config.provider_id,
        );
        let orch = orchestrate_context(
            &ErrReasoner,
            &db,
            "m-self",
            Some("Budget Planning"),
            "Budget planning and hiring runway.",
            &nothing,
            &config,
        );

        assert_eq!(
            orch, det,
            "a reasoner error must fall back byte-identically to the deterministic floor"
        );
        // The brain did not produce a plan → no flywheel correction.
        assert_eq!(
            db.list_corrections("context_plan", 100, &nothing)
                .unwrap()
                .len(),
            0,
            "a failed brain must not capture a context_plan correction"
        );
    }
}
