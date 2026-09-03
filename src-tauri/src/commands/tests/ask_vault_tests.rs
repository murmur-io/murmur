use super::*;
use crate::agent::ToolExecutor;
use crate::reason::LocalReasoner;
use crate::storage::models::{Folder, Meeting, MeetingStatus, NoteRecord};
use crate::storage::Db;
use serde_json::Value;
use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex};

const TEST_DEK: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn tmp_db() -> Db {
    let p = crate::storage::db::unique_temp_path("murmur-askvault", "sqlite");
    Db::open_with_key(&p, TEST_DEK).unwrap()
}

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(f)
}

fn seed_note(db: &Db, id: &str, title: &str, markdown: &str, folder: Option<&str>) {
    db.insert_meeting(&Meeting {
        id: id.into(),
        started_at: "2026-06-26T09:00:00Z".into(),
        ended_at: None,
        title: Some(title.into()),
        duration_s: 60,
        audio_path: None,
        status: MeetingStatus::Summarized,
        folder_id: None,
    })
    .unwrap();
    db.upsert_note(&NoteRecord {
        meeting_id: id.into(),
        provider_id: "claude_code".into(),
        markdown: markdown.into(),
        created_at: "2026-06-26T09:05:00Z".into(),
        exported_path: None,
        model_requested: None,
        model_served: None,
        gateway_host: None,
    })
    .unwrap();
    db.set_note_folder(id, folder).unwrap();
}

/// A LOCKED folder + a blanked-note meeting inside it — the sealed-and-not-unlocked at-rest
/// shape (title still indexed; the visibility gate is what must hide it).
fn seed_sealed(db: &Db, meeting_id: &str, folder_id: &str, title: &str) {
    db.insert_folder(&Folder {
        id: folder_id.into(),
        name: "Secret".into(),
        path: "Secret".into(),
        parent_id: None,
        locked: true,
        created_at: "2026-06-26T00:00:00Z".into(),
    })
    .unwrap();
    seed_note(db, meeting_id, title, "", Some(folder_id));
}

struct PromptSpyProvider {
    calls: std::sync::atomic::AtomicUsize,
    systems: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl crate::summarize::provider::SummarizerProvider for PromptSpyProvider {
    fn id(&self) -> &str {
        "prompt-spy"
    }
    async fn availability(&self) -> crate::summarize::provider::Availability {
        crate::summarize::provider::Availability::Available
    }
    async fn summarize(
        &self,
        _request: &crate::summarize::provider::SummarizeRequest,
    ) -> crate::error::Result<String> {
        Ok(String::new())
    }
    async fn complete(&self, system: &str, _user: &str) -> crate::error::Result<String> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.systems.lock().unwrap().push(system.to_string());
        Ok("answer".into())
    }
}

#[test]
fn shipped_dashboard_provider_sink_omits_global_memory_and_mutation_refuses_before_call() {
    let db = tmp_db();
    seed_note(&db, "m1", "Board note", "BOARD MATERIAL", None);
    db.insert_dashboard("b1", "Board", None, None, "2026-08-01T09:00:00Z")
        .unwrap();
    db.insert_dashboard_tile(
        "t1",
        "b1",
        "meeting",
        Some("m1"),
        None,
        4,
        None,
        "2026-08-01T09:00:01Z",
    )
    .unwrap();
    let context = crate::commands::dashboards::dashboard_composite_context(
        &db,
        "b1",
        &HashSet::new(),
        200_000,
        &[],
        None,
    )
    .unwrap();
    // The global memory block must not exist at all on the shipped prepacked board provider seam.
    let spy = std::sync::Arc::new(PromptSpyProvider {
        calls: std::sync::atomic::AtomicUsize::new(0),
        systems: Mutex::new(Vec::new()),
    });
    let admission = crate::state::ContentDispatchAdmission::for_test(
        std::sync::Arc::new(Mutex::new(())),
        || Ok(()),
    );
    let out = block_on(ask_vault_prepacked_dashboard_dispatch(
        &context,
        "question",
        &[],
        admission,
        {
            let spy = spy.clone();
            move || {
                Ok(spy
                    as std::sync::Arc<dyn crate::summarize::provider::SummarizerProvider>)
            }
        },
    ))
    .unwrap();
    assert_eq!(out.answer, "answer");
    let system = spy.systems.lock().unwrap().join("\n");
    assert!(!system.contains("WHAT YOU KNOW ABOUT THE USER"));

    let stale = context.witness.clone();
    db.delete_dashboard_tile("t1").unwrap();
    let db = std::sync::Arc::new(db);
    let admission =
        crate::state::ContentDispatchAdmission::for_test(std::sync::Arc::new(Mutex::new(())), {
            let db = db.clone();
            move || {
                crate::commands::dashboards::require_dashboard_context_witness(
                    &db,
                    &stale,
                    &HashSet::new(),
                )
            }
        });
    let before = spy.calls.load(std::sync::atomic::Ordering::SeqCst);
    let err = block_on(ask_vault_prepacked_dashboard_dispatch(
        &context,
        "question",
        &[],
        admission,
        {
            let spy = spy.clone();
            move || {
                Ok(spy
                    as std::sync::Arc<dyn crate::summarize::provider::SummarizerProvider>)
            }
        },
    ))
    .unwrap_err();
    assert!(matches!(err, AppError::Locked(_)));
    assert_eq!(spy.calls.load(std::sync::atomic::Ordering::SeqCst), before);
}

#[test]
fn stateless_dashboard_history_is_refused_before_any_provider_path() {
    let history = vec![ChatTurn {
        role: "assistant".into(),
        content: "old secret".into(),
    }];
    assert!(require_backend_owned_dashboard_history(false, None, &history).is_ok());
    assert!(matches!(
        require_backend_owned_dashboard_history(false, Some("b1"), &history),
        Err(AppError::Locked(_))
    ));
    assert!(require_backend_owned_dashboard_history(true, Some("b1"), &history).is_ok());
    assert!(require_backend_owned_dashboard_history(false, Some("b1"), &[]).is_ok());
}

/// A reasoner whose `structured()` returns canned JSON in sequence (a test double — the
/// production loop drives the real ReasonerCell dispatch). Exhaustion yields an empty answer,
/// which the loop treats as non-convergence.
struct ScriptReasoner {
    script: Mutex<VecDeque<crate::error::Result<Value>>>,
}
impl ScriptReasoner {
    fn ok(steps: Vec<Value>) -> Self {
        Self {
            script: Mutex::new(steps.into_iter().map(Ok).collect()),
        }
    }
}
impl LocalReasoner for ScriptReasoner {
    fn id(&self) -> &str {
        "script"
    }
    fn reason(&self, _s: &str, _u: &str) -> crate::error::Result<String> {
        Ok("unused".into())
    }
    fn structured(&self, _s: &str, _u: &str, _schema: &Value) -> crate::error::Result<Value> {
        self.script
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Ok(serde_json::json!({ "answer": "" })))
    }
}

/// The VAULT-SCOPED executor exactly as `ask_vault_agentic_attempt` builds it (no meeting,
/// read-only, NO note drafts), minus the AppHandle (headless: connectors unavailable).
fn ask_executor<'a>(
    db: &'a Db,
    unlocked: &'a Mutex<HashSet<String>>,
    cfg: &'a AppConfig,
) -> crate::tools::GatedToolExecutor<'a> {
    crate::tools::GatedToolExecutor {
        db,
        unlocked,
        config: cfg,
        meeting_id: "",
        app: None,
        recording_token: None,
        allow_writes: false,
        note_drafts: false,
        scope: crate::tools::AssistantScope::Full,
        seal: None,
        proposed_note: Mutex::new(None),
    }
}

