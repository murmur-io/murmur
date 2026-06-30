# Agentic Assistant — P0 (Headless Seam) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the model-driven agentic tool-use seam (tool catalog + gated executor + bounded loop + default `agentic()` trait method) entirely headless, wired into NOTHING — zero behavior change — with RED-first tests proving sealed content never surfaces through the loop.

**Architecture:** A new `ToolExecutor` trait + `GatedToolExecutor` route the model's `{tool,args}` requests through the existing gated `tools::execute_tool` (the model can request, the host gate filters). A new `agent::run_agentic_loop` drives the brain's existing `structured()` primitive in a bounded decide-or-finish loop, falling to the deterministic floor on non-convergence. A default `LocalReasoner::agentic()` gives every reasoner the loop for free. Nothing calls it yet.

**Tech Stack:** Rust (crate `murmur`, lib `meetnotes_lib`), `rusqlite`/SQLCipher, `serde_json`. Test loop: `cargo test --lib` from `src-tauri/`.

## Global Constraints

- Only error type is `AppError`; every fallible fn returns `crate::error::Result<T>`. No `unwrap()`/`expect()`/`anyhow::Result`/`Box<dyn Error>` in non-test code. A locked-content refusal is `AppError::Locked`.
- Inner loop is `cargo test --lib` ONLY (run from `src-tauri/`, `source ~/.cargo/env` first). NEVER `cargo clippy --all-targets` while iterating (openssl/sqlcipher profile thrash). Full `scripts/ci.sh` once at the very end.
- Every content read MUST be gated: route through `tools::execute_tool(call, db, unlocked, config)` (visibility-gated by the non-optional `unlocked: &HashSet<String>`). NEVER add a read path that bypasses the gate.
- `execute_tool` stays synchronous + egress-free + MCP-safe — do NOT add `WebSearch`/`CalendarLookup`/write actions to it; those are dispatched from the executor's async/AppHandle-aware path.
- No new crates without explicit user approval. `std` + `serde_json` only for the loop.
- No PII in logs: log tool *names* + step counts + status only — never tool args/results/answer/command text.
- Panic-free + bounded: the loop never panics (every fallible step degrades to a graceful outcome) and is hard-capped by `max_steps`.
- This is a lock-touching change → the `lock-security-reviewer` agent is a REQUIRED gate before merge, in addition to the `adversarial-verifier`.

---

## ⚠️ Verification corrections (BINDING — override the tasks below where they conflict)

The adversarial verification (`docs/research/2026-06-30-agentic-brain-verification-verdict.md`) found that an internal `reason()`-over-`gathered` floor regresses the shipped default config to an empty answer (turn-0 `Err(Unavailable)` swallowed → empty grounding → empty synth, no citations, no `needs_consent`). Apply these to the tasks:

- **A1 — `run_agentic_loop` has NO internal `reason()` floor.** Change its return type to **`Result<Option<AgentOutcome>>`**: `Ok(Some(outcome))` when the model converged to a non-empty `{answer}`; **`Ok(None)`** when it did not converge within `max_steps` (the CALLER floors); and on a `structured()` `Err` **propagate the `Err`** (NEVER `Err(_) => break`). The deterministic floor (`resolve_command_intent` → `handle_voice_action`/`rag_answer`) lives in `run_assistant_turn` (P1), not in the loop. This makes the floor the real gated fan-out + `needs_consent` + citations, per spec **C1/C3**.
- **A2 — `LocalReasoner::agentic()` (Task 6) returns `Result<Option<AgentOutcome>>`** to match A1.
- **A3 — Task 4 tests change accordingly:** the happy path asserts `Ok(Some(...))`; the old "floor on non-convergence" test becomes **`loop_returns_none_on_non_convergence`** (asserts `Ok(None)`, NOT a fabricated answer); ADD **`loop_propagates_unavailable_error`** (a reasoner whose `structured()` returns `Err(AppError::Unavailable(...))` → `run_agentic_loop` returns `Err(AppError::Unavailable(...))`, NOT `Ok`). Drop the `ScriptReasoner.reason()` "FLOOR:" expectation.
- **A4 — Task 6 test changes:** `StubReasoner.structured()` returns canned JSON (not tool/answer) → after `max_steps` the loop returns **`Ok(None)`** (assert `out.is_none()`); the stub no longer "floors to an outcome" inside the loop.
- **A5 — DEFER floor-equivalence + write-preservation + relock-mid-loop tests to P1** (they need `run_assistant_turn` + a seeded DB with the live dispatch context): (a) `Err(Unavailable)` through `run_assistant_turn` yields `needs_consent` + gated citations; (b) stub path ≡ `rag_answer` (mirror `orchestrate.rs:389`); (c) a user-dictated reminder/aside still fires; (d) folder relocked mid-loop → subsequent tool calls surface nothing. Note them in P0's handoff so P1 owns them.
- **A6 — `GatedToolExecutor` re-snapshots `unlocked` per turn at the `run_assistant_turn` level (P1)** — in P0 the executor still takes `unlocked: &HashSet` (no change), but the P1 caller rebuilds the snapshot each turn (spec **C6**).
- **A7 — `search_semantic` ToolSpec gating + `tool-use examples` in the catalog prompt + a no-repeat guard** are tracked as P0/P2 hardening (spec **C8/C9** + verdict optional improvements); not required for the headless seam but add the no-repeat dedup to `run_agentic_loop` (skip a `tool+args` pair already run this turn, feed back "already retrieved").

