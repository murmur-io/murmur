pub mod agent;
pub mod applog;
pub mod audio;
pub mod audit;
pub mod brain_reactions;
pub mod brief_runner;
pub mod calendar;
pub mod commands;
pub mod connectors;
pub mod crypto;
pub mod e2ee;
pub mod embed;
pub mod enrich;
pub mod errcode;
pub mod error;
pub mod eval;
pub mod events;
pub mod export;
pub mod extract;
pub mod import;
pub mod facts;
mod instance_lock;
pub mod links;
pub mod machine;
pub mod mcp;
pub mod memory;
pub mod orchestrate;
pub mod perf;
pub mod pipeline;
pub mod proactive;
pub mod prompts;
pub mod reason;
pub(crate) mod reminder_audit;
pub mod rerank;
pub mod router;
pub mod screenshare;
pub mod secrets;
pub mod settings;
pub mod share;
pub mod state;
pub mod storage;
pub mod summarize;
pub mod thermal;
pub mod tools;
pub mod transcribe;
pub mod update;
pub mod user_memory;
pub mod verify;
pub mod voice_action;

#[cfg(test)]
mod quality_artifact_tests;

use tauri::window::{Effect, EffectsBuilder};
use tauri::{Emitter, Manager, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

use crate::state::AppState;

/// Global hotkey that summons / dismisses the floating recorder bar (like a Spotlight bar).
const SUMMON_SHORTCUT: &str = "CmdOrCtrl+Shift+R";

/// Builds the tauri::Builder, manages AppState, registers commands + the floating bar, runs.
pub fn run() {
    // ── Whisper/ggml-metal residency-set abort guard (macOS 15+ / Apple-silicon) ──
    // whisper.cpp 1.8.3's ggml-metal "residency sets" path (compiled only on macOS >= 15.0)
    // asserts `[rsets->data count] == 0` when the Metal device is freed
    // (ggml-metal-device.m `ggml_metal_rsets_free` → GGML_ASSERT). The teardown ordering on
    // Apple-silicon frees the device while a buffer's residency set is still registered, so the
    // assert fires and `ggml_abort` ABORTS the process at whisper's per-transcription Metal free.
    // GGML_ASSERT (ggml.h:288) is NOT NDEBUG-gated, so this aborts in RELEASE too — it is not a
    // debug-only crash. The upstream-sanctioned switch is the `GGML_METAL_NO_RESIDENCY` env var,
    // read live inside `ggml_metal_device_init` (ggml-metal-device.m:768): when set, the device's
    // residency-set collection is never created (`dev->rsets = nil`), so `ggml_metal_rsets_free`
    // returns early and the assert can never fire. Residency sets are only a GPU-memory residency
    // *hint* (keep buffers wired to avoid OS reclamation); disabling them does not change
    // transcription output — it only forgoes a minor perf optimization. Set here at process entry,
    // strictly before any ggml-Metal device can be initialized. SAFETY: single-threaded at
    // startup, before any thread that could read the env. (Mirror guard in `Transcriber::load`.)
    std::env::set_var("GGML_METAL_NO_RESIDENCY", "1");
    commands::prepare_reminder_runtime_probe_environment();

    // Acquire global process ownership BEFORE touching even the shared log, encrypted library, or
    // recovery ledger. Lease expiry alone cannot prove a mic-only recorder died: a stalled spool
    // thread may miss renewal while CoreAudio remains live, and a second startup would otherwise
    // truncate that generation to its last checkpoint. The local guard lives through Tauri's
    // blocking `.run()` and the exit cleanup; close/OOM/SIGKILL releases it in the kernel.
    let _instance_guard = match instance_lock::acquire() {
        Ok(instance_lock::AcquireResult::Acquired(guard)) => guard,
        Ok(instance_lock::AcquireResult::AlreadyRunning) => {
            instance_lock::show_startup_refusal(instance_lock::StartupRefusal::AlreadyRunning);
            return;
        }
        Err(error) => {
            eprintln!("Murmur startup guard failed: {error}");
            instance_lock::show_startup_refusal(instance_lock::StartupRefusal::GuardUnavailable);
            return;
        }
    };

    let log_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    // Persist logs to a FILE as well as stderr. When the app is launched via LaunchServices (the normal
    // double-click / DMG install) its stderr is discarded, so a signed/release build was previously
    // un-diagnosable — every keychain/share failure was invisible. Logs carry NO PII (IDs, stages,
    // counts, durations only — see the no-PII rule), so persisting them on-device is safe. Fresh file
    // per launch, with the previous session ROTATED to `murmur.prev.log` rather than truncated away
    // (a crash is diagnosed from the run BEFORE the relaunch that goes looking for it — see
    // `applog`); O_APPEND so concurrent writes stay line-atomic. Best-effort: if the file can't be
    // opened we fall back to stderr-only rather than fail startup.
    let log_file = crate::applog::rotate_and_open();
    match log_file {
        Some(file) => {
            use tracing_subscriber::fmt::writer::MakeWriterExt;
            // Wrap the file in an Arc ONCE and tee stderr + the file. tracing-subscriber implements
            // MakeWriter for Arc<W> where &W: io::Write (std impls Write for &File), so every event
            // writes through the shared handle with NO per-event fd clone. The previous
            // `move || file.try_clone().expect(...)` closure ran a `dup(2)` per log line and PANICKED
            // on fd exhaustion — inside the logging subsystem whose whole job is to stay alive through
            // exactly the resource pressure this app has hit in the field. No panic on the log hot path.
            let file = std::sync::Arc::new(file);
            tracing_subscriber::fmt()
                .with_env_filter(log_filter)
                .with_ansi(false)
                .with_writer(std::io::stderr.and(file))
                .init()
        }
        None => tracing_subscriber::fmt().with_env_filter(log_filter).init(),
    }

    // PANIC VISIBILITY (2026-07-16): tokio swallows a panic at the task boundary and the default
    // hook prints only to stderr — which LaunchServices discards for a double-clicked app, so a
    // production pipeline panic left ZERO evidence in murmur.log (the "wedged on Transcribing…"
    // forensics found an idle process and an empty trail). Route every panic through `tracing`
    // (which tees to murmur.log) FIRST, then chain to the previous hook so dev stderr/backtrace
    // behavior is preserved. PII: the PAYLOAD is sanitized before persisting (home-dir prefix
    // redacted + length-capped — an `expect` on an `io::Error` embeds the failing path, and under
    // the vault that filename is note-title-derived); the source LOCATION is compiled-in and
    // carries no user content, so it stays verbatim.
    let previous_panic_hook = std::panic::take_hook();
    // Resolved ONCE here, not inside the hook — the hook must do no avoidable work while the
    // process is already panicking.
    let panic_home = dirs::home_dir().map(|h| h.to_string_lossy().into_owned());
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "unknown".to_string());
        let message = if let Some(s) = info.payload().downcast_ref::<&str>() {
            (*s).to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "non-string panic payload".to_string()
        };
        let message = sanitize_panic_message(&message, panic_home.as_deref());
        tracing::error!(target: "panic", %location, %message, "panic");
        previous_panic_hook(info);
    }));

    let mut builder = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    // Only one shortcut is registered; toggle the bar on key-down.
                    if event.state() == ShortcutState::Pressed {
                        toggle_bar(app);
                    }
                })
                .build(),
        );
    if commands::reminder_runtime_probe_requested() {
        // The exact Harness task drives real product commands through the normal Tauri IPC bridge.
        // The fixed script claims one webview per process; its content-free control endpoint only
        // sequences the runner-owned restart and validates counters/receipts.
        builder = builder.append_invoke_initialization_script(
            commands::reminder_runtime_probe_initialization_script(),
        );
    }
    builder
        .invoke_handler(tauri::generate_handler![
            commands::set_focus_meeting,
            commands::start_recording,
            commands::stop_recording,
            commands::recording_level,
            commands::recording_status,
            commands::set_mic_muted,
            commands::is_mic_muted,
            commands::begin_voice_command,
            commands::end_voice_command,
            commands::ask_assistant_text,
            commands::ask_assistant_chat,
            commands::list_assistant_threads,
            commands::list_input_devices,
            commands::output_is_builtin_speakers,
            commands::get_last_note,
            commands::update_note,
            commands::verify_note_sources,
            commands::apply_note_verify_markers,
            commands::enrich_note_context,
            commands::apply_note_enrichment,
            commands::link_related_notes,
            commands::save_manual_notes,
            commands::get_manual_notes,
            commands::get_note_receipts,
            commands::get_fact_receipt,
            commands::append_to_companion_note,
            commands::get_or_create_companion_note,
            commands::convert_meeting_to_note,
            commands::import_document,
            commands::import_text,
            commands::scan_import,
            commands::run_import,
            commands::cancel_import,
            commands::brain_overview,
            commands::list_documents,
            commands::get_document,
            commands::generate_note_from_document,
            commands::delete_document,
            commands::get_user_memory,
            commands::forget_user_fact,
            commands::forget_entity_fact,
            commands::clear_user_memory,
            commands::import_memories,
            // Re-Truth (the vault heals itself) — supersession review + one-tap stamp + undo.
            commands::preview_supersessions,
            commands::apply_supersessions,
            commands::undo_supersessions,
            // Vault Audit v1 — deterministic vault-health inbox (run + list + resolve).
            commands::run_vault_audit,
            commands::list_audit_findings,
            commands::resolve_audit_finding,
            // Vault Audit Phase 3 — weekly schedule + user-initiated cloud explain.
            commands::get_audit_schedule,
            commands::set_audit_schedule,
            commands::explain_audit_finding,
            commands::get_config,
            commands::get_mcp_config,
            commands::get_mcp_status,
            commands::save_config,
            commands::get_storage_report,
            commands::free_up_space,
            commands::reveal_audio_dir,
            commands::consent_to_cloud_egress,
            commands::revoke_cloud_egress,
            commands::consent_to_web_search,
            commands::consent_to_jira,
            commands::consent_to_slack,
            commands::consent_to_notion,
            commands::consent_to_clickup,
            // Developer mode — the on-device log reader (Settings → Developer → Logs).
            commands::read_app_log,
            commands::clear_app_log,
            commands::export_diagnostics_bundle,
            commands::reveal_app_log,
            // Brain v2 L5 — scheduled briefs (schedule CRUD + propose-accept runs).
            commands::list_brief_schedules,
            commands::create_brief_schedule,
            commands::update_brief_schedule,
            commands::delete_brief_schedule,
            commands::list_brief_runs,
            commands::accept_brief,
            commands::dismiss_brief,
            // Brain v2 L5 — MCP server config (per-server consent-gated external connectors).
            commands::list_mcp_servers,
            commands::add_mcp_server,
            commands::remove_mcp_server,
            commands::consent_to_mcp_server,
            commands::revoke_mcp_consent,
            commands::test_mcp_server,
            // M3-CLIENT — sharing account + zero-knowledge link shares (mode A).
            commands::account_status,
            commands::account_signup,
            commands::account_send_code,
            commands::account_login,
            commands::account_logout,
            commands::unlock_sharing_with_biometric,
            commands::consent_to_share_egress,
            commands::revoke_share_egress,
            commands::mark_sharing_choice_made,
            commands::share_note_to_link,
            commands::list_my_shares,
            commands::revoke_share,
            // M5-CLIENT — Murmur↔Murmur (mode B): invite by email + accept into the vault.
            commands::preview_share_recipient,
            commands::share_note_to_user,
            commands::share_rewrap_pending,
            commands::list_share_inbox,
            commands::accept_share,
            commands::decline_share,
            // M6 Shared Brain (Organizations): create/status/members + consent + preview + share.
            commands::org_create,
            commands::org_status,
            commands::org_list_statuses,
            commands::org_list_cached_statuses,
            commands::org_set_context_enabled,
            commands::org_refresh,
            commands::org_invite_member,
            commands::org_list_members,
            commands::org_remove_member,
            commands::org_leave,
            commands::consent_to_org_egress,
            commands::revoke_org_egress,
            commands::preview_org_share,
            commands::share_meeting_to_org,
            commands::share_document_to_org,
            commands::list_org_shares,
            commands::meeting_org_shares,
            commands::list_meeting_org_shares,
            commands::org_live_shares_for_source,
            commands::revoke_org_share,
            commands::org_sweep_pending,
            commands::org_sync_now,
            commands::org_get_item,
            commands::add_org_item_to_container,
            commands::org_update_item,
            commands::org_set_item_access,
            // Shared containers — publish a whole Folder or Space to an org (2026-08-29).
            commands::preview_container_share,
            commands::share_container_to_org,
            commands::unshare_container,
            commands::set_container_share_access,
            commands::list_container_share_status,
            commands::sync_container_shares,
            commands::list_shared_workspace,
            commands::list_org_share_targets,
            commands::set_shared_placement,
            commands::clear_shared_placement,
            commands::delete_org_item_as_author,
            commands::org_resolve_source,
            commands::list_org_items,
            commands::folder_active_shares,
            commands::revoke_shares_for_folder,
            commands::list_tasks,
            commands::get_task,
            commands::set_task_container,
            commands::move_dashboard_to_container,
            commands::create_task,
            commands::update_task,
            commands::delete_task,
            commands::set_task_local_refs,
            commands::task_list_assignees,
            commands::set_anthropic_key,
            commands::has_anthropic_key,
            commands::set_gateway_key,
            commands::has_gateway_key,
            commands::clear_gateway_key,
            commands::list_gateway_models,
            commands::list_models,
            commands::gateway_health,
            commands::get_egress_ledger,
            commands::set_web_search_api_key,
            commands::has_web_search_key,
            commands::set_jira_token,
            commands::has_jira_token,
            commands::set_slack_token,
            commands::has_slack_token,
            commands::set_notion_token,
            commands::has_notion_token,
            commands::set_clickup_token,
            commands::has_clickup_token,
            commands::provider_statuses,
            commands::resummarize,
            commands::retry_transcription,
            commands::list_meetings,
            commands::search_meetings,
            commands::delete_meeting,
            // TRASH — the 30-day recoverable holding area. `delete_meeting` / `delete_note` /
            // `delete_folder` / `delete_note_folder` now route content HERE instead of destroying it.
            commands::list_trash,
            commands::count_trash,
            commands::restore_trash_item,
            commands::delete_trash_item_forever,
            commands::empty_trash,
            commands::purge_expired_trash,
            commands::get_trash_retention_days,
            commands::set_trash_retention_days,
            commands::rename_meeting,
            commands::chat_meeting,
            commands::chat_meeting_persisted,
            commands::list_ask_conversations,
            commands::load_ask_conversation,
            commands::ask_vault_persisted,
            commands::export_audio,
            commands::export_mic_master,
            commands::export_sys_master,
            commands::export_note,
            commands::detect_meeting_app,
            commands::set_meeting_tags,
            commands::get_meeting_tags,
            commands::list_all_tags,
            commands::list_meetings_by_tag,
            commands::list_builtin_recipes,
            commands::list_saved_recipes,
            commands::save_recipe,
            commands::delete_recipe,
            commands::list_note_templates,
            commands::save_note_template,
            commands::delete_note_template,
            commands::list_saved_views,
            commands::upsert_saved_view,
            commands::delete_saved_view,
            commands::reorder_saved_views,
            commands::list_meeting_action_summaries,
            commands::run_recipe,
            commands::get_action_items,
            commands::patch_note_tasks,
            // First-class Murmur reminder store. `add_reminder` immediately below remains the
            // separate Apple Reminders osascript integration.
            commands::list_reminders,
            commands::get_reminder_summary,
            commands::create_reminder,
            commands::update_reminder,
            commands::delete_reminder,
            commands::complete_reminder,
            commands::dismiss_reminder_occurrence,
            commands::audit_reminder_suggestions,
            commands::accept_reminder_suggestion,
            commands::dismiss_reminder_suggestion,
            commands::reminder_runtime_probe_control,
            // Dashboards — boards + tiles over existing sources. `get_dashboard` resolves every
            // tile through the gated readers; `get_dashboard_sources` feeds the SHIPPED
            // `ask_vault(explicit_sources: …)` path, so board-scoped Ask adds no new AI surface.
            commands::list_dashboards,
            commands::create_dashboard,
            commands::update_dashboard,
            commands::delete_dashboard,
            commands::reorder_dashboards,
            commands::get_dashboard,
            commands::get_dashboard_sources,
            commands::add_dashboard_tile,
            commands::update_dashboard_tile,
            commands::delete_dashboard_tile,
            commands::reorder_dashboard_tiles,
            commands::refresh_dashboard_answer,
            commands::add_reminder,
            commands::pin_moment,
            commands::link_meeting_entities,
            commands::get_graph,
            commands::get_full_graph,
            commands::get_entity_detail,
            commands::get_entity_knowledge_diff,
            commands::get_backlinks,
            commands::list_links,
            commands::accept_link,
            commands::dismiss_link,
            commands::link_items,
            commands::unlink_items,
            commands::resolve_wikilink,
            commands::list_link_candidates,
            commands::get_person_dossier,
            commands::list_people,
            commands::ask_vault,
            commands::entity_dossier,
            commands::generate_digest,
            commands::topic_threads,
            commands::export_canvas,
            commands::next_calendar_event,
            commands::list_calendar_events,
            commands::calendar_context_for,
            commands::get_meeting_detail,
            commands::get_meeting_segments,
            commands::get_analytics,
            commands::get_timeline,
            commands::generate_timeline,
            commands::timeline_generation_on_device,
            commands::rename_speaker,
            commands::suggest_speaker_labels,
            commands::list_voiceprints,
            commands::forget_voiceprint,
            commands::clear_voiceprints,
            commands::model_present,
            commands::download_model,
            commands::cancel_model_download,
            commands::delete_whisper_model,
            commands::parakeet_models_present,
            commands::download_parakeet_models,
            commands::brain_model_present,
            commands::list_brain_models,
            commands::brain_model_retirement_nudge,
            commands::whisper_recommendation,
            commands::machine_change_nudge,
            commands::dismiss_machine_change_nudge,
            commands::brain_posture,
            commands::resolved_ai_map,
            commands::set_brain_posture,
            commands::brain_live_ram_ok,
            commands::brain_reactions_shadow_count,
            commands::set_brain_contradiction_cards,
            commands::select_brain_model,
            commands::download_brain_model,
            commands::afm_available,
            commands::embed_model_present,
            commands::list_embed_models,
            commands::select_embed_model,
            commands::download_embed_model,
            commands::reindex_embeddings,
            commands::related_meetings,
            commands::ner_model_present,
            commands::download_ner_model,
            commands::toggle_bar,
            commands::list_folders,
            // Workspace hierarchy plus review-first organization of visible unfiled recordings.
            commands::list_workspace_tree,
            commands::list_container_items,
            commands::get_container,
            // The "Add related" hierarchy picker's own gated reader — separate from the workspace
            // tree above on purpose (three linkable leaf kinds, never task/dashboard; a bounded
            // window that CONTAINS the anchor rather than a capped tree payload).
            commands::get_related_picker_bootstrap,
            commands::list_related_picker_items,
            commands::search_related_picker,
            commands::plan_workspace_organization,
            commands::apply_workspace_organization,
            commands::create_space,
            commands::create_folder,
            commands::rename_folder,
            commands::delete_folder,
            commands::move_note,
            commands::get_filing_recovery_status,
            commands::retry_filing_recovery,
            commands::keep_existing_filing_file,
            // Notes feature — authored `documents(kind='note')` CRUD + note folders + vault export.
            commands::create_note,
            commands::suggest_note_title,
            commands::get_note,
            commands::update_note_doc,
            commands::save_note_text,
            commands::list_notes,
            commands::move_note_doc,
            commands::delete_note,
            commands::export_note_doc,
            commands::add_note_attachment,
            commands::list_note_attachments,
            commands::delete_note_attachment,
            commands::list_note_folders,
            commands::create_note_folder,
            commands::rename_note_folder,
            commands::delete_note_folder,
            commands::move_note_folder,
            // Feature C — typed note front-matter properties (note-folder schemas + Table/Board).
            commands::get_note_folder_schema,
            commands::set_note_folder_schema,
            commands::list_notes_typed,
            // Notes — selection Brain-assistant (WP4) + auto-organize (WP5) + link sharing (WP6).
            commands::note_assistant_action,
            commands::plan_organize_notes,
            commands::apply_organize_plan,
            commands::share_note_to_link_doc,
            commands::lock_folder,
            commands::lock_folder_allow_remote_access,
            commands::unlock_folder,
            commands::unlock_meeting,
            commands::relock_folder,
            commands::relock_all,
            commands::remove_lock,
            commands::discard_unrecoverable_folder_lock,
            commands::discard_unrecoverable_meeting_lock,
            commands::check_for_update_guarded,
            update::app_info,
            update::open_release_page,
        ])
        .setup(|app| {
            // Open the encrypted library FIRST. If it fails (keychain access denied, or the DB
            // key doesn't match / the file is corrupt) we must NOT panic/abort — that is the
            // v0.3.0 hard-crash. Instead show a friendly dialog and exit cleanly, leaving the DB
            // and its backups untouched on disk.
            let state = match AppState::init() {
                Ok(s) => s,
                Err(e) => {
                    show_fatal_init_dialog(app.handle().clone(), &e);
                    // Return Ok so the event loop spins: the dialog runs on a worker thread and
                    // dispatches to the main run loop, then calls std::process::exit(1). We do NOT
                    // run the rest of setup (windows/tray/MCP) without a valid AppState.
                    return Ok(());
                }
            };
            app.manage(state);
            // One Tauri-managed, process-local cancellation authority is shared by the MCP
            // transport and every relock entrypoint. It contains socket clones and content-free
            // lease ids only; no meeting data or authentication material.
            app.manage(std::sync::Arc::new(crate::mcp::McpResponseGate::new()));
            // Managed BEFORE `mcp::spawn` so the listener's first status write lands on a
            // handle the `get_mcp_status` command can already read.
            app.manage(std::sync::Arc::new(crate::mcp::McpListenerStatus::default()));

            // PRE-WINDOW legacy-recovery guard. Historical paired crash artifacts are plaintext and
            // cannot be honestly presented as protected by an already-locked folder. Publish their
            // SQLCipher markers and read folder lock state synchronously, before egress setup,
            // windows/tray/MCP, helper detection, or recovery claims. A locked owner takes the same
            // graceful fatal-dialog path as DB init failure; DB and files remain untouched.
            let mut legacy_recovery_preflight = {
                let state = app.state::<AppState>();
                match crate::audio::spill::startup_legacy_recovery_preflight(&state.db) {
                    Ok(protection) => protection,
                    Err(error) => {
                        show_fatal_init_dialog(app.handle().clone(), &error);
                        return Ok(());
                    }
                }
            };

            // Phase 2b — wire the content-free egress ledger. Done here, once, after a successful
            // AppState::init() so the DB is guaranteed open. Every subsequent cloud provider call
            // (routed through RedactingProvider) writes ONE content-free row to egress_log.
            {
                let state = app.state::<AppState>();
                crate::summarize::egress_log::set_egress_sink(std::sync::Arc::new(
                    crate::summarize::egress_log::DbEgressSink::new(state.db.clone()),
                ));
            }

            // Load the user's saved NOTE TEMPLATES into the renderer registry so a note-style
            // selection of a saved template resolves at Stop-time (`summarize::template::
            // build_template` sees only the style STRING, not the DB). Best-effort: a read failure
            // just leaves the registry empty (built-in styles still work) — never crash boot.
            {
                let state = app.state::<AppState>();
                match state.db.list_note_templates() {
                    Ok(templates) => crate::summarize::template::set_saved_templates(templates),
                    Err(e) => tracing::warn!(
                        target: "note_templates",
                        error = %e,
                        "failed to load saved note templates at startup"
                    ),
                }
            }

            // Crash-recovery (STAGE 2 spill salvage + STAGE 3 disk salvage + STAGE 1 reconcile) +
            // surviving-helper detection. A session that died mid-record (crash / SIGKILL /
            // `tauri dev` hot-rebuild) never ran `stop_recording`, so the meeting row
            // `start_recording` inserted up-front sits
            // as a `RECORDING` "ghost" AND its mic audio (RAM-only) + far-side scratch would be lost.
            // ORDER IS LOAD-BEARING:
            //   1) DETECT surviving helpers BEFORE moving or reading any recovery audio content.
            //      The scanner never signals cross-launch PIDs; any live/orphan/ambiguous helper defers ALL
            //      salvage and disables the age sweep, because it may still be writing the same
            //      inode and model/audio recovery must not overlap another process.
            //   2) CLAIM spill salvage: find inflight mic spills of crashed recordings. A paired
            //      far-side scratch is preserved with an exclusive copy-on-write clone outside
            //      $TMPDIR only after the clean scan proves no helper can still be writing it; this
            //      compatibility path is not loaded into the recovery pipeline.
            //   2b) CLAIM disk salvage (2026-07-16): among the ghosts the spill did NOT claim (spill
            //      wins — it has both streams), any whose ARCHIVE WAV survived on disk (the pipeline
            //      died AFTER finalize_meeting) is re-run from disk instead of being flipped to
            //      ERROR with intact, never-re-transcribed audio. Sealed audio / a locked folder is
            //      NEVER decrypted here — those rows fall through to reconcile (terminal ERROR, audio
            //      untouched) and stay recoverable via `retry_transcription` after an unlock.
            //   3) RECONCILE the remaining ghosts to terminal `ERROR` — SKIPPING every claimed row
            //      (salvage sets their final status itself), so a claimed row isn't clobbered.
            //   4) SWEEP stale scratch only after both the clean helper scan and exact claims.
            //   5) SPAWN the async salvage worker: reconstruct each claimed recording + run it through
            //      the EXISTING post-Stop pipeline → a real transcript+note. Best-effort; never
            //      deletes un-salvaged audio.
            //   (1-5 run inside ONE detached task below — sequenced, off the setup thread.)
            // Detect any helper ORPHANED by a previous session that died without a clean Stop.
            // Cross-process PID signaling cannot be bound atomically to a process generation on
            // Darwin, so startup never guesses: any orphan/ambiguous helper preserves all scratch;
            // the next Start repeats the scan and fails closed. New audio helpers watch the exact
            // parent-owned stdin pipe and retain a 4 h wall cap; Brain independently watches its
            // parent-owned stdin pipe and exits even during a stuck generation. A helper owned by
            // a LIVE Murmur process (a genuinely concurrent instance) is never touched and defers
            // recovery/capture until that owner exits.
            //
            // Steps 1-5 stay OFF the setup thread because process-table/filesystem probes and
            // recovery are blocking. The scan+sweep is awaited before salvage starts, and an early
            // `start_recording` has its own awaited fail-closed scan under the recording-priority
            // boundary.
            {
                let recovery_owner = match crate::perf::begin_startup_recovery() {
                    Ok(owner) => owner,
                    Err(error) => {
                        show_fatal_init_dialog(app.handle().clone(), &error);
                        return Ok(());
                    }
                };
                // Start background reminder work only after every synchronous fatal startup
                // preflight has accepted this process. A rejected DB/recovery launch must not
                // materialize occurrences or emit events while its fatal dialog is exiting.
                commands::spawn_reminder_scheduler(app.handle().clone());
                let app_handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    // This affine owner is deliberately retained across BOTH detection/claim and the
                    // actual salvage-thread join. Start fails fast while it exists, so recovery ASR,
                    // RAM and global status events can never overlap a fresh recording.
                    let recovery_owner = recovery_owner;
                    let worker_app = app_handle.clone();
                    let recovered = tauri::async_runtime::spawn_blocking(move || {
                        let state = worker_app.state::<AppState>();
                        let _lifecycle = state
                            .lifecycle
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        if let Err(error) = crate::audio::aec::detect_surviving_capture_helpers(Some(
                            &mut legacy_recovery_preflight.scratch_protection,
                        )) {
                            // A helper may still have the exact scratch inode open. Do not rename,
                            // read, reconcile, or enqueue any source while that writer/model is
                            // alive; leave every durable marker/artifact for a later clean launch.
                            legacy_recovery_preflight
                                .scratch_protection
                                .preserve_all();
                            tracing::error!(target: "startup", error = %error, "helper active or scan ambiguous; deferring all recording recovery and preserving scratch");
                            return Ok((Vec::new(), Vec::new(), Vec::new()));
                        }
                        let recovery_dirs = crate::pipeline::recording_inflight_dir()
                            .and_then(|inflight| {
                                crate::pipeline::audio_dir().map(|archive| (inflight, archive))
                            });
                        let (ledger_jobs, mut claimed) = match recovery_dirs {
                            Ok((inflight, archive)) => {
                                crate::audio::source::claim_stale_recording_generations(
                                    &state.db,
                                    &inflight,
                                    &archive,
                                )
                            }
                            Err(error) => {
                                tracing::warn!(target: "startup", error = %error, "recording ledger recovery directory unavailable");
                                (Vec::new(), Vec::new())
                            }
                        };
                        let (salvage_jobs, spill_claimed) = crate::audio::spill::claim_inflight(
                            &state.db,
                            &mut legacy_recovery_preflight,
                        )?;
                        claimed.extend(spill_claimed);
                        let (disk_salvage_jobs, disk_claimed) =
                            crate::audio::spill::claim_disk_salvage(&state.db, &claimed);
                        claimed.extend(disk_claimed);
                        if let Err(error) = state.db.reconcile_stuck_recordings_except(&claimed) {
                            tracing::warn!(target: "startup", error = %error, "could not reconcile stuck recordings");
                        }
                        crate::audio::aec::sweep_stale_scratch(
                            &legacy_recovery_preflight.scratch_protection,
                        );
                        Ok::<_, crate::error::AppError>((
                            ledger_jobs,
                            salvage_jobs,
                            disk_salvage_jobs,
                        ))
                    })
                    .await;
                    let salvage_thread = match recovered {
                        Ok(Ok((ledger_jobs, salvage_jobs, disk_salvage_jobs))) => {
                            // After the clean fail-closed helper scan, process exact ledger prefixes
                            // first, then legacy spill and archived-WAV recovery. The returned owner
                            // is joined below before recording admission is reopened.
                            crate::audio::spill::spawn_salvage(
                                app_handle.clone(),
                                ledger_jobs,
                                salvage_jobs,
                                disk_salvage_jobs,
                            )
                        }
                        Ok(Err(error)) => {
                            // A post-preflight identity/inventory failure is a startup boundary,
                            // not a best-effort recovery miss. Continuing would expose the UI and
                            // run the stale-temp sweep without authenticated ownership of every
                            // historical source. Preserve all artifacts and fail closed.
                            // Keep recording admission closed until the fatal-dialog worker exits
                            // the process; returning normally would drop this owner and reopen the
                            // global Start path while the modal is still visible.
                            std::mem::forget(recovery_owner);
                            show_fatal_init_dialog(app_handle.clone(), &error);
                            return;
                        }
                        Err(error) => {
                            tracing::warn!(target: "startup", error = %error, "recording recovery worker join failed; artifacts preserved");
                            None
                        }
                    };
                    if let Some(salvage_thread) = salvage_thread {
                        match tauri::async_runtime::spawn_blocking(move || salvage_thread.join())
                            .await
                        {
                            Ok(Ok(())) => {}
                            Ok(Err(_)) => tracing::warn!(target: "startup", "recording salvage worker panicked; artifacts preserved"),
                            Err(error) => tracing::warn!(target: "startup", error = %error, "recording salvage join task failed; artifacts preserved"),
                        }
                    }

                    drop(recovery_owner);
                    // The standby listener also owns Whisper RAM; start it only after recovery has
                    // relinquished priority. Its own lifecycle gate suppresses a race with a user
                    // who starts recording immediately at this boundary.
                    commands::restart_voice_listener(app_handle);
                });
            }
            // R1: reclaim any STALE `*.part` model-download residue (a crash / force-quit / aborted
            // model switch mid-download orphans up to ~3.1 GB). Only removes a `.part` older than 1 h
            // so a live in-progress download is never raced. Best-effort; never fatal to launch.
            if let Ok(models_dir) = crate::transcribe::model::models_dir() {
                crate::transcribe::model::sweep_stale_model_parts(&models_dir);
            }
            // P1: notice a MACHINE CHANGE (restore-from-backup / Migration Assistant onto a
            // different Mac) and record the model-size provenance for existing installs.
            //
            // Both are settings-row writes, deliberately NOT an event: Tauri does not buffer
            // events and the webview has not called `listen()` yet during `setup`, so an event
            // emitted here would simply be lost. The FE PULLS via `machine_change_nudge()`.
            // `note_machine_fingerprint` writes the pending row BEFORE moving the fingerprint, so
            // a crash in between leaves the nudge recoverable rather than silently consumed.
            // Best-effort throughout: a settings write failure must never be fatal to launch, and
            // nothing here logs any user content.
            {
                let state = app.state::<AppState>();
                let fingerprint = crate::machine::current_fingerprint();
                if let Err(e) =
                    crate::settings::note_machine_fingerprint(&state.db, fingerprint.as_deref())
                {
                    tracing::warn!(target: "startup", error = %e, "machine fingerprint compare failed");
                }
                let (onboarded, model_size) = match state.config.lock() {
                    Ok(c) => (c.onboarded, c.model_size.clone()),
                    Err(_) => (false, String::new()),
                };
                if let Err(e) =
                    crate::settings::backfill_model_size_source(&state.db, onboarded, &model_size)
                {
                    tracing::warn!(target: "startup", error = %e, "model_size_source backfill failed");
                }
            }
            // R2: reclaim any orphaned export temp DOTFILE (`.<stem>.<pid>.tmp` / `.edit.<pid>.tmp`)
            // a SIGKILL between fsync + rename left in the user's Obsidian vault. Only removes a temp
            // whose PID is dead OR whose mtime is > 1 h, and never descends into `.obsidian`/`.trash`,
            // so a live export is never raced. Skipped entirely when no vault is configured.
            {
                let state = app.state::<AppState>();
                let vault = state.config.lock().ok().and_then(|c| c.vault_path.clone());
                if let Some(vault) = vault.filter(|v| !v.trim().is_empty()) {
                    crate::export::obsidian::sweep_stale_export_tmp(std::path::Path::new(&vault));
                }
            }

            create_bar_window(app.handle())?;
            if let Err(e) = app.global_shortcut().register(SUMMON_SHORTCUT) {
                tracing::warn!(target: "shortcut", error = %e, "could not register global shortcut");
            }
            setup_tray(app.handle())?;
            // Localhost MCP server (read-only meeting tools for Claude Desktop/Code; no egress).
            // Pass the AppHandle so the server resolves the ONE managed AppState for every request:
            // DB, live session unlock set, seal epoch, and lifecycle guard cannot drift into an
            // independently-opened/snapshotted authority.
            if !commands::reminder_runtime_probe_requested() {
                let state = app.state::<AppState>();
                let require_token = state
                    .config
                    .lock()
                    .map(|c| c.mcp_require_token)
                    // Poisoned config ⇒ fail CLOSED (require the token) — aligned with the
                    // reasoner-dispatch poison posture (unreadable config never relaxes auth).
                    .unwrap_or(true);
                crate::mcp::spawn(app.handle().clone(), require_token);
            } else {
                // The canonical runtime smoke treats the real MCP listener as its readiness
                // witness. In this isolated debug process only, the IPC privacy probe starts MCP
                // after it has proved relock/restart masking and the no-egress configuration matrix.
                tracing::info!(
                    target: "reminders",
                    "native reminder runtime privacy probe is pending"
                );
            }
            // Brain v2 L1.1 — TOPIC-CHUNK startup backfill: index topic segments for every VISIBLE
            // meeting that has a transcript, content-hash idempotent (an already-indexed vault is a
            // cheap probe pass), in batches of 20 on a blocking worker so setup never stalls.
            // Skipped entirely when the real embed model is absent (the no-stub-vector-at-rest
            // invariant) — the default install writes nothing.
            {
                let state = app.state::<AppState>();
                let db = state.db.clone();
                // Respect the feature flag: with semantic search OFF the pipeline never
                // auto-indexes, so the backfill must not write topic plaintext either
                // (flag discipline — same posture as `should_auto_index`).
                let semantic_enabled = state
                    .config
                    .lock()
                    .map(|c| c.semantic_search_enabled)
                    .unwrap_or(false);
                tauri::async_runtime::spawn_blocking(move || {
                    let background_epoch = crate::perf::background_epoch();
                    if !crate::perf::background_epoch_is_current(background_epoch) {
                        return;
                    }
                    // 2026-07-13 launch-freeze incident: on a RAM-starved machine, defer the whole
                    // backfill (incl. the lazy Candle/Metal embedder load) rather than starting it
                    // at launch — it is content-hash idempotent, so a later, healthier launch just
                    // picks up where this one left off.
                    if !crate::transcribe::model::topic_backfill_ram_permits_now() {
                        tracing::info!(target: "rag", "topic-chunk backfill skipped: low system RAM");
                        return;
                    }
                    // Brain v3 (audit gap #2) — the IDEMPOTENT repair tick: backfill meetings with a
                    // note but no chunks / chunks but no vectors, and documents via the needs-only
                    // probe (chunk-only backfill runs even MODEL-ABSENT, like the reindex doc leg —
                    // so it runs BEFORE the model/flag early-return below). All reads inside run
                    // under the EMPTY unlock set (sealed content is never touched, even
                    // mid-session-unlock). Counts only — no PII.
                    let model_present = crate::embed::embed_model_present();
                    let embedder: Box<dyn crate::embed::Embedder> = if model_present {
                        match crate::embed::background_persistence_embedder(background_epoch) {
                            Ok(embedder) => embedder,
                            Err(error) => {
                                // Presence changed or construction became impossible after the
                                // probe. Defer the idempotent repair instead of writing stub or a
                                // second model's vectors into the same index generation.
                                tracing::warn!(target: "rag", error = %error, "brain index repair tick deferred: embed model unavailable");
                                return;
                            }
                        }
                    } else {
                        // Model-absent work is strictly document chunk/FTS repair; this epoch-aware
                        // stub cannot persist vectors because `model_present == false` gates them.
                        crate::embed::background_embedder(background_epoch)
                    };
                    match crate::commands::backfill_missing_brain_indexes_background(
                        &db,
                        semantic_enabled,
                        model_present,
                        embedder.as_ref(),
                        background_epoch,
                    ) {
                        Ok((meetings, docs)) if meetings + docs > 0 => {
                            tracing::info!(
                                target: "rag",
                                meetings,
                                docs,
                                "brain index repair tick backfilled missing chunks/vectors"
                            );
                        }
                        Ok(_) => {}
                        Err(e) => {
                            tracing::warn!(target: "rag", error = %e, "brain index repair tick failed");
                        }
                    }
                    if model_present {
                        // Heal a partial/interrupted Org rebuild under the SAME pinned REAL model
                        // and epoch. Bounded to four items at startup; canonical replica rows,
                        // chunks/FTS and feed cursors are untouched.
                        match crate::commands::repair_missing_org_embeddings(
                            &db,
                            embedder.as_ref(),
                            4,
                            Some(background_epoch),
                        ) {
                            Ok(indexed) if indexed > 0 => {
                                tracing::info!(target: "rag", indexed, "org vector repair tick backfilled missing vectors");
                            }
                            Ok(_) => {}
                            Err(e) => {
                                tracing::warn!(target: "rag", error = %e, "org vector repair tick deferred");
                            }
                        }
                    }
                    if !semantic_enabled || !model_present {
                        return;
                    }
                    if !crate::perf::background_epoch_is_current(background_epoch) {
                        return;
                    }
                    let started = std::time::Instant::now();
                    match db.backfill_topic_chunks_idempotent_background(
                        embedder.as_ref(),
                        background_epoch,
                    ) {
                        Ok(indexed) if indexed > 0 => {
                            tracing::info!(
                                target: "rag",
                                indexed,
                                elapsed_ms = started.elapsed().as_millis() as u64,
                                "topic-chunk backfill complete"
                            );
                        }
                        Ok(_) => {}
                        Err(e) => {
                            tracing::warn!(target: "rag", error = %e, "topic-chunk backfill failed");
                        }
                    }
                });
            }
            // Brain v2 L2.1 — the HOURLY memory consolidation/reflection job. Each tick re-resolves
            // everything from the LIVE AppState (the `memory_consolidation_enabled` +
            // `user_memory_enabled` flags, the LIGHT local-or-stub reasoner — NEVER cloud, so zero
            // egress — and the vault path), runs one `run_consolidation_pass` on a BLOCKING worker
            // (the local reasoner is synchronous; the DB lock is never held across an LLM call),
            // and warns-and-continues on any failure — the loop never exits. First tick is a full
            // interval after launch (no startup Metal contention); a stub/light-model-absent tick
            // is a cheap no-op inside `consolidation_tick`.
            {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    loop {
                        tokio::time::sleep(std::time::Duration::from_secs(
                            crate::memory::CONSOLIDATION_INTERVAL_SECS,
                        ))
                        .await;
                        let tick_handle = handle.clone();
                        let joined = tauri::async_runtime::spawn_blocking(move || {
                            crate::memory::consolidation_tick(&tick_handle);
                        })
                        .await;
                        if let Err(e) = joined {
                            tracing::warn!(target: "memory", error = %e, "consolidation tick join failed");
                        }
                        // Vault Audit Phase 3 — the WEEKLY scheduled-audit due-check rides this
                        // same hourly cadence (no new cadence class; first check a full interval
                        // after launch, so run-on-launch-if-due comes free at +1h). The tick
                        // claims-before-run, gates on thermal/RAM, runs the pass on a blocking
                        // worker, and never errors (warn-and-continue inside).
                        crate::audit::audit_weekly_tick(&handle).await;
                    }
                });
            }
            // Brain v2 L5 — the 60s SCHEDULED-BRIEF runner (mirrors the memory loop above: each
            // tick re-reads the LIVE schedules + config from AppState, warns-and-continues on any
            // failure, never exits; the FIRST tick is a full interval after launch). Corpus reads
            // are gated with the EMPTY unlock set inside `brief_tick`; synthesis rides the Notes
            // provider seam (consent gate + redaction + ledger). Quiet when no schedules exist.
            {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    loop {
                        tokio::time::sleep(std::time::Duration::from_secs(
                            crate::brief_runner::BRIEF_TICK_SECS,
                        ))
                        .await;
                        crate::brief_runner::brief_tick(&handle).await;
                    }
                });
            }
            // M6 Shared Brain — the background org-feed sync loop. Each tick best-effort (a) drains the
            // OUTBOUND org-share queue (offline-failed publishes + pending revokes) and (b) pulls +
            // ingests the INBOUND org feed into the local int8 partition, so every member's brain stays
            // fresh for Ask/MCP WITHOUT opening Settings — the "replicated brain" contract. Mirrors the
            // memory/brief loops: re-reads live AppState each tick via `try_state`, gates to a cheap
            // no-op when logged out / no org joined, first tick a short delay after launch.
            {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(
                        crate::commands::ORG_SYNC_FIRST_DELAY_SECS,
                    ))
                    .await;
                    loop {
                        if let Some(state) = handle.try_state::<AppState>() {
                            // On a PRODUCTIVE tick (≥1 ingest/tombstone) emit a content-free
                            // `org-feed-updated` ping so an open FE view (Notes org picker /
                            // Settings shared-brain list) re-fetches without polling. Best-effort:
                            // a failed emit never breaks the loop. The count is aggregated across
                            // orgs by the all-orgs tick, so we report `1` (≥1 org changed).
                            if crate::commands::org_background_sync_tick(
                                state.inner(),
                                Some(handle.clone()),
                            )
                            .await
                            {
                                crate::events::emit_org_feed_updated(&handle, 1);
                            }
                        }
                        tokio::time::sleep(std::time::Duration::from_secs(
                            crate::commands::ORG_SYNC_TICK_SECS,
                        ))
                        .await;
                    }
                });
            }
            // LOGS — the retention pruner. The on-device log is kept for 24 h and no longer
            // (`applog::RETENTION_HOURS`); this tick is what makes "and no longer" true without
            // anyone opening the Developer-mode Logs view. Cheap and silent when nothing has
            // expired: it stats the previous-session file and scans the current one's head.
            {
                tauri::async_runtime::spawn(async move {
                    loop {
                        tokio::time::sleep(std::time::Duration::from_secs(
                            crate::applog::PRUNE_TICK_SECS,
                        ))
                        .await;
                        if let Err(error) = crate::applog::prune_expired() {
                            tracing::warn!(target: "applog", %error, "log prune tick failed");
                        }
                    }
                });
            }
            // TRASH — the expired-entry purge loop. Rides the same shape as the memory/brief loops:
            // re-reads live AppState each tick, warns-and-continues on any failure, never exits, and
            // the FIRST tick is a full interval after launch (so a cold start is never competing with
            // it). Cheap and quiet when the trash is empty. A SEALED entry is skipped by
            // `purge_expired` rather than force-purged — the lock outranks the schedule, so a locked
            // folder can hold expired entries past their date until the user unlocks it.
            {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    loop {
                        tokio::time::sleep(std::time::Duration::from_secs(
                            crate::commands::TRASH_PURGE_TICK_SECS,
                        ))
                        .await;
                        if let Some(state) = handle.try_state::<AppState>() {
                            match crate::commands::purge_expired(state.inner(), Some(&handle)).await
                            {
                                Ok(n) if n > 0 => {
                                    crate::events::emit_trash_updated(
                                        &handle,
                                        state.inner().db.count_trash_entries().unwrap_or(0),
                                    );
                                }
                                Ok(_) => {}
                                Err(e) => {
                                    tracing::warn!(target: "trash", error = %e, "expired-trash purge tick failed");
                                }
                            }
                        }
                    }
                });
            }
            // Screen-share auto-relock watcher: on capture START, relock all session-unlocked
            // folders + zeroize the KEK and toast the UI. Gated by K_RELOCK_ON_SCREENSHARE.
            crate::screenshare::spawn(app.handle().clone());
            // Closing the main window HIDES it (recoverable from the tray) instead of
            // quitting — so the floating bar is never the only way back into the app.
            // B12/C4: on a window close, relock all session-unlocked folders + zeroize the cached
            // master KEK + checkpoint the WAL — even though the app keeps running in the tray, the
            // unlocked content should not survive the user closing the window.
            if let Some(main) = app.get_webview_window("main") {
                let w = main.clone();
                let handle = app.handle().clone();
                main.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        // Window-close only HIDES (recoverable from the tray) — do NOT stop capture.
                        relock_and_zeroize_on_lifecycle(&handle, LIFECYCLE_CTX_WINDOW_CLOSE);
                        let _ = w.hide();
                    }
                });
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|handle, event| {
            // B12/C4 app-quit hook: on ExitRequested, relock everything, zeroize the cached KEK, and
            // checkpoint+truncate the WAL so no plaintext lingers past the process. This is the
            // last-chance cleanup before the process tears down.
            if let tauri::RunEvent::ExitRequested { .. } = event {
                // True exit — relock + stop_all_capture (C2) run FIRST inside the hook, THEN kill the
                // brain sidecar so its multi-GB model RAM is reclaimed on quit (bounded reap — never
                // hangs app-exit). The child also owns an exact stdin-lifetime watcher, so losing
                // the host pipe terminates it without relying on a recycled PID at next launch.
                relock_and_zeroize_on_lifecycle(handle, LIFECYCLE_CTX_APP_EXIT);
                crate::reason::sidecar::kill_on_quit();
            }
        });
}

