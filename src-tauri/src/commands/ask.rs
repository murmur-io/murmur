//! Agentic Ask / in-meeting-chat helper commands — extracted verbatim from `commands` (God-file
//! split, a PURE MOVE — the gate LOGIC is UNCHANGED, only relocated). This is the "ask my meeting /
//! ask my vault" read surface: the typed-question spawn (`ask_assistant_text`), the persisted @brain
//! thread list (`list_assistant_threads`), the chat-history budgeting (`format_chat` /
//! `trim_history_to_budget` / `capped_ask_history` + the `CHAT_*`/`ASK_MAX_STEPS` caps), the
//! vault-scoped agentic attempt + its testable loop core (`ask_vault_agentic_attempt` /
//! `ask_vault_loop` / `agent_outcome_to_ask_result` / `build_ask_vault_floor_prompt`), the
//! user-fact-extraction thread read (`gated_meeting_thread_turns`), and the import reasoner handle
//! (`import_extraction_reasoner`). EVERY content read here stays GATED byte-identically.
//! `list_assistant_threads` fails CLOSED (poisoned lock ⇒ empty unlock set) and routes through
//! `list_assistant_threads_visible` (`visibility_clause`), so a sealed-and-not-session-unlocked meeting
//! returns EMPTY — never an existence-leaking error. `gated_meeting_thread_turns` snapshots the live
//! session unlock set via `super::unlocked_snapshot` and reads `list_assistant_interactions_visible`,
//! so a sealed meeting's turns are EMPTY (fail-closed) before they ever reach user-fact extraction.
//! `build_ask_vault_floor_prompt` / `ask_vault_loop` pack only the GATED corpus exactly as before.
//! Several members are promoted to `pub(crate)` (bodies byte-identical) because STAYING code in
//! `commands/mod.rs` still calls them — the staying `ask_vault` command (`capped_ask_history`,
//! `ask_vault_agentic_attempt`), the staying `ask_vault_floor` (`build_ask_vault_floor_prompt`), the
//! staying `ask_assistant_chat` command (`format_chat`), the sibling `commands/facts.rs`
//! (`gated_meeting_thread_turns`), the staying `import_memories` (`import_extraction_reasoner`), and
//! the staying `chat_format_tests`/`ask_vault_tests` modules. The shared `AskFloorPrompt` enum STAYS
//! in `commands/mod.rs` (promoted `pub(crate)`) because the staying `ask_vault_floor` consumes it;
//! `build_ask_vault_floor_prompt` reaches it through `use super::*`.
//! Every symbol keeps its EXACT prior body/signature and is re-exported at `crate::commands` via
//! `pub use ask_commands::*;` in `commands/mod.rs`, so `generate_handler![commands::ask_assistant_text]`
//! in `lib.rs` and every `crate::commands::…` caller resolve UNCHANGED. No gate LOGIC changed — only
//! relocation + visibility widening.

use super::*;

/// Ask the in-meeting assistant a TYPED question (the text composer — the twin of the voice trigger).
/// Routes the typed command through the SAME gated agentic brain as voice ([`spawn_assistant_turn`] →
/// `run_assistant_turn`): the model decides which gated tools to call, falling through to the
/// deterministic floor on no-consent / non-convergence, and the answer arrives via
/// `EVENT_VOICE_ACTION_RESULT` with the live tool-trace on `EVENT_ASSISTANT_TOOL`. Runs OFF-thread
/// (the brain can take seconds). The text is the user's OWN words — the SAME egress class as a
/// dictated voice command (no new egress). Emits the "thinking…" processing affordance immediately.
/// `thread_id` is OPTIONAL: the FE passes an @brain thread's id to keep the exchange in that
/// thread; when absent (the voice/wake twin sends none) the backend GENERATES a UUID v4 inside the
/// turn, so every persisted exchange carries a thread identity going forward.
/// `meeting_id` (FE camelCase `meetingId`) is the OPTIONAL scope meeting this thread is bound to
/// (Phase 4): the backend resolves `meeting_id.or(state.current_meeting)`, so an explicit FE id wins
/// (a past/anchored thread scopes correctly) while a `None` keeps the live-recording pointer. This is
/// what kills the wrong-meeting bug (idle @brain no longer defaults to a vault-wide arbitrary meeting).
#[tauri::command]
pub fn ask_assistant_text(
    app: AppHandle,
    text: String,
    thread_id: Option<String>,
    meeting_id: Option<String>,
) -> Result<(), AppError> {
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err(AppError::InvalidArg("empty question".into()));
    }
    let _ = app.emit(
        crate::events::EVENT_VOICE_COMMAND_PROCESSING,
        crate::events::VoiceCommandProcessingPayload { active: true },
    );
    crate::transcribe::live::spawn_assistant_turn(app, text, thread_id, meeting_id);
    Ok(())
}