/// Brain v2 L3 — the flag-ON JIT wiring, bound headless (adversarial finding 2026-07-10: only
/// the flag-OFF byte-identity was tested): the exact glue `ask_vault_agentic_attempt` runs
/// when `ask_jit_retrieval` is ON — `build_meeting_listing_visible` (gated; the FTS/recency
/// path with semantic search off) fed through `ask_vault_loop` → `agentic_system_jit` — must
/// inject the "MEETING LISTING" section WITH the visible meeting's `id | title | date` line
/// into the system prompt the model actually sees. The AppState-driven flag read in
/// `ask_vault_agentic_attempt` itself stays code-read-verified (it needs a running app).
#[test]
fn ask_jit_flag_on_injects_the_gated_listing_into_the_system_prompt() {
    /// Records every SYSTEM prompt handed to the model, then answers immediately.
    struct SystemRecordingReasoner {
        systems: Mutex<Vec<String>>,
    }
    impl LocalReasoner for SystemRecordingReasoner {
        fn id(&self) -> &str {
            "system-recording"
        }
        fn reason(&self, _s: &str, _u: &str) -> crate::error::Result<String> {
            Ok(String::new())
        }
        fn structured(&self, s: &str, _u: &str, _schema: &Value) -> crate::error::Result<Value> {
            self.systems.lock().unwrap().push(s.to_string());
            Ok(serde_json::json!({ "answer": "done" }))
        }
    }

    let db = tmp_db();
    seed_note(
        &db,
        "m1",
        "Atlas Kickoff",
        "We decided to ship atlas on Friday.",
        None,
    );
    let cfg = AppConfig::default();
    let unlocked = Mutex::new(HashSet::new());
    let exec = ask_executor(&db, &unlocked, &cfg);

    // The flag-ON branch of `ask_vault_agentic_attempt`, headless shape: semantic search off
    // ⇒ empty query vector ⇒ the gated FTS/recency listing.
    let listing = crate::summarize::vault_context::build_meeting_listing_visible(
        &db,
        "atlas",
        &[],
        30,
        &HashSet::new(),
    )
    .unwrap();
    assert!(
        listing.contains("Atlas Kickoff"),
        "seed self-check: the visible meeting must be listed: {listing}"
    );

    let r = SystemRecordingReasoner {
        systems: Mutex::new(Vec::new()),
    };
    let out = ask_vault_loop(
        &r,
        &exec,
        &db,
        &unlocked,
        "atlas?",
        &[],
        "",
        &listing,
        false,
        None,
        crate::reason::GenOptions::ask_answer(),
    )
    .unwrap()
    .expect("scripted brain converged");
    assert_eq!(out.answer, "done");
    let systems = r.systems.lock().unwrap();
    let sys = systems.first().expect("at least one model turn");
    assert!(
        sys.contains("MEETING LISTING"),
        "flag-ON must inject the JIT listing section into the system prompt"
    );
    assert!(
        sys.contains("Atlas Kickoff") && sys.contains("m1"),
        "the visible meeting's `id | title | date` line must reach the model"
    );
}

/// RED-first floor equivalence (the binding test of "the floor is today's behavior"): the
/// extracted floor prompt must be BYTE-IDENTICAL to the pre-change statement sequence —
/// `build_vault_context_visible` → `vault_chat::build` — for the same inputs, and the
/// empty-corpus early return must keep the exact canned string. RED-proven: perturbing the
/// floor (e.g. swapping the corpus builder for the fail-closed shim, or reordering sections)
/// fails the byte equality here.
#[test]
fn ask_floor_prompt_matches_pre_change_implementation() {
    let db = tmp_db();
    seed_note(
        &db,
        "m1",
        "Atlas Kickoff",
        "We decided to ship atlas on Friday.",
        None,
    );
    seed_note(&db, "m2", "Weekly Sync", "Anna owns QA for atlas.", None);
    // Tier 1 flipped the semantic default ON; PIN it false so this test keeps EXPLICITLY exercising
    // the FTS-floor branch (its stated purpose) rather than the hybrid branch — which, on an empty
    // vec_chunks, happens to degenerate to the same bytes. Defensive/self-documenting, not a strict
    // regression guard (the hybrid path is byte-identical here even without the pin).
    let cfg = AppConfig {
        semantic_search_enabled: false,
        ..AppConfig::default()
    };
    let unlocked = HashSet::new();
    let history = vec![
        ChatTurn {
            role: "user".into(),
            content: "earlier question".into(),
        },
        ChatTurn {
            role: "assistant".into(),
            content: "earlier answer".into(),
        },
    ];
    let q = "what did we decide about atlas?";

    // The PRE-CHANGE implementation, replicated statement-for-statement.
    let (corpus, want_sources) = crate::summarize::vault_context::build_vault_context_visible(
        &db,
        q,
        &cfg.provider_id,
        &unlocked,
    )
    .unwrap();
    assert!(
        !corpus.trim().is_empty(),
        "fixture must produce a non-empty corpus"
    );
    // Empty memory brief ⇒ the floor prompt must stay BYTE-IDENTICAL to the pre-memory build.
    let (want_system, want_user) = crate::summarize::vault_chat::build(&corpus, &history, q, "");

    match build_ask_vault_floor_prompt(&db, &cfg, &unlocked, q, &history, "", None, None, None)
        .unwrap()
    {
        AskFloorPrompt::Ready {
            system,
            user,
            sources,
        } => {
            assert_eq!(
                system, want_system,
                "floor system prompt diverged from pre-change"
            );
            assert_eq!(
                user, want_user,
                "floor user prompt diverged from pre-change"
            );
            assert_eq!(
                sources
                    .iter()
                    .map(|s| s.meeting_id.as_str())
                    .collect::<Vec<_>>(),
                want_sources
                    .iter()
                    .map(|s| s.meeting_id.as_str())
                    .collect::<Vec<_>>(),
                "floor sources diverged from pre-change"
            );
        }
        AskFloorPrompt::Empty(_) => panic!("a non-empty corpus must yield Ready"),
    }

    // The empty-vault early return keeps the EXACT pre-change canned answer.
    let empty = tmp_db();
    match build_ask_vault_floor_prompt(&empty, &cfg, &unlocked, q, &[], "", None, None, None)
        .unwrap()
    {
        AskFloorPrompt::Empty(r) => {
            assert_eq!(
                r.answer,
                "No meeting notes to search yet — record and summarize a meeting first."
            );
            assert!(r.sources.is_empty() && r.citations.is_empty());
        }
        AskFloorPrompt::Ready { .. } => panic!("an empty vault must yield the canned Empty"),
    }
}

/// The floor's ERROR/CONSENT semantics are untouched: an unconsented cloud provider is refused
/// by `make_provider`'s fail-closed gate with `AppError::Unavailable` — exactly the pre-change
/// behavior the FE consent flow keys on.
#[test]
fn ask_floor_preserves_no_consent_error_semantics() {
    let db = tmp_db();
    seed_note(
        &db,
        "m1",
        "Atlas Kickoff",
        "We decided to ship atlas on Friday.",
        None,
    );
    let cfg = AppConfig {
        provider_id: "anthropic".into(),
        ..AppConfig::default()
    };
    assert!(
        !cfg.cloud_egress_consented,
        "fresh config defaults to consent OFF"
    );
    let db = std::sync::Arc::new(db);
    let res = block_on(ask_vault_floor(
        &db,
        &cfg,
        &HashSet::new(),
        "atlas?",
        &[],
        "",
        None,
        &std::sync::Arc::new(tokio::sync::Semaphore::new(1)),
        None,
        None,
    ));
    assert!(
        matches!(res, Err(AppError::Unavailable(_))),
        "no-consent floor must keep the Unavailable refusal: {res:?}"
    );
}

