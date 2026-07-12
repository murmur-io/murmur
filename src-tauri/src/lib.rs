pub mod agent;
pub mod audio;
pub mod brain_reactions;
pub mod brief_runner;
pub mod calendar;
pub mod commands;
pub mod connectors;
pub mod crypto;
pub mod e2ee;
pub mod embed;
pub mod enrich;
pub mod error;
pub mod eval;
pub mod events;
pub mod export;
pub mod facts;
pub mod mcp;
pub mod memory;
pub mod orchestrate;
pub mod pipeline;
pub mod proactive;
pub mod prompts;
pub mod reason;
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

    let log_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    // Persist logs to a FILE as well as stderr. When the app is launched via LaunchServices (the normal
    // double-click / DMG install) its stderr is discarded, so a signed/release build was previously
    // un-diagnosable — every keychain/share failure was invisible. Logs carry NO PII (IDs, stages,
    // counts, durations only — see the no-PII rule), so persisting them on-device is safe. Fresh file
    // per launch (truncate at start); O_APPEND so concurrent writes stay line-atomic. Best-effort: if the
    // file can't be opened we fall back to stderr-only rather than fail startup.
    let log_file = dirs::data_dir()
        .map(|b| b.join(crate::state::app_dir_name()))
        .and_then(|dir| {
            std::fs::create_dir_all(&dir).ok()?;
            let path = dir.join("murmur.log");
            let _ = std::fs::write(&path, b""); // truncate for a fresh per-session log
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .ok()
        });
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

    tauri::Builder::default()
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
        )
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
            commands::import_document,
            commands::import_text,
            commands::brain_overview,
            commands::list_documents,
            commands::get_document,
            commands::delete_document,
            commands::get_user_memory,
            commands::forget_user_fact,
            commands::clear_user_memory,
            commands::import_memories,
            // Re-Truth (the vault heals itself) — supersession review + one-tap stamp + undo.
            commands::preview_supersessions,
            commands::apply_supersessions,
            commands::undo_supersessions,
            commands::get_config,
            commands::get_mcp_config,
            commands::save_config,
            commands::get_storage_report,
            commands::free_up_space,
            commands::reveal_audio_dir,
            commands::consent_to_cloud_egress,
            commands::revoke_cloud_egress,
            commands::consent_to_web_search,
            commands::consent_to_jira,
            commands::consent_to_slack,
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
            commands::org_resolve_source,
            commands::list_org_items,
            commands::folder_active_shares,
            commands::revoke_shares_for_folder,
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
            commands::provider_statuses,
            commands::resummarize,
            commands::list_meetings,
            commands::search_meetings,
            commands::delete_meeting,
            commands::rename_meeting,
            commands::chat_meeting,
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
            commands::run_recipe,
            commands::get_action_items,
            commands::list_open_commitments,
            commands::patch_note_tasks,
            commands::add_reminder,
            commands::pin_moment,
            commands::link_meeting_entities,
            commands::get_graph,
            commands::get_entity_detail,
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
            commands::parakeet_models_present,
            commands::download_parakeet_models,
            commands::brain_model_present,
            commands::list_brain_models,
            commands::brain_model_retirement_nudge,
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
            commands::create_folder,
            commands::rename_folder,
            commands::delete_folder,
            commands::move_note,
            // Notes feature — authored `documents(kind='note')` CRUD + note folders + vault export.
            commands::create_note,
            commands::get_note,
            commands::update_note_doc,
            commands::save_note_text,
            commands::list_notes,
            commands::move_note_doc,
            commands::delete_note,
            commands::export_note_doc,
            commands::list_note_folders,
            commands::create_note_folder,
            commands::rename_note_folder,
            commands::delete_note_folder,
            commands::move_note_folder,
            // Notes — selection Brain-assistant (WP4) + auto-organize (WP5) + link sharing (WP6).
            commands::note_assistant_action,
            commands::plan_organize_notes,
            commands::apply_organize_plan,
            commands::share_note_to_link_doc,
            commands::lock_folder,
            commands::unlock_folder,
            commands::unlock_meeting,
            commands::relock_folder,
            commands::relock_all,
            commands::remove_lock,
            commands::discard_unrecoverable_folder_lock,
            commands::discard_unrecoverable_meeting_lock,
            update::check_for_update,
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

            // Phase 2b — wire the content-free egress ledger. Done here, once, after a successful
            // AppState::init() so the DB is guaranteed open. Every subsequent cloud provider call
            // (routed through RedactingProvider) writes ONE content-free row to egress_log.
            {
                let state = app.state::<AppState>();
                crate::summarize::egress_log::set_egress_sink(std::sync::Arc::new(
                    crate::summarize::egress_log::DbEgressSink::new(state.db.clone()),
                ));
            }

            // Crash-recovery (STAGE 2 salvage + STAGE 1 reconcile) + orphan reap. A session that died
            // mid-record (crash / SIGKILL / `tauri dev` hot-rebuild) never ran `stop_recording`, so the
            // meeting row `start_recording` inserted up-front sits as a `RECORDING` "ghost" AND its mic
            // audio (RAM-only) + far-side scratch would be lost. ORDER IS LOAD-BEARING:
            //   1) CLAIM salvage: find inflight mic spills of crashed recordings + MOVE the paired
            //      far-side scratch out of $TMPDIR (before the reaper below deletes it). Runs FIRST so
            //      the reaper can't eat a recoverable far-side track.
            //   2) RECONCILE the remaining ghosts to terminal `ERROR` — SKIPPING the claimed rows
            //      (salvage sets their final status itself), so a claimed row isn't clobbered.
            //   3) REAP orphaned capture helpers + sweep stale scratch (post-claim, so only truly-
            //      abandoned files remain).
            //   4) SPAWN the async salvage worker: reconstruct each claimed recording + run it through
            //      the EXISTING post-Stop pipeline → a real transcript+note. After the reap so the
            //      paired helper is already down. Best-effort; never deletes un-salvaged audio.
            let salvage_jobs = {
                let state = app.state::<AppState>();
                let (jobs, claimed) = crate::audio::spill::claim_inflight(&state.db);
                if let Err(e) = state.db.reconcile_stuck_recordings_except(&claimed) {
                    tracing::warn!(target: "startup", error = %e, "could not reconcile stuck recordings");
                }
                jobs
            };

            // Reap any capture helper ORPHANED by a previous session that died without a clean Stop
            // (crash / force-quit / a `tauri dev` hot-rebuild SIGKILLing the app mid-record). Such a
            // helper reparents to launchd and keeps capturing to a temp WAV for up to 4h — GBs of
            // dead-session audio the file-age sweep below can't catch (its mtime stays fresh). Run
            // FIRST so the kill releases the file, THEN reclaim any stale scratch left behind.
            // Nothing records yet at setup, so any live capture helper is by definition an orphan.
            // (Salvage above already moved any RECOVERABLE far-side scratch out of the reaper's reach.)
            crate::audio::aec::reap_orphaned_capture_helpers();
            crate::audio::aec::sweep_stale_scratch();
            // R1: reclaim any STALE `*.part` model-download residue (a crash / force-quit / aborted
            // model switch mid-download orphans up to ~3.1 GB). Only removes a `.part` older than 1 h
            // so a live in-progress download is never raced. Best-effort; never fatal to launch.
            if let Ok(models_dir) = crate::transcribe::model::models_dir() {
                crate::transcribe::model::sweep_stale_model_parts(&models_dir);
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

            // STAGE 2 salvage worker (async, detached): now that the reaper has downed any orphan
            // helper, reconstruct + pipeline each claimed crashed recording. No-op when nothing crashed.
            crate::audio::spill::spawn_salvage(app.handle().clone(), salvage_jobs);

            create_bar_window(app.handle())?;
            if let Err(e) = app.global_shortcut().register(SUMMON_SHORTCUT) {
                tracing::warn!(target: "shortcut", error = %e, "could not register global shortcut");
            }
            commands::restart_voice_listener(app.handle().clone());
            setup_tray(app.handle())?;
            // Localhost MCP server (read-only meeting tools for Claude Desktop/Code; no egress).
            // Share the session unlock set so sealed-and-not-unlocked notes stay invisible.
            // Must mirror `AppState::db_path` exactly (same dir + filename) so the MCP server opens
            // the SAME DB the app did — `app_dir_name()` keeps the dev/release split consistent.
            if let Some(db_path) = dirs::data_dir()
                .map(|b| b.join(crate::state::app_dir_name()).join("meetnotes.sqlite"))
            {
                let state = app.state::<AppState>();
                let unlocked = state.unlocked_folders.clone();
                let require_token = state
                    .config
                    .lock()
                    .map(|c| c.mcp_require_token)
                    // Poisoned config ⇒ fail CLOSED (require the token) — aligned with the
                    // reasoner-dispatch poison posture (unreadable config never relaxes auth).
                    .unwrap_or(true);
                crate::mcp::spawn(db_path, unlocked, require_token);
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
                    if !semantic_enabled || !crate::embed::embed_model_present() {
                        return;
                    }
                    let embedder = crate::embed::active_embedder();
                    match db.backfill_topic_chunks_idempotent(embedder.as_ref()) {
                        Ok(indexed) if indexed > 0 => {
                            tracing::info!(target: "rag", indexed, "topic-chunk backfill complete");
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
                            if crate::commands::org_background_sync_tick(state.inner()).await {
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
                // hangs app-exit; a slow-dying child reparents to launchd and the startup reaper
                // SIGTERMs it next launch).
                relock_and_zeroize_on_lifecycle(handle, LIFECYCLE_CTX_APP_EXIT);
                crate::reason::sidecar::kill_on_quit();
            }
        });
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
/// (`stop_all_capture`) FIRST, so quitting mid-recording finalizes + reaps the Swift capture helpers
/// instead of orphaning them to launchd until their 4h self-cap (the next-launch reaper then becomes
/// the safety net, not the primary path). We deliberately do NOT stop capture on `ctx ==
/// "window-close"`: closing the window only HIDES it and the app keeps recording in the tray — killing
/// capture there would silently drop an active tray recording.
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
    // relock_all_inner clears the unlock set, zeroizes the cached KEK, re-blanks all sealed notes,
    // and (as of B12) checkpoints the WAL.
    if let Err(e) = crate::commands::relock_all_inner(state.inner()) {
        tracing::warn!(target: "lock", error = %e, ctx, "lifecycle relock_all failed");
    }
    // Belt-and-suspenders WAL checkpoint in case relock_all short-circuited before its own.
    if let Err(e) = state.db.checkpoint_truncate() {
        tracing::warn!(target: "lock", error = %e, ctx, "lifecycle wal_checkpoint(TRUNCATE) failed");
    }
}

/// Startup-failure handler: AppState::init() returned Err. Show a clear, non-technical native
/// dialog explaining that the encrypted library couldn't be opened, then exit cleanly with code 1
/// — NEVER a Rust panic/abort (the v0.3.0 hard-crash). The two failure modes are distinguished in
/// both the message and the log:
///   (a) [`AppError::KeychainDenied`] — macOS refused keychain access (user clicked "Deny", or the
///       keychain is locked).
///   (b) anything else (storage / migration) — the DB couldn't be opened: the key doesn't match
///       the data (e.g. restored from another Mac) or the file is damaged.
/// This NEVER touches the database or its backups — it is a read-only, fail-safe exit path.
fn show_fatal_init_dialog(handle: tauri::AppHandle, err: &crate::error::AppError) {
    use crate::error::AppError;
    use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

    const TITLE: &str = "Murmur can't open your library";

    let (body, log_reason) = match err {
        AppError::KeychainDenied(_) => (
            "macOS didn't grant access to your keychain, so Murmur couldn't unlock its encrypted \
             database.\n\nYour notes are safe and have not been changed. Please reopen Murmur and \
             choose \"Always Allow\" when macOS asks for keychain access. If this keeps happening, \
             contact support.",
            "keychain access denied or unavailable",
        ),
        _ => (
            "Murmur couldn't unlock its encrypted database. This can happen if the database key \
             doesn't match the data on this Mac (for example after restoring from a backup or \
             another computer) or if the file is damaged.\n\nYour notes have NOT been changed or \
             deleted. Please reopen Murmur, and if this keeps happening, contact support.",
            "database could not be opened (key mismatch / corruption)",
        ),
    };

    // Technical detail goes to the log only (Display carries no PII / no secret material).
    tracing::error!(target: "state", error = %err, reason = log_reason, "startup aborted: AppState::init failed");

    // Hide the config-created main window so its webview can't flash a broken state behind the
    // dialog (its commands would have no managed AppState).
    if let Some(main) = handle.get_webview_window("main") {
        let _ = main.hide();
    }

    // blocking_show() MUST run off the main thread — it dispatches the native dialog to the main
    // run loop and blocks the caller, so calling it on the main thread would deadlock. Spawn a
    // worker that shows the modal, then exits cleanly (code 1) once the user clicks OK.
    let body = body.to_string();
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