/// Max chars of a panic PAYLOAD persisted to murmur.log by the global hook. Generous for every
/// panic message our code produces, while a pathological payload (a formatted struct dump, a
/// whole file body threaded into an `expect`) can no longer flood the log.
const PANIC_MESSAGE_MAX_CHARS: usize = 400;

/// Sanitize a panic payload before the global hook persists it to murmur.log (hardening
/// 2026-07-16): redact the user's home-directory prefix (→ `~`) and length-cap the result
/// ([`PANIC_MESSAGE_MAX_CHARS`]). An `expect` on an `io::Error` embeds the failing PATH in the
/// payload, and under the vault that filename is note-title-derived — i.e. PII the log must not
/// carry (rule: logs carry IDs/stages/counts only). The panic LOCATION is handled separately by
/// the caller and stays verbatim — compiled-in source paths carry no user content.
///
/// PURE + PANIC-FREE (this runs inside the panic hook): char-boundary-safe truncation, no
/// allocation tricks, no unwraps. A `home` of `None` or a degenerate one-char home (redacting
/// `/` would blank every path separator) skips redaction.
fn sanitize_panic_message(message: &str, home: Option<&str>) -> String {
    let redacted = match home {
        Some(h) if h.len() > 1 => message.replace(h, "~"),
        _ => message.to_string(),
    };
    // `char_indices` finds the byte offset of the cap-th char (if any) — boundary-safe for
    // multi-byte payloads, and O(cap) instead of counting the whole string.
    match redacted.char_indices().nth(PANIC_MESSAGE_MAX_CHARS) {
        Some((byte_idx, _)) => format!("{}…[truncated]", &redacted[..byte_idx]),
        None => redacted,
    }
}