/// One message in the in-meeting CHAT conversation (the dedicated chat panel). `role` is `"user"` or
/// `"assistant"`; the FE sends the FULL conversation (incl. the new user message as the last item) on
/// every turn, so the brain gets the prior turns as context (multi-turn memory). NO id/timestamp — the
/// FE owns the conversation state; the backend is stateless per call.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMsg {
    pub role: String,
    pub text: String,
}

/// Cap on conversation turns fed back as context — bounds tokens (and cloud egress) on a long chat.
pub(crate) const CHAT_CONTEXT_TURNS: usize = 12;

/// Brain v2 L3 — token-ish CHAR budget on the rendered chat history (~16k tokens). The turn-only
/// cap ([`CHAT_CONTEXT_TURNS`]) bounds the COUNT but not the SIZE — 12 pasted-document-sized turns
/// still blow a small model's context — so the char budget trims on top of it, OLDEST-first.
pub(crate) const CHAT_HISTORY_CHAR_BUDGET: usize = 64_000;

/// Brain v2 L3 — trim `messages` to the newest suffix whose total text is within `budget` chars.
/// OLDEST-first: walk backward from the newest message accumulating chars and cut where the budget
/// runs out. The NEWEST message is ALWAYS kept, even if it alone exceeds the budget (dropping the
/// user's live question is never acceptable — the provider's own limits bound that pathological
/// case). Pure slice-in/slice-out, so the boundary is unit-testable.
pub(crate) fn trim_history_to_budget(messages: &[ChatMsg], budget: usize) -> &[ChatMsg] {
    let mut total = 0usize;
    let mut start = messages.len();
    for (i, m) in messages.iter().enumerate().rev() {
        let cost = m.text.chars().count();
        // Keep the newest unconditionally; stop BEFORE an older message that would bust the budget.
        if start < messages.len() && total + cost > budget {
            break;
        }
        total = total.saturating_add(cost);
        start = i;
    }
    &messages[start..]
}

/// Format the chat `messages` into `(latest, conversation)`: `latest` is the user's newest message
/// (drives intent-routing + the deterministic floor), `conversation` is the recent history rendered
/// for the agentic loop's context — capped to the last [`CHAT_CONTEXT_TURNS`] turns AND (L3) to
/// [`CHAT_HISTORY_CHAR_BUDGET`] chars, oldest-first. Errors when the last message is not a
/// non-empty user message.
pub(crate) fn format_chat(messages: &[ChatMsg]) -> Result<(String, String), AppError> {
    let last = messages
        .last()
        .ok_or_else(|| AppError::InvalidArg("empty chat".into()))?;
    if !last.role.eq_ignore_ascii_case("user") || last.text.trim().is_empty() {
        return Err(AppError::InvalidArg(
            "the last chat message must be a non-empty user message".into(),
        ));
    }
    let latest = last.text.trim().to_string();
    let start = messages.len().saturating_sub(CHAT_CONTEXT_TURNS);
    let recent = trim_history_to_budget(&messages[start..], CHAT_HISTORY_CHAR_BUDGET);
    let mut convo =
        String::from("This is an ongoing chat during a live meeting. Conversation so far:\n");
    for m in recent {
        let who = if m.role.eq_ignore_ascii_case("assistant") {
            "Assistant"
        } else {
            "User"
        };
        convo.push_str(&format!("{who}: {}\n", m.text.trim()));
    }
    convo.push_str("\nAnswer the User's LATEST message, using the conversation above for context.");
    Ok((latest, convo))
}