/// LOCAL_LOOPBACK authorization oracle for the exact two Ask paths that consume
/// `vault_chat::render_conversation`: a loopback Ollama floor excludes sealed corpus rows and a
/// stale visibility admission prevents the provider future from being constructed; the agentic
/// loop carries the same admission through `DurableDispatchReasoner`, invokes the real
/// provider-backed reasoner, and rejects before `provider.complete` can open a connection. This
/// binds the history-budget change to the unchanged dispatch gate instead of treating loopback as
/// implicitly authorized. It is the mandatory LOCAL_LOOPBACK evidence in acceptance item 6, not
/// an assertion that production dispatch behavior changed.
#[test]
fn loopback_ollama_ask_paths_revalidate_visibility_before_provider_dispatch() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let ollama_base_url = format!("http://{}", listener.local_addr().unwrap());
    let loopback_connections = std::sync::Arc::new(AtomicUsize::new(0));
    let loopback_connections_for_server = std::sync::Arc::clone(&loopback_connections);
    let stop_server = std::sync::Arc::new(AtomicBool::new(false));
    let stop_server_worker = std::sync::Arc::clone(&stop_server);
    let server = std::thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !stop_server_worker.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    use std::io::{Read, Write};

                    loopback_connections_for_server.fetch_add(1, Ordering::SeqCst);
                    stream
                        .set_read_timeout(Some(std::time::Duration::from_secs(1)))
                        .unwrap();
                    let mut request = [0_u8; 4096];
                    let _ = stream.read(&mut request);
                    let body = br#"{"response":"unexpected dispatch","done":true}"#;
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes());
                    let _ = stream.write_all(body);
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(error) => panic!("unexpected listener error: {error}"),
            }
        }
    });
    let db = tmp_db();
    seed_note(&db, "open1", "Atlas Kickoff", "OPEN-ATLAS-CONTEXT", None);
    db.insert_folder(&Folder {
        id: "sealed-folder".into(),
        name: "Sealed".into(),
        path: "Sealed".into(),
        parent_id: None,
        locked: true,
        created_at: "2026-06-26T00:00:00Z".into(),
    })
    .unwrap();
    // Keep plaintext in this adversarial fixture so the assertion proves the read predicate, not
    // merely the normal at-rest blanking invariant.
    seed_note(
        &db,
        "sealed1",
        "Atlas Secret",
        "SEALED-PLAINTEXT-MUST-NOT-REACH-OLLAMA",
        Some("sealed-folder"),
    );

    let cfg = AppConfig {
        provider_id: crate::summarize::PROVIDER_OLLAMA.into(),
        role_ask_connection: crate::summarize::PROVIDER_OLLAMA.into(),
        ollama_base_url,
        semantic_search_enabled: false,
        ..AppConfig::default()
    };
    assert!(
        !crate::summarize::egress_is_cloud(crate::summarize::PROVIDER_OLLAMA, &cfg),
        "fixture must exercise the LOCAL_LOOPBACK row"
    );

    let unlocked = HashSet::new();
    let history = [ChatTurn {
        role: "user".into(),
        content: "PRIOR-HISTORY-WOULD-REACH-THE-PROVIDER".into(),
    }];
    let prompt = build_ask_vault_floor_prompt(
        &db, &cfg, &unlocked, "atlas", &history, "", None, None, None,
    )
    .unwrap();
    let AskFloorPrompt::Ready { system, user, .. } = prompt else {
        panic!("visible fixture must produce a provider-bound floor prompt");
    };
    assert!(system.contains("OPEN-ATLAS-CONTEXT"));
    assert!(!system.contains("SEALED-PLAINTEXT-MUST-NOT-REACH-OLLAMA"));
    assert!(user.contains("PRIOR-HISTORY-WOULD-REACH-THE-PROVIDER"));

    let floor_validations = std::sync::Arc::new(AtomicUsize::new(0));
    let floor_validation_spy = std::sync::Arc::clone(&floor_validations);
    let floor_admission = crate::state::ContentDispatchAdmission::for_test(
        std::sync::Arc::new(Mutex::new(())),
        move || {
            floor_validation_spy.fetch_add(1, Ordering::SeqCst);
            Err(AppError::Locked(
                "relocked before loopback floor dispatch".into(),
            ))
        },
    );
    let floor_result = block_on(ask_vault_floor_authorized(
        &std::sync::Arc::new(db),
        &cfg,
        &unlocked,
        "atlas",
        &history,
        "",
        None,
        &std::sync::Arc::new(tokio::sync::Semaphore::new(1)),
        None,
        None,
        floor_admission,
    ));
    assert!(matches!(floor_result, Err(AppError::Locked(_))));
    assert_eq!(floor_validations.load(Ordering::SeqCst), 1);

    let resolved = crate::summarize::roles::resolve(crate::summarize::roles::Role::Ask, &cfg);
    assert_eq!(resolved.connection, crate::summarize::PROVIDER_OLLAMA);
    assert!(
        !resolved.is_reasoner_only(),
        "loopback Ollama must exercise the real agentic provider route"
    );
    let agentic = crate::reason::CloudReasoner::for_role(
        std::sync::Arc::new(Mutex::new(cfg.clone())),
        crate::summarize::roles::Role::Ask,
        std::sync::Arc::new(tokio::sync::Semaphore::new(1)),
    );
    assert_eq!(agentic.id(), "cloud:ollama");
    let agentic_validations = std::sync::Arc::new(AtomicUsize::new(0));
    let agentic_validation_spy = std::sync::Arc::clone(&agentic_validations);
    let agentic_admission = crate::state::ContentDispatchAdmission::for_test(
        std::sync::Arc::new(Mutex::new(())),
        move || {
            agentic_validation_spy.fetch_add(1, Ordering::SeqCst);
            Err(AppError::Locked(
                "relocked before loopback agentic dispatch".into(),
            ))
        },
    );
    let guarded_agentic = durable_dispatch_reasoner(&agentic, agentic_admission);
    let db = tmp_db();
    let unlocked = Mutex::new(HashSet::new());
    let executor = ask_executor(&db, &unlocked, &cfg);
    let agentic_result = ask_vault_loop(
        &guarded_agentic,
        &executor,
        &db,
        &unlocked,
        "atlas",
        &history,
        "",
        "",
        false,
        None,
        crate::reason::GenOptions::ask_answer(),
    );
    assert!(matches!(agentic_result, Err(AppError::Locked(_))));
    assert_eq!(agentic_validations.load(Ordering::SeqCst), 1);
    stop_server.store(true, Ordering::SeqCst);
    server.join().unwrap();
    assert_eq!(
        loopback_connections.load(Ordering::SeqCst),
        0,
        "visibility rejection must prevent every loopback Ollama connection"
    );
}