/// Lifecycle-hook context tags. `app-exit` is the TRUE quit (stop capture + relock); `window-close`
/// merely hides the window while the app keeps running/recording in the tray (relock only, NEVER
/// stop capture). Kept as named constants so the C2 exit-only gate can't drift on a typo.
const LIFECYCLE_CTX_APP_EXIT: &str = "app-exit";
const LIFECYCLE_CTX_WINDOW_CLOSE: &str = "window-close";

/// Shared lifecycle cleanup (B12/C4): relock every session-unlocked folder (which re-blanks plaintext
/// + zeroizes the cached master KEK) and checkpoint+truncate the WAL. Best-effort and panic-free —
///   invoked from both the window-close and app-exit paths, where there is no Result to surface. No-op
///   if AppState was never managed (the graceful-init failure path returns early without it).
///
/// C2: on the TRUE exit path (`ctx == "app-exit"`) we ALSO stop every capture path in-process
/// (`stop_all_capture`) FIRST, so quitting mid-recording finalizes + reaps the Swift capture helpers.
/// Each helper also watches an exact parent-owned stdin pipe and self-terminates if the host dies;
/// next-launch discovery is detection-only and never signals a PID. We deliberately do NOT stop
/// capture on `ctx == "window-close"`: closing the window only HIDES it and the app keeps recording
/// in the tray — killing capture there would silently drop an active tray recording.
fn relock_and_zeroize_on_lifecycle(handle: &tauri::AppHandle, ctx: &str) {
    use crate::state::AppState;
    let Some(state) = handle.try_state::<AppState>() else {
        return; // init failed / state not managed — nothing to clean up.
    };
    // C2: FIRST, on the true exit path only, finalize + reap the capture helpers. Never on a mere
    // window-hide (the tray recording must survive that).
    if ctx == LIFECYCLE_CTX_APP_EXIT {
        crate::commands::stop_all_capture(state.inner());
    }
    // The wrapper first cancels every active MCP content socket, then clears the unlock set,
    // zeroizes the cached KEK, re-blanks all sealed notes, and checkpoints the WAL.
    if let Err(e) = crate::commands::relock_all_with_visibility_gate(handle, state.inner()) {
        tracing::warn!(target: "lock", error = %e, ctx, "lifecycle relock_all failed");
    }
    // Belt-and-suspenders WAL checkpoint in case relock_all short-circuited before its own.
    if let Err(e) = state.db.checkpoint_truncate() {
        tracing::warn!(target: "lock", error = %e, ctx, "lifecycle wal_checkpoint(TRUNCATE) failed");
    }
}