/// List the PERSISTED @brain thread exchanges for a meeting (only rows carrying a `thread_id`),
/// oldest first — the durable substrate the FE rebuilds its thread panels from across meeting
/// switches / restarts. GATED read: it routes through `list_assistant_threads_visible`
/// (`visibility_clause`-backed), so a sealed-and-not-session-unlocked meeting returns EMPTY —
/// never an error that leaks existence. On seal the rows are purged anyway
/// (`purge_assistant_interactions_tx`); the gate is defense-in-depth.
#[tauri::command]
pub fn list_assistant_threads(
    state: State<'_, AppState>,
    meeting_id: String,
) -> Result<Vec<crate::storage::models::AssistantThreadRow>, AppError> {
    // Poisoned lock ⇒ empty unlock set ⇒ fail CLOSED (sealed meetings stay invisible) — the same
    // posture as the `get_meeting_detail` interactions read.
    let unlocked = state
        .unlocked_folders
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default();
    state
        .db
        .list_assistant_threads_visible(&meeting_id, &unlocked)
}

/// The reasoner the memory IMPORT extracts on: the LIGHT engine handle (`ReasonerCell::light`) —
/// LOCAL-or-stub, NEVER cloud, regardless of Brain Live / role config / consent. The FE copy
/// promises "extracts the durable facts on-device"; routing the pasted export through
/// `extraction_reasoner()` (cloud-classified Notes provider under the default `brain_live=false`)
/// would egress a content class users were told stays local (lock-security W2). Stub ⇒ the import
/// extracts nothing (0 imported) — degrade, never egress.
pub(crate) fn import_extraction_reasoner(
    state: &AppState,
) -> std::sync::Arc<dyn crate::reason::LocalReasoner> {
    state.reasoner.light()
}

/// Read the meeting's OWN @brain THREAD TURNS for user-fact extraction (design spec D5), GATED by the
/// live session unlock snapshot: `list_assistant_interactions_visible` returns the meeting's turns
/// only when the meeting is VISIBLE (a sealed-not-unlocked meeting returns EMPTY — fail-closed). Only
/// the USER COMMAND text is included (the high-signal part — an explicit "zapamiętaj, że…"); the
/// assistant's answer is never fed back into extraction. Best-effort: any read error ⇒ empty string
/// (extraction degrades to note+notes). Content-free on error.
pub(crate) fn gated_meeting_thread_turns(state: &AppState, meeting_id: &str) -> String {
    let unlocked = match unlocked_snapshot(state) {
        Ok(u) => u,
        Err(_) => return String::new(),
    };
    let turns = match state
        .db
        .list_assistant_interactions_visible(meeting_id, &unlocked)
    {
        Ok(t) => t,
        Err(_) => return String::new(),
    };
    turns
        .iter()
        .map(|i| format!("User: {}", i.command.trim()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Max agentic rounds for the Ask surface. Not live-latency-bound like the in-meeting cascade tiers
/// (`TIER1/2/3_MAX_STEPS` in `transcribe::live`, kept small so up-to-three tiers stay live-safe), so
/// the deliberately vault-wide Ask page gets a little more room to search + read — still bounded.
const ASK_MAX_STEPS: usize = 6;

pub(crate) struct DurableDispatchReasoner<'a> {
    inner: &'a dyn crate::reason::LocalReasoner,
    admission: crate::state::ContentDispatchAdmission,
}

pub(crate) fn durable_dispatch_reasoner<'a>(
    inner: &'a dyn crate::reason::LocalReasoner,
    admission: crate::state::ContentDispatchAdmission,
) -> DurableDispatchReasoner<'a> {
    DurableDispatchReasoner { inner, admission }
}