// Keep this cloud-factory oracle beside the base branch's LOCAL_LOOPBACK dispatch oracle above:
// together they bind both sides of Ask's provider classification after the history-budget merge.
/// The public Ask command resolves BOTH of its model branches from the Ask role: the agentic
/// attempt obtains `ReasonerCell::current_for(Role::Ask)`, while its deterministic floor obtains
/// `provider_for(Role::Ask)`. Exercise those production factories side-by-side and prove that
/// every remotely-capable target refuses before transport construction when consent is absent.
#[test]
fn ask_agentic_and_floor_factories_share_the_fail_closed_consent_gate() {
    let cases = [
        ("codex_cli", "http://localhost:11434"),
        ("ollama", "https://ollama.remote.example/api"),
        ("future_remote_provider", "http://localhost:11434"),
    ];

    for (connection, ollama_base_url) in cases {
        let cfg = AppConfig {
            brain_backend: crate::settings::config::BrainBackend::Cloud,
            role_ask_connection: connection.to_string(),
            ollama_base_url: ollama_base_url.to_string(),
            cloud_egress_consented: false,
            ..AppConfig::default()
        };
        let shared_cfg = std::sync::Arc::new(Mutex::new(cfg.clone()));
        let heavy = std::sync::Arc::new(tokio::sync::Semaphore::new(1));

        let agentic = crate::reason::ReasonerCell::new(
            std::sync::Arc::clone(&shared_cfg),
            std::sync::Arc::clone(&heavy),
        )
        .current_for(crate::summarize::roles::Role::Ask)
        .reason("Ask system", "private vault question");
        let agentic_message = match agentic {
            Err(AppError::Unavailable(message)) => message,
            other => panic!(
                "Ask agentic factory must refuse unconsented {connection}: {other:?}"
            ),
        };

        let floor = crate::summarize::provider_for(
            crate::summarize::roles::Role::Ask,
            &cfg,
            &heavy,
        );
        let floor_message = match floor {
            Err(AppError::Unavailable(message)) => message,
            Err(other) => panic!(
                "Ask floor factory must refuse unconsented {connection}, got {other:?}"
            ),
            Ok(_) => panic!("Ask floor factory built unconsented {connection}"),
        };
        assert_eq!(
            agentic_message, floor_message,
            "agentic and floor Ask branches must expose the same consent refusal"
        );
    }
}

#[derive(Default)]
struct AskCaptureProvider {
    prompts: Mutex<Vec<(String, String)>>,
}

#[async_trait::async_trait]
impl crate::summarize::provider::SummarizerProvider for AskCaptureProvider {
    fn id(&self) -> &str {
        crate::summarize::PROVIDER_CODEX_CLI
    }

    async fn availability(&self) -> crate::summarize::provider::Availability {
        crate::summarize::provider::Availability::Available
    }

    async fn summarize(
        &self,
        request: &crate::summarize::provider::SummarizeRequest,
    ) -> crate::error::Result<String> {
        Ok(request.transcript.clone())
    }

    async fn complete(&self, system: &str, user: &str) -> crate::error::Result<String> {
        self.prompts
            .lock()
            .unwrap()
            .push((system.to_string(), user.to_string()));
        Ok(format!("{system}\n{user}"))
    }
}

#[derive(Default)]
struct AskCaptureEgressSink {
    entries: Mutex<Vec<crate::summarize::egress_log::EgressEntry>>,
}

impl crate::summarize::egress_log::EgressSink for AskCaptureEgressSink {
    fn record(&self, entry: crate::summarize::egress_log::EgressEntry) {
        self.entries.lock().unwrap().push(entry);
    }
}

/// Once Ask consent is granted, its floor still has no alternate transport seam: role resolution,
/// redaction, dispatch, response restoration, and the content-free ledger are all exercised through
/// `provider_for_with_test_egress_sink(Role::Ask)` without a CLI, AppHandle, or network socket.
#[test]
fn ask_role_factory_redacts_before_dispatch_and_records_content_free_ledger() {
    let inner = std::sync::Arc::new(AskCaptureProvider::default());
    let sink = std::sync::Arc::new(AskCaptureEgressSink::default());
    let cfg = AppConfig {
        role_ask_connection: crate::summarize::PROVIDER_CODEX_CLI.to_string(),
        role_ask_model: "gpt-ask-test".to_string(),
        cloud_egress_consented: true,
        ..AppConfig::default()
    };
    let provider = crate::summarize::provider_for_with_test_egress_sink(
        crate::summarize::roles::Role::Ask,
        &cfg,
        &std::sync::Arc::new(tokio::sync::Semaphore::new(1)),
        sink.clone(),
        inner.clone(),
    )
    .expect("the Ask role must use the canonical consent/redaction/ledger factory");

    let output = block_on(provider.complete(
        "Answer only from the provided vault context.",
        "Contact alice@example.com about the launch.",
    ))
    .unwrap();
    assert!(
        output.contains("alice@example.com"),
        "the local response restores the redacted value"
    );

    let prompts = inner.prompts.lock().unwrap();
    assert_eq!(prompts.len(), 1);
    let dispatched = format!("{}\n{}", prompts[0].0, prompts[0].1);
    assert!(!dispatched.contains("alice@example.com"));
    assert!(dispatched.contains("⟪EMAIL_1⟫"));
    drop(prompts);

    let entries = sink.entries.lock().unwrap();
    assert_eq!(entries.len(), 1);
    let entry = &entries[0];
    assert_eq!(entry.provider_id, crate::summarize::PROVIDER_CODEX_CLI);
    assert_eq!(entry.model_requested, "gpt-ask-test");
    assert_eq!(entry.call_kind, "complete");
    assert_eq!(entry.redactions.email, 1);
    let debug = format!("{entry:?}");
    assert!(!debug.contains("alice@example.com"));
    assert!(!debug.contains("launch"));
}

/// The Cloud loop path: a scripted brain calls a GATED tool, answers, and the tool-derived
/// `[[Title]]` citations flow into the DTO — verbatim in `citations` AND resolved (gated) into
/// the structured `sources` chips.
#[test]
fn ask_loop_tool_citations_flow_into_dto() {
    let db = tmp_db();
    seed_note(
        &db,
        "m1",
        "Atlas Kickoff",
        "## Action items\n- [ ] Anna — ship the deck 2026-07-10\n",
        None,
    );
    let cfg = AppConfig::default();
    let unlocked = Mutex::new(HashSet::new());
    let exec = ask_executor(&db, &unlocked, &cfg);
    let brain = ScriptReasoner::ok(vec![
        serde_json::json!({ "tool": "get_open_commitments", "args": {} }),
        serde_json::json!({ "answer": "Anna ships the deck by 2026-07-10 [[Atlas Kickoff]]." }),
    ]);
    let out = ask_vault_loop(
        &brain,
        &exec,
        &db,
        &unlocked,
        "who owns the deck?",
        &[],
        "",
        "",
        false,
        None,
        crate::reason::GenOptions::ask_answer(),
    )
    .unwrap()
    .expect("scripted brain converged");
    assert_eq!(
        out.answer,
        "Anna ships the deck by 2026-07-10 [[Atlas Kickoff]]."
    );
    assert!(
        out.citations.contains(&"[[Atlas Kickoff]]".to_string()),
        "tool-derived citation must reach the DTO verbatim: {:?}",
        out.citations
    );
    assert_eq!(
        out.sources.len(),
        1,
        "the citation resolves to ONE source chip"
    );
    assert_eq!(out.sources[0].meeting_id, "m1");
    assert_eq!(out.sources[0].title, "Atlas Kickoff");
    assert_eq!(out.sources[0].started_at, "2026-06-26T09:00:00Z");
}