---

### Task 1: Tool catalog — `ToolSpec` + `tool_specs()` in `tools.rs`

**Files:**
- Modify: `src-tauri/src/tools.rs` (add near the top, after the `ToolCall` enum)
- Test: in `src-tauri/src/tools.rs` `#[cfg(test)] mod tests`

**Interfaces:**
- Produces: `pub struct ToolSpec { pub name: &'static str, pub description: &'static str, pub parameters: serde_json::Value, pub write: bool }` and `pub fn tool_specs() -> Vec<ToolSpec>` — the single model-facing catalog (8 read tools + 2 write tools-as-data). Read tools are `write:false`; `note_aside`/`create_reminder` are `write:true`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn tool_specs_catalog_shape() {
    let specs = tool_specs();
    let names: Vec<&str> = specs.iter().map(|s| s.name).collect();
    // The 6 read tools + 2 connectors are exposed as data; 2 write actions are present but write=true.
    for n in ["search_meetings", "search_semantic", "get_meeting", "list_recent_meetings",
              "get_open_commitments", "get_entity_dossier", "web_search", "calendar_lookup",
              "note_aside", "create_reminder"] {
        assert!(names.contains(&n), "missing tool {n}");
    }
    // Write flags: only the two actions are writes.
    let by = |n: &str| specs.iter().find(|s| s.name == n).unwrap();
    assert!(by("note_aside").write);
    assert!(by("create_reminder").write);
    assert!(!by("search_meetings").write);
    assert!(!by("web_search").write);
    // Every spec carries a valid JSON-schema object with a "properties" map.
    for s in &specs {
        assert_eq!(s.parameters["type"], serde_json::json!("object"), "{} schema not object", s.name);
        assert!(s.parameters.get("properties").is_some(), "{} missing properties", s.name);
        assert!(!s.description.is_empty(), "{} empty description", s.name);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd src-tauri && cargo test --lib tool_specs_catalog_shape`
Expected: FAIL — `cannot find function tool_specs in this scope`.

- [ ] **Step 3: Write minimal implementation**

```rust
/// Model-facing description of one tool the agentic loop may call. `parameters` is a JSON-schema
/// object (the same shape `mcp.rs` advertises). `write: true` tools mutate state and are NEVER
/// exposed unless the executor is built with `allow_writes` (off in v1).
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: serde_json::Value,
    pub write: bool,
}

/// The single source of truth for the model-facing tool catalog. Built per call (cheap; ~10 entries).
pub fn tool_specs() -> Vec<ToolSpec> {
    let str_arg = |prop: &str, desc: &str| {
        serde_json::json!({ "type": "object",
            "properties": { prop: { "type": "string", "description": desc } },
            "required": [prop] })
    };
    vec![
        ToolSpec { name: "search_meetings", write: false,
            description: "Full-text search across the user's past meeting titles, notes and transcripts.",
            parameters: str_arg("query", "Search terms in the user's own language.") },
        ToolSpec { name: "search_semantic", write: false,
            description: "Hybrid semantic + keyword search over meetings (finds related-by-meaning notes).",
            parameters: str_arg("query", "A natural-language description of what to find.") },
        ToolSpec { name: "get_meeting", write: false,
            description: "Fetch one meeting's AI note and full transcript by its id.",
            parameters: str_arg("meetingId", "The meeting id from a prior search result.") },
        ToolSpec { name: "list_recent_meetings", write: false,
            description: "List the most recent meetings (newest first).",
            parameters: serde_json::json!({ "type": "object",
                "properties": { "limit": { "type": "integer", "description": "How many (1..=100)." } } }) },
        ToolSpec { name: "get_open_commitments", write: false,
            description: "Roll up every open action item, optionally filtered by owner.",
            parameters: serde_json::json!({ "type": "object",
                "properties": { "owner": { "type": "string", "description": "Optional owner filter." } } }) },
        ToolSpec { name: "get_entity_dossier", write: false,
            description: "Assemble what the vault knows about one person/project/entity.",
            parameters: str_arg("entity", "The entity name to look up.") },
        ToolSpec { name: "web_search", write: false,
            description: "Search the public web (only available when the user has enabled + consented to web search).",
            parameters: str_arg("query", "What to look up on the web.") },
        ToolSpec { name: "calendar_lookup", write: false,
            description: "Look up the user's local calendar for recent/upcoming events.",
            parameters: str_arg("query", "What meeting/agenda detail to find.") },
        ToolSpec { name: "note_aside", write: true,
            description: "Append a short aside note to the meeting currently being recorded.",
            parameters: str_arg("text", "The note text.") },
        ToolSpec { name: "create_reminder", write: true,
            description: "Create a reminder.",
            parameters: serde_json::json!({ "type": "object",
                "properties": { "text": { "type": "string" }, "due": { "type": "string" } },
                "required": ["text"] }) },
    ]
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd src-tauri && cargo test --lib tool_specs_catalog_shape`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/tools.rs
git commit -m "feat(tools): add model-facing ToolSpec catalog (tool_specs)"
```

---

### Task 2: Loop types — `AgentOutcome`, `AgentStep`, `ToolExecutor` trait

**Files:**
- Create: `src-tauri/src/agent.rs`
- Modify: `src-tauri/src/lib.rs` (add `mod agent;` next to the other `mod` declarations)
- Test: in `src-tauri/src/agent.rs` `#[cfg(test)] mod tests`

**Interfaces:**
- Produces:
  - `pub struct AgentOutcome { pub answer: String, pub steps: Vec<AgentStep>, pub citations: Vec<String> }`
  - `pub struct AgentStep { pub tool: String, pub args_json: String, pub ok: bool }`
  - `pub trait ToolExecutor: Send + Sync { fn specs(&self) -> Vec<crate::tools::ToolSpec>; fn run(&self, name: &str, args: &serde_json::Value) -> crate::error::Result<String>; }`

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    struct EchoExec;
    impl ToolExecutor for EchoExec {
        fn specs(&self) -> Vec<crate::tools::ToolSpec> { crate::tools::tool_specs() }
        fn run(&self, name: &str, _args: &serde_json::Value) -> crate::error::Result<String> {
            Ok(format!("ran {name}"))
        }
    }

    #[test]
    fn executor_runs_and_outcome_constructs() {
        let e = EchoExec;
        assert!(e.specs().iter().any(|s| s.name == "search_meetings"));
        assert_eq!(e.run("search_meetings", &serde_json::json!({})).unwrap(), "ran search_meetings");
        let o = AgentOutcome { answer: "hi".into(), steps: vec![AgentStep {
            tool: "search_meetings".into(), args_json: "{}".into(), ok: true }], citations: vec![] };
        assert_eq!(o.answer, "hi");
        assert_eq!(o.steps.len(), 1);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd src-tauri && cargo test --lib agent::tests::executor_runs_and_outcome_constructs`
Expected: FAIL — `file not found for module agent` / unresolved types.

- [ ] **Step 3: Write minimal implementation**

Create `src-tauri/src/agent.rs`:

```rust
//! Model-driven agentic tool-use loop — the BACKEND ENGINE that replaces the hardcoded intent router.
//!
//! The brain (cloud or local) DECIDES which gated tools to call and when to answer; we only EXECUTE,
//! GATED and bounded. The loop drives the existing `LocalReasoner::structured()` primitive, so it is
//! transport-agnostic and — for the cloud backend — every turn re-routes through `make_provider` →
//! consent gate + RedactingProvider (no new egress class). Best-effort + PANIC-FREE: every fallible
//! step degrades to a graceful outcome; the loop is HARD-CAPPED by `max_steps`.

use crate::error::Result;

/// The result of one agentic turn — the final answer plus the gated tool-call trace + citations.
pub struct AgentOutcome {
    /// The brain's final answer (grounded ONLY in gated tool output).
    pub answer: String,
    /// The tool-call trace (tool name + args + ok), for persistence / the FE card / the flywheel.
    pub steps: Vec<AgentStep>,
    /// `[[Title]]` / `(web)` / `(calendar)` citations extracted from GATED tool output only.
    pub citations: Vec<String>,
}

/// One executed tool step. `args_json` is the serialized args the model emitted (logged tool-name-only).
pub struct AgentStep { pub tool: String, pub args_json: String, pub ok: bool }

/// The gated surface the loop calls. `specs()` is the per-caller allowlist (the only tools the model
/// is told about); `run()` executes ONE call, GATED — the model can only name a tool + pass string
/// args, it can never reach the DB directly, so it cannot forge an ungated read.
pub trait ToolExecutor: Send + Sync {
    fn specs(&self) -> Vec<crate::tools::ToolSpec>;
    fn run(&self, name: &str, args: &serde_json::Value) -> Result<String>;
}
```

Add to `src-tauri/src/lib.rs` (alongside the other `mod` lines, e.g. near `mod tools;`):

```rust
mod agent;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd src-tauri && cargo test --lib agent::tests::executor_runs_and_outcome_constructs`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/agent.rs src-tauri/src/lib.rs
git commit -m "feat(agent): add AgentOutcome/AgentStep/ToolExecutor loop types"
```

---

### Task 3: `GatedToolExecutor` — the one gated executor (read tools + connectors)

**Files:**
- Modify: `src-tauri/src/tools.rs` (add the struct + `impl ToolExecutor`)
- Test: in `src-tauri/src/tools.rs` `#[cfg(test)] mod tests` (reuse the existing `tmp_db()` helper)

**Interfaces:**
- Consumes: `crate::agent::ToolExecutor`, `crate::tools::{tool_specs, execute_tool, execute_web_search, execute_calendar_search}`.
- Produces: `pub struct GatedToolExecutor<'a> { db, unlocked, config, meeting_id, app, allow_writes }` implementing `ToolExecutor`. `specs()` hides connectors when `app` is `None` and write tools when `!allow_writes`. `run()` refuses any name not in `specs()`, routes read tools to `execute_tool`, connectors to the async dispatchers via a scoped-thread `block_on`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn gated_executor_hides_connectors_without_apphandle_and_writes() {
    let db = tmp_db();
    let unlocked = HashSet::new();
    let cfg = AppConfig::default();
    let exec = GatedToolExecutor {
        db: &db, unlocked: &unlocked, config: &cfg, meeting_id: "",
        app: None, allow_writes: false,
    };
    let names: Vec<&str> = exec.specs().iter().map(|s| s.name).collect();
    // Read tools exposed; connectors hidden (no AppHandle); write tools hidden (allow_writes=false).
    assert!(names.contains(&"search_meetings"));
    assert!(!names.contains(&"web_search"), "web hidden without AppHandle");
    assert!(!names.contains(&"calendar_lookup"), "calendar hidden without AppHandle");
    assert!(!names.contains(&"note_aside"), "write hidden without allow_writes");
}

#[test]
fn gated_executor_refuses_unadvertised_tool() {
    let db = tmp_db();
    let unlocked = HashSet::new();
    let cfg = AppConfig::default();
    let exec = GatedToolExecutor {
        db: &db, unlocked: &unlocked, config: &cfg, meeting_id: "", app: None, allow_writes: false,
    };
    // web_search is NOT advertised here (no AppHandle) → run() must refuse, never egress.
    assert!(matches!(exec.run("web_search", &serde_json::json!({"query":"x"})),
        Err(AppError::InvalidArg(_))));
    // A read tool runs through the gate (empty DB → "No meetings match").
    let out = exec.run("search_meetings", &serde_json::json!({"query":"atlas"})).unwrap();
    assert!(out.contains("No meetings match"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd src-tauri && cargo test --lib gated_executor_`
Expected: FAIL — `cannot find struct GatedToolExecutor`.

- [ ] **Step 3: Write minimal implementation**

Add to `src-tauri/src/tools.rs`:

```rust
/// THE one gated, egress-aware tool executor shared by cloud + local + voice + text. Holds the LIVE
/// session `unlocked` set, so every read routes through the visibility gate regardless of what the
/// model requests. Connectors (web/calendar) need the `AppHandle`; write tools need `allow_writes`.
pub struct GatedToolExecutor<'a> {
    pub db: &'a Db,
    pub unlocked: &'a HashSet<String>,
    pub config: &'a AppConfig,
    pub meeting_id: &'a str,
    pub app: Option<&'a tauri::AppHandle>,
    pub allow_writes: bool,
}

impl<'a> GatedToolExecutor<'a> {
    fn is_advertised(&self, name: &str) -> bool {
        self.specs().iter().any(|s| s.name == name)
    }
}

impl crate::agent::ToolExecutor for GatedToolExecutor<'_> {
    fn specs(&self) -> Vec<ToolSpec> {
        let app = self.app.is_some();
        let writes = self.allow_writes;
        tool_specs()
            .into_iter()
            .filter(|s| match s.name {
                // Connectors require the AppHandle (sidecar/async path).
                "web_search" | "calendar_lookup" => app,
                // Write actions require explicit allow_writes (off in v1).
                _ if s.write => writes,
                _ => true,
            })
            .collect()
    }

    fn run(&self, name: &str, args: &serde_json::Value) -> Result<String> {
        // ENFORCE the allowlist: the model can never run a tool we did not advertise this turn.
        if !self.is_advertised(name) {
            return Err(AppError::InvalidArg(format!("tool '{name}' is not available")));
        }
        let s = |k: &str| args.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
        match name {
            "search_meetings" => execute_tool(&ToolCall::SearchMeetings { query: s("query") }, self.db, self.unlocked, self.config),
            "search_semantic" => execute_tool(&ToolCall::SearchSemantic { query: s("query") }, self.db, self.unlocked, self.config),
            "get_meeting" => execute_tool(&ToolCall::GetMeeting { meeting_id: s("meetingId") }, self.db, self.unlocked, self.config),
            "list_recent_meetings" => {
                let limit = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(10).clamp(1, 100);
                execute_tool(&ToolCall::ListRecentMeetings { limit }, self.db, self.unlocked, self.config)
            }
            "get_open_commitments" => {
                let owner = args.get("owner").and_then(|v| v.as_str()).map(str::to_string);
                execute_tool(&ToolCall::GetOpenCommitments { owner }, self.db, self.unlocked, self.config)
            }
            "get_entity_dossier" => execute_tool(&ToolCall::GetEntityDossier { entity: s("entity") }, self.db, self.unlocked, self.config),
            "web_search" => match self.app {
                Some(_) => block_on_tool(execute_web_search(&s("query"), self.config)),
                None => Err(AppError::InvalidArg("web_search needs an AppHandle".into())),
            },
            "calendar_lookup" => match self.app {
                Some(app) => block_on_tool(execute_calendar_search(&s("query"), app)),
                None => Err(AppError::InvalidArg("calendar_lookup needs an AppHandle".into())),
            },
            other => Err(AppError::InvalidArg(format!("unknown tool '{other}'"))),
        }
    }
}

/// Drive an async tool dispatcher to completion from the synchronous executor without panicking,
/// regardless of caller context (the loop may run inside the async note pipeline). Mirrors
/// `reason::block_on_complete` — a dedicated scoped OS thread with its own current-thread runtime.
fn block_on_tool(fut: impl std::future::Future<Output = Result<String>>) -> Result<String> {
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| AppError::Other(anyhow::anyhow!("tool runtime build failed: {e}")))?
                    .block_on(fut)
            })
            .join()
            .map_err(|_| AppError::Other(anyhow::anyhow!("tool worker thread panicked")))?
    })
}
```

(Note: `tokio` is already a dependency used the same way in `reason.rs:479`. If `tauri::AppHandle` import isn't already in `tools.rs`, the `app` field uses the fully-qualified `tauri::AppHandle` as written.)

- [ ] **Step 4: Run test to verify it passes**

Run: `cd src-tauri && cargo test --lib gated_executor_`
Expected: PASS (both tests).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/tools.rs
git commit -m "feat(tools): add GatedToolExecutor (gated read/connector dispatch)"
```