impl crate::reason::LocalReasoner for DurableDispatchReasoner<'_> {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn model_admission_managed(&self) -> bool {
        self.inner.model_admission_managed()
    }

    fn reason(&self, system: &str, user: &str) -> crate::error::Result<String> {
        self.inner.reason_admitted(system, user, &self.admission)
    }

    fn reason_with(
        &self,
        system: &str,
        user: &str,
        opts: crate::reason::GenOptions,
    ) -> crate::error::Result<String> {
        self.inner
            .reason_with_admitted(system, user, opts, &self.admission)
    }

    fn structured(
        &self,
        system: &str,
        user: &str,
        schema: &serde_json::Value,
    ) -> crate::error::Result<serde_json::Value> {
        self.inner
            .structured_admitted(system, user, schema, &self.admission)
    }

    fn structured_with(
        &self,
        system: &str,
        user: &str,
        schema: &serde_json::Value,
        opts: crate::reason::GenOptions,
    ) -> crate::error::Result<serde_json::Value> {
        self.inner
            .structured_with_admitted(system, user, schema, opts, &self.admission)
    }

    fn structured_with_observation(
        &self,
        system: &str,
        user: &str,
        schema: &serde_json::Value,
        opts: crate::reason::GenOptions,
    ) -> crate::error::Result<crate::reason::StructuredObservation> {
        self.inner
            .structured_with_observation_admitted(system, user, schema, opts, &self.admission)
    }
}

/// First cap the incoming Ask history to the last [`CHAT_CONTEXT_TURNS`] turns. The shared
/// `vault_chat::render_conversation` seam then applies the strict rendered-character budget to both
/// the deterministic floor and agentic Ask. Keeping the count cap here still avoids cloning an
/// unbounded number of durable rows before the render-time content bound is applied.
pub(crate) fn capped_ask_history(history: &[ChatTurn]) -> &[ChatTurn] {
    let start = history.len().saturating_sub(CHAT_CONTEXT_TURNS);
    &history[start..]
}

pub(crate) fn remove_duplicate_dashboard_question(history: &mut Vec<ChatTurn>, question: &str) {
    if history
        .last()
        .map(|turn| turn.role == "user" && turn.content.trim() == question.trim())
        == Some(true)
    {
        history.pop();
    }
}

/// The query vector Ask searches with — or an EMPTY one, which means "no KNN leg".
///
/// Both Ask paths go through here so there is one place to get this right, and one place to test.
/// The bug this replaced: `active_admitted_embedder` falls back to `StubEmbedder`, a hash bag whose
/// "similarity" carries no semantics, so without the model installed Ask embedded the question into
/// noise and fused it into the answer at full weight. `active_persistence_embedder_if_available`
/// returns `None` for a stub snapshot, and `search_hybrid_visible` short-circuits on an empty
/// vector, so the leg drops out and `score_fuse` redistributes its weight to the legs that have
/// something to say.
pub(crate) fn ask_query_vector(question: &str, semantic_enabled: bool) -> Vec<f32> {
    ask_query_vector_with(
        question,
        semantic_enabled,
        crate::embed::active_persistence_embedder_if_available(),
    )
}

/// As [`ask_query_vector`], with the embedder handed in.
///
/// The seam exists because resolving the embedder internally made the function untestable in the
/// direction that matters. Review proved it: a body of `Vec::new()` — ignoring both the flag and the
/// embedder — passed the test, because the suite only ever runs under the `#[cfg(test)]`-forced
/// stub, where empty is also the CORRECT answer. That mutant would silently disable semantic search
/// for every user who has the model, and nothing would have caught it.
///
/// With the handle injected, a test can pass a real one and assert the vector actually comes back,
/// which distinguishes "correctly calls through" from "always returns empty" without needing 470 MB
/// of weights on disk.
///
/// Errors are swallowed on purpose: a failure to embed is the same situation as no model, and Ask
/// degrading to keyword search beats Ask refusing to answer.
pub(crate) fn ask_query_vector_with(
    question: &str,
    semantic_enabled: bool,
    embedder: Option<Box<dyn crate::embed::Embedder>>,
) -> Vec<f32> {
    if !semantic_enabled {
        return Vec::new();
    }
    embedder
        .and_then(|embedder| {
            // QUERY side: the e5 `query:` prefix (asymmetric with the `passage:` index side).
            embedder
                .embed_query(std::slice::from_ref(&question.to_string()))
                .ok()
        })
        .and_then(|vectors| vectors.into_iter().next())
        .unwrap_or_default()
}