/// Startup-failure handler: show a clear, non-technical native dialog, then exit cleanly with code
/// 1 — NEVER a Rust panic/abort (the v0.3.0 hard-crash). The failure modes are distinguished in
/// both the message and the log:
///   (a) [`AppError::KeychainDenied`] — macOS refused keychain access (user clicked "Deny", or the
///       keychain is locked).
///   (b) [`AppError::Locked`] — unfinished legacy recovery belongs to an already-locked folder.
///   (c) anything else (storage / migration) — the DB couldn't be opened: the key doesn't match
///       the data (e.g. restored from another Mac) or the file is damaged.
/// This fail-safe path never mutates user content or deletes database/files.
fn show_fatal_init_dialog(handle: tauri::AppHandle, err: &crate::error::AppError) {
    use crate::error::AppError;
    use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

    const TITLE: &str = "Murmur can't open your library";

    let (body, log_reason): (String, &str) = match err {
        // `Secrets` carries guidance the user has to ACT on — which key to restore, and from where.
        // Every other arm here is a hardcoded body, so before this the crafted message reached the
        // log and nothing else: the one place a locked-out person needs it was the one place it did
        // not appear. `AppError` messages on this path are content-free by construction (no note
        // text, no paths, no key material), so showing this one is safe.
        AppError::Secrets(detail) => (
            format!(
                "Murmur couldn't unlock its encrypted database.\n\n{detail}\n\nYour notes have \
                 NOT been changed or deleted."
            ),
            "database key unavailable — refused to replace it",
        ),
        AppError::KeychainDenied(_) => (
            "macOS didn't grant access to your keychain, so Murmur couldn't unlock its encrypted \
             database.\n\nYour notes are safe and have not been changed. Please reopen Murmur and \
             choose \"Always Allow\" when macOS asks for keychain access. If this keeps happening, \
             contact support."
                .to_string(),
            "keychain access denied or unavailable",
        ),
        AppError::Locked(_) => (
            "Murmur found unfinished legacy recording recovery files that cannot be safely opened \
             by this build. Those historical files are preserved, but Murmur cannot treat them as \
             sealed or recover them until their folder ownership is authenticated.\n\nYour notes, \
             content, and audio files have NOT been changed or deleted. Murmur may have recorded a \
             recovery marker. For authenticated recovery, reopen Murmur v1.0.1 (or the dedicated \
             recovery build), use Touch ID to unlock the affected folder, then relaunch this Murmur \
             build."
                .to_string(),
            "unfinished legacy recording recovery is locked or ambiguous",
        ),
        _ => (
            "Murmur couldn't unlock its encrypted database. This can happen if the database key \
             doesn't match the data on this Mac (for example after restoring from a backup or \
             another computer) or if the file is damaged.\n\nYour notes have NOT been changed or \
             deleted. Please reopen Murmur, and if this keeps happening, contact support."
                .to_string(),
            "database could not be opened (key mismatch / corruption)",
        ),
    };

    // Technical detail goes to the log only (Display carries no PII / no secret material).
    tracing::error!(target: "state", error = %err, reason = log_reason, "startup aborted safely");

    // Hide the config-created main window so its webview can't flash a broken state behind the
    // dialog (its commands would have no managed AppState).
    if let Some(main) = handle.get_webview_window("main") {
        let _ = main.hide();
    }

    // blocking_show() MUST run off the main thread — it dispatches the native dialog to the main
    // run loop and blocks the caller, so calling it on the main thread would deadlock. Spawn a
    // worker that shows the modal, then exits cleanly (code 1) once the user clicks OK.
    std::thread::spawn(move || {
        handle
            .dialog()
            .message(body)
            .title(TITLE)
            .kind(MessageDialogKind::Error)
            .buttons(MessageDialogButtons::Ok)
            .blocking_show();
        std::process::exit(1);
    });
}