/// ORG_READ production-chain oracle for the stateless Ask surface used by Quick Search.
///
/// This drives the real `ask_vault_loop` and `GatedToolExecutor` with a model that requests
/// `org_brain_search`, then echoes only that gated tool-result block into the answer DTO. The
/// fixtures exercise every gate before the new Angular sink: local membership/admission,
/// per-install context enablement, tombstone exclusion, and dispatch authorization. A failed gate
/// must leave the secret absent from all three DTO plaintext lanes (answer, sources, citations).
/// The admitted case also pins the production tool's hard 20-result bound.
#[test]
fn quick_search_org_ask_chain_is_conjunctively_gated_and_bounded() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct ToolResultEchoReasoner {
        calls: AtomicUsize,
    }
    impl LocalReasoner for ToolResultEchoReasoner {
        fn id(&self) -> &str {
            "org-tool-result-echo"
        }

        fn reason(&self, _s: &str, _u: &str) -> crate::error::Result<String> {
            Ok(String::new())
        }

        fn structured(
            &self,
            _system: &str,
            user: &str,
            _schema: &Value,
        ) -> crate::error::Result<Value> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                return Ok(serde_json::json!({
                    "tool": "org_brain_search",
                    "args": { "query": "project cipher" }
                }));
            }
            let answer = user
                .rsplit_once("[org_brain_search result]\n")
                .map(|(_, result)| result.trim())
                .unwrap_or("No authorized org context.");
            Ok(serde_json::json!({ "answer": answer }))
        }
    }

    fn seed_membership(db: &Db, enabled: bool) {
        db.upsert_org_state(&crate::storage::OrgState {
            org_id: "org-quick-search".into(),
            name: "Quick Search Org".into(),
            role: "member".into(),
            joined_at: "2026-08-13T00:00:00Z".into(),
            consented: true,
            last_seq: 0,
            generation: 1,
            context_enabled: enabled,
        })
        .unwrap();
    }

    fn seed_org_secret(db: &Db, item_id: &str, secret: &str, seq: u64) {
        db.upsert_org_item(
            item_id,
            "org-quick-search",
            seq,
            "colleague",
            &format!("{secret} project cipher"),
            &format!("{secret} project cipher decision"),
            "2026-08-13T09:00:00Z",
            1,
            1,
            &[seq as u8; 32],
            None,
            None,
            Some(&crate::embed::StubEmbedder),
        )
        .unwrap();
    }

    /// Production feed-ingest admission: prepares the bounded index, then commits plaintext and
    /// cursor together only while the org membership still exists. Positive disclosure evidence
    /// must enter through this seam rather than the raw storage upsert used for hostile stale-row
    /// fixtures below.
    fn ingest_authorized_org_secret(db: &Db, item_id: &str, secret: &str, seq: u64) {
        let title = format!("{secret} project cipher");
        let markdown = format!("{secret} project cipher decision");
        let prepared = Db::prepare_org_item_index(
            &title,
            "2026-08-13T09:00:00Z",
            &markdown,
            Some(&crate::embed::StubEmbedder),
        )
        .unwrap();
        assert!(
            db.commit_org_feed_item(
                item_id,
                "org-quick-search",
                seq,
                "colleague",
                &title,
                &markdown,
                "2026-08-13T09:00:00Z",
                1,
                1,
                &[seq as u8; 32],
                None,
                Some("authorized-colleague"),
                &prepared,
            )
            .unwrap(),
            "member-gated feed admission must commit the positive fixture"
        );
    }

    fn run_chain(db: &Db, executor: &dyn crate::agent::ToolExecutor) -> AskVaultResult {
        let unlocked = Mutex::new(HashSet::new());
        ask_vault_loop(
            &ToolResultEchoReasoner {
                calls: AtomicUsize::new(0),
            },
            executor,
            db,
            &unlocked,
            "What did the team decide about project cipher?",
            &[],
            "",
            "",
            true,
            None,
            crate::reason::GenOptions::ask_answer(),
        )
        .unwrap()
        .expect("the scripted Ask chain converges")
    }

    fn assert_secret_absent(result: &AskVaultResult, secret: &str, gate: &str) {
        assert!(
            !result.answer.contains(secret),
            "{gate}: rejected org plaintext reached the Quick Search answer DTO: {}",
            result.answer
        );
        assert!(
            result
                .sources
                .iter()
                .all(|source| !source.title.contains(secret)),
            "{gate}: rejected org plaintext reached a source DTO: {:?}",
            result.sources
        );
        assert!(
            result
                .citations
                .iter()
                .all(|citation| !citation.contains(secret)),
            "{gate}: rejected org plaintext reached a citation DTO: {:?}",
            result.citations
        );
    }

    let cfg = AppConfig {
        semantic_search_enabled: false,
        ..AppConfig::default()
    };

    // Positive control + result bound: the exact executor used by stateless `ask_vault` exposes
    // at most twenty fused org hits to the model and therefore to its answer DTO.
    let live = tmp_db();
    seed_membership(&live, true);
    for n in 1..=25 {
        ingest_authorized_org_secret(
            &live,
            &format!("live-{n}"),
            &format!("ORG_LIVE_{n}"),
            n,
        );
    }
    let live_unlocked = Mutex::new(HashSet::new());
    let live_executor = ask_executor(&live, &live_unlocked, &cfg);
    let live_result = run_chain(&live, &live_executor);
    let rendered_hits = live_result.answer.matches("[org · colleague]").count();
    assert_eq!(
        rendered_hits, 20,
        "the production org Ask result stays hard-bounded"
    );
    assert!(live_result.answer.contains("ORG_LIVE_"));
    assert!(
        live_result.sources.is_empty() && live_result.citations.is_empty(),
        "org provenance stays in the labelled answer; it must not be forged into local-meeting \
         source chips or wikilink citations"
    );

    // No joined `org_state`: even a stale plaintext replica cannot be advertised or read.
    let non_member = tmp_db();
    let membership_secret = "ORG_SECRET_MEMBERSHIP";
    seed_org_secret(&non_member, "membership", membership_secret, 1);
    let non_member_unlocked = Mutex::new(HashSet::new());
    let non_member_executor = ask_executor(&non_member, &non_member_unlocked, &cfg);
    assert_secret_absent(
        &run_chain(&non_member, &non_member_executor),
        membership_secret,
        "membership/admission",
    );

    let disabled = tmp_db();
    seed_membership(&disabled, false);
    let disabled_secret = "ORG_SECRET_CONTEXT_DISABLED";
    seed_org_secret(&disabled, "disabled", disabled_secret, 1);
    let disabled_unlocked = Mutex::new(HashSet::new());
    let disabled_executor = ask_executor(&disabled, &disabled_unlocked, &cfg);
    assert_secret_absent(
        &run_chain(&disabled, &disabled_executor),
        disabled_secret,
        "context-enabled",
    );

    let tombstoned = tmp_db();
    seed_membership(&tombstoned, true);
    let tombstone_secret = "ORG_SECRET_TOMBSTONED";
    seed_org_secret(&tombstoned, "tombstoned", tombstone_secret, 1);
    tombstoned.tombstone_org_item("tombstoned").unwrap();
    let tombstoned_unlocked = Mutex::new(HashSet::new());
    let tombstoned_executor = ask_executor(&tombstoned, &tombstoned_unlocked, &cfg);
    assert_secret_absent(
        &run_chain(&tombstoned, &tombstoned_executor),
        tombstone_secret,
        "tombstone",
    );

    // The durable authorization wrapper is the command's post-snapshot dispatch barrier. A stale
    // capability refuses the local tool before its plaintext enters the loop transcript.
    let unauthorized = tmp_db();
    seed_membership(&unauthorized, true);
    let authorization_secret = "ORG_SECRET_AUTHORIZATION";
    seed_org_secret(&unauthorized, "unauthorized", authorization_secret, 1);
    let unauthorized_unlocked = Mutex::new(HashSet::new());
    let inner = ask_executor(&unauthorized, &unauthorized_unlocked, &cfg);
    let admission =
        crate::state::ContentDispatchAdmission::for_test(Arc::new(Mutex::new(())), || {
            Err(AppError::Locked("stale org authorization".into()))
        });
    let admitted = crate::agent::AdmittedToolExecutor::new(&inner, admission);
    assert_secret_absent(
        &run_chain(&unauthorized, &admitted),
        authorization_secret,
        "dispatch authorization",
    );
}