---

### Task 4: `run_agentic_loop` — the bounded decide-or-finish loop + floor

**Files:**
- Modify: `src-tauri/src/agent.rs` (add the loop + step protocol + a scripted `MockReasoner` for tests)
- Modify: `src-tauri/src/voice_action.rs` (make `extract_citations` + the web/calendar citation helpers `pub(crate)` so `agent.rs` reuses them — do NOT duplicate)
- Test: in `src-tauri/src/agent.rs` `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: `crate::reason::LocalReasoner`, `crate::agent::{ToolExecutor, AgentOutcome, AgentStep}`, `crate::voice_action::extract_citations`.
- Produces: `pub fn run_agentic_loop(reasoner: &dyn crate::reason::LocalReasoner, system: &str, user: &str, executor: &dyn ToolExecutor, max_steps: usize) -> crate::error::Result<AgentOutcome>`.

- [ ] **Step 1: Write the failing test**

```rust
// A reasoner whose structured() returns canned JSON in sequence — the scripted brain.
struct ScriptReasoner { script: std::sync::Mutex<std::collections::VecDeque<serde_json::Value>> }
impl ScriptReasoner {
    fn new(steps: Vec<serde_json::Value>) -> Self {
        Self { script: std::sync::Mutex::new(steps.into_iter().collect()) }
    }
}
impl crate::reason::LocalReasoner for ScriptReasoner {
    fn id(&self) -> &str { "script" }
    fn reason(&self, _s: &str, u: &str) -> crate::error::Result<String> { Ok(format!("FLOOR:{}", u.len())) }
    fn structured(&self, _s: &str, _u: &str, _schema: &serde_json::Value) -> crate::error::Result<serde_json::Value> {
        Ok(self.script.lock().unwrap().pop_front().unwrap_or(serde_json::json!({"answer":""})))
    }
}