/// Run the vault-scoped agentic attempt for [`ask_vault`]. Returns `Some(result)` ONLY when the
/// loop CONVERGED; `None` on non-convergence or ordinary loop errors — incl. `Unavailable` (no cloud
/// consent) — so the caller floors to the pre-agentic path with its original semantics. A
/// visibility `Locked` error propagates and MUST NOT fall through to another provider path.
pub(crate) fn ask_vault_agentic_attempt(
    app: &AppHandle,
    question: &str,
    history: &[ChatTurn],
    thread_id: &str,
    config: AppConfig,
    dispatch_admission: crate::state::ContentDispatchAdmission,
    durable_history: bool,
) -> Result<Option<AskVaultResult>, AppError> {
    let state = app.state::<AppState>();
    // Re-resolved per turn (never a startup snapshot): consent/provider/backend changes apply.
    // ASK role — under the legacy fallback this dispatches exactly like the pre-role `current()`.
    let reasoner = state
        .reasoner
        .current_for_config(crate::summarize::roles::Role::Ask, &config);
    // VAULT-SCOPED executor: no live meeting, READ-ONLY, and NO note drafts (the Ask page has no
    // notes flow / Accept affordance, so `propose_note` is not advertised on this surface). The
    // AppHandle is present so web_search / calendar_lookup participate under their existing
    // consent/availability gates. Every read re-checks the LIVE unlocked set per call (C6).
    let executor = crate::tools::GatedToolExecutor {
        db: &state.db,
        unlocked: &state.unlocked_folders,
        config: &config,
        meeting_id: "",
        app: Some(app),
        recording_token: None,
        allow_writes: false,
        note_drafts: false,
        // The Ask page is DELIBERATELY vault-wide (Phase 5 preserves it unchanged) — the FULL
        // per-surface catalog, NOT a cascade tier: it is not the in-meeting @brain surface the
        // current-first cascade governs.
        scope: if durable_history {
            crate::tools::AssistantScope::DurableAsk
        } else {
            crate::tools::AssistantScope::Full
        },
        // Seal-on-write handles (residual W1): read-only today (`allow_writes: false`), but the
        // executor carries the live seam so a future write surface can never silently skip it.
        seal: Some(crate::tools::SealAccess {
            master_kek: &state.master_kek,
            lifecycle: &state.lifecycle,
        }),
        proposed_note: std::sync::Mutex::new(None),
    };
    let sink = crate::transcribe::live::ToolEventSink {
        app: app.clone(),
        event: crate::events::EVENT_ASK_TOOL,
        thread_id: thread_id.to_string(),
    };
    // Gated cross-meeting USER MEMORY brief for the agentic persona (parity with the @brain loop):
    // VISIBLE facts only under the LIVE unlock snapshot, empty when memory is disabled ⇒ the persona
    // is byte-identical. Rides the loop's existing redaction + consent egress (no new class).
    let unlocked_now = state
        .unlocked_folders
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default();
    let memory_brief = gated_memory_brief_for_injection(&state, &unlocked_now, question);
    // Brain v2 L3 — JIT retrieval (behind `ask_jit_retrieval`, default OFF): seed the persona with
    // a compact GATED meeting listing (id | title | date, top-30 — hybrid when semantic search is
    // on, gated FTS otherwise) + search-then-`get_meeting` instructions, instead of any pre-packed
    // content. Flag OFF passes "" ⇒ the persona is BYTE-IDENTICAL to the legacy agentic prompt.
    // Every candidate source is `visibility_clause`-gated, so a sealed-not-unlocked meeting
    // contributes no line; a listing failure degrades to the legacy prompt (never an error).
    let jit_listing = if config.ask_jit_retrieval {
        let query_vec = ask_query_vector(question, config.semantic_search_enabled);
        crate::summarize::vault_context::build_meeting_listing_visible(
            &state.db,
            question,
            &query_vec,
            30,
            &unlocked_now,
        )
        .unwrap_or_default()
    } else {
        String::new()
    };
    // L3: the ASK preset + the `loop_transcript_compaction` flag (default ON) + the default-off
    // grammar gate (a no-op on today's cloud-only agentic reasoners).
    let opts = crate::reason::GenOptions::ask_answer()
        .with_transcript_compaction(config.loop_transcript_compaction)
        .with_grammar_constraint(config.brain_heavy_grammar_enabled);
    // Persisted conversations intentionally never ingest mutable org-replica content: v1 has no
    // typed org provenance on each exchange. Keep both the catalog and persona hint aligned.
    let org_available = !durable_history && crate::tools::org_brain_available(&state.db, &config);
    // Durable Ask must bind BOTH model/provider dispatch and every model-selected connector
    // future to the same lifecycle admission. The adapter preserves every stateless caller while
    // making relock/reseal cancellation cover connector factory + every async poll.
    let executor = crate::agent::AdmittedToolExecutor::new(&executor, dispatch_admission.clone());
    let reasoner = durable_dispatch_reasoner(&*reasoner, dispatch_admission);
    match ask_vault_loop(
        &reasoner,
        &executor,
        &state.db,
        &state.unlocked_folders,
        question,
        history,
        &memory_brief,
        &jit_listing,
        org_available,
        Some(&sink as &dyn crate::agent::DeltaSink),
        opts,
    ) {
        Ok(converged) => Ok(converged),
        Err(AppError::Locked(message)) => Err(AppError::Locked(message)),
        Err(e) => {
            // PII rule: the error only — never the question/history text.
            tracing::debug!(
                target: "ask",
                error = %e,
                "ask agentic loop unavailable/failed; flooring to corpus completion"
            );
            Ok(None)
        }
    }
}