/// Create the always-on-top, frameless, transparent floating recorder bar (hidden until
/// summoned with the global shortcut). It loads the `/bar` route of the same frontend.
fn create_bar_window(app: &tauri::AppHandle) -> tauri::Result<()> {
    if app.get_webview_window("bar").is_some() {
        return Ok(());
    }
    let win = WebviewWindowBuilder::new(app, "bar", WebviewUrl::App("bar".into()))
        .title("Murmur")
        .inner_size(540.0, 58.0)
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .shadow(true)
        .effects(
            EffectsBuilder::new()
                .effect(Effect::HudWindow)
                .radius(29.0)
                .build(),
        )
        .visible(false)
        .build()?;
    position_bar_top_center(&win);
    Ok(())
}

/// Toggle the floating bar: hide if visible, otherwise reposition top-centre, show + focus.
/// Bound to the global ⌘⇧R shortcut and exposed to the UI via `commands::toggle_bar`.
pub fn toggle_bar(app: &tauri::AppHandle) {
    if let Some(win) = app.get_webview_window("bar") {
        if matches!(win.is_visible(), Ok(true)) {
            let _ = win.hide();
        } else {
            position_bar_top_center(&win);
            let _ = win.show();
            let _ = win.set_focus();
        }
    }
}

/// Menu-bar (tray) icon so Murmur is always reachable — even with no window open or only
/// the floating bar showing. Left-click opens the main window; the menu offers Open, the
/// recorder bar, and Quit.
fn setup_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

    let open = MenuItem::with_id(app, "open", "Open Murmur", true, None::<&str>)?;
    let record = MenuItem::with_id(app, "record", "Start / Stop recording", true, None::<&str>)?;
    let bar = MenuItem::with_id(app, "bar", "Recorder bar  (⌘⇧R)", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Murmur", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &open,
            &record,
            &bar,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;

    let mut builder = TrayIconBuilder::new()
        .menu(&menu)
        .show_menu_on_left_click(false)
        .tooltip("Murmur")
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_main(app),
            "record" => toggle_record(app),
            "bar" => toggle_bar(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

/// Tray "Start / Stop recording": ask the (possibly hidden) main window to toggle a
/// recording. The webview stays alive while hidden, so this records without opening a window.
fn toggle_record(app: &tauri::AppHandle) {
    if let Some(main) = app.get_webview_window("main") {
        let _ = main.emit(crate::events::EVENT_TOGGLE_RECORD, ());
    }
}

/// Show + focus the main window (after a hide/minimize), recreating it if it was closed.
fn show_main(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

/// Centre the bar horizontally near the top of the monitor it's on.
fn position_bar_top_center(win: &tauri::WebviewWindow) {
    if let (Ok(Some(monitor)), Ok(size)) = (win.current_monitor(), win.outer_size()) {
        let screen = monitor.size();
        let scale = monitor.scale_factor();
        let x = ((screen.width as f64 - size.width as f64) / 2.0).max(0.0);
        let y = (48.0 * scale).min(screen.height as f64 * 0.3);
        let _ = win.set_position(tauri::PhysicalPosition::new(x, y));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hardening item 1 (RED-before-GREEN): the panic hook used to persist the RAW payload to
    /// murmur.log, so an `expect` on an `io::Error` leaked the failing vault path — whose
    /// filename is note-title-derived, i.e. PII. Every home-dir occurrence must now be redacted
    /// to `~` before the payload reaches `tracing`.
    #[test]
    fn panic_message_redacts_the_home_dir_prefix() {
        let msg = "failed to export note: No such file or directory (os error 2) \
                   at /Users/jane/Vault/Meetings/Salary review with Bob.md \
                   (backup: /Users/jane/Vault/.trash/Salary review with Bob.md)";
        let out = sanitize_panic_message(msg, Some("/Users/jane"));
        assert!(
            !out.contains("/Users/jane"),
            "the home prefix must not survive: {out}"
        );
        assert!(
            out.contains("~/Vault/Meetings/Salary review with Bob.md"),
            "the path is kept readable, just home-relative: {out}"
        );
        assert!(
            out.contains("~/Vault/.trash/"),
            "EVERY occurrence is redacted, not just the first: {out}"
        );
    }

    /// The payload is length-capped so a pathological panic (a struct dump / file body threaded
    /// into an `expect`) cannot flood murmur.log. The cap is marked, and multi-byte chars at the
    /// boundary must never split (this fn runs INSIDE the panic hook — it must not panic).
    #[test]
    fn panic_message_is_length_capped_char_boundary_safe() {
        // 600 two-byte chars ('ł') — a byte-indexed truncation at 400 would split a char.
        let long = "ł".repeat(600);
        let out = sanitize_panic_message(&long, None);
        assert!(
            out.ends_with("…[truncated]"),
            "a capped payload is marked: {out}"
        );
        let kept = out.trim_end_matches("…[truncated]");
        assert_eq!(
            kept.chars().count(),
            PANIC_MESSAGE_MAX_CHARS,
            "exactly the cap survives"
        );
        assert!(
            kept.chars().all(|c| c == 'ł'),
            "no mangled chars at the cut"
        );
    }

    /// A short, home-free payload passes through byte-identical (the common case — our own panic
    /// messages), and degenerate homes (`None` / `"/"`) never trigger redaction ("/"-replacement
    /// would blank every path separator).
    #[test]
    fn panic_message_passthrough_and_degenerate_home() {
        let msg = "recorder mutex poisoned";
        assert_eq!(sanitize_panic_message(msg, None), msg);
        assert_eq!(sanitize_panic_message(msg, Some("/")), msg);
        let pathy = "open /etc/hosts failed";
        assert_eq!(
            sanitize_panic_message(pathy, Some("/")),
            pathy,
            "a one-char home must not be substituted into every separator"
        );
    }

    /// A legacy capture helper can keep writing the same inode after a rename. The helper scan must
    /// therefore precede every crash-recovery claim, not merely the broad stale-file sweep.
    #[test]
    fn startup_helper_scan_precedes_every_recovery_claim() {
        let source = include_str!("lib.rs");
        let worker = source
            .find("let recovered = tauri::async_runtime::spawn_blocking")
            .expect("startup recovery worker exists");
        let worker_source = &source[worker..];
        let scan = worker_source
            .find("detect_surviving_capture_helpers")
            .expect("helper scan exists in recovery worker");
        let ledger_claim = worker_source
            .find("claim_stale_recording_generations")
            .expect("ledger recovery claim exists");
        let legacy_claim = worker_source
            .find("crate::audio::spill::claim_inflight")
            .expect("legacy recovery claim exists");
        assert!(scan < ledger_claim && scan < legacy_claim);
    }

    /// A second process must be rejected before it can ROTATE the shared log, open/migrate the
    /// SQLCipher library, or reach either recovery claimant. This source-order pin complements the
    /// kernel-lock unit test without launching a second GUI process in the library test suite.
    ///
    /// The log surface used to be an inline `std::fs::write(&path, b"")` truncate here; it now
    /// lives in `applog::rotate_and_open` (which moves the previous session aside instead of
    /// destroying it), so the pin follows the CALL — the invariant is unchanged: a losing second
    /// process must not touch the shared file at all.
    #[test]
    fn instance_lock_precedes_every_shared_startup_surface() {
        let source = include_str!("lib.rs");
        let acquire = source
            .find("instance_lock::acquire()")
            .expect("process-wide instance lock exists");
        let log_truncate = source
            .find("crate::applog::rotate_and_open()")
            .expect("per-launch log rotation exists");
        let state_init = source.find("AppState::init()").expect("state init exists");
        let ledger_claim = source
            .find("claim_stale_recording_generations")
            .expect("ledger recovery claim exists");
        let legacy_claim = source
            .find("crate::audio::spill::claim_inflight")
            .expect("legacy recovery claim exists");
        assert!(
            acquire < log_truncate
                && acquire < state_init
                && acquire < ledger_claim
                && acquire < legacy_claim
        );
    }
}