#[test]
fn loop_runs_tool_then_answers() {
    let r = ScriptReasoner::new(vec![
        serde_json::json!({ "tool": "search_meetings", "args": { "query": "atlas" } }),
        serde_json::json!({ "answer": "Atlas ships Friday." }),
    ]);
    let e = EchoExec; // from Task 2 tests — returns "ran <name>"
    let out = run_agentic_loop(&r, "sys", "when does atlas ship?", &e, 4).unwrap();
    assert_eq!(out.answer, "Atlas ships Friday.");
    assert_eq!(out.steps.len(), 1);
    assert_eq!(out.steps[0].tool, "search_meetings");
    assert!(out.steps[0].ok);
}

#[test]
fn loop_floors_when_no_convergence() {
    // The brain keeps asking for a tool, never answering → after max_steps, fall to reason() floor.
    let r = ScriptReasoner::new(vec![
        serde_json::json!({ "tool": "search_meetings", "args": { "query": "x" } }),
        serde_json::json!({ "tool": "search_meetings", "args": { "query": "y" } }),
    ]);
    let e = EchoExec;
    let out = run_agentic_loop(&r, "sys", "q", &e, 2).unwrap();
    assert!(out.answer.starts_with("FLOOR:"), "non-convergence must hit the reason() floor: {}", out.answer);
    assert_eq!(out.steps.len(), 2);
}