/// The loop contract at this surface: non-convergence → `Ok(None)` (the command floors); a
/// reasoner error — the no-consent `Unavailable` — PROPAGATES (the command's attempt wrapper
/// converts it to a floor, whose semantics `ask_floor_preserves_no_consent_error_semantics`
/// pins).
#[test]
fn ask_loop_non_convergence_floors_and_errors_propagate() {
    let db = tmp_db();
    let cfg = AppConfig::default();
    let unlocked = Mutex::new(HashSet::new());
    let exec = ask_executor(&db, &unlocked, &cfg);

    // Script exhaustion yields an empty answer → the loop bails without converging.
    let stuck = ScriptReasoner::ok(vec![
        serde_json::json!({ "tool": "search_meetings", "args": { "query": "a" } }),
    ]);
    let out = ask_vault_loop(
        &stuck,
        &exec,
        &db,
        &unlocked,
        "q",
        &[],
        "",
        "",
        false,
        None,
        crate::reason::GenOptions::ask_answer(),
    )
    .unwrap();
    assert!(
        out.is_none(),
        "non-convergence must return Ok(None) for the command to floor"
    );

    struct Refuses;
    impl LocalReasoner for Refuses {
        fn id(&self) -> &str {
            "refuses"
        }
        fn reason(&self, _s: &str, _u: &str) -> crate::error::Result<String> {
            Ok("unused".into())
        }
        fn structured(&self, _s: &str, _u: &str, _schema: &Value) -> crate::error::Result<Value> {
            Err(AppError::Unavailable("no consent".into()))
        }
    }
    let res = ask_vault_loop(
        &Refuses,
        &exec,
        &db,
        &unlocked,
        "q",
        &[],
        "",
        "",
        false,
        None,
        crate::reason::GenOptions::ask_answer(),
    );
    assert!(
        matches!(res, Err(AppError::Unavailable(_))),
        "a loop error must propagate so the attempt wrapper can floor: {res:?}"
    );
}

/// The 12-message history discipline (CHAT_CONTEXT_TURNS) now applies to Ask: a 13th message
/// is dropped from both the capped slice and the rendered conversation. RED vs the pre-change
/// code, which rendered the history uncapped.
#[test]
fn ask_history_cap_enforced() {
    let msgs: Vec<ChatTurn> = (0..13)
        .map(|i| ChatTurn {
            role: "user".into(),
            content: format!("turn-{i}-text"),
        })
        .collect();
    let capped = capped_ask_history(&msgs);
    assert_eq!(capped.len(), CHAT_CONTEXT_TURNS);
    assert_eq!(
        capped[0].content, "turn-1-text",
        "the oldest message beyond the cap is dropped"
    );
    let rendered = crate::summarize::vault_chat::render_conversation(capped, "final question");
    assert!(
        !rendered.contains("turn-0-text"),
        "turn beyond the cap must not render"
    );
    assert!(
        rendered.contains("turn-12-text"),
        "the newest capped turn renders"
    );
    assert!(
        rendered.trim_end().ends_with("Assistant:"),
        "render keeps the completion cue"
    );

    // A short history passes through untouched.
    assert_eq!(capped_ask_history(&msgs[..3]).len(), 3);
}

const ASK_HISTORY_TEST_BUDGET: usize = 16_000;
const ASK_HISTORY_TEST_OMISSION_MARKER: &str =
    "[Earlier Ask history omitted to fit the context budget.]\n";

fn rendered_prior_history<'a>(rendered: &'a str, question: &str) -> &'a str {
    let current_turn = format!("User: {}\nAssistant:", question.trim());
    rendered
        .strip_suffix(&current_turn)
        .expect("the current question and completion cue must remain intact")
}

/// Focused resource/egress regression: a count cap alone does not bound twelve legal,
/// pasted-document-sized turns. The shared renderer must keep a suffix of complete newest turns,
/// disclose that older context was omitted, and leave the separately supplied question intact.
#[test]
fn ask_history_char_budget_bounds_many_large_turns_and_keeps_newest_context() {
    let mut history: Vec<ChatTurn> = (0..11)
        .map(|i| ChatTurn {
            role: if i % 2 == 0 { "user" } else { "assistant" }.into(),
            content: format!("OLDER-TURN-{i}-{}", "ż".repeat(3_000)),
        })
        .collect();
    history.push(ChatTurn {
        role: "assistant".into(),
        content: "NEWEST-COMPLETE-TURN-Ω".into(),
    });
    let question = "CURRENT-QUESTION-zażółć-🙂";
    let rendered =
        crate::summarize::vault_chat::render_conversation(capped_ask_history(&history), question);
    let prior = rendered_prior_history(&rendered, question);

    assert!(
        prior.chars().count() <= ASK_HISTORY_TEST_BUDGET,
        "rendered prior Ask history exceeded the strict budget: {}",
        prior.chars().count()
    );
    assert!(prior.starts_with(ASK_HISTORY_TEST_OMISSION_MARKER));
    assert!(prior.contains("Assistant: NEWEST-COMPLETE-TURN-Ω\n"));
    assert!(
        !prior.contains("OLDER-TURN-0-"),
        "the oldest complete turn must be dropped rather than partially packed"
    );
    assert!(rendered.ends_with("User: CURRENT-QUESTION-zażółć-🙂\nAssistant:"));
}

/// When even the newest prior turn cannot fit whole, preserve an honestly marked, Unicode-safe
/// suffix. This keeps the immediate context without permitting one legal row to bypass the bound.
#[test]
fn ask_history_char_budget_marks_and_safely_suffixes_one_oversized_newest_turn() {
    let history = [ChatTurn {
        role: "assistant".into(),
        content: format!("HEAD-SENTINEL-{}-TAIL-Ω", "ą🙂".repeat(12_000)),
    }];
    let question = "CURRENT-QUESTION-STAYS-WHOLE";
    let rendered = crate::summarize::vault_chat::render_conversation(&history, question);
    let prior = rendered_prior_history(&rendered, question);

    assert!(prior.chars().count() <= ASK_HISTORY_TEST_BUDGET);
    assert!(prior.starts_with(ASK_HISTORY_TEST_OMISSION_MARKER));
    assert!(prior.contains("Assistant: …"));
    assert!(!prior.contains("HEAD-SENTINEL"));
    assert!(prior.contains("-TAIL-Ω"));
    assert!(rendered.ends_with("User: CURRENT-QUESTION-STAYS-WHOLE\nAssistant:"));
}

/// Reserving the long omission marker must not partially truncate a newest turn that fits within
/// the full history budget by itself. Use a shorter honest marker and retain the complete turn.
#[test]
fn ask_history_char_budget_keeps_a_complete_newest_turn_near_the_boundary() {
    // "Assistant: " (11) + content (15,968) + newline (1) = 15,980 characters, leaving
    // enough room for the compact 18-character omission marker but not the long marker.
    let newest_content = "ż".repeat(15_968);
    let history = [
        ChatTurn {
            role: "user".into(),
            content: "older".repeat(30),
        },
        ChatTurn {
            role: "assistant".into(),
            content: newest_content.clone(),
        },
    ];
    let rendered = crate::summarize::vault_chat::render_conversation(&history, "q");
    let prior = rendered_prior_history(&rendered, "q");

    assert_eq!(prior.chars().count(), 15_998);
    assert!(prior.starts_with("[Earlier omitted]\n"));
    assert!(prior.ends_with(&format!("Assistant: {newest_content}\n")));
    assert!(!prior.contains("older"));
}