/// The testable core of the agentic Ask path: drive [`crate::agent::run_agentic_loop`] with the
/// vault-QA persona over the rendered conversation, then map a converged outcome onto the Ask DTO.
/// `Ok(None)` = non-convergence (caller floors); `Err` propagates (caller floors) — the loop
/// contract of `run_informational`, applied to the Ask surface.
///
/// `jit_listing` (Brain v2 L3) is the compact gated meeting listing for JIT retrieval — `""` (the
/// `ask_jit_retrieval`-off path) keeps the persona BYTE-IDENTICAL to the legacy agentic prompt
/// (`agentic_system_jit`'s empty-listing contract). `opts` carries the caller's per-step
/// generation bounds (the P0.3 ASK preset + the L3 compaction/grammar flags). `org_available` (A2)
/// is threaded straight to `agentic_system_jit` — the caller passes the SAME `org_brain_available`
/// predicate that gates the tool's own advertisement, so the hint and the actual tool availability
/// can never diverge.
#[allow(clippy::too_many_arguments)]
pub(crate) fn ask_vault_loop(
    reasoner: &dyn crate::reason::LocalReasoner,
    executor: &dyn crate::agent::ToolExecutor,
    db: &crate::storage::Db,
    unlocked: &std::sync::Mutex<std::collections::HashSet<String>>,
    question: &str,
    history: &[ChatTurn],
    memory_brief: &str,
    jit_listing: &str,
    org_available: bool,
    sink: Option<&dyn crate::agent::DeltaSink>,
    opts: crate::reason::GenOptions,
) -> Result<Option<AskVaultResult>, AppError> {
    let system =
        crate::summarize::vault_chat::agentic_system_jit(memory_brief, jit_listing, org_available);
    let user = crate::summarize::vault_chat::render_conversation(history, question);
    let Some(outcome) = crate::agent::run_agentic_loop_with_policy(
        reasoner,
        &system,
        &user,
        executor,
        ASK_MAX_STEPS,
        sink,
        opts,
        crate::agent::AnswerGroundingPolicy::RetryUnknownAfterUnopenedSearchHit,
    )?
    else {
        return Ok(None);
    };
    // Resolve sources against the LIVE unlocked set (fail-closed on a poisoned lock: no source
    // chips rather than an ungated resolution).
    let unlocked_now = unlocked.lock().map(|g| g.clone()).unwrap_or_default();
    Ok(Some(agent_outcome_to_ask_result(
        db,
        &unlocked_now,
        outcome,
    )))
}