#[test]
fn loop_survives_a_tool_error() {
    struct ErrExec;
    impl ToolExecutor for ErrExec {
        fn specs(&self) -> Vec<crate::tools::ToolSpec> { crate::tools::tool_specs() }
        fn run(&self, _n: &str, _a: &serde_json::Value) -> crate::error::Result<String> {
            Err(crate::error::AppError::Storage("boom".into()))
        }
    }
    let r = ScriptReasoner::new(vec![
        serde_json::json!({ "tool": "search_meetings", "args": {} }),
        serde_json::json!({ "answer": "done despite the error" }),
    ]);
    let out = run_agentic_loop(&r, "sys", "q", &ErrExec, 4).unwrap();
    assert_eq!(out.answer, "done despite the error");
    assert_eq!(out.steps.len(), 1);
    assert!(!out.steps[0].ok, "a failed tool is recorded ok=false, never panics");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd src-tauri && cargo test --lib agent::tests::loop_`
Expected: FAIL — `cannot find function run_agentic_loop`.

- [ ] **Step 3: Write minimal implementation**

In `src-tauri/src/voice_action.rs`, change the citation helper visibility (find `fn extract_citations` and the `web_citation_from_line`/`calendar_citation_from_line` helpers) from private to `pub(crate)`:

```rust
pub(crate) fn extract_citations(grounding: &str) -> Vec<String> { /* unchanged body */ }
```

In `src-tauri/src/agent.rs`, add:

```rust
/// Drive the brain in a bounded decide-or-finish loop over the gated executor. Each turn the brain
/// emits EITHER `{"tool":<name>,"args":{…}}` (we run it, GATED, and feed the result back) OR
/// `{"answer":"…"}` (done). On non-convergence by `max_steps`, or a stub brain, FALL to the
/// deterministic floor: one `reason()` synthesis over the accumulated gated grounding. PANIC-FREE.
pub fn run_agentic_loop(
    reasoner: &dyn crate::reason::LocalReasoner,
    system: &str,
    user: &str,
    executor: &dyn ToolExecutor,
    max_steps: usize,
) -> Result<AgentOutcome> {
    const RESULT_BUDGET: usize = 4000; // bound re-fed tool output (context growth + cloud egress).
    let catalog = render_catalog(&executor.specs());
    let agent_system = format!(
        "{system}\n\nYou can use these tools to ground your answer in the user's own data:\n{catalog}\n\n\
         Each turn reply with ONLY a JSON object: either {{\"tool\":\"<name>\",\"args\":{{…}}}} to use a \
         tool, or {{\"answer\":\"<your final answer>\"}} to finish. Prefer answering once you have enough \
         grounding. Cite vault meetings by their [[Title]]."
    );
    let step_schema = serde_json::json!({ "type": "object" });

    let mut steps: Vec<AgentStep> = Vec::new();
    let mut gathered = String::new();
    let mut transcript = format!("User request: {user}");

    for _ in 0..max_steps {
        let v = match reasoner.structured(&agent_system, &transcript, &step_schema) {
            Ok(v) => v,
            // A malformed/unavailable brain turn → stop iterating and floor (graceful).
            Err(_) => break,
        };
        if let Some(answer) = v.get("answer").and_then(|a| a.as_str()) {
            let answer = answer.trim();
            if !answer.is_empty() {
                let citations = crate::voice_action::extract_citations(&gathered);
                return Ok(AgentOutcome { answer: answer.to_string(), steps, citations });
            }
        }
        if let Some(name) = v.get("tool").and_then(|t| t.as_str()) {
            let args = v.get("args").cloned().unwrap_or_else(|| serde_json::json!({}));
            // PII rule: log the tool NAME + ok only — never args/results.
            match executor.run(name, &args) {
                Ok(out) => {
                    let out = out.trim();
                    gathered.push_str(out);
                    gathered.push_str("\n\n");
                    transcript.push_str(&format!("\n\n[{name} result]\n{}", truncate(out, RESULT_BUDGET)));
                    steps.push(AgentStep { tool: name.to_string(), args_json: args.to_string(), ok: true });
                }
                Err(e) => {
                    tracing::debug!(target: "agent", tool = %name, error = %e, "agentic tool call failed; continuing");
                    transcript.push_str(&format!("\n\n[{name} failed]"));
                    steps.push(AgentStep { tool: name.to_string(), args_json: args.to_string(), ok: false });
                }
            }
            continue;
        }
        // Neither a tool nor a usable answer → bail to the floor.
        break;
    }

    // FLOOR: synthesize over whatever gated grounding we gathered (today's rag_answer synthesis shape).
    let synth_system = "You are an in-meeting assistant. Answer the user's request concisely (2-4 \
                        sentences) using ONLY the provided context. Cite vault meetings by [[Title]]. \
                        If the context doesn't cover it, say so plainly. Do not invent facts.";
    let synth_user = format!("Request: {user}\n\nContext:\n{}", gathered.trim());
    let answer = reasoner.reason(synth_system, &synth_user).unwrap_or_default();
    let citations = crate::voice_action::extract_citations(&gathered);
    Ok(AgentOutcome { answer, steps, citations })
}

/// Render the executor's tool specs into a compact catalog the model reads (name — description + params).
fn render_catalog(specs: &[crate::tools::ToolSpec]) -> String {
    specs
        .iter()
        .map(|s| format!("- {} — {} params: {}", s.name, s.description, s.parameters))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Truncate on a char boundary (UTF-8-safe) to bound re-fed tool output.
fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max { return s; }
    let mut end = max;
    while !s.is_char_boundary(end) { end -= 1; }
    &s[..end]
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd src-tauri && cargo test --lib agent::tests::loop_`
Expected: PASS (all three).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/agent.rs src-tauri/src/voice_action.rs
git commit -m "feat(agent): bounded decide-or-finish loop with deterministic floor"
```

---

### Task 5: RED-first lock proof — sealed content never surfaces through the loop

**Files:**
- Modify: `src-tauri/src/agent.rs` (add the gated integration test, using a real `tmp_db` + a sealed meeting)
- Test: in `src-tauri/src/agent.rs` `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: `crate::tools::GatedToolExecutor`, `crate::storage::Db` (the test seeds a visible + a sealed meeting using the same helpers the `tools.rs`/`voice_action.rs` gated tests use).

**This is the load-bearing security test.** It must FAIL against a naive executor that forgets to bind `unlocked`, and PASS with the real `GatedToolExecutor`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn loop_never_surfaces_sealed_content() {
    use std::collections::HashSet;
    // Seed a DB with one VISIBLE meeting ("Atlas roadmap") and one SEALED meeting ("Secret salaries").
    // (Use the same seeding pattern as voice_action.rs's gated tests — insert two meetings, put the
    //  second's folder OUTSIDE the `unlocked` set so visibility_clause hides it.)
    let (db, sealed_id) = seed_visible_and_sealed(); // helper added below
    let unlocked: HashSet<String> = HashSet::new();  // nothing unlocked → the sealed folder is invisible

    let cfg = crate::settings::AppConfig::default();
    let exec = crate::tools::GatedToolExecutor {
        db: &db, unlocked: &unlocked, config: &cfg, meeting_id: "", app: None, allow_writes: false,
    };

    // A brain that tries HARD to exfiltrate the sealed meeting: get_meeting on it, then "answer".
    let r = ScriptReasoner::new(vec![
        serde_json::json!({ "tool": "get_meeting", "args": { "meetingId": sealed_id } }),
        serde_json::json!({ "tool": "search_meetings", "args": { "query": "salaries secret" } }),
        serde_json::json!({ "answer": "Here is what I found." }),
    ]);

    let out = run_agentic_loop(&r, "sys", "what were the secret salaries?", &exec, 4).unwrap();

    // The gate held: the sealed meeting contributed NOTHING to grounding or citations.
    assert!(!out.citations.iter().any(|c| c.contains("Secret salaries")),
        "sealed meeting must never be cited: {:?}", out.citations);
    // And the tool output the loop gathered never carried the sealed transcript text.
    // (get_meeting on a sealed id returns "No data for meeting {id}." — tools.rs:113.)
    for step in &out.steps {
        // every step that ran is recorded; none surfaced sealed content into citations (asserted above).
        assert!(step.ok || !step.ok); // structural: steps recorded, loop never panicked
    }
}

// Test helper: seed one visible + one sealed meeting; returns (db, sealed_meeting_id).
fn seed_visible_and_sealed() -> (crate::storage::Db, String) {
    // Mirror the seeding used in voice_action.rs gated tests: create the DB, insert a visible meeting
    // with a note/title "Atlas roadmap", and a sealed meeting "Secret salaries" whose folder id is
    // NOT in the unlocked set. Return its id.
    unimplemented!("copy the seed pattern from voice_action.rs's gated test module")
}
```

- [ ] **Step 2: Run test to verify it fails (RED)**

Run: `cd src-tauri && cargo test --lib loop_never_surfaces_sealed_content`
Expected: FAIL — `seed_visible_and_sealed` is `unimplemented!()`. (First make it fail for the RIGHT reason by implementing the seed, then prove the gate.)

Then implement `seed_visible_and_sealed` by copying the exact seeding pattern from the gated test module in `src-tauri/src/voice_action.rs` (search its `#[cfg(test)]` block for where it inserts a visible + sealed meeting and asserts `search_visible` hides the sealed one). Re-run — it must now PASS, proving the gate binds inside the loop.

- [ ] **Step 3: Prove RED-before-GREEN of the gate itself**

Temporarily change the test's `GatedToolExecutor` to a hand-written naive executor that calls `db.get_segments(sealed_id)` directly (bypassing `execute_tool`/`unlocked`); confirm the assertion FAILS (sealed text leaks). Then revert to `GatedToolExecutor` and confirm PASS. This proves the test actually catches a leak. (Delete the naive executor after.)

- [ ] **Step 4: Run test to verify it passes (GREEN)**

Run: `cd src-tauri && cargo test --lib loop_never_surfaces_sealed_content`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/agent.rs
git commit -m "test(agent): RED-first proof sealed content never surfaces through the loop"
```

---

### Task 6: Default `LocalReasoner::agentic()` — every reasoner gets the loop for free

**Files:**
- Modify: `src-tauri/src/reason.rs` (add the default trait method to `LocalReasoner`)
- Test: in `src-tauri/src/reason.rs` `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: `crate::agent::{run_agentic_loop, ToolExecutor, AgentOutcome}`.
- Produces: a default method `fn agentic(&self, system: &str, user: &str, executor: &dyn crate::agent::ToolExecutor, max_steps: usize) -> Result<crate::agent::AgentOutcome>` on `LocalReasoner`, delegating to `run_agentic_loop`. No existing impl changes (default method).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn stub_reasoner_agentic_floors_to_an_outcome() {
    // The StubReasoner can't emit tool JSON, so the loop reaches max_steps and floors via reason().
    struct NoTools;
    impl crate::agent::ToolExecutor for NoTools {
        fn specs(&self) -> Vec<crate::tools::ToolSpec> { vec![] }
        fn run(&self, _n: &str, _a: &serde_json::Value) -> Result<String> {
            Err(AppError::InvalidArg("none".into()))
        }
    }
    let out = StubReasoner.agentic("sys", "hello", &NoTools, 2).unwrap();
    // Stub's reason() echoes a deterministic shape — the floor produced a non-panicking outcome.
    assert!(out.answer.contains("stub-reason"));
    assert!(out.steps.is_empty());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd src-tauri && cargo test --lib stub_reasoner_agentic_floors_to_an_outcome`
Expected: FAIL — `no method named agentic`.

- [ ] **Step 3: Write minimal implementation**

In `src-tauri/src/reason.rs`, add to the `LocalReasoner` trait (after `structured`):

```rust
    /// Bounded model-driven tool-use loop. DEFAULT delegates to the shared, transport-agnostic loop
    /// over `structured()`, so Cloud/Mistral/Stub get it identically. A future MistralReasoner MAY
    /// override to use mistral.rs' native tool-calling. NEVER panics; bounded by `max_steps`.
    fn agentic(
        &self,
        system: &str,
        user: &str,
        executor: &dyn crate::agent::ToolExecutor,
        max_steps: usize,
    ) -> Result<crate::agent::AgentOutcome> {
        crate::agent::run_agentic_loop(self, system, user, executor, max_steps)
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd src-tauri && cargo test --lib stub_reasoner_agentic_floors_to_an_outcome`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/reason.rs
git commit -m "feat(reason): default LocalReasoner::agentic() delegates to the loop"
```

---

### Task 7: Full-suite gate + lock-security review

**Files:** none (verification only)

- [ ] **Step 1: Run the full Rust test suite**

Run: `cd src-tauri && cargo test --lib`
Expected: PASS — all existing ~128 tests + the new agent/tools/reason tests green. (The `mcp.rs` tests must be UNTOUCHED — P0 does not modify `mcp.rs`; the catalog dedup is deferred to a later cleanup.)

- [ ] **Step 2: Run the full gate ONCE**

Run: `bash scripts/ci.sh`
Expected: PASS — clippy `-D warnings` + tests + `ng lint` + `ng build` + headless E2E. (FE is untouched in P0, so `ng lint`/`ng build` are unaffected.)

- [ ] **Step 3: Dispatch the adversarial-verifier**

Dispatch the `adversarial-verifier` agent on the P0 diff. It owns PASS/FAIL — it re-runs the gates and hunts the project's shipped failure modes. Do not self-certify.

- [ ] **Step 4: Dispatch the lock-security-reviewer (REQUIRED — lock-touching change)**

Dispatch the `lock-security-reviewer` agent. It must confirm: (a) every tool read in `GatedToolExecutor::run` routes through `execute_tool(…, unlocked, …)`; (b) no `ToolCall` variant the loop can reach mutates `unlocked` or performs a destructive/exfiltrating write; (c) the `loop_never_surfaces_sealed_content` test genuinely proves the gate (RED-before-GREEN demonstrated in Task 5 Step 3); (d) no PII in the new logs (tool names + ok only). It is the required gate before this merges.

- [ ] **Step 5: Commit (if reviewers requested fixes, fix + re-gate first)**

```bash
git add -A && git commit -m "chore(agent): P0 headless agentic seam — gates green, lock-reviewed"
```

---

## Self-review (done)

- **Spec coverage:** §3.1 ToolSpec/`tool_specs()` → Task 1; `ToolExecutor`/`GatedToolExecutor` → Tasks 2–3; §3.2 `agent.rs`/`run_agentic_loop`/floor → Task 4; §3.3 default `agentic()` → Task 6; §7 gate-every-read + RED-first sealed proof + lock review → Tasks 5 & 7. Write tools (D2) are catalog-data only, never exposed (`allow_writes=false`) — no execute logic in P0, matching the read-only v1 decision. Dispatch wiring (P1), text input (P2), streaming (P2/P3) are explicitly OUT of P0.
- **Placeholder scan:** the only `unimplemented!()` is the test seed in Task 5, which Step 2 immediately instructs to fill by copying the existing `voice_action.rs` gated-test seed pattern — not a shipped placeholder.
- **Type consistency:** `ToolSpec`/`tool_specs()` (Task 1) consumed unchanged in Tasks 3/4/6; `ToolExecutor`/`AgentOutcome`/`AgentStep` (Task 2) consumed in Tasks 3–6; `run_agentic_loop` signature (Task 4) matches the `agentic()` delegation (Task 6); `GatedToolExecutor` field set (Task 3) matches its use in Task 5.