/// Even with zero scalar headroom, the complete newest content wins over an older turn; a leading
/// ellipsis replaces only the normal separator space so omission remains disclosed within 16,000.
#[test]
fn ask_history_char_budget_keeps_exact_boundary_newest_content_with_older_omission() {
    let newest_content = "🙂".repeat(15_988);
    let history = [
        ChatTurn {
            role: "user".into(),
            content: "older".repeat(30),
        },
        ChatTurn {
            role: "assistant".into(),
            content: newest_content.clone(),
        },
    ];
    let rendered = crate::summarize::vault_chat::render_conversation(&history, "q");
    let prior = rendered_prior_history(&rendered, "q");

    assert_eq!(prior.chars().count(), ASK_HISTORY_TEST_BUDGET);
    assert_eq!(prior, format!("…Assistant:{newest_content}\n"));
}

/// Every adaptive-marker boundary keeps the newest turn complete and never exceeds the budget.
/// The cases exercise all marker choices: one-scalar prefix, two-scalar line marker, and compact
/// or long labelled marker. Zero headroom is covered by the exact-boundary regression above.
#[test]
fn ask_history_char_budget_adapts_omission_marker_to_available_headroom() {
    for headroom in [1usize, 2, 17, 18, 56, 57] {
        let newest_content = "ż".repeat(15_988 - headroom);
        let history = [
            ChatTurn {
                role: "user".into(),
                content: "older".repeat(30),
            },
            ChatTurn {
                role: "assistant".into(),
                content: newest_content.clone(),
            },
        ];
        let rendered = crate::summarize::vault_chat::render_conversation(&history, "q");
        let prior = rendered_prior_history(&rendered, "q");

        assert!(
            prior.chars().count() <= ASK_HISTORY_TEST_BUDGET,
            "headroom {headroom} exceeded the history budget"
        );
        assert!(
            prior.ends_with(&format!("Assistant: {newest_content}\n")),
            "headroom {headroom} did not preserve the complete newest turn"
        );
        assert!(!prior.contains("older"));
        match headroom {
            1 => assert!(prior.starts_with('…')),
            2 | 17 => assert!(prior.starts_with("…\n")),
            18 | 56 => assert!(prior.starts_with("[Earlier omitted]\n")),
            57 => assert!(prior.starts_with(ASK_HISTORY_TEST_OMISSION_MARKER)),
            _ => unreachable!(),
        }
    }
}

/// Marker readability must not cost an adjacent complete turn. Choose the largest newest
/// contiguous suffix first, then shorten the honest omission marker when the long label would make
/// that suffix overflow.
#[test]
fn ask_history_char_budget_shortens_marker_to_keep_a_larger_complete_suffix() {
    // Rendered costs: 100 + 30 + 15,920 = 16,050. The long 57-scalar marker can accompany only
    // the newest turn, but the 18-scalar compact marker plus the newest two costs 15,968.
    let dropped_content = format!("DROP-{}", "o".repeat(88));
    let adjacent_content = format!("KEEP-{}", "a".repeat(18));
    let newest_content = format!("NEWEST-{}", "ż".repeat(15_901));
    let history = [
        ChatTurn {
            role: "user".into(),
            content: dropped_content.clone(),
        },
        ChatTurn {
            role: "user".into(),
            content: adjacent_content.clone(),
        },
        ChatTurn {
            role: "assistant".into(),
            content: newest_content.clone(),
        },
    ];
    let rendered = crate::summarize::vault_chat::render_conversation(&history, "q");
    let prior = rendered_prior_history(&rendered, "q");

    assert_eq!(prior.chars().count(), 15_968);
    assert!(prior.starts_with("[Earlier omitted]\n"));
    assert!(!prior.contains(&dropped_content));
    assert!(prior.contains(&format!("User: {adjacent_content}\n")));
    assert!(prior.ends_with(&format!("Assistant: {newest_content}\n")));
}

/// A history exactly on the scalar-value boundary remains byte-identical to the old short-history
/// rendering. One extra scalar is what activates the explicit truncation path.
#[test]
fn ask_history_char_budget_preserves_the_exact_boundary() {
    // "User: " (6) + content (15,993) + newline (1) = exactly 16,000 characters.
    let content = "ż".repeat(15_993);
    let history = [ChatTurn {
        role: "user".into(),
        content: content.clone(),
    }];
    let rendered = crate::summarize::vault_chat::render_conversation(&history, "q");
    let prior = rendered_prior_history(&rendered, "q");
    assert_eq!(prior.chars().count(), ASK_HISTORY_TEST_BUDGET);
    assert_eq!(prior, format!("User: {content}\n"));
    assert!(!prior.contains(ASK_HISTORY_TEST_OMISSION_MARKER));

    // One additional scalar must activate the marked, bounded truncation path.
    let over_content = "ż".repeat(15_994);
    let over_history = [ChatTurn {
        role: "user".into(),
        content: over_content,
    }];
    let over_rendered = crate::summarize::vault_chat::render_conversation(&over_history, "q");
    let over_prior = rendered_prior_history(&over_rendered, "q");
    assert!(over_prior.chars().count() <= ASK_HISTORY_TEST_BUDGET);
    assert!(over_prior.starts_with(ASK_HISTORY_TEST_OMISSION_MARKER));
    assert!(over_prior.contains("User: …"));
}

#[test]
fn dashboard_history_drops_only_a_duplicate_current_question() {
    let mut history = vec![
        ChatTurn {
            role: "assistant".into(),
            content: "earlier answer".into(),
        },
        ChatTurn {
            role: "user".into(),
            content: "  current question  ".into(),
        },
    ];
    remove_duplicate_dashboard_question(&mut history, "current question");
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].content, "earlier answer");

    remove_duplicate_dashboard_question(&mut history, "different question");
    assert_eq!(history.len(), 1, "unrelated history remains stable");
}

/// SURFACE SPLIT: the vault executor must NOT advertise `propose_note` (the Ask page has no
/// notes flow / Accept affordance) and must REFUSE to run it (the allowlist fails closed);
/// the in-meeting executor (note_drafts: true) still advertises it. RED-able: drop the
/// `"propose_note" => self.note_drafts` filter arm and the first assertion fails.
#[test]
fn propose_note_hidden_on_ask_surface_but_kept_in_meeting() {
    let db = tmp_db();
    let cfg = AppConfig::default();
    let unlocked = Mutex::new(HashSet::new());

    let vault = ask_executor(&db, &unlocked, &cfg);
    let names_specs = vault.specs();
    let names: Vec<&str> = names_specs.iter().map(|s| s.name.as_str()).collect();
    assert!(
        !names.contains(&"propose_note"),
        "the Ask surface must not advertise propose_note: {names:?}"
    );
    let res = vault.run("propose_note", &serde_json::json!({ "content": "draft" }));
    assert!(
        matches!(res, Err(AppError::InvalidArg(_))),
        "an un-advertised propose_note must be refused by the allowlist: {res:?}"
    );

    let in_meeting = crate::tools::GatedToolExecutor {
        db: &db,
        unlocked: &unlocked,
        config: &cfg,
        meeting_id: "live1",
        app: None,
        recording_token: None,
        allow_writes: false,
        note_drafts: true,
        scope: crate::tools::AssistantScope::Full,
        seal: None,
        proposed_note: Mutex::new(None),
    };
    assert!(
        in_meeting.specs().iter().any(|s| s.name == "propose_note"),
        "the in-meeting surface keeps propose_note advertised"
    );
}