/// Map a converged [`crate::agent::AgentOutcome`] onto the Ask DTO. `citations` carries the loop's
/// gated citation strings verbatim (`[[Title]]` / `(web) …`); `sources` additionally resolves each
/// `[[Title]]` to its VISIBLE meeting (id + date) so the existing source chips keep working. A
/// title that doesn't resolve to a visible meeting simply contributes no source — never an error,
/// never an ungated read (`meeting_by_title_visible` applies the same visibility predicate as
/// every gated reader).
fn agent_outcome_to_ask_result(
    db: &crate::storage::Db,
    unlocked: &std::collections::HashSet<String>,
    outcome: crate::agent::AgentOutcome,
) -> AskVaultResult {
    let mut sources: Vec<crate::storage::models::VaultSource> = Vec::new();
    for cite in &outcome.citations {
        let Some(title) = cite.strip_prefix("[[").and_then(|c| c.strip_suffix("]]")) else {
            continue; // "(web) …" / "(calendar) …" attributions have no meeting to resolve.
        };
        match db.meeting_by_title_visible(title, unlocked) {
            Ok(Some(m)) if !sources.iter().any(|s| s.meeting_id == m.id) => {
                sources.push(crate::storage::models::VaultSource {
                    meeting_id: m.id,
                    title: m.title.unwrap_or_else(|| title.to_string()),
                    started_at: m.started_at,
                    origin: None,
                });
            }
            Ok(_) => {}
            Err(e) => {
                tracing::debug!(target: "ask", error = %e, "citation source resolution failed")
            }
        }
    }
    AskVaultResult {
        answer: outcome.answer,
        sources,
        citations: outcome.citations,
    }
}