/// LOCK INVARIANT at the Ask surface: a scripted brain that tries to exfiltrate a
/// sealed-not-unlocked meeting through the vault executor surfaces NOTHING sealed — not in the
/// direct tool reads, not in the DTO's citations, not in the resolved `sources` (the
/// citation→source resolver applies the same visibility predicate and only resolves once the
/// folder is session-unlocked).
#[test]
fn ask_loop_never_surfaces_sealed_content() {
    let db = tmp_db();
    seed_note(
        &db,
        "open1",
        "Atlas Kickoff",
        "We decided to ship atlas on Friday.",
        None,
    );
    seed_sealed(&db, "sealed1", "fsec", "Atlas Secret Terms");

    // Seed self-check: the fixture must be sealed-not-unlocked BEFORE we prove the gate.
    let nothing = HashSet::new();
    assert!(db.meeting_is_visible("open1", &nothing).unwrap());
    assert!(
        !db.meeting_is_visible("sealed1", &nothing).unwrap(),
        "seed fixture: sealed1 must be gated"
    );

    let cfg = AppConfig::default();
    let unlocked = Mutex::new(HashSet::new());
    let exec = ask_executor(&db, &unlocked, &cfg);

    // Direct gate proof on THIS surface's executor shape.
    let got = exec
        .run(
            "get_meeting",
            &serde_json::json!({ "meetingId": "sealed1" }),
        )
        .unwrap();
    assert!(
        got.starts_with("No data"),
        "sealed fetch must be gated: {got}"
    );

    // The full loop, driven by an exfiltrating script.
    let brain = ScriptReasoner::ok(vec![
        serde_json::json!({ "tool": "get_meeting", "args": { "meetingId": "sealed1" } }),
        serde_json::json!({ "tool": "search_meetings", "args": { "query": "Atlas Secret Terms" } }),
        serde_json::json!({ "answer": "Here is what I found." }),
    ]);
    let out = ask_vault_loop(
        &brain,
        &exec,
        &db,
        &unlocked,
        "the secret terms?",
        &[],
        "",
        "",
        false,
        None,
        crate::reason::GenOptions::ask_answer(),
    )
    .unwrap()
    .expect("converged");
    assert!(
        !out.answer.contains("Atlas Secret Terms") && !out.answer.contains("SEALED"),
        "sealed source content/title must not reach the produced answer: {}",
        out.answer
    );
    assert!(
        out.citations.iter().all(|c| !c.contains("Secret")),
        "sealed title must never be cited: {:?}",
        out.citations
    );
    assert!(
        out.sources.iter().all(|s| s.meeting_id != "sealed1"),
        "sealed meeting must never resolve into sources: {:?}",
        out.sources
    );

    // The resolver itself is gated: the sealed title resolves ONLY once session-unlocked.
    assert!(
        db.meeting_by_title_visible("Atlas Secret Terms", &nothing)
            .unwrap()
            .is_none(),
        "sealed-not-unlocked title must not resolve"
    );
    let mut open = HashSet::new();
    open.insert("fsec".to_string());
    assert_eq!(
        db.meeting_by_title_visible("Atlas Secret Terms", &open)
            .unwrap()
            .expect("session-unlocked title resolves")
            .id,
        "sealed1"
    );
}

/// The Ask trace stream is its OWN event — record-screen stores must never see Ask chips.
#[test]
fn ask_tool_event_is_distinct() {
    assert_ne!(
        crate::events::EVENT_ASK_TOOL,
        crate::events::EVENT_ASSISTANT_TOOL
    );
    assert_ne!(
        crate::events::EVENT_ASK_TOOL,
        crate::events::EVENT_CHAT_TOOL
    );
}

/// Ask must never search with a vector it did not really compute.
///
/// This is the half of the fix that had NO coverage, and review proved it the hard way: it reverted
/// `commands/ask.rs` alone to the pre-fix `active_admitted_embedder()` calls — restoring the exact
/// bug, where a missing model means the question is embedded into `StubEmbedder`'s hash noise and
/// fused into the answer at full weight — and the entire suite stayed green. Nothing could tell the
/// two states apart.
///
/// It could not, because the logic lived inline in two call sites with nothing to call. It is now
/// `ask_query_vector`, and this drives it directly.
///
/// The suite runs with a stub snapshot forced (`#[cfg(test)]` in `active_embedder_snapshot`, so no
/// test ever loads 470 MB of weights), which is exactly the no-model condition users hit — so the
/// assertion is that an EMPTY vector comes back. Empty is the honest answer: `search_hybrid_visible`
/// short-circuits on it and `score_fuse` redistributes the KNN weight to the legs that have
/// something to say. A non-empty vector here is hash noise wearing a semantic costume.
#[test]
fn ask_produces_no_query_vector_without_a_real_embedder() {
    let with_semantics = crate::commands::ask_commands::ask_query_vector("what did we decide", true);
    assert!(
        with_semantics.is_empty(),
        "a stub snapshot must yield NO vector — got {} dimensions of hash noise, which the fusion \
         would then weight as if it meant something",
        with_semantics.len()
    );

    // And with semantic search switched off there is nothing to compute either way.
    assert!(
        crate::commands::ask_commands::ask_query_vector("what did we decide", false).is_empty(),
        "semantic search off must not embed anything"
    );
}

/// And with a real embedder, Ask must actually USE it.
///
/// The sibling test above only asserts the no-model branch, and review showed why that is not
/// enough on its own: a body of `Vec::new()` — ignoring the flag and the embedder entirely — passed
/// it, because under the suite's forced stub snapshot empty is also the CORRECT answer. That mutant
/// would silently disable semantic search for every user who HAS the model, and Ask would quietly
/// stop using their notes.
///
/// This drives the injectable seam with a deterministic fake standing in for a real embedder, so
/// the two states are finally distinguishable without 470 MB of weights on disk.
#[test]
fn ask_uses_a_real_embedder_when_one_is_available() {
    /// Returns a recognisable constant vector — enough to tell "called through" from "returned
    /// empty", which is the only distinction under test.
    struct FakeRealEmbedder;
    impl crate::embed::Embedder for FakeRealEmbedder {
        fn dim(&self) -> usize {
            crate::embed::EMBED_DIM
        }
        fn embed(&self, texts: &[String]) -> crate::error::Result<Vec<Vec<f32>>> {
            Ok(texts
                .iter()
                .map(|_| vec![0.25_f32; crate::embed::EMBED_DIM])
                .collect())
        }
    }

    let vector = crate::commands::ask_commands::ask_query_vector_with(
        "what did we decide",
        true,
        Some(Box::new(FakeRealEmbedder)),
    );
    assert_eq!(
        vector.len(),
        crate::embed::EMBED_DIM,
        "with a real embedder available Ask must return ITS vector — an empty one here means the \
         semantic leg was dropped for a user who has the model"
    );
    assert!(
        vector.iter().all(|v| (*v - 0.25).abs() < 1e-6),
        "and it must be the embedder's OWN output, not something manufactured"
    );

    // The flag still wins: semantics off means nothing is embedded, model or not.
    assert!(
        crate::commands::ask_commands::ask_query_vector_with(
            "what did we decide",
            false,
            Some(Box::new(FakeRealEmbedder)),
        )
        .is_empty(),
        "semantic search off must not embed, even with an embedder in hand"
    );
}