/// Everything the pre-agentic `ask_vault` did BEFORE its provider call, verbatim: gated corpus
/// assembly (hybrid when semantic search is ON, FTS otherwise — Phase 2b semantics unchanged), the
/// empty-corpus early return, and the corpus prompt build. The floor-equivalence test binds this
/// to the original statement sequence.
#[allow(clippy::too_many_arguments)] // cohesive gated-Ask surface: corpus/consent state + explicit sources.
pub(crate) fn build_ask_vault_floor_prompt(
    db: &crate::storage::Db,
    config: &AppConfig,
    unlocked: &std::collections::HashSet<String>,
    question: &str,
    history: &[ChatTurn],
    memory_brief: &str,
    reranker: Option<&dyn crate::rerank::Reranker>,
    explicit_sources: Option<&[crate::storage::models::SourceRef]>,
    pinned_org_item_id: Option<&str>,
    // CONTAINER SCOPE — folder/Space ids the answer must be drawn from, or `None` for the whole
    // vault. Deliberately NOT part of `explicit_sources`: those are PINNED CONTENT, packed into the
    // corpus verbatim, and `LinkKind::Container` is excluded from that set on purpose (a container
    // holds no text of its own). A scope is the other thing entirely — it narrows RETRIEVAL, so a
    // folder of a hundred notes costs what a folder of five costs, and ranking still decides what
    // is relevant inside it.
    scope_folder_ids: Option<&[String]>,
) -> Result<AskFloorPrompt, AppError> {
    // Budget on the ASK-role provider's RESOLVED connection — the corpus egresses to it. With
    // role keys absent this is the legacy `provider_id` for EVERY brain_backend (the pre-role
    // floor always ignored `brain_backend`), so the packed corpus is byte-identical.
    let ask_conn =
        crate::summarize::roles::provider_target(crate::summarize::roles::Role::Ask, config)
            .connection;
    // note↔meeting-links PR-2 — PINNED corpus: when the caller supplied an explicit source list, the
    // FTS/vector SEARCH is skipped entirely and the corpus is EXACTLY those sources (+ their capped,
    // gated link-expansion) — the user controls the context. Same `unlocked` visibility gate as
    // every search leg (a sealed source/neighbour contributes nothing). `None` ⇒ the exact existing
    // whole-vault search below, byte-for-byte.
    // Expand each scoped container to its SUBTREE. Scoping to a Space means "everything filed
    // under it", which is the opposite of what a container RELATION does (that one never fans out)
    // — the difference is the point: a relation says where something belongs, a scope says where to
    // look. Resolution is by folder id, so the scope survives a rename or a move of the folder.
    let scope_ids: Option<Vec<String>> = match scope_folder_ids {
        Some(ids) if !ids.is_empty() => Some(db.folder_scope_ids(ids)?),
        _ => None,
    };
    let scope = scope_ids.as_deref();
    let has_pinned_sources = explicit_sources.map(|s| !s.is_empty()).unwrap_or(false);
    let (corpus, sources) = if has_pinned_sources || pinned_org_item_id.is_some() {
        // PINNED corpus (deterministic; vault-wide search SKIPPED). Pack the pinned ORG item FIRST
        // (the shared note being viewed — pinned so it's ALWAYS in context; the local Brain's search
        // never surfaces org-feed content), then the explicit sources (+ their gated link-expansion).
        // In current callers the two are mutually exclusive — the org viewer sends only the org id,
        // the note editor sends only explicit sources — so no double-budget concern arises.
        let budget = crate::summarize::vault_context::budget_for(&ask_conn);
        let mut corpus = String::new();
        if let Some(org_id) = pinned_org_item_id {
            corpus.push_str(&crate::summarize::vault_context::pack_pinned_org_item(
                db, org_id, &ask_conn,
            )?);
        }
        let sources = if let Some(srcs) = explicit_sources.filter(|s| !s.is_empty()) {
            let (src_corpus, src_sources) =
                crate::summarize::vault_context::build_vault_context_pinned_visible_with_budget(
                    db, srcs, budget, unlocked,
                )?;
            if !corpus.is_empty() && !src_corpus.is_empty() {
                corpus.push_str("\n\n");
            }
            // The pinned org item is packed FIRST + is the primary anchor. If a caller supplies BOTH
            // a pinned org id AND explicit sources (the org viewer's Ask WITH user-added scope), cap
            // the sources to the REMAINING budget so the combined corpus honors ONE budget, not two —
            // load-bearing for tiny local (Ollama) context windows. `chars().take` is char-boundary
            // safe (mirrors pack_notes/pack_pinned_org_item). Org-only or sources-only ⇒ this is a
            // no-op (remaining == full budget, and the un-pinned side is empty).
            let remaining = budget.saturating_sub(corpus.len());
            corpus.extend(src_corpus.chars().take(remaining));
            src_sources
        } else {
            Vec::new()
        };
        (corpus, sources)
    } else if config.semantic_search_enabled {
        let query_vec = ask_query_vector(question, true);
        crate::summarize::vault_context::build_vault_context_hybrid_visible(
            db, question, &ask_conn, &query_vec, unlocked, reranker, scope,
        )?
    } else {
        crate::summarize::vault_context::build_vault_context_visible(
            db, question, &ask_conn, unlocked, scope,
        )?
    };
    if corpus.trim().is_empty() {
        return Ok(AskFloorPrompt::Empty(AskVaultResult {
            answer: "No meeting notes to search yet — record and summarize a meeting first."
                .to_string(),
            sources: Vec::new(),
            citations: Vec::new(),
        }));
    }
    let (system, user) =
        crate::summarize::vault_chat::build(&corpus, history, question, memory_brief);
    Ok(AskFloorPrompt::Ready {
        system,
        user,
        sources,
    })
}
